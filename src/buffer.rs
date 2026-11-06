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

/// Remove `n` bytes from the front. O(len): prefer [`ByteQueue`] for buffers
/// that are consumed from the front repeatedly.
pub fn buf_consume(buf: &mut Buffer, n: usize) {
    buf.drain(..n);
}

/// A FIFO byte buffer with an O(1) amortized front-consume.
///
/// Consuming only advances a read offset; the dead prefix is compacted away
/// once it is at least half of the allocation, so every byte is moved at most
/// a constant number of times (instead of once per `consume`, which made
/// pipelined requests and large partial writes quadratic).
#[derive(Default, Debug)]
pub struct ByteQueue {
    buf: Vec<u8>,
    start: usize,
}

impl ByteQueue {
    /// Below this size the dead prefix is not worth compacting.
    const COMPACT_MIN: usize = 64 * 1024;

    pub fn new() -> Self {
        Self::default()
    }

    /// Unconsumed bytes.
    pub fn data(&self) -> &[u8] {
        &self.buf[self.start..]
    }

    pub fn len(&self) -> usize {
        self.buf.len() - self.start
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn append(&mut self, data: &[u8]) {
        self.buf.extend_from_slice(data);
    }

    /// The backing vector, for serializers that append in place. Only append
    /// to it (or patch bytes they appended themselves).
    pub fn writer(&mut self) -> &mut Buffer {
        &mut self.buf
    }

    /// Drop `n` bytes from the front.
    pub fn consume(&mut self, n: usize) {
        assert!(n <= self.len(), "consume past end of ByteQueue");
        self.start += n;
        if self.start == self.buf.len() {
            self.buf.clear();
            self.start = 0;
        } else if self.start >= Self::COMPACT_MIN && self.start * 2 >= self.buf.len() {
            self.buf.drain(..self.start);
            self.start = 0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_queue_fifo() {
        let mut q = ByteQueue::new();
        assert!(q.is_empty());
        q.append(b"hello world");
        q.consume(6);
        assert_eq!(q.data(), b"world");
        q.append(b"!");
        assert_eq!(q.data(), b"world!");
        q.consume(6);
        assert!(q.is_empty());

        // Large interleaved traffic still yields the right bytes.
        let mut expect = Vec::new();
        for i in 0..10_000u32 {
            let chunk = i.to_ne_bytes();
            q.append(&chunk);
            expect.extend_from_slice(&chunk);
            if i % 3 == 0 {
                let n = q.len() / 2;
                assert_eq!(&q.data()[..n], &expect[..n]);
                q.consume(n);
                expect.drain(..n);
            }
        }
        assert_eq!(q.data(), &expect[..]);
    }
}
