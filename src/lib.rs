use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use sha1_smol::Sha1;
use std::{collections::HashMap, str::Utf8Error, string::FromUtf8Error};
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
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
    #[error("Handshake was not completed: {0}")]
    HandshakeError(#[from] HandshakeError),
    #[error("Invalid frame: {0}")]
    FrameError(#[from] FrameError),
    #[error("Could not receive message: {0}")]
    ReceiveError(#[from] ReceiveError),
}

#[derive(Error, Debug)]
pub enum HandshakeError {
    #[error("Got an unexpected HTTP status in response: {0}")]
    UnexpectedStatus(String),
    #[error("Invalid UTF-8")]
    InvalidUtf8(#[from] Utf8Error),
    #[error("Invalid header line")]
    InvalidHeaderLine(String),
    #[error("Missing or invalid header: {0}")]
    MissingOrInvalidHeader(&'static str),
}

#[derive(Error, Debug)]
pub enum FrameError {
    #[error("Invalid payload length")]
    InvalidPayloadLen,
}

#[derive(Error, Debug)]
pub enum ReceiveError {
    #[error("The opcode was not expected at this time: {0:?}")]
    InvalidOpcode(Opcode),
    #[error("Tried to receive a message on a closed websocket")]
    SendOnClosed,
    #[error("The websocket has been closed")]
    Closed(Option<Close>),
    #[error("Invalid UTF-8 in Text message")]
    InvalidUtf8(#[from] FromUtf8Error),
    #[error("Invalid UTF-8 in Close frame")]
    InvalidUtf8Close(#[from] Utf8Error),
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

impl Error {
    pub fn closed_normally(&self) -> bool {
        matches!(self, Error::ReceiveError(ReceiveError::Closed(_)))
    }
}

#[derive(Debug)]
pub enum Message {
    Text(String),
    Binary(Vec<u8>),
    Ping(Vec<u8>),
    Pong(Vec<u8>),
}

#[derive(Clone, Copy)]
pub enum Role {
    Client,
    Server,
}

enum State {
    Closed,
    Open,
    PartialRead {
        first_opcode: Opcode,
        read_payload: Vec<u8>,
    },
}

pub struct WebSocket<Stream> {
    stream: Stream,
    secure: bool,
    role: Role,
    state: State,
}

impl<Stream> WebSocket<Stream> {
    pub fn stream(&self) -> &Stream {
        &self.stream
    }

    pub fn stream_mut(&mut self) -> &mut Stream {
        &mut self.stream
    }

    pub fn is_secure(&self) -> bool {
        self.secure
    }

    pub fn role(&self) -> Role {
        self.role
    }
}

async fn read_until_crlf_crlf(
    stream: &mut (impl AsyncRead + AsyncWrite + Unpin),
) -> Result<Vec<u8>, Error> {
    const CRLF_CRLF: &[u8] = b"\r\n\r\n";

    let mut response = Vec::<u8>::new();
    loop {
        let mut buf = [0; 2048];
        let read = stream.read(&mut buf).await?;
        response.extend(buf[..read].iter());
        if response.ends_with(CRLF_CRLF) {
            break;
        }
    }

    Ok(response)
}

const SWITCHING_PROTOCOLS: &str = "HTTP/1.1 101 Switching Protocols";
const SEC_WEBSOCKET_ACCEPT_UUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

impl<Stream> WebSocket<Stream>
where
    Stream: AsyncRead + AsyncWrite + Unpin,
{
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
        let response_str = std::str::from_utf8(&response).map_err(HandshakeError::from)?;

        let mut headers = HashMap::new();
        for (i, line) in response_str.lines().enumerate() {
            if i == 0 {
                if line != SWITCHING_PROTOCOLS {
                    return Err(Error::HandshakeError(HandshakeError::UnexpectedStatus(
                        line.into(),
                    )));
                } else {
                    continue;
                }
            }

            if line == "" {
                break;
            }

            let mut split = line.split(": ");

            let Some(header) = split.next() else {
                return Err(Error::HandshakeError(HandshakeError::InvalidHeaderLine(
                    line.into(),
                )));
            };

            let Some(value) = split.next() else {
                return Err(Error::HandshakeError(HandshakeError::InvalidHeaderLine(
                    line.into(),
                )));
            };

            headers.insert(header.to_lowercase(), value.to_lowercase());
        }

        if !matches!(
            headers.get("connection").map(|value| value.as_str()),
            Some("upgrade")
        ) {
            return Err(Error::HandshakeError(
                HandshakeError::MissingOrInvalidHeader("connection"),
            ));
        }

        if !matches!(
            headers.get("upgrade").map(|value| value.as_str()),
            Some("websocket")
        ) {
            return Err(Error::HandshakeError(
                HandshakeError::MissingOrInvalidHeader("upgrade"),
            ));
        }

        let expect_sec_websocket_accept_bytes =
            Sha1::from(format!("{}{}", key_base64, SEC_WEBSOCKET_ACCEPT_UUID))
                .digest()
                .bytes();
        let expect_sec_websocket_accept_base64 = BASE64
            .encode(expect_sec_websocket_accept_bytes)
            .to_lowercase();

        if !matches!(
            headers.get("sec-websocket-accept").map(|value| value.as_str()),
            Some(sec_websocket_accept_base64) if sec_websocket_accept_base64 == expect_sec_websocket_accept_base64
        ) {
            return Err(Error::HandshakeError(
                HandshakeError::MissingOrInvalidHeader("sec-websocket-accept"),
            ));
        }

        Ok(WebSocket {
            stream,
            secure,
            role: Role::Client,
            state: State::Open,
        })
    }

    pub async fn read(&mut self) -> Result<Message, Error> {
        fn closed(frame: Frame, state: &mut State) -> ReceiveError {
            *state = State::Closed;
            let payload = frame.payload();
            match payload[..] {
                [] | [_] => ReceiveError::Closed(None),
                [status_high, status_low, ref reason @ ..] => {
                    let status = u16::from_be_bytes([status_high, status_low]);
                    match std::str::from_utf8(reason) {
                        Ok(reason) => ReceiveError::Closed(Some(Close {
                            status,
                            reason: reason.into(),
                        })),
                        Err(err) => ReceiveError::InvalidUtf8Close(err),
                    }
                }
            }
        }

        match &mut self.state {
            State::Closed => return Err(Error::ReceiveError(ReceiveError::SendOnClosed)),

            state @ State::Open => {
                let frame = Frame::from_stream(&mut self.stream).await?;

                match frame.opcode() {
                    Opcode::Text | Opcode::Binary => {}
                    Opcode::Ping => return Ok(Message::Ping(frame.payload())),
                    Opcode::Pong => return Ok(Message::Pong(frame.payload())),
                    Opcode::Close => return Err(Error::ReceiveError(closed(frame, state))),
                    op => return Err(Error::ReceiveError(ReceiveError::InvalidOpcode(op))),
                }

                let first_opcode = frame.opcode();
                let mut final_frame = frame.fin();
                let mut read_payload = frame.payload();

                while !final_frame {
                    let frame = Frame::from_stream(&mut self.stream).await?;
                    final_frame = frame.fin();

                    match frame.opcode() {
                        Opcode::Continuation => {
                            read_payload.extend(frame.payload());
                        }

                        Opcode::Close => return Err(Error::ReceiveError(closed(frame, state))),
                        Opcode::Ping => {
                            *state = State::PartialRead {
                                first_opcode,
                                read_payload,
                            };
                            return Ok(Message::Ping(frame.payload()));
                        }
                        Opcode::Pong => {
                            *state = State::PartialRead {
                                first_opcode,
                                read_payload,
                            };
                            return Ok(Message::Pong(frame.payload()));
                        }

                        op => return Err(Error::ReceiveError(ReceiveError::InvalidOpcode(op))),
                    }
                }

                match first_opcode {
                    Opcode::Text => Ok(Message::Text(
                        String::from_utf8(read_payload).map_err(ReceiveError::from)?,
                    )),

                    Opcode::Binary => Ok(Message::Binary(read_payload)),

                    _ => unreachable!("Not text or binary"),
                }
            }

            state @ State::PartialRead { .. } => {
                let frame = Frame::from_stream(&mut self.stream).await?;
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
                        Opcode::Close => Err(Error::ReceiveError(closed(frame, state))),

                        op => Err(Error::ReceiveError(ReceiveError::InvalidOpcode(op))),
                    }
                }

                if let Some(message) = extend(frame, state)? {
                    return Ok(message);
                }

                while !final_frame {
                    let frame = Frame::from_stream(&mut self.stream).await?;
                    final_frame = frame.fin();

                    if let Some(message) = extend(frame, state)? {
                        return Ok(message);
                    }
                }

                let State::PartialRead {
                    read_payload,
                    first_opcode,
                } = std::mem::replace(state, State::Open)
                else {
                    unreachable!("Not partial read");
                };

                match first_opcode {
                    Opcode::Text => Ok(Message::Text(
                        String::from_utf8(read_payload).map_err(ReceiveError::from)?,
                    )),

                    Opcode::Binary => Ok(Message::Binary(read_payload)),

                    _ => unreachable!("Not text or binary"),
                }
            }
        }
    }

    pub async fn write(&mut self, message: Message) -> Result<(), Error> {
        todo!();
    }
}

#[derive(Debug)]
struct Frame {
    first_short: u16,
    #[allow(unused)]
    mask_key: Option<u32>,
    payload: Vec<u8>,
}

#[rustfmt::skip]                  const FIN: u16         = 0b1000_0000_0000_0000;
#[rustfmt::skip] #[allow(unused)] const RSV1: u16        = 0b0100_0000_0000_0000;
#[rustfmt::skip] #[allow(unused)] const RSV2: u16        = 0b0010_0000_0000_0000;
#[rustfmt::skip] #[allow(unused)] const RSV3: u16        = 0b0001_0000_0000_0000;
#[rustfmt::skip]                  const OPCODE: u16      = 0b0000_1111_0000_0000;
#[rustfmt::skip]                  const MASK: u16        = 0b0000_0000_1000_0000;
#[rustfmt::skip]                  const PAYLOAD_LEN: u16 = 0b0000_0000_0111_1111;

#[derive(Debug, Clone, Copy)]
pub enum Opcode {
    Continuation,
    Text,
    Binary,
    Close,
    Ping,
    Pong,
    Reserved,
}

impl Frame {
    async fn from_stream(
        stream: &mut (impl AsyncRead + AsyncWrite + Unpin),
    ) -> Result<Frame, Error> {
        // TODO: more performant buffered reading
        let first_short = stream.read_u16().await?;

        let payload_len = match (first_short & PAYLOAD_LEN) as u8 {
            len @ 0..=125 => len as u64,

            126 => stream.read_u16().await? as u64,

            127 => stream.read_u64().await?,

            _ => {
                unreachable!("Initial payload length more than 7 bits");
            }
        };

        let mask_key = if first_short & MASK != 0 {
            Some(stream.read_u32().await?)
        } else {
            None
        };

        let mut i = 0;
        let mut payload = Vec::<u8>::new();
        payload.extend(std::iter::repeat(0).take_while(|_| {
            i += 1;
            i <= payload_len
        }));

        stream.read_exact(&mut payload).await?;

        if let Some(mask) = mask_key {
            let mask_bytes = mask.to_ne_bytes();
            for (i, byte) in payload.iter_mut().enumerate() {
                let j = i % 4;
                *byte ^= mask_bytes[j];
            }
        }

        Ok(Frame {
            first_short,
            mask_key,
            payload,
        })
    }

    fn fin(&self) -> bool {
        self.first_short & FIN != 0
    }

    fn payload(self) -> Vec<u8> {
        self.payload
    }

    fn opcode(&self) -> Opcode {
        match (self.first_short & OPCODE) >> 8 {
            0 => Opcode::Continuation,
            1 => Opcode::Text,
            2 => Opcode::Binary,
            8 => Opcode::Close,
            9 => Opcode::Ping,
            10 => Opcode::Pong,
            _ => Opcode::Reserved,
        }
    }
}
