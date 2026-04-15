use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::Duration;

use governor::{Quota, RateLimiter};
use rand::Rng;
use tokio::sync::Mutex;
use tokio::time::{Instant, sleep};

use crate::error::{AppError, Result};
use crate::types::PacingStats;

#[derive(Debug, Clone, Copy)]
pub enum PaceBucket {
    Request,
    Download,
}

#[derive(Clone)]
pub struct Pacer {
    request_limiter: Arc<
        RateLimiter<
            governor::state::direct::NotKeyed,
            governor::state::InMemoryState,
            governor::clock::DefaultClock,
        >,
    >,
    download_limiter: Arc<
        RateLimiter<
            governor::state::direct::NotKeyed,
            governor::state::InMemoryState,
            governor::clock::DefaultClock,
        >,
    >,
    jitter_ms: u64,
    flood_sleep_threshold_secs: u64,
    max_flood_retries: u32,
    stats: Arc<Mutex<PacingStats>>,
    cooldown_until: Arc<Mutex<Option<Instant>>>,
}

impl Pacer {
    pub fn new(
        request_delay_ms: u64,
        download_delay_ms: u64,
        jitter_ms: u64,
        flood_sleep_threshold_secs: u64,
    ) -> Self {
        let burst = NonZeroU32::MIN;
        let request_quota = Quota::with_period(Duration::from_millis(request_delay_ms.max(1)))
            .unwrap_or_else(|| Quota::per_second(burst))
            .allow_burst(burst);
        let download_quota = Quota::with_period(Duration::from_millis(download_delay_ms.max(1)))
            .unwrap_or_else(|| Quota::per_second(burst))
            .allow_burst(burst);

        Self {
            request_limiter: Arc::new(RateLimiter::direct(request_quota)),
            download_limiter: Arc::new(RateLimiter::direct(download_quota)),
            jitter_ms,
            flood_sleep_threshold_secs,
            max_flood_retries: 3,
            stats: Arc::new(Mutex::new(PacingStats::default())),
            cooldown_until: Arc::new(Mutex::new(None)),
        }
    }

    pub async fn wait_for_turn(&self, bucket: PaceBucket) {
        match bucket {
            PaceBucket::Request => self.request_limiter.until_ready().await,
            PaceBucket::Download => self.download_limiter.until_ready().await,
        }

        if self.jitter_ms > 0 {
            let delay = rand::thread_rng().gen_range(0..=self.jitter_ms);
            sleep(Duration::from_millis(delay)).await;
        }

        self.wait_for_global_cooldown().await;
    }

    pub async fn wait_for_download_step(&self) {
        self.wait_for_global_cooldown().await;
    }

    pub async fn sleep_on_flood_wait(
        &self,
        operation: &str,
        seconds: i32,
        attempt: u32,
    ) -> Result<()> {
        if seconds <= 0 || attempt >= self.max_flood_retries {
            return Err(AppError::FloodWaitExceeded {
                operation: operation.to_string(),
                seconds,
            });
        }

        let sleep_secs = (seconds as u64).saturating_add(1);
        if self.flood_sleep_threshold_secs > 0 && sleep_secs > self.flood_sleep_threshold_secs {
            return Err(AppError::FloodWaitExceeded {
                operation: operation.to_string(),
                seconds,
            });
        }

        {
            let mut stats = self.stats.lock().await;
            stats.flood_wait_count += 1;
            stats.flood_sleep_ms_total += sleep_secs * 1_000;
        }

        {
            let mut cooldown = self.cooldown_until.lock().await;
            let until = Instant::now() + Duration::from_secs(sleep_secs);
            match *cooldown {
                Some(existing) if existing >= until => {}
                _ => *cooldown = Some(until),
            }
        }

        self.wait_for_global_cooldown().await;
        Ok(())
    }

    pub async fn stats(&self) -> PacingStats {
        let mut stats = self.stats.lock().await.clone();
        let now = Instant::now();
        let cooldown_until = { *self.cooldown_until.lock().await };
        stats.cooldown_active = cooldown_until.is_some_and(|until| until > now);
        stats
    }

    async fn wait_for_global_cooldown(&self) {
        loop {
            let maybe_until = { *self.cooldown_until.lock().await };
            let Some(until) = maybe_until else {
                break;
            };
            let now = Instant::now();
            if until <= now {
                let mut cooldown = self.cooldown_until.lock().await;
                if cooldown.is_some_and(|value| value <= Instant::now()) {
                    *cooldown = None;
                }
                break;
            }
            sleep(until - now).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "current_thread")]
    async fn rejects_flood_waits_above_configured_threshold() {
        let pacer = Pacer::new(1, 1, 0, 2);
        let result = pacer.sleep_on_flood_wait("test", 5, 0).await;
        assert!(matches!(result, Err(AppError::FloodWaitExceeded { .. })));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn accepts_flood_waits_when_threshold_disabled() {
        let pacer = Pacer::new(1, 1, 0, 0);
        pacer
            .sleep_on_flood_wait("test", 1, 0)
            .await
            .expect("flood wait should be honored");
        let stats = pacer.stats().await;
        assert_eq!(stats.flood_wait_count, 1);
        assert_eq!(stats.flood_sleep_ms_total, 2_000);
    }
}
