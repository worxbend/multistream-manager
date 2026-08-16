//! The command palette: every action in the program, searchable by name.
//!
//! A terminal interface with enough keys becomes a program you have to have
//! been taught. `ctrl+y` toggles emote highlighting, `space a` shows the
//! activity column, `alt+w` swaps which half of the combined tab has the
//! keyboard — all reasonable, none guessable. The footer can only ever show a
//! handful of them, and a help screen you have to remember to open is not much
//! better than a manual.
//!
//! `ctrl+p` opens a list of everything, filtered as you type. Every entry
//! shows the key that runs it, so using the palette teaches you the key you
//! could have pressed, and after a while you stop needing the palette for the
//! things you do often.
//!
//! **The palette does not implement anything.** Each entry names the keys it
//! stands for, and choosing it replays exactly those keys through the normal
//! key handling. That is deliberate: an entry that reimplemented an action
//! would be free to drift away from what the key actually does — and a
//! discoverability feature that lies about the keys is worse than none. It
//! also means every entry is testable by asking whether replaying its keys
//! does anything at all.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::prelude::*;
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};

use crate::theme;

/// One entry in the palette.
pub struct Entry {
    /// What it does, in plain words.
    pub title: &'static str,
    /// The keys that do it, shown as a hint and replayed when it is chosen.
    /// More than one for a chord like `space a`.
    pub keys: &'static [Key],
    /// The key hint as it is written in the footer, which is not always
    /// derivable from `keys` (`q / ctrl+c`, `↑/↓`).
    pub shortcut: &'static str,
    /// Extra words that should match this entry when searching, for the times
    /// the title uses a word you would not have thought of.
    pub keywords: &'static [&'static str],
    /// Whether this action needs a chat to be open before it can do anything.
    ///
    /// Recorded rather than inferred so the test that replays every entry
    /// knows which ones legitimately do nothing on a fresh session with no
    /// chat connected yet, instead of having to treat "did nothing" as
    /// always acceptable.
    pub needs_chat: bool,
}

/// A key to replay, in a form that can be written down as a constant.
#[derive(Debug, Clone, Copy)]
pub struct Key {
    pub code: KeyCode,
    pub modifiers: KeyModifiers,
}

impl Key {
    const fn plain(code: KeyCode) -> Self {
        Self {
            code,
            modifiers: KeyModifiers::NONE,
        }
    }
    const fn ctrl(c: char) -> Self {
        Self {
            code: KeyCode::Char(c),
            modifiers: KeyModifiers::CONTROL,
        }
    }
    const fn alt(c: char) -> Self {
        Self {
            code: KeyCode::Char(c),
            modifiers: KeyModifiers::ALT,
        }
    }
    const fn char(c: char) -> Self {
        Self::plain(KeyCode::Char(c))
    }

    pub fn event(self) -> KeyEvent {
        KeyEvent::new(self.code, self.modifiers)
    }
}

/// Every action the palette offers.
///
/// Grouped roughly the way the interface is: getting around first, then the
/// stream, then chat, then appearance, then leaving.
pub const ENTRIES: &[Entry] = &[
    Entry {
        title: "Go to the Stream Info tab",
        keys: &[Key::alt('1')],
        shortcut: "alt+1",
        keywords: &["tab", "stream", "info", "dashboard", "settings"],
        needs_chat: false,
    },
    Entry {
        title: "Go to the Chat tab",
        keys: &[Key::alt('2')],
        shortcut: "alt+2",
        keywords: &["tab", "chat", "messages", "twitch", "youtube"],
        needs_chat: false,
    },
    Entry {
        title: "Go to the Combined tab",
        keys: &[Key::alt('3')],
        shortcut: "alt+3",
        keywords: &["tab", "combined", "both", "split", "everything"],
        needs_chat: false,
    },
    Entry {
        title: "Swap which half of the Combined tab has the keyboard",
        keys: &[Key::alt('w')],
        shortcut: "alt+w",
        keywords: &["combined", "focus", "swap", "switch", "half", "pane"],
        needs_chat: false,
    },
    Entry {
        title: "Show the message history",
        keys: &[Key::alt('m')],
        shortcut: "alt+m",
        keywords: &[
            "messages",
            "notifications",
            "history",
            "log",
            "toast",
            "what happened",
        ],
        needs_chat: false,
    },
    Entry {
        title: "Go to the OBS tab",
        keys: &[Key::alt('4')],
        shortcut: "alt+4",
        keywords: &[
            "tab", "obs", "studio", "scenes", "audio", "record", "stream",
        ],
        needs_chat: false,
    },
    Entry {
        title: "Refresh the live statistics now",
        keys: &[Key::char('r')],
        shortcut: "r",
        keywords: &["refresh", "poll", "viewers", "stats", "statistics"],
        needs_chat: false,
    },
    Entry {
        title: "Copy the Twitch stream key to the clipboard",
        keys: &[Key::char('y')],
        shortcut: "y",
        keywords: &["key", "stream key", "clipboard", "copy", "obs", "twitch"],
        needs_chat: false,
    },
    Entry {
        title: "Copy the YouTube stream key to the clipboard",
        keys: &[Key::char('Y')],
        shortcut: "Y",
        keywords: &["key", "stream key", "clipboard", "copy", "obs", "youtube"],
        needs_chat: false,
    },
    Entry {
        title: "Open the watch page in a browser",
        keys: &[Key::char('o')],
        shortcut: "o",
        keywords: &["open", "watch", "browser", "url", "link", "page"],
        needs_chat: false,
    },
    Entry {
        title: "Edit the stream title, category and tags",
        keys: &[Key::char('e')],
        shortcut: "e",
        keywords: &["edit", "title", "category", "tags", "form", "settings"],
        needs_chat: false,
    },
    Entry {
        title: "Focus the message box",
        keys: &[Key::char('i')],
        shortcut: "i",
        keywords: &["chat", "compose", "write", "send", "input", "type"],
        needs_chat: false,
    },
    Entry {
        title: "Search the chat",
        keys: &[Key::char('/')],
        shortcut: "/",
        keywords: &["chat", "search", "find", "grep"],
        needs_chat: true,
    },
    Entry {
        title: "Reconnect the chat",
        keys: &[Key::ctrl('r')],
        shortcut: "ctrl+r",
        keywords: &["chat", "reconnect", "connection", "retry", "dropped"],
        needs_chat: false,
    },
    Entry {
        title: "Open the emoji picker",
        keys: &[Key::ctrl('e')],
        shortcut: "ctrl+e",
        keywords: &["emoji", "emote", "picker", "insert", "smiley"],
        needs_chat: false,
    },
    Entry {
        title: "Clear every chat message filter",
        keys: &[Key::char('0')],
        shortcut: "0",
        keywords: &["filter", "filters", "reset", "clear", "all", "show"],
        needs_chat: true,
    },
    Entry {
        title: "Show only messages that mention you",
        keys: &[Key::char('1')],
        shortcut: "1",
        keywords: &["filter", "mentions", "highlight", "me"],
        needs_chat: true,
    },
    Entry {
        title: "Next chat",
        keys: &[Key::char(']')],
        shortcut: "]",
        keywords: &["chat", "next", "switch", "channel"],
        needs_chat: true,
    },
    Entry {
        title: "Previous chat",
        keys: &[Key::char('[')],
        shortcut: "[",
        keywords: &["chat", "previous", "back", "switch", "channel"],
        needs_chat: true,
    },
    Entry {
        title: "Widen the left chat pane",
        keys: &[Key::char('<')],
        shortcut: "<",
        keywords: &["pane", "resize", "wider", "narrower", "split", "layout"],
        needs_chat: false,
    },
    Entry {
        title: "Narrow the left chat pane",
        keys: &[Key::char('>')],
        shortcut: ">",
        keywords: &["pane", "resize", "wider", "narrower", "split", "layout"],
        needs_chat: false,
    },
    Entry {
        title: "Reset the pane sizes",
        keys: &[Key::char('=')],
        shortcut: "=",
        keywords: &["pane", "resize", "reset", "default", "layout"],
        needs_chat: false,
    },
    Entry {
        title: "Choose a theme",
        keys: &[Key::ctrl('t')],
        shortcut: "ctrl+t",
        keywords: &[
            "theme",
            "themes",
            "colour",
            "color",
            "colours",
            "palette",
            "appearance",
            "dark",
            "light",
        ],
        needs_chat: false,
    },
    Entry {
        title: "Change how much the interface animates",
        keys: &[Key::alt('a')],
        shortcut: "alt+a",
        keywords: &[
            "animation",
            "animations",
            "motion",
            "reduced",
            "off",
            "still",
            "accessibility",
        ],
        needs_chat: false,
    },
    Entry {
        title: "Show or hide the process telemetry",
        keys: &[Key::alt('t')],
        shortcut: "alt+t",
        keywords: &[
            "telemetry",
            "cpu",
            "memory",
            "fps",
            "frame rate",
            "performance",
            "status",
        ],
        needs_chat: false,
    },
    Entry {
        title: "Start or stop streaming in OBS",
        keys: &[Key::alt('4'), Key::char('s')],
        shortcut: "alt+4 then s",
        keywords: &[
            "obs",
            "stream",
            "streaming",
            "start",
            "stop",
            "go live",
            "broadcast",
        ],
        needs_chat: false,
    },
    Entry {
        title: "Start or stop recording in OBS",
        keys: &[Key::alt('4'), Key::char('r')],
        shortcut: "alt+4 then r",
        keywords: &["obs", "record", "recording", "start", "stop", "capture"],
        needs_chat: false,
    },
    Entry {
        title: "Mute or unmute every OBS audio input",
        keys: &[Key::alt('4'), Key::char('M')],
        shortcut: "alt+4 then M",
        keywords: &[
            "obs",
            "mute",
            "unmute",
            "audio",
            "microphone",
            "mic",
            "silence",
            "panic",
        ],
        needs_chat: false,
    },
    Entry {
        title: "Reconnect to OBS",
        keys: &[Key::alt('4'), Key::char('R')],
        shortcut: "alt+4 then R",
        keywords: &["obs", "reconnect", "connection", "retry", "dropped"],
        needs_chat: false,
    },
    Entry {
        title: "Quit",
        keys: &[Key::ctrl('c')],
        shortcut: "q / ctrl+c",
        keywords: &["quit", "exit", "close", "leave"],
        needs_chat: false,
    },
];

/// The palette's state. `None` in `App` means it is closed.
#[derive(Debug, Default, Clone)]
pub struct CommandPalette {
    /// What has been typed so far.
    pub query: String,
    /// Which of the *matching* entries is selected.
    pub selected: usize,
}

impl CommandPalette {
    /// The entries matching the current query, as indices into [`ENTRIES`].
    pub fn matches(&self) -> Vec<usize> {
        matches_for(&self.query)
    }

    /// The entry that would run if Enter were pressed now.
    pub fn chosen(&self) -> Option<&'static Entry> {
        self.matches()
            .get(self.selected)
            .map(|index| &ENTRIES[*index])
    }

    /// Move the selection, wrapping at both ends.
    pub fn move_by(&mut self, delta: isize) {
        let count = self.matches().len() as isize;
        if count == 0 {
            self.selected = 0;
            return;
        }
        self.selected = (self.selected as isize + delta).rem_euclid(count) as usize;
    }

    /// Type a character into the query.
    pub fn push(&mut self, c: char) {
        self.query.push(c);
        // A narrowed list has a different first entry, so the selection goes
        // back to the top rather than pointing at whatever happens to be at
        // the old index now.
        self.selected = 0;
    }

    pub fn backspace(&mut self) {
        self.query.pop();
        self.selected = 0;
    }
}

/// Which entries match `query`.
///
/// The rule is "all of the typed words appear somewhere in the entry", not a
/// fuzzy subsequence match. Typing `copy key` finds "Copy the Twitch stream
/// key to the clipboard" whichever order the words are in, and typing three
/// letters does not return two-thirds of the list.
fn matches_for(query: &str) -> Vec<usize> {
    let needles: Vec<String> = query
        .split_whitespace()
        .map(|word| word.to_ascii_lowercase())
        .collect();
    if needles.is_empty() {
        return (0..ENTRIES.len()).collect();
    }
    ENTRIES
        .iter()
        .enumerate()
        .filter(|(_, entry)| {
            let haystack = format!(
                "{} {} {}",
                entry.title.to_ascii_lowercase(),
                entry.shortcut.to_ascii_lowercase(),
                entry.keywords.join(" ")
            );
            needles.iter().all(|needle| haystack.contains(needle))
        })
        .map(|(index, _)| index)
        .collect()
}

/// Draw the palette over the bottom half of `area`.
///
/// The bottom, not the middle: what you were looking at when you opened it is
/// usually what you want the command to act on, so covering the top half would
/// hide the thing you are about to change.
pub fn draw(frame: &mut Frame, area: Rect, palette: &CommandPalette, chat_open: bool) {
    let sk = theme::skin();
    let matches = palette.matches();

    // As tall as it needs to be, up to half the screen.
    let wanted = matches.len().min(12) as u16 + 4;
    let height = wanted.min(area.height);
    if height < 4 || area.width < 20 {
        return;
    }
    let rect = Rect {
        x: area.x,
        y: area.y + area.height - height,
        width: area.width,
        height,
    };

    frame.render_widget(Clear, rect);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(sk.accent))
        .style(Style::new().bg(sk.surface))
        .title(" Commands ");
    let inner = block.inner(rect);
    frame.render_widget(block, rect);

    let rows = Layout::vertical([Constraint::Length(1), Constraint::Min(0)])
        .horizontal_margin(1)
        .split(inner);

    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("› ", Style::new().fg(sk.accent)),
            Span::styled(palette.query.clone(), Style::new().fg(sk.foreground)),
            Span::styled("▌", Style::new().fg(sk.accent)),
            Span::styled(
                if matches.is_empty() {
                    "   nothing matches".to_string()
                } else {
                    format!("   {} of {}", matches.len(), ENTRIES.len())
                },
                Style::new().fg(sk.muted),
            ),
        ])),
        rows[0],
    );

    let height = rows[1].height as usize;
    // Scroll the window so the selection stays visible in a long list.
    let first = palette
        .selected
        .saturating_sub(height.saturating_sub(1))
        .min(matches.len().saturating_sub(height.min(matches.len())));

    let widest = matches
        .iter()
        .map(|index| ENTRIES[*index].shortcut.chars().count())
        .max()
        .unwrap_or(0);

    let lines: Vec<Line> = matches
        .iter()
        .enumerate()
        .skip(first)
        .take(height)
        .map(|(position, index)| {
            let entry = &ENTRIES[*index];
            let selected = position == palette.selected;
            // An action that needs an open chat says so, rather than being
            // chosen and appearing to do nothing.
            let unavailable = entry.needs_chat && !chat_open;
            let title_color = match (selected, unavailable) {
                (_, true) => sk.muted,
                (true, false) => sk.foreground,
                (false, false) => sk.muted,
            };
            let mut line = Line::from(vec![
                Span::styled(
                    if selected { "▸ " } else { "  " },
                    Style::new().fg(sk.accent),
                ),
                Span::styled(
                    format!("{:<widest$}  ", entry.shortcut),
                    Style::new().fg(sk.accent),
                ),
                Span::styled(
                    entry.title,
                    if selected && !unavailable {
                        Style::new().fg(title_color).add_modifier(Modifier::BOLD)
                    } else {
                        Style::new().fg(title_color)
                    },
                ),
                Span::styled(
                    if unavailable {
                        "  (needs an open chat)"
                    } else {
                        ""
                    },
                    Style::new().fg(sk.warning),
                ),
            ]);
            if selected {
                line = line.style(Style::new().bg(sk.selection));
            }
            line
        })
        .collect();

    frame.render_widget(Paragraph::new(lines), rows[1]);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_query_lists_everything() {
        assert_eq!(matches_for("").len(), ENTRIES.len());
        assert_eq!(matches_for("   ").len(), ENTRIES.len());
    }

    /// Words in any order, which is what people actually type.
    #[test]
    fn every_typed_word_has_to_appear_somewhere_in_the_entry() {
        let by_title = matches_for("copy key");
        let reversed = matches_for("key copy");
        assert_eq!(by_title, reversed);
        assert_eq!(by_title.len(), 2, "one entry per platform");
        assert!(by_title
            .iter()
            .all(|index| ENTRIES[*index].title.contains("stream key")));
    }

    #[test]
    fn searching_ignores_case_and_finds_words_from_the_keywords() {
        // "colour" appears in no title, only in the keywords.
        let found = matches_for("COLOUR");
        assert_eq!(found.len(), 1);
        assert_eq!(ENTRIES[found[0]].title, "Choose a theme");
    }

    #[test]
    fn a_query_matching_nothing_returns_nothing_rather_than_everything() {
        assert!(matches_for("xyzzy").is_empty());
    }

    #[test]
    fn the_shortcut_itself_is_searchable() {
        let found = matches_for("alt+3");
        assert_eq!(found.len(), 1);
        assert!(ENTRIES[found[0]].title.contains("Combined"));
    }

    #[test]
    fn typing_resets_the_selection_to_the_top_of_the_narrowed_list() {
        let mut palette = CommandPalette::default();
        palette.move_by(5);
        assert_eq!(palette.selected, 5);
        palette.push('t');
        assert_eq!(palette.selected, 0);
        palette.move_by(1);
        palette.backspace();
        assert_eq!(palette.selected, 0);
    }

    #[test]
    fn the_selection_wraps_at_both_ends() {
        let mut palette = CommandPalette::default();
        palette.move_by(-1);
        assert_eq!(palette.selected, ENTRIES.len() - 1);
        palette.move_by(1);
        assert_eq!(palette.selected, 0);
    }

    #[test]
    fn moving_within_an_empty_result_list_does_not_panic() {
        let mut palette = CommandPalette {
            query: "xyzzy".into(),
            selected: 0,
        };
        palette.move_by(1);
        assert_eq!(palette.selected, 0);
        assert!(palette.chosen().is_none());
    }

    /// Every entry must name at least one key, since choosing an entry works
    /// by replaying its keys — an entry with none would do nothing at all.
    #[test]
    fn every_entry_names_the_keys_it_replays() {
        for entry in ENTRIES {
            assert!(!entry.keys.is_empty(), "{} replays nothing", entry.title);
            assert!(!entry.shortcut.is_empty(), "{} shows no key", entry.title);
        }
    }

    /// Two entries with the same title would be indistinguishable in the list.
    #[test]
    fn no_two_entries_share_a_title() {
        let mut titles: Vec<&str> = ENTRIES.iter().map(|entry| entry.title).collect();
        titles.sort_unstable();
        let count = titles.len();
        titles.dedup();
        assert_eq!(titles.len(), count, "two entries share a title");
    }
}
