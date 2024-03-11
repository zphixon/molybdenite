use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use sha1_smol::Sha1;
use std::{
    collections::{HashMap, VecDeque},
    io::Cursor,
    str::Utf8Error,
    string::FromUtf8Error,
};
use thiserror::Error;
use tokio::io::{
    AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader, ReadHalf,
    WriteHalf,
};
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

#[derive(Clone, Copy)]
pub enum Role {
    Client { mask: u32 },
    Server,
}

#[derive(Debug)]
pub enum State {
    SentClose,
    ReceivedClose,
    Closed,
    Open,
    PartialRead {
        first_opcode: Opcode,
        read_payload: Vec<u8>,
    },
}

pub struct WebSocket<Stream> {
    reader: FrameReader<ReadHalf<Stream>>,
    writer: WriteHalf<Stream>,
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
        self.writer.flush().await?;
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

        let (reader, writer) = tokio::io::split(stream);
        Ok(WebSocket {
            reader: FrameReader::new(reader),
            writer,
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
        let mask = u32::from_le_bytes(mask_bytes);

        let (reader, writer) = tokio::io::split(stream);
        Ok(WebSocket {
            reader: FrameReader::new(reader),
            writer,
            secure,
            role: Role::Client { mask },
            state: State::Open,
        })
    }

    pub async fn read(&mut self) -> Result<Message, Error> {
        fn got_close(frame: Frame, state: &mut State) -> Error {
            *state = State::ReceivedClose;
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

        match &mut self.state {
            State::Closed | State::ReceivedClose => return Err(Error::WasClosed),

            state @ State::SentClose => {
                let frame = self.reader.read().await?;
                match frame.opcode() {
                    Opcode::Close => {
                        let result = Err(got_close(frame, state));
                        *state = State::Closed;
                        result
                    }

                    op => Err(Error::InvalidOpcode(op)),
                }
            }

            state @ State::Open => {
                let frame = self.reader.read().await?;

                match frame.opcode() {
                    Opcode::Text | Opcode::Binary => {}
                    Opcode::Ping => return Ok(Message::Ping(frame.payload())),
                    Opcode::Pong => return Ok(Message::Pong(frame.payload())),
                    Opcode::Close => return Err(got_close(frame, state)),
                    op => return Err(Error::InvalidOpcode(op)),
                }

                let first_opcode = frame.opcode();
                let mut final_frame = frame.fin();
                let mut read_payload = frame.payload();

                while !final_frame {
                    let frame = self.reader.read().await?;
                    final_frame = frame.fin();

                    match frame.opcode() {
                        Opcode::Continuation => {
                            read_payload.extend(frame.payload());
                        }

                        Opcode::Close => return Err(got_close(frame, state)),
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

                        op => return Err(Error::InvalidOpcode(op)),
                    }
                }

                match first_opcode {
                    Opcode::Text => Ok(Message::Text(String::from_utf8(read_payload)?)),
                    Opcode::Binary => Ok(Message::Binary(read_payload)),
                    _ => unreachable!("Not text or binary"),
                }
            }

            state @ State::PartialRead { .. } => {
                let frame = self.reader.read().await?;
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
                        Opcode::Close => Err(got_close(frame, state)),

                        op => Err(Error::InvalidOpcode(op)),
                    }
                }

                if let Some(message) = extend(frame, state)? {
                    return Ok(message);
                }

                while !final_frame {
                    let frame = self.reader.read().await?;
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
                    Opcode::Text => Ok(Message::Text(String::from_utf8(read_payload)?)),
                    Opcode::Binary => Ok(Message::Binary(read_payload)),
                    _ => unreachable!("Not text or binary"),
                }
            }
        }
    }

    pub async fn write(&mut self, message: Message) -> Result<(), Error> {
        match self.state {
            State::SentClose | State::ReceivedClose | State::Closed => {
                return Err(Error::WasClosed)
            }
            _ => {}
        }

        let opcode: u8 = match &message {
            Message::Text(_) => 1,
            Message::Binary(_) => 2,
            Message::Ping(_) => 9,
            Message::Pong(_) => 10,
        };

        Frame::to_stream(
            opcode,
            match &message {
                Message::Text(text) => text.as_bytes(),
                Message::Binary(data) | Message::Ping(data) | Message::Pong(data) => {
                    data.as_slice()
                }
            },
            match self.role() {
                Role::Client { mask } => Some(mask),
                Role::Server => None,
            },
            &mut self.writer,
        )
        .await?;

        Ok(())
    }

    pub async fn close(&mut self) -> Result<(), Error> {
        if matches!(self.state, State::Closed) {
            return Err(Error::WasClosed);
        }

        Frame::to_stream(
            8,
            &[],
            match self.role() {
                Role::Client { mask } => Some(mask),
                Role::Server => None,
            },
            &mut self.writer,
        )
        .await?;

        self.state = State::SentClose;

        Ok(())
    }
}

#[rustfmt::skip]                  const FIN: u16         = 0b1000_0000_0000_0000;
#[rustfmt::skip] #[allow(unused)] const RSV1: u16        = 0b0100_0000_0000_0000;
#[rustfmt::skip] #[allow(unused)] const RSV2: u16        = 0b0010_0000_0000_0000;
#[rustfmt::skip] #[allow(unused)] const RSV3: u16        = 0b0001_0000_0000_0000;
#[rustfmt::skip]                  const OPCODE: u16      = 0b0000_1111_0000_0000;
#[rustfmt::skip]                  const MASK: u16        = 0b0000_0000_1000_0000;
#[rustfmt::skip]                  const PAYLOAD_LEN: u16 = 0b0000_0000_0111_1111;

struct FrameReader<Reader> {
    data: Vec<u8>,
    end_read: usize,
    reader: Reader,
}

impl<Reader: AsyncReadExt + Unpin> FrameReader<Reader> {
    fn new(reader: Reader) -> Self {
        FrameReader {
            data: Vec::with_capacity(4096),
            end_read: 0,
            reader,
        }
    }

    async fn read(&mut self) -> Result<Frame, Error> {
        use std::mem::size_of;

        const FIRST_SHORT_SIZE: usize = size_of::<u16>();
        const EXTENDED_PAYLOAD_SIZE: usize = size_of::<u16>();
        const BIG_EXTENDED_PAYLOAD_SIZE: usize = size_of::<u64>();
        const MASK_KEY_SIZE: usize = size_of::<u32>();

        let mut frame_size;
        let mut first_short;
        let mut mask_key;
        let mut header_size;

        'cause_got_full_frame: loop {
            'to_read_more_bytes: loop {
                let read_buf = &self.data[..self.end_read];
                if read_buf.len() < FIRST_SHORT_SIZE {
                    break 'to_read_more_bytes;
                }

                first_short = u16::from_be_bytes([read_buf[0], read_buf[1]]);
                let payload_len_size = match first_short & PAYLOAD_LEN {
                    0..=125 => 0,
                    126 => EXTENDED_PAYLOAD_SIZE,
                    127 => BIG_EXTENDED_PAYLOAD_SIZE,
                    _ => unreachable!(),
                };
                let mask_key_size = if first_short & MASK != 0 {
                    MASK_KEY_SIZE
                } else {
                    0
                };
                header_size = FIRST_SHORT_SIZE + payload_len_size + mask_key_size;
                if read_buf.len() < header_size {
                    break 'to_read_more_bytes;
                }

                let payload_len = match payload_len_size {
                    0 => (first_short & PAYLOAD_LEN) as u64,
                    EXTENDED_PAYLOAD_SIZE => u16::from_be_bytes([
                        read_buf[FIRST_SHORT_SIZE + 0],
                        read_buf[FIRST_SHORT_SIZE + 1],
                    ]) as u64,
                    BIG_EXTENDED_PAYLOAD_SIZE => u64::from_be_bytes([
                        read_buf[FIRST_SHORT_SIZE + 0],
                        read_buf[FIRST_SHORT_SIZE + 1],
                        read_buf[FIRST_SHORT_SIZE + 2],
                        read_buf[FIRST_SHORT_SIZE + 3],
                        read_buf[FIRST_SHORT_SIZE + 4],
                        read_buf[FIRST_SHORT_SIZE + 5],
                        read_buf[FIRST_SHORT_SIZE + 6],
                        read_buf[FIRST_SHORT_SIZE + 7],
                    ]),
                    _ => unreachable!(),
                };
                mask_key = if first_short & MASK != 0 {
                    Some(u32::from_be_bytes([
                        read_buf[FIRST_SHORT_SIZE + payload_len_size + 0],
                        read_buf[FIRST_SHORT_SIZE + payload_len_size + 1],
                        read_buf[FIRST_SHORT_SIZE + payload_len_size + 2],
                        read_buf[FIRST_SHORT_SIZE + payload_len_size + 3],
                    ]))
                } else {
                    None
                };
                if payload_len > usize::MAX as u64 {
                    todo!();
                }
                frame_size = header_size + payload_len as usize;
                if read_buf.len() < frame_size {
                    break 'to_read_more_bytes;
                }

                break 'cause_got_full_frame;
            }

            let mut buffer = Vec::with_capacity(4096);

            let read = self.reader.read_buf(&mut buffer).await?;
            self.data.extend(buffer.iter());
            self.end_read += read;
        }

        let payload = self
            .data
            .splice(header_size..frame_size, [])
            .collect::<Vec<_>>();

        self.data.splice(0..header_size, []).for_each(|_| {});
        self.end_read = self.data.len();

        Ok(Frame {
            first_short,
            mask_key,
            payload,
        })
    }
}

struct FrameWriter<Writer> {
    data: Vec<u8>,
    writer: Writer,
}

impl<Writer: AsyncWrite + Unpin> FrameWriter<Writer> {
    async fn write(frame: Frame) -> Result<(), Error> {
        Ok(())
    }
}

#[derive(Debug)]
struct Frame {
    first_short: u16,
    #[allow(unused)]
    mask_key: Option<u32>,
    payload: Vec<u8>,
}

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
    async fn to_stream(
        opcode: u8,
        payload: &[u8],
        mask: Option<u32>,
        stream: &mut (impl AsyncWrite + Unpin),
    ) -> Result<(), Error> {
        let mut bytes = Vec::with_capacity(128);

        let payload_len_byte = match payload.len() {
            0..=125 => payload.len(),
            126.. if payload.len() as u64 <= u32::MAX as u64 => 126,
            126.. => 127,
        } as u8;

        bytes.push((FIN >> 8) as u8 | opcode);
        bytes.push((if mask.is_some() { MASK as u8 } else { 0 }) | payload_len_byte);

        match payload_len_byte {
            0..=125 => {}
            126 => bytes.extend((payload.len() as u16).to_be_bytes()),
            127 => bytes.extend((payload.len() as u64).to_be_bytes()),
            _ => unreachable!("Incorrect payload len byte"),
        }

        if let Some(mask) = mask {
            bytes.extend(mask.to_be_bytes());
            let mask_bytes = mask.to_be_bytes();
            bytes.extend(payload.iter().enumerate().map(|(i, byte)| {
                let j = i % 4;
                byte ^ mask_bytes[j]
            }));
        } else {
            bytes.extend(payload);
        }

        stream.write_all(&bytes).await?;

        Ok(())
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

