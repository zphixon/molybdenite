use molybdenite::{AsMessageRef, Message, MessageRef, WebSocket};
use std::{
    sync::atomic::{AtomicU16, Ordering},
    time::Duration,
};
use tokio::{
    net::{TcpListener, TcpStream},
    runtime::Runtime,
};
use url::Url;

const TEXT: &str = "dumpty yikes dumpty donkey dooby donkey";
static PORT: AtomicU16 = AtomicU16::new(9122);

fn next_port() -> u16 {
    PORT.fetch_add(1, Ordering::SeqCst)
}

fn next_address() -> &'static str {
    Box::leak(format!("127.0.0.1:{}", next_port()).into_boxed_str())
}

fn ws_url(addr: &str, case: &str) -> Url {
    Url::parse(&format!("ws://{}/{}", addr, case)).unwrap()
}

async fn echo_client(addr: &str, fragment_size: usize, case: &str) {
    let stream = TcpStream::connect(addr).await.unwrap();
    let mut ws = WebSocket::client(ws_url(addr, case), stream).unwrap();
    ws.set_fragment_size(fragment_size);
    ws.client_handshake().await.unwrap();
    loop {
        let msg = ws.read().await.unwrap();
        match msg.as_message_ref() {
            MessageRef::Close(_) => break,
            _ => {
                ws.write(msg.pong()).await.unwrap();
                ws.flush().await.unwrap();
            }
        }
    }
    ws.close().await.unwrap();
}

async fn expect_echo(
    ws: &mut WebSocket<TcpStream>,
    fragment_size: usize,
    messages: &[MessageRef<'_>],
) {
    ws.set_fragment_size(fragment_size);
    for message in messages {
        ws.write(*message).await.unwrap();
    }
    ws.close().await.unwrap();
    let mut count = 0;
    for message in messages {
        let expect_response = message.pong();
        let got_message = ws.read().await.unwrap();
        assert_eq!(got_message.as_message_ref(), expect_response);
        if got_message.is_close() {
            break;
        }
        count += 1;
    }
    assert_eq!(count, messages.len());
}

fn echo(messages: Vec<MessageRef<'static>>, fragment_size: usize, case: &str) {
    let addr = next_address();

    let runtime = Runtime::new().unwrap();
    let (send_done, recv_done) = std::sync::mpsc::channel();

    let server_send_done = send_done.clone();
    let _server_handle = runtime.spawn(async move {
        let listener = TcpListener::bind(addr).await.unwrap();
        let (socket, _) = listener.accept().await.unwrap();
        let mut ws = WebSocket::server(false, socket);
        ws.server_handshake().await.unwrap();
        expect_echo(&mut ws, fragment_size, messages.as_slice()).await;
        server_send_done.send(()).unwrap();
    });

    let case: &'static str = Box::leak(String::from(case).into_boxed_str());
    let client_send_done = send_done.clone();
    let _client_handle = runtime.spawn(async move {
        echo_client(addr, fragment_size, case).await;
        client_send_done.send(()).unwrap();
    });

    recv_done.recv_timeout(Duration::from_secs(10)).unwrap();
    recv_done.recv_timeout(Duration::from_secs(10)).unwrap();
}

#[test]
fn test_next_port() {
    assert_ne!(next_port(), next_port());
}

#[test]
fn test_next_address() {
    assert_ne!(next_address(), next_address());
}

#[test]
fn single_text_message_then_close() {
    let addr = next_address();

    let runtime = Runtime::new().unwrap();
    let (send_done, recv_done) = std::sync::mpsc::channel();

    let server_send_done = send_done.clone();
    let _server_handle = runtime.spawn(async move {
        let listener = TcpListener::bind(addr).await.unwrap();

        let (socket, _) = listener.accept().await.unwrap();
        let mut ws = WebSocket::server(false, socket);

        ws.server_handshake().await.unwrap();
        ws.write(MessageRef::Text(TEXT)).await.unwrap();
        ws.close().await.unwrap();
        while !matches!(ws.read().await.unwrap(), Message::Close(_)) {}

        server_send_done.send(()).unwrap();
    });

    let client_send_done = send_done.clone();
    let _client_handle = runtime.spawn(async move {
        let client = TcpStream::connect(addr).await.unwrap();
        let mut ws =
            WebSocket::client(ws_url(addr, "single text message then close"), client).unwrap();

        ws.client_handshake().await.unwrap();
        assert!(matches!(ws.read().await.unwrap(), Message::Text(text) if text == TEXT));
        assert!(matches!(ws.read().await.unwrap(), Message::Close(None)));
        ws.close().await.unwrap();

        client_send_done.send(()).unwrap();
    });

    recv_done.recv_timeout(Duration::from_millis(200)).unwrap();
    recv_done.recv_timeout(Duration::from_millis(200)).unwrap();
}

#[test]
#[should_panic]
fn fragment_size_zero_panics() {
    let runtime = Runtime::new().unwrap();
    let (send_done, recv_done) = std::sync::mpsc::channel();
    let _task = runtime.spawn(async move {
        let listener = TcpListener::bind(next_address()).await.unwrap();
        let (stream, _) = listener.accept().await.unwrap();
        let mut ws = WebSocket::server(false, stream);
        ws.set_fragment_size(0);
        send_done.send(()).unwrap();
    });
    recv_done.recv_timeout(Duration::from_millis(200)).unwrap();
}

#[test]
fn test_fragmented_echo() {
    let messages = vec![
        MessageRef::Text(TEXT),
        MessageRef::Text(TEXT),
        MessageRef::Text(TEXT),
        MessageRef::Text(""),
        MessageRef::Text(TEXT),
        MessageRef::Text(TEXT),
        MessageRef::Text(TEXT),
        MessageRef::Ping(TEXT.as_bytes()),
        MessageRef::Text(include_str!("../src/lib.rs")),
        MessageRef::Pong(&[]),
        MessageRef::Binary(include_bytes!("../src/lib.rs")),
        MessageRef::Text(include_str!("../src/lib.rs")),
        MessageRef::Pong(&[]),
        MessageRef::Binary(include_bytes!("../src/lib.rs")),
    ];

    (0..messages.len()).for_each(|len| {
        [
            1,
            2,
            32,
            999,
            978243,
            molybdenite::DEFAULT_FRAGMENT_SIZE / 2,
            molybdenite::DEFAULT_FRAGMENT_SIZE,
        ]
        .into_iter()
        .for_each(|fragment_size| {
            echo(
                messages[0..len].to_vec(),
                fragment_size,
                &format!("test_echo:len={}:fragment_size={}", len, fragment_size),
            );
        });
    });
}
