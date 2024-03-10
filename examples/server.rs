use anyhow::{anyhow, Context, Result};
use std::{fs::File, io::BufReader, net::ToSocketAddrs, path::PathBuf, sync::Arc};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
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

async fn handle(mut stream: impl AsyncReadExt + AsyncWriteExt + Unpin) -> Result<()> {
    let mut request = Vec::<u8>::new();
    loop {
        let mut buf = [0; 2048];
        let read = stream.read(&mut buf).await?;
        request.extend(buf[..read].iter());
        if read < 4 || &buf[read - 4..read] == b"\r\n\r\n" {
            break;
        }
    }
    let request_str = std::str::from_utf8(&request).context("request utf8")?;
    println!("client sent: {:?}", request_str);

    stream.shutdown().await.context("shutdown")?;
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
                let future = async move {
                    let stream = acceptor.accept(stream).await.context("accept tls tls")?;
                    handle(stream).await.context("handle")?;
                    Result::<()>::Ok(())
                };

                tokio::spawn(async move {
                    if let Err(err) = future.await {
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

                let future = async move {
                    handle(stream).await.context("handle")?;
                    Result::<()>::Ok(())
                };

                tokio::spawn(async move {
                    if let Err(err) = future.await {
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
