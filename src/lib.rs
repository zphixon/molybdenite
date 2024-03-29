#![deny(missing_docs)]

//! Async websocket implementation

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

/// Errors that may occur
#[non_exhaustive]
#[derive(Error, Debug)]
pub enum Error {
    /// Generic Tokio I/O error
    #[error("I/O: {0}")]
    Io(#[from] tokio::io::Error),
    /// Problem generating client mask key
    #[error("Could not get random data")]
    GetRandom(getrandom::Error),
    /// Something was not valid UTF-8
    #[error("Invalid UTF-8: {0}")]
    Utf8(Utf8Error),
    /// The handshake could not be completed
    #[error("Handshake unsuccessful: {0}")]
    Handshake(HandshakeError),
    /// A frame received was incorrect
    #[error("A frame was malformed: {0}")]
    Frame(FrameError),
    /// A websocket was closed
    #[error("Tried to send or receive on a closed websocket")]
    WasClosed,
    /// Called [`WebSocket::client_handshake`] on a server, or
    /// [`WebSocket::server_handshake`] on a client
    #[error("Called `client_handshake` on a server, or `server_handshake` on a client")]
    IncorrectRole,
    /// A handshake has not carried out yet. Call
    /// [`WebSocket::client_handshake`] or [`WebSocket::server_handshake`] to do
    /// a handshake
    #[error("A handshake has not yet occurred on this websocket")]
    NoHandshake,
    #[doc(hidden)]
    #[error("This is a bug 😭 {0}")]
    Bug(&'static str),
}

/// UTF-8 decoding error
#[non_exhaustive]
#[derive(Debug, Error)]
pub enum Utf8Error {
    /// The reason supplied in the close message not valid UTF-8
    #[error("Close reason")]
    CloseReason,
    /// A [`Message::Text`] was not valid UTF-8
    #[error("Text message")]
    TextMessage,
    /// A fragmented [`Message::Text`] was not valid UTF-8. This should be
    /// considered identical to [`Utf8Error::TextMessage`].
    #[error("Text message")]
    TextMessagePartialRead,
    /// The WebSocket handshake was not valid UTF-8. The request from the client
    /// or the response from the server may cause this.
    #[error("Handshake data")]
    Handshake,
}

/// A WebSocket frame was not valid
#[non_exhaustive]
#[derive(Debug, Error)]
pub enum FrameError {
    /// A reserved opcode was sent
    #[error("Reserved opcode")]
    ReservedOpcode,
    /// A control frame (close, ping, pong) was fragmented
    #[error("Fragmented control frame")]
    FragmentedControl,
    /// The payload of a control frame (close, ping, pong) was more than 125
    /// bytes long
    #[error("Control frame too large")]
    LargeControl,
    /// A reserved bit on the frame was set
    #[error("Reserved bit set")]
    RsvSet,
    /// The frame paylaod was too long, greater than either
    /// [`DEFAULT_MAX_PAYLOAD_LEN`] or the value passed to
    /// [`WebSocket::set_max_payload_len`]
    #[error("Frame payload too long (greater than usize::MAX)")]
    FramePayloadTooLong,
    /// The frame in its entirety was too long, regardless of the configured
    /// maximum payload length
    #[error("Entire frame too long (would overflow usize)")]
    FrameTooLong,
    /// An unexpected opcode was received
    #[error("Unexpected opcode (open state)")]
    UnexpectedOpcodeOpen,
    /// An unexpected opcode was received, this essentially identical to
    /// [`FrameError::UnexpectedOpcodeOpen`]
    #[error("Unexpected opcode (open state, non-final frame)")]
    UnexpectedOpcodeOpenNonFinal,
    /// An unexpected opcode was received, this essentially identical to
    /// [`FrameError::UnexpectedOpcodeOpen`]
    #[error("Unexpected opcode (partial read state)")]
    UnexpectedOpcodePartialRead,
}

/// An error in the handshake occurred
#[non_exhaustive]
#[derive(Debug, Error)]
pub enum HandshakeError {
    /// The HTTP request line from the client was invalid
    #[error("Invalid request: {0}")]
    InvalidRequest(String),
    /// A header was missing, or its value was incorrect
    #[error("Missing or invalid header: {0}")]
    MissingOrInvalidHeader(String),
    /// The request scheme was not `ws` or `wss`
    #[error("Incorrect scheme for request: {0}")]
    IncorrectScheme(String),
    /// The request URL does not have a host (IP or domain name)
    #[error("Request URL is missing a host")]
    NoHostInUrl,
    /// The server did not respond the way we expected
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

/// The value from a close message
#[derive(Debug, PartialEq)]
pub struct Close {
    bytes: Vec<u8>,
}

impl Close {
    /// Status of a close message
    pub fn status(&self) -> u16 {
        debug_assert!(self.bytes.len() >= 2);
        u16::from_be_bytes([self.bytes[0], self.bytes[1]])
    }

    /// The reason the websocket was closed
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

/// A reference to a websocket message
///
/// Used to prevent needless data copying (other than what is necessary to write
/// it over a stream). A type implementing [`AsMessageRef`] is passed to
/// [`WebSocket::write`], this may be `MessageRef` or [`Message`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MessageRef<'data> {
    /// A (UTF-8) text message
    Text(&'data str),
    /// A binary message
    Binary(&'data [u8]),
    /// A ping message
    ///
    /// Sending a [`MessageRef::Ping`] should result in [`WebSocket::read`]
    /// eventually returning a [`Message::Pong`].
    Ping(&'data [u8]),
    /// A pong message
    Pong(&'data [u8]),
    /// A close message
    ///
    /// Sending [`MessageRef::Close`] (or [`Message::Close`]) will cause the
    /// websocket to be in a closed state. No further messages can be sent; an
    /// attempt to do so will return [`Error::WasClosed`]. This is equivalent to
    /// calling [`WebSocket::close`].
    Close(Option<&'data Close>),
}

impl<'data> MessageRef<'data> {
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

    /// Interpret the message as a [`str`]
    ///
    /// - For [`MessageRef::Text`], return the text
    /// - For [`MessageRef::Binary`], [`MessageRef::Ping`], [`MessageRef::Pong`]
    ///   - If data is not valid UTF-8, return [`Utf8Error::TextMessage`]
    ///   - If the data is UTF-8, return the data as UTF-8
    /// - Variant [`MessageRef::Close`]
    ///   - If the close reason is not valid UTF-8, return [`Utf8Error::CloseReason`]
    ///   - If the close reason is UTF-8, return the reason as UTF-8
    pub fn as_str(&self) -> Result<&'data str, Error> {
        match self {
            MessageRef::Text(text) => Ok(text),
            MessageRef::Binary(data) | MessageRef::Ping(data) | MessageRef::Pong(data) => {
                std::str::from_utf8(data).map_err(|_| Error::Utf8(Utf8Error::TextMessage))
            }
            MessageRef::Close(Some(close)) => close.reason(),
            MessageRef::Close(None) => Ok(""),
        }
    }

    /// Extract text from [`MessageRef::Text`]
    ///
    /// If the message is [`MessageRef::Text`], return the inner `&str`.
    pub fn as_text(&self) -> Option<&'data str> {
        match self {
            MessageRef::Text(text) => Some(text),
            _ => None,
        }
    }

    /// Return `true` if the message is [`MessageRef::Text`]
    pub fn is_text(&self) -> bool {
        matches!(self, MessageRef::Text(_))
    }

    /// Return `true` if the message is [`MessageRef::Binary`]
    pub fn is_binary(&self) -> bool {
        matches!(self, MessageRef::Binary(_))
    }

    /// Return `true` if the message is [`MessageRef::Ping`]
    pub fn is_ping(&self) -> bool {
        matches!(self, MessageRef::Ping(_))
    }

    /// Return `true` if the message is [`MessageRef::Pong`]
    pub fn is_pong(&self) -> bool {
        matches!(self, MessageRef::Pong(_))
    }

    /// Return `true` if the message is [`MessageRef::Close`]
    pub fn is_close(&self) -> bool {
        matches!(self, MessageRef::Close(_))
    }

    /// Transform a [`MessageRef::Ping`] into [`MessageRef::Pong`]
    pub fn pong(&self) -> Self {
        match self {
            MessageRef::Ping(data) => MessageRef::Pong(data),
            _ => *self,
        }
    }
}

/// An owned websocket message
///
/// Essentially equivalent to [`MessageRef`]. This type is returned by
/// [`WebSocket::read`].
#[derive(Debug)]
pub enum Message {
    /// A (UTF-8) text message
    Text(String),
    /// A binary message
    Binary(Vec<u8>),
    /// A ping message
    ///
    /// Sending a [`Message::Ping`] should result in [`WebSocket::read`]
    /// eventually returning a [`Message::Pong`].
    Ping(Vec<u8>),
    /// A pong message
    Pong(Vec<u8>),
    /// A close message
    ///
    /// Sending [`Message::Close`]  will cause the websocket to be in a closed
    /// state. No further messages can be sent; an attempt to do so will return
    /// [`Error::WasClosed`]. This is equivalent to calling
    /// [`WebSocket::close`].
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

    /// Interpret the message as a [`str`]
    ///
    /// Equivalent to [`MessageRef::as_str`].
    pub fn as_str(&self) -> Result<&str, Error> {
        self.as_ref().as_str()
    }

    /// Extract text from [`Message::Text`]
    ///
    /// Equivalent to [`MessageRef::as_text`]
    pub fn as_text(&self) -> Option<&str> {
        self.as_ref().as_text()
    }

    /// Return `true` if the message is [`Message::Text`]
    pub fn is_text(&self) -> bool {
        self.as_ref().is_text()
    }

    /// Return `true` if the message is [`Message::Binary`]
    pub fn is_binary(&self) -> bool {
        self.as_ref().is_binary()
    }

    /// Return `true` if the message is [`Message::Ping`]
    pub fn is_ping(&self) -> bool {
        self.as_ref().is_ping()
    }

    /// Return `true` if the message is [`Message::Pong`]
    pub fn is_pong(&self) -> bool {
        self.as_ref().is_pong()
    }

    /// Return `true` if the message is [`Message::Close`]
    pub fn is_close(&self) -> bool {
        self.as_ref().is_close()
    }

    /// Transform a [`Message::Ping`] into [`MessageRef::Pong`]
    pub fn pong(&self) -> MessageRef {
        self.as_ref().pong()
    }
}

/// Create a [`MessageRef`] from a type
pub trait AsMessageRef<'data> {
    /// Create the [`MessageRef`]
    fn as_message_ref(self) -> MessageRef<'data>;
}

impl<'data> AsMessageRef<'data> for &'data Message {
    fn as_message_ref(self) -> MessageRef<'data> {
        self.as_ref()
    }
}

impl<'data> AsMessageRef<'data> for MessageRef<'data> {
    fn as_message_ref(self) -> MessageRef<'data> {
        self
    }
}

/// A websocket role
#[derive(Clone)]
pub enum Role {
    /// The client
    ///
    /// If the role of a [`WebSocket`] is [`Role::Client`], calling
    /// [`WebSocket::client_handshake`] with a URL will begin a websocket
    /// handshake.
    Client {
        /// The URL of the request made in [`WebSocket::client`]
        url: Url,
    },
    /// The server
    ///
    /// If the role of a [`WebSocket`] is [`Role::Server`], calling
    /// [`WebSocket::server_handshake`] will listen for a websocket handshake,
    /// and return the URL requested by the client.
    Server,
}

/// State of a WebSocket instance
#[derive(Debug)]
enum State {
    /// No more messages allowed to be sent
    Closed,
    /// No handshake has taken place yet
    NoHandshake,
    /// Ready for stuff
    Open,
    /// Got a control frame between non-FIN frames
    PartialRead {
        first_opcode: Opcode,
        read_payload: Vec<u8>,
    },
}

/// The default maximum handshake length, in bytes
///
/// If the HTTP handshake is larger than this amount, the data will be silently
/// truncated.
pub const DEFAULT_MAX_HANDSHAKE_LEN: usize = 8192;
/// The default maxmimum payload length, in bytes
///
/// 268 MB.
pub const DEFAULT_MAX_PAYLOAD_LEN: usize = 0x0fffffff;
/// The default fragmentation size
///
/// Messages longer than this will be split into multiple frames.
pub const DEFAULT_FRAGMENT_SIZE: usize = DEFAULT_MAX_PAYLOAD_LEN;

/// A websocket
///
/// To get a websocket connection up and running, call [`WebSocket::server`]
/// then [`WebSocket::server_handshake`], or [`WebSocket::client`] then
/// [`WebSocket::client_handshake`] to start sending and receiving [`Message`]s.
pub struct WebSocket<Stream> {
    stream: FrameStream<Stream>,
    secure: bool,
    role: Role,
    state: State,
    max_handshake_len: usize,
    max_payload_len: usize,
    fragment_size: usize,
}

impl<Stream> WebSocket<Stream>
where
    Stream: AsyncRead + AsyncWrite + Unpin,
{
    /// Whether the websocket is "secure"
    ///
    /// Returns true if the URL passed to [`WebSocket::client`] had a scheme of
    /// `wss`. Otherwise, returns the value passed to [`WebSocket::server`].
    pub fn is_secure(&self) -> bool {
        self.secure
    }

    /// The role of this websocket
    pub fn role(&self) -> &Role {
        &self.role
    }

    /// Set the maximum handshake length
    ///
    /// The default value is [`DEFAULT_MAX_HANDSHAKE_LEN`].
    pub fn set_max_handshake_len(&mut self, max_handshake_len: usize) {
        self.max_handshake_len = max_handshake_len;
    }

    /// Set the maximum payload length
    ///
    /// The default value is [`DEFAULT_MAX_PAYLOAD_LEN`].
    pub fn set_max_payload_len(&mut self, max_payload_len: usize) {
        self.max_payload_len = max_payload_len;
    }

    /// Set the fragment size
    ///
    /// Messages larger than the given fragment size will be split into multiple
    /// frames. They will still reach their destination as a single [`Message`].
    ///
    /// # Panics
    ///
    /// This function will panic if `fragment_size` is `0`.
    pub fn set_fragment_size(&mut self, fragment_size: usize) {
        if fragment_size == 0 {
            panic!("Cannot have zero fragment size");
        }
        self.fragment_size = fragment_size;
    }

    /// Create a websocket with a [`Role::Server`] role
    ///
    /// This function does not listen for a handshake. Call
    /// [`WebSocket::server_handshake`] to wait for one. Forgetting to do this
    /// will cause [`WebSocket::read`] and [`WebSocket::write`] to return
    /// [`Error::NoHandshake`].
    pub fn server(secure: bool, stream: Stream) -> Self {
        WebSocket {
            stream: FrameStream::new(BufStream::new(stream), None),
            secure,
            role: Role::Server,
            state: State::NoHandshake,
            max_handshake_len: DEFAULT_MAX_HANDSHAKE_LEN,
            max_payload_len: DEFAULT_MAX_PAYLOAD_LEN,
            fragment_size: DEFAULT_FRAGMENT_SIZE,
        }
    }

    /// Create a websocket with a [`Role::Client`] role
    ///
    /// This function does not listen for a handshake. Call
    /// [`WebSocket::client_handshake`] and provide a URL to initiate one.
    /// Forgetting to do this will cause [`WebSocket::read`] and
    /// [`WebSocket::write`] to return [`Error::NoHandshake`].
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
            fragment_size: DEFAULT_FRAGMENT_SIZE,
        })
    }

    /// Pinkie promise that you've already done a handshake
    ///
    /// If you've already done a handshake, call this method rather than
    /// [`WebSocket::client_handshake`] or [`WebSocket::server_handshake`].
    pub fn skip_handshake(&mut self) {
        self.state = State::Open;
    }

    /// Wait for a websocket handshake
    ///
    /// A websocket with a role [`Role::Server`] will return the reqeusted URL
    /// from a client that makes a connection. A websocket with a role
    /// [`Role::Client`] will return [`Error::IncorrectRole`].
    pub async fn server_handshake(&mut self) -> Result<Url, Error> {
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

    /// Initiate a websocket handshake
    ///
    /// A websocket with a role [`Role::Client`] will initiate a handshake with
    /// the URL passed to [`WebSocket::client`].  A websocket with a role
    /// [`Role::Server`] will return [`Error::IncorrectRole`].
    pub async fn client_handshake(&mut self) -> Result<(), Error> {
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

    /// Read a message from the other end of this websocket
    ///
    /// # Cancel safety
    ///
    /// This function is cancel safe. If this method is used as an event in
    /// [`tokio::select!`] and some other branch completes first, no data will
    /// have been read.
    pub async fn read(&mut self) -> Result<Message, Error> {
        // got a close. turn it into Message::Close
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
                // wait for a frame
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
                    // read frames until we see the final frame
                    let frame = self.next_frame().await?;
                    final_frame = frame.fin();

                    match frame.opcode() {
                        Opcode::Continuation => {
                            // extend the payload while we get continuation frames
                            read_payload.extend(frame.payload());
                        }

                        Opcode::Close => return Ok(close(frame)),

                        // reading a control frame other than close puts us in
                        // partial read. keep anything we've gotten so far
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

                // done!
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
                // we were interrupted by a control frame
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

                // keep reading only continuation and control frames, extending
                // the current message's payload
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

                // done!
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

    /// Write a message to the other end of the websocket
    ///
    /// # Cancel safety
    ///
    /// This function is not cancel safe. If used as a branch in
    /// [`tokio::select!`] and another branch completes first, the `message` may
    /// have not been written completely.
    pub async fn write<'data>(&mut self, message: impl AsMessageRef<'data>) -> Result<(), Error> {
        match &self.state {
            State::Closed => return Err(Error::WasClosed),
            State::NoHandshake => return Err(Error::NoHandshake),
            _ => {}
        }

        let message = message.as_message_ref();
        self.stream
            .write_message(message, self.fragment_size)
            .await?;
        Ok(())
    }

    /// Write out any messages sitting in intermediate buffers
    ///
    /// Calling this may be necessary after calling [`WebSocket::write`].
    pub async fn flush(&mut self) -> Result<(), Error> {
        match &self.state {
            State::Closed => return Err(Error::WasClosed),
            State::NoHandshake => return Err(Error::NoHandshake),
            _ => {}
        }

        self.stream.flush().await
    }

    /// Write an empty [`Message::Close`] to the other end of the websocket
    ///
    /// This method is equivalent to calling [`WebSocket::write`] with
    /// [`MessageRef::Close`]`(&[])`, then calling [`WebSocket::flush`].
    ///
    /// # Cancel safety
    ///
    /// This function is not cancel safe. If used as a branch in
    /// [`tokio::select!`] and another branch completes first, the close message
    /// may have not been written completely.
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
