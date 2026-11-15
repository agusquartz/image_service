use crate::config::RateLimitConfig;

use std::{
    collections::HashMap,
    sync::Mutex,
    time::{Duration, Instant},
};

const MAX_CLIENT_BUCKETS: usize = 100_000;

#[derive(Debug, Clone, Copy)]
pub enum RateClass {
    Read,
    Write,
}

#[derive(Debug)]
struct Window {
    started_at: Instant,
    requests: u32,
}

#[derive(Default)]
struct RateLimitState {
    windows: HashMap<String, Window>,
}

/// In-process, fixed-window request limiter with separate read/write budgets.
///
/// The map is capped to prevent clients rotating source addresses from
/// growing process memory without bound. Expired entries are removed when
/// the cap is reached.
pub struct RateLimiter {
    config: RateLimitConfig,
    state: Mutex<RateLimitState>,
}

impl RateLimiter {
    pub fn new(config: RateLimitConfig) -> Self {
        Self {
            config,
            state: Mutex::new(RateLimitState::default()),
        }
    }

    pub fn check_at(&self, identity: &str, class: RateClass, now: Instant) -> Result<(), u64> {
        let (limit, window) = match class {
            RateClass::Read => (self.config.read_requests, self.config.window_seconds),
            RateClass::Write => (self.config.write_requests, self.config.window_seconds),
        };
        if limit == 0 {
            return Ok(());
        }

        let duration = Duration::from_secs(window);
        let key = format!("{class:?}:{identity}");
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        if let Some(bucket) = state.windows.get_mut(&key) {
            if now.duration_since(bucket.started_at) >= duration {
                bucket.started_at = now;
                bucket.requests = 1;
                return Ok(());
            }
            if bucket.requests < limit {
                bucket.requests += 1;
                return Ok(());
            }
            let retry_after = duration.saturating_sub(now.duration_since(bucket.started_at));
            return Err(retry_after.as_secs_f64().ceil().max(1.0) as u64);
        }

        if state.windows.len() >= MAX_CLIENT_BUCKETS {
            state
                .windows
                .retain(|_, bucket| now.duration_since(bucket.started_at) < duration);
            if state.windows.len() >= MAX_CLIENT_BUCKETS {
                return Err(window.max(1));
            }
        }

        state.windows.insert(
            key,
            Window {
                started_at: now,
                requests: 1,
            },
        );
        Ok(())
    }

    pub fn check(&self, identity: &str, class: RateClass) -> Result<(), u64> {
        self.check_at(identity, class, Instant::now())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(read_requests: u32, write_requests: u32) -> RateLimitConfig {
        RateLimitConfig {
            read_requests,
            write_requests,
            window_seconds: 10,
        }
    }

    #[test]
    fn applies_separate_read_and_write_budgets_per_client() {
        let limiter = RateLimiter::new(config(2, 1));
        let start = Instant::now();

        assert!(limiter.check_at("client-a", RateClass::Read, start).is_ok());
        assert!(
            limiter
                .check_at("client-a", RateClass::Read, start + Duration::from_secs(1))
                .is_ok()
        );
        assert_eq!(
            limiter.check_at("client-a", RateClass::Read, start + Duration::from_secs(2)),
            Err(8)
        );

        assert!(
            limiter
                .check_at("client-a", RateClass::Write, start + Duration::from_secs(2))
                .is_ok()
        );
        assert_eq!(
            limiter.check_at("client-a", RateClass::Write, start + Duration::from_secs(3)),
            Err(9)
        );
        assert!(
            limiter
                .check_at("client-b", RateClass::Read, start + Duration::from_secs(3))
                .is_ok()
        );
    }

    #[test]
    fn resets_the_budget_after_the_window_expires() {
        let limiter = RateLimiter::new(config(1, 1));
        let start = Instant::now();
        assert!(limiter.check_at("client", RateClass::Read, start).is_ok());
        assert_eq!(limiter.check_at("client", RateClass::Read, start), Err(10));
        assert!(
            limiter
                .check_at("client", RateClass::Read, start + Duration::from_secs(10))
                .is_ok()
        );
    }

    #[test]
    fn zero_request_limit_disables_rate_limiting_for_that_class() {
        let limiter = RateLimiter::new(config(0, 1));
        let start = Instant::now();
        for _ in 0..10 {
            assert!(limiter.check_at("client", RateClass::Read, start).is_ok());
        }
        assert!(limiter.check_at("client", RateClass::Write, start).is_ok());
        assert!(limiter.check_at("client", RateClass::Write, start).is_err());
    }
}
