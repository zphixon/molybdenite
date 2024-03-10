use anyhow::{anyhow, Context, Result};
use std::{fs::File, io::BufReader, net::ToSocketAddrs, path::PathBuf, sync::Arc};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};
use tokio_rustls::{
    rustls::{pki_types::ServerName, ClientConfig, RootCertStore},
    TlsConnector,
};
use url::Url;

#[derive(argh::FromArgs)]
#[argh(description = "client example")]
struct Args {
    #[argh(option, description = "remote host to connect to")]
    request: Url,

    #[argh(option, description = "domain (CN) of the remote host")]
    domain: Option<String>,

    #[argh(option, description = "ca certificate")]
    ca: Option<PathBuf>,
}

async fn do_connect(url: Url, stream: impl AsyncReadExt + AsyncWriteExt + Unpin) -> Result<()> {
    let mut ws = molybdenite::WebSocket::client_from_stream(url, stream).await?;

    loop {
        match ws.read().await {
            Ok(message) => {
                println!("server sent: {:?}", message);
            }

            Err(err) if err.closed_normally() => {
                println!("closed normally");
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

    let addr = (
        args.request.host_str().ok_or_else(|| anyhow!("no host"))?,
        args.request
            .port_or_known_default()
            .ok_or_else(|| anyhow!("no port"))?,
    )
        .to_socket_addrs()
        .context("to socket address")?
        .next()
        .ok_or_else(|| anyhow!("get socket address"))?;

    if args.request.scheme() == "wss" {
        let mut root_cert_store = RootCertStore::empty();
        root_cert_store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());

        if let Some(ca) = args.ca.as_ref() {
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

        let dns_name = ServerName::try_from(
            args.domain
                .as_ref()
                .map(|domain| domain.as_str())
                .unwrap_or(args.request.host_str().expect("already know we have host")),
        )
        .context("server name")?
        .to_owned();

        let tcp_stream = TcpStream::connect(addr).await.context("connect")?;
        let tls_stream = connector
            .connect(dns_name, tcp_stream)
            .await
            .context("tls")?;

        do_connect(args.request, tls_stream).await?;
    } else {
        do_connect(
            args.request,
            TcpStream::connect(addr).await.context("connect")?,
        )
        .await?;
    }

    Ok(())
}
