//! Notifications: transient pop-ups, and the modal history behind them.
//!
//! The model is vim's, because vim solved this problem a long time ago and
//! most people who live in a terminal already know the shape of it.
//!
//! In vim, something happens and a short message appears at the bottom of the
//! screen. It does not interrupt you, it does not need dismissing, and it goes
//! away on its own. But it is not *lost* either: `:messages` brings back every
//! message of the session, in order, so a notice that flashed past while you
//! were looking elsewhere can still be read afterwards.
//!
//! That is exactly what this does, and it is why "modal" is the right word.
//! There are two modes of looking at a notification:
//!
//! * **transient** — up to a few pop-ups stacked at the bottom right, each
//!   expiring on its own timer. Nothing is blocked while they are up, and
//!   nothing needs a keypress to get rid of them.
//! * **modal** — `alt+m` opens the message history, which takes over the
//!   screen and every key until you press `esc`. Everything the session has
//!   raised is there, oldest first, scrollable.
//!
//! A stream is exactly the situation this suits. Things happen while you are
//! reading chat or talking to a camera — a token refreshed, a send was refused,
//! a platform dropped its connection — and each of those is worth a glance but
//! not worth a dialog box with an OK button on it. And when something later
//! goes wrong, the history is the record of what led up to it.

use ratatui::prelude::*;
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap};
use std::collections::VecDeque;
use std::time::{Duration, Instant};

use crate::theme;

/// How serious a notification is, which decides its colour and its glyph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Info,
    Success,
    Warning,
    Error,
}

impl Level {
    /// The glyph in front of the message.
    ///
    /// A glyph as well as a colour, because colour alone is not a signal
    /// everyone can read: roughly one man in twelve cannot reliably tell the
    /// warning colour from the success one.
    pub fn glyph(self) -> &'static str {
        match self {
            Level::Info => "•",
            Level::Success => "✔",
            Level::Warning => "▲",
            Level::Error => "✖",
        }
    }

    fn color(self) -> Color {
        let sk = theme::skin();
        match self {
            Level::Info => sk.accent,
            Level::Success => sk.success,
            Level::Warning => sk.warning,
            Level::Error => sk.error,
        }
    }

    /// How long a message of this level stays up, given the configured base
    /// duration.
    ///
    /// An error stays up three times as long as a notice. The reason is
    /// simply that it is likelier to be the thing you need to act on, and
    /// re-reading it should not require opening the history.
    fn lifetime(self, base: Duration) -> Duration {
        match self {
            Level::Error => base * 3,
            Level::Warning => base * 2,
            _ => base,
        }
    }
}

/// One notification.
#[derive(Debug, Clone)]
pub struct Toast {
    pub text: String,
    pub level: Level,
    /// When it was raised. Used both for expiry and for the timestamp in the
    /// history.
    pub at: chrono::DateTime<chrono::Local>,
    /// The monotonic instant it was raised at.
    ///
    /// Separate from `at` on purpose: a wall clock can jump backwards (a time
    /// zone change, an NTP correction), and a notification that outlives the
    /// session because the clock moved would be a silly way to lose a screen.
    born: Instant,
    lifetime: Duration,
}

impl Toast {
    /// How far through its life this notification is, from 0.0 to 1.0.
    fn age(&self, now: Instant) -> f64 {
        let elapsed = now.saturating_duration_since(self.born);
        (elapsed.as_secs_f64() / self.lifetime.as_secs_f64()).clamp(0.0, 1.0)
    }

    fn expired(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.born) >= self.lifetime
    }
}

/// The most pop-ups shown at once.
///
/// Four is enough to see a burst arriving and few enough that they cannot
/// swallow the interface. Anything beyond that pushes the oldest out early —
/// it is still in the history, which is the point of having one.
const MAX_VISIBLE: usize = 4;

/// How many messages the history keeps.
const HISTORY_LIMIT: usize = 500;

/// The last fraction of a notification's life, over which it fades out.
///
/// A pop-up that vanishes between one frame and the next reads as a glitch;
/// one that dims first reads as finishing.
const FADE_SHARE: f64 = 0.25;

/// Everything about notifications: what is on screen, what has been said, and
/// whether the history is open.
#[derive(Debug, Default)]
pub struct Toasts {
    /// The pop-ups currently on screen, oldest first.
    active: VecDeque<Toast>,
    /// Every notification of the session, oldest first.
    history: VecDeque<Toast>,
    /// Whether the modal history is open.
    pub history_open: bool,
    /// How many lines the history is scrolled back from its newest.
    pub history_scroll: usize,
}

impl Toasts {
    /// Raise a notification.
    pub fn push(&mut self, level: Level, text: impl Into<String>, base_lifetime: Duration) {
        let toast = Toast {
            text: text.into(),
            level,
            at: chrono::Local::now(),
            born: Instant::now(),
            lifetime: level.lifetime(base_lifetime),
        };
        self.history.push_back(toast.clone());
        while self.history.len() > HISTORY_LIMIT {
            self.history.pop_front();
        }
        self.active.push_back(toast);
        while self.active.len() > MAX_VISIBLE {
            self.active.pop_front();
        }
    }

    /// Drop the pop-ups whose time is up. Their history entries stay.
    pub fn expire(&mut self, now: Instant) {
        self.active.retain(|toast| !toast.expired(now));
    }

    /// Take every pop-up off the screen at once, without touching the
    /// history. This is what `esc` does.
    pub fn dismiss_all(&mut self) {
        self.active.clear();
    }

    /// Whether any pop-up is on screen.
    pub fn showing(&self) -> bool {
        !self.active.is_empty()
    }

    /// The text of every pop-up currently on screen.
    ///
    /// Used by the tests, which check what a keypress raised; production
    /// draws the pop-ups rather than reading their text back out.
    #[cfg(test)]
    pub fn visible_text(&self) -> Vec<&str> {
        self.active
            .iter()
            .map(|toast| toast.text.as_str())
            .collect()
    }

    /// Everything raised this session, oldest first.
    pub fn history(&self) -> &VecDeque<Toast> {
        &self.history
    }

    /// Open the modal history, at its newest entry.
    pub fn open_history(&mut self) {
        self.history_open = true;
        self.history_scroll = 0;
    }

    pub fn close_history(&mut self) {
        self.history_open = false;
        self.history_scroll = 0;
    }

    /// Scroll the history back (positive) or forward (negative), stopping at
    /// both ends rather than wrapping — wrapping a log would make it
    /// impossible to tell the oldest entry from the newest.
    pub fn scroll_history(&mut self, delta: isize) {
        let furthest = self.history.len().saturating_sub(1);
        self.history_scroll =
            (self.history_scroll as isize + delta).clamp(0, furthest as isize) as usize;
    }
}

/// Draw the stack of pop-ups in the bottom-right corner of `area`.
///
/// Bottom right because that is the emptiest part of this interface — the log
/// occupies the bottom left, the panes the top — and because it is where a
/// notification is conventionally expected, so the eye goes there without
/// being taught to.
pub fn draw(frame: &mut Frame, area: Rect, toasts: &Toasts, now: Instant) {
    let sk = theme::skin();
    if toasts.active.is_empty() || area.width < 12 || area.height < 3 {
        return;
    }

    // Wide enough to read a sentence, never more than half the screen.
    let width = (area.width / 2).clamp(24, 52);

    // Newest at the bottom, so a burst reads downwards the way a chat does.
    let mut bottom = area.y + area.height;
    for toast in toasts.active.iter().rev() {
        let text = format!("{} {}", toast.level.glyph(), toast.text);
        // Two cells of padding either side, plus the box.
        let inner_width = width.saturating_sub(4).max(1);
        let rows = text.chars().count().div_ceil(inner_width as usize).max(1) as u16;
        let height = rows + 2;
        if bottom < area.y + height {
            break;
        }
        bottom -= height;

        let rect = Rect {
            x: area.x + area.width.saturating_sub(width),
            y: bottom,
            width,
            height,
        };

        // Fade the last quarter of its life by blending its colour toward the
        // canvas, so it dims out rather than blinking off.
        let age = toast.age(now);
        let fade = if age > 1.0 - FADE_SHARE {
            (1.0 - age) / FADE_SHARE
        } else {
            1.0
        };
        let color = blend(sk.canvas, toast.level.color(), fade);

        frame.render_widget(Clear, rect);
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(text, Style::new().fg(color))))
                .wrap(Wrap { trim: true })
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_type(BorderType::Rounded)
                        .border_style(Style::new().fg(color))
                        .style(Style::new().bg(sk.surface)),
                ),
            rect,
        );
    }
}

/// Draw the modal message history over the whole area.
pub fn draw_history(frame: &mut Frame, area: Rect, toasts: &Toasts) {
    let sk = theme::skin();
    frame.render_widget(Clear, area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(sk.accent))
        .style(Style::new().bg(sk.canvas))
        .title(" Messages ");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let rows = Layout::vertical([Constraint::Min(0), Constraint::Length(1)])
        .horizontal_margin(1)
        .split(inner);

    let height = rows[0].height as usize;
    let history = toasts.history();

    if history.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "Nothing has happened yet this session.",
                Style::new().fg(sk.muted),
            ))),
            rows[0],
        );
    } else {
        // Show a window ending `history_scroll` lines back from the newest,
        // so scrolling walks backwards through the session.
        let end = history.len().saturating_sub(toasts.history_scroll);
        let start = end.saturating_sub(height);
        let lines: Vec<Line> = history
            .iter()
            .take(end)
            .skip(start)
            .map(|toast| {
                Line::from(vec![
                    Span::styled(
                        toast.at.format("%H:%M:%S ").to_string(),
                        Style::new().fg(sk.muted),
                    ),
                    Span::styled(
                        format!("{} ", toast.level.glyph()),
                        Style::new().fg(toast.level.color()),
                    ),
                    Span::styled(toast.text.clone(), Style::new().fg(sk.foreground)),
                ])
            })
            .collect();
        frame.render_widget(Paragraph::new(lines), rows[0]);
    }

    let hint = if toasts.history_scroll > 0 {
        format!(
            "j/k or ↑/↓ scroll   g newest   esc close   ({} back)",
            toasts.history_scroll
        )
    } else {
        "j/k or ↑/↓ scroll   esc close".to_string()
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(hint, Style::new().fg(sk.muted)))),
        rows[1],
    );
}

/// Blend `toward` into `base` by `amount`, where 1.0 is `toward` unchanged.
///
/// A colour the terminal defines rather than one with known channel values
/// cannot be blended, so it is returned as-is: a notification that does not
/// fade is much better than one that does not appear.
fn blend(base: Color, toward: Color, amount: f64) -> Color {
    let (Color::Rgb(br, bg, bb), Color::Rgb(tr, tg, tb)) = (base, toward) else {
        return toward;
    };
    let amount = amount.clamp(0.0, 1.0);
    let mix = |from: u8, to: u8| (from as f64 + (to as f64 - from as f64) * amount).round() as u8;
    Color::Rgb(mix(br, tr), mix(bg, tg), mix(bb, tb))
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: Duration = Duration::from_secs(5);

    fn toasts() -> Toasts {
        Toasts::default()
    }

    #[test]
    fn a_notification_appears_and_is_remembered() {
        let mut toasts = toasts();
        toasts.push(Level::Info, "hello", BASE);
        assert!(toasts.showing());
        assert_eq!(toasts.history().len(), 1);
    }

    /// The whole point of the history: a pop-up going away must not take the
    /// message with it.
    #[test]
    fn expiring_clears_the_popup_but_keeps_the_history() {
        let mut toasts = toasts();
        toasts.push(Level::Info, "hello", Duration::from_millis(1));
        std::thread::sleep(Duration::from_millis(5));
        toasts.expire(Instant::now());
        assert!(!toasts.showing());
        assert_eq!(toasts.history().len(), 1);
    }

    #[test]
    fn dismissing_clears_the_screen_but_keeps_the_history() {
        let mut toasts = toasts();
        toasts.push(Level::Error, "broken", BASE);
        toasts.dismiss_all();
        assert!(!toasts.showing());
        assert_eq!(toasts.history().len(), 1);
    }

    /// A burst of notifications must not be able to take over the screen.
    #[test]
    fn only_a_few_popups_are_shown_at_once_but_all_are_recorded() {
        let mut toasts = toasts();
        for index in 0..20 {
            toasts.push(Level::Info, format!("message {index}"), BASE);
        }
        assert_eq!(toasts.active.len(), MAX_VISIBLE);
        assert_eq!(toasts.history().len(), 20);
        // The ones still showing are the newest, not the first four.
        assert!(toasts.active.back().is_some_and(|t| t.text.ends_with("19")));
    }

    #[test]
    fn the_history_is_bounded_so_a_long_session_cannot_grow_without_limit() {
        let mut toasts = toasts();
        for index in 0..(HISTORY_LIMIT + 50) {
            toasts.push(Level::Info, format!("message {index}"), BASE);
        }
        assert_eq!(toasts.history().len(), HISTORY_LIMIT);
        // The oldest were dropped, not the newest.
        assert!(toasts
            .history()
            .back()
            .is_some_and(|t| t.text.ends_with(&(HISTORY_LIMIT + 49).to_string())));
    }

    /// An error is likelier to be the thing you have to act on, so it stays
    /// up longer than a passing notice.
    #[test]
    fn a_more_serious_message_stays_up_longer() {
        assert!(Level::Error.lifetime(BASE) > Level::Warning.lifetime(BASE));
        assert!(Level::Warning.lifetime(BASE) > Level::Info.lifetime(BASE));
        assert_eq!(Level::Success.lifetime(BASE), Level::Info.lifetime(BASE));
    }

    /// Scrolling a log that wraps around would make the oldest entry
    /// indistinguishable from the newest, so both ends are hard stops.
    #[test]
    fn scrolling_the_history_stops_at_both_ends() {
        let mut toasts = toasts();
        for index in 0..5 {
            toasts.push(Level::Info, format!("message {index}"), BASE);
        }
        toasts.scroll_history(-10);
        assert_eq!(toasts.history_scroll, 0, "cannot scroll past the newest");
        toasts.scroll_history(100);
        assert_eq!(toasts.history_scroll, 4, "cannot scroll past the oldest");
    }

    #[test]
    fn scrolling_an_empty_history_does_not_panic() {
        let mut toasts = toasts();
        toasts.scroll_history(5);
        toasts.scroll_history(-5);
        assert_eq!(toasts.history_scroll, 0);
    }

    #[test]
    fn opening_the_history_starts_at_the_newest_entry() {
        let mut toasts = toasts();
        toasts.push(Level::Info, "hello", BASE);
        toasts.scroll_history(3);
        toasts.close_history();
        toasts.open_history();
        assert!(toasts.history_open);
        assert_eq!(toasts.history_scroll, 0);
    }

    /// Every level must be distinguishable without relying on colour, since
    /// colour alone is not a signal everyone can read.
    #[test]
    fn every_level_has_its_own_glyph() {
        let glyphs: Vec<&str> = [Level::Info, Level::Success, Level::Warning, Level::Error]
            .iter()
            .map(|level| level.glyph())
            .collect();
        let mut unique = glyphs.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), glyphs.len(), "two levels share a glyph");
    }

    #[test]
    fn a_notification_fades_rather_than_vanishing() {
        let base = Color::Rgb(0, 0, 0);
        let toward = Color::Rgb(100, 100, 100);
        assert_eq!(blend(base, toward, 1.0), toward);
        assert_eq!(blend(base, toward, 0.0), base);
        assert_eq!(blend(base, toward, 0.5), Color::Rgb(50, 50, 50));
        // A terminal-defined colour cannot be blended, so it comes back whole.
        assert_eq!(blend(Color::Reset, toward, 0.5), toward);
    }
}
