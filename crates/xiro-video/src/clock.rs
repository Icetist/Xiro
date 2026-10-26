//! Host clock for frame timestamps and ping/pong sync: Unix epoch milliseconds that
//! advance monotonically, so wall-clock adjustments never make frames jump.

use std::sync::OnceLock;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

static ORIGIN: OnceLock<(Instant, f64)> = OnceLock::new();

/// Milliseconds since the Unix epoch, with sub-millisecond precision.
pub fn now_ms() -> f64 {
    let (start, epoch_ms) = *ORIGIN.get_or_init(|| {
        let epoch = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
        (Instant::now(), epoch.as_secs_f64() * 1000.0)
    });
    epoch_ms + start.elapsed().as_secs_f64() * 1000.0
}

/// Host time of an earlier instant.
pub fn at(instant: Instant) -> f64 {
    let now = Instant::now();
    now_ms() - now.saturating_duration_since(instant).as_secs_f64() * 1000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_epoch_based_and_monotonic() {
        let a = now_ms();
        let b = now_ms();
        assert!(b >= a);
        let wall = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs_f64() * 1000.0;
        assert!((a - wall).abs() < 1000.0);
    }

    #[test]
    fn past_instants_map_to_earlier_times() {
        let earlier = Instant::now();
        std::thread::sleep(std::time::Duration::from_millis(5));
        assert!(at(earlier) <= now_ms() - 4.0);
    }
}
