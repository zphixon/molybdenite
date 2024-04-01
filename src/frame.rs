use crate::{Error, FrameError, Message, MessageRef};
use bytes::BytesMut;
use std::mem::size_of;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufStream};

pub const FIN: u16 = 0b1000_0000_0000_0000;
pub const RSV1: u16 = 0b0100_0000_0000_0000;
pub const RSV2: u16 = 0b0010_0000_0000_0000;
pub const RSV3: u16 = 0b0001_0000_0000_0000;
pub const OPCODE: u16 = 0b0000_1111_0000_0000;
pub const MASK: u16 = 0b0000_0000_1000_0000;
pub const PAYLOAD_LEN: u16 = 0b0000_0000_0111_1111;
pub const SMALL_PAYLOAD: u16 = 0b0000_0000_0111_1101;
pub const SMALL_PAYLOAD_USIZE: usize = SMALL_PAYLOAD as usize;
pub const EXTENDED_PAYLOAD: u16 = 0b0000_0000_0111_1110;
pub const EXTENDED_PAYLOAD_USIZE: usize = EXTENDED_PAYLOAD as usize;
pub const BIG_EXTENDED_PAYLOAD: u16 = 0b0000_0000_0111_1111;
pub const FIRST_SHORT_SIZE: usize = size_of::<u16>();
pub const EXTENDED_PAYLOAD_SIZE: usize = size_of::<u16>();
pub const BIG_EXTENDED_PAYLOAD_SIZE: usize = size_of::<u64>();
pub const MASK_KEY_SIZE: usize = size_of::<u32>();

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Opcode {
    Continuation = 0,
    Text = 1,
    Binary = 2,
    Close = 8,
    Ping = 9,
    Pong = 10,
    Reserved,
}

impl Opcode {
    fn is_control(&self) -> bool {
        matches!(self, Opcode::Close | Opcode::Ping | Opcode::Pong)
    }

    pub fn ping_pong_message(&self, payload: Vec<u8>) -> Result<Message, Error> {
        match self {
            Opcode::Ping => Ok(Message::Ping(payload)),
            Opcode::Pong => Ok(Message::Pong(payload)),
            _ => Err(Error::Bug("ping pong opcode not ping or pong?")),
        }
    }
}

fn unmask_payload(payload: &mut [u8], mask_key: u32) {
    let mask_bytes = mask_key.to_be_bytes();
    for (i, byte) in payload.iter_mut().enumerate() {
        let j = i % 4;
        *byte ^= mask_bytes[j];
    }
}

/// unknown if cancel safe
async fn write_masked(
    data: &[u8],
    mask_key: u32,
    writer: &mut (impl AsyncWrite + Unpin),
) -> Result<(), Error> {
    let mask_bytes = mask_key.to_be_bytes();
    for (i, byte) in data.iter().enumerate() {
        let j = i % 4;
        writer.write_u8(*byte ^ mask_bytes[j]).await?;
    }
    Ok(())
}

fn first_short(fin: bool, opcode: Opcode, mask_key: Option<u32>, payload: &[u8]) -> u16 {
    let mut first_short = if fin { FIN } else { 0 };
    first_short |= (opcode as u16) << 8;

    first_short |= match payload.len() {
        0..=SMALL_PAYLOAD_USIZE => payload.len() as u16,
        EXTENDED_PAYLOAD_USIZE.. if payload.len() <= u16::MAX as usize => EXTENDED_PAYLOAD,
        _ => BIG_EXTENDED_PAYLOAD,
    };

    if mask_key.is_some() {
        first_short |= MASK;
    }

    first_short
}

fn num_to_opcode(n: usize) -> Opcode {
    match n {
        0 => Opcode::Continuation,
        1 => Opcode::Text,
        2 => Opcode::Binary,
        8 => Opcode::Close,
        9 => Opcode::Ping,
        10 => Opcode::Pong,
        _ => Opcode::Reserved,
    }
}

fn short_to_opcode(short: u16) -> Opcode {
    num_to_opcode(((short & OPCODE) >> 8) as usize)
}

fn rsv_set(short: u16) -> bool {
    short & (RSV1 | RSV2 | RSV3) != 0
}

#[derive(Debug)]
pub struct Frame {
    first_short: u16,
    payload: Vec<u8>,
}

impl Frame {
    pub fn fin(&self) -> bool {
        self.first_short & FIN != 0
    }

    pub fn payload(self) -> Vec<u8> {
        self.payload
    }

    pub fn opcode(&self) -> Opcode {
        short_to_opcode(self.first_short)
    }
}

pub struct FrameStream<Stream> {
    stream: BufStream<Stream>,
    buffer: BytesMut,
    mask_key: Option<u32>,
}

impl<Stream> FrameStream<Stream>
where
    Stream: AsyncRead + AsyncWrite + Unpin,
{
    pub fn inner_mut(&mut self) -> &mut BufStream<Stream> {
        &mut self.stream
    }

    pub fn new(stream: BufStream<Stream>, mask_key: Option<u32>) -> Self {
        FrameStream {
            stream,
            buffer: BytesMut::with_capacity(4096),
            mask_key,
        }
    }

    pub async fn flush(&mut self) -> Result<(), Error> {
        self.stream.flush().await?;
        Ok(())
    }

    /// cancellation safe
    pub async fn read_frame(&mut self, max_len: usize) -> Result<Option<Frame>, Error> {
        loop {
            if let Some(frame) = parse_frame(&mut self.buffer, max_len)? {
                return Ok(Some(frame));
            }

            if self.stream.read_buf(&mut self.buffer).await? == 0 {
                if self.buffer.is_empty() {
                    return Ok(None);
                } else {
                    return Err(Error::unexpected_close());
                }
            }
        }
    }

    /// not cancellation safe
    pub async fn write_message(
        &mut self,
        message: MessageRef<'_>,
        fragment_size: usize,
    ) -> Result<(), Error> {
        // Number of frames that need to be written = ceil(len / frag_size)
        let must_write = (message.payload().len() as f32 / fragment_size as f32).ceil() as usize;
        if must_write <= 1 || message.opcode().is_control() {
            // We only need one, or it's a control frame which cannot be fragmented
            self.write_frame(
                first_short(true, message.opcode(), self.mask_key, message.payload()),
                message.payload(),
            )
            .await?;
            return Ok(());
        }

        let mut start = 0;
        let mut written = 0;
        while written < must_write {
            let partial_payload =
                &message.payload()[start..(start + fragment_size).min(message.payload().len())];

            let first_short = if written == 0 {
                // First frame is normal opcode and !FIN
                first_short(false, message.opcode(), self.mask_key, partial_payload)
            } else if written + 1 != must_write {
                // Frames between first and last are continuation and !FIN
                first_short(false, Opcode::Continuation, self.mask_key, partial_payload)
            } else {
                // Last frame is continuation and FIN
                first_short(true, Opcode::Continuation, self.mask_key, partial_payload)
            };

            self.write_frame(first_short, partial_payload).await?;
            start += fragment_size;
            written += 1;
        }

        Ok(())
    }

    /// not cancellation safe
    async fn write_frame(&mut self, first_short: u16, payload: &[u8]) -> Result<(), Error> {
        self.stream.write_u16(first_short).await?;

        if payload.len() <= SMALL_PAYLOAD_USIZE {
            // :)
        } else if SMALL_PAYLOAD_USIZE < payload.len() && payload.len() <= u16::MAX as usize {
            self.stream.write_u16(payload.len() as u16).await?;
        } else {
            self.stream.write_u64(payload.len() as u64).await?;
        }

        if let Some(mask_key) = self.mask_key {
            self.stream.write_u32(mask_key).await?;
            write_masked(payload, mask_key, &mut self.stream).await?;
        } else {
            let mut written = 0;
            while written < payload.len() {
                written += self.stream.write(&payload[written..]).await?;
            }
        }

        Ok(())
    }

    /// not cancellation safe
    pub async fn write_close(&mut self) -> Result<(), Error> {
        let mut first_short = 0b1000_1000_0000_0000;

        if self.mask_key.is_some() {
            first_short |= MASK;
        }

        self.stream.write_u16(first_short).await?;

        if let Some(mask_key) = self.mask_key {
            self.stream.write_u32(mask_key).await?;
        }

        Ok(())
    }
}

pub fn parse_frame(buffer: &mut BytesMut, max_len: usize) -> Result<Option<Frame>, Error> {
    if buffer.len() < FIRST_SHORT_SIZE {
        return Ok(None);
    }

    let first_short = u16::from_be_bytes([buffer[0], buffer[1]]);
    let payload_len_size = match first_short & PAYLOAD_LEN {
        0..=SMALL_PAYLOAD => 0,
        EXTENDED_PAYLOAD => EXTENDED_PAYLOAD_SIZE,
        BIG_EXTENDED_PAYLOAD => BIG_EXTENDED_PAYLOAD_SIZE,
        _ => return Err(Error::Bug("unexpected payload len")),
    };
    let mask_key_size = if first_short & MASK != 0 {
        MASK_KEY_SIZE
    } else {
        0
    };
    let header_size = FIRST_SHORT_SIZE + payload_len_size + mask_key_size;
    let opcode = short_to_opcode(first_short);
    if Opcode::Reserved == opcode {
        return Err(Error::Frame(FrameError::ReservedOpcode));
    }
    if opcode.is_control() {
        if first_short & FIN == 0 {
            return Err(Error::Frame(FrameError::FragmentedControl));
        }
        if payload_len_size != 0 {
            return Err(Error::Frame(FrameError::LargeControl));
        }
    }
    if rsv_set(first_short) {
        return Err(Error::Frame(FrameError::RsvSet));
    }
    if buffer.len() < header_size {
        return Ok(None);
    }

    let payload_len = match payload_len_size {
        0 => (first_short & PAYLOAD_LEN) as u64,
        EXTENDED_PAYLOAD_SIZE => {
            u16::from_be_bytes([buffer[FIRST_SHORT_SIZE], buffer[FIRST_SHORT_SIZE + 1]]) as u64
        }
        BIG_EXTENDED_PAYLOAD_SIZE => u64::from_be_bytes([
            buffer[FIRST_SHORT_SIZE],
            buffer[FIRST_SHORT_SIZE + 1],
            buffer[FIRST_SHORT_SIZE + 2],
            buffer[FIRST_SHORT_SIZE + 3],
            buffer[FIRST_SHORT_SIZE + 4],
            buffer[FIRST_SHORT_SIZE + 5],
            buffer[FIRST_SHORT_SIZE + 6],
            buffer[FIRST_SHORT_SIZE + 7],
        ]),
        _ => return Err(Error::Bug("unexpected payload len size")),
    };
    let mask_key = if first_short & MASK != 0 {
        Some(u32::from_be_bytes([
            buffer[FIRST_SHORT_SIZE + payload_len_size],
            buffer[FIRST_SHORT_SIZE + payload_len_size + 1],
            buffer[FIRST_SHORT_SIZE + payload_len_size + 2],
            buffer[FIRST_SHORT_SIZE + payload_len_size + 3],
        ]))
    } else {
        None
    };
    if payload_len > max_len as u64 {
        return Err(Error::Frame(FrameError::FramePayloadTooLong));
    }
    let frame_size = header_size
        .checked_add(payload_len as usize)
        .ok_or(Error::Frame(FrameError::FrameTooLong))?;
    if buffer.len() < frame_size {
        return Ok(None);
    }

    let whole_frame = buffer.split_to(frame_size);
    let mut payload = whole_frame[header_size..frame_size].to_vec();

    if let Some(mask_key) = mask_key {
        unmask_payload(&mut payload, mask_key);
    }

    Ok(Some(Frame {
        first_short,
        payload,
    }))
}
