mod frame;
mod handshake;

use frame::{Frame, FrameStream, Opcode};
use std::{str::Utf8Error, string::FromUtf8Error};
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncWrite, BufStream};
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
    #[error("Got a fragmented control frame")]
    FragmentedControl,
    #[error("Got control frame larger than 127 bytes")]
    TooLargeControl,
    #[error("RSV bits were set")]
    RsvSet,
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

    pub async fn flush(&mut self) -> Result<(), Error> {
        self.stream.flush().await
    }

    pub async fn server_from_stream(secure: bool, stream: Stream) -> Result<Self, Error> {
        let mut stream = BufStream::new(stream);
        handshake::server(&mut stream).await?;

        Ok(WebSocket {
            stream: FrameStream::new(stream, None),
            secure,
            role: Role::Server,
            state: State::Open,
        })
    }

    pub async fn client_from_stream(url: Url, stream: Stream) -> Result<Self, Error> {
        let mut stream = BufStream::new(stream);
        let secure = url.scheme() == "wss";
        handshake::client(url, &mut stream).await?;

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
        self.stream.write_close().await?;
        self.flush().await
    }
}
