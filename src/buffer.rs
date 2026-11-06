//! Byte buffers and append helpers, ported from `buffer.h`.

pub type Buffer = Vec<u8>;

/// Append raw bytes to the back.
pub fn buf_append(buf: &mut Buffer, data: &[u8]) {
    buf.extend_from_slice(data);
}

/// Append the native-endian bytes of an `u32`.
pub fn buf_append_u32(buf: &mut Buffer, value: u32) {
    buf.extend_from_slice(&value.to_ne_bytes());
}

/// Append the native-endian bytes of an `i64`.
pub fn buf_append_i64(buf: &mut Buffer, value: i64) {
    buf.extend_from_slice(&value.to_ne_bytes());
}

/// Append the native-endian bytes of an `f64`.
pub fn buf_append_f64(buf: &mut Buffer, value: f64) {
    buf.extend_from_slice(&value.to_ne_bytes());
}

/// Remove `n` bytes from the front. O(n), matching the C++ implementation.
pub fn buf_consume(buf: &mut Buffer, n: usize) {
    buf.drain(..n);
}
