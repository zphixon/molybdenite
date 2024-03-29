use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use tokio::{
    io::BufStream,
    net::{TcpListener, TcpStream},
};
use url::Url;

async fn case_count(host: &str) -> u32 {
    let stream = BufStream::new(TcpStream::connect("localhost:9001").await.unwrap());

    let mut ws = molybdenite::WebSocket::client(
        Url::parse(&format!("ws://{}/getCaseCount", host)).unwrap(),
        stream,
    )
    .unwrap();
    ws.client_handshake().await.unwrap();

    let msg = ws.read().await.unwrap();

    match msg {
        molybdenite::Message::Text(data) => data.parse().unwrap(),
        molybdenite::Message::Binary(data)
        | molybdenite::Message::Ping(data)
        | molybdenite::Message::Pong(data) => std::str::from_utf8(&data).unwrap().parse().unwrap(),
        molybdenite::Message::Close(_) => {
            panic!("unexpected close?");
        }
    }
}

async fn run_test_client(
    case: u32,
    host: &str,
    fragment_size: usize,
) -> Result<(), molybdenite::Error> {
    let stream = BufStream::new(TcpStream::connect("localhost:9001").await.unwrap());

    let mut ws = molybdenite::WebSocket::client(
        Url::parse(&format!(
            "ws://{}/runCase?case={}&agent=molybdenite",
            host, case
        ))
        .unwrap(),
        stream,
    )
    .unwrap();
    ws.set_fragment_size(fragment_size);
    ws.client_handshake().await?;

    while let Ok(msg) = ws.read().await {
        match msg {
            molybdenite::Message::Text(_) | molybdenite::Message::Binary(_) => {
                ws.write(&msg).await?;
                ws.flush().await?;
            }
            molybdenite::Message::Ping(data) => {
                ws.write(&molybdenite::Message::Pong(data)).await?;
                ws.flush().await?;
            }
            molybdenite::Message::Close(_) => break,
            _ => {}
        }
    }

    ws.close().await?;

    Ok(())
}

async fn run_test_server(bind: SocketAddr, fragment_size: usize) -> Result<(), molybdenite::Error> {
    let listener = TcpListener::bind(bind).await?;

    loop {
        let (stream, _) = listener.accept().await?;
        let mut ws = molybdenite::WebSocket::server(false, BufStream::new(stream));
        ws.set_fragment_size(fragment_size);
        ws.server_handshake().await?;

        tokio::spawn(async move {
            while let Ok(msg) = ws.read().await {
                match msg {
                    molybdenite::Message::Text(_) | molybdenite::Message::Binary(_) => {
                        ws.write(&msg).await?;
                        ws.flush().await?;
                    }
                    molybdenite::Message::Ping(data) => {
                        ws.write(&molybdenite::Message::Pong(data)).await?;
                        ws.flush().await?;
                    }
                    molybdenite::Message::Close(_) => break,
                    _ => {}
                }
            }
            ws.close().await?;
            Result::<(), molybdenite::Error>::Ok(())
        });
    }
}

async fn update_reports() {
    let stream = BufStream::new(TcpStream::connect("localhost:9001").await.unwrap());

    let mut ws = molybdenite::WebSocket::client(
        Url::parse("ws://localhost:9001/updateReports?agent=molybdenite").unwrap(),
        stream,
    )
    .unwrap();
    ws.client_handshake().await.unwrap();

    ws.close().await.unwrap();
}

#[derive(argh::FromArgs)]
#[argh(description = "autobahn test suite driver")]
struct Args {
    #[argh(subcommand, description = "role that we should be testing")]
    role: Role,
    #[argh(
        option,
        description = "size that payloads should be fragmented into",
        default = "molybdenite::DEFAULT_FRAGMENT_SIZE"
    )]
    fragment_size: usize,
}

#[derive(argh::FromArgs)]
#[argh(subcommand)]
enum Role {
    Serve(Serve),
    Request(Request),
}

const DEFAULT_BIND: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 9001);

#[derive(argh::FromArgs)]
#[argh(
    subcommand,
    name = "serve",
    description = "test using molybdenite as a server - should be run against `run_autobahn_container.sh run_test_client`"
)]
struct Serve {
    #[argh(
        positional,
        description = "address to bind the molybdenite test server to",
        default = "DEFAULT_BIND"
    )]
    bind: SocketAddr,
}

const DEFAULT_HOST: &str = "localhost:9001";

#[derive(argh::FromArgs)]
#[argh(
    subcommand,
    name = "request",
    description = "test using molybdenite as a client - should be run against `run_autobahn_container.sh run_test_server`"
)]
struct Request {
    #[argh(
        positional,
        description = "host to connect to",
        default = "DEFAULT_HOST.into()"
    )]
    host: String,

    #[argh(
        option,
        description = "case number to run. unfortunately not a case number like 6.1.2, just a regular number like 66"
    )]
    case: Option<u32>,
}

#[tokio::main]
async fn main() {
    let args = argh::from_env::<Args>();

    match args.role {
        Role::Serve(Serve { bind }) => {
            run_test_server(bind, args.fragment_size).await.unwrap();
            update_reports().await;
        }

        Role::Request(Request { host, case }) => {
            if let Some(case) = case {
                let result = run_test_client(case, &host, args.fragment_size).await;
                update_reports().await;
                result.unwrap();
                return;
            }

            let case_count = case_count(&host).await;

            for case in 1..=case_count {
                if let Err(err) = run_test_client(case, &host, args.fragment_size).await {
                    println!("error on case {}: {}", case, err);
                }
            }

            update_reports().await;
        }
    }
}
