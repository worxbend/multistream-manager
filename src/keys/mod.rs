//! Key bindings: what they are, and how to change them.
//!
//! Every key in this program is configurable, and the defaults are shaped the
//! way AstroNvim shapes Neovim's — because that is a shape a great many
//! people who live in a terminal already have in their fingers.
//!
//! What "AstroNvim-like" means here, concretely:
//!
//! * **A leader key**, space by default, in front of everything memorable.
//! * **Two-letter mnemonic groups** after it: `<Leader>o` is OBS, so
//!   `<Leader>os` is OBS → stream and `<Leader>om` is OBS → mute. The first
//!   letter names a subject, the second a verb, and neither has to be
//!   remembered as a whole.
//! * **A which-key popup**: press the leader and wait, and the choices appear
//!   rather than having to be known.
//! * **`]` and `[` for next and previous**, over whatever the current thing
//!   is — `]t` for tabs, `]c` for chats.
//! * **vim's own movement keys left alone**: `j`, `k`, `g`, `G` and friends
//!   mean what they mean everywhere else, so they are not behind a leader.
//! * **Control chords for the few things wanted from anywhere**: `<C-p>` for
//!   the command palette, `<C-c>` to quit.
//!
//! The letters follow AstroNvim's where the concept matches — `u` for
//! interface toggles, `f` for find, `q` for quit, `b` for buffers (tabs here)
//! — and diverge only where this program has no equivalent of what AstroNvim
//! puts there.
//!
//! Nothing here is fixed: `[keys]` in the config file rebinds any action,
//! and a binding of `""` removes it.

pub mod action;
pub mod chord;

pub use action::{Action, Context};
pub use chord::{parse_chord, write_chord, Key, ParseError};

use crossterm::event::KeyCode;
use std::collections::BTreeMap;

/// The default leader: space, as in AstroNvim.
pub fn default_leader() -> Key {
    Key::char(' ')
}

/// One binding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding {
    pub context: Context,
    pub chord: Vec<Key>,
    pub action: Action,
}

/// Every binding in force, and the leader they are written against.
#[derive(Debug, Clone)]
pub struct Keymap {
    pub leader: Key,
    /// Bindings by context and chord. A `BTreeMap` rather than a hash map so
    /// listings — the which-key popup, the help — come out in a stable order
    /// rather than a different one each run.
    bindings: BTreeMap<(Context, Vec<Key>), Action>,
}

impl Default for Keymap {
    fn default() -> Self {
        Self::defaults(default_leader())
    }
}

impl Keymap {
    /// The built-in bindings.
    pub fn defaults(leader: Key) -> Self {
        let mut map = Self {
            leader,
            bindings: BTreeMap::new(),
        };
        for (context, written, action) in DEFAULTS {
            let chord = parse_chord(written, leader)
                .unwrap_or_else(|err| panic!("built-in binding {written:?} is malformed: {err}"));
            map.bindings.insert((*context, chord), *action);
        }
        map
    }

    /// Bind a chord, replacing whatever was on it.
    pub fn bind(&mut self, context: Context, chord: Vec<Key>, action: Action) {
        self.bindings.insert((context, chord), action);
    }

    /// Remove a binding, reporting whether there was one.
    ///
    /// The answer matters: `[keys.global] "q" = ""` looks like it turns off
    /// quitting and does nothing at all, because `q` is bound in
    /// `stream_info`, `chat`, `obs` and `config` rather than globally. The
    /// removal silently hit nothing and `q` still quit.
    pub fn unbind(&mut self, context: Context, chord: &[Key]) -> bool {
        self.bindings.remove(&(context, chord.to_vec())).is_some()
    }

    /// Every context in which `chord` is bound to something.
    ///
    /// Used to explain an unbind that hit nothing, by naming where the chord
    /// actually lives.
    pub fn contexts_binding(&self, chord: &[Key]) -> Vec<Context> {
        Context::ALL
            .into_iter()
            .filter(|context| self.bindings.contains_key(&(*context, chord.to_vec())))
            .collect()
    }

    /// What a chord does in a context, if anything.
    ///
    /// The active context is tried first and [`Context::Global`] second, so a
    /// tab can give a key a local meaning without having to redefine
    /// everything else.
    pub fn action(&self, context: Context, chord: &[Key]) -> Option<Action> {
        self.bindings
            .get(&(context, chord.to_vec()))
            .or_else(|| self.bindings.get(&(Context::Global, chord.to_vec())))
            .copied()
    }

    /// Whether a chord is the beginning of a longer one.
    ///
    /// This is what makes `<Leader>o` wait rather than doing nothing: it is
    /// not itself bound, but several bindings start with it.
    pub fn is_prefix(&self, context: Context, chord: &[Key]) -> bool {
        self.bindings.keys().any(|(bound_context, bound)| {
            (*bound_context == context || *bound_context == Context::Global)
                && bound.len() > chord.len()
                && bound.starts_with(chord)
        })
    }

    /// Every binding that continues `prefix`, for the which-key popup.
    ///
    /// Returns the *next* key of each, with what it leads to: either an
    /// action, or a group of further choices.
    pub fn continuations(&self, context: Context, prefix: &[Key]) -> Vec<Continuation> {
        let mut found: BTreeMap<Key, Continuation> = BTreeMap::new();

        for ((bound_context, chord), action) in &self.bindings {
            if *bound_context != context && *bound_context != Context::Global {
                continue;
            }
            if chord.len() <= prefix.len() || !chord.starts_with(prefix) {
                continue;
            }
            let next = chord[prefix.len()];
            let entry = found.entry(next).or_insert(Continuation {
                key: next,
                action: None,
                group: None,
                count: 0,
            });
            entry.count += 1;
            if chord.len() == prefix.len() + 1 {
                // A binding that ends here — the context-specific one wins
                // over a global one of the same shape, matching `action`.
                if entry.action.is_none() || *bound_context == context {
                    entry.action = Some(*action);
                }
            } else {
                entry.group = Some(group_name(*action));
            }
        }

        found.into_values().collect()
    }

    /// Every binding, for the help screen and for `msm keys`.
    pub fn all(&self) -> Vec<Binding> {
        self.bindings
            .iter()
            .map(|((context, chord), action)| Binding {
                context: *context,
                chord: chord.clone(),
                action: *action,
            })
            .collect()
    }

    /// The key that runs `action` in `context`, written out.
    ///
    /// Always with a context, never without: a bare `j` is
    /// `chat.scroll_down`, `obs.down` and `config.next_section` — three
    /// different actions in three contexts. Answering "what key runs this"
    /// without saying where produced a key that runs something else where the
    /// asker is standing.
    pub fn binding_in(&self, action: Action, context: Context) -> Option<String> {
        self.best_chord(action, context)
            .map(|chord| write_chord(&chord, self.leader))
    }

    /// The keys that run `action` *in `context`*, as keys to replay.
    ///
    /// Only bindings that would actually fire there are considered: the
    /// context's own, and `Global`. Replaying a chord from somewhere else
    /// would be re-resolved where the user is standing and run whatever lives
    /// on those keys there.
    pub fn chord_in(&self, action: Action, context: Context) -> Option<Vec<Key>> {
        self.best_chord(action, context)
    }

    /// The shortest, least-modified chord bound to `action` that would fire
    /// in `context`.
    fn best_chord(&self, action: Action, context: Context) -> Option<Vec<Key>> {
        self.bindings
            .iter()
            .filter(|((bound_context, _), bound)| {
                // A context's own binding or a global one — the two that
                // `resolve_key` would find.
                **bound == action
                    && (*bound_context == context || *bound_context == Context::Global)
            })
            .map(|((_, chord), _)| chord)
            .min_by_key(|chord| {
                let modifiers: u32 = chord
                    .iter()
                    .map(|key| key.modifiers.bits().count_ones())
                    .sum();
                (chord.len(), modifiers, chord.to_vec())
            })
            .cloned()
    }

    /// Bindings that shadow each other, so the config can be checked.
    ///
    /// A chord bound in a context and also globally is not a conflict — that
    /// is how a tab gives a key a local meaning. Two bindings in the *same*
    /// context cannot both apply, but a `BTreeMap` cannot hold both, so the
    /// only reportable case is the shadowing one.
    pub fn shadowed(&self) -> Vec<(Context, String, Action, Action)> {
        let mut found = Vec::new();
        for ((context, chord), action) in &self.bindings {
            if *context == Context::Global {
                continue;
            }
            if let Some(global) = self.bindings.get(&(Context::Global, chord.clone())) {
                found.push((*context, write_chord(chord, self.leader), *action, *global));
            }
        }
        found
    }
}

impl Keymap {
    /// Bindings made unreachable by a shorter one that is a prefix of them.
    ///
    /// `resolve_key` checks for an exact match *before* it checks whether the
    /// chord is a prefix, so binding `<Leader>o` to something makes every
    /// `<Leader>o…` binding — the whole OBS group — unreachable. Nothing said
    /// so: `shadowed` only compares identical chords, and the keys simply
    /// stopped working.
    pub fn swallowed(&self) -> Vec<(String, Action, usize)> {
        let mut found = Vec::new();
        for ((context, chord), action) in &self.bindings {
            let buried = self
                .bindings
                .keys()
                .filter(|(other_context, other)| {
                    other_context == context
                        && other.len() > chord.len()
                        && other.starts_with(chord)
                })
                .count();
            if buried > 0 {
                found.push((write_chord(chord, self.leader), *action, buried));
            }
        }
        found.sort_by(|a, b| a.0.cmp(&b.0));
        found
    }
}

/// One choice in the which-key popup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Continuation {
    pub key: Key,
    /// What this key does, if it completes a binding.
    pub action: Option<Action>,
    /// What this key opens, if it is a prefix.
    pub group: Option<&'static str>,
    /// How many bindings lie under it.
    pub count: usize,
}

impl Continuation {
    /// What to show beside the key.
    pub fn label(&self) -> String {
        match (self.action, self.group) {
            (Some(action), _) => action.describe().to_string(),
            (None, Some(group)) => format!("+{group}"),
            (None, None) => String::new(),
        }
    }

    /// Whether this opens a group rather than doing something.
    pub fn is_group(&self) -> bool {
        self.action.is_none() && self.group.is_some()
    }
}

/// The name a group of bindings is shown under, taken from the actions in it.
fn group_name(action: Action) -> &'static str {
    match action.group() {
        "tab" => "tabs",
        "app" => "app",
        "ui" => "interface",
        "stream" => "stream",
        "chat" => "chat",
        "obs" => "obs",
        other => other,
    }
}

/// The built-in bindings, as `(context, chord, action)`.
///
/// Written in the same notation the config file uses, so what is here and
/// what someone would write to change it are the same thing.
#[rustfmt::skip]
const DEFAULTS: &[(Context, &str, Action)] = &[
    // --- from anywhere ----------------------------------------------------
    // Control chords for the handful of things wanted regardless of where
    // you are, exactly as AstroNvim keeps <C-s> and <C-q> outside the leader.
    (Context::Global, "<C-c>",     Action::Quit),
    (Context::Global, "<C-p>",     Action::CommandPalette),

    // Alt+digit for the tabs. Terminals cannot tell ctrl+1 from a plain 1,
    // which is why this is alt rather than ctrl.
    (Context::Global, "<A-1>",     Action::TabStreamInfo),
    (Context::Global, "<A-2>",     Action::TabChat),
    (Context::Global, "<A-3>",     Action::TabCombined),
    (Context::Global, "<A-4>",     Action::TabObs),
    (Context::Global, "<A-5>",     Action::TabConfig),
    (Context::Global, "<A-w>",     Action::CombinedSwapFocus),
    (Context::Global, "<A-m>",     Action::MessageHistory),

    // `]` and `[` for next and previous, over whatever the current thing is.
    (Context::Global, "]t",        Action::TabNext),
    (Context::Global, "[t",        Action::TabPrevious),

    // --- <Leader> ---------------------------------------------------------
    // Single letters directly under the leader, for the things done most.
    (Context::Global, "<Leader>q", Action::Quit),
    (Context::Global, "<Leader>?", Action::WhichKey),
    (Context::Global, "<Leader>/", Action::ChatSearch),

    // <Leader>b — tabs. AstroNvim's `b` is buffers; tabs are this program's
    // nearest equivalent, and the muscle memory of "b for the thing I switch
    // between" carries straight over.
    (Context::Global, "<Leader>bs", Action::TabStreamInfo),
    (Context::Global, "<Leader>bc", Action::TabChat),
    (Context::Global, "<Leader>bb", Action::TabCombined),
    (Context::Global, "<Leader>bo", Action::TabObs),
    (Context::Global, "<Leader>bg", Action::TabConfig),
    (Context::Global, "<Leader>bn", Action::TabNext),
    (Context::Global, "<Leader>bp", Action::TabPrevious),

    // <Leader>f — find, as in AstroNvim.
    (Context::Global, "<Leader>ff", Action::CommandPalette),
    (Context::Global, "<Leader>fm", Action::MessageHistory),
    (Context::Global, "<Leader>fc", Action::ChatSearch),
    (Context::Global, "<Leader>fk", Action::WhichKey),

    // <Leader>u — interface toggles, as in AstroNvim. `uc` is the
    // configuration tab, which is where the rest of them can also be reached
    // for anyone who would rather see the list than remember the letter.
    (Context::Global, "<Leader>uc", Action::TabConfig),
    (Context::Global, "<Leader>ut", Action::ThemePicker),
    (Context::Global, "<Leader>ua", Action::CycleAnimations),
    (Context::Global, "<Leader>uy", Action::ToggleTelemetry),
    (Context::Global, "<Leader>un", Action::MessageHistory),

    // <Leader>s — the stream itself. AstroNvim puts search here; this
    // program's whole subject is streaming, so the letter goes to that and
    // find keeps `f`.
    (Context::Global, "<Leader>sg", Action::GoLive),
    // Deliberately not next to `sg` on the keyboard: `se`, `sr`, `sy` are all
    // harmless, and this one ends the broadcast. It also asks a second time.
    (Context::Global, "<Leader>sx", Action::EndStream),
    (Context::Global, "<Leader>se", Action::EditStreamInfo),
    (Context::Global, "<Leader>sr", Action::RefreshStats),
    (Context::Global, "<Leader>sy", Action::CopyTwitchKey),
    (Context::Global, "<Leader>sY", Action::CopyYouTubeKey),
    (Context::Global, "<Leader>so", Action::OpenWatchPage),
    (Context::Global, "<Leader>sp", Action::StreamNextProfile),

    // The Config tab. These were hardcoded key matches until they became
    // actions, which is what makes them rebindable and what puts them in
    // which-key, <Leader>? and the command palette.
    (Context::Config, "j",              Action::ConfigNextSection),
    (Context::Config, "<Down>",         Action::ConfigNextSection),
    (Context::Config, "k",              Action::ConfigPreviousSection),
    (Context::Config, "<Up>",           Action::ConfigPreviousSection),
    (Context::Config, "<Tab>",          Action::ConfigSwapPane),
    (Context::Config, "h",              Action::ConfigSwapPane),
    (Context::Config, "l",              Action::ConfigSwapPane),
    (Context::Config, "<Enter>",        Action::ConfigActivate),
    (Context::Config, "a",              Action::ConfigAddAccount),
    (Context::Config, "d",              Action::ConfigForgetAccount),
    (Context::Config, "r",              Action::ConfigRefreshChecks),
    // `q` quits from every other tab and every Config footer promised it did
    // here too; it was swallowed by the layout editor's catch-all instead.
    (Context::Config, "q",              Action::Quit),

    // <Leader>c — chat.
    (Context::Global, "<Leader>cc", Action::ChatCompose),
    (Context::Global, "<Leader>cj", Action::ChatJoin),
    (Context::Global, "<Leader>cx", Action::ChatClose),
    (Context::Global, "<Leader>cm", Action::ChatMarker),
    (Context::Global, "<Leader>cr", Action::ChatReconnect),
    (Context::Global, "<Leader>cs", Action::ChatSearch),
    (Context::Global, "<Leader>ce", Action::ChatEmojiPicker),
    (Context::Global, "<Leader>ca", Action::ChatToggleActivity),
    (Context::Global, "<Leader>ci", Action::ChatToggleInspect),
    (Context::Global, "<Leader>c0", Action::ChatClearFilters),

    // <Leader>o — OBS.
    (Context::Global, "<Leader>os", Action::ObsToggleStream),
    (Context::Global, "<Leader>or", Action::ObsToggleRecord),
    (Context::Global, "<Leader>op", Action::ObsPauseRecording),
    (Context::Global, "<Leader>om", Action::ObsToggleMute),
    (Context::Global, "<Leader>oM", Action::ObsMuteAll),
    (Context::Global, "<Leader>oP", Action::ObsNextProfile),
    (Context::Global, "<Leader>oC", Action::ObsNextCollection),
    (Context::Global, "<Leader>oR", Action::ObsReconnect),
    (Context::Global, "<Leader>ou", Action::ObsRefresh),

    // --- Stream Info ------------------------------------------------------
    (Context::StreamInfo, "q",     Action::Quit),
    (Context::StreamInfo, "r",     Action::RefreshStats),
    (Context::StreamInfo, "e",     Action::EditStreamInfo),
    (Context::StreamInfo, "o",     Action::OpenWatchPage),
    (Context::StreamInfo, "y",     Action::CopyTwitchKey),
    (Context::StreamInfo, "Y",     Action::CopyYouTubeKey),

    // --- Chat -------------------------------------------------------------
    // vim's own movement, untouched: these mean the same here as anywhere.
    (Context::Chat, "j",           Action::ChatScrollDown),
    (Context::Chat, "k",           Action::ChatScrollUp),
    (Context::Chat, "<Down>",      Action::ChatScrollDown),
    (Context::Chat, "<Up>",        Action::ChatScrollUp),
    (Context::Chat, "<PageUp>",    Action::ChatPageUp),
    (Context::Chat, "<PageDown>",  Action::ChatPageDown),
    (Context::Chat, "g",           Action::ChatToTop),
    (Context::Chat, "G",           Action::ChatToBottom),
    (Context::Chat, "h",           Action::ChatFocusPreviousPane),
    (Context::Chat, "l",           Action::ChatFocusNextPane),
    (Context::Chat, "<Tab>",       Action::ChatFocusNextPane),
    (Context::Chat, "i",           Action::ChatCompose),
    (Context::Chat, "/",           Action::ChatSearch),
    (Context::Chat, "n",           Action::ChatSearchNext),
    (Context::Chat, "N",           Action::ChatSearchPrevious),
    (Context::Chat, "r",           Action::ChatReply),
    (Context::Chat, "]",           Action::ChatNextChat),
    (Context::Chat, "[",           Action::ChatPreviousChat),
    (Context::Chat, "}",           Action::ChatNextAccount),
    (Context::Chat, "{",           Action::ChatPreviousAccount),
    (Context::Chat, "<lt>",        Action::ChatWiden),
    (Context::Chat, ">",           Action::ChatNarrow),
    (Context::Chat, "=",           Action::ChatResetPanes),
    (Context::Chat, "<C-r>",       Action::ChatReconnect),
    // The presentation toggles. These were literal key matches with no
    // action, so they were in no list anywhere — not which-key, not the full
    // binding map, not the palette — and could not be rebound.
    (Context::Chat, "<C-g>",       Action::ChatCycleLayout),
    (Context::Chat, "<C-b>",       Action::ChatCycleBadges),
    (Context::Chat, "<C-y>",       Action::ChatToggleEmoteHighlight),
    (Context::Chat, "<C-n>",       Action::ChatToggleFullUsername),
    (Context::Chat, "<C-t>",       Action::ChatToggleTimestamps),
    (Context::Chat, "<C-e>",       Action::ChatEmojiPicker),
    (Context::Chat, "q",           Action::Quit),

    // --- OBS --------------------------------------------------------------
    (Context::Obs, "j",            Action::ObsDown),
    (Context::Obs, "k",            Action::ObsUp),
    (Context::Obs, "<Down>",       Action::ObsDown),
    (Context::Obs, "<Up>",         Action::ObsUp),
    (Context::Obs, "h",            Action::ObsSwapPane),
    (Context::Obs, "l",            Action::ObsSwapPane),
    (Context::Obs, "<Tab>",        Action::ObsSwapPane),
    (Context::Obs, "<CR>",         Action::ObsActivate),
    (Context::Obs, "m",            Action::ObsToggleMute),
    (Context::Obs, "M",            Action::ObsMuteAll),
    (Context::Obs, "+",            Action::ObsVolumeUp),
    (Context::Global, "<Leader>ob", Action::ObsSaveReplay),
    (Context::Obs, "]",            Action::ObsVolumeUpFine),
    (Context::Obs, "[",            Action::ObsVolumeDownFine),
    (Context::Obs, "=",            Action::ObsVolumeUp),
    (Context::Obs, "-",            Action::ObsVolumeDown),
    (Context::Obs, "s",            Action::ObsToggleStream),
    (Context::Obs, "r",            Action::ObsToggleRecord),
    (Context::Obs, "p",            Action::ObsPauseRecording),
    (Context::Obs, "P",            Action::ObsNextProfile),
    (Context::Obs, "C",            Action::ObsNextCollection),
    (Context::Obs, "R",            Action::ObsReconnect),
    (Context::Obs, "u",            Action::ObsRefresh),
    (Context::Obs, "q",            Action::Quit),
];

/// Read a leader from the config file.
pub fn parse_leader(written: &str) -> Result<Key, ParseError> {
    let keys = parse_chord(written, default_leader())?;
    match keys.as_slice() {
        [only] => Ok(*only),
        _ => Err(ParseError {
            input: written.to_string(),
            reason: "the leader has to be a single key".to_string(),
        }),
    }
}

/// Whether a key could start a chord rather than being typed text.
///
/// Used to keep the keymap out of the way of text boxes: while a message is
/// being written, a letter is a letter.
pub fn is_command_key(key: Key) -> bool {
    !key.is_text() || matches!(key.code, KeyCode::Esc | KeyCode::Enter | KeyCode::Tab)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn map() -> Keymap {
        Keymap::default()
    }

    fn chord(written: &str) -> Vec<Key> {
        parse_chord(written, default_leader()).expect("a valid chord")
    }

    /// Every built-in binding has to parse, or the program would panic on
    /// start-up. Building the default map is the check.
    #[test]
    fn the_built_in_bindings_are_all_valid() {
        let map = map();
        assert!(map.all().len() > 60, "there should be a good many");
    }

    #[test]
    fn the_leader_is_space_as_in_astronvim() {
        assert_eq!(default_leader(), Key::char(' '));
    }

    #[test]
    fn a_leader_sequence_resolves_to_its_action() {
        let map = map();
        assert_eq!(
            map.action(Context::Global, &chord("<Leader>os")),
            Some(Action::ObsToggleStream)
        );
        assert_eq!(
            map.action(Context::Global, &chord("<Leader>bc")),
            Some(Action::TabChat)
        );
    }

    /// The whole point of contexts: `j` scrolls chat in one place and moves
    /// down a scene list in another.
    #[test]
    fn the_same_key_can_mean_different_things_in_different_contexts() {
        let map = map();
        assert_eq!(
            map.action(Context::Chat, &chord("j")),
            Some(Action::ChatScrollDown)
        );
        assert_eq!(map.action(Context::Obs, &chord("j")), Some(Action::ObsDown));
        // And nothing at all where it has no meaning.
        assert_eq!(map.action(Context::StreamInfo, &chord("j")), None);
    }

    #[test]
    fn a_global_binding_works_from_every_context() {
        let map = map();
        for context in Context::ALL {
            assert_eq!(
                map.action(context, &chord("<C-p>")),
                Some(Action::CommandPalette),
                "the palette must be reachable from {context:?}"
            );
        }
    }

    /// A context-specific binding has to win over a global one of the same
    /// shape, or a tab could never give a key a local meaning.
    #[test]
    fn a_context_binding_wins_over_a_global_one() {
        let mut map = map();
        map.bind(Context::Global, chord("x"), Action::Quit);
        map.bind(Context::Obs, chord("x"), Action::ObsMuteAll);

        assert_eq!(
            map.action(Context::Obs, &chord("x")),
            Some(Action::ObsMuteAll)
        );
        assert_eq!(map.action(Context::Chat, &chord("x")), Some(Action::Quit));
    }

    /// A prefix must wait for more rather than doing nothing, which is what
    /// makes the which-key popup possible.
    #[test]
    fn an_incomplete_sequence_is_recognised_as_a_prefix() {
        let map = map();
        assert!(map.is_prefix(Context::Global, &chord("<Leader>")));
        assert!(map.is_prefix(Context::Global, &chord("<Leader>o")));
        assert!(!map.is_prefix(Context::Global, &chord("<Leader>os")));
        assert!(!map.is_prefix(Context::Global, &chord("<Leader>zz")));
    }

    #[test]
    fn the_continuations_of_a_prefix_are_listed_for_the_popup() {
        let map = map();
        let choices = map.continuations(Context::Global, &chord("<Leader>o"));
        let keys: BTreeSet<String> = choices.iter().map(|choice| choice.key.write()).collect();
        assert!(keys.contains("s"), "OBS stream should be under <Leader>o");
        assert!(keys.contains("m"), "OBS mute should be under <Leader>o");

        // Each one says what it does.
        let stream = choices
            .iter()
            .find(|choice| choice.key == Key::char('s'))
            .expect("the stream binding");
        assert_eq!(stream.action, Some(Action::ObsToggleStream));
        assert!(!stream.is_group());
    }

    /// The top level of the popup shows groups, not sixty individual keys.
    #[test]
    fn the_leader_shows_groups_rather_than_every_binding() {
        let map = map();
        let choices = map.continuations(Context::Global, &chord("<Leader>"));
        let obs = choices
            .iter()
            .find(|choice| choice.key == Key::char('o'))
            .expect("the OBS group");
        assert!(obs.is_group(), "<Leader>o opens a group");
        assert_eq!(obs.label(), "+obs");
        assert!(obs.count > 1);

        // And a leaf under the leader still reads as what it does.
        let quit = choices
            .iter()
            .find(|choice| choice.key == Key::char('q'))
            .expect("quit");
        assert_eq!(quit.action, Some(Action::Quit));
    }

    #[test]
    fn rebinding_replaces_and_unbinding_removes() {
        let mut map = map();
        map.bind(Context::Global, chord("<Leader>os"), Action::Quit);
        assert_eq!(
            map.action(Context::Global, &chord("<Leader>os")),
            Some(Action::Quit)
        );

        map.unbind(Context::Global, &chord("<Leader>os"));
        assert_eq!(map.action(Context::Global, &chord("<Leader>os")), None);
    }

    /// The shortest binding is the one worth teaching, since it is the one
    /// somebody would actually press.
    #[test]
    fn the_binding_shown_for_an_action_is_its_shortest() {
        let map = map();
        // Quit is on <C-c>, <Leader>q and q — the shortest wins.
        assert_eq!(
            map.binding_in(Action::Quit, Context::Chat).as_deref(),
            Some("q")
        );
        // The command palette is on <C-p> and <Leader>ff.
        assert_eq!(
            map.binding_in(Action::CommandPalette, Context::Chat)
                .as_deref(),
            Some("<C-p>")
        );
    }

    /// The same key means different things in different contexts, so the key
    /// shown for an action has to be one that would fire where it is shown.
    /// A bare `j` is `chat.scroll_down`, `obs.down` *and*
    /// `config.next_section`.
    #[test]
    fn the_binding_shown_is_one_that_would_fire_in_that_context() {
        let map = map();

        assert_eq!(
            map.binding_in(Action::ChatScrollDown, Context::Chat)
                .as_deref(),
            Some("j")
        );
        assert_eq!(
            map.binding_in(Action::ChatScrollDown, Context::Obs),
            None,
            "chat scrolling is not on any key that fires on the OBS tab"
        );
        assert_eq!(
            map.binding_in(Action::ObsDown, Context::Obs).as_deref(),
            Some("j")
        );
    }

    #[test]
    fn an_unbound_action_has_no_binding_to_show() {
        let mut map = map();
        for binding in map.all() {
            if binding.action == Action::ObsMuteAll {
                map.unbind(binding.context, &binding.chord);
            }
        }
        assert_eq!(map.binding_in(Action::ObsMuteAll, Context::Obs), None);
    }

    /// Changing the leader has to move every binding written against it,
    /// rather than leaving them on the old key.
    #[test]
    fn changing_the_leader_moves_every_leader_binding() {
        let comma = Key::char(',');
        let map = Keymap::defaults(comma);

        assert_eq!(
            map.action(Context::Global, &[comma, Key::char('o'), Key::char('s')]),
            Some(Action::ObsToggleStream)
        );
        assert_eq!(
            map.action(Context::Global, &chord("<Leader>os")),
            None,
            "space should mean nothing once the leader has moved"
        );
    }

    /// The defaults must not shadow each other in a way that makes a key
    /// unreachable — a context binding hiding a global one is fine and
    /// intended, but it should be a short, deliberate list.
    #[test]
    fn the_default_shadowing_is_deliberate() {
        let map = map();
        let shadowed = map.shadowed();
        for (context, chord, local, global) in &shadowed {
            // The only shadowing in the defaults is `q`, which quits
            // globally and also quits from each tab.
            assert_eq!(
                (local, global),
                (&Action::Quit, &Action::Quit),
                "{chord} in {context:?} shadows something unexpectedly"
            );
        }
    }

    /// Every action ought to be reachable somehow, or it is dead weight
    /// nobody can run.
    ///
    /// Note the limit of this check: it proves every *action* has a key, not
    /// that every *capability* has an action. Closing a chat was implemented
    /// and tested and completely unreachable for exactly that reason — it
    /// hung off a second, private space-leader inside the chat key handler
    /// that the real keymap consumed before it ever ran, and no `Action`
    /// named it, so nothing here noticed.
    #[test]
    fn every_action_has_a_default_binding() {
        let map = map();
        let bound: BTreeSet<Action> = map
            .all()
            .into_iter()
            .map(|binding| binding.action)
            .collect();
        let missing: Vec<&str> = Action::ALL
            .iter()
            .filter(|action| !bound.contains(action))
            .map(|action| action.name())
            .collect();
        assert!(missing.is_empty(), "unreachable actions: {missing:?}");
    }

    #[test]
    fn a_leader_has_to_be_one_key() {
        assert_eq!(parse_leader("<Space>"), Ok(Key::char(' ')));
        assert_eq!(parse_leader(","), Ok(Key::char(',')));
        assert!(parse_leader("ab").is_err());
        assert!(parse_leader("").is_err());
    }

    #[test]
    fn text_keys_are_not_treated_as_command_keys() {
        assert!(!is_command_key(Key::char('j')));
        assert!(is_command_key(Key::ctrl('j')));
        assert!(is_command_key(Key::plain(KeyCode::Esc)));
        assert!(is_command_key(Key::plain(KeyCode::Enter)));
    }
}
