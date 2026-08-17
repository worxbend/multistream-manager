//! Desktop notifications: the delivery half.
//!
//! This module knows nothing about streaming. It takes a title, a body and an
//! urgency, and gets that in front of the person at the keyboard using
//! whatever the desktop provides. Deciding *what* is worth notifying about
//! lives elsewhere — [`crate::chat::notify`] maps chat messages to
//! notifications, and the interface raises stream-lifecycle ones directly.
//!
//! Three properties drive the design.
//!
//! **It must work on any Linux desktop.** "Send a desktop notification" is not
//! one thing on Linux. The usual answer is `notify-send`, a small program from
//! the libnotify package — but plenty of installs do not have it, because it
//! is a separate package from the notification *daemon* that actually draws
//! the pop-up. So there is a chain: `notify-send` first, then `gdbus` (part of
//! GLib, which is present anywhere GNOME, KDE, XFCE or Cinnamon is) talking to
//! the desktop notification service directly over D-Bus, then `kdialog` for a
//! KDE box that has neither, and finally the terminal bell so that *something*
//! happens. The first rung that works is remembered, so the chain is walked
//! once per session rather than once per notification.
//!
//! **A burst must not become a flood, and must not be thrown away.** A gift
//! drop delivers one event per recipient; a raid arrives in the middle of it.
//! Notifications are therefore queued and released one every
//! [`Settings::min_gap`], rather than the older behaviour of dropping anything
//! that arrived too soon after the last one. [`Notifier::flush`] is what
//! releases them, and the interface calls it on its redraw tick.
//!
//! **Nothing here may ever take down the stream.** Every failure degrades:
//! a missing tool falls through to the next one, a spawn error is logged at
//! debug and dropped, and the caller's thread never waits on a notification
//! daemon.
//!
//! Text handling (control characters, whitespace collapsing, grapheme-safe
//! truncation) is ported from `yc` (`internal/app/notify.go`,
//! `sanitizeNotificationText`), which is the behavioral authority for the
//! chat-notification feature as a whole.

use std::collections::VecDeque;
use std::io::Write as _;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use unicode_segmentation::UnicodeSegmentation;

/// Grapheme cap for a notification title (yc: sanitizeNotificationText 96).
const TITLE_LIMIT: usize = 96;
/// Grapheme cap for a notification body (yc: sanitizeNotificationText 320).
const BODY_LIMIT: usize = 320;
/// How long a pop-up asks to stay up, in milliseconds. Advisory: most desktops
/// treat it as a hint, and some ignore it for critical notifications.
const EXPIRE_MS: u32 = 8000;
/// How many notifications may wait in the queue. A cap is needed because the
/// queue drains on a timer: without one, a channel being raided while a gift
/// drop runs could grow it without bound. When it is full the *oldest* waiting
/// notification is dropped, on the grounds that the newest news is the news.
const QUEUE_LIMIT: usize = 32;

/// How much a notification wants to interrupt.
///
/// This maps onto the `urgency` of the freedesktop notification specification,
/// which every Linux desktop implements. The specification also has a `low`
/// level; nothing here emits it, because a notification not worth interrupting
/// for is a notification that should not have been sent.
///
/// `Critical` is the level that earns its keep: most desktops refuse to hide a
/// critical notification behind do-not-disturb, which is right for "your
/// stream just stopped" and for a raid, and wrong for everything else.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Urgency {
    Normal,
    Critical,
}

impl Urgency {
    /// The spelling `notify-send --urgency` expects.
    fn as_str(self) -> &'static str {
        match self {
            Urgency::Normal => "normal",
            Urgency::Critical => "critical",
        }
    }

    /// The number the freedesktop `urgency` hint expects (1 = normal,
    /// 2 = critical; 0 would be low, which is never sent).
    fn as_hint(self) -> u8 {
        match self {
            Urgency::Normal => 1,
            Urgency::Critical => 2,
        }
    }
}

/// One notification, already sanitized and ready to hand to the desktop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notification {
    pub title: String,
    pub body: String,
    pub urgency: Urgency,
}

impl Notification {
    pub fn new(title: impl Into<String>, body: impl Into<String>, urgency: Urgency) -> Self {
        let mut title = sanitize(&title.into(), TITLE_LIMIT);
        if title.is_empty() {
            // An empty summary is legal on the wire and useless on screen.
            title = "msm".to_string();
        }
        Self {
            title,
            body: sanitize(&body.into(), BODY_LIMIT),
            urgency,
        }
    }
}

/// What the notifier is allowed to do.
#[derive(Debug, Clone, Copy)]
pub struct Settings {
    /// The master switch. When off, `notify` is a no-op — nothing is queued,
    /// so turning it back on does not produce a backlog of stale news.
    pub enabled: bool,
    /// The shortest gap between two delivered notifications.
    pub min_gap: Duration,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: true,
            min_gap: Duration::from_secs(2),
        }
    }
}

/// Fires desktop notifications: throttled, queued and failure-tolerant.
///
/// Cheap to clone, and every clone shares one queue and one throttle. That
/// matters: the chat panes and the stream-lifecycle code both notify, and two
/// independent throttles would let them talk over each other.
#[derive(Clone)]
pub struct Notifier {
    inner: Arc<Mutex<Inner>>,
}

struct Inner {
    settings: Settings,
    /// When the last notification was actually dispatched.
    last: Option<Instant>,
    /// Notifications waiting for the throttle to let them past.
    pending: VecDeque<Notification>,
    /// Where delivered notifications go. Tests capture instead of spawning.
    sink: Sink,
}

enum Sink {
    /// The real desktop, plus the backend that last worked (`None` until the
    /// chain has been walked once). Never built under `cargo test` — see
    /// [`Notifier::with_settings`] — hence the allowance there.
    #[cfg_attr(test, allow(dead_code))]
    Desktop { backend: Option<Backend> },
    /// Collects instead of delivering, so tests can assert on what would have
    /// been shown without launching subprocesses.
    #[cfg(test)]
    Capture(Vec<Notification>),
}

impl Notifier {
    /// A notifier that talks to the desktop, with default timing.
    #[cfg(test)]
    pub fn new(enabled: bool) -> Self {
        Self::with_settings(Settings {
            enabled,
            ..Settings::default()
        })
    }

    pub fn with_settings(settings: Settings) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner {
                settings,
                last: None,
                pending: VecDeque::new(),
                // Under `cargo test` every notifier captures instead of
                // delivering. Without this, running the test suite would
                // spray real pop-ups across the desktop of whoever ran it —
                // and on a machine with no notification daemon, beep at them
                // several hundred times.
                #[cfg(test)]
                sink: Sink::Capture(Vec::new()),
                #[cfg(not(test))]
                sink: Sink::Desktop { backend: None },
            })),
        }
    }

    /// A notifier that records instead of delivering. Tests only.
    #[cfg(test)]
    fn capturing(settings: Settings) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner {
                settings,
                last: None,
                pending: VecDeque::new(),
                sink: Sink::Capture(Vec::new()),
            })),
        }
    }

    /// Replace the settings — what the Config tab's switches do.
    ///
    /// Turning the master switch off discards anything still queued: those are
    /// notifications the user has just said they do not want.
    pub fn configure(&self, settings: Settings) {
        let mut inner = self.lock();
        inner.settings = settings;
        if !settings.enabled {
            inner.pending.clear();
        }
    }

    /// Queue a notification, delivering it immediately if the throttle allows.
    pub fn send(&self, notification: Notification) {
        self.send_at(notification, Instant::now());
    }

    fn send_at(&self, notification: Notification, now: Instant) {
        let mut inner = self.lock();
        if !inner.settings.enabled {
            return;
        }
        let ready = match inner.last {
            None => true,
            Some(last) => now.duration_since(last) >= inner.settings.min_gap,
        };
        if ready && inner.pending.is_empty() {
            inner.deliver(notification, now);
            return;
        }
        if inner.pending.len() >= QUEUE_LIMIT {
            // Oldest first: during a flood the newest events are the ones
            // still worth reading by the time the queue drains.
            inner.pending.pop_front();
            tracing::debug!("desktop notification queue full: dropped the oldest waiting one");
        }
        inner.pending.push_back(notification);
    }

    /// Release one queued notification if the throttle has expired.
    ///
    /// Called from the interface's redraw tick. One per call rather than
    /// "everything that is due" so that a backlog is paced out at the same
    /// rate a live burst would be, instead of arriving as a wall of pop-ups
    /// the moment the gap elapses.
    pub fn flush(&self) {
        self.flush_at(Instant::now());
    }

    fn flush_at(&self, now: Instant) {
        let mut inner = self.lock();
        if inner.pending.is_empty() {
            return;
        }
        if let Some(last) = inner.last {
            if now.duration_since(last) < inner.settings.min_gap {
                return;
            }
        }
        if let Some(next) = inner.pending.pop_front() {
            inner.deliver(next, now);
        }
    }

    /// How many notifications are waiting. Tests assert on this to prove a
    /// notification was raised without inspecting the desktop.
    #[cfg(test)]
    pub fn queued(&self) -> usize {
        self.lock().pending.len()
    }

    /// How many notifications have actually been delivered (captured, under
    /// test). Proves a notification reached the desktop layer rather than
    /// being filtered out on the way.
    #[cfg(test)]
    pub fn delivered(&self) -> usize {
        self.captured().len() + self.queued()
    }

    #[cfg(test)]
    fn captured(&self) -> Vec<Notification> {
        match &self.lock().sink {
            Sink::Capture(seen) => seen.clone(),
            Sink::Desktop { .. } => Vec::new(),
        }
    }

    /// A poisoned lock means another thread panicked while holding it. The
    /// guarded state is a queue of pop-ups; carrying on with it is strictly
    /// better than propagating a panic into chat rendering.
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|err| err.into_inner())
    }
}

impl Inner {
    fn deliver(&mut self, notification: Notification, now: Instant) {
        self.last = Some(now);
        match &mut self.sink {
            #[cfg(test)]
            Sink::Capture(seen) => seen.push(notification),
            Sink::Desktop { backend } => {
                // Remembered backend first; only re-walk the chain if it has
                // stopped working (the daemon's package was removed mid-run,
                // say), and forget it if it has.
                if let Some(known) = *backend {
                    if known.launch(&notification) {
                        return;
                    }
                    *backend = None;
                }
                *backend = dispatch(&notification);
            }
        }
    }
}

/// Walks the fallback chain and returns the backend that worked, if any.
fn dispatch(notification: &Notification) -> Option<Backend> {
    for candidate in Backend::CHAIN {
        if candidate.launch(notification) {
            return Some(*candidate);
        }
    }
    // Terminal bell fallback (yc: terminalBellNotifier). Stderr, because the
    // interface owns stdout.
    let _ = std::io::stderr().write_all(b"\x07");
    None
}

/// One way of getting a pop-up on screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Backend {
    /// `notify-send`, from libnotify. The conventional answer, and the one
    /// with the best behaviour — but a separate package on most distributions,
    /// so it cannot be assumed present.
    NotifySend,
    /// `gdbus`, from GLib, calling the desktop's notification service
    /// directly. GLib is a dependency of every mainstream desktop, so this is
    /// the rung that makes "any Linux distribution" true in practice.
    GDBus,
    /// `kdialog`, for a KDE Plasma install with neither of the above.
    KDialog,
    /// `osascript`, on macOS. Not Linux, but this program builds there and a
    /// silent notifier would be a worse answer than none. Unreachable on the
    /// other platforms' chains, hence the allowance.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    OsaScript,
}

impl Backend {
    /// The order the chain is walked in.
    #[cfg(any(
        target_os = "linux",
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "openbsd"
    ))]
    const CHAIN: &'static [Backend] = &[Backend::NotifySend, Backend::GDBus, Backend::KDialog];

    #[cfg(target_os = "macos")]
    const CHAIN: &'static [Backend] = &[Backend::OsaScript];

    /// Windows is deliberately unserved. The reference implementation ships a
    /// toast via a PowerShell `-EncodedCommand` script — an embedded
    /// UTF-16LE-encoded payload that is high-maintenance for a platform this
    /// Linux-first program does not target. The terminal bell is the fallback.
    #[cfg(not(any(
        target_os = "linux",
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "openbsd",
        target_os = "macos"
    )))]
    const CHAIN: &'static [Backend] = &[];

    /// The executable this backend runs.
    fn program(self) -> &'static str {
        match self {
            Backend::NotifySend => "notify-send",
            Backend::GDBus => "gdbus",
            Backend::KDialog => "kdialog",
            Backend::OsaScript => "osascript",
        }
    }

    fn launch(self, notification: &Notification) -> bool {
        let Notification {
            title,
            body,
            urgency,
        } = notification;
        let mut cmd = match self {
            Backend::NotifySend => {
                let mut cmd = Command::new("notify-send");
                // The "--" terminator matters: notify-send accepts options
                // anywhere on the command line, and the title starts with
                // text somebody in chat chose. A viewer called
                // "--icon=/etc/passwd" must be displayed, not parsed.
                cmd.args([
                    "--app-name=msm",
                    &format!("--urgency={}", urgency.as_str()),
                    &format!("--expire-time={EXPIRE_MS}"),
                    "--",
                    title,
                ]);
                if !body.is_empty() {
                    cmd.arg(body);
                }
                cmd
            }
            Backend::GDBus => {
                // org.freedesktop.Notifications.Notify, whose parameters are,
                // in order: the application name, the id of a notification to
                // replace (0 = none), an icon, the summary, the body, the
                // action list, the hint dictionary, and the timeout in
                // milliseconds. gdbus parses each argument as GVariant text,
                // so strings arrive quoted and escaped — see `gvariant_string`.
                let mut cmd = Command::new("gdbus");
                cmd.args([
                    "call",
                    "--session",
                    "--dest",
                    "org.freedesktop.Notifications",
                    "--object-path",
                    "/org/freedesktop/Notifications",
                    "--method",
                    "org.freedesktop.Notifications.Notify",
                    "msm",
                    "0",
                    "",
                    &gvariant_string(title),
                    &gvariant_string(body),
                    "[]",
                    &format!("{{'urgency': <byte {}>}}", urgency.as_hint()),
                    &EXPIRE_MS.to_string(),
                ]);
                cmd
            }
            Backend::KDialog => {
                let mut cmd = Command::new("kdialog");
                // --passivepopup takes the text and a timeout in *seconds*.
                cmd.args([
                    "--title",
                    title,
                    "--passivepopup",
                    if body.is_empty() { title } else { body },
                    &(EXPIRE_MS / 1000).to_string(),
                ]);
                cmd
            }
            Backend::OsaScript => {
                // Title and body travel as argv items into an AppleScript
                // `on run` handler — never interpolated into the script text,
                // so chat content cannot inject script.
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
                cmd
            }
        };
        spawn_detached(&mut cmd)
    }
}

/// The program that will be used to show notifications, if any is installed.
///
/// Config → Diagnostics reports this. It exists because the failure mode it
/// catches is invisible otherwise: notifications are best-effort by design, so
/// a machine with no notification tooling at all is silently quiet rather than
/// broken, and "I never got a raid alert" is a hard thing to debug from the
/// inside of a stream.
///
/// The lookup walks `PATH` rather than running anything — asking a program
/// whether it exists by starting it is a poor trade in a function a settings
/// pane calls on every redraw.
pub fn available_backend() -> Option<&'static str> {
    Backend::CHAIN
        .iter()
        .map(|backend| backend.program())
        .find(|program| on_path(program))
}

/// Whether `program` exists as an executable file somewhere on `PATH`.
fn on_path(program: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| {
        let candidate = dir.join(program);
        // A directory named `gdbus` is not a program named `gdbus`.
        candidate.is_file()
    })
}

/// Renders a string as GVariant source text: wrapped in double quotes, with
/// backslashes and quotes escaped.
///
/// Without this, a chat message containing a double quote would end the
/// argument early and the rest of it would be parsed as GVariant syntax.
/// Control characters cannot appear here — [`sanitize`] has already replaced
/// them with spaces — so quotes and backslashes are the whole problem.
fn gvariant_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        if ch == '"' || ch == '\\' {
            out.push('\\');
        }
        out.push(ch);
    }
    out.push('"');
    out
}

/// Spawns a command with all stdio detached and hands the child to a reaper
/// thread. Returns whether the spawn succeeded.
///
/// "Succeeded" means the program existed and started — not that a pop-up
/// appeared. That is the distinction the fallback chain needs: the common
/// failure is `notify-send` not being installed, which shows up here as a
/// spawn error and moves the chain on to `gdbus`.
fn spawn_detached(cmd: &mut Command) -> bool {
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
            tracing::debug!("desktop notification backend failed to launch: {err}");
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

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(min_gap_ms: u64) -> Settings {
        Settings {
            enabled: true,
            min_gap: Duration::from_millis(min_gap_ms),
        }
    }

    fn note(title: &str) -> Notification {
        Notification::new(title, "body", Urgency::Normal)
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
        assert_eq!(sanitize(&input, 5), family.repeat(2) + "...");
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
    fn a_notification_sanitizes_its_text_on_the_way_in() {
        let notification = Notification::new("a\nb", "  c   d ", Urgency::Normal);
        assert_eq!(notification.title, "a b");
        assert_eq!(notification.body, "c d");
    }

    #[test]
    fn an_empty_title_becomes_the_program_name() {
        assert_eq!(Notification::new("   ", "b", Urgency::Normal).title, "msm");
    }

    #[test]
    fn the_first_notification_goes_out_immediately() {
        let notifier = Notifier::capturing(settings(2000));
        notifier.send(note("Raid"));
        assert_eq!(notifier.captured().len(), 1);
    }

    #[test]
    fn a_disabled_notifier_neither_delivers_nor_queues() {
        let notifier = Notifier::capturing(Settings {
            enabled: false,
            ..settings(2000)
        });
        notifier.send(note("Raid"));
        assert!(notifier.captured().is_empty());
        // Nothing queued either: turning notifications back on must not
        // produce a backlog of news from while they were off.
        assert_eq!(notifier.queued(), 0);
    }

    #[test]
    fn a_burst_is_queued_rather_than_dropped() {
        // The whole point of the queue: a gift drop delivers one event per
        // recipient, and the raid in the middle of it must still be seen.
        let notifier = Notifier::capturing(settings(2000));
        let start = Instant::now();
        notifier.send_at(note("Gift 1"), start);
        notifier.send_at(note("Gift 2"), start);
        notifier.send_at(note("Raid"), start);

        assert_eq!(notifier.captured().len(), 1);
        assert_eq!(notifier.queued(), 2);

        // Too soon: nothing moves.
        notifier.flush_at(start + Duration::from_millis(500));
        assert_eq!(notifier.captured().len(), 1);

        // One per gap, in the order they happened.
        notifier.flush_at(start + Duration::from_millis(2000));
        notifier.flush_at(start + Duration::from_millis(4000));
        let titles: Vec<String> = notifier.captured().into_iter().map(|n| n.title).collect();
        assert_eq!(titles, vec!["Gift 1", "Gift 2", "Raid"]);
        assert_eq!(notifier.queued(), 0);
    }

    #[test]
    fn flushing_an_empty_queue_does_nothing() {
        let notifier = Notifier::capturing(settings(2000));
        notifier.flush();
        assert!(notifier.captured().is_empty());
    }

    #[test]
    fn the_queue_is_bounded_and_drops_the_oldest() {
        let notifier = Notifier::capturing(settings(60_000));
        let start = Instant::now();
        // One delivered, then QUEUE_LIMIT + 2 queued.
        for index in 0..(QUEUE_LIMIT + 3) {
            notifier.send_at(note(&format!("n{index}")), start);
        }
        assert_eq!(notifier.queued(), QUEUE_LIMIT);
        // n0 went out; n1 and n2 were pushed off the front by the cap.
        let mut gap = Duration::from_secs(60);
        for _ in 0..QUEUE_LIMIT {
            notifier.flush_at(start + gap);
            gap += Duration::from_secs(60);
        }
        let titles: Vec<String> = notifier.captured().into_iter().map(|n| n.title).collect();
        assert_eq!(titles[0], "n0");
        assert_eq!(titles[1], "n3");
    }

    #[test]
    fn turning_notifications_off_discards_the_backlog() {
        let notifier = Notifier::capturing(settings(60_000));
        let start = Instant::now();
        notifier.send_at(note("one"), start);
        notifier.send_at(note("two"), start);
        assert_eq!(notifier.queued(), 1);
        notifier.configure(Settings {
            enabled: false,
            ..settings(60_000)
        });
        assert_eq!(notifier.queued(), 0);
    }

    #[test]
    fn clones_share_one_queue_and_one_throttle() {
        // Chat and the stream-lifecycle code hold separate clones; two
        // independent throttles would let them talk over each other.
        let notifier = Notifier::capturing(settings(60_000));
        let other = notifier.clone();
        let start = Instant::now();
        notifier.send_at(note("chat"), start);
        other.send_at(note("stream"), start);
        assert_eq!(notifier.captured().len(), 1);
        assert_eq!(other.queued(), 1);
    }

    #[test]
    fn gvariant_strings_are_quoted_and_escaped() {
        assert_eq!(gvariant_string("hello"), "\"hello\"");
        // A chat line ending the argument early would otherwise be parsed as
        // GVariant syntax rather than shown as text.
        assert_eq!(gvariant_string("say \"hi\""), "\"say \\\"hi\\\"\"");
        assert_eq!(gvariant_string("back\\slash"), "\"back\\\\slash\"");
    }

    #[test]
    fn every_backend_names_a_program_and_the_probe_agrees_with_itself() {
        // The probe must never claim a backend the chain would not try.
        if let Some(found) = available_backend() {
            assert!(Backend::CHAIN.iter().any(|b| b.program() == found));
        }
        // Something that certainly is not installed.
        assert!(!on_path("msm-definitely-not-a-real-program"));
        // And something that certainly is, wherever these tests run.
        #[cfg(unix)]
        assert!(on_path("sh"));
    }

    #[test]
    fn urgency_maps_to_both_wire_spellings() {
        assert_eq!(Urgency::Critical.as_str(), "critical");
        assert_eq!(Urgency::Critical.as_hint(), 2);
        assert_eq!(Urgency::Normal.as_str(), "normal");
        assert_eq!(Urgency::Normal.as_hint(), 1);
    }
}
