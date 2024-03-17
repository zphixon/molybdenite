#![no_main]

use libfuzzer_sys::fuzz_target;
use bytes::{BytesMut, BufMut};

fuzz_target!(|data: &[u8]| {
    let mut buffer = BytesMut::new();
    buffer.put(data);
    let _frame = molybdenite::frame::parse_frame(&mut buffer);
});
