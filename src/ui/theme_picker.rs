//! The theme picker: a full-screen list of every palette, previewed live.
//!
//! Opened with `ctrl+t`. Moving the selection changes the theme *immediately*,
//! so the whole interface behind the list redraws in the palette under the
//! cursor. That is the entire point: colours cannot be judged from nine
//! swatches in a row, only from the thing you actually spend hours looking at.
//! `Enter` keeps the previewed theme and writes it to the config file, `Esc`
//! puts back whatever was active before the picker opened.
//!
//! It takes the whole screen rather than docking as a strip under the chat,
//! because a palette judged through a chat pane that has been squeezed to half
//! its width is a palette judged badly.

use ratatui::prelude::*;
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};

use crate::theme::{self, Palette};

/// The name of the entry that previews the user's own `[appearance.custom_theme]`.
pub const CUSTOM_ENTRY: &str = "custom";

/// The picker's state. `None` in `App` means it is closed.
#[derive(Debug, Clone)]
pub struct ThemePicker {
    /// Which entry the cursor is on, as an index into [`entries`].
    pub selected: usize,
    /// The palette that was active when the picker opened, put back by `Esc`.
    ///
    /// The palette itself rather than its name: a `custom` entry cannot be
    /// looked up again by name, and cancelling has to restore what the user
    /// was actually looking at.
    pub original_palette: Palette,
    /// Set when saving the choice failed, so the reason can be shown instead
    /// of the picker silently doing nothing.
    pub save_error: Option<String>,
}

/// Every selectable entry: the built-in presets in their table order, then a
/// `custom` entry that previews whatever the config's custom colours hold.
pub fn entries() -> Vec<String> {
    theme::preset_names()
        .into_iter()
        .map(str::to_string)
        .chain(std::iter::once(CUSTOM_ENTRY.to_string()))
        .collect()
}

impl ThemePicker {
    /// Open the picker with the cursor on the theme currently in use.
    pub fn open(current_name: &str, current_palette: &Palette) -> Self {
        let entries = entries();
        let selected = entries
            .iter()
            .position(|name| name.eq_ignore_ascii_case(current_name.trim()))
            .unwrap_or(0);
        Self {
            selected,
            original_palette: current_palette.clone(),
            save_error: None,
        }
    }

    /// The name of the entry under the cursor.
    pub fn selected_name(&self) -> String {
        entries()
            .get(self.selected)
            .cloned()
            .unwrap_or_else(|| theme::DEFAULT_PRESET.to_string())
    }

    /// Move the cursor, wrapping at both ends so a long list can be reached
    /// from either direction.
    pub fn move_by(&mut self, delta: isize) {
        let count = entries().len() as isize;
        if count == 0 {
            return;
        }
        self.selected = (self.selected as isize + delta).rem_euclid(count) as usize;
    }

    pub fn move_to(&mut self, index: usize) {
        let count = entries().len();
        if index < count {
            self.selected = index;
        }
    }

    /// The last entry, for the `End` key.
    pub fn last_index(&self) -> usize {
        entries().len().saturating_sub(1)
    }
}

/// Draw the picker over the whole area.
///
/// `custom` is the palette the config's `[appearance.custom_theme]` holds. It
/// has to be passed in because the `custom` entry is the one row whose
/// swatches cannot be looked up from the built-in table.
pub fn draw(frame: &mut Frame, area: Rect, picker: &ThemePicker, custom: &Palette) {
    let sk = theme::skin();
    frame.render_widget(Clear, area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(sk.accent))
        .style(Style::new().bg(sk.canvas))
        .title(" Themes ");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let rows = Layout::vertical([
        Constraint::Length(2), // the explanation
        Constraint::Min(0),    // the list
        Constraint::Length(1), // the key hints
    ])
    .horizontal_margin(1)
    .split(inner);

    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(
                "Moving the selection applies the theme straight away — look past this list.",
                Style::new().fg(sk.foreground),
            )),
            Line::from(Span::styled(
                format!(
                    "{} built-in palettes, plus your own under [appearance.custom_theme].",
                    theme::preset_names().len()
                ),
                Style::new().fg(sk.muted),
            )),
        ]),
        rows[0],
    );

    draw_list(frame, rows[1], picker, custom);

    let hints = match &picker.save_error {
        Some(error) => Line::from(Span::styled(
            format!("could not save: {error}"),
            Style::new().fg(sk.error).add_modifier(Modifier::BOLD),
        )),
        None => Line::from(Span::styled(
            "↑/↓ or j/k preview   enter keep and save   esc cancel",
            Style::new().fg(sk.muted),
        )),
    };
    frame.render_widget(Paragraph::new(hints), rows[2]);
}

/// One swatch block. Two cells wide so a colour is visible rather than
/// implied, and so the nine columns line up down the page.
const SWATCH: &str = "██";

fn draw_list(frame: &mut Frame, area: Rect, picker: &ThemePicker, custom: &Palette) {
    let sk = theme::skin();
    let names = entries();
    if area.height == 0 || names.is_empty() {
        return;
    }

    // Keep the cursor in view by scrolling the window rather than the list:
    // the selection stays where the eye already is instead of the whole page
    // jumping when it reaches the bottom.
    let height = area.height as usize;
    let first = picker.selected.saturating_sub(height.saturating_sub(1) / 2);
    let first = first.min(names.len().saturating_sub(height.min(names.len())));

    let widest = names.iter().map(String::len).max().unwrap_or(0);
    let presets = theme::presets();

    let lines: Vec<Line> = names
        .iter()
        .enumerate()
        .skip(first)
        .take(height)
        .map(|(index, name)| {
            let palette = if name == CUSTOM_ENTRY {
                custom.clone()
            } else {
                presets.get(name).cloned().unwrap_or_default()
            };
            let selected = index == picker.selected;

            let mut spans = vec![
                Span::styled(
                    if selected { "▸ " } else { "  " },
                    Style::new().fg(sk.accent),
                ),
                Span::styled(
                    format!("{name:<widest$}  "),
                    if selected {
                        Style::new().fg(sk.foreground).add_modifier(Modifier::BOLD)
                    } else {
                        Style::new().fg(sk.muted)
                    },
                ),
            ];
            // The swatch strip: every entry draws all nine roles in the same
            // order, so the columns can be read down the page as well as
            // across — which is how you spot the one theme with a green
            // accent without reading a single name.
            for role in theme::ROLES {
                spans.push(Span::styled(
                    SWATCH,
                    Style::new().fg(theme::color(palette.role(role))),
                ));
            }
            let mut line = Line::from(spans);
            if selected {
                line = line.style(Style::new().bg(sk.selection));
            }
            line
        })
        .collect();

    frame.render_widget(Paragraph::new(lines), area);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_list_holds_every_preset_plus_the_custom_entry() {
        let names = entries();
        assert_eq!(names.len(), theme::preset_names().len() + 1);
        assert_eq!(names.last().map(String::as_str), Some(CUSTOM_ENTRY));
    }

    #[test]
    fn opening_the_picker_lands_on_the_theme_already_in_use() {
        let picker = ThemePicker::open("nord", &theme::default_palette());
        assert_eq!(picker.selected_name(), "nord");
    }

    #[test]
    fn opening_on_an_unknown_theme_lands_on_the_first_entry() {
        let picker = ThemePicker::open("no-such-theme", &theme::default_palette());
        assert_eq!(picker.selected, 0);
    }

    #[test]
    fn the_cursor_wraps_at_both_ends() {
        let mut picker = ThemePicker::open(theme::DEFAULT_PRESET, &theme::default_palette());
        picker.move_to(0);
        picker.move_by(-1);
        assert_eq!(picker.selected, picker.last_index());
        picker.move_by(1);
        assert_eq!(picker.selected, 0);
    }

    /// Every entry must resolve to a palette that can actually be drawn —
    /// including the `custom` one, which is the only entry whose colours the
    /// program did not author.
    #[test]
    fn every_entry_previews_a_complete_palette() {
        let custom = theme::default_palette();
        for name in entries() {
            let (palette, _) = theme::resolve(&name, &custom);
            for role in theme::ROLES {
                assert!(
                    !palette.role(role).is_empty(),
                    "entry {name} has no {role} colour"
                );
            }
        }
    }
}
