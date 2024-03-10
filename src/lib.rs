use base64::Engine;
use sha1_smol::Sha1;
use std::{collections::HashMap, str::Utf8Error};
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

impl From<getrandom::Error> for Error {
    fn from(error: getrandom::Error) -> Self {
        Error::GetRandom(error)
    }
}

#[derive(Error, Debug)]
pub enum ReceiveError {
    #[error("The opcode was not expected at this time: {0:?}")]
    InvalidOpcode(Opcode),
}

#[derive(Clone, Copy)]
pub enum Role {
    Client,
    Server,
}

pub struct WebSocket<Stream> {
    stream: Stream,
    secure: bool,
    role: Role,
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
        if response.ends_with(CRLF_CRLF){
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
        let key_base64 = base64::engine::general_purpose::STANDARD.encode(key_bytes);

        let request = format!(
            concat!(
                "GET {} HTTP/1.1\r\n",
                "Host: {}:{}\r\n",
                "Connection: Upgrade\r\n",
                "Upgrade: websocket\r\n",
                "Sec-WebSocket-Key: {}\r\n",
                "Sec-WebSocket-Version: 13\r\n",
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
        let expect_sec_websocket_accept_base64 = base64::engine::general_purpose::STANDARD
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
        })
    }

    pub async fn read(&mut self) -> Result<(), Error> {
        let frame = Frame::from_stream(&mut self.stream).await?;

        let op = frame.opcode();
        match op {
            Opcode::Text | Opcode::Binary | Opcode::Ping | Opcode::Pong => {}
            Opcode::Close => todo!(),
            op => return Err(Error::ReceiveError(ReceiveError::InvalidOpcode(op))),
        }

        let mut fin = frame.fin();
        let mut payload = frame.payload();

        while !fin {
            let frame = Frame::from_stream(&mut self.stream).await?;
            match frame.opcode() {
                Opcode::Continuation => {}
                Opcode::Ping | Opcode::Pong | Opcode::Close => todo!(),
                op => return Err(Error::ReceiveError(ReceiveError::InvalidOpcode(op))),
            }
            fin = frame.fin();
            payload.extend(frame.payload().into_iter());
        }

        todo!("payload: {:?}", String::from_utf8_lossy(&payload));
    }
}

#[derive(Debug)]
struct Frame {
    first_short: u16,
    mask_key: Option<u32>,
    payload: Vec<u8>,
}

const FIN: u16 = 0b1000_0000_0000_0000;
const RSV1: u16 = 0b0100_0000_0000_0000;
const RSV2: u16 = 0b0010_0000_0000_0000;
const RSV3: u16 = 0b0001_0000_0000_0000;
const MASK: u16 = 0b0000_1000_0000_0000;
const OPCODE: u16 = 0b0000_0111_1000_0000;
const PAYLOAD_LEN: u16 = 0b0000_0000_0111_1111;

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
        match (self.first_short & OPCODE) >> 7 {
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
