//! Shared helpers, ported from the C++ `common.h`.
//!
//! The C++ project uses `container_of()` plus intrusive nodes embedded in the
//! payload. Safe Rust cannot take that approach, so every container here stores
//! stable indices into an arena instead. The algorithms (chaining, rotations,
//! progressive rehashing, heap back-references) are unchanged.

/// FNV-1 style hash, identical to the C++ `str_hash()`.
pub fn str_hash(data: &[u8]) -> u64 {
    let mut h: u32 = 0x811C_9DC5;
    for &byte in data {
        h = h.wrapping_add(u32::from(byte)).wrapping_mul(0x0100_0193);
    }
    u64::from(h)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_and_spreads_keys() {
        assert_eq!(str_hash(b"hello"), str_hash(b"hello"));
        assert_ne!(str_hash(b"hello"), str_hash(b"hellp"));
        assert_ne!(str_hash(b""), str_hash(b"a"));
    }
}
