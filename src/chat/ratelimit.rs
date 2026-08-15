//! Client-side send limits, ported from the two source apps.
//!
//! Provenance: twi commit 7c6ad6b `internal/twitch/rate_limit.go` (sliding
//! window + duplicate suppression) and yc commit 9e67efd
//! `internal/youtube/send.go` (token bucket). Both apps decline a send
//! *locally, before dispatch* — on Twitch because the server silently drops
//! over-limit messages, on YouTube because an insert is charged its (estimated
//! 50-unit) quota cost whether it succeeds or not, so learning the limit by
//! hitting it is the most expensive way to find it. A rejection always says
//! how long to wait, not just "no".

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

/// Why a send was declined without touching the network.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendDenied {
    /// Too many messages in the window; retry after the given wait.
    RateLimited(Duration),
    /// Identical to a recent message to the same target.
    Duplicate,
}

/// Twitch's chat limit: 20 messages per 30 seconds, tracked as a sliding
/// window of send timestamps. Deliberately conservative — twi applies the
/// plain-user limit even for moderators/broadcasters, and so does this port.
#[derive(Debug)]
pub struct SlidingWindow {
    limit: usize,
    window: Duration,
    stamps: VecDeque<Instant>,
}

/// Twitch chat send limit (messages per window).
pub const TWITCH_SEND_LIMIT: usize = 20;
/// Twitch chat send window.
pub const TWITCH_SEND_WINDOW: Duration = Duration::from_secs(30);
/// How long an identical message to the same target is suppressed.
pub const DUPLICATE_WINDOW: Duration = Duration::from_secs(30);

impl SlidingWindow {
    pub fn new(limit: usize, window: Duration) -> Self {
        Self {
            limit: limit.max(1),
            window,
            stamps: VecDeque::new(),
        }
    }

    /// Twitch defaults.
    pub fn twitch() -> Self {
        Self::new(TWITCH_SEND_LIMIT, TWITCH_SEND_WINDOW)
    }

    /// Record a send at `now` if the window allows one, else say how long to
    /// wait for the oldest stamp to age out.
    pub fn acquire(&mut self, now: Instant) -> Result<(), SendDenied> {
        while let Some(&oldest) = self.stamps.front() {
            if now.duration_since(oldest) >= self.window {
                self.stamps.pop_front();
            } else {
                break;
            }
        }
        if self.stamps.len() >= self.limit {
            // Panics cannot happen: the branch above guarantees a front stamp
            // younger than the window.
            let oldest = *self.stamps.front().expect("limit > 0 implies a stamp");
            let wait = self.window - now.duration_since(oldest);
            return Err(SendDenied::RateLimited(wait));
        }
        self.stamps.push_back(now);
        Ok(())
    }
}

/// Suppresses sending the exact same text to the same target twice within the
/// window. Key = target + `\0` + text, exactly as twi keys it (for replies twi
/// includes the parent id; callers fold that into `target`).
#[derive(Debug, Default)]
pub struct DuplicateSuppressor {
    last: HashMap<String, Instant>,
}

impl DuplicateSuppressor {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record `text` sent to `target` at `now`, or refuse a duplicate.
    pub fn acquire(&mut self, target: &str, text: &str, now: Instant) -> Result<(), SendDenied> {
        let key = format!("{target}\0{text}");
        // Drop stale entries opportunistically so the map cannot grow without
        // bound over a long session.
        self.last
            .retain(|_, sent| now.duration_since(*sent) < DUPLICATE_WINDOW);
        if let Some(sent) = self.last.get(&key) {
            if now.duration_since(*sent) < DUPLICATE_WINDOW {
                return Err(SendDenied::Duplicate);
            }
        }
        self.last.insert(key, now);
        Ok(())
    }

    /// Forget a record made by [`Self::acquire`].
    ///
    /// Callers have to reserve the text *before* handing it to the transport,
    /// because that is the only way to stop two sends of the same text from
    /// racing. When the transport then fails, nothing reached the platform,
    /// so the reservation must be undone — otherwise the user is told the
    /// message was not delivered and is then refused when they retype it.
    pub fn release(&mut self, target: &str, text: &str) {
        self.last.remove(&format!("{target}\0{text}"));
    }
}

/// yc's send limiter: a token bucket with burst 3, refilling one token every
/// 2 seconds. Distinct from the sliding window because YouTube's practical
/// limit is short-burst-shaped, not window-shaped.
#[derive(Debug)]
pub struct TokenBucket {
    burst: u32,
    refill_every: Duration,
    tokens: u32,
    last_refill: Instant,
}

/// YouTube send burst size.
pub const YOUTUBE_SEND_BURST: u32 = 3;
/// YouTube send refill interval (one token per this).
pub const YOUTUBE_SEND_REFILL: Duration = Duration::from_secs(2);

impl TokenBucket {
    pub fn new(burst: u32, refill_every: Duration, now: Instant) -> Self {
        Self {
            burst: burst.max(1),
            refill_every,
            tokens: burst.max(1),
            last_refill: now,
        }
    }

    /// yc defaults.
    pub fn youtube(now: Instant) -> Self {
        Self::new(YOUTUBE_SEND_BURST, YOUTUBE_SEND_REFILL, now)
    }

    /// Take a token, or say how long until one refills.
    pub fn acquire(&mut self, now: Instant) -> Result<(), SendDenied> {
        let elapsed = now.duration_since(self.last_refill);
        if !self.refill_every.is_zero() {
            let refilled = (elapsed.as_millis() / self.refill_every.as_millis()) as u32;
            if refilled > 0 {
                self.tokens = (self.tokens.saturating_add(refilled)).min(self.burst);
                // Advance by whole refill periods so partial progress toward
                // the next token is not lost.
                self.last_refill += self.refill_every * refilled;
            }
        }
        if self.tokens == 0 {
            let wait = self
                .refill_every
                .saturating_sub(now.duration_since(self.last_refill));
            return Err(SendDenied::RateLimited(wait));
        }
        self.tokens -= 1;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sliding_window_allows_the_limit_then_refuses_with_a_wait() {
        let mut window = SlidingWindow::new(3, Duration::from_secs(30));
        let start = Instant::now();
        for i in 0..3 {
            window
                .acquire(start + Duration::from_secs(i))
                .expect("inside the limit");
        }
        let denied = window
            .acquire(start + Duration::from_secs(3))
            .expect_err("the fourth send must be declined");
        match denied {
            SendDenied::RateLimited(wait) => {
                // The oldest stamp is at t=0, so the wait is 30-3 = 27s.
                assert_eq!(wait, Duration::from_secs(27));
            }
            other => panic!("expected a rate limit, got {other:?}"),
        }
    }

    #[test]
    fn the_sliding_window_frees_a_slot_once_the_oldest_ages_out() {
        let mut window = SlidingWindow::new(2, Duration::from_secs(10));
        let start = Instant::now();
        window.acquire(start).unwrap();
        window.acquire(start + Duration::from_secs(1)).unwrap();
        assert!(window.acquire(start + Duration::from_secs(2)).is_err());
        // At t=10 the t=0 stamp is exactly window-old and expires.
        window
            .acquire(start + Duration::from_secs(10))
            .expect("a slot must have freed");
    }

    #[test]
    fn identical_text_to_the_same_target_is_suppressed_within_the_window() {
        let mut dupes = DuplicateSuppressor::new();
        let start = Instant::now();
        dupes.acquire("#chan", "hello", start).unwrap();
        assert_eq!(
            dupes.acquire("#chan", "hello", start + Duration::from_secs(5)),
            Err(SendDenied::Duplicate)
        );
        // A different channel is fine; so is the same text after the window.
        dupes
            .acquire("#other", "hello", start + Duration::from_secs(5))
            .unwrap();
        dupes
            .acquire("#chan", "hello", start + DUPLICATE_WINDOW)
            .unwrap();
    }

    /// A send that fails in the transport never reached the platform, so the
    /// user retyping the identical text must not be refused as a duplicate.
    #[test]
    fn releasing_a_failed_send_lets_the_same_text_be_retried() {
        let mut dupes = DuplicateSuppressor::new();
        let start = Instant::now();
        dupes.acquire("#chan", "hello", start).unwrap();
        dupes.release("#chan", "hello");
        dupes
            .acquire("#chan", "hello", start + Duration::from_secs(1))
            .expect("a rolled-back send must not block the retry");
        // The retry itself is still recorded, so a second copy is refused.
        assert_eq!(
            dupes.acquire("#chan", "hello", start + Duration::from_secs(2)),
            Err(SendDenied::Duplicate)
        );
    }

    /// Releasing text that was never recorded (or belongs to another target)
    /// must not disturb an unrelated live reservation.
    #[test]
    fn releasing_an_unknown_entry_leaves_other_reservations_alone() {
        let mut dupes = DuplicateSuppressor::new();
        let start = Instant::now();
        dupes.acquire("#chan", "hello", start).unwrap();
        dupes.release("#other", "hello");
        dupes.release("#chan", "goodbye");
        assert_eq!(
            dupes.acquire("#chan", "hello", start + Duration::from_secs(1)),
            Err(SendDenied::Duplicate)
        );
    }

    #[test]
    fn the_token_bucket_allows_a_burst_then_refills_one_per_interval() {
        let start = Instant::now();
        let mut bucket = TokenBucket::youtube(start);
        for _ in 0..YOUTUBE_SEND_BURST {
            bucket.acquire(start).expect("inside the burst");
        }
        assert!(matches!(
            bucket.acquire(start),
            Err(SendDenied::RateLimited(_))
        ));
        // After one refill interval exactly one send is allowed again.
        let later = start + YOUTUBE_SEND_REFILL;
        bucket.acquire(later).expect("one token refilled");
        assert!(bucket.acquire(later).is_err());
    }

    #[test]
    fn the_token_bucket_never_exceeds_its_burst_after_a_long_idle() {
        let start = Instant::now();
        let mut bucket = TokenBucket::youtube(start);
        let much_later = start + Duration::from_secs(3600);
        for _ in 0..YOUTUBE_SEND_BURST {
            bucket.acquire(much_later).expect("refilled to burst");
        }
        assert!(bucket.acquire(much_later).is_err());
    }
}
