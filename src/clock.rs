//! Monotonic clock helper, ported from `clock.h`.

use std::sync::OnceLock;
use std::time::Instant;

fn start() -> Instant {
    static START: OnceLock<Instant> = OnceLock::new();
    *START.get_or_init(Instant::now)
}

/// Milliseconds on a clock that never goes backwards.
pub fn monotonic_ms() -> u64 {
    start().elapsed().as_millis() as u64
}
