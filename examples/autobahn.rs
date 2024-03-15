use tokio::{io::BufStream, net::TcpStream};
use url::Url;

async fn case_count() -> u32 {
    let stream = BufStream::new(TcpStream::connect("localhost:9001").await.unwrap());

    let mut ws = molybdenite::WebSocket::client_from_stream(
        Url::parse("ws://localhost:9001/getCaseCount").unwrap(),
        stream,
    )
    .await
    .unwrap();

    let msg = ws.read().await.unwrap();

    match msg {
        molybdenite::Message::Text(data) => data.parse().unwrap(),
        molybdenite::Message::Binary(data)
        | molybdenite::Message::Ping(data)
        | molybdenite::Message::Pong(data) => std::str::from_utf8(&data).unwrap().parse().unwrap(),
    }
}

async fn run_test(case: u32) -> Result<(), molybdenite::Error> {
    let stream = BufStream::new(TcpStream::connect("localhost:9001").await.unwrap());

    let mut ws = molybdenite::WebSocket::client_from_stream(
        Url::parse(&format!(
            "ws://localhost:9001/runCase?case={}&agent=molybdenite",
            case
        ))
        .unwrap(),
        stream,
    )
    .await
    .unwrap();

    loop {
        match ws.read().await {
            Ok(msg) => match msg {
                molybdenite::Message::Text(_) | molybdenite::Message::Binary(_) => {
                    ws.write(msg).await?;
                    ws.flush().await?;
                }
                molybdenite::Message::Ping(data) => {
                    ws.write(molybdenite::Message::Pong(data)).await?;
                    ws.flush().await?;
                }
                _ => {}
            },

            Err(_) => break,
        }
    }

    ws.close().await?;
    ws.flush().await?;

    Ok(())
}

async fn update_reports() {
    let stream = BufStream::new(TcpStream::connect("localhost:9001").await.unwrap());

    let mut ws = molybdenite::WebSocket::client_from_stream(
        Url::parse("ws://localhost:9001/updateReports?agent=molybdenite").unwrap(),
        stream,
    )
    .await
    .unwrap();

    ws.close().await.unwrap();
}

#[tokio::main]
async fn main() {
    let args = std::env::args().collect::<Vec<String>>();
    if args.len() >= 2 {
        if let Ok(case) = args[1].as_str().parse::<u32>() {
            let result = run_test(case).await;
            update_reports().await;
            result.unwrap();
            return;
        } else {
            panic!("not a number");
        }
    }

    let case_count = case_count().await;

    for case in 1..=case_count {
        if let Err(err) = run_test(case).await {
            println!("error on case {}: {}", case, err);
        }
    }

    update_reports().await;
}
