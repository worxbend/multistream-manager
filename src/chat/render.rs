//! Pure rendering: normalized chat messages → styled ratatui lines.
//!
//! Ported from `twi` (commit 7c6ad6bbbc3dec1b6af2ddbd03b55f96c0c162cf,
//! internal/render/message.go + internal/render/badges.go +
//! internal/theme/palette.go `IdentityColor`) and `yc` (commit
//! 9e67efd10c0790ec22df2c944bcee6be1bc37cf8, internal/render/superchat.go
//! `TierColor`/chips + internal/youtube/fragments.go `SplitFragments`).
//!
//! Everything here is a pure function of its inputs — no I/O, no globals —
//! so the draw loop can call it every frame and the tests can call it with
//! literal messages.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::chat::{ChatMessage, MembershipDetails, MembershipKind, MessageKind, PlatformMeta};

/// Rows narrower than this render as if they were this wide; below it no
/// prefix + body layout survives (twi: minimumRenderWidth).
const MINIMUM_RENDER_WIDTH: u16 = 8;

/// WCAG (Web Content Accessibility Guidelines) minimum contrast for normal
/// text — the same 4.5:1 threshold both source apps enforce.
const MINIMUM_TEXT_CONTRAST: f64 = 4.5;

// ---------------------------------------------------------------------------
// Palette
//
// The host owns its palette (PLAN.md row T31: themes are not ported); these
// mirror the fixed colors in `ui/draw.rs::theme` so the chat rows sit on the
// same canvas as the rest of the TUI.
// ---------------------------------------------------------------------------

const TEXT: Color = Color::Rgb(230, 237, 243);
const MUTED: Color = Color::Rgb(139, 148, 158);
const ACCENT: Color = Color::Rgb(145, 71, 255);
const SUCCESS: Color = Color::Rgb(63, 185, 80);
const WARNING: Color = Color::Rgb(210, 153, 34);
const ERROR: Color = Color::Rgb(248, 81, 73);
/// The terminal canvas the chip text sits on. Chips draw their tier color as
/// the background (yc: a solid block is impossible to scroll past where a
/// tinted word is not), so the text uses the dark canvas color.
const CANVAS: Color = Color::Rgb(13, 17, 23);

// ---------------------------------------------------------------------------
// Identity colors (twi/yc theme.IdentityColor)
// ---------------------------------------------------------------------------

/// A deterministic, random-looking color for an identity that meets text
/// contrast against every provided background. Keeps author colors stable
/// across frames and sessions without any mutable UI state.
///
/// Port note: the Go original also has a light-canvas branch (darker
/// candidate lightnesses when any background is lighter than 0.45 relative
/// luminance). This host's TUI paints a fixed dark palette (`ui/draw.rs`), so
/// only the dark-canvas candidate ladder is ported; a light background simply
/// fails the contrast check and falls through to `fallback`, which is the
/// same safe outcome the original guarantees.
pub fn identity_color(identity: &str, backgrounds: &[Color], fallback: Color) -> Color {
    let identity = identity.trim().to_lowercase();
    if identity.is_empty() {
        return fallback;
    }
    let hash = fnv1a64(identity.as_bytes());
    let hue = (hash % 360) as f64 / 360.0;
    let saturation = 0.62 + ((hash >> 12) % 18) as f64 / 100.0;

    // Only concrete RGB backgrounds can be contrast-checked; named/indexed
    // terminal colors have no knowable RGB value, so they are skipped the way
    // the original skips unparseable hex strings.
    let parsed: Vec<(u8, u8, u8)> = backgrounds
        .iter()
        .filter_map(|color| match color {
            Color::Rgb(r, g, b) => Some((*r, *g, *b)),
            _ => None,
        })
        .collect();
    if parsed.is_empty() {
        return fallback;
    }

    for lightness in [0.68, 0.74, 0.80, 0.86] {
        let candidate = hsl_to_rgb(hue, saturation, lightness);
        let readable = parsed
            .iter()
            .all(|bg| contrast_ratio(candidate, *bg) >= MINIMUM_TEXT_CONTRAST);
        if readable {
            return Color::Rgb(candidate.0, candidate.1, candidate.2);
        }
    }
    fallback
}

/// FNV-1a (Fowler–Noll–Vo, variant 1a) 64-bit hash — the exact hash the Go
/// standard library's `hash/fnv` implements, so identities map to the same
/// hues as in the source apps.
fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// HSL (hue, saturation, lightness — all 0..=1) to RGB, matching the Go
/// original's rounding so identical identities produce identical bytes.
fn hsl_to_rgb(hue: f64, saturation: f64, lightness: f64) -> (u8, u8, u8) {
    let c = (1.0 - (2.0 * lightness - 1.0).abs()) * saturation;
    let h = hue * 6.0;
    let x = c * (1.0 - (h % 2.0 - 1.0).abs());
    let (r, g, b) = if h < 1.0 {
        (c, x, 0.0)
    } else if h < 2.0 {
        (x, c, 0.0)
    } else if h < 3.0 {
        (0.0, c, x)
    } else if h < 4.0 {
        (0.0, x, c)
    } else if h < 5.0 {
        (x, 0.0, c)
    } else {
        (c, 0.0, x)
    };
    let m = lightness - c / 2.0;
    let channel = |v: f64| ((v + m) * 255.0).round() as u8;
    (channel(r), channel(g), channel(b))
}

/// WCAG contrast ratio between two colors, from 1 (identical) to 21
/// (black on white).
fn contrast_ratio(a: (u8, u8, u8), b: (u8, u8, u8)) -> f64 {
    let la = relative_luminance(a);
    let lb = relative_luminance(b);
    let (light, dark) = if la > lb { (la, lb) } else { (lb, la) };
    (light + 0.05) / (dark + 0.05)
}

/// WCAG relative luminance of an sRGB color.
fn relative_luminance((r, g, b): (u8, u8, u8)) -> f64 {
    0.2126 * linearized(r) + 0.7152 * linearized(g) + 0.0722 * linearized(b)
}

/// One sRGB channel undone from gamma encoding, per the WCAG definition.
fn linearized(component: u8) -> f64 {
    let value = f64::from(component) / 255.0;
    if value <= 0.04045 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}

// ---------------------------------------------------------------------------
// Badges (twi badges.go, plus yc's synthesized owner/member/verified sets)
// ---------------------------------------------------------------------------

/// The palette role a badge glyph is tinted with. Roles rather than colors so
/// the mapping stays meaningful if the host palette ever changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BadgeTone {
    /// Channel authority itself (broadcaster / owner).
    Alert,
    /// Delegated authority (moderator, platform staff).
    Success,
    /// Distinguished-but-not-empowered (VIP, artist, verified).
    Accent,
    /// Paying supporter status.
    Warning,
    /// Everything unrecognized or purely cosmetic.
    Muted,
}

impl BadgeTone {
    /// The concrete color for this tone on the host palette.
    pub fn color(self) -> Color {
        match self {
            Self::Alert => ERROR,
            Self::Success => SUCCESS,
            Self::Accent => ACCENT,
            Self::Warning => WARNING,
            Self::Muted => MUTED,
        }
    }
}

/// The single-cell icon and tone for a badge set id.
///
/// The glyphs are plain Unicode symbols, not Nerd Font private-use
/// codepoints, so they render in an unpatched terminal font; every one is
/// deliberately width-1 (asserted in a test) so badge columns line up between
/// rows — emoji-presentation characters would be width-2 and break that.
/// Unknown sets get a neutral marker so an unrecognized badge still occupies
/// its column rather than silently disappearing.
pub fn badge_glyph(set: &str) -> (&'static str, BadgeTone) {
    match set.trim().to_lowercase().as_str() {
        "broadcaster" | "owner" => ("◉", BadgeTone::Alert),
        "moderator" => ("⚔", BadgeTone::Success),
        "vip" => ("◆", BadgeTone::Accent),
        "subscriber" | "member" => ("★", BadgeTone::Warning),
        "founder" => ("♦", BadgeTone::Warning),
        "staff" | "admin" | "global_mod" => ("⚙", BadgeTone::Success),
        "partner" | "verified" => (
            "✓",
            if set.eq_ignore_ascii_case("verified") {
                BadgeTone::Accent
            } else {
                BadgeTone::Muted
            },
        ),
        "turbo" => ("↯", BadgeTone::Muted),
        "premium" => ("♛", BadgeTone::Warning),
        "bits" | "bits-leader" => ("◈", BadgeTone::Muted),
        "sub-gifter" => ("♥", BadgeTone::Warning),
        "artist" => ("✎", BadgeTone::Accent),
        _ => ("•", BadgeTone::Muted),
    }
}

// ---------------------------------------------------------------------------
// Fragments (yc fragments.go SplitFragments)
// ---------------------------------------------------------------------------

/// The semantic role of one piece of a message body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FragKind {
    Text,
    /// `@name` at a word boundary.
    Mention,
    /// `:shortcode:` — YouTube channel emoji arrive as literal shortcodes the
    /// API never resolves to images, so they render as fixed chips.
    Shortcode,
    /// `http(s)://…` or `www.…` at a word boundary.
    Url,
    /// One emoji grapheme cluster (a flag, keycap, or ZWJ family moves whole).
    Emoji,
}

/// One classified piece of a message body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frag {
    pub kind: FragKind,
    pub text: String,
}

/// Splits a plain message body into semantic fragments.
///
/// Mentions and URLs only start at a word boundary so an address inside a
/// word (`mailto:me@example.com`) is not mistaken for a mention; a shortcode
/// may start anywhere, because YouTube emits them flush against neighboring
/// text. Adjacent plain-text pieces are coalesced into one fragment.
pub fn split_fragments(text: &str) -> Vec<Frag> {
    let mut fragments: Vec<Frag> = Vec::new();
    let mut pending = String::new();

    // Closures cannot borrow `fragments` and `pending` mutably at once, so
    // flushing is a free function over both.
    fn flush(fragments: &mut Vec<Frag>, pending: &mut String) {
        if !pending.is_empty() {
            fragments.push(Frag {
                kind: FragKind::Text,
                text: std::mem::take(pending),
            });
        }
    }

    let mut at_word_start = true;
    let mut skip_until = 0usize;
    for (start, cluster) in text.grapheme_indices(true) {
        if start < skip_until {
            // A token consumed this cluster already; the segmenter still owns
            // every boundary, we only fast-forward past what was emitted.
            continue;
        }

        // Emoji first: a cluster can be several runes and must move as one.
        if is_emoji_cluster(cluster) {
            flush(&mut fragments, &mut pending);
            fragments.push(Frag {
                kind: FragKind::Emoji,
                text: cluster.to_string(),
            });
            skip_until = start + cluster.len();
            at_word_start = false;
            continue;
        }

        if let Some((token, kind)) = match_token(&text[start..], cluster, at_word_start) {
            flush(&mut fragments, &mut pending);
            skip_until = start + token.len();
            fragments.push(Frag {
                kind,
                text: token.to_string(),
            });
            at_word_start = false;
            continue;
        }

        pending.push_str(cluster);
        at_word_start = is_boundary_cluster(cluster);
    }
    flush(&mut fragments, &mut pending);
    fragments
}

/// Tries the anchored token patterns against the remainder of the message.
fn match_token<'a>(
    remainder: &'a str,
    cluster: &str,
    at_word_start: bool,
) -> Option<(&'a str, FragKind)> {
    if at_word_start && cluster == "@" {
        if let Some(token) = match_mention(remainder) {
            return Some((token, FragKind::Mention));
        }
    }
    if cluster == ":" {
        if let Some(token) = match_shortcode(remainder) {
            return Some((token, FragKind::Shortcode));
        }
    }
    if at_word_start && matches!(cluster, "h" | "H" | "w" | "W") {
        if let Some(token) = match_url(remainder) {
            return Some((token, FragKind::Url));
        }
    }
    None
}

/// `^@[letter/number/_][letter/number/_.-]*` — YouTube display names allow
/// letters, digits, underscores, hyphens, and periods (Unicode letters and
/// digits included, matching the Go `\p{L}\p{N}` classes).
fn match_mention(remainder: &str) -> Option<&str> {
    let rest = remainder.strip_prefix('@')?;
    let mut chars = rest.char_indices();
    let (_, first) = chars.next()?;
    if !(first.is_alphanumeric() || first == '_') {
        return None;
    }
    let mut end = 1 + first.len_utf8();
    for (offset, c) in chars {
        if c.is_alphanumeric() || matches!(c, '_' | '.' | '-') {
            end = 1 + offset + c.len_utf8();
        } else {
            break;
        }
    }
    Some(&remainder[..end])
}

/// `^:_?[A-Za-z0-9][A-Za-z0-9_+\-]*:` — a channel-emoji literal, custom ones
/// prefixed with an underscore.
fn match_shortcode(remainder: &str) -> Option<&str> {
    let bytes = remainder.as_bytes();
    let mut i = 1; // past the opening ':'
    if bytes.get(i) == Some(&b'_') {
        i += 1;
    }
    if !bytes.get(i)?.is_ascii_alphanumeric() {
        return None;
    }
    i += 1;
    while let Some(&b) = bytes.get(i) {
        if b.is_ascii_alphanumeric() || matches!(b, b'_' | b'+' | b'-') {
            i += 1;
        } else {
            break;
        }
    }
    if bytes.get(i) == Some(&b':') {
        Some(&remainder[..i + 1])
    } else {
        None
    }
}

/// `^(https?://|www\.)[^\s<>"']+` — an http(s) link or a bare `www.` host.
fn match_url(remainder: &str) -> Option<&str> {
    let scheme_len = ["https://", "http://", "www."]
        .iter()
        .find(|scheme| {
            // `get` rather than an index: the prefix boundary may fall inside
            // a multi-byte character, which is simply "no match", not a panic.
            remainder
                .get(..scheme.len())
                .is_some_and(|head| head.eq_ignore_ascii_case(scheme))
        })
        .map(|scheme| scheme.len())?;
    let rest = &remainder[scheme_len..];
    let body_len = rest
        .char_indices()
        .find(|(_, c)| c.is_whitespace() || matches!(c, '<' | '>' | '"' | '\''))
        .map_or(rest.len(), |(i, _)| i);
    if body_len == 0 {
        // "www." alone is not a URL; the Go pattern requires at least one
        // body character after the anchor.
        return None;
    }
    Some(&remainder[..scheme_len + body_len])
}

/// Whether a cluster ends a word, so the next cluster may begin a mention or
/// URL. Only the leading rune matters: a cluster whose base is a letter is
/// part of a word however many marks follow it.
fn is_boundary_cluster(cluster: &str) -> bool {
    match cluster.chars().next() {
        Some(base) => base.is_whitespace() || "([{<\"'`,;".contains(base),
        None => false,
    }
}

/// Whether a grapheme cluster renders as an emoji.
///
/// A pragmatic approximation of Unicode's Extended_Pictographic property,
/// ported from yc's `emojiRanges` table: generous on purpose, because a
/// false positive costs one fragment boundary while a false negative would
/// split a ZWJ (zero-width joiner) sequence across a wrap. Known precision
/// limits: some dingbats and geometric shapes in U+2600–U+27BF and
/// U+2B00–U+2BFF classify as emoji even in their text presentation, and
/// codepoints added to Unicode after the table was written may be missed.
fn is_emoji_cluster(cluster: &str) -> bool {
    cluster
        .chars()
        .any(|r| matches!(r, '\u{FE0F}' | '\u{20E3}' | '\u{200D}') || in_emoji_ranges(r))
}

/// yc's `emojiRanges` table, verbatim (each row is an inclusive codepoint
/// range; the Go table's stride quirks expand to exactly these).
fn in_emoji_ranges(r: char) -> bool {
    matches!(u32::from(r),
        0x00A9 | 0x00AE
        | 0x203C
        | 0x2049
        | 0x2122
        | 0x2139
        | 0x2194..=0x21AA
        | 0x231A..=0x231B
        | 0x2328
        | 0x23CF..=0x23FA
        | 0x24C2
        | 0x25AA..=0x25FE
        | 0x2600..=0x27BF
        | 0x2934..=0x2935
        | 0x2B00..=0x2BFF
        | 0x3030
        | 0x303D
        | 0x3297 | 0x3299
        | 0x1F000..=0x1FAFF
        | 0x1FC00..=0x1FFFD)
}

// ---------------------------------------------------------------------------
// Super Chat tier ladder (yc superchat.go TierColor)
// ---------------------------------------------------------------------------

/// The color role of a paid-amount chip. YouTube's own ladder runs eleven
/// tiers; eleven distinct hues do not exist in a small palette, so the ladder
/// collapses onto six monotonic steps — the exact amount is on the chip
/// either way, the color only has to convey "more than the last one".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChipTone {
    Accent,
    /// Halfway between accent and success (yc: `Mix(accent, success, 0.5)`).
    AccentSuccessMix,
    Success,
    Warning,
    /// Halfway between warning and error (yc: `Mix(warning, error, 0.5)`).
    WarningErrorMix,
    Error,
}

impl ChipTone {
    /// The concrete color for this tone.
    ///
    /// The mixes are fixed 50/50 RGB averages of the host palette, computed
    /// once here so the ladder stays monotonic if a palette value moves:
    /// accent (145,71,255) + success (63,185,80) → (104,128,168);
    /// warning (210,153,34) + error (248,81,73) → (229,117,54).
    pub fn color(self) -> Color {
        match self {
            Self::Accent => ACCENT,
            Self::AccentSuccessMix => Color::Rgb(104, 128, 168),
            Self::Success => SUCCESS,
            Self::Warning => WARNING,
            Self::WarningErrorMix => Color::Rgb(229, 117, 54),
            Self::Error => ERROR,
        }
    }
}

/// Maps a YouTube purchase tier (1–11; 0 = unknown) onto the six-step ladder.
pub fn tier_color(tier: u8) -> ChipTone {
    match tier {
        0 | 1 => ChipTone::Accent,
        2 => ChipTone::AccentSuccessMix,
        3..=4 => ChipTone::Success,
        5..=6 => ChipTone::Warning,
        7..=8 => ChipTone::WarningErrorMix,
        9..=11 => ChipTone::Error,
        _ => ChipTone::Accent,
    }
}

// ---------------------------------------------------------------------------
// Message rendering (twi message.go inline layout)
// ---------------------------------------------------------------------------

/// How badges are drawn beside a username.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BadgeMode {
    /// One icon per badge — compact enough to sit inline with the name.
    #[default]
    Glyph,
    /// Bracketed labels such as `[moderator]` — widest, but unambiguous on
    /// any terminal and any font.
    Text,
    /// No badges.
    Off,
}

/// Presentation switches for [`render_message`]. Everything else the layout
/// needs travels in the message itself.
#[derive(Debug, Clone)]
pub struct RenderOpts {
    pub badge_mode: BadgeMode,
    pub timestamps: bool,
}

impl Default for RenderOpts {
    fn default() -> Self {
        Self {
            badge_mode: BadgeMode::Glyph,
            timestamps: true,
        }
    }
}

/// A styled piece queued for wrapping. `atomic` pieces (badges, mentions,
/// shortcodes, URLs, emoji, chips) move to the next row whole; text pieces
/// break between words.
struct Piece {
    text: String,
    style: Style,
    atomic: bool,
}

/// Renders one message as wrapped terminal lines:
/// `HH:MM badges name: body`, continuation rows indented under the body.
pub fn render_message(msg: &ChatMessage, width: u16, opts: &RenderOpts) -> Vec<Line<'static>> {
    let width = usize::from(width.max(MINIMUM_RENDER_WIDTH));

    let mut prefix: Vec<Piece> = Vec::new();

    if opts.timestamps {
        // `--:--` for an unknown timestamp: absent data is never a negative
        // fact, but the column must keep its width so rows align.
        let stamp = match msg.timestamp {
            Some(ts) => ts.format("%H:%M").to_string(),
            None => "--:--".to_string(),
        };
        prefix.push(Piece {
            text: format!("{stamp} "),
            style: Style::new().fg(MUTED),
            atomic: true,
        });
    }

    match opts.badge_mode {
        BadgeMode::Off => {}
        BadgeMode::Glyph => {
            for badge in &msg.author.badges {
                let (glyph, tone) = badge_glyph(&badge.set);
                prefix.push(Piece {
                    text: format!("{glyph} "),
                    style: Style::new().fg(tone.color()),
                    atomic: true,
                });
            }
        }
        BadgeMode::Text => {
            for badge in &msg.author.badges {
                let (_, tone) = badge_glyph(&badge.set);
                prefix.push(Piece {
                    text: format!("[{}] ", badge.set),
                    style: Style::new().fg(tone.color()),
                    atomic: true,
                });
            }
        }
    }

    let action = msg.kind == MessageKind::Action;
    if action {
        prefix.push(Piece {
            text: "* ".to_string(),
            style: Style::new().fg(ACCENT).add_modifier(Modifier::BOLD),
            atomic: true,
        });
    }

    // The color keys off the stable platform id when there is one, so a
    // display-name change does not recolor someone mid-conversation.
    let identity = if msg.author.id.is_empty() {
        &msg.author.login
    } else {
        &msg.author.id
    };
    let name_color = identity_color(identity, &[CANVAS], TEXT);
    let name = if msg.author.display_name.is_empty() {
        &msg.author.login
    } else {
        &msg.author.display_name
    };
    prefix.push(Piece {
        text: name.to_string(),
        style: Style::new().fg(name_color).add_modifier(Modifier::BOLD),
        atomic: true,
    });
    prefix.push(Piece {
        // An action reads as prose ("* name does a thing"), so no colon.
        text: if action { " " } else { ": " }.to_string(),
        style: Style::new().fg(TEXT),
        atomic: true,
    });

    let content = content_pieces(msg);
    wrap(prefix, content, width)
}

/// The body pieces of a message, before wrapping.
fn content_pieces(msg: &ChatMessage) -> Vec<Piece> {
    // The deleted branch comes first and returns: the original text must
    // never reach the screen once a moderator removed it.
    if msg.deleted {
        return vec![Piece {
            text: "[message deleted]".to_string(),
            style: Style::new()
                .fg(MUTED)
                .add_modifier(Modifier::ITALIC | Modifier::CROSSED_OUT),
            atomic: false,
        }];
    }

    let mut pieces: Vec<Piece> = Vec::new();

    if msg.kind == MessageKind::Notice {
        pieces.push(Piece {
            text: "[notice] ".to_string(),
            style: Style::new().fg(WARNING).add_modifier(Modifier::BOLD),
            atomic: true,
        });
    }

    if let Some(PlatformMeta::YouTube(meta)) = &msg.meta {
        if let Some(paid) = &meta.paid {
            // The chip is drawn as colored ground with dark text rather than
            // as colored text: the point of a Super Chat row is that it is
            // impossible to scroll past, and a solid block reads at a glance
            // where a tinted word does not.
            let label = if paid.display.trim().is_empty() {
                format!("{} {}", paid.micros / 1_000_000, paid.currency)
            } else {
                paid.display.trim().to_string()
            };
            pieces.push(Piece {
                text: format!(" {label} "),
                style: Style::new()
                    .fg(CANVAS)
                    .bg(tier_color(paid.tier).color())
                    .add_modifier(Modifier::BOLD),
                atomic: true,
            });
            pieces.push(Piece {
                text: " ".to_string(),
                style: Style::new().fg(TEXT),
                atomic: true,
            });
        }
        if let Some(membership) = &meta.membership {
            let (label, color) = membership_chip(membership);
            pieces.push(Piece {
                text: format!(" {label} "),
                style: Style::new()
                    .fg(CANVAS)
                    .bg(color)
                    .add_modifier(Modifier::BOLD),
                atomic: true,
            });
            pieces.push(Piece {
                text: " ".to_string(),
                style: Style::new().fg(TEXT),
                atomic: true,
            });
        }
    }

    let body_modifier = if msg.kind == MessageKind::Action {
        Modifier::ITALIC
    } else {
        Modifier::empty()
    };
    for frag in split_fragments(&msg.text) {
        let (style, atomic) = match frag.kind {
            FragKind::Text => (Style::new().fg(TEXT).add_modifier(body_modifier), false),
            FragKind::Mention => (
                Style::new()
                    .fg(ACCENT)
                    .add_modifier(Modifier::BOLD | body_modifier),
                true,
            ),
            FragKind::Shortcode => (Style::new().fg(SUCCESS).add_modifier(body_modifier), true),
            FragKind::Url => (
                Style::new()
                    .fg(ACCENT)
                    .add_modifier(Modifier::UNDERLINED | body_modifier),
                true,
            ),
            FragKind::Emoji => (Style::new().fg(TEXT).add_modifier(body_modifier), true),
        };
        pieces.push(Piece {
            text: frag.text,
            style,
            atomic,
        });
    }
    pieces
}

/// The membership chip label + tone (yc superchat.go membershipChipLabel):
/// it states what happened, not who it happened to — the author column
/// already names the person.
fn membership_chip(details: &MembershipDetails) -> (String, Color) {
    match details.kind {
        MembershipKind::New => ("★ new member".to_string(), ACCENT),
        MembershipKind::Upgrade => ("★ member upgrade".to_string(), ACCENT),
        MembershipKind::Milestone => {
            // Months of 0 means "unknown", which must render as nothing
            // rather than "0 mo".
            if details.months > 0 {
                (format!("★ member {} mo", details.months), ACCENT)
            } else {
                ("★ member milestone".to_string(), ACCENT)
            }
        }
        MembershipKind::Gifting => {
            let label = match details.gift_count {
                0 => "♥ gift memberships".to_string(),
                1 => "♥ 1 gift membership".to_string(),
                n => format!("♥ {n} gift memberships"),
            };
            (label, SUCCESS)
        }
        MembershipKind::GiftReceived => ("♥ gift member".to_string(), SUCCESS),
    }
}

// ---------------------------------------------------------------------------
// Wrapping (twi message.go wrap / wrapChunks / appendWrappedClusters)
// ---------------------------------------------------------------------------

/// Wraps prefix + content pieces into rows no wider than `width`, indenting
/// continuation rows by the prefix width so the body forms a clean column.
fn wrap(prefix: Vec<Piece>, content: Vec<Piece>, width: usize) -> Vec<Line<'static>> {
    let prefix_width: usize = prefix.iter().map(|p| p.text.width()).sum();
    // A prefix as wide as the row would leave no room for text; twi halves
    // the indent in that case rather than wrapping every grapheme.
    let indent = if prefix_width >= width {
        width / 2
    } else {
        prefix_width
    };

    let mut wrapper = Wrapper {
        rows: Vec::new(),
        current: Vec::new(),
        used: 0,
        width,
        indent: 0,
    };
    for piece in prefix {
        wrapper.push_atomic(piece.text, piece.style);
    }
    wrapper.indent = indent;
    for piece in content {
        if piece.atomic {
            wrapper.push_atomic(piece.text, piece.style);
        } else {
            wrapper.push_text(&piece.text, piece.style);
        }
    }
    wrapper.finish()
}

struct Wrapper {
    rows: Vec<Line<'static>>,
    current: Vec<Span<'static>>,
    used: usize,
    width: usize,
    indent: usize,
}

impl Wrapper {
    fn break_row(&mut self) {
        let spans = std::mem::take(&mut self.current);
        self.rows.push(Line::from(spans));
        if self.indent > 0 {
            self.current.push(Span::raw(" ".repeat(self.indent)));
        }
        self.used = self.indent;
    }

    fn place(&mut self, text: String, style: Style) {
        self.used += text.width();
        self.current.push(Span::styled(text, style));
    }

    /// An atomic piece moves to the next row whole. A piece wider than a full
    /// row cannot move whole anywhere, so it degrades to per-grapheme
    /// breaking — the one deviation from "atomic frags never split", taken
    /// because the alternative is a row wider than the terminal.
    fn push_atomic(&mut self, text: String, style: Style) {
        let piece_width = text.width();
        if piece_width == 0 {
            return;
        }
        if self.used + piece_width > self.width && self.used > self.indent {
            self.break_row();
        }
        if piece_width > self.width - self.indent.min(self.width - 1) {
            self.push_clusters(&text, style);
            return;
        }
        self.place(text, style);
    }

    /// A text piece breaks between words: word-sized chunks (a run of
    /// non-spaces plus the spaces that follow, so a taken break lands between
    /// words rather than stranding a space at a row start), falling back to
    /// per-grapheme for a word wider than a line.
    fn push_text(&mut self, text: &str, style: Style) {
        for chunk in wrap_chunks(text) {
            let chunk_width = chunk.width();
            if chunk_width > 0
                && chunk_width <= self.width - self.indent
                && self.used + chunk_width > self.width
                && self.used > self.indent
                && !chunk.trim().is_empty()
            {
                self.break_row();
            }
            self.push_clusters(&chunk, style);
        }
    }

    /// Appends grapheme by grapheme, breaking whenever the next cluster
    /// would not fit — the fallback for words wider than a full line.
    fn push_clusters(&mut self, text: &str, style: Style) {
        let mut run = String::new();
        let mut run_width = 0usize;
        for cluster in text.graphemes(true) {
            if cluster == "\n" {
                if !run.is_empty() {
                    self.place(std::mem::take(&mut run), style);
                    run_width = 0;
                }
                self.break_row();
                continue;
            }
            let cluster_width = cluster.width();
            if self.used + run_width + cluster_width > self.width
                && self.used + run_width > self.indent
            {
                if !run.is_empty() {
                    self.place(std::mem::take(&mut run), style);
                    run_width = 0;
                }
                self.break_row();
            }
            run.push_str(cluster);
            run_width += cluster_width;
        }
        if !run.is_empty() {
            self.place(run, style);
        }
    }

    fn finish(mut self) -> Vec<Line<'static>> {
        let spans = std::mem::take(&mut self.current);
        self.rows.push(Line::from(spans));
        self.rows
    }
}

/// Splits text into word-sized chunks, each a run of non-space graphemes
/// together with the spaces that follow it (twi wrapChunks).
fn wrap_chunks(text: &str) -> Vec<String> {
    let mut chunks: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut in_trailing_space = false;
    for cluster in text.graphemes(true) {
        if cluster == "\n" {
            if !current.is_empty() {
                chunks.push(std::mem::take(&mut current));
            }
            chunks.push(cluster.to_string());
            in_trailing_space = false;
            continue;
        }
        let is_space = cluster.trim().is_empty();
        if !is_space && in_trailing_space {
            chunks.push(std::mem::take(&mut current));
            in_trailing_space = false;
        }
        current.push_str(cluster);
        if is_space {
            in_trailing_space = true;
        }
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::{Badge, ChatAuthor, PaidAmount, TwitchMeta, YouTubeMeta};

    fn message(text: &str) -> ChatMessage {
        ChatMessage {
            id: "m1".into(),
            timestamp: None,
            author: ChatAuthor {
                id: "u1".into(),
                login: "someone".into(),
                display_name: "Someone".into(),
                badges: vec![],
                color_hint: None,
            },
            text: text.into(),
            kind: MessageKind::Chat,
            deleted: false,
            historical: false,
            local_echo: false,
            meta: None,
        }
    }

    fn joined(lines: &[Line<'_>]) -> String {
        lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn line_width(line: &Line<'_>) -> usize {
        line.spans.iter().map(|s| s.content.as_ref().width()).sum()
    }

    // -- identity_color -----------------------------------------------------

    const DARK_BG: Color = Color::Rgb(13, 17, 23);

    /// Standalone contrast implementation so the test does not trust the
    /// helper it is checking.
    fn test_contrast(a: (u8, u8, u8), b: (u8, u8, u8)) -> f64 {
        fn lin(c: u8) -> f64 {
            let v = f64::from(c) / 255.0;
            if v <= 0.04045 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        }
        let lum = |(r, g, b): (u8, u8, u8)| 0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b);
        let (la, lb) = (lum(a), lum(b));
        let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
        (hi + 0.05) / (lo + 0.05)
    }

    #[test]
    fn identity_color_is_deterministic() {
        let a = identity_color("somebody", &[DARK_BG], TEXT);
        let b = identity_color("somebody", &[DARK_BG], TEXT);
        assert_eq!(a, b);
        // Case and surrounding whitespace do not change the identity.
        assert_eq!(identity_color(" SomeBody ", &[DARK_BG], TEXT), a);
    }

    #[test]
    fn different_identities_get_different_colors() {
        let a = identity_color("alice", &[DARK_BG], TEXT);
        let b = identity_color("bob", &[DARK_BG], TEXT);
        assert_ne!(a, b);
    }

    #[test]
    fn identity_colors_meet_wcag_contrast_on_the_background() {
        for identity in ["alice", "bob", "carol", "dave", "erin", "mallory", "yui"] {
            let color = identity_color(identity, &[DARK_BG], TEXT);
            let Color::Rgb(r, g, b) = color else {
                panic!("expected an RGB color for {identity}, got {color:?}");
            };
            let ratio = test_contrast((r, g, b), (13, 17, 23));
            assert!(
                ratio >= 4.5,
                "{identity} -> {color:?} has contrast {ratio:.2} < 4.5"
            );
        }
    }

    #[test]
    fn empty_identity_and_unknowable_backgrounds_use_the_fallback() {
        assert_eq!(identity_color("", &[DARK_BG], TEXT), TEXT);
        // Named terminal colors have no knowable RGB, so no contrast check
        // is possible and the fallback wins.
        assert_eq!(identity_color("alice", &[Color::Black], TEXT), TEXT);
        assert_eq!(identity_color("alice", &[], TEXT), TEXT);
    }

    // -- badges -------------------------------------------------------------

    #[test]
    fn every_badge_glyph_is_one_cell_wide() {
        for set in [
            "broadcaster",
            "owner",
            "moderator",
            "vip",
            "subscriber",
            "member",
            "founder",
            "staff",
            "admin",
            "global_mod",
            "partner",
            "verified",
            "turbo",
            "premium",
            "bits",
            "bits-leader",
            "sub-gifter",
            "artist",
            "definitely-not-a-badge",
        ] {
            let (glyph, _) = badge_glyph(set);
            assert_eq!(
                glyph.width(),
                1,
                "badge glyph for {set:?} ({glyph}) is not width 1"
            );
        }
    }

    #[test]
    fn badge_tones_distinguish_authority_from_status() {
        assert_eq!(badge_glyph("broadcaster").1, BadgeTone::Alert);
        assert_eq!(badge_glyph("owner").1, BadgeTone::Alert);
        assert_eq!(badge_glyph("moderator").1, BadgeTone::Success);
        assert_eq!(badge_glyph("vip").1, BadgeTone::Accent);
        assert_eq!(badge_glyph("verified").1, BadgeTone::Accent);
        assert_eq!(badge_glyph("subscriber").1, BadgeTone::Warning);
        assert_eq!(badge_glyph("sub-gifter").1, BadgeTone::Warning);
        assert_eq!(badge_glyph("partner").1, BadgeTone::Muted);
        assert_eq!(badge_glyph("mystery").1, BadgeTone::Muted);
    }

    // -- fragments ----------------------------------------------------------

    fn kinds(frags: &[Frag]) -> Vec<(FragKind, &str)> {
        frags.iter().map(|f| (f.kind, f.text.as_str())).collect()
    }

    #[test]
    fn mentions_are_recognized_at_word_starts_only() {
        let frags = split_fragments("hi @alice and me@example.com");
        assert_eq!(
            kinds(&frags),
            vec![
                (FragKind::Text, "hi "),
                (FragKind::Mention, "@alice"),
                (FragKind::Text, " and me@example.com"),
            ]
        );
    }

    #[test]
    fn a_mailto_address_is_not_a_mention() {
        let frags = split_fragments("mailto:me@x");
        assert!(
            frags.iter().all(|f| f.kind != FragKind::Mention),
            "{frags:?}"
        );
    }

    #[test]
    fn a_mention_after_an_opening_bracket_is_recognized() {
        let frags = split_fragments("(@bob)");
        assert_eq!(
            kinds(&frags),
            vec![
                (FragKind::Text, "("),
                (FragKind::Mention, "@bob"),
                (FragKind::Text, ")"),
            ]
        );
    }

    #[test]
    fn shortcodes_match_flush_against_text() {
        let frags = split_fragments("nice:_wave:done");
        assert_eq!(
            kinds(&frags),
            vec![
                (FragKind::Text, "nice"),
                (FragKind::Shortcode, ":_wave:"),
                (FragKind::Text, "done"),
            ]
        );
    }

    #[test]
    fn an_unclosed_colon_is_plain_text() {
        let frags = split_fragments("ratio 3:4 here");
        assert_eq!(kinds(&frags), vec![(FragKind::Text, "ratio 3:4 here")]);
    }

    #[test]
    fn urls_are_recognized_at_word_starts() {
        let frags = split_fragments("see https://example.com/x and www.example.org now");
        assert_eq!(
            kinds(&frags),
            vec![
                (FragKind::Text, "see "),
                (FragKind::Url, "https://example.com/x"),
                (FragKind::Text, " and "),
                (FragKind::Url, "www.example.org"),
                (FragKind::Text, " now"),
            ]
        );
    }

    #[test]
    fn a_url_inside_a_word_is_not_a_url() {
        let frags = split_fragments("nothttps://example.com");
        assert_eq!(
            kinds(&frags),
            vec![(FragKind::Text, "nothttps://example.com")]
        );
    }

    #[test]
    fn emoji_clusters_are_kept_whole() {
        // A ZWJ family sequence is one cluster and must stay one fragment.
        let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}";
        let frags = split_fragments(&format!("hi {family}!"));
        assert_eq!(
            kinds(&frags),
            vec![
                (FragKind::Text, "hi "),
                (FragKind::Emoji, family),
                (FragKind::Text, "!"),
            ]
        );
    }

    #[test]
    fn adjacent_text_is_coalesced() {
        let frags = split_fragments("plain words only");
        assert_eq!(frags.len(), 1);
        assert_eq!(frags[0].kind, FragKind::Text);
    }

    // -- tier ladder --------------------------------------------------------

    #[test]
    fn the_tier_ladder_is_monotonic_over_six_steps() {
        assert_eq!(tier_color(0), ChipTone::Accent);
        assert_eq!(tier_color(1), ChipTone::Accent);
        assert_eq!(tier_color(2), ChipTone::AccentSuccessMix);
        assert_eq!(tier_color(3), ChipTone::Success);
        assert_eq!(tier_color(4), ChipTone::Success);
        assert_eq!(tier_color(5), ChipTone::Warning);
        assert_eq!(tier_color(6), ChipTone::Warning);
        assert_eq!(tier_color(7), ChipTone::WarningErrorMix);
        assert_eq!(tier_color(8), ChipTone::WarningErrorMix);
        assert_eq!(tier_color(9), ChipTone::Error);
        assert_eq!(tier_color(11), ChipTone::Error);
        assert_eq!(tier_color(12), ChipTone::Accent);
    }

    // -- rendering ----------------------------------------------------------

    #[test]
    fn no_rendered_line_exceeds_the_width() {
        let msg = message("the quick brown fox jumps over the lazy dog again and again and again");
        for width in [8u16, 20, 30, 47, 80] {
            let lines = render_message(&msg, width, &RenderOpts::default());
            for line in &lines {
                assert!(
                    line_width(line) <= usize::from(width),
                    "line {:?} wider than {width}",
                    joined(std::slice::from_ref(line))
                );
            }
        }
    }

    #[test]
    fn continuation_rows_are_indented_by_the_prefix_width() {
        let msg = message("word ".repeat(20).trim_end());
        let lines = render_message(&msg, 30, &RenderOpts::default());
        assert!(lines.len() > 1, "expected wrapping at width 30");
        // Prefix: "--:-- " (6) + "Someone" (7) + ": " (2) = 15 cells.
        let text = joined(&lines);
        for row in text.lines().skip(1) {
            assert!(
                row.starts_with(&" ".repeat(15)),
                "continuation row {row:?} lacks the 15-cell indent"
            );
        }
    }

    #[test]
    fn a_sixty_char_word_wraps_per_grapheme() {
        let long = "x".repeat(60);
        let msg = message(&long);
        let lines = render_message(&msg, 24, &RenderOpts::default());
        for line in &lines {
            assert!(line_width(line) <= 24);
        }
        // Every grapheme survives the split.
        let text = joined(&lines).replace([' ', '\n'], "");
        assert_eq!(text.matches('x').count(), 60);
    }

    #[test]
    fn wide_graphemes_are_measured_in_cells_not_chars() {
        // CJK is two cells per glyph; a width-20 row (15 of which are the
        // prefix) fits only a couple per continuation row, so any per-char
        // (rather than per-cell) accounting would overflow immediately.
        let msg = message(&"漢".repeat(30));
        let lines = render_message(&msg, 20, &RenderOpts::default());
        for line in &lines {
            assert!(
                line_width(line) <= 20,
                "CJK row overflows: {:?}",
                joined(std::slice::from_ref(line))
            );
        }
        let text = joined(&lines).replace([' ', '\n'], "");
        assert_eq!(text.chars().filter(|c| *c == '漢').count(), 30);
    }

    #[test]
    fn a_deleted_message_never_shows_its_original_text() {
        let mut msg = message("the secret original words");
        msg.deleted = true;
        let lines = render_message(&msg, 80, &RenderOpts::default());
        let text = joined(&lines);
        assert!(!text.contains("secret"), "deleted text leaked: {text}");
        assert!(
            !text.contains("original words"),
            "deleted text leaked: {text}"
        );
        assert!(text.contains("[message deleted]"));
    }

    #[test]
    fn a_notice_gets_the_notice_prefix() {
        let mut msg = message("emote-only mode is on");
        msg.kind = MessageKind::Notice;
        let text = joined(&render_message(&msg, 80, &RenderOpts::default()));
        assert!(text.contains("[notice] emote-only mode is on"), "{text}");
    }

    #[test]
    fn a_paid_message_carries_an_amount_chip() {
        let mut msg = message("take my money");
        msg.kind = MessageKind::Paid;
        msg.meta = Some(PlatformMeta::YouTube(YouTubeMeta {
            raw_type: "superChatEvent".into(),
            paid: Some(PaidAmount {
                micros: 5_000_000,
                currency: "USD".into(),
                display: "$5.00".into(),
                tier: 3,
            }),
            membership: None,
        }));
        let lines = render_message(&msg, 80, &RenderOpts::default());
        let text = joined(&lines);
        assert!(text.contains(" $5.00 "), "amount chip missing: {text}");
        // The chip precedes the body.
        assert!(
            text.find("$5.00").unwrap_or(usize::MAX) < text.find("take my money").unwrap_or(0),
            "{text}"
        );
        let chip = lines[0]
            .spans
            .iter()
            .find(|s| s.content.contains("$5.00"))
            .expect("the chip span exists (asserted by the contains above)");
        assert_eq!(chip.style.bg, Some(ChipTone::Success.color()));
        assert!(chip.style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn a_membership_message_carries_a_chip() {
        let mut msg = message("");
        msg.kind = MessageKind::Membership;
        msg.meta = Some(PlatformMeta::YouTube(YouTubeMeta {
            raw_type: "newSponsorEvent".into(),
            paid: None,
            membership: Some(MembershipDetails {
                kind: MembershipKind::New,
                level: String::new(),
                months: 0,
                gift_count: 0,
            }),
        }));
        let text = joined(&render_message(&msg, 80, &RenderOpts::default()));
        assert!(text.contains("★ new member"), "{text}");
    }

    #[test]
    fn an_action_renders_with_a_star_prefix_and_no_colon() {
        let mut msg = message("does a little dance");
        msg.kind = MessageKind::Action;
        let text = joined(&render_message(&msg, 80, &RenderOpts::default()));
        assert!(text.contains("* Someone does a little dance"), "{text}");
        assert!(!text.contains("Someone:"), "{text}");
    }

    #[test]
    fn timestamps_render_as_placeholder_when_unknown_and_hide_when_off() {
        let msg = message("hello");
        let with = joined(&render_message(&msg, 80, &RenderOpts::default()));
        assert!(with.starts_with("--:-- "), "{with}");
        let opts = RenderOpts {
            timestamps: false,
            ..RenderOpts::default()
        };
        let without = joined(&render_message(&msg, 80, &opts));
        assert!(!without.contains("--:--"), "{without}");
    }

    #[test]
    fn badge_modes_switch_between_glyphs_labels_and_nothing() {
        let mut msg = message("hi");
        msg.author.badges = vec![Badge {
            set: "moderator".into(),
            id: "1".into(),
            info: String::new(),
        }];
        let glyph = joined(&render_message(&msg, 80, &RenderOpts::default()));
        assert!(glyph.contains("⚔ "), "{glyph}");
        let text_mode = RenderOpts {
            badge_mode: BadgeMode::Text,
            ..RenderOpts::default()
        };
        let text = joined(&render_message(&msg, 80, &text_mode));
        assert!(text.contains("[moderator] "), "{text}");
        let off = RenderOpts {
            badge_mode: BadgeMode::Off,
            ..RenderOpts::default()
        };
        let none = joined(&render_message(&msg, 80, &off));
        assert!(
            !none.contains('⚔') && !none.contains("[moderator]"),
            "{none}"
        );
    }

    #[test]
    fn mention_url_and_shortcode_frags_are_styled() {
        let mut msg = message("hey @alice see https://x.example :wave:");
        msg.meta = Some(PlatformMeta::Twitch(TwitchMeta::default()));
        let lines = render_message(&msg, 120, &RenderOpts::default());
        let find = |needle: &str| {
            lines[0]
                .spans
                .iter()
                .find(|s| s.content.contains(needle))
                .unwrap_or_else(|| panic!("span {needle:?} missing"))
                .style
        };
        assert_eq!(find("@alice").fg, Some(ACCENT));
        assert!(find("@alice").add_modifier.contains(Modifier::BOLD));
        assert!(find("https://x.example")
            .add_modifier
            .contains(Modifier::UNDERLINED));
        assert_eq!(find(":wave:").fg, Some(SUCCESS));
    }

    #[test]
    fn widths_below_the_minimum_are_raised() {
        let msg = message("tiny");
        // Must not panic or loop; rendered as if width were 8.
        let lines = render_message(&msg, 1, &RenderOpts::default());
        for line in &lines {
            assert!(line_width(line) <= 8);
        }
    }
}
