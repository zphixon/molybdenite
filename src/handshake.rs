use crate::Error;
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use sha1_smol::Sha1;
use std::collections::HashMap;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufStream};
use url::Url;

const SWITCHING_PROTOCOLS: &str = "HTTP/1.1 101 Switching Protocols";
const SEC_WEBSOCKET_ACCEPT_UUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

async fn read_headers(
    stream: &mut BufStream<impl AsyncRead + AsyncWrite + Unpin>,
) -> Result<HashMap<String, String>, Error> {
    #[derive(Debug)]
    enum ExpectUtf8Byte {
        Any,
        Continuation(u8),
    }
    impl ExpectUtf8Byte {
        fn next(self, byte: u8) -> Result<ExpectUtf8Byte, Error> {
            match self {
                ExpectUtf8Byte::Any => {
                    if byte & 0b10000000 == 0b00000000 {
                        Ok(ExpectUtf8Byte::Any)
                    } else if byte & 0b11100000 == 0b11000000 {
                        Ok(ExpectUtf8Byte::Continuation(1))
                    } else if byte & 0b11110000 == 0b11100000 {
                        Ok(ExpectUtf8Byte::Continuation(2))
                    } else if byte & 0b11111000 == 0b11110000 {
                        Ok(ExpectUtf8Byte::Continuation(3))
                    } else {
                        Err(Error::InvalidUtf8Header)
                    }
                }

                ExpectUtf8Byte::Continuation(n @ (1 | 2 | 3)) => {
                    if byte & 0b11000000 == 0b10000000 {
                        if n == 1 {
                            Ok(ExpectUtf8Byte::Any)
                        } else {
                            Ok(ExpectUtf8Byte::Continuation(n - 1))
                        }
                    } else {
                        Err(Error::InvalidUtf8Header)
                    }
                }

                _ => Err(Error::Bug("utf8 checker")),
            }
        }
    }

    let mut state = ExpectUtf8Byte::Any;
    let mut headers_bytes = Vec::new();
    loop {
        let byte = stream.read_u8().await?;
        state = state.next(byte)?;
        headers_bytes.push(byte);
        if headers_bytes.len() > 32767 || headers_bytes.ends_with(b"\r\n\r\n") {
            // bro stop
            break;
        }
    }

    let mut headers = HashMap::new();
    let headers_str = std::str::from_utf8(&headers_bytes)?.trim();
    for line in headers_str.lines() {
        if line.is_empty() {
            break;
        }
        let mut split = line.split(": ");

        let Some(header) = split.next() else {
            return Err(Error::InvalidHeaderLine(line.into()));
        };

        let Some(value) = split.next() else {
            return Err(Error::InvalidHeaderLine(line.into()));
        };

        headers.insert(header.to_lowercase(), String::from(value));
    }

    Ok(headers)
}

pub async fn server(
    stream: &mut BufStream<impl AsyncRead + AsyncWrite + Unpin>,
    secure: bool,
) -> Result<Url, Error> {
    let mut request_line_bytes = Vec::new();
    stream.read_until(b'\n', &mut request_line_bytes).await?;
    let request_line = std::str::from_utf8(&request_line_bytes)?.trim();
    let mut split = request_line.split_ascii_whitespace();
    let (Some("GET"), Some(got_request_path), Some("HTTP/1.1")) =
        (split.next(), split.next(), split.next())
    else {
        return Err(Error::InvalidRequest(request_line.into()));
    };
    let request_path = String::from(got_request_path);

    let headers = read_headers(stream).await?;

    let Some(host) = headers.get("host") else {
        return Err(Error::MissingOrInvalidHeader("Host"));
    };

    let request_url: Url = format!(
        "{}://{}{}",
        if secure { "wss" } else { "ws" },
        host,
        request_path
    )
    .parse::<Url>()
    .map_err(|err| Error::InvalidRequest(err.to_string()))?;

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

    Ok(request_url)
}

pub async fn client(
    url: &Url,
    stream: &mut BufStream<impl AsyncRead + AsyncWrite + Unpin>,
) -> Result<(), Error> {
    let ("ws" | "wss") = url.scheme() else {
        return Err(Error::IncorrectScheme);
    };

    let secure = url.scheme() == "wss";
    let host = url.host_str().ok_or_else(|| Error::NoHost)?;
    let port = url.port().unwrap_or(if secure { 443 } else { 80 });

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

    let mut response_line_bytes = Vec::new();
    stream.read_until(b'\n', &mut response_line_bytes).await?;
    let response_line = std::str::from_utf8(&response_line_bytes)?.trim();
    if response_line != SWITCHING_PROTOCOLS {
        return Err(Error::UnexpectedStatus(response_line.into()));
    }

    let headers = read_headers(stream).await?;

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
