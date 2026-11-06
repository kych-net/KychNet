use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// 允许同时跟踪的 key 数量上限。超过后按「最早到期的四分之一」淘汰。
///
/// 有这个上限才不会反过来变成 DoS：限流器自己不能被海量不同 key 撑爆。
const MAX_KEYS: usize = 20_000;

/// 进程内固定窗口限流器。
///
/// 只覆盖「单个实例内的暴力尝试」：进程重启后计数丢失，多实例部署时每台各自计数。
/// 需要跨实例统一限流请换 Redis 之类的集中存储，不要指望这里。
///
/// 之所以自己写而不是引第三方 crate：这里只需要「按 key 计数 + 超时清零」，
/// 而且必须给它加一个 key 数量上限（现成限流库大多没有）。
#[derive(Clone, Default)]
pub struct RateLimiter {
    slots: Arc<Mutex<HashMap<String, Window>>>,
}

struct Window {
    count: u32,
    until: Instant,
}

impl RateLimiter {
    /// 记一次请求。放行返回 `None`；超额返回 `Some(还需等待多久)`。
    ///
    /// 超限时窗口不会顺延，所以等待时间不会随着攻击者继续尝试而无限拉长。
    pub fn check(&self, key: &str, max: u32, window: Duration) -> Option<Duration> {
        let now = Instant::now();
        // 中毒的锁也要继续工作：限流器宁可退化成「按当前计数判断」，
        // 也不能因为别处 panic 就 panic 或永久放行。
        let mut slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());

        if slots.len() >= MAX_KEYS {
            evict_expired(&mut slots, now);
            if slots.len() >= MAX_KEYS {
                evict_oldest(&mut slots);
            }
        }

        let slot = slots.entry(key.to_owned()).or_insert(Window {
            count: 0,
            until: now + window,
        });
        if now >= slot.until {
            slot.count = 0;
            slot.until = now + window;
        }
        slot.count += 1;

        if slot.count > max {
            Some(slot.until.saturating_duration_since(now))
        } else {
            None
        }
    }

    /// 清掉某个 key 的计数。用于「验证成功后解除锁定」。
    pub fn reset(&self, key: &str) {
        let mut slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        slots.remove(key);
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.slots.lock().unwrap_or_else(|e| e.into_inner()).len()
    }
}

fn evict_expired(slots: &mut HashMap<String, Window>, now: Instant) {
    slots.retain(|_, window| now < window.until);
}

/// 表满且没有过期项时，淘汰最早到期的四分之一，给新 key 腾位置。
fn evict_oldest(slots: &mut HashMap<String, Window>) {
    let mut order: Vec<(Instant, String)> = slots
        .iter()
        .map(|(key, window)| (window.until, key.clone()))
        .collect();
    order.sort_unstable();
    let remove = (order.len() / 4).max(1);
    for (_, key) in order.into_iter().take(remove) {
        slots.remove(&key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_up_to_max_then_blocks() {
        let limiter = RateLimiter::default();
        for _ in 0..3 {
            assert!(limiter.check("k", 3, Duration::from_secs(60)).is_none());
        }
        let retry = limiter.check("k", 3, Duration::from_secs(60));
        assert!(retry.is_some());
        // 继续尝试不会把窗口推后。
        assert!(retry.unwrap() <= Duration::from_secs(60));
    }

    #[test]
    fn counters_are_independent_per_key() {
        let limiter = RateLimiter::default();
        assert!(limiter.check("a", 1, Duration::from_secs(60)).is_none());
        assert!(limiter.check("a", 1, Duration::from_secs(60)).is_some());
        assert!(limiter.check("b", 1, Duration::from_secs(60)).is_none());
    }

    #[test]
    fn window_resets_after_it_elapses() {
        let limiter = RateLimiter::default();
        assert!(limiter.check("k", 1, Duration::ZERO).is_none());
        assert!(limiter.check("k", 1, Duration::ZERO).is_none(), "窗口过期后应重新计数");
    }

    #[test]
    fn reset_clears_the_counter() {
        let limiter = RateLimiter::default();
        assert!(limiter.check("k", 1, Duration::from_secs(60)).is_none());
        assert!(limiter.check("k", 1, Duration::from_secs(60)).is_some());
        limiter.reset("k");
        assert!(limiter.check("k", 1, Duration::from_secs(60)).is_none());
    }

    #[test]
    fn table_stays_bounded_under_key_flooding() {
        let limiter = RateLimiter::default();
        for i in 0..(MAX_KEYS as u32 * 2) {
            limiter.check(&format!("flood-{i}"), 100, Duration::from_secs(60));
        }
        assert!(limiter.len() <= MAX_KEYS, "限流器自身被撑爆: {}", limiter.len());
    }
}
