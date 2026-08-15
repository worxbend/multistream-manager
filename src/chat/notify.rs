//! Desktop notifications for high-signal chat events.
//!
//! Ported from `yc` (commit 9e67efd10c0790ec22df2c944bcee6be1bc37cf8,
//! internal/app/notify.go); `twi` (commit
//! 7c6ad6bbbc3dec1b6af2ddbd03b55f96c0c162cf) carries an equivalent notifier,
//! so `yc` is the single behavioral authority here.
//!
//! The reference uses no notification library, and neither does this port:
//! each supported platform is one subprocess launch, which keeps the
//! dependency set at zero for a feature that is optional by nature. A missed
//! notification must never take down chat, so every failure degrades (to the
//! terminal bell, or to nothing) instead of propagating.
//!
//! Deviation from the reference: `yc` ships a Windows toast via a PowerShell
//! `-EncodedCommand` script. That path is omitted here — this application is
//! Linux-first, and the UTF-16LE-encoded embedded script is high-maintenance
//! for a platform this program does not target; Windows gets a debug log line
//! instead.

use std::io::Write as _;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use unicode_segmentation::UnicodeSegmentation;

use crate::chat::{ChatMessage, MembershipKind, MessageKind, PlatformMeta};

/// Grapheme cap for a notification title (yc: sanitizeNotificationText 96).
const TITLE_LIMIT: usize = 96;
/// Grapheme cap for a notification body (yc: sanitizeNotificationText 320).
const BODY_LIMIT: usize = 320;
/// Minimum gap between two delivered notifications. The reference coalesces
/// gift bursts at the UI layer; this port's equivalent guard lives in the
/// notifier itself: a flood of high-signal events (a gift drop delivers one
/// event per recipient) must not become a flood of desktop toasts.
const MIN_GAP: Duration = Duration::from_secs(2);

/// Fires desktop notifications, throttled and failure-tolerant.
pub struct Notifier {
    enabled: bool,
    /// When the last notification was actually dispatched; used to throttle.
    last: Option<Instant>,
}

impl Notifier {
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            last: None,
        }
    }

    /// Delivers one notification, best-effort.
    ///
    /// The subprocess is spawned detached (all stdio null) and never waited
    /// on from the caller's thread: chat rendering must not stall behind a
    /// notification daemon. A spawn failure is logged at debug and dropped —
    /// the fallback is the terminal bell on stderr.
    pub fn notify(&mut self, title: &str, body: &str) {
        if !self.enabled {
            return;
        }
        let now = Instant::now();
        if let Some(last) = self.last {
            if now.duration_since(last) < MIN_GAP {
                return;
            }
        }
        self.last = Some(now);

        let mut title = sanitize(title, TITLE_LIMIT);
        let body = sanitize(body, BODY_LIMIT);
        if title.is_empty() {
            title = "msm".to_string();
        }
        dispatch(&title, &body);
    }
}

/// Launches the platform's notification tool, falling back to a terminal
/// bell when the platform has none or the launch fails.
fn dispatch(title: &str, body: &str) {
    let launched = if cfg!(any(
        target_os = "linux",
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "openbsd"
    )) {
        // The "--" terminator matters: notify-send accepts options anywhere
        // on the command line, and the body starts with an attacker-chosen
        // author name, so a chatter called "--icon=/etc/passwd" would
        // otherwise be parsed as an option rather than displayed as text.
        let mut cmd = Command::new("notify-send");
        cmd.args([
            "--app-name=msm",
            "--urgency=normal",
            "--expire-time=8000",
            "--",
            title,
        ]);
        if !body.is_empty() {
            cmd.arg(body);
        }
        spawn_detached(cmd)
    } else if cfg!(target_os = "macos") {
        // Title and body travel as argv items into an AppleScript `on run`
        // handler — never interpolated into the script text, so chat content
        // cannot inject script.
        let mut cmd = Command::new("osascript");
        cmd.args([
            "-e",
            "on run argv",
            "-e",
            "display notification item 2 of argv with title item 1 of argv",
            "-e",
            "end run",
            title,
            body,
        ]);
        spawn_detached(cmd)
    } else if cfg!(target_os = "windows") {
        // Deliberately skipped: the reference's PowerShell toast is an
        // embedded UTF-16LE-encoded script, high-maintenance for a platform
        // this Linux-first app does not target.
        tracing::debug!("desktop notification skipped: no Windows toast support in this port");
        return;
    } else {
        false
    };

    if !launched {
        // Terminal bell fallback (yc: terminalBellNotifier). Stderr, because
        // the TUI owns stdout.
        let _ = std::io::stderr().write_all(b"\x07");
    }
}

/// Spawns a command with all stdio detached and hands the child to a reaper
/// thread. Returns whether the spawn succeeded.
fn spawn_detached(mut cmd: Command) -> bool {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    match cmd.spawn() {
        Ok(mut child) => {
            // Reap off-thread: an un-waited child stays a zombie until this
            // process exits, and the caller's thread must never block on a
            // notification daemon.
            std::thread::spawn(move || {
                let _ = child.wait();
            });
            true
        }
        Err(err) => {
            // A notification that cannot be delivered is not a chat failure,
            // so the error is logged at debug and dropped.
            tracing::debug!("desktop notification failed to launch: {err}");
            false
        }
    }
}

/// Sanitizes text bound for a shell-launched notifier: control characters
/// become spaces, whitespace runs collapse to one space, and the result is
/// capped at `limit` grapheme clusters (yc: sanitizeNotificationText).
///
/// Clusters, not chars: display names and chat bodies are routinely a run of
/// ZWJ emoji, and a char-sliced limit cuts one in half — which leaves a
/// dangling joiner in a string handed to a subprocess, where it binds to
/// whatever the daemon prints next.
fn sanitize(value: &str, limit: usize) -> String {
    let cleaned: String = value
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let collapsed = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    if limit == 0 {
        return collapsed;
    }
    let count = collapsed.graphemes(true).count();
    if count <= limit {
        return collapsed;
    }
    // Reserve room for the ellipsis when the cap allows it, exactly as the
    // reference does.
    let (keep, suffix) = if limit > 3 {
        (limit - 3, "...")
    } else {
        (limit, "")
    };
    let truncated: String = collapsed.graphemes(true).take(keep).collect();
    truncated + suffix
}

/// Decides whether a message is worth a desktop notification, and builds the
/// title/body when it is.
///
/// The selection is deliberately narrow (yc: notificationFromMessage): only
/// money and membership events qualify — things a creator wants pulled out of
/// a fast-moving chat. Ordinary messages, mode changes, and moderation are
/// never notified, because a notification for every line is a notification
/// for none. Historical rows are backlog, not news (notifying for the priming
/// page would mean a burst of alerts for events from before the app started);
/// local echoes are our own sends; deleted rows are moderation, not news.
pub fn high_signal(msg: &ChatMessage) -> Option<(String, String)> {
    if msg.historical || msg.local_echo || msg.deleted {
        return None;
    }
    let title = match msg.kind {
        MessageKind::Paid => paid_title(msg),
        MessageKind::Membership => membership_title(msg),
        _ => return None,
    };
    Some((title, body_for(msg)))
}

/// Title for a paid event: the event name plus the platform's pre-localized
/// amount display when one exists (yc: notificationTitleForMessage).
fn paid_title(msg: &ChatMessage) -> String {
    let (name, display) = match &msg.meta {
        Some(PlatformMeta::YouTube(meta)) => {
            // The wire type distinguishes a Super Sticker from a Super Chat;
            // both are `Paid` in the normalized model.
            let name = if meta.raw_type.to_ascii_lowercase().contains("sticker") {
                "Super Sticker"
            } else {
                "Super Chat"
            };
            let display = meta.paid.as_ref().map(|p| p.display.as_str()).unwrap_or("");
            (name, display)
        }
        _ => ("Super Chat", ""),
    };
    if display.is_empty() {
        name.to_string()
    } else {
        format!("{name} {display}")
    }
}

/// Title for a membership event (yc: notificationTitleForMessage).
fn membership_title(msg: &ChatMessage) -> String {
    let kind = match &msg.meta {
        Some(PlatformMeta::YouTube(meta)) => meta.membership.as_ref().map(|m| m.kind),
        _ => None,
    };
    match kind {
        Some(MembershipKind::New) => "New member",
        // The reference has no distinct upgrade string; the closest label.
        Some(MembershipKind::Upgrade) => "New member",
        Some(MembershipKind::Milestone) => "Member milestone",
        Some(MembershipKind::Gifting) => "Gifted memberships",
        Some(MembershipKind::GiftReceived) => "Gift membership received",
        // A Membership row with no details (e.g. a Twitch sub normalized
        // without meta) still deserves its generic title.
        None => "New member",
    }
    .to_string()
}

/// Body: author, membership context when present, then the message text
/// (yc: notificationBodyForMessage).
fn body_for(msg: &ChatMessage) -> String {
    let mut parts: Vec<String> = Vec::new();
    let author = msg.author.display_name.trim();
    if !author.is_empty() {
        parts.push(author.to_string());
    }
    if let Some(PlatformMeta::YouTube(meta)) = &msg.meta {
        if let Some(membership) = &meta.membership {
            let level = membership.level.trim();
            if !level.is_empty() {
                parts.push(level.to_string());
            }
            // Zero means "the platform did not say", which must render as
            // nothing rather than "0 months".
            if membership.months > 0 {
                parts.push(format!("{} months", membership.months));
            }
            if membership.gift_count > 0 {
                parts.push(format!("{} gifts", membership.gift_count));
            }
        }
    }
    let head = parts.join(" · ");
    let text = msg.text.trim();
    match (head.is_empty(), text.is_empty()) {
        (_, true) => head,
        (true, false) => text.to_string(),
        (false, false) => format!("{head}: {text}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::{ChatAuthor, MembershipDetails, PaidAmount, YouTubeMeta};

    fn message(kind: MessageKind) -> ChatMessage {
        ChatMessage {
            id: "m1".into(),
            timestamp: None,
            author: ChatAuthor {
                display_name: "Alice".into(),
                ..Default::default()
            },
            text: "hello".into(),
            kind,
            deleted: false,
            historical: false,
            local_echo: false,
            meta: None,
        }
    }

    fn paid_message() -> ChatMessage {
        let mut msg = message(MessageKind::Paid);
        msg.meta = Some(PlatformMeta::YouTube(YouTubeMeta {
            raw_type: "superChatEvent".into(),
            paid: Some(PaidAmount {
                micros: 5_000_000,
                currency: "USD".into(),
                display: "$5.00".into(),
                tier: 2,
            }),
            membership: None,
        }));
        msg
    }

    #[test]
    fn sanitize_replaces_control_chars_and_collapses_whitespace() {
        assert_eq!(sanitize("a\x00b\tc\r\nd", 96), "a b c d");
        assert_eq!(sanitize("  lots   of \t spaces  ", 96), "lots of spaces");
    }

    #[test]
    fn sanitize_caps_at_grapheme_boundaries() {
        // Family emoji: one grapheme cluster, seven chars. A char-based cap
        // would split it; a grapheme cap keeps it whole.
        let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}\u{200D}\u{1F466}";
        let input = family.repeat(10);
        let out = sanitize(&input, 5);
        assert_eq!(out, family.repeat(2) + "...");
    }

    #[test]
    fn sanitize_short_limits_have_no_ellipsis() {
        assert_eq!(sanitize("abcdef", 3), "abc");
    }

    #[test]
    fn sanitize_leaves_short_text_alone() {
        assert_eq!(sanitize("short", 96), "short");
    }

    #[test]
    fn high_signal_notifies_for_paid() {
        let (title, body) = high_signal(&paid_message()).expect("paid must notify");
        assert_eq!(title, "Super Chat $5.00");
        assert_eq!(body, "Alice: hello");
    }

    #[test]
    fn high_signal_distinguishes_super_sticker() {
        let mut msg = paid_message();
        if let Some(PlatformMeta::YouTube(meta)) = &mut msg.meta {
            meta.raw_type = "superStickerEvent".into();
        }
        let (title, _) = high_signal(&msg).expect("sticker must notify");
        assert_eq!(title, "Super Sticker $5.00");
    }

    #[test]
    fn high_signal_notifies_for_membership_with_context() {
        let mut msg = message(MessageKind::Membership);
        msg.text = String::new();
        msg.meta = Some(PlatformMeta::YouTube(YouTubeMeta {
            raw_type: "memberMilestoneChatEvent".into(),
            paid: None,
            membership: Some(MembershipDetails {
                kind: MembershipKind::Milestone,
                level: "Gold".into(),
                months: 12,
                gift_count: 0,
            }),
        }));
        let (title, body) = high_signal(&msg).expect("membership must notify");
        assert_eq!(title, "Member milestone");
        assert_eq!(body, "Alice · Gold · 12 months");
    }

    #[test]
    fn high_signal_ignores_plain_chat() {
        assert!(high_signal(&message(MessageKind::Chat)).is_none());
        assert!(high_signal(&message(MessageKind::Action)).is_none());
        assert!(high_signal(&message(MessageKind::Notice)).is_none());
        assert!(high_signal(&message(MessageKind::Unknown)).is_none());
    }

    #[test]
    fn high_signal_ignores_historical_local_echo_and_deleted_paid() {
        let mut historical = paid_message();
        historical.historical = true;
        assert!(high_signal(&historical).is_none());

        let mut echo = paid_message();
        echo.local_echo = true;
        assert!(high_signal(&echo).is_none());

        let mut deleted = paid_message();
        deleted.deleted = true;
        assert!(high_signal(&deleted).is_none());
    }

    #[test]
    fn disabled_notifier_is_a_no_op() {
        // Must not spawn anything or bell; also exercises the enabled gate.
        let mut notifier = Notifier::new(false);
        notifier.notify("title", "body");
        assert!(notifier.last.is_none());
    }
}
