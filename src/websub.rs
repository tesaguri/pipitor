pub mod hub;

mod subscriber;

pub use subscriber::{Content, Subscriber};

use std::convert::TryInto;

use base64::display::Base64Display;

fn encode_callback_id(id: &[u8]) -> Base64Display<'_> {
    // `id` should be the byte representation of an `i64` in little endian.
    debug_assert!(id.len() == 8);

    // Strip the most significant zeroes.
    let i = id.iter().rev().position(|&b| b != 0).unwrap_or(id.len());
    let id = &id[..(id.len() - i)];
    Base64Display::with_config(id, base64::URL_SAFE_NO_PAD)
}

fn decode_callback_id(id: &str) -> Option<u64> {
    if id.len() > 11 {
        return None;
    }
    let mut buf = [0; 9];
    let n = base64::decode_config_slice(id, base64::URL_SAFE_NO_PAD, &mut buf).ok()?;
    if n > 8 {
        return None;
    }
    Some(u64::from_le_bytes(buf[..8].try_into().unwrap()))
}
