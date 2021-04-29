#[cfg(feature = "test-utils")]
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;
#[cfg(not(feature = "test-utils"))]
use std::time::SystemTime;

#[cfg(feature = "test-utils")]
static MOCK_TIME: AtomicU64 = AtomicU64::new(0);

#[cfg(not(feature = "test-utils"))]
pub fn now() -> u64 {
    use std::time::UNIX_EPOCH;
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

#[cfg(feature = "test-utils")]
pub fn now() -> u64 {
    MOCK_TIME.load(Ordering::SeqCst)
}

#[cfg(feature = "test-utils")]
pub mod mock {
    use super::*;

    pub fn advance_time(seconds: u64) {
        MOCK_TIME.fetch_add(seconds, Ordering::SeqCst);
    }

    pub fn set_time(seconds: u64) {
        MOCK_TIME.store(seconds, Ordering::SeqCst);
    }
}

pub fn ms_since(start: &Instant) -> f32 {
    start.elapsed().as_secs_f32() * 1000.0
}

#[cfg(test)]
#[cfg(feature = "test-utils")]
mod test {
    use super::*;

    #[test]
    fn test_mock_create() {
        assert_eq!(now(), 0);
    }

    #[test]
    fn test_advance() {
        assert_eq!(now(), 0);
        mock::advance_time(10);
        assert_eq!(now(), 10);
    }
}
