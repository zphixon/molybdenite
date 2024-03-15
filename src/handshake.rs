use crate::Error;
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use sha1_smol::Sha1;
use std::collections::HashMap;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufStream};
use url::Url;

const SWITCHING_PROTOCOLS: &str = "HTTP/1.1 101 Switching Protocols";
const SEC_WEBSOCKET_ACCEPT_UUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

async fn read_until_crlf_crlf(
    stream: &mut BufStream<impl AsyncRead + AsyncWrite + Unpin>,
) -> Result<Vec<u8>, Error> {
    const CRLF_CRLF: &[u8] = b"\r\n\r\n";

    let mut response = Vec::new();

    while !response.ends_with(CRLF_CRLF) {
        stream.read_until(b'\n', &mut response).await?;
    }

    Ok(response)
}

pub async fn server(
    stream: &mut BufStream<impl AsyncRead + AsyncWrite + Unpin>,
) -> Result<(), Error> {
    let request_bytes = read_until_crlf_crlf(stream).await?;
    let request_str = std::str::from_utf8(&request_bytes)?;

    let mut headers = HashMap::new();
    for (i, line) in request_str.lines().enumerate() {
        if i == 0 {
            let mut split = line.split_ascii_whitespace();
            // TODO: query
            let (Some("GET"), Some(_), Some("HTTP/1.1")) =
                (split.next(), split.next(), split.next())
            else {
                return Err(Error::UnexpectedRequest(line.into()));
            };
            continue;
        }

        if line == "" {
            break;
        }

        let mut split = line.split(": ");

        let Some(header) = split.next() else {
            return Err(Error::InvalidHeaderLine(line.into()));
        };

        let Some(value) = split.next() else {
            return Err(Error::InvalidHeaderLine(line.into()));
        };

        headers.insert(header.to_lowercase(), value);
    }

    // TODO: validate
    let Some(_host) = headers.get("host") else {
        return Err(Error::MissingOrInvalidHeader("Host"));
    };

    if headers
        .get("connection")
        .map(|connection| connection.eq_ignore_ascii_case("upgrade"))
        != Some(true)
    {
        return Err(Error::MissingOrInvalidHeader("Connection"));
    }

    if headers
        .get("upgrade")
        .map(|upgrade| upgrade.eq_ignore_ascii_case("websocket"))
        != Some(true)
    {
        return Err(Error::MissingOrInvalidHeader("Upgrade"));
    }

    if headers
        .get("sec-websocket-version")
        .map(|swv| swv.eq_ignore_ascii_case("13"))
        != Some(true)
    {
        return Err(Error::MissingOrInvalidHeader("Sec-WebSocket-Version"));
    }

    let Some(key_base64) = headers.get("sec-websocket-key") else {
        return Err(Error::MissingOrInvalidHeader("Sec-WebSocket-Key"));
    };

    let sec_websocket_accept_bytes =
        Sha1::from(format!("{}{}", key_base64, SEC_WEBSOCKET_ACCEPT_UUID))
            .digest()
            .bytes();

    let sec_websocket_accept_base64 = BASE64.encode(sec_websocket_accept_bytes);

    let response = format!(
        concat!(
            "{}\r\n",
            "Connection: Upgrade\r\n",
            "Upgrade: websocket\r\n",
            "Sec-WebSocket-Accept: {}\r\n",
            "\r\n",
        ),
        SWITCHING_PROTOCOLS, sec_websocket_accept_base64,
    );

    stream.write_all(response.as_bytes()).await?;
    stream.flush().await?;

    Ok(())
}

pub async fn client(
    url: Url,
    stream: &mut BufStream<impl AsyncRead + AsyncWrite + Unpin>,
) -> Result<(), Error> {
    let ("ws" | "wss") = url.scheme() else {
        return Err(Error::IncorrectScheme);
    };

    let secure = url.scheme() == "wss";
    let host = url.host_str().ok_or_else(|| Error::NoHost)?;
    let port = url.port().unwrap_or_else(|| if secure { 443 } else { 80 });

    let resource_name = match url.query() {
        Some(query) => format!("{}?{}", url.path(), query),
        None => url.path().into(),
    };

    let mut key_bytes = [0u8; 16];
    getrandom::getrandom(&mut key_bytes)?;
    let key_base64 = BASE64.encode(key_bytes);

    let request = format!(
        concat!(
            "GET {} HTTP/1.1\r\n",
            "Host: {}:{}\r\n",
            "Connection: Upgrade\r\n",
            "Upgrade: websocket\r\n",
            "Sec-WebSocket-Version: 13\r\n",
            "Sec-WebSocket-Key: {}\r\n",
            "\r\n",
        ),
        resource_name, host, port, key_base64,
    );

    stream.write_all(request.as_bytes()).await?;
    stream.flush().await?;

    let response = read_until_crlf_crlf(stream).await?;
    let response_str = std::str::from_utf8(&response)?;

    let mut headers = HashMap::new();
    for (i, line) in response_str.lines().enumerate() {
        if i == 0 {
            if line != SWITCHING_PROTOCOLS {
                return Err(Error::UnexpectedStatus(line.into()));
            } else {
                continue;
            }
        }

        if line == "" {
            break;
        }

        let mut split = line.split(": ");

        let (Some(header), Some(value)) = (split.next(), split.next()) else {
            return Err(Error::InvalidHeaderLine(line.into()));
        };

        headers.insert(header.to_lowercase(), value);
    }

    let expect_sec_websocket_accept_bytes =
        Sha1::from(format!("{}{}", key_base64, SEC_WEBSOCKET_ACCEPT_UUID))
            .digest()
            .bytes();

    let expect_sec_websocket_accept_base64 = BASE64.encode(expect_sec_websocket_accept_bytes);

    if headers
        .get("connection")
        .map(|connection| connection.eq_ignore_ascii_case("upgrade"))
        != Some(true)
    {
        return Err(Error::MissingOrInvalidHeader("Connection"));
    }

    if headers
        .get("upgrade")
        .map(|upgrade| upgrade.eq_ignore_ascii_case("websocket"))
        != Some(true)
    {
        return Err(Error::MissingOrInvalidHeader("Upgrade"));
    }

    if headers
        .get("sec-websocket-accept")
        .map(|swa| swa.eq_ignore_ascii_case(expect_sec_websocket_accept_base64.as_str()))
        != Some(true)
    {
        return Err(Error::MissingOrInvalidHeader("Sec-WebSocket-Accept"));
    }

    Ok(())
}
