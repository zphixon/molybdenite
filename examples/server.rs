use anyhow::{anyhow, Context, Result};
use std::{fs::File, io::BufReader, net::ToSocketAddrs, path::PathBuf, sync::Arc};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt, BufStream},
    net::TcpListener,
};
use tokio_rustls::{rustls::ServerConfig, TlsAcceptor};

#[derive(argh::FromArgs)]
#[argh(description = "server example")]
struct Args {
    #[argh(option, description = "address to bind to")]
    bind: String,

    #[argh(option, description = "cert private key")]
    key: Option<PathBuf>,

    #[argh(option, description = "certificate")]
    cert: Option<PathBuf>,
}

async fn handle(
    stream: BufStream<impl AsyncReadExt + AsyncWriteExt + Unpin>,
    secure: bool,
) -> Result<()> {
    let mut ws = molybdenite::WebSocket::server_from_stream(secure, stream).await?;

    ws.write(molybdenite::Message::Text("dumptydonkeydooby".into()))
        .await?;
    ws.flush().await?;

    ws.close().await?;
    ws.flush().await?;
    loop {
        match ws.read().await {
            Ok(msg) => {
                println!("client sent: {:?}", msg);
            }

            Err(molybdenite::Error::Closed(_)) => {
                println!("client closed");
                break;
            }

            Err(err) => anyhow::bail!(err),
        }
    }

    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Args = argh::from_env();

    let addr = args
        .bind
        .to_socket_addrs()
        .context("to socket address")?
        .next()
        .ok_or_else(|| anyhow!("to socket address"))?;

    match (args.cert.as_ref(), args.key.as_ref()) {
        (Some(cert), Some(key)) => {
            let certs = rustls_pemfile::certs(&mut BufReader::new(
                File::open(cert).context("open cert file")?,
            ))
            .collect::<Result<Vec<_>, _>>()
            .context("parse cert file")?;

            let key = rustls_pemfile::private_key(&mut BufReader::new(
                File::open(key).context("open key file")?,
            ))
            .context("parse key file")?
            .ok_or_else(|| anyhow!("no key file"))?;

            let config = ServerConfig::builder()
                .with_no_client_auth()
                .with_single_cert(certs, key)
                .context("server config")?;
            let acceptor = TlsAcceptor::from(Arc::new(config));

            let listener = TcpListener::bind(addr).await.context("bind tls")?;

            loop {
                let (stream, peer_addr) = listener.accept().await.context("accept tls tcp")?;
                println!("new client at {}", peer_addr);

                let acceptor = acceptor.clone();

                tokio::spawn(async move {
                    let Ok(stream) = acceptor.accept(stream).await.context("accept tls tls") else {
                        println!("tls broke");
                        return;
                    };

                    let stream = BufStream::new(stream);
                    if let Err(err) = handle(stream, true).await {
                        println!("error: {:?}", err);
                    }
                });
            }
        }

        (None, None) => {
            let listener = TcpListener::bind(addr).await.context("bind")?;
            loop {
                let (stream, peer_addr) = listener.accept().await.context("accept tcp")?;
                println!("new client at {}", peer_addr);

                tokio::spawn(async move {
                    let stream = BufStream::new(stream);
                    if let Err(err) = handle(stream, false).await.context("handle") {
                        println!("error: {:?}", err);
                    }
                });
            }
        }

        _ => {
            anyhow::bail!("if using tls, both --cert and --key must be passed");
        }
    }
}
