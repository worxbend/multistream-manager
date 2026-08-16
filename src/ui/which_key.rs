//! The which-key popup: the keys that could come next, shown rather than
//! remembered.
//!
//! A *chord* here is a run of key presses that together mean one thing —
//! press the leader (space), then `o`, then `s`, and the stream starts. That
//! shape is borrowed from Neovim, and so is this panel: it is a copy of the
//! idea behind Neovim's `which-key` plugin.
//!
//! The reason it exists is discoverability. A terminal program with sixty
//! bindings and no popup is a program you have to be *taught* — somebody has
//! to hand you a list, or you have to go and read the documentation, before
//! you can do anything beyond the two keys you happened to guess. With the
//! popup, pressing the leader and pausing answers the question directly:
//! here are the letters that mean something right now, and here is what each
//! of them leads to. The keys teach themselves, in the order you happen to
//! want them.
//!
//! Two views live here:
//!
//! * [`draw`] is the popup proper — the choices that follow a partly typed
//!   chord, docked to the bottom of the screen so the interface you were
//!   looking at stays visible above it.
//! * [`draw_all`] is the "show me everything" screen — every binding that
//!   applies, grouped by subject, taking the whole area.
//!
//! Nothing in this file holds state or reads a key. It is handed a keymap and
//! a rectangle and it draws; deciding *when* to show the popup belongs to the
//! code that owns the pending chord.

use ratatui::prelude::*;
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};

use crate::keys::{write_chord, Context, Key, Keymap};
use crate::theme;

/// The gap between one column of choices and the next, in cells.
///
/// Two spaces rather than one: a single space between `q → quit` and the next
/// key reads as part of the label, and the eye loses the column boundary.
const COLUMN_GAP: usize = 2;

/// The narrowest a column may be squeezed before a second column stops being
/// worth having. Below this a key and the beginning of its label do not fit
/// together, and two half-legible columns are worse than one readable one.
const MINIMUM_COLUMN_WIDTH: usize = 12;

/// The most columns to spread the choices over.
///
/// Three is a limit found by looking rather than derived: past that the eye
/// has to scan too far sideways to find a single letter, which is the one
/// thing this panel exists to make quick.
const MAXIMUM_COLUMNS: usize = 3;

/// The arrow between a key and what it does.
const ARROW: &str = " → ";

/// Draw the popup for a partly typed chord, docked to the bottom of `area`.
///
/// `prefix` is what has been pressed so far — `[space]` after the leader,
/// `[space, o]` once `o` has followed it. The panel lists every key that
/// could come next in that context.
///
/// It is docked to the bottom, like the command palette, for the same reason
/// the palette is: whatever prompted you to reach for a key is usually still
/// on screen, and a popup in the middle would cover it.
pub fn draw(frame: &mut Frame, area: Rect, keymap: &Keymap, context: Context, prefix: &[Key]) {
    // A zero-sized area is not a bug to shout about — a terminal genuinely
    // can be dragged down to nothing — so there is simply nothing to draw.
    if area.width == 0 || area.height == 0 {
        return;
    }

    let sk = theme::skin();
    let choices = keymap.continuations(context, prefix);

    // The panel's own borders and its two fixed lines (header and footer) are
    // charged for before any choices are, so the height it asks for is the
    // height it actually needs.
    const CHROME: u16 = 4;

    // Half the area at most: the popup is a hint over the interface, not a
    // replacement for it, and covering more than half hides the thing the key
    // was going to act on.
    let ceiling = (area.height / 2).max(1);

    // The layout is worked out against the width the panel will have *inside*
    // its border, which is two cells narrower than the area.
    let inner_width = area.width.saturating_sub(2) as usize;
    let plan = Plan::for_width(&choices, inner_width);

    // At least one content row even when there is nothing to list, so the
    // "nothing bound under this key" line has somewhere to go.
    let content_rows = plan.rows.max(1);
    let wanted = CHROME.saturating_add(content_rows as u16);
    let height = wanted.min(ceiling).min(area.height);

    let panel = Rect {
        x: area.x,
        y: area.y + area.height - height,
        width: area.width,
        height,
    };

    // `Clear` blanks whatever the panel sits on top of. Without it the
    // interface underneath shows through the gaps between the words.
    frame.render_widget(Clear, panel);

    let typed = write_chord(prefix, keymap.leader);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(sk.accent))
        .style(Style::new().bg(sk.canvas))
        .title(format!(" {typed} "));
    let inner = block.inner(panel);
    frame.render_widget(block, panel);

    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let rows = Layout::vertical([
        Constraint::Length(1), // what has been typed, and how many choices follow
        Constraint::Min(0),    // the choices themselves
        Constraint::Length(1), // how to get out
    ])
    .split(inner);

    let header = Line::from(vec![
        Span::styled(
            typed,
            Style::new().fg(sk.accent).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("  {}", describe_count(choices.len())),
            Style::new().fg(sk.muted),
        ),
    ]);
    frame.render_widget(Paragraph::new(header), rows[0]);

    if choices.is_empty() {
        // A prefix with nothing under it can happen when a config unbinds
        // everything below a key. Saying so beats an empty box that looks
        // like a drawing fault.
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "nothing bound under this key",
                Style::new().fg(sk.muted),
            ))),
            rows[1],
        );
    } else {
        let entries: Vec<Entry> = choices.iter().map(Entry::from_continuation).collect();
        draw_columns(frame, rows[1], &entries, &plan);
    }

    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "esc cancel",
            Style::new().fg(sk.muted),
        ))),
        rows[2],
    );
}

/// One thing shown in the popup: the key to press, what it leads to, and
/// whether that is a group or an action.
///
/// Pulled out of [`crate::keys::Continuation`] so the drawing code below can
/// be shared with [`draw_all`], which has no continuations to hand.
struct Entry {
    key: String,
    label: String,
    /// Groups are drawn in the accent colour so a branch and a leaf can be
    /// told apart at a glance — pressing one does something, pressing the
    /// other opens another list, and confusing the two is the popup's easiest
    /// mistake to make.
    is_group: bool,
}

impl Entry {
    fn from_continuation(continuation: &crate::keys::Continuation) -> Self {
        Self {
            key: continuation.key.write(),
            // The label already carries the leading `+` for a group, which is
            // the mark Neovim's plugin uses and the one people expect.
            label: continuation.label(),
            is_group: continuation.is_group(),
        }
    }

    /// How many cells this entry needs on one line.
    fn width(&self) -> usize {
        self.key.chars().count() + ARROW.chars().count() + self.label.chars().count()
    }
}

/// How the choices are to be arranged: how many columns, how wide each, and
/// therefore how many rows are needed.
struct Plan {
    columns: usize,
    column_width: usize,
    rows: usize,
}

impl Plan {
    /// Work out an arrangement that fits `width` cells across.
    ///
    /// The number of columns is derived from the widest entry rather than
    /// fixed, because the entries vary a great deal — `q → quit` is nine
    /// cells and `<PageDown>` with a long description is over thirty. Sizing
    /// every column to the widest entry keeps the arrow in a straight line
    /// down the page, which is what makes the list scannable.
    fn for_width(choices: &[crate::keys::Continuation], width: usize) -> Self {
        if choices.is_empty() || width == 0 {
            return Self {
                columns: 1,
                column_width: width,
                rows: 0,
            };
        }

        let widest = choices
            .iter()
            .map(|choice| Entry::from_continuation(choice).width())
            .max()
            .unwrap_or(0)
            .max(1);

        // How many columns of the widest entry, plus the gaps between them,
        // fit across. Solved directly rather than by trying each count: a
        // column costs `widest + COLUMN_GAP` and the last one is not followed
        // by a gap.
        let mut columns = (width + COLUMN_GAP) / (widest + COLUMN_GAP);
        columns = columns.clamp(1, MAXIMUM_COLUMNS).min(choices.len());

        // Never let the arithmetic produce columns too narrow to read. This
        // bites on a wide terminal showing very short entries, where the
        // division above would happily allow more columns than the eye wants.
        while columns > 1 {
            let candidate = (width - (columns - 1) * COLUMN_GAP) / columns;
            if candidate >= MINIMUM_COLUMN_WIDTH {
                break;
            }
            columns -= 1;
        }

        let column_width = if columns > 1 {
            (width - (columns - 1) * COLUMN_GAP) / columns
        } else {
            width
        };

        // Ceiling division: an odd count over two columns still needs the
        // extra row.
        let rows = choices.len().div_ceil(columns);

        Self {
            columns,
            column_width,
            rows,
        }
    }
}

/// Lay entries out in columns, filling down each column before moving right.
///
/// Down-then-across rather than across-then-down because the keys arrive
/// sorted, and reading a sorted list down a column is how every other list of
/// keys is read.
fn draw_columns(frame: &mut Frame, area: Rect, entries: &[Entry], plan: &Plan) {
    if area.width == 0 || area.height == 0 || entries.is_empty() {
        return;
    }

    let sk = theme::skin();
    let visible_rows = (area.height as usize).min(plan.rows);
    if visible_rows == 0 {
        return;
    }

    let mut lines: Vec<Line> = Vec::with_capacity(visible_rows);
    for row in 0..visible_rows {
        let mut spans: Vec<Span> = Vec::new();
        // Cells used so far, tracked so the line can be cut short rather than
        // trusting the widths to add up — a stray wide character would
        // otherwise push the last column past the edge.
        let mut used = 0usize;

        for column in 0..plan.columns {
            // Down each column in turn: entry `n` of column `c` is at index
            // `c * rows + n` in the sorted list.
            let index = column * plan.rows + row;
            let Some(entry) = entries.get(index) else {
                continue;
            };

            if column > 0 {
                let gap = COLUMN_GAP.min(area.width as usize - used.min(area.width as usize));
                if gap == 0 {
                    break;
                }
                spans.push(Span::raw(" ".repeat(gap)));
                used += gap;
            }

            let remaining = (area.width as usize).saturating_sub(used);
            if remaining == 0 {
                break;
            }
            let cell_width = plan.column_width.min(remaining);

            let key = truncate(&entry.key, cell_width);
            let key_width = key.chars().count();
            spans.push(Span::styled(
                key,
                Style::new().fg(sk.accent).add_modifier(Modifier::BOLD),
            ));
            used += key_width;

            let after_key = cell_width.saturating_sub(key_width);
            let arrow = truncate(ARROW, after_key);
            let arrow_width = arrow.chars().count();
            spans.push(Span::styled(arrow, Style::new().fg(sk.muted)));
            used += arrow_width;

            let label_room = after_key.saturating_sub(arrow_width);
            let label = truncate(&entry.label, label_room);
            let label_width = label.chars().count();
            spans.push(Span::styled(
                label,
                if entry.is_group {
                    Style::new().fg(sk.accent)
                } else {
                    Style::new().fg(sk.foreground)
                },
            ));
            used += label_width;

            // Pad out to the full column width so the next column starts
            // where the one above it did.
            let padding = cell_width.saturating_sub(key_width + arrow_width + label_width);
            if padding > 0 {
                spans.push(Span::raw(" ".repeat(padding)));
                used += padding;
            }
        }

        lines.push(Line::from(spans));
    }

    frame.render_widget(Paragraph::new(lines), area);
}

/// Cut a string to at most `width` cells, on a character boundary.
///
/// Cutting rather than wrapping: a binding list that reflows is harder to
/// read than one that is clipped, and the panel promises never to draw past
/// its own edge.
fn truncate(text: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    if text.chars().count() <= width {
        return text.to_string();
    }
    text.chars().take(width).collect()
}

/// The header's count, written as a sentence rather than a bare number.
fn describe_count(count: usize) -> String {
    match count {
        0 => "nothing follows".to_string(),
        1 => "1 choice".to_string(),
        many => format!("{many} choices"),
    }
}

/// Draw every binding that applies in `context`, over the whole of `area`.
///
/// This is the answer to "show me all of it" — the screen somebody opens once
/// to see the shape of the keymap, as opposed to the popup, which answers one
/// question in the middle of doing something else. Bindings from `context`
/// and from [`Context::Global`] are both shown, because both are in force,
/// and they are grouped by subject so related keys sit together rather than
/// being scattered by alphabetical accident.
pub fn draw_all(frame: &mut Frame, area: Rect, keymap: &Keymap, context: Context) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let sk = theme::skin();
    frame.render_widget(Clear, area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(sk.accent))
        .style(Style::new().bg(sk.canvas))
        .title(" Keys ");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let rows = Layout::vertical([
        Constraint::Min(0),    // the bindings
        Constraint::Length(1), // how to get out
    ])
    .horizontal_margin(1)
    .split(inner);

    let mut bindings: Vec<crate::keys::Binding> = keymap
        .all()
        .into_iter()
        .filter(|binding| binding.context == context || binding.context == Context::Global)
        .collect();

    // Sorted by group first so the headings come out in a stable order, then
    // by the description so the rows under a heading read alphabetically.
    // `Keymap` already returns a stable order; this re-groups it without
    // introducing a new source of run-to-run variation.
    bindings.sort_by(|left, right| {
        left.action
            .group()
            .cmp(right.action.group())
            .then_with(|| left.action.describe().cmp(right.action.describe()))
            .then_with(|| left.chord.cmp(&right.chord))
    });

    // The chord column is sized to the longest chord present, so the
    // descriptions line up in one straight column down the page. Capped at a
    // third of the width so one unusually long binding cannot squeeze the
    // descriptions out of existence.
    let available = rows[0].width as usize;
    let widest_chord = bindings
        .iter()
        .map(|binding| write_chord(&binding.chord, keymap.leader).chars().count())
        .max()
        .unwrap_or(0)
        .min((available / 3).max(1));

    let mut lines: Vec<Line> = Vec::new();
    let mut current_group: Option<&str> = None;
    for binding in &bindings {
        let group = binding.action.group();
        if current_group != Some(group) {
            // A blank line before every heading but the first, so the groups
            // read as blocks rather than as one long list with bold rows in it.
            if current_group.is_some() {
                lines.push(Line::default());
            }
            lines.push(Line::from(Span::styled(
                group.to_string(),
                Style::new().fg(sk.accent).add_modifier(Modifier::BOLD),
            )));
            current_group = Some(group);
        }

        let chord = truncate(
            &write_chord(&binding.chord, keymap.leader),
            widest_chord.min(available),
        );
        let padding = widest_chord.saturating_sub(chord.chars().count());
        let used = chord.chars().count() + padding + COLUMN_GAP;
        let description = truncate(binding.action.describe(), available.saturating_sub(used));

        lines.push(Line::from(vec![
            Span::styled(chord, Style::new().fg(sk.accent)),
            Span::raw(" ".repeat(padding + COLUMN_GAP)),
            Span::styled(description, Style::new().fg(sk.foreground)),
        ]));
    }

    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            "no bindings",
            Style::new().fg(sk.muted),
        )));
    }

    // Clipped, not scrolled: this function has no state to hold a scroll
    // position in, and inventing one here would make it something the caller
    // has to drive.
    lines.truncate(rows[0].height as usize);
    frame.render_widget(Paragraph::new(lines), rows[0]);

    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "esc cancel",
            Style::new().fg(sk.muted),
        ))),
        rows[1],
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::{default_leader, parse_chord};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn chord(written: &str) -> Vec<Key> {
        parse_chord(written, default_leader()).expect("a valid chord")
    }

    /// Draw the popup into a terminal of the given size and read the screen
    /// back as text, which is what the assertions below inspect.
    fn render_popup(width: u16, height: u16, prefix: &str) -> String {
        let keymap = Keymap::default();
        let prefix = chord(prefix);
        render(width, height, |frame| {
            draw(frame, frame.area(), &keymap, Context::Global, &prefix)
        })
    }

    fn render_all(width: u16, height: u16, context: Context) -> String {
        let keymap = Keymap::default();
        render(width, height, |frame| {
            draw_all(frame, frame.area(), &keymap, context)
        })
    }

    fn render(width: u16, height: u16, mut body: impl FnMut(&mut Frame)) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height))
            .expect("the test backend never fails to initialise");
        terminal
            .draw(|frame| body(frame))
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
    fn the_popup_lists_the_keys_that_can_follow_the_prefix() {
        let screen = render_popup(90, 30, "<Leader>o");
        assert!(screen.contains('s'), "the OBS stream key should be shown");
        assert!(
            screen.contains("stream"),
            "each key should be shown with what it does:\n{screen}"
        );
    }

    #[test]
    fn the_popup_shows_the_chord_typed_so_far_and_how_many_choices_follow() {
        let screen = render_popup(90, 30, "<Leader>");
        assert!(
            screen.contains("<Leader>"),
            "the header should echo what was pressed:\n{screen}"
        );
        assert!(
            screen.contains("choices"),
            "the header should count the choices:\n{screen}"
        );
    }

    #[test]
    fn a_group_is_marked_with_a_plus_so_it_reads_as_a_branch() {
        let screen = render_popup(90, 30, "<Leader>");
        assert!(
            screen.contains("+obs"),
            "the OBS group should be marked as a group:\n{screen}"
        );
    }

    #[test]
    fn the_popup_offers_a_way_out() {
        let screen = render_popup(90, 30, "<Leader>");
        assert!(screen.contains("esc cancel"), "{screen}");
    }

    /// The popup is a hint over the interface, so it must leave the top of
    /// the screen alone rather than taking it all.
    #[test]
    fn the_popup_covers_no_more_than_half_the_area() {
        let screen = render_popup(90, 40, "<Leader>");
        let blank_top = screen.lines().take(20).all(|line| line.trim().is_empty());
        assert!(blank_top, "the top half should be untouched:\n{screen}");
    }

    /// Every line has to fit the terminal exactly. A column arithmetic slip
    /// would show up here as a line longer than the screen.
    #[test]
    fn the_columns_never_overflow_the_width() {
        for width in [20u16, 30, 45, 60, 80, 120, 200] {
            let screen = render_popup(width, 30, "<Leader>c");
            for line in screen.lines() {
                assert_eq!(
                    line.chars().count(),
                    width as usize,
                    "a line is the wrong width at {width} columns:\n{screen}"
                );
            }
        }
    }

    /// A wide terminal should use the width rather than leaving two thirds of
    /// it empty, which is the whole reason the layout is in columns.
    #[test]
    fn a_wide_terminal_gets_more_than_one_column() {
        let keymap = Keymap::default();
        let choices = keymap.continuations(Context::Global, &chord("<Leader>c"));
        let narrow = Plan::for_width(&choices, 30);
        let wide = Plan::for_width(&choices, 160);
        assert_eq!(narrow.columns, 1);
        assert!(
            wide.columns > 1,
            "160 cells should hold more than one column"
        );
        assert!(wide.columns <= MAXIMUM_COLUMNS);
        assert!(wide.rows < narrow.rows, "columns should shorten the list");
    }

    /// A config can unbind everything under a key, and the popup must say so
    /// rather than showing an empty box that looks broken.
    #[test]
    fn an_empty_continuation_list_is_handled() {
        let keymap = Keymap::default();
        let prefix = chord("zzz");
        let screen = render(60, 12, |frame| {
            draw(frame, frame.area(), &keymap, Context::Global, &prefix)
        });
        assert!(
            screen.contains("nothing bound"),
            "an empty list should be explained:\n{screen}"
        );
    }

    #[test]
    fn an_empty_prefix_is_handled() {
        let screen = render(60, 12, |frame| {
            let keymap = Keymap::default();
            draw(frame, frame.area(), &keymap, Context::Global, &[])
        });
        assert!(!screen.is_empty());
    }

    /// Terminals really can be dragged to nothing, and a panic in drawing
    /// takes the whole program with it.
    #[test]
    fn a_tiny_terminal_does_not_panic() {
        for (width, height) in [(1u16, 1u16), (1, 4), (4, 1), (2, 2), (3, 5), (10, 3)] {
            let _ = render_popup(width, height, "<Leader>");
            let _ = render_all(width, height, Context::Chat);
        }
    }

    /// A zero-height rectangle can be handed in by a layout that had no room
    /// left to give, so both entry points must survive it.
    #[test]
    fn a_zero_sized_area_draws_nothing_and_does_not_panic() {
        let keymap = Keymap::default();
        let prefix = chord("<Leader>");
        let screen = render(20, 6, |frame| {
            let empty = Rect {
                x: 0,
                y: 0,
                width: 0,
                height: 0,
            };
            draw(frame, empty, &keymap, Context::Global, &prefix);
            draw_all(frame, empty, &keymap, Context::Global);
        });
        assert!(
            screen.trim().is_empty(),
            "nothing should have been drawn:\n{screen}"
        );
    }

    #[test]
    fn the_full_listing_shows_bindings_grouped_by_subject() {
        let screen = render_all(100, 60, Context::Chat);
        assert!(screen.contains("chat"), "the chat group:\n{screen}");
        assert!(
            screen.contains("<Leader>"),
            "chords should be written the way the config writes them:\n{screen}"
        );
    }

    /// The listing shows what is in force where you are: a context's own
    /// bindings and the global ones, and nothing from another tab.
    #[test]
    fn the_full_listing_covers_the_context_and_the_global_bindings() {
        let screen = render_all(100, 200, Context::Obs);
        assert!(
            screen.contains("<C-p>"),
            "a global binding should appear:\n{screen}"
        );
    }

    #[test]
    fn the_full_listing_never_overflows_the_width() {
        for width in [10u16, 24, 40, 100] {
            let screen = render_all(width, 40, Context::Chat);
            for line in screen.lines() {
                assert_eq!(
                    line.chars().count(),
                    width as usize,
                    "a line is the wrong width at {width} columns:\n{screen}"
                );
            }
        }
    }

    #[test]
    fn truncation_cuts_rather_than_overflowing() {
        assert_eq!(truncate("abcdef", 3), "abc");
        assert_eq!(truncate("ab", 5), "ab");
        assert_eq!(truncate("ab", 0), "");
    }
}
