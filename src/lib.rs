mod frame;

use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use frame::{Frame, FrameStream, Opcode};
use sha1_smol::Sha1;
use std::{collections::HashMap, str::Utf8Error, string::FromUtf8Error};
use thiserror::Error;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use url::Url;

#[derive(Error, Debug)]
pub enum Error {
    #[error("I/O: {0}")]
    TokioIo(#[from] tokio::io::Error),
    #[error("Could not get random data")]
    GetRandom(getrandom::Error),
    #[error("URL does not have a host")]
    NoHost,
    #[error("Incorrect scheme, not one of \"ws\" or \"wss\"")]
    IncorrectScheme,
    #[error("Got an unexpected HTTP status in response: {0}")]
    UnexpectedStatus(String),
    #[error("Invalid header line")]
    InvalidHeaderLine(String),
    #[error("Missing or invalid header: {0}")]
    MissingOrInvalidHeader(&'static str),
    #[error("Invalid payload length")]
    InvalidPayloadLen,
    #[error("The opcode was not expected at this time: {0:?}")]
    InvalidOpcode(Opcode),
    #[error("Tried to send/receive a message on a closed websocket")]
    WasClosed,
    #[error("The websocket has been closed")]
    Closed(Option<Close>),
    #[error("Invalid UTF-8")]
    InvalidUtf8(#[from] FromUtf8Error),
    #[error("Invalid UTF-8 in Close frame")]
    InvalidUtf8Close(#[from] Utf8Error),
    #[error("Got an unexpected HTTP request from client: {0}")]
    UnexpectedRequest(String),
}

#[derive(Debug)]
pub struct Close {
    pub status: u16,
    pub reason: String,
}

impl From<getrandom::Error> for Error {
    fn from(error: getrandom::Error) -> Self {
        Error::GetRandom(error)
    }
}

#[derive(Debug)]
pub enum Message {
    Text(String),
    Binary(Vec<u8>),
    Ping(Vec<u8>),
    Pong(Vec<u8>),
}

impl Message {
    fn into_bytes_opcode(self) -> (Vec<u8>, Opcode) {
        match self {
            Message::Text(text) => (text.into_bytes(), Opcode::Text),
            Message::Binary(data) => (data, Opcode::Binary),
            Message::Ping(data) => (data, Opcode::Ping),
            Message::Pong(data) => (data, Opcode::Pong),
        }
    }
}

#[derive(Clone, Copy)]
pub enum Role {
    Client,
    Server,
}

#[derive(Debug)]
pub enum State {
    Closed,
    Open,
    PartialRead {
        first_opcode: Opcode,
        read_payload: Vec<u8>,
    },
}

pub struct WebSocket<Stream> {
    stream: FrameStream<Stream>,
    secure: bool,
    role: Role,
    pub state: State,
}

async fn read_until_crlf_crlf(
    stream: &mut (impl AsyncRead + AsyncWrite + Unpin),
) -> Result<Vec<u8>, Error> {
    const CRLF_CRLF: &[u8] = b"\r\n\r\n";

    let mut response = Vec::new();

    let mut buf_reader = BufReader::new(stream);
    while !response.ends_with(CRLF_CRLF) {
        buf_reader.read_until(b'\n', &mut response).await?;
    }

    Ok(response)
}

const SWITCHING_PROTOCOLS: &str = "HTTP/1.1 101 Switching Protocols";
const SEC_WEBSOCKET_ACCEPT_UUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

impl<Stream> WebSocket<Stream>
where
    Stream: AsyncRead + AsyncWrite + Unpin,
{
    pub fn is_secure(&self) -> bool {
        self.secure
    }

    pub fn role(&self) -> Role {
        self.role
    }

    pub async fn flush(&mut self) -> Result<(), Error> {
        //self.writer.flush().await?;
        Ok(())
    }

    pub async fn server_from_stream(secure: bool, mut stream: Stream) -> Result<Self, Error> {
        let request_bytes = read_until_crlf_crlf(&mut stream).await?;
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

        if headers.get("connection") != Some(&"Upgrade") {
            return Err(Error::MissingOrInvalidHeader("Connection"));
        }

        if headers.get("upgrade") != Some(&"websocket") {
            return Err(Error::MissingOrInvalidHeader("Upgrade"));
        }

        if headers.get("sec-websocket-version") != Some(&"13") {
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

        Ok(WebSocket {
            stream: FrameStream::new(stream, None),
            secure,
            role: Role::Server,
            state: State::Open,
        })
    }

    pub async fn client_from_stream(url: Url, mut stream: Stream) -> Result<Self, Error> {
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

        let response = read_until_crlf_crlf(&mut stream).await?;
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

        if headers.get("connection") != Some(&"Upgrade") {
            return Err(Error::MissingOrInvalidHeader("Connection"));
        }

        if headers.get("upgrade") != Some(&"websocket") {
            return Err(Error::MissingOrInvalidHeader("Upgrade"));
        }

        if headers.get("sec-websocket-accept") != Some(&expect_sec_websocket_accept_base64.as_str())
        {
            return Err(Error::MissingOrInvalidHeader("Sec-WebSocket-Accept"));
        }

        let mut mask_bytes = [0u8; 4];
        getrandom::getrandom(&mut mask_bytes)?;
        let mask_key = u32::from_le_bytes(mask_bytes);

        Ok(WebSocket {
            stream: FrameStream::new(stream, Some(mask_key)),
            secure,
            role: Role::Client,
            state: State::Open,
        })
    }

    async fn next_frame(&mut self) -> Result<Frame, Error> {
        match self.stream.read_frame().await? {
            Some(frame) => Ok(frame),
            None => {
                self.state = State::Closed;
                Err(Error::Closed(None))
            }
        }
    }

    pub async fn read(&mut self) -> Result<Message, Error> {
        fn got_close(frame: Frame) -> Error {
            let payload = frame.payload();
            match payload[..] {
                [] | [_] => Error::Closed(None),
                [status_high, status_low, ref reason @ ..] => {
                    let status = u16::from_be_bytes([status_high, status_low]);
                    match std::str::from_utf8(reason) {
                        Ok(reason) => Error::Closed(Some(Close {
                            status,
                            reason: reason.into(),
                        })),
                        Err(err) => Error::InvalidUtf8Close(err),
                    }
                }
            }
        }

        match self.state {
            State::Closed => return Err(Error::WasClosed),

            State::Open => {
                let frame = self.next_frame().await?;

                match frame.opcode() {
                    Opcode::Text | Opcode::Binary => {}
                    Opcode::Ping => return Ok(Message::Ping(frame.payload())),
                    Opcode::Pong => return Ok(Message::Pong(frame.payload())),
                    Opcode::Close => return Err(got_close(frame)),
                    op => return Err(Error::InvalidOpcode(op)),
                }

                let first_opcode = frame.opcode();
                let mut final_frame = frame.fin();
                let mut read_payload = frame.payload();

                while !final_frame {
                    let frame = self.next_frame().await?;
                    final_frame = frame.fin();

                    match frame.opcode() {
                        Opcode::Continuation => {
                            read_payload.extend(frame.payload());
                        }

                        Opcode::Close => return Err(got_close(frame)),
                        Opcode::Ping => {
                            self.state = State::PartialRead {
                                first_opcode,
                                read_payload,
                            };
                            return Ok(Message::Ping(frame.payload()));
                        }
                        Opcode::Pong => {
                            self.state = State::PartialRead {
                                first_opcode,
                                read_payload,
                            };
                            return Ok(Message::Pong(frame.payload()));
                        }

                        op => return Err(Error::InvalidOpcode(op)),
                    }
                }

                match first_opcode {
                    Opcode::Text => Ok(Message::Text(String::from_utf8(read_payload)?)),
                    Opcode::Binary => Ok(Message::Binary(read_payload)),
                    _ => unreachable!("Not text or binary"),
                }
            }

            State::PartialRead { .. } => {
                let frame = self.next_frame().await?;
                let mut final_frame = frame.fin();

                fn extend(frame: Frame, state: &mut State) -> Result<Option<Message>, Error> {
                    match frame.opcode() {
                        Opcode::Continuation => {
                            let State::PartialRead { read_payload, .. } = state else {
                                unreachable!("Not partial read");
                            };
                            read_payload.extend(frame.payload());
                            Ok(None)
                        }
                        Opcode::Ping => Ok(Some(Message::Ping(frame.payload()))),
                        Opcode::Pong => Ok(Some(Message::Pong(frame.payload()))),
                        Opcode::Close => Err(got_close(frame)),

                        op => Err(Error::InvalidOpcode(op)),
                    }
                }

                if let Some(message) = extend(frame, &mut self.state)? {
                    return Ok(message);
                }

                while !final_frame {
                    let frame = self.next_frame().await?;
                    final_frame = frame.fin();

                    if let Some(message) = extend(frame, &mut self.state)? {
                        return Ok(message);
                    }
                }

                let State::PartialRead {
                    read_payload,
                    first_opcode,
                } = std::mem::replace(&mut self.state, State::Open)
                else {
                    unreachable!("Not partial read");
                };

                match first_opcode {
                    Opcode::Text => Ok(Message::Text(String::from_utf8(read_payload)?)),
                    Opcode::Binary => Ok(Message::Binary(read_payload)),
                    _ => unreachable!("Not text or binary"),
                }
            }
        }
    }

    pub async fn write(&mut self, message: Message) -> Result<(), Error> {
        self.stream.write_message(message).await
    }

    pub async fn close(&mut self) -> Result<(), Error> {
        self.stream.write_close().await
    }
}
