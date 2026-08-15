//! The normalized chat model shared by every chat backend.
//!
//! Twitch chat arrives over IRC with IRCv3 tags; YouTube live chat arrives as
//! JSON pages from the Data API. The terminal UI must not care: everything is
//! normalized into the one [`ChatMessage`] shape defined here, and whatever is
//! genuinely platform-specific (bits, Super Chat tiers, membership levels)
//! rides in the optional [`PlatformMeta`] side-car instead of widening the
//! core struct.
//!
//! Ported behavior notes: the model mirrors the normalized message types of
//! `twi` (commit 7c6ad6b, internal/twitch/client.go) and `yc` (commit 9e67efd,
//! internal/youtube/model.go). Two of their shared invariants are load-bearing
//! here and are enforced by the consumers of this model:
//!
//! * **Deleted text is never reprinted.** A deletion flips [`ChatMessage::deleted`];
//!   renderers must replace the body, not restyle it.
//! * **Absent data is never a negative fact.** Empty strings and `None` mean
//!   "unknown", not "no" — e.g. a member whose tenure was never observed has
//!   `months: 0`, which must render as nothing rather than "0 months".

// The chat feature lands across several commits; until the adapters and the
// UI consume these types, the dead-code lint would fail the build. Removed in
// the commit that wires the Chat tab.
#![allow(dead_code)]

pub mod ratelimit;
pub mod ring;

use chrono::{DateTime, Utc};

use crate::model::Platform;

/// Someone who said something (or had something happen to them) in a chat.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChatAuthor {
    /// The platform's stable id: Twitch `user-id`, YouTube `channelId`.
    /// This — not the display name — keys grouping and identity colors,
    /// because it is the one thing a chatter cannot change.
    pub id: String,
    /// Twitch login / YouTube `@handle`. Empty when unknown (YouTube only
    /// exposes a handle when the channel URL carries one).
    pub login: String,
    /// The name to render.
    pub display_name: String,
    /// Normalized badges, already in display order.
    pub badges: Vec<Badge>,
    /// Twitch's `color` tag, kept for inspection. Deliberately *not* used for
    /// rendering: both source apps derive the rendered color from the author
    /// identity so it stays readable on any palette.
    pub color_hint: Option<String>,
}

/// One chat badge, normalized across platforms.
///
/// Twitch: `set` is the badge set id (`moderator`, `subscriber`, …), `id` the
/// version, `info` the badge-info payload (e.g. subscriber tenure months).
/// YouTube: synthesized sets `owner` / `moderator` / `member` / `verified`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Badge {
    pub set: String,
    pub id: String,
    pub info: String,
}

/// What kind of row a message renders as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageKind {
    /// An ordinary chat line.
    Chat,
    /// A `/me` action (Twitch CTCP ACTION).
    Action,
    /// A platform notice rendered with a `[notice]` prefix (USERNOTICE text,
    /// members-only-mode toggles, poll announcements, …).
    Notice,
    /// Money attached: bits are on a `Chat` row via meta, but Super Chats,
    /// Super Stickers and gifts are their own kind so filters can find them.
    Paid,
    /// Membership / subscription events.
    Membership,
    /// A type this program does not recognize. Rendered readably from
    /// whatever display text the platform supplied — never dropped.
    Unknown,
}

/// The one message shape the UI consumes.
#[derive(Debug, Clone)]
pub struct ChatMessage {
    /// Platform message id. Used to reconcile local echoes and deletions.
    pub id: String,
    /// When the platform says it was sent. `None` renders as `--:--`.
    pub timestamp: Option<DateTime<Utc>>,
    pub author: ChatAuthor,
    /// The plain text body. Fragmentation (mentions, emotes, emoji) happens
    /// at render time so the stored history stays cheap.
    pub text: String,
    pub kind: MessageKind,
    /// The body must render as `[message deleted]` and the original text must
    /// never reach the screen again.
    pub deleted: bool,
    /// Part of the backlog delivered when the chat was first opened; must not
    /// trigger notifications or unread counts.
    pub historical: bool,
    /// Rendered optimistically from our own successful send, replaced when
    /// the platform echoes the authoritative copy back.
    pub local_echo: bool,
    /// Platform-specific extras.
    pub meta: Option<PlatformMeta>,
}

/// Platform-specific extras that ride alongside the normalized core.
#[derive(Debug, Clone)]
pub enum PlatformMeta {
    Twitch(TwitchMeta),
    YouTube(YouTubeMeta),
}

/// Twitch extras (twi: internal/twitch/irc.go normalizers).
#[derive(Debug, Clone, Default)]
pub struct TwitchMeta {
    /// From the `bits` tag; 0 = no cheer.
    pub bits: u64,
    /// `first-msg` tag: this author's first message ever in the channel.
    pub first_message: bool,
    /// USERNOTICE `msg-id` (`sub`, `raid`, `announcement`, …) — empty for
    /// ordinary messages.
    pub system_event: String,
    /// Reply threading context from the `reply-parent-*` tags.
    pub reply: Option<ReplyContext>,
}

/// The message a reply points at (Twitch threads; YouTube has no threading —
/// its convention is a literal `@Name ` prefix, so it never produces this).
#[derive(Debug, Clone, Default)]
pub struct ReplyContext {
    pub parent_id: String,
    pub parent_author: String,
    pub parent_text: String,
}

/// YouTube extras (yc: internal/youtube/model.go).
#[derive(Debug, Clone, Default)]
pub struct YouTubeMeta {
    /// The wire `snippet.type`, preserved verbatim so unknown types stay
    /// diagnosable.
    pub raw_type: String,
    pub paid: Option<PaidAmount>,
    pub membership: Option<MembershipDetails>,
}

/// Money, in integer micros. No float ever touches an amount between the wire
/// and the screen (yc invariant: floats corrupt currency).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PaidAmount {
    pub micros: i64,
    pub currency: String,
    /// YouTube's pre-localized `amountDisplayString`, preferred for display.
    pub display: String,
    /// Super Chat tier 1–11; 0 = unknown / not tiered.
    pub tier: u8,
}

/// What happened, membership-wise.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MembershipKind {
    New,
    Upgrade,
    Milestone,
    Gifting,
    GiftReceived,
}

#[derive(Debug, Clone)]
pub struct MembershipDetails {
    pub kind: MembershipKind,
    /// Level name, empty when the platform did not say.
    pub level: String,
    /// Milestone months; 0 = unknown, which must render as nothing.
    pub months: u32,
    /// Gifted membership count for gifting events.
    pub gift_count: u32,
}

/// The visible connection state of one chat.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionStatus {
    Connecting,
    Connected,
    Reconnecting,
    Disconnected,
    Failed,
    /// YouTube only: polling stopped because the daily API quota ran out (or
    /// crossed the reserve threshold). A hard failure mode the user must see.
    QuotaPaused,
    /// The chat ended (stream over, chat disabled). History stays readable.
    Closed,
}

impl ConnectionStatus {
    /// Short label for pane headers.
    pub fn label(self) -> &'static str {
        match self {
            Self::Connecting => "connecting",
            Self::Connected => "connected",
            Self::Reconnecting => "reconnecting",
            Self::Disconnected => "disconnected",
            Self::Failed => "failed",
            Self::QuotaPaused => "quota paused",
            Self::Closed => "ended",
        }
    }
}

/// Everything a chat backend can tell the UI.
///
/// One enum over one channel, rather than the five parallel channels the Go
/// originals use: the UI demultiplexes with a `match`, and a single channel
/// preserves the relative order of messages and the moderation events that
/// refer to them.
#[derive(Debug, Clone)]
pub enum ChatEvent {
    /// Boxed: a ChatMessage is an order of magnitude larger than the other
    /// variants, and every connection-state event would otherwise pay its
    /// size on the channel.
    Message(Box<ChatMessage>),
    Connection {
        status: ConnectionStatus,
        /// Human-readable explanation ("chat cursor expired", "out of API
        /// quota until 09:00"). Empty when the status speaks for itself.
        detail: String,
    },
    /// One message was removed (Twitch CLEARMSG, YouTube tombstone/deletion).
    MessageDeleted { message_id: String },
    /// Every message from one author was removed (Twitch CLEARCHAT with a
    /// target). `timeout_secs` is `None` for a permanent ban.
    UserPurged {
        author_login: String,
        timeout_secs: Option<u64>,
    },
    /// The whole chat was cleared by a moderator.
    ChatCleared,
}

/// A chat someone can open: which platform, through which account, watching
/// which channel.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ChatKey {
    pub platform: Platform,
    /// The token-store key of the account this chat speaks through
    /// (`twitch`, `twitch:somelogin`, `youtube`, `youtube:UC…`).
    pub account: String,
    /// Twitch: lowercase channel login. YouTube: the raw target as typed
    /// (video id, `@handle`, URL, …) — resolution happens in the adapter.
    pub target: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connection_status_labels_are_lowercase_and_short() {
        for status in [
            ConnectionStatus::Connecting,
            ConnectionStatus::Connected,
            ConnectionStatus::Reconnecting,
            ConnectionStatus::Disconnected,
            ConnectionStatus::Failed,
            ConnectionStatus::QuotaPaused,
            ConnectionStatus::Closed,
        ] {
            let label = status.label();
            assert!(!label.is_empty() && label.len() <= 12);
            assert_eq!(label, label.to_lowercase());
        }
    }
}
