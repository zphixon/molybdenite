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
    ws.connect().await.unwrap();

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

async fn run_test_client(case: u32, host: &str) -> Result<(), molybdenite::Error> {
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
    ws.connect().await?;

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
    ws.flush().await?;

    Ok(())
}

async fn run_test_server(bind: SocketAddr) -> Result<(), molybdenite::Error> {
    let listener = TcpListener::bind(bind).await?;

    loop {
        let (stream, _) = listener.accept().await?;
        let mut ws = molybdenite::WebSocket::server(false, BufStream::new(stream));
        ws.accept().await?;

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
    ws.connect().await.unwrap();

    ws.close().await.unwrap();
}

#[derive(argh::FromArgs)]
#[argh(description = "")]
struct Args {
    #[argh(subcommand, description = "a")]
    role: Role,
}

#[derive(argh::FromArgs)]
#[argh(subcommand)]
enum Role {
    Serve(Serve),
    Request(Request),
}

const DEFAULT_BIND: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 9001);

#[derive(argh::FromArgs)]
#[argh(subcommand, name = "serve", description = "")]
struct Serve {
    #[argh(positional, description = "a", default = "DEFAULT_BIND")]
    bind: SocketAddr,
}

const DEFAULT_HOST: &str = "localhost:9001";

#[derive(argh::FromArgs)]
#[argh(subcommand, name = "request", description = "")]
struct Request {
    #[argh(positional, description = "a", default = "DEFAULT_HOST.into()")]
    host: String,

    #[argh(option, description = "a")]
    case: Option<u32>,
}

#[tokio::main]
async fn main() {
    let args = argh::from_env::<Args>();

    match args.role {
        Role::Serve(Serve { bind }) => {
            run_test_server(bind).await.unwrap();
            update_reports().await;
        }

        Role::Request(Request { host, case }) => {
            if let Some(case) = case {
                let result = run_test_client(case, &host).await;
                update_reports().await;
                result.unwrap();
                return;
            }

            let case_count = case_count(&host).await;

            for case in 1..=case_count {
                if let Err(err) = run_test_client(case, &host).await {
                    println!("error on case {}: {}", case, err);
                }
            }

            update_reports().await;
        }
    }
}
