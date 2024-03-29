use anyhow::{anyhow, Context, Result};
use std::{
    fs::File,
    io::BufReader,
    net::{SocketAddr, ToSocketAddrs},
    path::PathBuf,
    sync::Arc,
};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    net::{TcpListener, TcpStream},
};
use tokio_rustls::{
    rustls::{pki_types::ServerName, ClientConfig, RootCertStore, ServerConfig},
    TlsAcceptor, TlsConnector,
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

    #[argh(option, description = "cert private key")]
    key: Option<PathBuf>,

    #[argh(option, description = "certificate")]
    cert: Option<PathBuf>,
}

#[derive(argh::FromArgs)]
#[argh(subcommand, name = "request", description = "")]
struct Request {
    #[argh(positional, description = "a")]
    request: Url,

    #[argh(option, description = "ca certificate")]
    ca: Option<PathBuf>,
}

async fn do_client(request: Url, stream: impl AsyncRead + AsyncWrite + Unpin) -> Result<()> {
    let mut ws = molybdenite::WebSocket::client(request, stream)?;
    ws.connect().await?;

    loop {
        match ws.read().await {
            Ok(molybdenite::Message::Close(_)) => {
                println!("server initiated close");
                break;
            }
            Ok(message) => {
                println!("server sent: {:?}", message);
                ws.write(&message).await?;
            }
            Err(err) => anyhow::bail!(err),
        }
    }

    ws.close().await?;

    Ok(())
}

async fn do_server(stream: impl AsyncRead + AsyncWrite + Unpin, secure: bool) -> Result<()> {
    let mut ws = molybdenite::WebSocket::server(secure, stream);
    let request = ws.accept().await?;
    println!("request was {}", request);

    ws.write(molybdenite::MessageRef::Text("dumptydonkeydooby"))
        .await?;
    ws.close().await?;

    loop {
        match ws.read().await {
            Ok(molybdenite::Message::Close(_)) => {
                println!("client finished close");
                break;
            }
            Ok(message) => println!("client sent: {:?}", message),
            Err(err) => anyhow::bail!(err),
        }
    }

    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = argh::from_env::<Args>();

    match args.role {
        Role::Serve(Serve { bind, key, cert }) => {
            let acceptor = match (key, cert) {
                (Some(key), Some(cert)) => {
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
                    Some(TlsAcceptor::from(Arc::new(config)))
                }

                (None, None) => None,

                _ => anyhow::bail!("cannot set only one of --key and --cert"),
            };

            let listener = TcpListener::bind(bind).await?;
            loop {
                let (stream, peer) = listener.accept().await?;
                let acceptor = acceptor.clone();

                tokio::spawn(async move {
                    println!("new peer: {}", peer);
                    if let Some(acceptor) = acceptor {
                        do_server(acceptor.accept(stream).await?, true).await?;
                    } else {
                        do_server(stream, false).await?;
                    }
                    println!("closed: {}", peer);
                    Result::<()>::Ok(())
                });
            }
        }

        Role::Request(Request { request, ca }) => {
            let addr = (
                request.host_str().ok_or_else(|| anyhow!("no host"))?,
                request
                    .port_or_known_default()
                    .ok_or_else(|| anyhow!("no port"))?,
            )
                .to_socket_addrs()
                .context("to socket address")?
                .next()
                .ok_or_else(|| anyhow!("get socket address"))?;

            if request.scheme() == "wss" {
                let mut root_cert_store = RootCertStore::empty();
                root_cert_store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());

                if let Some(ca) = ca.as_ref() {
                    let ca_file = File::open(ca).context("read ca file")?;
                    let mut buf_reader = BufReader::new(ca_file);
                    rustls_pemfile::certs(&mut buf_reader)
                        .collect::<Result<Vec<_>, _>>()
                        .context("read certs from CA")?
                        .into_iter()
                        .map(|cert| root_cert_store.add(cert))
                        .collect::<Result<Vec<_>, _>>()
                        .context("add certs to root store")?;
                }

                let config = ClientConfig::builder()
                    .with_root_certificates(root_cert_store)
                    .with_no_client_auth();
                let connector = TlsConnector::from(Arc::new(config));

                let dns_name =
                    ServerName::try_from(request.host_str().expect("already know we have host"))
                        .context("server name")?
                        .to_owned();

                let tcp_stream = TcpStream::connect(addr).await.context("connect")?;
                let tls_stream = connector
                    .connect(dns_name, tcp_stream)
                    .await
                    .context("tls")?;

                println!("connecting (tls)");
                do_client(request, tls_stream).await?;
                println!("closed");
            } else {
                let stream = TcpStream::connect(format!(
                    "{}:{}",
                    request.host_str().expect("no host"),
                    request.port_or_known_default().expect("no port")
                ))
                .await?;

                println!("connecting");
                do_client(request, stream).await?;
                println!("closed");
            }
        }
    }

    Ok(())
}
