//! The YouTube live chat adapter: target parsing, the resolve ladder, the
//! quota-aware poll loop, and message sending.
//!
//! Provenance: ported from the Go reference `yc` (github.com/worxbend/yc,
//! commit 9e67efd), files `internal/youtube/model.go` (target parsing and the
//! normalized event inventory), `resolve.go` (the cheapest-first resolve
//! ladder), `list.go` + `poll.go` (the poll loop, backoff ladder, dedupe ring,
//! page-token retention, offline grace), `send.go` (grapheme cap and the
//! insert call), `quota.go` (the estimated unit ledger), and `transport.go`
//! (error classification and the redaction invariant).
//!
//! Two invariants from yc are load-bearing everywhere below:
//!
//! * **Quota is charged on dispatch.** Google charges even failed requests,
//!   so the local ledger records every request *before* the response is read.
//! * **No error string ever contains a request URL or a token.** The Data API
//!   accepts credentials in the URL, so a URL in an error message is a leaked
//!   credential. Every error below is composed from the HTTP status and
//!   Google's machine-readable `reason` only.

use std::collections::{HashSet, VecDeque};
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::Deserialize;
use tokio::sync::mpsc;
use unicode_segmentation::UnicodeSegmentation;

use crate::chat::ratelimit::{SendDenied, TokenBucket};
use crate::chat::source::{ChatCommand, ChatHandle, EventSender, TokenProvider, COMMAND_QUEUE};
use crate::chat::{
    Badge, ChatAuthor, ChatEvent, ChatKey, ChatMessage, ConnectionStatus, MembershipDetails,
    MembershipKind, MessageKind, PaidAmount, PlatformMeta, YouTubeMeta,
};

/// The real Data API base. Tests override it with a local fake server.
const API: &str = "https://www.googleapis.com/youtube/v3";

/// The live chat message length cap, in grapheme clusters (yc:
/// `MaxChatMessageRunes`). Enforced locally so an over-long draft never costs
/// the 50-unit insert charge just to be rejected.
const MAX_MESSAGE_GRAPHEMES: usize = 200;

/// How many recently seen message ids the poller remembers (yc:
/// `DedupeRingSize`). Retries, reconnects, and a retained page token all
/// re-deliver rows, so this must comfortably exceed one 2000-row page.
const DEDUPE_RING_SIZE: usize = 8000;

/// How long polling continues after the list response reports `offlineAt`
/// (yc: `OfflineGracePeriod`). Chat outlives the broadcast by a short window.
const OFFLINE_GRACE: Duration = Duration::from_secs(120);

/// Backoff ladder bounds (yc: poll.go). Rate limiting is capped higher than a
/// transient failure because the API asking us to slow down is a statement
/// about the next few minutes, while a 5xx is usually over in seconds.
const BACKOFF_TRANSIENT_CAP: Duration = Duration::from_secs(60);
const BACKOFF_RATE_LIMIT_CAP: Duration = Duration::from_secs(120);
const BACKOFF_STEP: f64 = 2.0;
const BACKOFF_FLOOR: f64 = 1.0;

/// The ±10% jitter spread on a healthy cadence, so several instances (or one
/// restarted in a loop) do not synchronize onto the same second.
const JITTER_FRACTION: f64 = 0.10;

/// Always request the documented maximum. Quota is charged per call, not per
/// item, so 2000 rows cost exactly what 200 cost and buy an order of
/// magnitude more headroom between polls.
const MAX_RESULTS_PER_POLL: u32 = 2000;

/// Estimated unit costs (yc: `DefaultCostTable`). Google publishes no cost
/// for any liveChat method; the 5/50 figures are community-observed estimates
/// consistent with the published read/write rule of thumb. videos.list and
/// channels.list are officially 1 unit each.
const COST_LIST: u64 = 5;
const COST_INSERT: u64 = 50;
/// Estimated cost of `liveChatMessages.delete` / `liveChatBans.insert`.
/// Unpublished, like the insert cost; the yc reference budgets 50 for each.
const COST_MODERATE: u64 = 50;
const COST_VIDEOS_LIST: u64 = 1;
const COST_CHANNELS_LIST: u64 = 1;

// ---------------------------------------------------------------------------
// Target parsing (yc: model.go ParseChatTarget)
// ---------------------------------------------------------------------------

/// How the user named the chat.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TargetKind {
    #[default]
    Unknown,
    VideoId,
    LiveChatId,
    ChannelId,
    Handle,
}

/// A chat the user asked for, at whatever stage of resolution it has reached.
/// The resolver fills fields in progressively, so a partially resolved target
/// is a normal state rather than an error.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ChatTarget {
    /// Exactly what the user typed, kept for display and diagnostics.
    pub raw: String,
    pub kind: TargetKind,
    pub video_id: String,
    pub channel_id: String,
    pub handle: String,
    pub live_chat_id: String,
    /// The broadcast title once known. The UI labels chats by title, never
    /// by id.
    pub title: String,
}

impl ChatTarget {
    /// Whether the target can be polled without further lookups.
    pub fn resolved(&self) -> bool {
        !self.live_chat_id.trim().is_empty()
    }

    /// The human-facing name: the broadcast title when known, otherwise
    /// whatever the user typed.
    pub fn label(&self) -> &str {
        let title = self.title.trim();
        if title.is_empty() {
            self.raw.trim()
        } else {
            title
        }
    }
}

/// Classify a user-supplied chat target without any network work.
///
/// Accepted forms, in the order they are tried: an explicit `livechat:`
/// prefix, a YouTube URL (youtube.com and friends, youtu.be), an `@handle`, a
/// `UC…` channel id, a bare live chat id (`Cg…`), an 11-character video id,
/// and finally a bare word of 3–30 characters treated as a handle — the only
/// remaining form that can still be resolved cheaply.
pub fn parse_target(raw: &str) -> anyhow::Result<ChatTarget> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        anyhow::bail!(
            "no chat target given; type a video ID, a watch URL, an @handle, or a channel ID"
        );
    }
    let mut target = ChatTarget {
        raw: trimmed.to_string(),
        ..ChatTarget::default()
    };

    // An explicit prefix skips every heuristic below, which is what a user
    // reaches for when a title or an ID is ambiguous.
    if trimmed.len() >= 9 && trimmed[..9].eq_ignore_ascii_case("livechat:") {
        target.kind = TargetKind::LiveChatId;
        target.live_chat_id = trimmed[9..].trim().to_string();
        return Ok(target);
    }

    if trimmed.contains('/') || trimmed.contains('.') {
        if let Some(mut parsed) = parse_target_url(trimmed) {
            parsed.raw = trimmed.to_string();
            return Ok(parsed);
        }
    }

    if let Some(handle) = trimmed.strip_prefix('@') {
        // Keep the '@' — channels.list?forHandle wants it and it is how the
        // user typed it.
        let _ = handle;
        target.kind = TargetKind::Handle;
        target.handle = trimmed.to_string();
    } else if is_channel_id(trimmed) {
        target.kind = TargetKind::ChannelId;
        target.channel_id = trimmed.to_string();
    } else if is_live_chat_id(trimmed) {
        // Live chat IDs are long opaque "Cg…" strings; nothing else a user
        // types is shaped like one, so a pasted ID and `livechat:` behave
        // identically.
        target.kind = TargetKind::LiveChatId;
        target.live_chat_id = trimmed.to_string();
    } else if is_video_id(trimmed) {
        target.kind = TargetKind::VideoId;
        target.video_id = trimmed.to_string();
    } else if is_bare_handle(trimmed) {
        // A bare word is treated as a handle: it is the only form left that
        // can still be resolved, and channels.list?forHandle costs 1 unit.
        target.kind = TargetKind::Handle;
        target.handle = format!("@{trimmed}");
    } else {
        anyhow::bail!(
            "unrecognized chat target; use a video ID, a watch URL, an @handle, or a channel ID"
        );
    }
    Ok(target)
}

/// Classify a YouTube URL, returning `None` for anything that is not
/// recognizably a YouTube target so the caller can fall through to the
/// bare-token heuristics rather than accepting a link to somewhere else.
fn parse_target_url(raw: &str) -> Option<ChatTarget> {
    let with_scheme;
    let candidate = if raw.contains("://") {
        raw
    } else {
        with_scheme = format!("https://{raw}");
        &with_scheme
    };
    let after_scheme = &candidate[candidate.find("://")? + 3..];
    let (host_port, path_and_query) = match after_scheme.split_once('/') {
        Some((host, rest)) => (host, rest),
        None => (after_scheme, ""),
    };
    let host = host_port
        .split(':')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    let host = host.strip_prefix("www.").unwrap_or(&host);
    let no_fragment = path_and_query.split('#').next().unwrap_or("");
    let (path, query) = match no_fragment.split_once('?') {
        Some((path, query)) => (path, query),
        None => (no_fragment, ""),
    };
    let path = path.trim_matches('/');

    let video = |id: &str| {
        Some(ChatTarget {
            kind: TargetKind::VideoId,
            video_id: id.to_string(),
            ..ChatTarget::default()
        })
    };

    match host {
        "youtu.be" => {
            return if is_video_id(path) { video(path) } else { None };
        }
        "youtube.com" | "m.youtube.com" | "music.youtube.com" | "youtube-nocookie.com" => {}
        _ => return None,
    }

    if let Some(id) = query
        .split('&')
        .find_map(|pair| pair.strip_prefix("v="))
        .map(str::trim)
    {
        if is_video_id(id) {
            return video(id);
        }
    }
    for prefix in ["live/", "shorts/", "embed/", "v/"] {
        if let Some(rest) = path.strip_prefix(prefix) {
            let id = rest.split('/').next().unwrap_or("");
            return if is_video_id(id) { video(id) } else { None };
        }
    }
    if let Some(rest) = path.strip_prefix("channel/") {
        let id = rest.split('/').next().unwrap_or("");
        if is_channel_id(id) {
            return Some(ChatTarget {
                kind: TargetKind::ChannelId,
                channel_id: id.to_string(),
                ..ChatTarget::default()
            });
        }
        return None;
    }
    for prefix in ["c/", "user/"] {
        if let Some(rest) = path.strip_prefix(prefix) {
            let name = rest.split('/').next().unwrap_or("");
            if name.is_empty() {
                return None;
            }
            return Some(ChatTarget {
                kind: TargetKind::Handle,
                handle: format!("@{}", name.trim_start_matches('@')),
                ..ChatTarget::default()
            });
        }
    }
    if path.starts_with('@') {
        let handle = path.split('/').next().unwrap_or("");
        return Some(ChatTarget {
            kind: TargetKind::Handle,
            handle: handle.to_string(),
            ..ChatTarget::default()
        });
    }
    None
}

fn is_id_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-'
}

fn is_handle_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '-'
}

/// The 11-character YouTube video id.
fn is_video_id(value: &str) -> bool {
    value.len() == 11 && value.chars().all(is_id_char)
}

/// The 24-character `UC…` channel id.
fn is_channel_id(value: &str) -> bool {
    value.len() == 24 && value.starts_with("UC") && value.chars().skip(2).all(is_id_char)
}

/// The opaque live chat id: `Cg` plus at least 24 more characters.
fn is_live_chat_id(value: &str) -> bool {
    value.len() >= 26 && value.starts_with("Cg") && value.chars().skip(2).all(is_handle_char)
}

/// A bare handle without its leading `@`.
fn is_bare_handle(value: &str) -> bool {
    (3..=30).contains(&value.len()) && value.chars().all(is_handle_char)
}

// ---------------------------------------------------------------------------
// The quota ledger (yc: quota.go, reduced to the per-session estimate)
// ---------------------------------------------------------------------------

/// A local estimate of the daily unit spend. Every dispatched request is
/// charged before its response is read, because Google charges failed
/// requests too. A limit of zero disables the ledger entirely.
///
/// The allowance is a *daily* budget: Google refills it at midnight Pacific
/// time. The ledger therefore remembers which Pacific day it is counting and
/// starts over when that day ends — without this, a session that exhausted
/// the estimate stayed parked forever, because the real quota came back at
/// midnight but the local count never did. Like the yc reference's fallback
/// path, "Pacific" is a fixed UTC−8 (no tz database dependency); being an
/// hour early during daylight-saving time only makes the estimate more
/// conservative, never less.
#[derive(Debug, Clone, Copy)]
struct QuotaLedger {
    limit: u64,
    used: u64,
    /// The Pacific calendar day (days since the epoch, UTC−8) `used` counts.
    day: i64,
}

/// The fixed offset standing in for America/Los_Angeles (yc's own fallback
/// when the tz database is unavailable).
const PACIFIC_FALLBACK_OFFSET_SECS: i64 = -8 * 3600;

/// The Pacific calendar day for `now`, as days since the Unix epoch.
fn pacific_day(now: chrono::DateTime<Utc>) -> i64 {
    (now.timestamp() + PACIFIC_FALLBACK_OFFSET_SECS).div_euclid(86_400)
}

impl QuotaLedger {
    fn new(limit: u64) -> Self {
        Self {
            limit,
            used: 0,
            day: pacific_day(Utc::now()),
        }
    }

    /// Start a fresh count when the Pacific day has rolled over.
    fn roll_over_if_due(&mut self) {
        let today = pacific_day(Utc::now());
        if today != self.day {
            self.day = today;
            self.used = 0;
        }
    }

    fn charge(&mut self, units: u64) {
        self.roll_over_if_due();
        self.used = self.used.saturating_add(units);
    }

    fn remaining(&mut self) -> u64 {
        self.roll_over_if_due();
        self.limit.saturating_sub(self.used)
    }

    /// Why polling must pause, if it must. The reserve exists so running out
    /// of read budget does not also take away the ability to send, which is
    /// the half of the client a stream owner cannot do without.
    fn pause_reason(&mut self, reserve_percent: u8) -> Option<String> {
        if self.limit == 0 {
            return None;
        }
        // No override is offered here on purpose. ctrl+r only clears the
        // reserve, so once nothing is left this pause would come straight
        // back — after another resolve-and-poll had already spent quota.
        if self.remaining() == 0 {
            return Some(
                "estimated daily API quota exhausted; polling stopped until the \
                 Pacific-midnight reset"
                    .to_string(),
            );
        }
        if reserve_percent == 0 {
            return None;
        }
        let reserve = self.limit * u64::from(reserve_percent.min(100)) / 100;
        if self.remaining() > reserve {
            return None;
        }
        Some(
            "estimated quota reserve reached; polling paused so message sending \
             keeps working. Press ctrl+r to override"
                .to_string(),
        )
    }
}

/// The shared, persisted quota estimate.
///
/// Every open YouTube chat spends from the *same* project allowance, so the
/// pollers must share one count — per-poller ledgers would each grant the
/// full daily budget. And the allowance is daily while sessions are not:
/// yc persists its ledger across restarts (internal/youtube/quota_store.go),
/// and so does this — a tiny JSON `{day, used}` file, written after every
/// charge (polls are seconds apart; the write is trivial) and reloaded on
/// startup, rolling over with the Pacific day like the in-memory count.
#[derive(Clone)]
pub struct QuotaStore {
    inner: std::sync::Arc<std::sync::Mutex<QuotaLedger>>,
    path: Option<std::path::PathBuf>,
}

#[derive(serde::Serialize, Deserialize)]
struct PersistedQuota {
    day: i64,
    used: u64,
}

impl QuotaStore {
    /// A store counting against `limit`, persisted at `path` when given.
    /// A saved count from an earlier session today is resumed; one from a
    /// previous Pacific day starts fresh.
    pub fn new(limit: u64, path: Option<std::path::PathBuf>) -> Self {
        let mut ledger = QuotaLedger::new(limit);
        if let Some(path) = &path {
            if let Ok(text) = std::fs::read_to_string(path) {
                if let Ok(saved) = serde_json::from_str::<PersistedQuota>(&text) {
                    if saved.day == ledger.day {
                        ledger.used = saved.used;
                    }
                }
            }
        }
        Self {
            inner: std::sync::Arc::new(std::sync::Mutex::new(ledger)),
            path,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, QuotaLedger> {
        // A poisoned mutex means another poller panicked mid-charge; the
        // count itself is a plain integer and still usable.
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn charge(&self, units: u64) {
        // The write happens INSIDE the lock, deliberately: two pollers
        // charging concurrently must not interleave their writes, or the
        // slower snapshot overwrites the newer one and the persisted count
        // runs backwards — a restart would then resume below what Google has
        // already charged. The file is a few dozen bytes on the config
        // filesystem and charges arrive at most once per poll interval, so
        // the held-lock write is bounded and cheap; correctness of the spend
        // record wins over microseconds of contention.
        let mut ledger = self.lock();
        ledger.charge(units);
        if let Some(path) = &self.path {
            let snapshot = PersistedQuota {
                day: ledger.day,
                used: ledger.used,
            };
            // Atomic temp+rename: a crash mid-write must leave the previous
            // complete file, never a truncated one that silently resets the
            // budget to zero on the next start. Best-effort beyond that — a
            // full disk must not cost the chat session, and the in-memory
            // count still protects this run.
            if let Ok(text) = serde_json::to_string(&snapshot) {
                let tmp = path.with_extension("json.tmp");
                if std::fs::write(&tmp, text).is_ok() {
                    let _ = std::fs::rename(&tmp, path);
                }
            }
        }
    }

    fn remaining(&self) -> u64 {
        self.lock().remaining()
    }

    fn pause_reason(&self, reserve_percent: u8) -> Option<String> {
        self.lock().pause_reason(reserve_percent)
    }
}

// ---------------------------------------------------------------------------
// The dedupe ring (yc: poll.go dedupeRing)
// ---------------------------------------------------------------------------

/// Remembers recently seen message ids in bounded space, evicting FIFO.
///
/// Deduplication is not optional: a retained page token, a retry, and a
/// reconnect all re-deliver rows, and a chat that prints the same Super Chat
/// twice is one the viewer stops trusting.
#[derive(Debug)]
struct DedupeRing {
    cap: usize,
    seen: HashSet<String>,
    order: VecDeque<String>,
}

impl DedupeRing {
    fn new(cap: usize) -> Self {
        Self {
            cap: cap.max(1),
            seen: HashSet::new(),
            order: VecDeque::new(),
        }
    }

    /// Record an id, reporting whether it is new. Empty ids are always "new"
    /// so an id-less row is never silently dropped.
    fn insert(&mut self, id: &str) -> bool {
        if id.is_empty() {
            return true;
        }
        if self.seen.contains(id) {
            return false;
        }
        if self.order.len() == self.cap {
            if let Some(evicted) = self.order.pop_front() {
                self.seen.remove(&evicted);
            }
        }
        self.order.push_back(id.to_string());
        self.seen.insert(id.to_string());
        true
    }

    /// Whether an id was seen recently, without recording it. This backs
    /// tombstone correlation: a tombstone for a message never shown has
    /// nothing on screen to redact.
    fn has(&self, id: &str) -> bool {
        !id.is_empty() && self.seen.contains(id)
    }
}

// ---------------------------------------------------------------------------
// Cadence arithmetic (yc: poll.go NextInterval / climb)
// ---------------------------------------------------------------------------

/// A cheap linear congruential generator for jitter. Jitter needs speed and
/// spread, not unpredictability, so no crypto-grade randomness is involved.
#[derive(Debug, Clone, Copy)]
struct Lcg(u64);

impl Lcg {
    fn new(seed: u64) -> Self {
        Self(seed | 1)
    }

    /// A uniform draw in [0, 1).
    fn next_f64(&mut self) -> f64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((self.0 >> 11) as f64) / ((1u64 << 53) as f64)
    }
}

/// Compute the next poll delay.
///
/// `pollingIntervalMillis` from the server is an *absolute* floor — polling
/// faster than YouTube advises is never allowed, under any circumstance. The
/// effective base is `max(server_floor, config_floor)` clamped to the config
/// ceiling when one is set (but never below the floor), scaled by the backoff
/// multiplier, then jittered ±10% — and the result is clamped back to the
/// floor so jitter can never dip below it.
fn next_interval(
    server_floor: Duration,
    config_floor: Duration,
    config_ceiling: Duration,
    backoff: f64,
    jitter: &mut Lcg,
) -> Duration {
    let floor = server_floor.max(config_floor);
    let mut base = floor;
    if !config_ceiling.is_zero() {
        // The ceiling stretches quota but must never push below the absolute
        // floor: a floor above the ceiling wins.
        base = base.min(config_ceiling.max(floor));
    }
    let scaled = base.mul_f64(backoff.max(BACKOFF_FLOOR));
    let spread = scaled.mul_f64(JITTER_FRACTION);
    let jittered = scaled - spread + spread.mul_f64(2.0).mul_f64(jitter.next_f64());
    jittered.max(floor)
}

/// Advance the backoff multiplier, capped so `base * multiplier` never
/// exceeds `ceiling`.
fn climb(backoff: f64, base: Duration, ceiling: Duration) -> f64 {
    let base = if base.is_zero() {
        Duration::from_secs(1)
    } else {
        base
    };
    let mut next = backoff * BACKOFF_STEP;
    let limit = ceiling.as_secs_f64() / base.as_secs_f64();
    if next > limit {
        next = limit;
    }
    next.max(BACKOFF_FLOOR)
}

/// A success decays the ladder one step rather than clearing it, so a
/// flapping connection does not slam straight back to full cadence.
fn decay(backoff: f64) -> f64 {
    (backoff / BACKOFF_STEP).max(BACKOFF_FLOOR)
}

// ---------------------------------------------------------------------------
// Wire types (subset of the liveChatMessages / videos / channels resources)
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireListResponse {
    #[serde(default)]
    items: Vec<WireMessage>,
    #[serde(default)]
    next_page_token: String,
    #[serde(default)]
    polling_interval_millis: u64,
    #[serde(default)]
    offline_at: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireMessage {
    #[serde(default)]
    id: String,
    #[serde(default)]
    snippet: WireSnippet,
    #[serde(default)]
    author_details: WireAuthor,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireSnippet {
    #[serde(rename = "type", default)]
    kind: String,
    #[serde(default)]
    display_message: String,
    #[serde(default)]
    published_at: String,
    /// Documented false on tombstones; absent otherwise.
    has_display_content: Option<bool>,
    text_message_details: Option<WireTextDetails>,
    super_chat_details: Option<WirePaidDetails>,
    super_sticker_details: Option<WireStickerDetails>,
    new_sponsor_details: Option<WireNewSponsor>,
    member_milestone_chat_details: Option<WireMilestone>,
    membership_gifting_details: Option<WireGifting>,
    gift_membership_received_details: Option<WireGiftReceived>,
    poll_details: Option<WirePoll>,
    user_banned_details: Option<WireBan>,
    message_deleted_details: Option<WireDeleted>,
    message_retracted_details: Option<WireRetracted>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireAuthor {
    #[serde(default)]
    channel_id: String,
    #[serde(default)]
    display_name: String,
    #[serde(default)]
    is_chat_owner: bool,
    #[serde(default)]
    is_chat_moderator: bool,
    #[serde(default)]
    is_chat_sponsor: bool,
    #[serde(default)]
    is_verified: bool,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireTextDetails {
    #[serde(default)]
    message_text: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WirePaidDetails {
    /// A uint64 sent as a JSON string; see [`parse_micros`].
    #[serde(default)]
    amount_micros: String,
    #[serde(default)]
    currency: String,
    #[serde(default)]
    amount_display_string: String,
    #[serde(default)]
    tier: u32,
    #[serde(default)]
    user_comment: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireStickerDetails {
    #[serde(default)]
    amount_micros: String,
    #[serde(default)]
    currency: String,
    #[serde(default)]
    amount_display_string: String,
    #[serde(default)]
    tier: u32,
    #[serde(default)]
    super_sticker_metadata: WireStickerMetadata,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireStickerMetadata {
    #[serde(default)]
    alt_text: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireNewSponsor {
    #[serde(default)]
    member_level_name: String,
    #[serde(default)]
    is_upgrade: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireMilestone {
    #[serde(default)]
    member_level_name: String,
    #[serde(default)]
    member_month: u32,
    #[serde(default)]
    user_comment: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireGifting {
    #[serde(default)]
    gift_memberships_count: u32,
    #[serde(default)]
    gift_memberships_level_name: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireGiftReceived {
    #[serde(default)]
    member_level_name: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WirePoll {
    #[serde(default)]
    metadata: WirePollMetadata,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WirePollMetadata {
    #[serde(default)]
    question_text: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireBan {
    #[serde(default)]
    banned_user_details: WireBannedUser,
    #[serde(default)]
    ban_type: String,
    /// A uint64 sent as a JSON string.
    #[serde(default)]
    ban_duration_seconds: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireBannedUser {
    #[serde(default)]
    channel_id: String,
    #[serde(default)]
    display_name: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireDeleted {
    #[serde(default)]
    deleted_message_id: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireRetracted {
    #[serde(default)]
    retracted_message_id: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireVideosResponse {
    #[serde(default)]
    items: Vec<WireVideo>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireVideo {
    #[serde(default)]
    id: String,
    #[serde(default)]
    snippet: WireVideoSnippet,
    #[serde(default)]
    live_streaming_details: WireLiveDetails,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireVideoSnippet {
    #[serde(default)]
    title: String,
    #[serde(default)]
    channel_id: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireLiveDetails {
    #[serde(default)]
    active_live_chat_id: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireChannelsResponse {
    #[serde(default)]
    items: Vec<WireChannel>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireChannel {
    #[serde(default)]
    id: String,
    #[serde(default)]
    snippet: WireChannelSnippet,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireChannelSnippet {
    #[serde(default)]
    title: String,
    #[serde(default)]
    custom_url: String,
}

// ---------------------------------------------------------------------------
// Normalization (yc: normalize.go)
// ---------------------------------------------------------------------------

/// Everything one wire item produced. Chat rows still have to pass the
/// dedupe ring; moderation events are always delivered.
#[derive(Debug, Default)]
struct Normalized {
    messages: Vec<ChatMessage>,
    events: Vec<ChatEvent>,
    chat_ended: bool,
}

/// Parse a string-encoded micros amount, clamping a uint64 above `i64::MAX`
/// rather than wrapping it negative. No float ever touches an amount between
/// the wire and the screen — floats corrupt currency.
fn parse_micros(value: &str) -> Option<i64> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Ok(signed) = trimmed.parse::<i64>() {
        return Some(signed);
    }
    if let Ok(unsigned) = trimmed.parse::<u64>() {
        return Some(unsigned.min(i64::MAX as u64) as i64);
    }
    None
}

/// Parse an RFC 3339 timestamp; an unreadable one becomes `None`, which the
/// renderer shows as `--:--` instead of jumping to the epoch.
fn parse_timestamp(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value.trim())
        .ok()
        .map(|parsed| parsed.with_timezone(&Utc))
}

/// Synthesize the badge set from `authorDetails` booleans in the fixed
/// order owner, moderator, member, verified (yc: `BadgesForAuthor`), with
/// the membership level name riding on the member badge when known.
fn badges_for(author: &WireAuthor, member_level: &str, force_member: bool) -> Vec<Badge> {
    let mut badges = Vec::new();
    let mut push = |set: &str, info: &str| {
        badges.push(Badge {
            set: set.to_string(),
            id: String::new(),
            info: info.to_string(),
        });
    };
    if author.is_chat_owner {
        push("owner", "");
    }
    if author.is_chat_moderator {
        push("moderator", "");
    }
    if author.is_chat_sponsor || force_member {
        push("member", member_level);
    }
    if author.is_verified {
        push("verified", "");
    }
    badges
}

/// The fields every normalized row shares.
fn base_message(
    item: &WireMessage,
    kind: MessageKind,
    text: String,
    historical: bool,
    member_level: &str,
    force_member: bool,
) -> ChatMessage {
    ChatMessage {
        id: item.id.clone(),
        timestamp: parse_timestamp(&item.snippet.published_at),
        author: ChatAuthor {
            id: item.author_details.channel_id.clone(),
            login: String::new(),
            display_name: item.author_details.display_name.clone(),
            badges: badges_for(&item.author_details, member_level, force_member),
            color_hint: None,
        },
        text,
        kind,
        deleted: false,
        historical,
        local_echo: false,
        meta: Some(PlatformMeta::YouTube(YouTubeMeta {
            raw_type: item.snippet.kind.trim().to_string(),
            paid: None,
            membership: None,
        })),
    }
}

/// Attach YouTube meta to an already built message.
fn set_meta(
    msg: &mut ChatMessage,
    paid: Option<PaidAmount>,
    membership: Option<MembershipDetails>,
) {
    if let Some(PlatformMeta::YouTube(meta)) = msg.meta.as_mut() {
        meta.paid = paid;
        meta.membership = membership;
    }
}

/// Convert one wire item into whichever normalized events it produces.
///
/// The unknown-type rule is absolute: any `snippet.type` not recognized
/// becomes a [`MessageKind::Unknown`] row with the display text and the raw
/// type preserved — never a crash, never a silent drop. Deletions and bans
/// become [`ChatEvent`]s, never chat rows, so removed text is not reprinted.
fn normalize_item(item: &WireMessage, historical: bool, seen: &DedupeRing) -> Normalized {
    let snippet = &item.snippet;
    let raw_type = snippet.kind.trim();
    let mut out = Normalized::default();

    // A tombstone is how deletion actually surfaces: a previously delivered
    // message reappearing with hasDisplayContent false, or with an empty
    // snippet. One for a message never shown has nothing on screen to redact
    // and is dropped rather than surfaced as an unactionable deletion.
    if raw_type.is_empty() || raw_type == "tombstone" || snippet.has_display_content == Some(false)
    {
        if seen.has(&item.id) {
            out.events.push(ChatEvent::MessageDeleted {
                message_id: item.id.clone(),
            });
        }
        return out;
    }

    match raw_type {
        "textMessageEvent" => {
            // The type-specific field is preferred over displayMessage
            // because displayMessage is a lossy render of it.
            let text = snippet
                .text_message_details
                .as_ref()
                .map(|details| details.message_text.trim())
                .filter(|text| !text.is_empty())
                .unwrap_or(snippet.display_message.trim())
                .to_string();
            out.messages.push(base_message(
                item,
                MessageKind::Chat,
                text,
                historical,
                "",
                false,
            ));
        }

        "superChatEvent" => {
            let details = snippet.super_chat_details.clone().unwrap_or_default();
            let text = if details.user_comment.trim().is_empty() {
                snippet.display_message.trim().to_string()
            } else {
                details.user_comment.trim().to_string()
            };
            let mut msg = base_message(item, MessageKind::Paid, text, historical, "", false);
            set_meta(
                &mut msg,
                Some(PaidAmount {
                    micros: parse_micros(&details.amount_micros).unwrap_or(0),
                    currency: details.currency.trim().to_string(),
                    display: details.amount_display_string.trim().to_string(),
                    tier: details.tier.min(u32::from(u8::MAX)) as u8,
                }),
                None,
            );
            out.messages.push(msg);
        }

        "superStickerEvent" => {
            let details = snippet.super_sticker_details.clone().unwrap_or_default();
            // The alt text is the only renderable form of a sticker: no
            // images are ever fetched.
            let text = if details.super_sticker_metadata.alt_text.trim().is_empty() {
                snippet.display_message.trim().to_string()
            } else {
                details.super_sticker_metadata.alt_text.trim().to_string()
            };
            let mut msg = base_message(item, MessageKind::Paid, text, historical, "", false);
            set_meta(
                &mut msg,
                Some(PaidAmount {
                    micros: parse_micros(&details.amount_micros).unwrap_or(0),
                    currency: details.currency.trim().to_string(),
                    display: details.amount_display_string.trim().to_string(),
                    tier: details.tier.min(u32::from(u8::MAX)) as u8,
                }),
                None,
            );
            out.messages.push(msg);
        }

        "newSponsorEvent" => {
            let details = snippet.new_sponsor_details.clone().unwrap_or_default();
            let kind = if details.is_upgrade {
                MembershipKind::Upgrade
            } else {
                MembershipKind::New
            };
            let mut msg = base_message(
                item,
                MessageKind::Membership,
                snippet.display_message.trim().to_string(),
                historical,
                &details.member_level_name,
                true,
            );
            set_meta(
                &mut msg,
                None,
                Some(MembershipDetails {
                    kind,
                    level: details.member_level_name.trim().to_string(),
                    months: 0,
                    gift_count: 0,
                }),
            );
            out.messages.push(msg);
        }

        "memberMilestoneChatEvent" => {
            let details = snippet
                .member_milestone_chat_details
                .clone()
                .unwrap_or_default();
            let text = if details.user_comment.trim().is_empty() {
                snippet.display_message.trim().to_string()
            } else {
                details.user_comment.trim().to_string()
            };
            let mut msg = base_message(
                item,
                MessageKind::Membership,
                text,
                historical,
                &details.member_level_name,
                true,
            );
            set_meta(
                &mut msg,
                None,
                Some(MembershipDetails {
                    kind: MembershipKind::Milestone,
                    level: details.member_level_name.trim().to_string(),
                    months: details.member_month,
                    gift_count: 0,
                }),
            );
            out.messages.push(msg);
        }

        "membershipGiftingEvent" => {
            let details = snippet
                .membership_gifting_details
                .clone()
                .unwrap_or_default();
            let mut msg = base_message(
                item,
                MessageKind::Membership,
                snippet.display_message.trim().to_string(),
                historical,
                "",
                false,
            );
            set_meta(
                &mut msg,
                None,
                Some(MembershipDetails {
                    kind: MembershipKind::Gifting,
                    level: details.gift_memberships_level_name.trim().to_string(),
                    months: 0,
                    gift_count: details.gift_memberships_count,
                }),
            );
            out.messages.push(msg);
        }

        "giftMembershipReceivedEvent" => {
            let details = snippet
                .gift_membership_received_details
                .clone()
                .unwrap_or_default();
            let mut msg = base_message(
                item,
                MessageKind::Membership,
                snippet.display_message.trim().to_string(),
                historical,
                &details.member_level_name,
                true,
            );
            set_meta(
                &mut msg,
                None,
                Some(MembershipDetails {
                    kind: MembershipKind::GiftReceived,
                    level: details.member_level_name.trim().to_string(),
                    months: 0,
                    gift_count: 0,
                }),
            );
            out.messages.push(msg);
        }

        "pollEvent" => {
            let question = snippet
                .poll_details
                .as_ref()
                .map(|details| details.metadata.question_text.trim())
                .unwrap_or("");
            let text = if question.is_empty() {
                "[poll]".to_string()
            } else {
                format!("[poll] {question}")
            };
            out.messages.push(base_message(
                item,
                MessageKind::Notice,
                text,
                historical,
                "",
                false,
            ));
        }

        "sponsorOnlyModeStartedEvent" | "sponsorOnlyModeEndedEvent" => {
            let fallback = if raw_type == "sponsorOnlyModeEndedEvent" {
                "members-only mode ended"
            } else {
                "members-only mode started"
            };
            let text = if snippet.display_message.trim().is_empty() {
                fallback.to_string()
            } else {
                snippet.display_message.trim().to_string()
            };
            out.messages.push(base_message(
                item,
                MessageKind::Notice,
                text,
                historical,
                "",
                false,
            ));
        }

        // chatEndedEvent carries no displayMessage and is documented as
        // silent. It is a room transition, not a chat row: the poller turns
        // it into ConnectionStatus::Closed and the history stays readable.
        "chatEndedEvent" => out.chat_ended = true,

        "messageDeletedEvent" => {
            let target = snippet
                .message_deleted_details
                .as_ref()
                .map(|details| details.deleted_message_id.clone())
                .unwrap_or_default();
            out.events
                .push(ChatEvent::MessageDeleted { message_id: target });
        }

        "messageRetractedEvent" => {
            let target = snippet
                .message_retracted_details
                .as_ref()
                .map(|details| details.retracted_message_id.clone())
                .unwrap_or_default();
            out.events
                .push(ChatEvent::MessageDeleted { message_id: target });
        }

        "userBannedEvent" => {
            let details = snippet.user_banned_details.clone().unwrap_or_default();
            let seconds = parse_micros(&details.ban_duration_seconds).unwrap_or(0);
            // A "temporary" ban with an unparseable duration still reads as a
            // timeout of unknown length rather than a permanent ban.
            let timeout = if details.ban_type.trim().eq_ignore_ascii_case("temporary") {
                Some(seconds.max(0) as u64)
            } else if seconds > 0 {
                Some(seconds as u64)
            } else {
                None
            };
            let login = if details.banned_user_details.display_name.trim().is_empty() {
                details.banned_user_details.channel_id.clone()
            } else {
                details.banned_user_details.display_name.clone()
            };
            out.events.push(ChatEvent::UserPurged {
                author_login: login,
                timeout_secs: timeout,
            });
        }

        _ => {
            // Unknown type: rendered readably from whatever display text the
            // platform supplied, raw type preserved for diagnostics.
            out.messages.push(base_message(
                item,
                MessageKind::Unknown,
                snippet.display_message.trim().to_string(),
                historical,
                "",
                false,
            ));
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Error classification (yc: errors.go, reduced to what the ladder needs)
// ---------------------------------------------------------------------------

/// The failure classes the poll loop reacts to differently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FailKind {
    /// 403 quotaExceeded / dailyLimitExceeded: never retried until reset.
    Quota,
    /// 403 rateLimitExceeded, or a 429: backoff with the higher cap.
    RateLimited,
    /// The chat is over or unreachable: a clean terminal state.
    ChatGone,
    /// A 400/404 that may be a stale continuation token.
    Rejected,
    /// A 401 or a scope problem: retrying spends quota proving the same
    /// credential is still wrong.
    Auth,
    /// 5xx or a network problem: retried on the backoff ladder.
    Transient,
}

/// A classified failure with a user-facing, already-redacted detail.
#[derive(Debug, Clone)]
struct Failure {
    kind: FailKind,
    detail: String,
}

impl Failure {
    fn transient(detail: &str) -> Self {
        Self {
            kind: FailKind::Transient,
            detail: detail.to_string(),
        }
    }
}

/// Map an HTTP status plus Google's machine-readable `reason` onto a failure
/// class. The detail is composed from these two values only, so no request
/// URL or token can ever reach an error message.
fn classify(status: u16, reason: &str) -> Failure {
    let reason_lc = reason.trim().to_ascii_lowercase();
    let kind = match reason_lc.as_str() {
        "quotaexceeded" | "dailylimitexceeded" => FailKind::Quota,
        "ratelimitexceeded" | "userratelimitexceeded" => FailKind::RateLimited,
        "livechatended" | "livechatdisabled" => FailKind::ChatGone,
        "livechatnotfound" | "videonotfound" | "channelnotfound" => FailKind::ChatGone,
        "forbidden" | "insufficientpermissions" | "autherror" | "unauthorized" => FailKind::Auth,
        "backenderror" | "internalerror" => FailKind::Transient,
        _ => match status {
            401 => FailKind::Auth,
            429 => FailKind::RateLimited,
            404 => FailKind::ChatGone,
            400 => FailKind::Rejected,
            500.. => FailKind::Transient,
            // Anything unlisted (402, 405, 408, 409, a stray 3xx) is not
            // evidence the saved login is wrong. Calling it Auth parks the
            // poller for good and tells the user to log in again under Config →
            // youtube`, which fixes nothing; Transient at least retries.
            _ => FailKind::Transient,
        },
    };
    let detail = match kind {
        // As with the ledger's own exhaustion pause: the override cannot buy
        // back a quota YouTube itself says is spent, so it is not advertised.
        FailKind::Quota => {
            "daily API quota exhausted; polling stopped until the Pacific-midnight reset"
                .to_string()
        }
        FailKind::RateLimited => "YouTube asked us to slow down; backing off".to_string(),
        FailKind::ChatGone => match reason_lc.as_str() {
            "livechatdisabled" => "this broadcast has chat turned off".to_string(),
            "livechatended" => "the live chat has ended".to_string(),
            _ => "the live chat was not found; the broadcast may be over".to_string(),
        },
        FailKind::Auth => {
            "YouTube refused the saved login; log in again under Config → Accounts".to_string()
        }
        FailKind::Rejected => format!("YouTube rejected the request (HTTP {status} {reason})"),
        FailKind::Transient => format!("YouTube had a temporary problem (HTTP {status})"),
    };
    Failure { kind, detail }
}

/// Read a failed response's body and classify it. The body is Google's own
/// error envelope; only its `reason` field is kept.
async fn classify_response(response: reqwest::Response) -> Failure {
    let status = response.status().as_u16();
    let body = response.text().await.unwrap_or_default();
    let reason = serde_json::from_str::<serde_json::Value>(&body)
        .ok()
        .and_then(|json| {
            json.pointer("/error/errors/0/reason")
                .and_then(|value| value.as_str())
                .map(str::to_string)
        })
        .unwrap_or_default();
    classify(status, &reason)
}

// ---------------------------------------------------------------------------
// The poller task
// ---------------------------------------------------------------------------

/// Everything one YouTube chat task needs. Mirrors the `[chat]` config table
/// plus the shared plumbing from `chat::source`.
pub struct SpawnParams {
    pub key: ChatKey,
    /// The fastest we may ever poll, in milliseconds (`poll_interval_floor_ms`).
    pub poll_floor_ms: u64,
    /// Optional ceiling in milliseconds; 0 means none (`poll_interval_ceiling_ms`).
    pub poll_ceiling_ms: u64,
    /// The shared quota estimate every YouTube poller charges against.
    pub quota: QuotaStore,
    /// Stop polling below this remaining percentage so sends keep working
    /// (`quota_reserve_percent`).
    pub quota_reserve_percent: u8,
    pub token: TokenProvider,
    pub events: EventSender,
    pub client: reqwest::Client,
    /// Test seam: a local fake server's base URL. `None` uses the real API.
    pub base: Option<String>,
}

/// Spawn the poll task for one YouTube chat. Dropping the returned handle
/// closes its command channel, which ends the task.
pub fn spawn(params: SpawnParams) -> ChatHandle {
    let (commands, command_rx) = mpsc::channel::<ChatCommand>(COMMAND_QUEUE);
    let key = params.key.clone();
    let task = tokio::spawn(async move {
        Poller::new(params).run(command_rx).await;
    });
    ChatHandle {
        key,
        commands,
        task,
    }
}

/// What a park or a sleep ended with.
enum Wake {
    /// The delay elapsed; poll again.
    Tick,
    /// The user asked for a reconnect: overrides any park or pause.
    Reconnect,
    /// The command channel closed: the handle was dropped, shut down.
    Shutdown,
}

struct Poller {
    key: ChatKey,
    floor: Duration,
    ceiling: Duration,
    reserve_percent: u8,
    token: TokenProvider,
    events: EventSender,
    client: reqwest::Client,
    base: String,
    ledger: QuotaStore,
    seen: DedupeRing,
    bucket: TokenBucket,
    jitter: Lcg,
    /// The last good continuation token. An empty `nextPageToken` never
    /// clears it: falling back to a token-less request re-delivers the whole
    /// backlog and charges for the privilege.
    retained_token: Option<String>,
    live_chat_id: String,
    label: String,
    /// Set by a manual reconnect out of a quota pause, so the reserve check
    /// does not immediately re-trip. Hard exhaustion (remaining = 0) still
    /// pauses.
    reserve_overridden: bool,
}

impl Poller {
    fn new(params: SpawnParams) -> Self {
        let now = std::time::Instant::now();
        // Seeding jitter from the key means two chats never share a jitter
        // sequence, which is the whole point of jittering.
        let seed = params
            .key
            .target
            .bytes()
            .fold(0xcbf2_9ce4_8422_2325u64, |acc, byte| {
                (acc ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
            });
        Self {
            // A floor of 0 in the config means "default", not "as fast as
            // possible": a 1 ms floor would let an error path retry-storm
            // through the daily quota in minutes.
            floor: Duration::from_millis(if params.poll_floor_ms == 0 {
                1000
            } else {
                params.poll_floor_ms
            }),
            ceiling: Duration::from_millis(params.poll_ceiling_ms),
            reserve_percent: params.quota_reserve_percent,
            ledger: params.quota.clone(),
            seen: DedupeRing::new(DEDUPE_RING_SIZE),
            bucket: TokenBucket::youtube(now),
            jitter: Lcg::new(seed),
            retained_token: None,
            live_chat_id: String::new(),
            label: params.key.target.clone(),
            reserve_overridden: false,
            base: params.base.unwrap_or_else(|| API.to_string()),
            key: params.key,
            token: params.token,
            events: params.events,
            client: params.client,
        }
    }

    fn emit(&self, event: ChatEvent) {
        // A send fails only when the UI side is gone, at which point there is
        // nobody left to tell.
        let _ = self.events.send((self.key.clone(), event));
    }

    fn emit_status(&self, status: ConnectionStatus, detail: impl Into<String>) {
        self.emit(ChatEvent::Connection {
            status,
            detail: detail.into(),
        });
    }

    /// The whole lifecycle. Returns only when the command channel closes.
    async fn run(mut self, mut rx: mpsc::Receiver<ChatCommand>) {
        let target = match parse_target(&self.key.target) {
            Ok(target) => target,
            Err(err) => {
                // format!("{err:#}") renders our own prose; no URL or token
                // ever enters a parse error.
                self.emit_status(ConnectionStatus::Failed, format!("{err:#}"));
                loop {
                    match self.park(&mut rx).await {
                        // The target will not become parseable by retrying,
                        // so a reconnect just restates the problem.
                        Wake::Reconnect => {
                            self.emit_status(ConnectionStatus::Failed, format!("{err:#}"));
                        }
                        _ => return,
                    }
                }
            }
        };

        // Session loop: every `continue 'session` is a (re)connect, resuming
        // from the retained continuation token when one survives.
        'session: loop {
            self.emit_status(ConnectionStatus::Connecting, "resolving live chat");

            // Resolution retries transient failures on the same ladder as a
            // failed poll, so a network blip while resolving does not strand
            // the chat.
            let mut backoff = BACKOFF_FLOOR;
            let resolved = loop {
                match self.resolve(&target).await {
                    Ok(resolved) => break resolved,
                    Err(failure) => match failure.kind {
                        FailKind::Transient | FailKind::RateLimited => {
                            let cap = if failure.kind == FailKind::RateLimited {
                                BACKOFF_RATE_LIMIT_CAP
                            } else {
                                BACKOFF_TRANSIENT_CAP
                            };
                            backoff = climb(backoff, self.floor, cap);
                            self.emit_status(ConnectionStatus::Reconnecting, failure.detail);
                            let delay = next_interval(
                                Duration::ZERO,
                                self.floor,
                                self.ceiling,
                                backoff,
                                &mut self.jitter,
                            );
                            match self.sleep(&mut rx, delay).await {
                                Wake::Tick => continue,
                                Wake::Reconnect => continue 'session,
                                Wake::Shutdown => return,
                            }
                        }
                        FailKind::Quota => {
                            self.emit_status(ConnectionStatus::QuotaPaused, failure.detail);
                            match self.park(&mut rx).await {
                                Wake::Reconnect => {
                                    self.reserve_overridden = true;
                                    continue 'session;
                                }
                                _ => return,
                            }
                        }
                        FailKind::Auth => {
                            self.emit_status(ConnectionStatus::Failed, failure.detail);
                            match self.park(&mut rx).await {
                                Wake::Reconnect => continue 'session,
                                _ => return,
                            }
                        }
                        FailKind::ChatGone | FailKind::Rejected => {
                            self.emit_status(ConnectionStatus::Closed, failure.detail);
                            match self.park(&mut rx).await {
                                Wake::Reconnect => continue 'session,
                                _ => return,
                            }
                        }
                    },
                }
            };
            self.live_chat_id = resolved.live_chat_id.clone();
            self.label = resolved.label().to_string();

            // Poll loop state, per session. `token_retried` limits the
            // stale-cursor recovery to once per session: one rejection is a
            // stale token, a second is a real answer.
            let mut page_token = self.retained_token.clone();
            let mut priming = page_token.is_none();
            if priming {
                self.emit_status(ConnectionStatus::Connecting, "loading recent messages");
            }
            let mut backoff = BACKOFF_FLOOR;
            let mut server_floor = Duration::ZERO;
            let mut token_retried = false;
            let mut connected = false;
            let mut offline_noted_at: Option<std::time::Instant> = None;
            let mut last_item_at: Option<std::time::Instant> = None;

            loop {
                match self.list(page_token.as_deref()).await {
                    Err(failure) => {
                        let effective_base = self.floor.max(server_floor);
                        match failure.kind {
                            FailKind::Quota => {
                                // Never retry an exhausted quota: every
                                // attempt is charged, and the allowance does
                                // not come back until the Pacific reset.
                                self.emit_status(ConnectionStatus::QuotaPaused, failure.detail);
                                match self.park(&mut rx).await {
                                    Wake::Reconnect => {
                                        self.reserve_overridden = true;
                                        continue 'session;
                                    }
                                    _ => return,
                                }
                            }
                            FailKind::Rejected | FailKind::ChatGone
                                if page_token.is_some() && !token_retried =>
                            {
                                // A 400/404 with a continuation token in hand
                                // is far more likely about the token than the
                                // chat. Drop the cursor and re-prime once —
                                // the dedupe ring keeps the re-primed page
                                // from reprinting what is on screen. A chat
                                // that really ended still ends: the
                                // token-less retry hits the terminal arm one
                                // call later.
                                token_retried = true;
                                page_token = None;
                                self.retained_token = None;
                                priming = true;
                                backoff = climb(backoff, effective_base, BACKOFF_TRANSIENT_CAP);
                                self.emit_status(
                                    ConnectionStatus::Reconnecting,
                                    "chat cursor expired; reloading recent messages",
                                );
                            }
                            FailKind::Rejected | FailKind::ChatGone => {
                                // Terminal but clean: the history stays
                                // readable, and ctrl+r re-resolves.
                                self.emit_status(ConnectionStatus::Closed, failure.detail);
                                match self.park(&mut rx).await {
                                    Wake::Reconnect => continue 'session,
                                    _ => return,
                                }
                            }
                            FailKind::Auth => {
                                self.emit_status(ConnectionStatus::Failed, failure.detail);
                                match self.park(&mut rx).await {
                                    Wake::Reconnect => continue 'session,
                                    _ => return,
                                }
                            }
                            FailKind::RateLimited => {
                                backoff = climb(backoff, effective_base, BACKOFF_RATE_LIMIT_CAP);
                                self.emit_status(ConnectionStatus::Reconnecting, failure.detail);
                            }
                            FailKind::Transient => {
                                backoff = climb(backoff, effective_base, BACKOFF_TRANSIENT_CAP);
                                self.emit_status(ConnectionStatus::Reconnecting, failure.detail);
                            }
                        }
                        let delay = next_interval(
                            server_floor,
                            self.floor,
                            self.ceiling,
                            backoff,
                            &mut self.jitter,
                        );
                        match self.sleep(&mut rx, delay).await {
                            Wake::Tick => continue,
                            Wake::Reconnect => continue 'session,
                            Wake::Shutdown => return,
                        }
                    }

                    Ok(response) => {
                        backoff = decay(backoff);
                        if !connected {
                            connected = true;
                            self.emit_status(
                                ConnectionStatus::Connected,
                                format!("connected to {}", self.label),
                            );
                        }

                        let mut delivered = 0usize;
                        for item in &response.items {
                            let normalized = normalize_item(item, priming, &self.seen);
                            for message in normalized.messages {
                                // The dedupe ring is the reason a retained
                                // token, a retry, and a reconnect never
                                // reprint a row.
                                if self.seen.insert(&message.id) {
                                    delivered += 1;
                                    self.emit(ChatEvent::Message(Box::new(message)));
                                }
                            }
                            for event in normalized.events {
                                self.emit(event);
                            }
                            if normalized.chat_ended {
                                self.emit_status(
                                    ConnectionStatus::Closed,
                                    "the live chat has ended",
                                );
                                match self.park(&mut rx).await {
                                    Wake::Reconnect => continue 'session,
                                    _ => return,
                                }
                            }
                        }
                        if delivered > 0 {
                            last_item_at = Some(std::time::Instant::now());
                        }
                        priming = false;

                        if response.polling_interval_millis > 0 {
                            server_floor = Duration::from_millis(response.polling_interval_millis);
                        }
                        // Retain the last good token; an empty nextPageToken
                        // never clears the retained one.
                        if !response.next_page_token.is_empty() {
                            page_token = Some(response.next_page_token.clone());
                            self.retained_token = page_token.clone();
                        }

                        // offlineAt is a warning, not a stop: chat outlives
                        // the broadcast. The window is measured from when
                        // this session *observed* it (offlineAt itself is a
                        // past timestamp), and any new message restarts it.
                        if !response.offline_at.trim().is_empty() {
                            let noted =
                                *offline_noted_at.get_or_insert_with(std::time::Instant::now);
                            let quiet_since = match last_item_at {
                                Some(item_at) if item_at > noted => item_at,
                                _ => noted,
                            };
                            if quiet_since.elapsed() > OFFLINE_GRACE {
                                self.emit_status(
                                    ConnectionStatus::Closed,
                                    "live chat closed after the broadcast ended",
                                );
                                match self.park(&mut rx).await {
                                    Wake::Reconnect => continue 'session,
                                    _ => return,
                                }
                            }
                        } else {
                            offline_noted_at = None;
                        }

                        if let Some(reason) = self.reserve_pause_reason() {
                            self.emit_status(ConnectionStatus::QuotaPaused, reason);
                            match self.park(&mut rx).await {
                                Wake::Reconnect => {
                                    self.reserve_overridden = true;
                                    continue 'session;
                                }
                                _ => return,
                            }
                        }

                        let delay = next_interval(
                            server_floor,
                            self.floor,
                            self.ceiling,
                            BACKOFF_FLOOR,
                            &mut self.jitter,
                        );
                        match self.sleep(&mut rx, delay).await {
                            Wake::Tick => continue,
                            Wake::Reconnect => continue 'session,
                            Wake::Shutdown => return,
                        }
                    }
                }
            }
        }
    }

    /// Why the local ledger says to pause, honoring a manual override of the
    /// reserve threshold (hard exhaustion still pauses).
    fn reserve_pause_reason(&mut self) -> Option<String> {
        let reserve = if self.reserve_overridden {
            0
        } else {
            self.reserve_percent
        };
        self.ledger.pause_reason(reserve)
    }

    /// Wait out a delay while still serving Send commands; a Reconnect cuts
    /// the wait short.
    async fn sleep(&mut self, rx: &mut mpsc::Receiver<ChatCommand>, delay: Duration) -> Wake {
        let deadline = tokio::time::Instant::now() + delay;
        loop {
            tokio::select! {
                _ = tokio::time::sleep_until(deadline) => return Wake::Tick,
                command = rx.recv() => match command {
                    None => return Wake::Shutdown,
                    Some(ChatCommand::Reconnect) => return Wake::Reconnect,
                    Some(ChatCommand::Send { text, .. }) => self.handle_send(text).await,
                    Some(ChatCommand::Delete { message_id }) => {
                        self.handle_delete(message_id).await
                    }
                    Some(ChatCommand::Ban {
                        channel_id,
                        timeout_secs,
                    }) => self.handle_ban(channel_id, timeout_secs).await,
                    Some(ChatCommand::Clip) => {
                        self.emit_local_notice("clips are a Twitch feature; YouTube has no clip API here")
                    }
                },
            }
        }
    }

    /// Hold the session open without polling. Sends still work — that is the
    /// whole point of the quota reserve — and only a Reconnect (or shutdown)
    /// leaves the park.
    async fn park(&mut self, rx: &mut mpsc::Receiver<ChatCommand>) -> Wake {
        loop {
            match rx.recv().await {
                None => return Wake::Shutdown,
                Some(ChatCommand::Reconnect) => return Wake::Reconnect,
                Some(ChatCommand::Send { text, .. }) => self.handle_send(text).await,
                Some(ChatCommand::Delete { message_id }) => self.handle_delete(message_id).await,
                Some(ChatCommand::Ban {
                    channel_id,
                    timeout_secs,
                }) => self.handle_ban(channel_id, timeout_secs).await,
                Some(ChatCommand::Clip) => self
                    .emit_local_notice("clips are a Twitch feature; YouTube has no clip API here"),
            }
        }
    }

    /// One `liveChatMessages.list` call. Charged before dispatch.
    async fn list(&mut self, page_token: Option<&str>) -> Result<WireListResponse, Failure> {
        let token = self.access_token().await?;
        self.ledger.charge(COST_LIST);

        let base = &self.base;
        let mut url = format!(
            "{base}/liveChat/messages?liveChatId={}&part=snippet,authorDetails&maxResults={MAX_RESULTS_PER_POLL}",
            urlencoding::encode(&self.live_chat_id),
        );
        if let Some(token) = page_token {
            url.push_str("&pageToken=");
            url.push_str(&urlencoding::encode(token));
        }
        let response = self
            .client
            .get(&url)
            .bearer_auth(&token)
            .send()
            .await
            .map_err(|_| Failure::transient("could not reach YouTube; check your connection"))?;
        if !response.status().is_success() {
            return Err(classify_response(response).await);
        }
        response
            .json::<WireListResponse>()
            .await
            .map_err(|_| Failure::transient("YouTube sent a chat page this program could not read"))
    }

    /// The resolve ladder (yc: resolve.go), cheapest first: an explicit live
    /// chat id costs nothing; `videos.list` and `channels.list` cost one unit
    /// each. `search.list` is never called — it spends a separate, scarce
    /// daily allowance and is not this program's decision to make.
    async fn resolve(&mut self, target: &ChatTarget) -> Result<ChatTarget, Failure> {
        if target.resolved() {
            return Ok(target.clone());
        }
        if !target.video_id.trim().is_empty() {
            return self.resolve_video(&target.video_id).await;
        }
        if !target.handle.trim().is_empty() || !target.channel_id.trim().is_empty() {
            let identifier = if target.handle.trim().is_empty() {
                target.channel_id.trim()
            } else {
                target.handle.trim()
            };
            let mut resolved = self.resolve_channel(identifier).await?;
            resolved.raw = target.raw.clone();
            if resolved.resolved() {
                return Ok(resolved);
            }
            // channels.list answered but reports no live chat. Finding the
            // broadcast from here would need search.list, which is never
            // called, so the honest answer is to ask for the video instead.
            return Err(Failure {
                kind: FailKind::ChatGone,
                detail: "this channel has no discoverable live chat; open the chat with the \
                         video's URL or ID instead"
                    .to_string(),
            });
        }
        Err(Failure {
            kind: FailKind::ChatGone,
            detail: "no chat target given".to_string(),
        })
    }

    /// `videos.list` (1 unit): video id → activeLiveChatId, keeping the
    /// video title as the chat's label.
    async fn resolve_video(&mut self, video_id: &str) -> Result<ChatTarget, Failure> {
        let token = self.access_token().await?;
        self.ledger.charge(COST_VIDEOS_LIST);

        let base = &self.base;
        let url = format!(
            "{base}/videos?part=snippet,liveStreamingDetails,status&id={}",
            urlencoding::encode(video_id),
        );
        let response = self
            .client
            .get(&url)
            .bearer_auth(&token)
            .send()
            .await
            .map_err(|_| Failure::transient("could not reach YouTube; check your connection"))?;
        if !response.status().is_success() {
            return Err(classify_response(response).await);
        }
        let parsed = response.json::<WireVideosResponse>().await.map_err(|_| {
            Failure::transient("YouTube sent a video listing this program could not read")
        })?;
        let Some(video) = parsed.items.first() else {
            return Err(Failure {
                kind: FailKind::ChatGone,
                detail: "no such video".to_string(),
            });
        };
        let target = ChatTarget {
            raw: video_id.to_string(),
            kind: TargetKind::VideoId,
            video_id: video.id.clone(),
            channel_id: video.snippet.channel_id.clone(),
            live_chat_id: video.live_streaming_details.active_live_chat_id.clone(),
            title: video.snippet.title.clone(),
            ..ChatTarget::default()
        };
        if !target.resolved() {
            // Either not a broadcast, or one that has already ended. Both
            // leave the title on screen.
            return Err(Failure {
                kind: FailKind::ChatGone,
                detail: "this video has no active live chat".to_string(),
            });
        }
        Ok(target)
    }

    /// `channels.list` (1 unit): @handle or channel id → channel identity.
    /// It cannot reveal a live chat id by itself; the caller decides what a
    /// chat-less channel means.
    async fn resolve_channel(&mut self, identifier: &str) -> Result<ChatTarget, Failure> {
        let token = self.access_token().await?;
        self.ledger.charge(COST_CHANNELS_LIST);

        let base = &self.base;
        let url = if is_channel_id(identifier) {
            format!(
                "{base}/channels?part=snippet,contentDetails&id={}",
                urlencoding::encode(identifier),
            )
        } else {
            let handle = if identifier.starts_with('@') {
                identifier.to_string()
            } else {
                format!("@{identifier}")
            };
            format!(
                "{base}/channels?part=snippet,contentDetails&forHandle={}",
                urlencoding::encode(&handle),
            )
        };
        let response = self
            .client
            .get(&url)
            .bearer_auth(&token)
            .send()
            .await
            .map_err(|_| Failure::transient("could not reach YouTube; check your connection"))?;
        if !response.status().is_success() {
            return Err(classify_response(response).await);
        }
        let parsed = response.json::<WireChannelsResponse>().await.map_err(|_| {
            Failure::transient("YouTube sent a channel listing this program could not read")
        })?;
        let Some(channel) = parsed.items.first() else {
            return Err(Failure {
                kind: FailKind::ChatGone,
                detail: "no such channel".to_string(),
            });
        };
        Ok(ChatTarget {
            raw: identifier.to_string(),
            kind: TargetKind::ChannelId,
            channel_id: channel.id.clone(),
            handle: channel.snippet.custom_url.clone(),
            title: channel.snippet.title.clone(),
            ..ChatTarget::default()
        })
    }

    /// Remove one message through `liveChatMessages.delete`.
    ///
    /// A success is echoed locally as a deletion event — the API does not
    /// reliably report deletions back through the poll stream (yc invariant:
    /// the tombstone may never arrive, so the moderator must see the effect
    /// immediately).
    async fn handle_delete(&mut self, message_id: String) {
        if message_id.is_empty() {
            return;
        }
        let token = match self.access_token().await {
            Ok(token) => token,
            Err(failure) => {
                self.emit_local_notice(&failure.detail);
                return;
            }
        };
        self.ledger.charge(COST_MODERATE);

        let base = &self.base;
        let url = format!(
            "{base}/liveChat/messages?id={}",
            urlencoding::encode(&message_id)
        );
        match self.client.delete(&url).bearer_auth(&token).send().await {
            Err(_) => {
                self.emit_local_notice("could not reach YouTube to delete; check your connection")
            }
            Ok(response) if !response.status().is_success() => {
                let failure = classify_response(response).await;
                self.emit_local_notice(&format!("delete failed: {}", failure.detail));
            }
            Ok(_) => self.emit(ChatEvent::MessageDeleted { message_id }),
        }
    }

    /// Ban an author permanently, or time them out, through
    /// `liveChatBans.insert`.
    ///
    /// A success is echoed locally as a purge for the same reason deletions
    /// are: the ban event may never come back through the poll stream.
    async fn handle_ban(&mut self, channel_id: String, timeout_secs: Option<u64>) {
        if channel_id.is_empty() {
            return;
        }
        // The request body carries `liveChatId`, which stays empty until the
        // chat resolves. Sending it anyway costs 50 quota units for a reply
        // that can only be a 400, so refuse before charging anything.
        if self.live_chat_id.is_empty() {
            self.emit_local_notice("cannot send yet: the chat is still connecting");
            return;
        }
        let token = match self.access_token().await {
            Ok(token) => token,
            Err(failure) => {
                self.emit_local_notice(&failure.detail);
                return;
            }
        };
        self.ledger.charge(COST_MODERATE);

        // The API wants the duration as a string, and refuses zero: a
        // sub-minute timeout is floored to one second, matching yc.
        let snippet = match timeout_secs {
            None => serde_json::json!({
                "liveChatId": self.live_chat_id,
                "type": "permanent",
                "bannedUserDetails": { "channelId": channel_id },
            }),
            Some(secs) => serde_json::json!({
                "liveChatId": self.live_chat_id,
                "type": "temporary",
                "banDurationSeconds": secs.max(1).to_string(),
                "bannedUserDetails": { "channelId": channel_id },
            }),
        };
        let base = &self.base;
        let url = format!("{base}/liveChat/bans?part=snippet");
        let sent = self
            .client
            .post(&url)
            .bearer_auth(&token)
            .json(&serde_json::json!({ "snippet": snippet }))
            .send()
            .await;
        match sent {
            Err(_) => {
                self.emit_local_notice("could not reach YouTube to ban; check your connection")
            }
            Ok(response) if !response.status().is_success() => {
                let failure = classify_response(response).await;
                self.emit_local_notice(&format!("ban failed: {}", failure.detail));
            }
            Ok(_) => self.emit(ChatEvent::UserPurged {
                author_login: channel_id,
                timeout_secs,
            }),
        }
    }

    /// Send one chat message through `liveChatMessages.insert`.
    ///
    /// The local token bucket declines *before* dispatch, because an insert
    /// costs an estimated 50 units whether the API accepts it or not:
    /// learning the limit by hitting it is the most expensive way to find it.
    /// A successful send emits a local-echo row under the returned id. The
    /// returned id is deliberately NOT added to the dedupe ring: the poller's
    /// later authoritative copy is allowed through, and the UI-side state
    /// replaces the echo by id — that reconciliation is the state layer's
    /// job, not the transport's.
    async fn handle_send(&mut self, text: String) {
        let text = cap_graphemes(text.trim(), MAX_MESSAGE_GRAPHEMES);
        if text.is_empty() {
            return;
        }
        if self.live_chat_id.is_empty() {
            self.emit_local_notice("cannot send yet: the chat is still connecting");
            return;
        }
        if let Err(denied) = self.bucket.acquire(std::time::Instant::now()) {
            let detail = match denied {
                SendDenied::RateLimited(wait) => {
                    format!("sending too fast; retry in {:.1}s", wait.as_secs_f64())
                }
                SendDenied::Duplicate => "that message was just sent".to_string(),
            };
            self.emit_local_notice(&detail);
            return;
        }
        let token = match self.access_token().await {
            Ok(token) => token,
            Err(failure) => {
                self.emit_local_notice(&failure.detail);
                return;
            }
        };
        self.ledger.charge(COST_INSERT);

        let base = &self.base;
        let url = format!("{base}/liveChat/messages?part=snippet");
        let body = serde_json::json!({
            "snippet": {
                "liveChatId": self.live_chat_id,
                "type": "textMessageEvent",
                "textMessageDetails": { "messageText": text },
            }
        });
        let sent = self
            .client
            .post(&url)
            .bearer_auth(&token)
            .json(&body)
            .send()
            .await;
        let response = match sent {
            Ok(response) => response,
            Err(_) => {
                self.emit_local_notice("could not reach YouTube to send; check your connection");
                return;
            }
        };
        if !response.status().is_success() {
            let failure = classify_response(response).await;
            self.emit_local_notice(&format!("message not sent: {}", failure.detail));
            return;
        }

        #[derive(Deserialize, Default)]
        #[serde(rename_all = "camelCase")]
        struct InsertResponse {
            #[serde(default)]
            id: String,
            #[serde(default)]
            snippet: InsertSnippet,
        }
        #[derive(Deserialize, Default)]
        #[serde(rename_all = "camelCase")]
        struct InsertSnippet {
            #[serde(default)]
            published_at: String,
        }
        let accepted = response.json::<InsertResponse>().await.unwrap_or_default();
        self.emit(ChatEvent::Message(Box::new(ChatMessage {
            id: accepted.id,
            timestamp: parse_timestamp(&accepted.snippet.published_at).or_else(|| Some(Utc::now())),
            author: ChatAuthor {
                // The API does not echo our own authorDetails on insert, and
                // resolving them would cost another unit; "you" is what the
                // renderer shows until the poller's authoritative copy — with
                // the real name and badges — replaces this row by id.
                display_name: "you".to_string(),
                ..ChatAuthor::default()
            },
            text,
            kind: MessageKind::Chat,
            deleted: false,
            historical: false,
            local_echo: true,
            meta: None,
        })));
    }

    /// A locally generated one-line explanation, shown as a notice row so a
    /// refused send is visible without corrupting the connection status.
    fn emit_local_notice(&self, detail: &str) {
        self.emit(ChatEvent::Message(Box::new(ChatMessage {
            id: String::new(),
            timestamp: Some(Utc::now()),
            author: ChatAuthor::default(),
            text: detail.to_string(),
            kind: MessageKind::Notice,
            deleted: false,
            historical: false,
            local_echo: true,
            meta: None,
        })));
    }

    /// Fetch a currently valid access token. A failure here is an auth
    /// failure by definition; the provider's own message is already
    /// user-facing prose from our auth layer and carries no secret.
    async fn access_token(&self) -> Result<String, Failure> {
        (self.token)().await.map_err(|err| Failure {
            kind: FailKind::Auth,
            detail: format!("{err:#}"),
        })
    }
}

/// Truncate to at most `limit` grapheme clusters without ever splitting one:
/// cutting a ZWJ emoji or a combining mark mid-cluster would put mojibake
/// into someone's public chat.
fn cap_graphemes(value: &str, limit: usize) -> String {
    let mut end = 0;
    for (count, (offset, grapheme)) in value.grapheme_indices(true).enumerate() {
        if count == limit {
            return value[..end].to_string();
        }
        end = offset + grapheme.len();
    }
    value.to_string()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// The shared store persists across sessions within one Pacific day and
    /// starts fresh on the next; two clones spend from the same count.
    #[test]
    fn the_quota_store_is_shared_and_persisted() {
        let scratch = crate::paths::test_support::ScratchConfigDir::new("quota-store");
        let path = scratch.path().join("quota.json");

        let store = QuotaStore::new(100, Some(path.clone()));
        let clone = store.clone();
        store.charge(30);
        clone.charge(20);
        assert_eq!(store.remaining(), 50, "clones spend from one count");

        // A new store (a new session) resumes today's count from disk.
        let resumed = QuotaStore::new(100, Some(path.clone()));
        assert_eq!(resumed.remaining(), 50);

        // A saved count from yesterday is discarded on load.
        let stale = PersistedQuota {
            day: pacific_day(Utc::now()) - 1,
            used: 90,
        };
        std::fs::write(&path, serde_json::to_string(&stale).unwrap()).unwrap();
        let fresh = QuotaStore::new(100, Some(path));
        assert_eq!(fresh.remaining(), 100, "yesterday's spend is not today's");
    }

    /// The daily budget refills at the Pacific midnight; the ledger must
    /// start over then instead of parking the session forever.
    #[test]
    fn the_quota_ledger_rolls_over_at_the_pacific_day_boundary() {
        let mut ledger = QuotaLedger::new(100);
        ledger.charge(100);
        assert_eq!(ledger.remaining(), 0);
        assert!(ledger.pause_reason(0).is_some(), "exhausted pauses");

        // Pretend the count belongs to yesterday: the next check refills.
        ledger.day -= 1;
        assert_eq!(ledger.remaining(), 100, "a new Pacific day starts fresh");
        assert!(ledger.pause_reason(10).is_none());
    }

    /// The fixed UTC−8 day arithmetic: one second before and after the
    /// boundary land on different days.
    #[test]
    fn pacific_day_changes_exactly_at_utc_minus_eight_midnight() {
        use chrono::TimeZone as _;
        // 08:00:00 UTC == 00:00:00 UTC−8.
        let boundary = Utc.with_ymd_and_hms(2026, 8, 15, 8, 0, 0).unwrap();
        assert_eq!(
            pacific_day(boundary) - 1,
            pacific_day(boundary - chrono::Duration::seconds(1))
        );
        assert_eq!(
            pacific_day(boundary),
            pacific_day(boundary + chrono::Duration::hours(23))
        );
    }

    /// A zero poll floor in the config means the default second, never a
    /// millisecond retry storm.
    #[test]
    fn a_zero_config_floor_falls_back_to_one_second() {
        let params = SpawnParams {
            key: ChatKey {
                platform: crate::model::Platform::YouTube,
                account: "youtube".into(),
                target: "x".into(),
            },
            poll_floor_ms: 0,
            poll_ceiling_ms: 0,
            quota: QuotaStore::new(0, None),
            quota_reserve_percent: 0,
            token: std::sync::Arc::new(|| Box::pin(async { Ok(String::new()) })),
            events: tokio::sync::mpsc::unbounded_channel().0,
            client: reqwest::Client::new(),
            base: None,
        };
        let poller = Poller::new(params);
        assert_eq!(poller.floor, Duration::from_millis(1000));
    }

    use crate::model::Platform;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    // -- parse_target -------------------------------------------------------

    #[test]
    fn parse_target_accepts_every_documented_form() {
        let live = "Cg0KC2FiY2RlZmdoaWprbA"; // Cg + 20: too short on purpose below
        let _ = live;
        type Case = (
            &'static str,
            TargetKind,
            fn(&ChatTarget) -> &str,
            &'static str,
        );
        let cases: Vec<Case> = vec![
            // Explicit prefix, case-insensitive, skips every heuristic.
            (
                "livechat:whatever-id",
                TargetKind::LiveChatId,
                |t| &t.live_chat_id,
                "whatever-id",
            ),
            (
                "LiveChat: spaced-id",
                TargetKind::LiveChatId,
                |t| &t.live_chat_id,
                "spaced-id",
            ),
            // Bare live chat id: Cg + at least 24 more.
            (
                "CgABCDEFGHIJKLMNOPQRSTUVWXYZ",
                TargetKind::LiveChatId,
                |t| &t.live_chat_id,
                "CgABCDEFGHIJKLMNOPQRSTUVWXYZ",
            ),
            // 11-character video id.
            (
                "dQw4w9WgXcQ",
                TargetKind::VideoId,
                |t| &t.video_id,
                "dQw4w9WgXcQ",
            ),
            // UC + 22 channel id.
            (
                "UCabcdefghijklmnopqrst12",
                TargetKind::ChannelId,
                |t| &t.channel_id,
                "UCabcdefghijklmnopqrst12",
            ),
            // Handles, with and without '@'.
            ("@someone", TargetKind::Handle, |t| &t.handle, "@someone"),
            ("someone", TargetKind::Handle, |t| &t.handle, "@someone"),
            // URL family.
            (
                "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
                TargetKind::VideoId,
                |t| &t.video_id,
                "dQw4w9WgXcQ",
            ),
            (
                "youtube.com/watch?t=1&v=dQw4w9WgXcQ",
                TargetKind::VideoId,
                |t| &t.video_id,
                "dQw4w9WgXcQ",
            ),
            (
                "https://m.youtube.com/live/dQw4w9WgXcQ",
                TargetKind::VideoId,
                |t| &t.video_id,
                "dQw4w9WgXcQ",
            ),
            (
                "https://music.youtube.com/shorts/dQw4w9WgXcQ",
                TargetKind::VideoId,
                |t| &t.video_id,
                "dQw4w9WgXcQ",
            ),
            (
                "https://youtube-nocookie.com/embed/dQw4w9WgXcQ",
                TargetKind::VideoId,
                |t| &t.video_id,
                "dQw4w9WgXcQ",
            ),
            (
                "youtube.com/v/dQw4w9WgXcQ/extra",
                TargetKind::VideoId,
                |t| &t.video_id,
                "dQw4w9WgXcQ",
            ),
            (
                "https://youtu.be/dQw4w9WgXcQ",
                TargetKind::VideoId,
                |t| &t.video_id,
                "dQw4w9WgXcQ",
            ),
            (
                "https://www.youtube.com/channel/UCabcdefghijklmnopqrst12",
                TargetKind::ChannelId,
                |t| &t.channel_id,
                "UCabcdefghijklmnopqrst12",
            ),
            (
                "youtube.com/c/SomeCreator",
                TargetKind::Handle,
                |t| &t.handle,
                "@SomeCreator",
            ),
            (
                "youtube.com/user/oldname",
                TargetKind::Handle,
                |t| &t.handle,
                "@oldname",
            ),
            (
                "https://www.youtube.com/@handle/streams",
                TargetKind::Handle,
                |t| &t.handle,
                "@handle",
            ),
        ];
        for (input, kind, field, expected) in cases {
            let target = parse_target(input).unwrap_or_else(|err| {
                panic!("{input:?} must parse, got error: {err:#}");
            });
            assert_eq!(target.kind, kind, "kind for {input:?}");
            assert_eq!(field(&target), expected, "value for {input:?}");
            assert_eq!(target.raw, input.trim(), "raw is preserved for {input:?}");
        }
    }

    #[test]
    fn parse_target_rejects_what_it_cannot_classify() {
        // Empty, too short for a handle, a non-YouTube URL, and a word with
        // characters no accepted form allows.
        for input in [
            "",
            "  ",
            "ab",
            "https://example.com/watch?v=dQw4w9WgXcQ",
            "has space",
        ] {
            assert!(parse_target(input).is_err(), "{input:?} must be rejected");
        }
    }

    #[test]
    fn a_short_cg_string_is_a_handle_not_a_live_chat_id() {
        // "Cg" + fewer than 24 more characters is shaped like a handle.
        let target = parse_target("CgShortOne").expect("parses as a handle");
        assert_eq!(target.kind, TargetKind::Handle);
        assert_eq!(target.handle, "@CgShortOne");
    }

    // -- micros -------------------------------------------------------------

    #[test]
    fn micros_parse_without_floats_and_clamp_uint64_overflow() {
        assert_eq!(parse_micros("4990000"), Some(4_990_000));
        assert_eq!(parse_micros(" 100 "), Some(100));
        assert_eq!(parse_micros(""), None);
        assert_eq!(parse_micros("not-a-number"), None);
        // i64::MAX + 1 as a uint64 must clamp, never wrap negative.
        assert_eq!(parse_micros("9223372036854775808"), Some(i64::MAX));
        assert_eq!(parse_micros("18446744073709551615"), Some(i64::MAX));
    }

    // -- normalization ------------------------------------------------------

    fn wire(json: &str) -> WireMessage {
        serde_json::from_str(json).expect("test wire JSON must parse")
    }

    fn empty_ring() -> DedupeRing {
        DedupeRing::new(8)
    }

    #[test]
    fn a_text_message_prefers_message_text_over_display_message() {
        let item = wire(
            r#"{"id":"m1","snippet":{"type":"textMessageEvent","publishedAt":"2026-08-01T12:00:00Z",
                "displayMessage":"rendered","textMessageDetails":{"messageText":"the real text"}},
                "authorDetails":{"channelId":"UCx","displayName":"Alice","isChatModerator":true}}"#,
        );
        let out = normalize_item(&item, true, &empty_ring());
        assert_eq!(out.messages.len(), 1);
        let msg = &out.messages[0];
        assert_eq!(msg.kind, MessageKind::Chat);
        assert_eq!(msg.text, "the real text");
        assert!(msg.historical);
        assert_eq!(msg.author.id, "UCx");
        assert_eq!(msg.author.display_name, "Alice");
        assert_eq!(msg.author.badges.len(), 1);
        assert_eq!(msg.author.badges[0].set, "moderator");
        assert_eq!(
            msg.timestamp.map(|t| t.to_rfc3339()),
            Some("2026-08-01T12:00:00+00:00".to_string())
        );
    }

    #[test]
    fn a_super_chat_becomes_a_paid_row_with_integer_micros() {
        let item = wire(
            r#"{"id":"m2","snippet":{"type":"superChatEvent","displayMessage":"$5.00 from Bob",
                "superChatDetails":{"amountMicros":"5000000","currency":"USD",
                "amountDisplayString":"$5.00","tier":3,"userComment":"great stream"}},
                "authorDetails":{"channelId":"UCy","displayName":"Bob"}}"#,
        );
        let out = normalize_item(&item, false, &empty_ring());
        let msg = &out.messages[0];
        assert_eq!(msg.kind, MessageKind::Paid);
        assert_eq!(msg.text, "great stream");
        let Some(PlatformMeta::YouTube(meta)) = &msg.meta else {
            panic!("expected YouTube meta");
        };
        assert_eq!(meta.raw_type, "superChatEvent");
        assert_eq!(
            meta.paid,
            Some(PaidAmount {
                micros: 5_000_000,
                currency: "USD".into(),
                display: "$5.00".into(),
                tier: 3,
            })
        );
    }

    #[test]
    fn a_super_sticker_renders_from_its_alt_text() {
        let item = wire(
            r#"{"id":"m3","snippet":{"type":"superStickerEvent",
                "superStickerDetails":{"amountMicros":"1990000","currency":"EUR",
                "amountDisplayString":"€1.99","tier":1,
                "superStickerMetadata":{"altText":"a dancing corgi"}}},
                "authorDetails":{"channelId":"UCz","displayName":"Cara"}}"#,
        );
        let out = normalize_item(&item, false, &empty_ring());
        let msg = &out.messages[0];
        assert_eq!(msg.kind, MessageKind::Paid);
        assert_eq!(msg.text, "a dancing corgi");
        let Some(PlatformMeta::YouTube(meta)) = &msg.meta else {
            panic!("expected YouTube meta");
        };
        assert_eq!(meta.paid.as_ref().map(|p| p.micros), Some(1_990_000));
    }

    #[test]
    fn new_sponsor_distinguishes_new_from_upgrade_and_adds_the_member_badge() {
        let fresh = wire(
            r#"{"id":"m4","snippet":{"type":"newSponsorEvent","displayMessage":"welcome",
                "newSponsorDetails":{"memberLevelName":"Gold","isUpgrade":false}},
                "authorDetails":{"channelId":"UCa","displayName":"Dee"}}"#,
        );
        let out = normalize_item(&fresh, false, &empty_ring());
        let msg = &out.messages[0];
        assert_eq!(msg.kind, MessageKind::Membership);
        let Some(PlatformMeta::YouTube(meta)) = &msg.meta else {
            panic!("expected YouTube meta");
        };
        let membership = meta.membership.as_ref().expect("membership details");
        assert_eq!(membership.kind, MembershipKind::New);
        assert_eq!(membership.level, "Gold");
        // The row announcing membership carries the member badge with the
        // level name, even though authorDetails did not say sponsor yet.
        assert!(msg
            .author
            .badges
            .iter()
            .any(|b| b.set == "member" && b.info == "Gold"));

        let upgrade = wire(
            r#"{"id":"m5","snippet":{"type":"newSponsorEvent",
                "newSponsorDetails":{"memberLevelName":"Platinum","isUpgrade":true}},
                "authorDetails":{"channelId":"UCa","displayName":"Dee"}}"#,
        );
        let out = normalize_item(&upgrade, false, &empty_ring());
        let Some(PlatformMeta::YouTube(meta)) = &out.messages[0].meta else {
            panic!("expected YouTube meta");
        };
        assert_eq!(
            meta.membership.as_ref().map(|m| m.kind),
            Some(MembershipKind::Upgrade)
        );
    }

    #[test]
    fn a_member_milestone_carries_months_and_uses_the_comment_as_text() {
        let item = wire(
            r#"{"id":"m6","snippet":{"type":"memberMilestoneChatEvent","displayMessage":"x",
                "memberMilestoneChatDetails":{"memberLevelName":"Gold","memberMonth":13,
                "userComment":"a year and a month!"}},
                "authorDetails":{"channelId":"UCb","displayName":"Eve"}}"#,
        );
        let out = normalize_item(&item, false, &empty_ring());
        let msg = &out.messages[0];
        assert_eq!(msg.text, "a year and a month!");
        let Some(PlatformMeta::YouTube(meta)) = &msg.meta else {
            panic!("expected YouTube meta");
        };
        let membership = meta.membership.as_ref().expect("membership details");
        assert_eq!(membership.kind, MembershipKind::Milestone);
        assert_eq!(membership.months, 13);
    }

    #[test]
    fn membership_gifting_and_gift_received_map_to_their_kinds() {
        let gifting = wire(
            r#"{"id":"m7","snippet":{"type":"membershipGiftingEvent",
                "membershipGiftingDetails":{"giftMembershipsCount":5,
                "giftMembershipsLevelName":"Gold"}},
                "authorDetails":{"channelId":"UCc","displayName":"Fay"}}"#,
        );
        let out = normalize_item(&gifting, false, &empty_ring());
        let Some(PlatformMeta::YouTube(meta)) = &out.messages[0].meta else {
            panic!("expected YouTube meta");
        };
        let membership = meta.membership.as_ref().expect("membership details");
        assert_eq!(membership.kind, MembershipKind::Gifting);
        assert_eq!(membership.gift_count, 5);

        let received = wire(
            r#"{"id":"m8","snippet":{"type":"giftMembershipReceivedEvent",
                "giftMembershipReceivedDetails":{"memberLevelName":"Gold"}},
                "authorDetails":{"channelId":"UCd","displayName":"Gil"}}"#,
        );
        let out = normalize_item(&received, false, &empty_ring());
        let Some(PlatformMeta::YouTube(meta)) = &out.messages[0].meta else {
            panic!("expected YouTube meta");
        };
        assert_eq!(
            meta.membership.as_ref().map(|m| m.kind),
            Some(MembershipKind::GiftReceived)
        );
    }

    #[test]
    fn a_poll_event_becomes_a_notice_with_the_question() {
        let item = wire(
            r#"{"id":"m9","snippet":{"type":"pollEvent",
                "pollDetails":{"metadata":{"questionText":"pineapple on pizza?"}}},
                "authorDetails":{"channelId":"UCe","displayName":"Host"}}"#,
        );
        let out = normalize_item(&item, false, &empty_ring());
        let msg = &out.messages[0];
        assert_eq!(msg.kind, MessageKind::Notice);
        assert_eq!(msg.text, "[poll] pineapple on pizza?");
    }

    #[test]
    fn a_ban_becomes_a_user_purge_never_a_chat_row() {
        let timeout = wire(
            r#"{"id":"m10","snippet":{"type":"userBannedEvent",
                "userBannedDetails":{"bannedUserDetails":{"channelId":"UCf","displayName":"Troll"},
                "banType":"temporary","banDurationSeconds":"300"}},
                "authorDetails":{"channelId":"UCmod","displayName":"Mod"}}"#,
        );
        let out = normalize_item(&timeout, false, &empty_ring());
        assert!(out.messages.is_empty(), "a ban must not produce a chat row");
        assert!(matches!(
            &out.events[0],
            ChatEvent::UserPurged { author_login, timeout_secs: Some(300) }
                if author_login == "Troll"
        ));

        let permanent = wire(
            r#"{"id":"m11","snippet":{"type":"userBannedEvent",
                "userBannedDetails":{"bannedUserDetails":{"channelId":"UCf","displayName":"Troll"},
                "banType":"permanent"}},
                "authorDetails":{"channelId":"UCmod","displayName":"Mod"}}"#,
        );
        let out = normalize_item(&permanent, false, &empty_ring());
        assert!(matches!(
            &out.events[0],
            ChatEvent::UserPurged {
                timeout_secs: None,
                ..
            }
        ));
    }

    #[test]
    fn a_deletion_event_targets_the_deleted_message_id() {
        let item = wire(
            r#"{"id":"m12","snippet":{"type":"messageDeletedEvent",
                "messageDeletedDetails":{"deletedMessageId":"victim-id"}},
                "authorDetails":{"channelId":"UCmod","displayName":"Mod"}}"#,
        );
        let out = normalize_item(&item, false, &empty_ring());
        assert!(out.messages.is_empty());
        assert!(matches!(
            &out.events[0],
            ChatEvent::MessageDeleted { message_id } if message_id == "victim-id"
        ));
    }

    #[test]
    fn a_tombstone_deletes_only_a_message_the_viewer_actually_saw() {
        let mut ring = empty_ring();
        ring.insert("seen-id");

        let seen = wire(r#"{"id":"seen-id","snippet":{"type":"tombstone"},"authorDetails":{}}"#);
        let out = normalize_item(&seen, false, &ring);
        assert!(matches!(
            &out.events[0],
            ChatEvent::MessageDeleted { message_id } if message_id == "seen-id"
        ));

        // A tombstone for an unseen message has nothing on screen to redact.
        let unseen =
            wire(r#"{"id":"never-shown","snippet":{"type":"tombstone"},"authorDetails":{}}"#);
        let out = normalize_item(&unseen, false, &ring);
        assert!(out.events.is_empty() && out.messages.is_empty());

        // hasDisplayContent=false on a seen id is the same fact by shape.
        let blanked = wire(
            r#"{"id":"seen-id","snippet":{"type":"textMessageEvent","hasDisplayContent":false},
                "authorDetails":{}}"#,
        );
        let out = normalize_item(&blanked, false, &ring);
        assert!(matches!(&out.events[0], ChatEvent::MessageDeleted { .. }));
    }

    #[test]
    fn chat_ended_produces_no_chat_row_and_flags_the_session() {
        let item = wire(
            r#"{"id":"m13","snippet":{"type":"chatEndedEvent","displayMessage":""},
                "authorDetails":{}}"#,
        );
        let out = normalize_item(&item, false, &empty_ring());
        assert!(out.messages.is_empty());
        assert!(out.chat_ended);
    }

    #[test]
    fn an_unknown_type_degrades_into_a_readable_row_with_raw_type_kept() {
        let item = wire(
            r#"{"id":"m14","snippet":{"type":"brandNewEventYouTubeAddedYesterday",
                "displayMessage":"something happened"},
                "authorDetails":{"channelId":"UCg","displayName":"Hal"}}"#,
        );
        let out = normalize_item(&item, false, &empty_ring());
        let msg = &out.messages[0];
        assert_eq!(msg.kind, MessageKind::Unknown);
        assert_eq!(msg.text, "something happened");
        let Some(PlatformMeta::YouTube(meta)) = &msg.meta else {
            panic!("expected YouTube meta");
        };
        assert_eq!(meta.raw_type, "brandNewEventYouTubeAddedYesterday");
    }

    #[test]
    fn sponsor_only_mode_changes_are_notices() {
        let started = wire(
            r#"{"id":"m15","snippet":{"type":"sponsorOnlyModeStartedEvent"},"authorDetails":{}}"#,
        );
        let out = normalize_item(&started, false, &empty_ring());
        assert_eq!(out.messages[0].kind, MessageKind::Notice);
        assert_eq!(out.messages[0].text, "members-only mode started");
    }

    #[test]
    fn badges_come_in_the_fixed_owner_mod_member_verified_order() {
        let author = WireAuthor {
            channel_id: "UCh".into(),
            display_name: "All Badges".into(),
            is_chat_owner: true,
            is_chat_moderator: true,
            is_chat_sponsor: true,
            is_verified: true,
        };
        let badges = badges_for(&author, "Gold", false);
        let sets: Vec<&str> = badges.iter().map(|b| b.set.as_str()).collect();
        assert_eq!(sets, ["owner", "moderator", "member", "verified"]);
    }

    // -- cadence ------------------------------------------------------------

    #[test]
    fn the_server_floor_beats_the_config_floor_and_jitter_never_dips_below() {
        let mut jitter = Lcg::new(7);
        let server = Duration::from_millis(5000);
        let config = Duration::from_millis(1000);
        for _ in 0..1000 {
            let interval =
                next_interval(server, config, Duration::ZERO, BACKOFF_FLOOR, &mut jitter);
            assert!(
                interval >= server,
                "the server floor is absolute; got {interval:?}"
            );
            assert!(interval <= server.mul_f64(1.0 + JITTER_FRACTION + 1e-9));
        }
        // And the other way round: a higher config floor wins over the server.
        for _ in 0..1000 {
            let interval =
                next_interval(config, server, Duration::ZERO, BACKOFF_FLOOR, &mut jitter);
            assert!(interval >= server);
        }
    }

    #[test]
    fn the_ceiling_stretches_the_cadence_but_never_pushes_below_the_floor() {
        let mut jitter = Lcg::new(3);
        // Ceiling above the floor: base is clamped to it.
        let interval = next_interval(
            Duration::from_secs(2),
            Duration::from_secs(1),
            Duration::from_secs(10),
            BACKOFF_FLOOR,
            &mut jitter,
        );
        assert!(interval >= Duration::from_secs(2));
        // A ceiling below the absolute floor loses: the floor wins.
        for _ in 0..100 {
            let interval = next_interval(
                Duration::from_secs(30),
                Duration::from_secs(1),
                Duration::from_secs(5),
                BACKOFF_FLOOR,
                &mut jitter,
            );
            assert!(interval >= Duration::from_secs(30));
        }
    }

    #[test]
    fn backoff_climbs_by_doubling_is_capped_and_decays_one_step() {
        let base = Duration::from_secs(5);
        let mut backoff = BACKOFF_FLOOR;
        backoff = climb(backoff, base, BACKOFF_TRANSIENT_CAP);
        assert_eq!(backoff, 2.0);
        backoff = climb(backoff, base, BACKOFF_TRANSIENT_CAP);
        assert_eq!(backoff, 4.0);
        backoff = climb(backoff, base, BACKOFF_TRANSIENT_CAP);
        // 8x on a 5s base would be 40s; still under the 60s cap.
        assert_eq!(backoff, 8.0);
        backoff = climb(backoff, base, BACKOFF_TRANSIENT_CAP);
        // 16x on 5s would be 80s: capped so base * multiplier == 60s.
        assert!((backoff - 12.0).abs() < 1e-9, "got {backoff}");

        // The rate-limit cap is higher: from 8x, doubling to 16x gives 80s,
        // still under the 120s cap.
        assert_eq!(climb(8.0, base, BACKOFF_RATE_LIMIT_CAP), 16.0);

        // A success decays one step, never below the floor.
        assert_eq!(decay(8.0), 4.0);
        assert_eq!(decay(1.5), 1.0);
        assert_eq!(decay(BACKOFF_FLOOR), BACKOFF_FLOOR);
    }

    // -- dedupe ring --------------------------------------------------------

    #[test]
    fn the_dedupe_ring_evicts_fifo_and_reports_new_vs_seen() {
        let mut ring = DedupeRing::new(3);
        assert!(ring.insert("a"));
        assert!(ring.insert("b"));
        assert!(ring.insert("c"));
        assert!(!ring.insert("a"), "a repeat is not new");
        // The fourth distinct id evicts the oldest ("a"), so "a" becomes new
        // again while "b" is still remembered.
        assert!(ring.insert("d"));
        assert!(!ring.has("a"));
        assert!(ring.has("b") && ring.has("c") && ring.has("d"));
        assert!(ring.insert("a"), "an evicted id reads as new again");
        // Empty ids are never recorded and never block a row.
        assert!(ring.insert(""));
        assert!(ring.insert(""));
        assert!(!ring.has(""));
    }

    // -- quota --------------------------------------------------------------

    #[test]
    fn the_quota_reserve_pauses_at_the_threshold_and_exhaustion_always_pauses() {
        let mut ledger = QuotaLedger::new(10_000);
        assert!(ledger.pause_reason(10).is_none());

        // Spend down to exactly the 10% reserve: 9000 used, 1000 remaining.
        ledger.charge(9_000);
        let reason = ledger.pause_reason(10).expect("the reserve must trip");
        assert!(
            reason.contains("sending"),
            "the reserve message must say sends still work: {reason}"
        );
        // One unit above the reserve does not trip.
        let mut above = QuotaLedger::new(10_000);
        above.charge(8_999);
        assert!(above.pause_reason(10).is_none());

        // With the reserve overridden (0%), only exhaustion pauses.
        assert!(ledger.pause_reason(0).is_none());
        ledger.charge(1_000);
        assert!(ledger.pause_reason(0).is_some());

        // A zero limit disables the ledger entirely.
        let mut unlimited = QuotaLedger::new(0);
        unlimited.charge(1_000_000);
        assert!(unlimited.pause_reason(10).is_none());
    }

    #[test]
    fn only_the_reserve_pause_offers_the_ctrl_r_override() {
        // The override clears the reserve and nothing else. When the whole
        // day's quota is gone the pause returns no matter what, so advertising
        // ctrl+r sent the user round a loop: each press re-resolved and polled
        // (spending real units) and then parked again immediately.
        let mut reserve_hit = QuotaLedger::new(10_000);
        reserve_hit.charge(9_000);
        let reserve = reserve_hit.pause_reason(10).expect("the reserve trips");
        assert!(reserve.contains("ctrl+r"), "reserve pause: {reserve}");

        let mut spent = QuotaLedger::new(10_000);
        spent.charge(10_000);
        let exhausted = spent.pause_reason(10).expect("exhaustion always pauses");
        assert!(
            !exhausted.contains("ctrl+r"),
            "exhaustion must not promise an override: {exhausted}"
        );
        // And with the reserve already overridden, the message is the same.
        assert!(!spent
            .pause_reason(0)
            .expect("still paused")
            .contains("ctrl+r"));
    }

    // -- moderation before the chat resolves ---------------------------------

    #[tokio::test]
    async fn banning_before_the_chat_resolves_costs_no_quota_and_says_why() {
        // A ban issued in the seconds before the chat id arrives used to send
        // `liveChatId: ""` — a guaranteed HTTP 400 — after already charging
        // 50 units, and the moderator saw only YouTube's opaque rejection.
        let quota = QuotaStore::new(10_000, None);
        let (events_tx, mut events_rx) = mpsc::unbounded_channel();
        let mut poller = Poller::new(SpawnParams {
            key: ChatKey {
                platform: Platform::YouTube,
                account: "youtube".into(),
                target: "CgABCDEFGHIJKLMNOPQRSTUVWXYZ".into(),
            },
            poll_floor_ms: 1,
            poll_ceiling_ms: 0,
            quota: quota.clone(),
            quota_reserve_percent: 10,
            token: Arc::new(|| Box::pin(async { Ok("test-token".to_string()) })),
            events: events_tx,
            client: reqwest::Client::new(),
            // Unreachable on purpose: a correct guard never reaches the network.
            base: Some("http://127.0.0.1:1/youtube/v3".to_string()),
        });
        poller.live_chat_id.clear();

        poller.handle_ban("UCsomeone".to_string(), None).await;

        assert_eq!(quota.remaining(), 10_000, "a refused ban must cost nothing");
        match events_rx
            .try_recv()
            .expect("the moderator must be told why")
        {
            (_, ChatEvent::Message(message)) => assert!(
                message.text.contains("still connecting"),
                "unhelpful notice: {}",
                message.text
            ),
            other => panic!("unexpected event: {other:?}"),
        }
    }

    // -- error classification ------------------------------------------------

    #[test]
    fn unlisted_http_statuses_are_transient_rather_than_auth_failures() {
        // These statuses say nothing about the saved login. Treating them as
        // auth failures parked the poller permanently and told the user to run
        // logging in again, which could never fix the real problem.
        for status in [402_u16, 405, 408, 409, 302] {
            let failure = classify(status, "");
            assert_eq!(
                failure.kind,
                FailKind::Transient,
                "HTTP {status} must be retried, not treated as a bad login"
            );
            assert!(!failure.detail.contains("msm login"));
        }

        // A genuine 401, and Google's explicit auth reasons, still park.
        assert_eq!(classify(401, "").kind, FailKind::Auth);
        assert_eq!(
            classify(403, "insufficientPermissions").kind,
            FailKind::Auth
        );
    }

    // -- grapheme cap -------------------------------------------------------

    #[test]
    fn the_send_cap_counts_graphemes_and_never_splits_a_cluster() {
        assert_eq!(cap_graphemes("hello", 200), "hello");
        assert_eq!(cap_graphemes("hello", 3), "hel");
        // A family emoji is one grapheme built from several code points; a
        // cap of 1 keeps it whole and a cap of 0 drops it whole.
        let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F466}";
        assert_eq!(cap_graphemes(family, 1), family);
        assert_eq!(cap_graphemes(family, 0), "");
        let two = format!("{family}x");
        assert_eq!(cap_graphemes(&two, 1), family);
    }

    // -- the poll loop against a fake server --------------------------------

    /// A local stand-in for the Data API (same pattern as the repo-root
    /// `youtube.rs` `fake_data_api`). It serves the list endpoint statefully:
    /// the first call primes with one message and a `nextPageToken`, the
    /// second continues with a fresh message, and every call after that
    /// answers 403 quotaExceeded.
    async fn fake_chat_api() -> (String, Arc<Mutex<Vec<String>>>, Arc<AtomicU32>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let calls = Arc::new(AtomicU32::new(0));
        let recorded = seen.clone();
        let counter = calls.clone();

        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let recorded = recorded.clone();
                let counter = counter.clone();
                tokio::spawn(async move {
                    let mut buffer = [0u8; 8192];
                    let read = socket.read(&mut buffer).await.unwrap_or(0);
                    let request = String::from_utf8_lossy(&buffer[..read]).to_string();
                    let target = request.split_whitespace().nth(1).unwrap_or("/").to_string();
                    recorded.lock().unwrap().push(target);
                    let call = counter.fetch_add(1, Ordering::SeqCst) + 1;

                    let (status, body) = match call {
                        1 => (
                            "200 OK",
                            r#"{"pollingIntervalMillis":1,"nextPageToken":"PAGE2","items":[
                                {"id":"m1","snippet":{"type":"textMessageEvent",
                                 "publishedAt":"2026-08-01T12:00:00Z",
                                 "textMessageDetails":{"messageText":"from the backlog"}},
                                 "authorDetails":{"channelId":"UCa","displayName":"Alice"}}]}"#
                                .to_string(),
                        ),
                        2 => (
                            "200 OK",
                            r#"{"pollingIntervalMillis":1,"nextPageToken":"PAGE3","items":[
                                {"id":"m1","snippet":{"type":"textMessageEvent",
                                 "textMessageDetails":{"messageText":"from the backlog"}},
                                 "authorDetails":{"channelId":"UCa","displayName":"Alice"}},
                                {"id":"m2","snippet":{"type":"textMessageEvent",
                                 "publishedAt":"2026-08-01T12:00:05Z",
                                 "textMessageDetails":{"messageText":"live now"}},
                                 "authorDetails":{"channelId":"UCb","displayName":"Bob"}}]}"#
                                .to_string(),
                        ),
                        _ => (
                            "403 Forbidden",
                            r#"{"error":{"code":403,"message":"quota",
                                "errors":[{"reason":"quotaExceeded"}]}}"#
                                .to_string(),
                        ),
                    };
                    let response = format!(
                        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = socket.write_all(response.as_bytes()).await;
                    let _ = socket.flush().await;
                });
            }
        });

        (base, seen, calls)
    }

    /// Drives the whole loop: prime -> stream -> a second page -> quota error
    /// -> QuotaPaused park, asserting the emitted events and that polling
    /// actually stops.
    #[tokio::test]
    async fn the_poller_primes_streams_pages_and_parks_on_quota_exhaustion() {
        let (base, seen, calls) = fake_chat_api().await;
        let (events_tx, mut events_rx) = mpsc::unbounded_channel();
        let key = ChatKey {
            platform: Platform::YouTube,
            account: "youtube".into(),
            // A bare live chat id resolves for free: zero resolve calls.
            target: "CgABCDEFGHIJKLMNOPQRSTUVWXYZ".into(),
        };
        let handle = spawn(SpawnParams {
            key: key.clone(),
            poll_floor_ms: 1,
            poll_ceiling_ms: 0,
            quota: QuotaStore::new(10_000, None),
            quota_reserve_percent: 10,
            token: Arc::new(|| Box::pin(async { Ok("test-token".to_string()) })),
            events: events_tx,
            client: reqwest::Client::new(),
            base: Some(base),
        });

        // Collect events until the quota pause arrives (or a timeout fails
        // the test rather than hanging it).
        let mut statuses = Vec::new();
        let mut messages: Vec<ChatMessage> = Vec::new();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            let event = tokio::time::timeout_at(deadline, events_rx.recv())
                .await
                .expect("the poller must reach QuotaPaused before the deadline")
                .expect("the event channel must stay open while the task runs");
            let (event_key, event) = event;
            assert_eq!(event_key, key);
            match event {
                ChatEvent::Message(msg) => messages.push(*msg),
                ChatEvent::Connection { status, detail } => {
                    statuses.push((status, detail.clone()));
                    if status == ConnectionStatus::QuotaPaused {
                        break;
                    }
                }
                other => panic!("unexpected event: {other:?}"),
            }
        }

        // The lifecycle: connecting (prime), connected, quota paused.
        assert!(statuses
            .iter()
            .any(|(s, _)| *s == ConnectionStatus::Connecting));
        assert!(statuses
            .iter()
            .any(|(s, _)| *s == ConnectionStatus::Connected));
        let (_, pause_detail) = statuses.last().expect("at least the pause");
        // A quota YouTube itself reports as spent cannot be overridden, so the
        // pause must not invite an override that would only re-poll and
        // re-park.
        assert!(
            !pause_detail.contains("ctrl+r"),
            "hard exhaustion must not promise an override: {pause_detail}"
        );

        // The primed row is historical; the streamed row is not; the
        // re-delivered m1 on page two was swallowed by the dedupe ring.
        assert_eq!(messages.len(), 2, "exactly two rows: {messages:?}");
        assert_eq!(messages[0].id, "m1");
        assert!(messages[0].historical);
        assert_eq!(messages[1].id, "m2");
        assert!(!messages[1].historical);
        assert_eq!(messages[1].text, "live now");

        // The requests: a token-less prime, then the retained page token.
        let requests = seen.lock().unwrap().clone();
        assert!(requests.len() >= 3, "prime, page, quota: {requests:?}");
        assert!(
            requests[0].contains("maxResults=2000") && !requests[0].contains("pageToken"),
            "the prime carries no token: {requests:?}"
        );
        assert!(
            requests[1].contains("pageToken=PAGE2"),
            "the second call continues from the first: {requests:?}"
        );

        // Parked means parked: no further requests arrive.
        let after_pause = calls.load(Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert_eq!(
            calls.load(Ordering::SeqCst),
            after_pause,
            "polling must stop while quota-paused"
        );

        // Dropping the handle closes the command channel and ends the task.
        let task = handle.task;
        drop(handle.commands);
        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .expect("the task must end when the handle is dropped")
            .expect("and end cleanly");
    }

    /// The redaction invariant: no detail string produced by classification
    /// carries a URL or a token-shaped value.
    #[test]
    fn classified_errors_never_mention_urls() {
        for (status, reason) in [
            (403, "quotaExceeded"),
            (403, "rateLimitExceeded"),
            (403, "liveChatEnded"),
            (404, ""),
            (400, "badThing"),
            (401, ""),
            (500, ""),
        ] {
            let failure = classify(status, reason);
            assert!(
                !failure.detail.contains("http://")
                    && !failure.detail.contains("https://")
                    && !failure.detail.contains("googleapis"),
                "detail leaks transport internals: {}",
                failure.detail
            );
        }
    }
}
