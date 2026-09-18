//! The `Style`/`Span`-building layer on top of `theme.rs`'s colour math.
//!
//! `theme.rs` deliberately knows nothing about ratatui's `Style` or `Span` —
//! it operates on [`Color`] and hex strings alone. Every screen still needs
//! the same handful of `Style`/`Span` idioms built from those colours (a
//! focused panel's border, a status badge, a zebra-striped row), so this
//! module holds them once, here, rather than each screen growing its own
//! slightly different copy.
//!
//! Every function here takes only values a caller already computes each
//! frame — a [`Skin`], a `bool`, a `Color`, a `usize`, a `&str` — and none of
//! them read or require anything from `App`.

use ratatui::style::{Modifier, Style};
use ratatui::text::Span;

use crate::theme::{self, Skin};

/// A hollow dot, for an "off"/"idle"/"offline" state paired with plain text —
/// every "on"/"active" counterpart state uses [`badge`] instead, which carries
/// its own colour and needs no separate glyph constant. Matches the glyph
/// already used for [`crate::health::Health::Idle`] and the privacy picker's
/// unselected options in `draw.rs`.
pub const DOT_HOLLOW: &str = "○";

/// The style a panel's border is drawn with, depending on whether that panel
/// currently has focus.
///
/// Factors out the ad hoc `Style::new().fg(if focused { sk.accent } else {
/// sk.border })` idiom that used to live at the combined-panel border in
/// `draw.rs`, so every screen with more than one simultaneously-visible panel
/// shares one rule for what "focused" looks like.
pub fn panel_border_style(focused: bool, sk: &Skin) -> Style {
    Style::new().fg(if focused { sk.accent } else { sk.border })
}

/// The style a panel's embedded title is drawn with, depending on whether
/// that panel currently has focus.
///
/// A focused panel's title is bold and the ordinary foreground colour, so it
/// reads as visibly brighter than an unfocused one's plain muted title — the
/// border alone is not always enough of a cue, especially for someone who is
/// not distinguishing the border's colour precisely.
pub fn panel_title_style(focused: bool, sk: &Skin) -> Style {
    if focused {
        Style::new().fg(sk.foreground).add_modifier(Modifier::BOLD)
    } else {
        Style::new().fg(sk.muted)
    }
}

/// A small filled pill rendered as owned spans: `label`, padded with one
/// space on each side, bold, with `tone` as the background.
///
/// The foreground is worked out from `tone` with
/// [`theme::contrast_text_for`] rather than assumed, so the label stays
/// readable against any tone under any of the built-in palettes — a bright
/// `warning` yellow needs dark text, a dark `error` red needs light text, and
/// this reads correctly either way without the caller having to know which.
pub fn badge(label: &str, tone: ratatui::style::Color, _sk: &Skin) -> Vec<Span<'static>> {
    // `_sk` is unused: the tone alone determines readable text via
    // `contrast_text_for`. Taking `&Skin` anyway keeps this call shape
    // consistent with `panel_border_style`, `panel_title_style` and
    // `zebra_style`, all of which do need it.
    vec![Span::styled(
        format!(" {label} "),
        Style::new()
            .bg(tone)
            .fg(theme::contrast_text_for(tone))
            .add_modifier(Modifier::BOLD),
    )]
}

/// The background a flat (non-widget-list) list row is drawn with, for a
/// subtle zebra stripe: a faint tint on odd rows, nothing on even rows.
///
/// Callers apply their own selection highlight afterward — on top of, not
/// instead of, this — so a selected row's own background always wins over
/// the stripe rather than the two competing.
pub fn zebra_style(row: usize, sk: &Skin) -> Style {
    if row % 2 == 1 {
        Style::new().bg(theme::blend_colors(sk.surface, sk.canvas, 0.5))
    } else {
        Style::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Color;

    fn skin() -> Skin {
        Skin::default()
    }

    #[test]
    fn panel_border_style_uses_accent_when_focused_and_border_otherwise() {
        let sk = skin();
        assert_eq!(panel_border_style(true, &sk).fg, Some(sk.accent));
        assert_eq!(panel_border_style(false, &sk).fg, Some(sk.border));
    }

    #[test]
    fn panel_title_style_is_bold_and_brighter_only_when_focused() {
        let sk = skin();
        let focused = panel_title_style(true, &sk);
        assert_eq!(focused.fg, Some(sk.foreground));
        assert!(focused.add_modifier.contains(Modifier::BOLD));

        let unfocused = panel_title_style(false, &sk);
        assert_eq!(unfocused.fg, Some(sk.muted));
        assert!(!unfocused.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn badge_pads_the_label_and_stays_readable_on_its_tone() {
        let sk = skin();
        let tone = Color::Rgb(255, 255, 0); // a bright tone: wants dark text
        let spans = badge("READY", tone, &sk);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].content, " READY ");
        assert_eq!(spans[0].style.bg, Some(tone));
        assert_eq!(spans[0].style.fg, Some(theme::contrast_text_for(tone)));
        assert!(spans[0].style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn zebra_style_stripes_odd_rows_only_and_selection_can_still_override_it() {
        let sk = skin();
        assert_eq!(zebra_style(0, &sk), Style::default());
        assert_eq!(zebra_style(2, &sk), Style::default());
        let odd = zebra_style(1, &sk);
        assert_eq!(
            odd.bg,
            Some(theme::blend_colors(sk.surface, sk.canvas, 0.5))
        );
    }
}
