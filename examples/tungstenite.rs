use anyhow::{anyhow, Result};
use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
};
use url::Url;

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

#[derive(argh::FromArgs)]
#[argh(subcommand, name = "serve", description = "")]
struct Serve {
    #[argh(positional, description = "a")]
    bind: SocketAddr,
}

#[derive(argh::FromArgs)]
#[argh(subcommand, name = "request", description = "")]
struct Request {
    #[argh(positional, description = "a")]
    request: Url,
}

fn do_client(request: Url, stream: impl Read + Write) -> Result<()> {
    let (mut ws, _) = tungstenite::client(request, stream).map_err(|err| anyhow!("{}", err))?;

    loop {
        match ws.read()? {
            tungstenite::Message::Close(_) => break,
            message => println!("server sent: {:?}", message),
        }
    }

    ws.close(None)?;

    Ok(())
}

fn do_server(stream: impl Read + Write) -> Result<()> {
    let mut ws = tungstenite::accept(stream).map_err(|err| anyhow!("{}", err))?;

    ws.write(tungstenite::Message::Text("dumptydonkeydooby".into()))?;
    ws.close(None)?;

    while !matches!(ws.read()?, tungstenite::Message::Close(_)) {}

    Ok(())
}

fn main() -> Result<()> {
    let args = argh::from_env::<Args>();

    match args.role {
        Role::Serve(Serve { bind }) => {
            let listener = TcpListener::bind(bind)?;
            loop {
                let (stream, peer) = listener.accept()?;
                std::thread::spawn(move || {
                    println!("new peer: {}", peer);
                    do_server(stream)?;
                    println!("closed");
                    Result::<()>::Ok(())
                });
            }
        }

        Role::Request(Request { request }) => {
            let stream = TcpStream::connect(format!(
                "{}:{}",
                request.host_str().expect("no host"),
                request.port_or_known_default().expect("no port")
            ))?;

            println!("connecting");
            do_client(request, stream)?;
            println!("closed");
        }
    }

    Ok(())
}
