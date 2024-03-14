use anyhow::{anyhow, Context, Result};
use std::{
    fs::File,
    io::{BufRead, BufReader, BufWriter, Read, Write},
    net::{TcpListener, ToSocketAddrs},
    path::PathBuf,
    sync::Arc,
};
use tokio_rustls::rustls::{ServerConfig, ServerConnection, StreamOwned};

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

fn handle(mut stream: impl Read + Write, _secure: bool) -> Result<()> {
    let buf_read = BufReader::new(&mut stream);
    for line in buf_read.lines() {
        println!("{:?}", line);
        if matches!(line.as_ref().map(|s| s.as_str()), Ok("")) {
            break;
        }
    }

    {
        let mut buf_write = BufWriter::new(&mut stream);
        buf_write.write_all(b"The entire bee mofie script")?;
    }

    // let mut ws = tungstenite::accept(stream).map_err(|e| anyhow!("{}", e))?;

    // ws.write(tungstenite::Message::Text("dumptydonkeydooby".into()))?;
    // ws.flush()?;

    // ws.close(None)?;
    // ws.flush()?;
    // loop {
    //     match ws.read() {
    //         Ok(msg) => {
    //             println!("client sent: {:?}", msg);
    //             if msg.is_close() {
    //                 break;
    //             }
    //         }

    //         Err(err) => anyhow::bail!(err),
    //     }
    // }

    Ok(())
}

fn main() -> Result<()> {
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

            let config = Arc::new(
                ServerConfig::builder()
                    .with_no_client_auth()
                    .with_single_cert(certs, key)
                    .context("server config")?,
            );

            let listener = TcpListener::bind(addr).context("bind tls")?;

            for tcp_stream in listener.incoming() {
                let tcp_stream = tcp_stream.context("tcp stream")?;
                let connection =
                    ServerConnection::new(Arc::clone(&config)).context("server connection")?;
                let tls_stream = StreamOwned::new(connection, tcp_stream);
                handle(tls_stream, true).context("handle tls")?;
            }

            Ok(())
        }

        (None, None) => {
            let listener = TcpListener::bind(addr).context("bind")?;
            for tcp_stream in listener.incoming() {
                let tcp_stream = tcp_stream.context("tcp stream")?;
                handle(tcp_stream, false).context("handle")?;
            }
            Ok(())
        }

        _ => {
            anyhow::bail!("if using tls, both --cert and --key must be passed");
        }
    }
}
