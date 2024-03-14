use anyhow::Result;
use url::Url;

#[derive(argh::FromArgs)]
#[argh(description = "client example")]
struct Args {
    #[argh(option, description = "remote host to connect to")]
    request: Url,
}

fn main() -> Result<()> {
    let Args { request } = argh::from_env();

    let (mut ws, _) = tungstenite::connect(request)?;

    loop {
        match ws.read()? {
            tungstenite::Message::Close(_) => {
                println!("server closed");
                break;
            }
            msg => println!("server sent: {:?}", msg),
        }
    }

    ws.close(None).unwrap();

    Ok(())
}
