#[cfg(feature = "molybdenite-fuzz")]
pub mod frame;
#[cfg(feature = "molybdenite-fuzz")]
pub mod handshake;

#[cfg(not(feature = "molybdenite-fuzz"))]
mod frame;
#[cfg(not(feature = "molybdenite-fuzz"))]
mod handshake;

use frame::{Frame, FrameStream, Opcode};
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
    #[error("Invalid UTF-8: {0}")]
    Utf8(Utf8Error),
    #[error("Handshake unsuccessful: {0}")]
    Handshake(HandshakeError),
    #[error("A frame was malformed: {0}")]
    Frame(FrameError),
    #[error("Tried to send or receive on a closed websocket")]
    WasClosed,
    #[error("Called `connect` on a server, or `accept` on a client")]
    IncorrectRole,
    #[error("A handshake has not yet occurred on this websocket")]
    NoHandshake,
    #[error("This is a bug 😭 {0}")]
    Bug(&'static str),
}

#[non_exhaustive]
#[derive(Debug, Error)]
pub enum Utf8Error {
    #[error("Close reason")]
    CloseReason,
    #[error("Text message")]
    TextMessage,
    #[error("Text message")]
    TextMessagePartialRead,
    #[error("Handshake data")]
    Handshake,
}

#[non_exhaustive]
#[derive(Debug, Error)]
pub enum FrameError {
    #[error("Reserved opcode")]
    ReservedOpcode,
    #[error("Fragmented control frame")]
    FragmentedControl,
    #[error("Control frame too large")]
    LargeControl,
    #[error("Reserved bit set")]
    RsvSet,
    #[error("Frame payload too long (greater than usize::MAX)")]
    FramePayloadTooLong,
    #[error("Entire frame too long (would overflow usize)")]
    FrameTooLong,
    #[error("Unexpected opcode (open state)")]
    UnexpectedOpcodeOpen,
    #[error("Unexpected opcode (open state, non-final frame)")]
    UnexpectedOpcodeOpenNonFinal,
    #[error("Unexpected opcode (partial read state)")]
    UnexpectedOpcodePartialRead,
}

#[non_exhaustive]
#[derive(Debug, Error)]
pub enum HandshakeError {
    #[error("Invalid request: {0}")]
    InvalidRequest(String),
    #[error("Missing or invalid header: {0}")]
    MissingOrInvalidHeader(String),
    #[error("Incorrect scheme for request: {0}")]
    IncorrectScheme(String),
    #[error("Request URL is missing a host")]
    NoHostInUrl,
    #[error("Unexpected response from server: {0}")]
    UnexpectedResponse(String),
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
        std::str::from_utf8(&self.bytes[2..]).map_err(|_| Error::Utf8(Utf8Error::CloseReason))
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

    pub fn as_str(&self) -> Result<&str, Error> {
        match self {
            Message::Text(text) => Ok(text.as_str()),
            Message::Binary(data) | Message::Ping(data) | Message::Pong(data) => {
                std::str::from_utf8(data.as_slice())
                    .map_err(|_| Error::Utf8(Utf8Error::TextMessage))
            }
            Message::Close(Some(close)) => close.reason(),
            Message::Close(None) => Ok(""),
        }
    }

    pub fn as_text(&self) -> Option<&str> {
        match self {
            Message::Text(text) => Some(text.as_str()),
            _ => None,
        }
    }

    pub fn is_text(&self) -> bool {
        matches!(self, Message::Text(_))
    }

    pub fn is_binary(&self) -> bool {
        matches!(self, Message::Binary(_))
    }

    pub fn is_ping(&self) -> bool {
        matches!(self, Message::Ping(_))
    }

    pub fn is_pong(&self) -> bool {
        matches!(self, Message::Pong(_))
    }

    pub fn is_close(&self) -> bool {
        matches!(self, Message::Close(_))
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

#[derive(Clone)]
pub enum Role {
    Client { url: Url },
    Server,
}

#[derive(Debug)]
enum State {
    Closed,
    NoHandshake,
    Open,
    PartialRead {
        first_opcode: Opcode,
        read_payload: Vec<u8>,
    },
}

pub const DEFAULT_MAX_HANDSHAKE_LEN: usize = 8192;
pub const DEFAULT_MAX_PAYLOAD_LEN: usize = 0x0fffffff;

pub struct WebSocket<Stream> {
    stream: FrameStream<Stream>,
    secure: bool,
    role: Role,
    state: State,
    max_handshake_len: usize,
    max_payload_len: usize,
}

impl<Stream> WebSocket<Stream>
where
    Stream: AsyncRead + AsyncWrite + Unpin,
{
    pub fn is_secure(&self) -> bool {
        self.secure
    }

    pub fn role(&self) -> &Role {
        &self.role
    }

    pub fn set_max_handshake_len(&mut self, max_handshake_len: usize) {
        self.max_handshake_len = max_handshake_len;
    }

    pub fn set_max_payload_len(&mut self, max_payload_len: usize) {
        self.max_payload_len = max_payload_len;
    }

    pub fn server(secure: bool, stream: Stream) -> Self {
        WebSocket {
            stream: FrameStream::new(BufStream::new(stream), None),
            secure,
            role: Role::Server,
            state: State::NoHandshake,
            max_handshake_len: DEFAULT_MAX_HANDSHAKE_LEN,
            max_payload_len: DEFAULT_MAX_PAYLOAD_LEN,
        }
    }

    pub fn client(url: Url, stream: Stream) -> Result<Self, Error> {
        let secure = url.scheme() == "wss";

        let mut mask_bytes = [0u8; 4];
        getrandom::getrandom(&mut mask_bytes)?;
        let mask_key = u32::from_le_bytes(mask_bytes);

        Ok(WebSocket {
            stream: FrameStream::new(BufStream::new(stream), Some(mask_key)),
            secure,
            role: Role::Client { url },
            state: State::NoHandshake,
            max_handshake_len: DEFAULT_MAX_HANDSHAKE_LEN,
            max_payload_len: DEFAULT_MAX_PAYLOAD_LEN,
        })
    }

    pub fn pinkie_promise_handshake(&mut self) {
        self.state = State::Open;
    }

    pub async fn accept(&mut self) -> Result<Url, Error> {
        match &self.role {
            Role::Client { .. } => Err(Error::IncorrectRole),
            Role::Server => {
                let url =
                    handshake::server(self.stream.inner_mut(), self.secure, self.max_handshake_len)
                        .await?;
                self.state = State::Open;
                Ok(url)
            }
        }
    }

    pub async fn connect(&mut self) -> Result<(), Error> {
        match &self.role {
            Role::Client { url } => {
                handshake::client(url, self.stream.inner_mut(), self.max_handshake_len).await?;
                self.state = State::Open;
                Ok(())
            }
            Role::Server => Err(Error::IncorrectRole),
        }
    }

    /// cancellation safe
    async fn next_frame(&mut self) -> Result<Frame, Error> {
        match self.stream.read_frame(self.max_payload_len).await? {
            Some(frame) => Ok(frame),
            None => {
                self.state = State::Closed;
                Err(Error::unexpected_close())
            }
        }
    }

    /// cancellation safe
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
            State::NoHandshake => Err(Error::NoHandshake),

            State::Open => {
                let frame = self.next_frame().await?;

                match frame.opcode() {
                    Opcode::Text | Opcode::Binary => {}
                    Opcode::Ping => return Ok(Message::Ping(frame.payload())),
                    Opcode::Pong => return Ok(Message::Pong(frame.payload())),
                    Opcode::Close => return Ok(close(frame)),
                    _ => return Err(Error::Frame(FrameError::UnexpectedOpcodeOpen)),
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

                        _ => return Err(Error::Frame(FrameError::UnexpectedOpcodeOpenNonFinal)),
                    }
                }

                match first_opcode {
                    Opcode::Text => Ok(Message::Text(
                        String::from_utf8(read_payload)
                            .map_err(|_| Error::Utf8(Utf8Error::TextMessage))?,
                    )),
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

                        _ => Err(Error::Frame(FrameError::UnexpectedOpcodePartialRead)),
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
                    Opcode::Text => Ok(Message::Text(
                        String::from_utf8(read_payload)
                            .map_err(|_| Error::Utf8(Utf8Error::TextMessagePartialRead))?,
                    )),
                    Opcode::Binary => Ok(Message::Binary(read_payload)),
                    _ => Err(Error::Bug("not text or binary in partial read")),
                }
            }
        }
    }

    /// not cancellation safe
    pub async fn write<'data>(&mut self, message: impl AsMessageRef<'data>) -> Result<(), Error> {
        match &self.state {
            State::Closed => return Err(Error::WasClosed),
            State::NoHandshake => return Err(Error::NoHandshake),
            _ => {}
        }

        let message = message.into_message_ref();
        self.stream.write_message(message).await
    }

    pub async fn flush(&mut self) -> Result<(), Error> {
        match &self.state {
            State::Closed => return Err(Error::WasClosed),
            State::NoHandshake => return Err(Error::NoHandshake),
            _ => {}
        }

        self.stream.flush().await
    }

    /// not cancellation safe
    pub async fn close(&mut self) -> Result<(), Error> {
        match &self.state {
            State::Closed => return Err(Error::WasClosed),
            State::NoHandshake => return Err(Error::NoHandshake),
            _ => {}
        }

        self.stream.write_close().await?;
        self.flush().await
    }
}
