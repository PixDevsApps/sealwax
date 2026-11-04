//! Clocks shared between processes. `CLOCK_MONOTONIC` is the same clock in every process on the
//! machine (it is also what `std::time::Instant` and `g_get_monotonic_time` read), so a timestamp
//! taken in one process can be subtracted from one taken in another. It stops during suspend;
//! `CLOCK_BOOTTIME` doesn't.

use std::time::Instant;

fn read(clock: libc::clockid_t) -> i64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `ts` is a valid, writable timespec and the clock ids are constants Linux supports.
    let rc = unsafe { libc::clock_gettime(clock, &mut ts) };
    assert_eq!(rc, 0, "clock_gettime failed");
    ts.tv_sec * 1_000_000_000 + ts.tv_nsec
}

pub fn mono_ns() -> i64 {
    read(libc::CLOCK_MONOTONIC)
}

pub fn boot_ns() -> i64 {
    read(libc::CLOCK_BOOTTIME)
}

pub fn real_ns() -> i64 {
    read(libc::CLOCK_REALTIME)
}

pub fn ms(from_ns: i64, to_ns: i64) -> f64 {
    (to_ns - from_ns) as f64 / 1e6
}

/// Converts `Instant`s (from libraries that hand them out) to `CLOCK_MONOTONIC` nanoseconds.
#[derive(Clone, Copy, Debug)]
pub struct InstantMap {
    instant: Instant,
    mono: i64,
}

impl InstantMap {
    pub fn now() -> Self {
        let instant = Instant::now();
        Self {
            instant,
            mono: mono_ns(),
        }
    }

    pub fn mono(&self, at: Instant) -> i64 {
        if at >= self.instant {
            self.mono + (at - self.instant).as_nanos() as i64
        } else {
            self.mono - (self.instant - at).as_nanos() as i64
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn monotonic_and_instant_agree() {
        let map = InstantMap::now();
        let later = Instant::now();
        let direct = mono_ns();
        let mapped = map.mono(later);
        assert!(mapped <= direct + 1_000_000, "{mapped} vs {direct}");
        assert!(direct - mapped < 50_000_000);
        let mono = mono_ns();
        assert!(boot_ns() >= mono);
    }
}
