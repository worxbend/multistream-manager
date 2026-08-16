//! Animation: the shared clock, and the effects that run off it.
//!
//! Every moving thing in the interface — the start-up splash, a heading that
//! types itself in, the shimmer on a live indicator, a notification sliding
//! away — is driven by one clock ticking about ten times a second, and every
//! effect is a *pure function of elapsed time*. Nothing here holds a timer,
//! spawns a task, or remembers which frame it drew last.
//!
//! That matters for two reasons. The drawing code stays a plain function of
//! state, so a test can ask "what does this look like 300 milliseconds in?"
//! and get a deterministic answer with no sleeping. And ten ticks a second is
//! cheap enough that animation costs a rounding error of CPU time, rather
//! than the busy loop a per-effect timer would need.
//!
//! Everything here works in *grapheme clusters* rather than characters. A
//! character is not a unit a terminal draws: `é` can be two characters, a flag
//! is two, and a family emoji can be seven. Revealing text one character at a
//! time would show half-formed glyphs. A grapheme cluster is what a reader
//! calls "one letter", so that is the unit every effect steps by.

use std::time::Duration;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::theme;

/// How often the animation clock ticks.
///
/// Ten frames a second: enough for a pulse, a typewriter or a sweep to read
/// as motion rather than as a slideshow, and slow enough that the cost does
/// not show up in `top`. Film runs at 24 and nobody calls that a slideshow;
/// coloured text moving across a terminal needs far less.
pub const FRAME_INTERVAL: Duration = Duration::from_millis(100);

/// How much the interface animates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    /// Every animated element is drawn at its finished frame. Nothing is
    /// hidden — the splash still shows its logo, a typed heading still shows
    /// its full text — it simply does not move.
    Off,
    /// Every effect still runs, in fewer and slower steps. For when motion is
    /// uncomfortable, or the terminal is at the far end of a slow link where
    /// each frame costs a round trip.
    Reduced,
    /// The full effect.
    #[default]
    Fast,
}

impl Mode {
    /// Read a mode from the config file. `None` means the value was not one
    /// of the three names.
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "off" | "none" | "false" => Some(Mode::Off),
            "reduced" | "slow" => Some(Mode::Reduced),
            "fast" | "on" | "full" | "true" => Some(Mode::Fast),
            _ => None,
        }
    }

    /// How long one step of an effect lasts in this mode.
    ///
    /// `Reduced` does not merely slow the same animation down — that would
    /// make it last longer, which is the opposite of what someone asking for
    /// less motion wants. It takes bigger steps at a slower rate, so the
    /// effect finishes in a comparable time having drawn a third of the
    /// frames.
    fn step(self, base: Duration) -> Duration {
        match self {
            Mode::Off => Duration::ZERO,
            Mode::Reduced => base * 3,
            Mode::Fast => base,
        }
    }

    /// How many units one step advances by.
    fn units_per_step(self) -> usize {
        match self {
            Mode::Off => 0,
            Mode::Reduced => 3,
            Mode::Fast => 1,
        }
    }
}

/// An animated treatment for a short label.
///
/// These are for *chrome* — a splash tagline, a section heading, a live
/// badge — and deliberately not for chat messages. Chat has to stay readable
/// and scroll-stable while it is arriving; animating the text people are
/// trying to read is a way of making it harder to read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Effect {
    /// No motion: the label in its resting colour.
    #[default]
    None,
    /// The label types itself in from the left, behind a blinking caret.
    Typewriter,
    /// A colour ramp between the base and accent colours rotates through the
    /// label, so colour appears to travel along it.
    GradientWave,
    /// A narrow bright band sweeps across the label, then rests before the
    /// next pass.
    Shimmer,
    /// The label slides back and forth along a fixed track, leaving a fading
    /// trail behind it.
    Bounce,
    /// The whole label fades between its base and accent colours, which reads
    /// as a slow heartbeat. Used for "recording" and "live" indicators.
    Pulse,
}

/// One run of styled text within an animated label.
///
/// Colours are `#rrggbb` strings rather than terminal escape codes, because
/// this module renders *what colour goes where* and knows nothing about
/// terminals. The drawing code turns these into ratatui spans.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cell {
    pub text: String,
    pub color: String,
    pub bold: bool,
}

/// How one animated label behaves.
#[derive(Debug, Clone)]
pub struct Config {
    pub effect: Effect,
    pub mode: Mode,
    /// The resting colour, and the near end of any colour ramp.
    pub base: String,
    /// The far end of a ramp, the bright band of a shimmer, and the caret.
    pub accent: String,
    /// The colour a bounce trail fades toward. Defaults to `base`, which
    /// renders the trail as a plain motion blur.
    pub trail: Option<String>,
    /// The caret glyph. Must be one cell wide so it can stand in for an
    /// unrevealed cluster without changing the label's width.
    pub cursor: String,
    /// How long one step lasts. `None` takes the effect's own default.
    pub step: Option<Duration>,
    /// The track a bounce travels along. A width at or below the label's own
    /// width leaves it stationary.
    pub width: usize,
    /// Shifts the starting point of the continuous effects by whole columns,
    /// so several labels can be staggered off one clock instead of each
    /// needing its own.
    pub offset: usize,
    pub bold: bool,
}

impl Config {
    /// A config for `effect`, running between two colours.
    pub fn new(effect: Effect, mode: Mode, base: &str, accent: &str) -> Self {
        Self {
            effect,
            mode,
            base: base.to_string(),
            accent: accent.to_string(),
            trail: None,
            cursor: DEFAULT_CURSOR.to_string(),
            step: None,
            width: 0,
            offset: 0,
            bold: false,
        }
    }

    pub fn bold(mut self, bold: bool) -> Self {
        self.bold = bold;
        self
    }

    pub fn step(mut self, step: Duration) -> Self {
        self.step = Some(step);
        self
    }

    pub fn width(mut self, width: usize) -> Self {
        self.width = width;
        self
    }

    pub fn offset(mut self, offset: usize) -> Self {
        self.offset = offset;
        self
    }

    pub fn trail(mut self, trail: &str) -> Self {
        self.trail = Some(trail.to_string());
        self
    }

    /// The step length actually used, after the mode and the effect's own
    /// default have had their say.
    fn resolved_step(&self) -> Duration {
        let base = self.step.unwrap_or(match self.effect {
            Effect::Typewriter => Duration::from_millis(30),
            Effect::Pulse => Duration::from_millis(120),
            _ => Duration::from_millis(60),
        });
        self.mode.step(base)
    }

    fn trail_color(&self) -> String {
        self.trail.clone().unwrap_or_else(|| self.base.clone())
    }
}

/// The default caret: a solid half-block, one cell wide.
pub const DEFAULT_CURSOR: &str = "▌";

/// How many columns the bright band of a shimmer covers.
const SHIMMER_BAND: usize = 3;
/// How many extra columns a shimmer travels past the end of the label before
/// wrapping, which reads as a pause between passes rather than a strobe.
const SHIMMER_REST: usize = 6;
/// How many fading ghosts a bounce leaves behind it.
const BOUNCE_TRAIL: usize = 3;

/// Render `text` under `config` at `elapsed` since the effect began.
///
/// The result always occupies the same display width as the source text (or
/// `config.width` for a bounce), on every frame including the very first. An
/// animated label therefore never reflows what is drawn around it.
pub fn frame(text: &str, config: &Config, elapsed: Duration) -> Vec<Cell> {
    let units = clusters(text);
    if units.is_empty() {
        return Vec::new();
    }
    let step = config.resolved_step();
    if config.mode == Mode::Off || step.is_zero() {
        return merge(finished_cells(&units, config));
    }
    let cells = match config.effect {
        Effect::None => finished_cells(&units, config),
        Effect::Typewriter => typewriter_cells(&units, config, elapsed, step),
        Effect::GradientWave => gradient_wave_cells(&units, config, elapsed, step),
        Effect::Shimmer => shimmer_cells(&units, config, elapsed, step),
        Effect::Bounce => bounce_cells(&units, config, elapsed, step),
        Effect::Pulse => pulse_cells(&units, config, elapsed, step),
    };
    merge(cells)
}

/// Whether the effect has reached its final frame and will not change again.
///
/// Only the typewriter finishes; the others repeat for as long as animation
/// is on. Callers that stop doing work once an effect settles must therefore
/// only do so for the one-shot effects.
pub fn is_done(text: &str, config: &Config, elapsed: Duration) -> bool {
    let step = config.resolved_step();
    if config.mode == Mode::Off || step.is_zero() || config.effect == Effect::None {
        return true;
    }
    if config.effect != Effect::Typewriter {
        return false;
    }
    let total = clusters(text).len();
    revealed(total, config, elapsed, step) >= total
}

/// One grapheme cluster and how many terminal cells it takes up.
#[derive(Debug, Clone)]
struct Cluster {
    text: String,
    width: usize,
}

fn clusters(text: &str) -> Vec<Cluster> {
    text.graphemes(true)
        .map(|cluster| Cluster {
            text: cluster.to_string(),
            width: UnicodeWidthStr::width(cluster).max(1),
        })
        .collect()
}

fn clusters_width(units: &[Cluster]) -> usize {
    units.iter().map(|unit| unit.width).sum()
}

fn finished_cells(units: &[Cluster], config: &Config) -> Vec<Cell> {
    units
        .iter()
        .map(|unit| Cell {
            text: unit.text.clone(),
            color: config.base.clone(),
            bold: config.bold,
        })
        .collect()
}

/// How many clusters a step-based effect has got through by `elapsed`.
fn revealed(total: usize, config: &Config, elapsed: Duration, step: Duration) -> usize {
    if elapsed.is_zero() || step.is_zero() {
        return 0;
    }
    let steps = (elapsed.as_nanos() / step.as_nanos()) as usize;
    (steps * config.mode.units_per_step().max(1)).min(total)
}

/// How many whole columns a continuous effect has travelled by `elapsed`.
fn phase(config: &Config, elapsed: Duration, step: Duration) -> usize {
    if step.is_zero() {
        return config.offset;
    }
    let steps = (elapsed.as_nanos() / step.as_nanos()) as usize;
    steps * config.mode.units_per_step().max(1) + config.offset
}

/// Reveal from the left, padding the rest with blanks.
///
/// Padding rather than truncating is what stops a centred tagline sliding
/// sideways as it types: the label occupies its final width from the first
/// frame, so whatever centres it never has to move.
fn typewriter_cells(
    units: &[Cluster],
    config: &Config,
    elapsed: Duration,
    step: Duration,
) -> Vec<Cell> {
    let shown = revealed(units.len(), config, elapsed, step);
    let caret = shown < units.len() && caret_visible(elapsed, step);
    units
        .iter()
        .enumerate()
        .map(|(index, unit)| {
            if index < shown {
                Cell {
                    text: unit.text.clone(),
                    color: config.base.clone(),
                    bold: config.bold,
                }
            } else if index == shown && caret {
                caret_cell(unit.width, config)
            } else {
                Cell {
                    text: " ".repeat(unit.width),
                    color: config.base.clone(),
                    bold: false,
                }
            }
        })
        .collect()
}

/// Blink the caret on a multiple of the typing step, so the blink rate scales
/// with the typing speed instead of needing a second timer of its own.
fn caret_visible(elapsed: Duration, step: Duration) -> bool {
    let blink = step.saturating_mul(4);
    if blink.is_zero() {
        return true;
    }
    (elapsed.as_nanos() / blink.as_nanos()).is_multiple_of(2)
}

fn caret_cell(cell_width: usize, config: &Config) -> Cell {
    let mut text = config.cursor.clone();
    let pad = cell_width.saturating_sub(UnicodeWidthStr::width(config.cursor.as_str()));
    text.push_str(&" ".repeat(pad));
    Cell {
        text,
        color: config.accent.clone(),
        bold: true,
    }
}

fn gradient_wave_cells(
    units: &[Cluster],
    config: &Config,
    elapsed: Duration,
    step: Duration,
) -> Vec<Cell> {
    let colors = theme::seamless_gradient(&config.base, &config.accent, clusters_width(units));
    if colors.is_empty() {
        return finished_cells(units, config);
    }
    let phase = phase(config, elapsed, step);
    let mut column = 0;
    units
        .iter()
        .map(|unit| {
            let cell = Cell {
                text: unit.text.clone(),
                color: colors[(column + phase) % colors.len()].clone(),
                bold: config.bold,
            };
            column += unit.width;
            cell
        })
        .collect()
}

/// Sweep a bright head across the label, and keep travelling past the end for
/// a few more columns so there is a gap before the next pass.
fn shimmer_cells(
    units: &[Cluster],
    config: &Config,
    elapsed: Duration,
    step: Duration,
) -> Vec<Cell> {
    let total = clusters_width(units).max(1);
    let head = phase(config, elapsed, step) % (total + SHIMMER_REST);
    let mut column = 0usize;
    units
        .iter()
        .map(|unit| {
            let distance = column.abs_diff(head);
            let mut cell = Cell {
                text: unit.text.clone(),
                color: config.base.clone(),
                bold: config.bold,
            };
            if distance <= SHIMMER_BAND {
                let intensity = 1.0 - distance as f64 / (SHIMMER_BAND + 1) as f64;
                cell.color = theme::mix(&config.base, &config.accent, intensity);
                cell.bold = config.bold || distance == 0;
            }
            column += unit.width;
            cell
        })
        .collect()
}

/// Fade the whole label between its two colours, on a triangle wave.
///
/// A triangle rather than an on/off blink: a hard blink at this rate reads as
/// a fault indicator, and can be genuinely unpleasant to sit next to for an
/// hour, which is how long a stream lasts.
fn pulse_cells(units: &[Cluster], config: &Config, elapsed: Duration, step: Duration) -> Vec<Cell> {
    const PERIOD: usize = 10;
    let position = phase(config, elapsed, step) % PERIOD;
    let half = PERIOD / 2;
    let intensity = if position <= half {
        position as f64 / half as f64
    } else {
        (PERIOD - position) as f64 / half as f64
    };
    let color = theme::mix(&config.base, &config.accent, intensity);
    units
        .iter()
        .map(|unit| Cell {
            text: unit.text.clone(),
            color: color.clone(),
            bold: config.bold,
        })
        .collect()
}

/// Draw the label at its current place on the track, preceded by fainter
/// ghosts of where it just was. Ghosts are drawn first so the label always
/// wins any overlap.
fn bounce_cells(
    units: &[Cluster],
    config: &Config,
    elapsed: Duration,
    step: Duration,
) -> Vec<Cell> {
    let label_width = clusters_width(units);
    let track_width = config.width.max(label_width);
    let travel = track_width - label_width;
    if travel == 0 {
        return finished_cells(units, config);
    }

    let (position, forward) = ping_pong(phase(config, elapsed, step), travel);
    let mut track = Track::new(track_width, &config.base);
    for ghost in (1..=BOUNCE_TRAIL).rev() {
        let at = if forward {
            position.checked_sub(ghost)
        } else {
            Some(position + ghost)
        };
        let Some(at) = at else { continue };
        if at > travel {
            continue;
        }
        let intensity = (BOUNCE_TRAIL - ghost + 1) as f64 / (BOUNCE_TRAIL + 1) as f64;
        let color = theme::mix(&config.trail_color(), &config.base, intensity);
        track.draw(at, units, &color, false);
    }
    track.draw(position, units, &config.base, config.bold);
    track.cells()
}

/// Map an ever-increasing step count onto a position that bounces between 0
/// and `travel`, plus whether it is currently moving forwards.
fn ping_pong(steps: usize, travel: usize) -> (usize, bool) {
    if travel == 0 {
        return (0, true);
    }
    let position = steps % (travel * 2);
    if position <= travel {
        (position, true)
    } else {
        (travel * 2 - position, false)
    }
}

/// A fixed-width row of cells that overlapping draws compose into.
///
/// A wide cluster occupies its first column and marks the rest as
/// continuations, so no sequence of overlapping draws can make the finished
/// row wider or narrower than the track it was built for.
struct Track {
    columns: Vec<Option<Cell>>,
    base: String,
}

impl Track {
    fn new(width: usize, base: &str) -> Self {
        Self {
            columns: vec![None; width],
            base: base.to_string(),
        }
    }

    fn draw(&mut self, at: usize, units: &[Cluster], color: &str, bold: bool) {
        let mut column = at;
        for unit in units {
            if column >= self.columns.len() {
                break;
            }
            self.columns[column] = Some(Cell {
                text: unit.text.clone(),
                color: color.to_string(),
                bold,
            });
            // A double-width cluster owns the column after it too. Blanking
            // it stops a later draw slotting a glyph into the half-cell.
            for continuation in 1..unit.width {
                if let Some(slot) = self.columns.get_mut(column + continuation) {
                    *slot = None;
                }
            }
            column += unit.width;
        }
    }

    fn cells(self) -> Vec<Cell> {
        let mut cells = Vec::with_capacity(self.columns.len());
        let mut skip = 0usize;
        for (index, slot) in self.columns.iter().enumerate() {
            if skip > 0 {
                skip -= 1;
                continue;
            }
            match slot {
                Some(cell) => {
                    let cell_width = UnicodeWidthStr::width(cell.text.as_str()).max(1);
                    // Only claim the extra columns if they are actually
                    // there; a cluster clipped by the track's end is drawn as
                    // a blank rather than allowed to overflow it.
                    if index + cell_width <= self.columns.len() {
                        skip = cell_width - 1;
                        cells.push(cell.clone());
                    } else {
                        cells.push(Cell {
                            text: " ".to_string(),
                            color: self.base.clone(),
                            bold: false,
                        });
                    }
                }
                None => cells.push(Cell {
                    text: " ".to_string(),
                    color: self.base.clone(),
                    bold: false,
                }),
            }
        }
        cells
    }
}

/// Join neighbouring cells that share a colour and weight.
///
/// Purely an efficiency measure: a 40-character gradient produces 40 cells,
/// but a typewriter's blank padding is one run, and drawing one span beats
/// drawing forty identical ones.
fn merge(cells: Vec<Cell>) -> Vec<Cell> {
    let mut merged: Vec<Cell> = Vec::with_capacity(cells.len());
    for cell in cells {
        match merged.last_mut() {
            Some(last) if last.color == cell.color && last.bold == cell.bold => {
                last.text.push_str(&cell.text);
            }
            _ => merged.push(cell),
        }
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The plain text of a frame — what the label physically occupies.
    ///
    /// A test helper rather than part of the module's API: the drawing code
    /// wants the styled cells, and the only thing that needs the text back
    /// out of them is a test checking what a frame says.
    fn plain(cells: &[Cell]) -> String {
        cells.iter().map(|cell| cell.text.as_str()).collect()
    }

    /// The display width of a frame, in terminal cells.
    fn width(cells: &[Cell]) -> usize {
        UnicodeWidthStr::width(plain(cells).as_str())
    }

    const BASE: &str = "#ffffff";
    const ACCENT: &str = "#ff0000";

    fn config(effect: Effect) -> Config {
        Config::new(effect, Mode::Fast, BASE, ACCENT).step(Duration::from_millis(10))
    }

    #[test]
    fn mode_parsing_accepts_the_names_and_rejects_anything_else() {
        assert_eq!(Mode::parse("off"), Some(Mode::Off));
        assert_eq!(Mode::parse(" REDUCED "), Some(Mode::Reduced));
        assert_eq!(Mode::parse("fast"), Some(Mode::Fast));
        assert_eq!(Mode::parse("sideways"), None);
        assert_eq!(Mode::default(), Mode::Fast);
    }

    /// The whole point of the width guarantee: whatever the effect and
    /// whenever it is sampled, the label occupies the same room, so nothing
    /// around it ever reflows.
    #[test]
    fn every_effect_keeps_the_labels_width_on_every_frame() {
        let text = "msm // one stream, two platforms";
        let expected = UnicodeWidthStr::width(text);
        for effect in [
            Effect::None,
            Effect::Typewriter,
            Effect::GradientWave,
            Effect::Shimmer,
            Effect::Pulse,
        ] {
            for tick in 0..40 {
                let cells = frame(text, &config(effect), Duration::from_millis(tick * 10));
                assert_eq!(
                    width(&cells),
                    expected,
                    "{effect:?} changed width at tick {tick}"
                );
            }
        }
    }

    #[test]
    fn a_bounce_keeps_the_width_of_its_track() {
        let config = config(Effect::Bounce).width(30);
        for tick in 0..60 {
            let cells = frame("hello", &config, Duration::from_millis(tick * 10));
            assert_eq!(width(&cells), 30, "bounce changed width at tick {tick}");
        }
    }

    #[test]
    fn a_typewriter_starts_blank_and_ends_complete() {
        let config = config(Effect::Typewriter);
        // Nothing of the label itself is showing yet — only the caret,
        // sitting where the first letter will land, and blank padding
        // holding the label's final width.
        let start = frame("hello", &config, Duration::ZERO);
        assert_eq!(plain(&start), "▌    ");
        assert!(!is_done("hello", &config, Duration::ZERO));

        let end = frame("hello", &config, Duration::from_millis(500));
        assert_eq!(plain(&end), "hello");
        assert!(is_done("hello", &config, Duration::from_millis(500)));
    }

    /// A grapheme cluster is one letter to a reader even when it is several
    /// characters, so a typewriter must never stop halfway through one.
    #[test]
    fn a_typewriter_reveals_whole_clusters_not_half_glyphs() {
        let text = "a👨‍👩‍👧b";
        let config = config(Effect::Typewriter);
        for tick in 0..20 {
            let shown = plain(&frame(text, &config, Duration::from_millis(tick * 10)));
            let shown = shown.trim_end_matches([' ', '▌'].as_ref());
            assert!(
                text.starts_with(shown),
                "tick {tick} produced {shown:?}, which is not a prefix of the label"
            );
        }
    }

    #[test]
    fn animation_off_draws_the_finished_frame_immediately() {
        let mut config = config(Effect::Typewriter);
        config.mode = Mode::Off;
        assert_eq!(plain(&frame("hello", &config, Duration::ZERO)), "hello");
        assert!(is_done("hello", &config, Duration::ZERO));
    }

    /// Reduced motion must still finish: an effect that never completes
    /// because it steps too slowly would leave a heading half-typed forever.
    #[test]
    fn reduced_motion_still_completes_a_typewriter() {
        let mut config = config(Effect::Typewriter);
        config.mode = Mode::Reduced;
        assert!(is_done("hello", &config, Duration::from_secs(2)));
    }

    #[test]
    fn a_continuous_effect_never_reports_itself_finished() {
        for effect in [Effect::GradientWave, Effect::Shimmer, Effect::Pulse] {
            assert!(
                !is_done("hello", &config(effect), Duration::from_secs(60)),
                "{effect:?} claimed to be finished"
            );
        }
    }

    #[test]
    fn a_continuous_effect_returns_to_its_starting_frame() {
        // A gradient wave over an n-column label has an n-column cycle, so
        // sampling one full cycle apart must give the same colours back.
        let text = "abcdefgh";
        let config = config(Effect::GradientWave);
        let start = frame(text, &config, Duration::from_millis(0));
        let cycle = frame(text, &config, Duration::from_millis(80));
        assert_eq!(start, cycle);
    }

    #[test]
    fn a_shimmer_puts_its_brightest_cell_where_the_head_is() {
        let text = "aaaaaaaaaa";
        let config = config(Effect::Shimmer);
        let cells = frame(text, &config, Duration::ZERO);
        // The head starts at column zero, so the first cell is the accent
        // colour itself and stands alone as its own bold run.
        assert_eq!(cells[0].color, ACCENT);
        assert!(cells[0].bold);
    }

    #[test]
    fn a_bounce_travels_out_and_comes_back() {
        assert_eq!(ping_pong(0, 4), (0, true));
        assert_eq!(ping_pong(4, 4), (4, true));
        assert_eq!(ping_pong(6, 4), (2, false));
        assert_eq!(ping_pong(8, 4), (0, true));
        assert_eq!(ping_pong(3, 0), (0, true));
    }

    #[test]
    fn neighbouring_cells_of_the_same_colour_become_one_run() {
        let cells = frame("hello", &config(Effect::None), Duration::ZERO);
        assert_eq!(cells.len(), 1);
        assert_eq!(cells[0].text, "hello");
    }

    #[test]
    fn an_empty_label_produces_no_cells() {
        assert!(frame("", &config(Effect::Typewriter), Duration::ZERO).is_empty());
    }

    /// A double-width glyph on a bounce track must claim exactly two columns,
    /// or the track's total width would drift as the label moves.
    #[test]
    fn a_wide_glyph_on_a_track_claims_both_of_its_columns() {
        let config = config(Effect::Bounce).width(12);
        for tick in 0..24 {
            let cells = frame("漢字", &config, Duration::from_millis(tick * 10));
            assert_eq!(width(&cells), 12, "wide glyphs drifted at tick {tick}");
        }
    }
}
