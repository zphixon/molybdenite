use crate::{Error, Message};
use bytes::BytesMut;
use std::mem::size_of;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufWriter};

pub const FIN: u16 = 0b1000_0000_0000_0000;
//pub const RSV1: u16 = 0b0100_0000_0000_0000;
//pub const RSV2: u16 = 0b0010_0000_0000_0000;
//pub const RSV3: u16 = 0b0001_0000_0000_0000;
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
#[derive(Debug, Clone, Copy)]
pub enum Opcode {
    Continuation = 0,
    Text = 1,
    Binary = 2,
    Close = 8,
    Ping = 9,
    Pong = 10,
    Reserved,
}

fn mask_payload(payload: &mut [u8], mask_key: u32) {
    let mask_bytes = mask_key.to_be_bytes();
    for (i, byte) in payload.iter_mut().enumerate() {
        let j = i % 4;
        *byte ^= mask_bytes[j];
    }
}

#[derive(Debug)]
pub struct Frame {
    first_short: u16,
    payload: Vec<u8>,
}

impl Frame {
    pub fn from_opcode_payload(
        opcode: Opcode,
        mut payload: Vec<u8>,
        mask_key: Option<u32>,
    ) -> Self {
        let mut first_short = FIN;
        first_short |= (opcode as u16) << 8;

        first_short |= match payload.len() {
            0..=SMALL_PAYLOAD_USIZE => payload.len() as u16,
            EXTENDED_PAYLOAD_USIZE.. if payload.len() <= u16::MAX as usize => EXTENDED_PAYLOAD,
            _ => BIG_EXTENDED_PAYLOAD,
        };

        if let Some(mask_key) = mask_key {
            first_short |= MASK;
            mask_payload(&mut payload, mask_key);
        }

        Frame {
            first_short,
            payload,
        }
    }

    pub fn fin(&self) -> bool {
        self.first_short & FIN != 0
    }

    pub fn payload(self) -> Vec<u8> {
        self.payload
    }

    pub fn opcode(&self) -> Opcode {
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

pub struct FrameStream<Stream> {
    stream: BufWriter<Stream>,
    buffer: BytesMut,
    mask_key: Option<u32>,
}

impl<Stream> FrameStream<Stream>
where
    Stream: AsyncRead + AsyncWrite + Unpin,
{
    pub fn new(stream: Stream, mask_key: Option<u32>) -> Self {
        FrameStream {
            stream: BufWriter::new(stream),
            buffer: BytesMut::with_capacity(4096),
            mask_key,
        }
    }

    pub async fn read_frame(&mut self) -> Result<Option<Frame>, Error> {
        loop {
            if let Some(frame) = self.parse_frame()? {
                return Ok(Some(frame));
            }

            if self.stream.read_buf(&mut self.buffer).await? == 0 {
                if self.buffer.is_empty() {
                    return Ok(None);
                } else {
                    return Err(Error::Closed(None));
                }
            }
        }
    }

    pub async fn write_message(&mut self, message: Message) -> Result<(), Error> {
        let (payload, opcode) = message.into_bytes_opcode();
        let frame = Frame::from_opcode_payload(opcode, payload, self.mask_key);
        self.write_frame(frame).await
    }

    pub async fn write_frame(&mut self, frame: Frame) -> Result<(), Error> {
        self.stream.write_u16(frame.first_short).await?;

        if 126 <= frame.payload.len() && frame.payload.len() <= u16::MAX as usize {
            self.stream.write_u16(frame.payload.len() as u16).await?;
        } else if u32::MAX as u64 <= frame.payload.len() as u64 {
            self.stream.write_u64(frame.payload.len() as u64).await?;
        }

        if let Some(mask_key) = self.mask_key {
            self.stream.write_u32(mask_key).await?;
        }

        let payload = frame.payload();
        let mut written = 0;
        while written < payload.len() {
            written += self.stream.write(&payload[written..]).await?;
        }

        Ok(())
    }

    pub fn parse_frame(&mut self) -> Result<Option<Frame>, Error> {
        if self.buffer.len() < FIRST_SHORT_SIZE {
            return Ok(None);
        }

        let first_short = u16::from_be_bytes([self.buffer[0], self.buffer[1]]);
        let payload_len_size = match first_short & PAYLOAD_LEN {
            0..=SMALL_PAYLOAD => 0,
            EXTENDED_PAYLOAD => EXTENDED_PAYLOAD_SIZE,
            BIG_EXTENDED_PAYLOAD => BIG_EXTENDED_PAYLOAD_SIZE,
            _ => unreachable!(),
        };
        let mask_key_size = if first_short & MASK != 0 {
            MASK_KEY_SIZE
        } else {
            0
        };
        let header_size = FIRST_SHORT_SIZE + payload_len_size + mask_key_size;
        if self.buffer.len() < header_size {
            return Ok(None);
        }

        let payload_len = match payload_len_size {
            0 => (first_short & PAYLOAD_LEN) as u64,
            EXTENDED_PAYLOAD_SIZE => u16::from_be_bytes([
                self.buffer[FIRST_SHORT_SIZE + 0],
                self.buffer[FIRST_SHORT_SIZE + 1],
            ]) as u64,
            BIG_EXTENDED_PAYLOAD_SIZE => u64::from_be_bytes([
                self.buffer[FIRST_SHORT_SIZE + 0],
                self.buffer[FIRST_SHORT_SIZE + 1],
                self.buffer[FIRST_SHORT_SIZE + 2],
                self.buffer[FIRST_SHORT_SIZE + 3],
                self.buffer[FIRST_SHORT_SIZE + 4],
                self.buffer[FIRST_SHORT_SIZE + 5],
                self.buffer[FIRST_SHORT_SIZE + 6],
                self.buffer[FIRST_SHORT_SIZE + 7],
            ]),
            _ => unreachable!(),
        };
        let mask_key = if first_short & MASK != 0 {
            Some(u32::from_be_bytes([
                self.buffer[FIRST_SHORT_SIZE + payload_len_size + 0],
                self.buffer[FIRST_SHORT_SIZE + payload_len_size + 1],
                self.buffer[FIRST_SHORT_SIZE + payload_len_size + 2],
                self.buffer[FIRST_SHORT_SIZE + payload_len_size + 3],
            ]))
        } else {
            None
        };
        if payload_len > usize::MAX as u64 {
            todo!();
        }
        let frame_size = header_size + payload_len as usize;
        if self.buffer.len() < frame_size {
            return Ok(None);
        }

        let whole_frame = self.buffer.split_to(frame_size);
        let mut payload = whole_frame[header_size..frame_size].to_vec();

        if let Some(mask_key) = mask_key {
            mask_payload(&mut payload, mask_key);
        }

        Ok(Some(Frame {
            first_short,
            payload,
        }))
    }

    pub async fn write_close(&mut self) -> Result<(), Error> {
        let mut first_short = 0b1000_1000_0000_0000;

        if self.mask_key.is_some() {
            first_short |= MASK;
        }

        self.write_frame(Frame {
            first_short,
            payload: vec![],
        })
        .await
    }
}
