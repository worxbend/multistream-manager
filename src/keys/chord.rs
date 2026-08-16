//! Keys, written the way vim writes them.
//!
//! A binding in the config file looks like `<Leader>os`, `<C-p>` or `]t` —
//! the notation vim, Neovim and every distribution built on them use. That is
//! a deliberate choice rather than a stylistic one: anyone who would want to
//! rebind keys in a terminal program already knows this notation, and
//! inventing a second one would mean they had to learn something to express
//! what they already know how to say.
//!
//! The forms understood:
//!
//! | written | means |
//! |---------|-------|
//! | `j` | the letter j |
//! | `J` | shift+j |
//! | `<Space>` | the space bar |
//! | `<Leader>` | whatever the leader is set to (space by default) |
//! | `<C-p>` | ctrl+p |
//! | `<A-4>` or `<M-4>` | alt+4 |
//! | `<S-Tab>` | shift+tab |
//! | `<CR>` or `<Enter>` | return |
//! | `<Esc>`, `<Tab>`, `<BS>`, `<Up>`, `<Down>`, `<Left>`, `<Right>` | those keys |
//! | `<PageUp>`, `<PageDown>`, `<Home>`, `<End>`, `<Del>` | those keys |
//! | `<F1>`…`<F12>` | function keys |
//!
//! Several of them in a row make a *chord*: `<Leader>os` is three keys —
//! leader, then `o`, then `s`.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// One key press.
///
/// Compared by code and modifiers only. Crossterm's own `KeyEvent` also
/// carries a kind and a state, which differ between terminals for the same
/// physical key — matching on those would make a binding work on one terminal
/// and not another.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Key {
    pub code: KeyCode,
    pub modifiers: KeyModifiers,
}

/// Keys are ordered so that listings come out in a stable, readable order.
///
/// Crossterm's `KeyCode` has no ordering of its own, and it needs one here:
/// bindings live in a sorted map so the which-key popup and the help show the
/// same order every run rather than whatever a hash produced this time.
///
/// The order is by kind first, then within a kind — so letters sort
/// alphabetically among themselves and the named keys stay grouped, which is
/// what a reader expects from a list of keys.
impl Ord for Key {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.sort_key().cmp(&other.sort_key())
    }
}

impl PartialOrd for Key {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Key {
    /// A value that sorts the way a list of keys should read.
    fn sort_key(&self) -> (u8, u32, u8) {
        let (kind, within) = match self.code {
            KeyCode::Char(c) => (0, c as u32),
            KeyCode::Enter => (1, 0),
            KeyCode::Esc => (1, 1),
            KeyCode::Tab => (1, 2),
            KeyCode::BackTab => (1, 3),
            KeyCode::Backspace => (1, 4),
            KeyCode::Delete => (1, 5),
            KeyCode::Insert => (1, 6),
            KeyCode::Up => (2, 0),
            KeyCode::Down => (2, 1),
            KeyCode::Left => (2, 2),
            KeyCode::Right => (2, 3),
            KeyCode::Home => (2, 4),
            KeyCode::End => (2, 5),
            KeyCode::PageUp => (2, 6),
            KeyCode::PageDown => (2, 7),
            KeyCode::F(n) => (3, u32::from(n)),
            _ => (9, 0),
        };
        (kind, within, self.modifiers.bits())
    }
}

impl Key {
    pub fn new(code: KeyCode, modifiers: KeyModifiers) -> Self {
        // A capital letter arrives from the terminal as the capital itself,
        // sometimes with the shift modifier set and sometimes without,
        // depending on the terminal. Normalising to "the character, no shift"
        // means `J` and `<S-j>` are the same binding however it arrives.
        if let KeyCode::Char(c) = code {
            if c.is_uppercase() {
                return Self {
                    code,
                    modifiers: modifiers.difference(KeyModifiers::SHIFT),
                };
            }
        }
        Self { code, modifiers }
    }

    /// The key this event represents, with the noise stripped off.
    pub fn from_event(event: KeyEvent) -> Self {
        Self::new(event.code, event.modifiers)
    }

    pub fn char(c: char) -> Self {
        Self::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    /// Constructors the tests use to write an expected key plainly. Not
    /// needed in the program itself, which builds keys either from a config
    /// string or from a terminal event.
    #[cfg(test)]
    pub fn plain(code: KeyCode) -> Self {
        Self::new(code, KeyModifiers::NONE)
    }

    #[cfg(test)]
    pub fn ctrl(c: char) -> Self {
        Self::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    #[cfg(test)]
    pub fn alt(c: char) -> Self {
        Self::new(KeyCode::Char(c), KeyModifiers::ALT)
    }

    /// Whether this is ordinary typed text rather than a command key.
    ///
    /// Used to decide what may go into a text box: a letter is text, a
    /// control- or alt-modified letter is a shortcut somebody pressed.
    pub fn is_text(&self) -> bool {
        matches!(self.code, KeyCode::Char(_))
            && !self.modifiers.contains(KeyModifiers::CONTROL)
            && !self.modifiers.contains(KeyModifiers::ALT)
    }

    /// Write this key the way the config file would.
    pub fn write(&self) -> String {
        let named = match self.code {
            KeyCode::Char(' ') => Some("Space".to_string()),
            KeyCode::Enter => Some("CR".to_string()),
            KeyCode::Esc => Some("Esc".to_string()),
            KeyCode::Tab => Some("Tab".to_string()),
            KeyCode::BackTab => Some("S-Tab".to_string()),
            KeyCode::Backspace => Some("BS".to_string()),
            KeyCode::Delete => Some("Del".to_string()),
            KeyCode::Up => Some("Up".to_string()),
            KeyCode::Down => Some("Down".to_string()),
            KeyCode::Left => Some("Left".to_string()),
            KeyCode::Right => Some("Right".to_string()),
            KeyCode::PageUp => Some("PageUp".to_string()),
            KeyCode::PageDown => Some("PageDown".to_string()),
            KeyCode::Home => Some("Home".to_string()),
            KeyCode::End => Some("End".to_string()),
            KeyCode::F(n) => Some(format!("F{n}")),
            KeyCode::Char(_) => None,
            _ => Some("?".to_string()),
        };

        let mut prefix = String::new();
        if self.modifiers.contains(KeyModifiers::CONTROL) {
            prefix.push_str("C-");
        }
        if self.modifiers.contains(KeyModifiers::ALT) {
            prefix.push_str("A-");
        }

        match (named, self.code) {
            (Some(name), _) => format!("<{prefix}{name}>"),
            // A bare letter needs no angle brackets; a modified one does.
            (None, KeyCode::Char(c)) if prefix.is_empty() => c.to_string(),
            (None, KeyCode::Char(c)) => format!("<{prefix}{c}>"),
            (None, _) => "?".to_string(),
        }
    }
}

/// A sequence of keys that runs one action.
pub type Chord = Vec<Key>;

/// Write a chord the way the config file would.
pub fn write_chord(chord: &[Key], leader: Key) -> String {
    chord
        .iter()
        .map(|key| {
            if *key == leader {
                "<Leader>".to_string()
            } else {
                key.write()
            }
        })
        .collect()
}

/// What went wrong reading a binding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub input: String,
    pub reason: String,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}: {}", self.input, self.reason)
    }
}

impl std::error::Error for ParseError {}

/// Read a binding like `<Leader>os` into the keys it stands for.
pub fn parse_chord(input: &str, leader: Key) -> Result<Chord, ParseError> {
    let fail = |reason: &str| ParseError {
        input: input.to_string(),
        reason: reason.to_string(),
    };

    let mut keys = Vec::new();
    let mut rest = input;

    while !rest.is_empty() {
        if let Some(after) = rest.strip_prefix('<') {
            let Some(end) = after.find('>') else {
                return Err(fail("a `<` with no closing `>`"));
            };
            let (name, remainder) = after.split_at(end);
            rest = &remainder[1..];
            keys.push(
                parse_named(name, leader).ok_or_else(|| fail(&format!("unknown key <{name}>")))?,
            );
        } else {
            let mut chars = rest.chars();
            let c = chars.next().expect("the string is not empty");
            rest = chars.as_str();
            keys.push(Key::char(c));
        }
    }

    if keys.is_empty() {
        return Err(fail("no keys in it"));
    }
    Ok(keys)
}

/// Read the inside of an angle-bracketed key: `C-p`, `Leader`, `CR`, `F5`.
fn parse_named(name: &str, leader: Key) -> Option<Key> {
    let mut modifiers = KeyModifiers::NONE;
    let mut rest = name;

    // Modifier prefixes stack: `<C-A-x>` is ctrl+alt+x.
    loop {
        let lower = rest.to_ascii_lowercase();
        if let Some(after) = lower.strip_prefix("c-") {
            modifiers |= KeyModifiers::CONTROL;
            rest = &rest[rest.len() - after.len()..];
        } else if let Some(after) = lower.strip_prefix("a-").or(lower.strip_prefix("m-")) {
            // `M-` is how vim writes alt, for historical reasons ("meta").
            modifiers |= KeyModifiers::ALT;
            rest = &rest[rest.len() - after.len()..];
        } else if let Some(after) = lower.strip_prefix("s-") {
            modifiers |= KeyModifiers::SHIFT;
            rest = &rest[rest.len() - after.len()..];
        } else {
            break;
        }
    }

    let code = match rest.to_ascii_lowercase().as_str() {
        "leader" => {
            // A modified leader is not a thing: the leader is one key, and
            // `<C-Leader>` would be asking for a key that does not exist.
            return (modifiers == KeyModifiers::NONE).then_some(leader);
        }
        "space" => KeyCode::Char(' '),
        "cr" | "enter" | "return" => KeyCode::Enter,
        "esc" | "escape" => KeyCode::Esc,
        "tab" => KeyCode::Tab,
        "bs" | "backspace" => KeyCode::Backspace,
        "del" | "delete" => KeyCode::Delete,
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        "pageup" => KeyCode::PageUp,
        "pagedown" => KeyCode::PageDown,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        "insert" => KeyCode::Insert,
        // vim's own escape for a literal `<`, which cannot be written bare
        // because it opens a key name.
        "lt" => KeyCode::Char('<'),
        "gt" => KeyCode::Char('>'),
        other => {
            if let Some(number) = other.strip_prefix('f') {
                match number.parse::<u8>() {
                    Ok(n @ 1..=12) => KeyCode::F(n),
                    _ => return None,
                }
            } else {
                let mut chars = rest.chars();
                let c = chars.next()?;
                if chars.next().is_some() {
                    return None;
                }
                KeyCode::Char(c)
            }
        }
    };

    // `<S-Tab>` is its own key rather than tab with a modifier, which is how
    // terminals actually report it.
    if code == KeyCode::Tab && modifiers.contains(KeyModifiers::SHIFT) {
        return Some(Key::new(
            KeyCode::BackTab,
            modifiers.difference(KeyModifiers::SHIFT),
        ));
    }

    // Shift on a letter means the capital, which is how it arrives.
    if let KeyCode::Char(c) = code {
        if modifiers.contains(KeyModifiers::SHIFT) && c.is_ascii_lowercase() {
            return Some(Key::new(
                KeyCode::Char(c.to_ascii_uppercase()),
                modifiers.difference(KeyModifiers::SHIFT),
            ));
        }
        // A control chord has no case: terminals send the same byte for
        // ctrl+p and ctrl+P, and vim writes `<C-P>` and `<C-p>` for the same
        // key. Folding to lower case makes both spellings one binding.
        if modifiers.contains(KeyModifiers::CONTROL) && c.is_ascii_uppercase() {
            return Some(Key::new(KeyCode::Char(c.to_ascii_lowercase()), modifiers));
        }
    }

    Some(Key::new(code, modifiers))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leader() -> Key {
        Key::char(' ')
    }

    #[test]
    fn a_bare_letter_is_one_key() {
        assert_eq!(parse_chord("j", leader()), Ok(vec![Key::char('j')]));
    }

    #[test]
    fn several_letters_make_a_sequence() {
        assert_eq!(
            parse_chord("]t", leader()),
            Ok(vec![Key::char(']'), Key::char('t')])
        );
    }

    #[test]
    fn the_leader_expands_to_whatever_it_is_set_to() {
        assert_eq!(
            parse_chord("<Leader>os", leader()),
            Ok(vec![Key::char(' '), Key::char('o'), Key::char('s')])
        );
        // A different leader produces a different first key, which is the
        // entire point of having one.
        assert_eq!(
            parse_chord("<Leader>q", Key::char(','))
                .expect("valid")
                .first()
                .copied(),
            Some(Key::char(','))
        );
    }

    #[test]
    fn modifiers_are_understood_in_vims_spelling() {
        assert_eq!(parse_chord("<C-p>", leader()), Ok(vec![Key::ctrl('p')]));
        assert_eq!(parse_chord("<A-4>", leader()), Ok(vec![Key::alt('4')]));
        // `M-` is vim's older spelling of alt, and plenty of configs use it.
        assert_eq!(parse_chord("<M-4>", leader()), Ok(vec![Key::alt('4')]));
        assert_eq!(
            parse_chord("<C-A-x>", leader()),
            Ok(vec![Key::new(
                KeyCode::Char('x'),
                KeyModifiers::CONTROL | KeyModifiers::ALT
            )])
        );
    }

    #[test]
    fn named_keys_are_understood() {
        for (written, expected) in [
            ("<CR>", KeyCode::Enter),
            ("<Enter>", KeyCode::Enter),
            ("<Esc>", KeyCode::Esc),
            ("<Tab>", KeyCode::Tab),
            ("<BS>", KeyCode::Backspace),
            ("<Space>", KeyCode::Char(' ')),
            ("<Up>", KeyCode::Up),
            ("<PageDown>", KeyCode::PageDown),
            ("<F5>", KeyCode::F(5)),
        ] {
            assert_eq!(
                parse_chord(written, leader()),
                Ok(vec![Key::plain(expected)]),
                "{written} should parse"
            );
        }
    }

    /// Names are matched without regard to case, because nobody remembers
    /// whether it is `<Esc>` or `<esc>` and both obviously mean the same key.
    #[test]
    fn key_names_ignore_case() {
        assert_eq!(
            parse_chord("<esc>", leader()),
            parse_chord("<ESC>", leader())
        );
        assert_eq!(
            parse_chord("<c-p>", leader()),
            parse_chord("<C-P>", leader())
        );
    }

    /// A capital letter arrives from the terminal in more than one shape
    /// depending on the terminal, so both spellings must mean the same key.
    #[test]
    fn a_capital_letter_and_shift_plus_a_letter_are_the_same_key() {
        assert_eq!(parse_chord("M", leader()), parse_chord("<S-m>", leader()));
        assert_eq!(
            Key::from_event(KeyEvent::new(KeyCode::Char('M'), KeyModifiers::SHIFT)),
            Key::char('M')
        );
    }

    #[test]
    fn shift_tab_is_its_own_key_the_way_terminals_report_it() {
        assert_eq!(
            parse_chord("<S-Tab>", leader()),
            Ok(vec![Key::plain(KeyCode::BackTab)])
        );
    }

    #[test]
    fn nonsense_is_refused_with_a_reason() {
        for bad in ["<C-p", "<nope>", "", "<F13>", "<C-toolong>"] {
            let error = parse_chord(bad, leader()).expect_err("{bad} should be refused");
            assert!(!error.reason.is_empty(), "{bad} needs a reason");
        }
    }

    /// A modified leader would be asking for a key that does not exist.
    #[test]
    fn a_modified_leader_is_refused() {
        assert!(parse_chord("<C-Leader>", leader()).is_err());
    }

    /// Round-tripping matters: the which-key popup and the help both show
    /// bindings using `write`, and what they show has to be something the
    /// config file would accept back.
    #[test]
    fn everything_written_can_be_read_back() {
        for written in [
            "j",
            "M",
            "<C-p>",
            "<A-4>",
            "<CR>",
            "<Esc>",
            "<Tab>",
            "<Space>",
            "<F5>",
            "<PageUp>",
            "]t",
            "<Leader>os",
        ] {
            let parsed = parse_chord(written, leader()).expect("parses");
            let printed = write_chord(&parsed, leader());
            let reparsed = parse_chord(&printed, leader()).expect("re-parses");
            assert_eq!(parsed, reparsed, "{written} became {printed}");
        }
    }

    #[test]
    fn the_leader_is_written_back_as_the_leader() {
        let chord = parse_chord("<Leader>uf", leader()).expect("parses");
        assert_eq!(write_chord(&chord, leader()), "<Leader>uf");
    }

    #[test]
    fn text_keys_are_told_apart_from_command_keys() {
        assert!(Key::char('j').is_text());
        assert!(Key::char(' ').is_text());
        assert!(!Key::ctrl('j').is_text());
        assert!(!Key::alt('j').is_text());
        assert!(!Key::plain(KeyCode::Enter).is_text());
    }
}
