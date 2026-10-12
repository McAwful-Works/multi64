//! The hex form of a cart read in `multi64d --serial-trace` lines, shared by every L2 pipe so that
//! all carts print the same thing.
//!
//! Not a crate: each pipe includes this file, the way the test-only `l2-test-support` is included.
//!
//! ```ignore
//! #[path = "../../l2-support/trace_hex.rs"]
//! mod trace_hex;
//! ```

/// `bytes` as lowercase hex: what the cart actually sent, so a garbled message can be read back
/// from a log.
pub fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

#[cfg(test)]
mod tests {
    use super::hex;

    #[test]
    fn serial_trace_hex_is_lowercase_and_padded() {
        assert_eq!(hex(&[0x00, 0xab, 0x10, 0xff]), "00ab10ff");
        assert_eq!(hex(&[]), "");
    }
}
