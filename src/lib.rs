#[cfg(feature = "molybdenite-fuzz")]
pub mod frame;
#[cfg(feature = "molybdenite-fuzz")]
pub mod handshake;

#[cfg(not(feature = "molybdenite-fuzz"))]
mod frame;
#[cfg(not(feature = "molybdenite-fuzz"))]
mod handshake;

use frame::{Frame, FrameStream, Opcode};
use std::{str::Utf8Error, string::FromUtf8Error};
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncWrite, BufStream};
use url::Url;

#[non_exhaustive]
#[derive(Error, Debug)]
pub enum Error {
    #[error("I/O: {0}")]
    Io(#[from] tokio::io::Error),
    #[error("Could not get random data")]
    GetRandom(getrandom::Error),
    #[error("Invalid UTF-8")]
    InvalidUtf8String(#[from] FromUtf8Error),
    #[error("Invalid UTF-8 in Close frame")]
    InvalidUtf8Str(#[from] Utf8Error),

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
    #[error("Invalid request from client: {0}")]
    InvalidRequest(String),

    #[error("Invalid payload length")]
    InvalidPayloadLen,
    #[error("Payload too long")]
    PayloadTooLong,
    #[error("The opcode was not expected at this time: {0:?}")]
    InvalidOpcode(Opcode),
    #[error("Tried to send/receive a message on a closed websocket")]
    WasClosed,
    #[error("Got a fragmented control frame")]
    FragmentedControl,
    #[error("Got control frame larger than 127 bytes")]
    TooLargeControl,
    #[error("RSV bits were set")]
    RsvSet,
    #[error("This is a bug 😭 {0}")]
    Bug(&'static str),
}

impl Error {
    fn unexpected_close() -> Error {
        Error::Io(tokio::io::Error::new(
            tokio::io::ErrorKind::UnexpectedEof,
            "WebSocket unexpectedly closed",
        ))
    }
}

#[derive(Debug)]
pub struct Close {
    bytes: Vec<u8>,
}

impl Close {
    pub fn status(&self) -> u16 {
        debug_assert!(self.bytes.len() >= 2);
        u16::from_be_bytes([self.bytes[0], self.bytes[1]])
    }

    pub fn reason(&self) -> Result<&str, Error> {
        debug_assert!(self.bytes.len() >= 2);
        Ok(std::str::from_utf8(&self.bytes[2..])?)
    }
}

impl From<getrandom::Error> for Error {
    fn from(error: getrandom::Error) -> Self {
        Error::GetRandom(error)
    }
}

#[derive(Debug, Clone, Copy)]
pub enum MessageRef<'data> {
    Text(&'data str),
    Binary(&'data [u8]),
    Ping(&'data [u8]),
    Pong(&'data [u8]),
    Close(Option<&'data Close>),
}

impl MessageRef<'_> {
    fn opcode(&self) -> Opcode {
        match self {
            MessageRef::Text(_) => Opcode::Text,
            MessageRef::Binary(_) => Opcode::Binary,
            MessageRef::Ping(_) => Opcode::Ping,
            MessageRef::Pong(_) => Opcode::Pong,
            MessageRef::Close(_) => Opcode::Close,
        }
    }

    fn payload(&self) -> &[u8] {
        match self {
            MessageRef::Text(text) => text.as_bytes(),
            MessageRef::Binary(data) => data,
            MessageRef::Ping(data) => data,
            MessageRef::Pong(data) => data,
            MessageRef::Close(close) => close.map(|close| close.bytes.as_slice()).unwrap_or(&[]),
        }
    }
}

#[derive(Debug)]
pub enum Message {
    Text(String),
    Binary(Vec<u8>),
    Ping(Vec<u8>),
    Pong(Vec<u8>),
    Close(Option<Close>),
}

impl Message {
    fn as_ref(&self) -> MessageRef {
        match self {
            Message::Text(text) => MessageRef::Text(text.as_str()),
            Message::Binary(data) => MessageRef::Binary(data.as_slice()),
            Message::Ping(data) => MessageRef::Ping(data.as_slice()),
            Message::Pong(data) => MessageRef::Pong(data.as_slice()),
            Message::Close(close) => MessageRef::Close(close.as_ref()),
        }
    }
}

pub trait AsMessageRef<'data> {
    fn into_message_ref(self) -> MessageRef<'data>;
}

impl<'data> AsMessageRef<'data> for &'data Message {
    fn into_message_ref(self) -> MessageRef<'data> {
        self.as_ref()
    }
}

impl<'data> AsMessageRef<'data> for MessageRef<'data> {
    fn into_message_ref(self) -> MessageRef<'data> {
        self
    }
}

#[derive(Clone, Copy)]
pub enum Role {
    Client,
    Server,
}

#[derive(Debug)]
enum State {
    Closed,
    Open,
    PartialRead {
        first_opcode: Opcode,
        read_payload: Vec<u8>,
    },
}

pub struct WebSocket<Stream> {
    stream: FrameStream<BufStream<Stream>>,
    secure: bool,
    role: Role,
    state: State,
}

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

    pub async fn accept(secure: bool, stream: Stream) -> Result<(Self, Url), Error> {
        let mut stream = BufStream::new(stream);
        let request_url = handshake::server(&mut stream, secure).await?;
        Ok((
            Self::accept_no_handshake(secure, stream).await?,
            request_url,
        ))
    }

    pub async fn accept_no_handshake(
        secure: bool,
        stream: BufStream<Stream>,
    ) -> Result<Self, Error> {
        Ok(WebSocket {
            stream: FrameStream::new(stream, None),
            secure,
            role: Role::Server,
            state: State::Open,
        })
    }

    pub async fn connect(url: &Url, stream: Stream) -> Result<Self, Error> {
        let mut stream = BufStream::new(stream);
        handshake::client(url, &mut stream).await?;
        Self::connect_no_handshake(url, stream).await
    }

    pub async fn connect_no_handshake(url: &Url, stream: BufStream<Stream>) -> Result<Self, Error> {
        let secure = url.scheme() == "wss";

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
                Err(Error::unexpected_close())
            }
        }
    }

    pub async fn read(&mut self) -> Result<Message, Error> {
        fn close(frame: Frame) -> Message {
            let payload = frame.payload();
            match payload[..] {
                [] | [_] => Message::Close(None),
                [..] => Message::Close(Some(Close { bytes: payload })),
            }
        }

        match self.state {
            State::Closed => Err(Error::WasClosed),

            State::Open => {
                let frame = self.next_frame().await?;

                match frame.opcode() {
                    Opcode::Text | Opcode::Binary => {}
                    Opcode::Ping => return Ok(Message::Ping(frame.payload())),
                    Opcode::Pong => return Ok(Message::Pong(frame.payload())),
                    Opcode::Close => return Ok(close(frame)),
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

                        Opcode::Close => return Ok(close(frame)),
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
                    _ => Err(Error::Bug("not text or binary in open")),
                }
            }

            State::PartialRead { .. } => {
                let frame = self.next_frame().await?;
                let mut final_frame = frame.fin();

                fn extend(frame: Frame, state: &mut State) -> Result<Option<Message>, Error> {
                    match frame.opcode() {
                        Opcode::Continuation => {
                            let State::PartialRead { read_payload, .. } = state else {
                                return Err(Error::Bug("not partial read extending"));
                            };
                            read_payload.extend(frame.payload());
                            Ok(None)
                        }
                        Opcode::Ping => Ok(Some(Message::Ping(frame.payload()))),
                        Opcode::Pong => Ok(Some(Message::Pong(frame.payload()))),
                        Opcode::Close => Ok(Some(close(frame))),

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
                    return Err(Error::Bug("not partial read in replace"));
                };

                match first_opcode {
                    Opcode::Text => Ok(Message::Text(String::from_utf8(read_payload)?)),
                    Opcode::Binary => Ok(Message::Binary(read_payload)),
                    _ => Err(Error::Bug("not text or binary in partial read")),
                }
            }
        }
    }

    pub async fn write<'data>(&mut self, message: impl AsMessageRef<'data>) -> Result<(), Error> {
        let message = message.into_message_ref();
        self.stream.write_message(message).await
    }

    pub async fn flush(&mut self) -> Result<(), Error> {
        self.stream.flush().await
    }

    pub async fn close(&mut self) -> Result<(), Error> {
        self.stream.write_close().await?;
        self.flush().await
    }
}
