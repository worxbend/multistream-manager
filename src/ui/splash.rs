//! The start-up splash.
//!
//! For the few seconds before the interface is ready, this covers the screen
//! with a logo, a tagline that types itself in, a progress bar, and a tiny
//! scripted chat.
//!
//! The chat is the part that needs explaining. A progress bar tells you
//! something is happening; it does not tell you what the thing you are
//! starting *does*. A handful of characters talking to each other does both,
//! and it does it using the same per-author colouring and the same layout
//! that the real chat panes use — so the splash is a preview rather than
//! unrelated decoration.
//!
//! Everything here is a pure function of how long the splash has been up. No
//! extra state, no second clock, and every frame reproducible in a test.
//!
//! Any keypress skips it. A few seconds is not long, but a wait nobody knows
//! is skippable is just a wait.

use ratatui::prelude::*;
use ratatui::widgets::{Clear, Paragraph};
use std::time::Duration;

use crate::anim::{self, Effect, Mode};
use crate::theme;

/// How long the splash stays up when it is not skipped.
///
/// Long enough to read the tagline and watch a few chat lines land, short
/// enough that someone opening the program for the hundredth time that week
/// is not made to sit through a title sequence.
pub const DURATION: Duration = Duration::from_millis(2600);

/// The block-capital wordmark, for terminals with room for it.
const LOGO: [&str; 5] = [
    "███╗   ███╗███████╗███╗   ███╗",
    "████╗ ████║██╔════╝████╗ ████║",
    "██╔████╔██║███████╗██╔████╔██║",
    "██║╚██╔╝██║╚════██║██║╚██╔╝██║",
    "██║ ╚═╝ ██║███████║██║ ╚═╝ ██║",
];

/// The one-line stand-in for a terminal too small for the wordmark.
const SMALL_LOGO: &str = "╭── ✦ msm ✦ ──╮";

/// The narrowest terminal, and the shortest, that gets the full wordmark.
const LOGO_MIN_WIDTH: u16 = 34;
const LOGO_MIN_HEIGHT: u16 = 14;

const TAGLINE: &str = "msm // one stream, two platforms";
const STRAPLINE: &str = "✦  set it once, go live everywhere  ✦";

/// How long the tagline waits before it starts typing, so the logo lands
/// first and registers on its own.
const TAGLINE_DELAY: Duration = Duration::from_millis(240);

/// How wide the content column is, however wide the terminal gets. Text
/// centred across a very wide terminal is unreadable — the eye loses the
/// start of the next line — so the column stops growing.
const MAX_CONTENT_WIDTH: u16 = 56;

/// The progress bar's width.
const PROGRESS_WIDTH: usize = 28;

/// Whether the splash should still be covering the interface.
pub fn is_showing(elapsed: Duration, skipped: bool, enabled: bool) -> bool {
    enabled && !skipped && elapsed < DURATION
}

/// How far through the splash is, from 0.0 to 1.0.
fn fraction(elapsed: Duration) -> f64 {
    (elapsed.as_secs_f64() / DURATION.as_secs_f64()).clamp(0.0, 1.0)
}

/// One participant in the start-up chat.
struct Mascot {
    name: &'static str,
    face: &'static str,
}

/// The cast.
///
/// Kaomoji rather than emoji, deliberately: these render at a predictable
/// width in every terminal, whereas an emoji is drawn at one cell by some
/// terminals and two by others, which would break the alignment of every row
/// after it.
const MASCOTS: [Mascot; 5] = [
    Mascot {
        name: "bitbuddy",
        face: "(•‿•)",
    },
    Mascot {
        name: "pixelcat",
        face: "(=^･ω･^=)",
    },
    Mascot {
        name: "streamgoose",
        face: "(°□°)",
    },
    Mascot {
        name: "lurkbot",
        face: "(¬‿¬)",
    },
    Mascot {
        name: "modmoth",
        face: "ʕ•ᴥ•ʔ",
    },
];

/// The script, as `(who, what)`.
///
/// It reads as two chats warming up at once, which is what this program is
/// for. Short enough to finish inside the splash, and generic enough not to
/// imply anything about anybody's actual channel.
const SCRIPT: [(usize, &str); 6] = [
    (0, "hey chat, we're live on both"),
    (1, "nyaa~ twitch side is up"),
    (2, "youtube chat reading you fine"),
    (3, "just lurking, as is tradition"),
    (4, "keeping it civil in here"),
    (0, "one title, two platforms, no tabs"),
];

/// When the chat starts and finishes, as a fraction of the splash. The logo
/// lands before it starts, and the last line is readable before the interface
/// replaces it.
const CHAT_START: f64 = 0.16;
const CHAT_END: f64 = 0.94;
/// How much of each line's slot is spent typing it in. The rest holds it
/// still, which is what makes the sequence readable rather than a blur.
const CHAT_TYPE_SHARE: f64 = 0.55;
/// The most vertical room the chat may claim.
const CHAT_MAX_ROWS: usize = 5;

/// Draw the splash over the whole area.
pub fn draw(frame: &mut Frame, area: Rect, elapsed: Duration, mode: Mode) {
    let sk = theme::skin();
    frame.render_widget(Clear, area);

    let content_width = area.width.saturating_sub(2).min(MAX_CONTENT_WIDTH);
    if content_width == 0 || area.height == 0 {
        return;
    }

    let logo: Vec<&str> = if area.width < LOGO_MIN_WIDTH || area.height < LOGO_MIN_HEIGHT {
        vec![SMALL_LOGO]
    } else {
        LOGO.to_vec()
    };

    let mut lines: Vec<Line<'static>> = Vec::new();

    if logo.len() == 1 {
        // The one-line logo has room to move, so it drifts back and forth
        // across the content column leaving a fading trail behind it. The
        // tall wordmark gets a colour wave instead: sliding five stacked rows
        // around would be seasickness rather than decoration.
        let config = anim::Config::new(
            Effect::Bounce,
            mode,
            &palette_accent(),
            &palette_foreground(),
        )
        .bold(true)
        .trail(&palette_muted())
        .width(content_width as usize);
        lines.push(Line::from(spans(&anim::frame(logo[0], &config, elapsed))));
    } else {
        // Each logo row is offset a little further along the same colour
        // ramp, so the wave runs diagonally down the wordmark rather than
        // every row flashing in unison.
        for (row, art) in logo.iter().enumerate() {
            let config = anim::Config::new(
                Effect::GradientWave,
                mode,
                &palette_accent(),
                &palette_foreground(),
            )
            .bold(true)
            .offset(row * 2);
            lines.push(Line::from(spans(&anim::frame(art, &config, elapsed))));
        }
    }

    // The tagline types itself in; the strapline gets a shimmer instead. Two
    // reveals running at once would compete, but a reveal and a sweep read as
    // one thing happening.
    let tagline = anim::Config::new(
        Effect::Typewriter,
        mode,
        &palette_foreground(),
        &palette_accent(),
    )
    .bold(true)
    .step(tagline_step(mode));
    lines.push(Line::from(spans(&anim::frame(
        &fit(TAGLINE, content_width),
        &tagline,
        elapsed.saturating_sub(TAGLINE_DELAY),
    ))));

    let strapline = anim::Config::new(Effect::Shimmer, mode, &palette_muted(), &palette_accent());
    lines.push(Line::from(spans(&anim::frame(
        &fit(STRAPLINE, content_width),
        &strapline,
        elapsed,
    ))));

    // Whatever vertical room is left after everything else has been placed
    // goes to the chat, so it never pushes the logo or the bar off a short
    // terminal.
    let chat = chat_lines(elapsed, content_width, chat_budget(area.height, logo.len()));
    if !chat.is_empty() {
        lines.push(Line::from(""));
        lines.extend(chat);
    }

    lines.push(Line::from(""));

    let bar = progress_bar(
        fraction(elapsed),
        PROGRESS_WIDTH.min(content_width as usize),
    );
    let bar_config = anim::Config::new(
        Effect::GradientWave,
        mode,
        &palette_accent(),
        &palette_foreground(),
    );
    lines.push(Line::from(spans(&anim::frame(&bar, &bar_config, elapsed))));

    // The skip hint waits until the tagline has finished typing. Adding a
    // second thing to read to a line that is still arriving competes with it;
    // once the tagline has settled there is room for both.
    let mut label = phase_label(fraction(elapsed)).to_string();
    if anim::is_done(
        &fit(TAGLINE, content_width),
        &tagline,
        elapsed.saturating_sub(TAGLINE_DELAY),
    ) {
        label.push_str("   ·   press any key to skip");
    }
    // The leading glyph pulses between muted and accent, so the line reads as
    // something in progress rather than a caption that happens to be there.
    let glyph_config = anim::Config::new(Effect::Pulse, mode, &palette_muted(), &palette_accent());
    let (glyph, rest) = split_first_glyph(&fit(&label, content_width));
    let mut label_spans = spans(&anim::frame(&glyph, &glyph_config, elapsed));
    label_spans.push(Span::styled(rest, Style::new().fg(sk.muted)));
    lines.push(Line::from(label_spans));

    // Trim from the bottom up when the terminal is too short: the wordmark
    // and the "what is happening" label matter more than the decoration
    // between them.
    let lines = fit_height(lines, area.height as usize);

    frame.render_widget(
        Paragraph::new(lines)
            .style(Style::new().bg(sk.canvas))
            .alignment(Alignment::Center),
        area,
    );
}

/// The tagline's typing speed.
///
/// Set explicitly rather than taken from the effect's default, because the
/// tagline has to finish inside the splash in every animation mode — a
/// reduced-motion default would still be typing when the splash lifts.
fn tagline_step(mode: Mode) -> Duration {
    match mode {
        Mode::Reduced => Duration::from_millis(90),
        _ => Duration::from_millis(45),
    }
}

/// How many rows the chat can have once everything else has taken its own.
fn chat_budget(height: u16, logo_rows: usize) -> usize {
    // logo + tagline + strapline + blank + blank + bar + label
    let fixed = logo_rows + 6;
    let spare = (height as usize).saturating_sub(fixed);
    spare.saturating_sub(1).min(CHAT_MAX_ROWS)
}

/// The chat rows for this moment: the lines that have already been said, the
/// newest one part-way through being typed.
fn chat_lines(elapsed: Duration, width: u16, budget: usize) -> Vec<Line<'static>> {
    if budget == 0 {
        return Vec::new();
    }
    let sk = theme::skin();
    let progress = fraction(elapsed);
    if progress < CHAT_START {
        return Vec::new();
    }

    // Where the script has got to, as a fraction of its own span.
    let span = ((progress - CHAT_START) / (CHAT_END - CHAT_START)).clamp(0.0, 1.0);
    let slot = 1.0 / SCRIPT.len() as f64;
    let current = ((span / slot).floor() as usize).min(SCRIPT.len() - 1);
    let within = ((span - current as f64 * slot) / slot).clamp(0.0, 1.0);

    let first = (current + 1).saturating_sub(budget);
    SCRIPT[first..=current]
        .iter()
        .enumerate()
        .map(|(offset, (mascot, text))| {
            let index = first + offset;
            let mascot = &MASCOTS[*mascot];
            // Every line but the newest is complete; the newest is typing.
            let shown = if index == current {
                let revealed = (within / CHAT_TYPE_SHARE).clamp(0.0, 1.0);
                let count = (text.chars().count() as f64 * revealed).round() as usize;
                text.chars().take(count).collect::<String>()
            } else {
                (*text).to_string()
            };
            // Pad the message out to its finished length. Each line is
            // centred on its own width, so without this a line being typed
            // would creep sideways as it grew — which is exactly the sort of
            // motion that makes text hard to read.
            let padding = text.chars().count().saturating_sub(shown.chars().count());
            let name = format!("{} {}", mascot.face, mascot.name);
            let room = width.saturating_sub(name.chars().count() as u16 + 2);
            Line::from(vec![
                Span::styled(
                    name.clone(),
                    Style::new()
                        .fg(mascot_color(mascot.name))
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(": ", Style::new().fg(sk.muted)),
                Span::styled(
                    fit(&format!("{shown}{}", " ".repeat(padding)), room),
                    Style::new().fg(sk.foreground),
                ),
            ])
        })
        .collect()
}

/// A mascot's name colour, hashed from the name exactly as a real chatter's
/// is, and contrast-corrected against the canvas the same way — so the splash
/// chat is coloured by the code the real chat panes use rather than by a
/// second, prettier rule.
fn mascot_color(name: &str) -> Color {
    let sk = theme::skin();
    crate::chat::render::identity_color(name, &[sk.canvas], sk.foreground)
}

fn progress_bar(fraction: f64, width: usize) -> String {
    if width <= 2 {
        return "◆".to_string();
    }
    let inner = width - 2;
    let filled = (fraction.clamp(0.0, 1.0) * inner as f64) as usize;
    if filled >= inner {
        format!("[{}]", "━".repeat(inner))
    } else {
        format!(
            "[{}◆{}]",
            "━".repeat(filled),
            "·".repeat(inner - filled - 1)
        )
    }
}

/// What the splash claims to be doing. These are real stages of start-up, in
/// the order they actually happen, rather than invented progress.
fn phase_label(fraction: f64) -> &'static str {
    match fraction {
        f if f < 0.22 => "◌ loading palette",
        f if f < 0.48 => "◐ reading saved logins",
        f if f < 0.76 => "◓ composing surfaces",
        _ => "● ready",
    }
}

/// Split the first grapheme cluster off a label, so it can be styled apart
/// from the words after it.
fn split_first_glyph(text: &str) -> (String, String) {
    let mut graphemes = unicode_segmentation::UnicodeSegmentation::graphemes(text, true);
    match graphemes.next() {
        Some(first) => (first.to_string(), graphemes.collect()),
        None => (String::new(), String::new()),
    }
}

fn palette_accent() -> String {
    hex(theme::skin().accent)
}

fn palette_foreground() -> String {
    hex(theme::skin().foreground)
}

fn palette_muted() -> String {
    hex(theme::skin().muted)
}

/// Turn a drawable colour back into the `#rrggbb` text the animation code
/// blends with. A colour the terminal itself defines has no channel values to
/// blend, so it falls back to a neutral grey rather than failing.
fn hex(color: Color) -> String {
    match color {
        Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
        _ => "#808080".to_string(),
    }
}

/// Turn animated cells into drawable spans.
fn spans(cells: &[anim::Cell]) -> Vec<Span<'static>> {
    cells
        .iter()
        .map(|cell| {
            let mut style = Style::new().fg(theme::color(&cell.color));
            if cell.bold {
                style = style.add_modifier(Modifier::BOLD);
            }
            Span::styled(cell.text.clone(), style)
        })
        .collect()
}

/// Cut text to `width` terminal cells without splitting a glyph.
fn fit(text: &str, width: u16) -> String {
    let width = width as usize;
    let mut out = String::new();
    let mut used = 0;
    for cluster in unicode_segmentation::UnicodeSegmentation::graphemes(text, true) {
        let cell_width = unicode_width::UnicodeWidthStr::width(cluster).max(1);
        if used + cell_width > width {
            break;
        }
        out.push_str(cluster);
        used += cell_width;
    }
    out
}

/// Drop lines from the bottom until the block fits the terminal's height.
fn fit_height(mut lines: Vec<Line<'static>>, height: usize) -> Vec<Line<'static>> {
    if height == 0 {
        return Vec::new();
    }
    while lines.len() > height {
        // The last line is the phase label — what is happening — which is
        // the single most useful line here, so it is kept and the one above
        // it is dropped instead.
        let victim = lines.len().saturating_sub(2);
        lines.remove(victim);
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn render(width: u16, height: u16, elapsed: Duration, mode: Mode) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height))
            .expect("the test backend never fails to initialise");
        terminal
            .draw(|frame| draw(frame, frame.area(), elapsed, mode))
            .expect("drawing must not fail");
        let buffer = terminal.backend().buffer().clone();
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn the_splash_hides_itself_once_its_time_is_up() {
        assert!(is_showing(Duration::ZERO, false, true));
        assert!(!is_showing(DURATION, false, true));
        assert!(!is_showing(Duration::ZERO, true, true), "a skip must win");
        assert!(
            !is_showing(Duration::ZERO, false, false),
            "turning it off must win"
        );
    }

    /// A start-up screen that crashes the program it is starting would be a
    /// remarkable own goal, so every size and moment has to be safe.
    #[test]
    fn drawing_never_panics_at_any_size_or_moment() {
        for (width, height) in [(1, 1), (2, 3), (20, 6), (40, 12), (80, 24), (200, 60)] {
            for step in 0..=10 {
                let elapsed = DURATION.mul_f64(step as f64 / 10.0);
                for mode in [Mode::Off, Mode::Reduced, Mode::Fast] {
                    render(width, height, elapsed, mode);
                }
            }
        }
    }

    #[test]
    fn a_wide_terminal_gets_the_wordmark_and_a_narrow_one_gets_the_small_logo() {
        let wide = render(80, 24, Duration::from_millis(100), Mode::Off);
        assert!(wide.contains("███"), "the wordmark should be showing");

        let narrow = render(24, 8, Duration::from_millis(100), Mode::Off);
        assert!(
            narrow.contains("msm"),
            "a narrow terminal still has to say what this is"
        );
        assert!(!narrow.contains("███"));
    }

    /// With animation off the splash still shows everything — it just does
    /// not move. Nothing may be hidden by the setting.
    #[test]
    fn animation_off_still_shows_the_finished_tagline() {
        let frame = render(80, 24, Duration::ZERO, Mode::Off);
        assert!(frame.contains(TAGLINE), "the tagline must be fully drawn");
    }

    /// With animation on, the tagline arrives over time rather than at once.
    #[test]
    fn the_tagline_types_itself_in() {
        let early = render(80, 24, TAGLINE_DELAY, Mode::Fast);
        assert!(!early.contains(TAGLINE));
        let late = render(80, 24, DURATION, Mode::Fast);
        assert!(late.contains(TAGLINE));
    }

    #[test]
    fn the_progress_bar_fills_up_as_time_passes() {
        assert!(progress_bar(0.0, 10).starts_with("[◆"));
        assert_eq!(progress_bar(1.0, 10), "[━━━━━━━━]");
        // Never wider than asked for, whatever it is showing.
        for width in 0..40 {
            for step in 0..=10 {
                let bar = progress_bar(step as f64 / 10.0, width);
                assert!(
                    bar.chars().count() <= width.max(1),
                    "a {width}-wide bar rendered {} cells",
                    bar.chars().count()
                );
            }
        }
    }

    /// The chat is the first thing to go on a short terminal, and the last
    /// thing to appear on a tall one.
    #[test]
    fn the_chat_only_appears_when_there_is_room_for_it() {
        assert_eq!(chat_budget(8, 5), 0);
        assert!(chat_budget(24, 5) > 0);
        assert!(chat_budget(24, 5) <= CHAT_MAX_ROWS);
    }

    #[test]
    fn the_chat_works_through_its_script_in_order() {
        let at = |f: f64| {
            let lines = chat_lines(DURATION.mul_f64(f), 60, CHAT_MAX_ROWS);
            lines
                .iter()
                .map(|line| {
                    line.spans
                        .iter()
                        .map(|span| span.content.as_ref())
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
        };
        // Nothing before the chat's own start.
        assert!(at(0.0).is_empty());
        // The first speaker leads.
        let early = at(CHAT_START + 0.05);
        assert!(early[0].contains("bitbuddy"), "got {early:?}");
        // By the end the last line of the script is on screen.
        let late = at(0.99);
        assert!(
            late.last().is_some_and(|line| line.contains("no tabs")),
            "got {late:?}"
        );
    }

    /// A line must never lose characters as time moves forward — the effect
    /// is a reveal, not a shuffle.
    #[test]
    fn a_chat_line_only_ever_gains_characters_while_it_is_being_typed() {
        let mut longest = 0;
        for step in 0..=40 {
            let progress = CHAT_START + (CHAT_END - CHAT_START) * (step as f64 / 40.0) * 0.16;
            let lines = chat_lines(DURATION.mul_f64(progress), 60, CHAT_MAX_ROWS);
            let Some(line) = lines.last() else { continue };
            let text: String = line
                .spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect();
            let width = text.trim_end().chars().count();
            assert!(
                width >= longest,
                "the line shrank from {longest} to {width} cells"
            );
            longest = width;
        }
    }
}
