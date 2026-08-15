//! The terminal UI's state and all of its keyboard handling.
//!
//! Everything in this file is **pure**: it takes a key press and mutates state,
//! and where slow work is needed it returns a [`Command`] for the worker to
//! carry out rather than doing it here. That is what makes the whole interaction
//! model testable — the tests below drive real key presses through `App` and
//! assert on what happens, with no terminal and no network involved.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::collections::{BTreeMap, VecDeque};

use super::input::TextInput;
use super::worker::{Command, Event, LogLevel};
use crate::backend::PlatformResult;
use crate::config::{Config, PresetConfig};
use crate::lang;
use crate::model::{
    limits, Category, Field, GoLiveOutcome, Platform, PlatformStats, Privacy, StreamPlan,
};
use crate::youtube;

/// Which screen is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    /// Pick Twitch, YouTube, or both.
    Platforms,
    /// Fill in the title, tags, category and everything else.
    Form,
    /// After a successful go-live: URLs, stream keys and live statistics.
    Dashboard,
}

/// One line in the activity log.
#[derive(Debug, Clone)]
pub struct LogLine {
    pub level: LogLevel,
    pub message: String,
    pub at: chrono::DateTime<chrono::Local>,
}

/// The autocomplete list that drops down under a field.
#[derive(Debug, Clone)]
pub struct Popup {
    /// Which field it belongs to.
    pub field: Field,
    /// `(value stored on selection, label shown in the list)`.
    pub items: Vec<(String, String)>,
    pub cursor: usize,
    /// `true` while a search request is in flight, so the UI can say "searching…"
    /// instead of "no matches" before the answer has arrived.
    pub loading: bool,
    /// `true` when `items` are stand-ins from a built-in list rather than the
    /// platform's own answer.
    ///
    /// While a search is in flight the previous results are normally left on
    /// screen, so that the list does not blink empty between keystrokes. That is
    /// the right thing to do with real results, but a *fallback* list left
    /// unfiltered would keep showing categories that no longer match what has
    /// been typed. This flag is how the two cases are told apart.
    pub fallback: bool,
}

impl Popup {
    fn selected(&self) -> Option<&(String, String)> {
        self.items.get(self.cursor)
    }
}

/// The top-level tabs. Stream Info is everything the app did before chat
/// arrived; Chat is the split Twitch/YouTube chat view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    StreamInfo,
    Chat,
}

/// The whole UI state.
pub struct App {
    pub screen: Screen,
    pub config: Config,
    /// Which top-level tab is showing. Alt+1 / Alt+2 switch (alt rather than
    /// ctrl because terminals cannot tell ctrl+1 from a plain 1).
    pub tab: Tab,
    /// State for the Chat tab (pane focus, account sub-tabs, split width).
    pub chat: super::chat_tab::ChatTabState,

    /// Which platforms are ticked on the first screen.
    pub selected: Vec<Platform>,
    pub platform_cursor: usize,

    /// Index into [`Field::ORDER`] for the focused form field.
    pub field_cursor: usize,
    pub inputs: BTreeMap<Field, TextInput>,

    /// The Twitch category chosen from the autocomplete, already resolved to an
    /// id. Kept separately from the text box so that typing a name without
    /// selecting a match does not silently look like a valid selection.
    pub twitch_category: Option<Category>,
    pub youtube_category_id: String,

    pub privacy: Privacy,
    pub made_for_kids: bool,
    pub auto_start: bool,
    pub auto_stop: bool,

    pub popup: Option<Popup>,
    /// Incremented on every keystroke that triggers a search, so that a slow
    /// reply to an earlier keystroke can be recognised as stale and dropped.
    pub search_generation: u64,
    /// Bumped on every go-live submission; the worker echoes it back in
    /// [`Event::WentLive`], so an answer to a superseded submission can be
    /// recognised as stale and dropped.
    pub go_generation: u64,

    pub log: VecDeque<LogLine>,
    /// Account names resolved during connection, per platform.
    pub accounts: BTreeMap<Platform, Result<String, String>>,
    pub results: Vec<PlatformResult>,
    pub stats: BTreeMap<Platform, PlatformStats>,

    /// Stream keys are masked until deliberately revealed, because this window
    /// is often visible on the stream itself.
    pub reveal_key: bool,
    /// `true` while the worker is busy, so the UI can show a spinner and refuse
    /// to submit the same plan twice.
    pub busy: bool,
    pub should_quit: bool,
    /// A transient one-line message shown at the bottom.
    pub toast: Option<String>,
    /// How many lines the activity log is scrolled back from its newest line.
    ///
    /// Zero means "show the tail", which is what a running session wants. Any
    /// other value pins the view that many lines further back so an error that
    /// has already scrolled past can be read. Counting back from the end rather
    /// than storing an absolute index means this state does not have to know how
    /// tall the panel is — only the drawing code does.
    pub log_scroll_back: usize,
}

impl App {
    /// Build the initial state from the saved config.
    pub fn new(config: Config) -> Self {
        let preset = config.preset.clone();
        let plan = preset.to_plan();

        let mut inputs = BTreeMap::new();
        inputs.insert(Field::Title, TextInput::new(plan.title.clone()));
        inputs.insert(Field::Description, TextInput::new(plan.description.clone()));
        inputs.insert(Field::Tags, TextInput::new(plan.tags_input()));
        inputs.insert(
            Field::TwitchCategory,
            TextInput::new(preset.twitch_category.clone()),
        );
        inputs.insert(
            Field::YouTubeCategory,
            TextInput::new(youtube::category_name(&plan.youtube_category_id)),
        );
        inputs.insert(Field::Language, TextInput::new(plan.language.clone()));

        let selected = if preset.platforms.is_empty() {
            Platform::ALL.to_vec()
        } else {
            preset.platforms.clone()
        };

        Self {
            tab: Tab::StreamInfo,
            chat: super::chat_tab::ChatTabState::new(&config),
            screen: Screen::Platforms,
            config,
            selected,
            platform_cursor: 0,
            field_cursor: 0,
            inputs,
            twitch_category: plan.twitch_category.clone(),
            youtube_category_id: plan.youtube_category_id.clone(),
            privacy: plan.privacy,
            made_for_kids: plan.made_for_kids,
            auto_start: plan.youtube_auto_start,
            auto_stop: plan.youtube_auto_stop,
            popup: None,
            search_generation: 0,
            go_generation: 0,
            log: VecDeque::new(),
            accounts: BTreeMap::new(),
            results: Vec::new(),
            stats: BTreeMap::new(),
            reveal_key: false,
            busy: false,
            should_quit: false,
            toast: None,
            log_scroll_back: 0,
        }
    }

    /// The currently focused form field.
    /// Change screen, clearing anything that belongs to the old one.
    ///
    /// Routing every transition through here is what stops a stale autocomplete
    /// popup from reappearing over the form later and silently swallowing
    /// Up/Down/Enter/Tab with no visible cause.
    fn go_to(&mut self, screen: Screen) {
        self.screen = screen;
        self.popup = None;
    }

    pub fn field(&self) -> Field {
        Field::ORDER[self.field_cursor.min(Field::ORDER.len() - 1)]
    }

    /// Read-only access to a field's text buffer.
    pub fn input(&self, field: Field) -> Option<&TextInput> {
        self.inputs.get(&field)
    }

    /// Whether a platform is ticked.
    pub fn is_selected(&self, platform: Platform) -> bool {
        self.selected.contains(&platform)
    }

    /// Whether a field is shown on the form at all.
    ///
    /// Fields belonging to a platform you are not streaming to are hidden, since
    /// filling them in would have no effect. Navigation has to agree with the
    /// renderer about this: if the cursor could land on a hidden field, the focus
    /// marker would vanish from the form and typing would go nowhere visible,
    /// which reads as the interface having frozen.
    pub fn is_field_visible(&self, field: Field) -> bool {
        match field {
            Field::Description
            | Field::YouTubeCategory
            | Field::Privacy
            | Field::MadeForKids
            | Field::AutoStart
            | Field::AutoStop => self.is_selected(Platform::YouTube),
            Field::TwitchCategory => self.is_selected(Platform::Twitch),
            // Title, Tags and Language apply to every platform.
            Field::Title | Field::Tags | Field::Language => true,
        }
    }

    /// Move the field cursor `step` places, skipping anything hidden.
    ///
    /// Falls back to leaving the cursor where it is if nothing is visible, which
    /// cannot happen while at least one platform is selected but keeps the loop
    /// bounded regardless.
    fn move_field(&mut self, forward: bool) {
        let count = Field::ORDER.len();
        for offset in 1..=count {
            let index = if forward {
                (self.field_cursor + offset) % count
            } else {
                (self.field_cursor + count - (offset % count)) % count
            };
            if self.is_field_visible(Field::ORDER[index]) {
                self.field_cursor = index;
                return;
            }
        }
    }

    /// Put the cursor on the first visible field. Used after the set of selected
    /// platforms changes, so the cursor cannot be stranded on a field that has
    /// just been hidden.
    pub fn ensure_field_visible(&mut self) {
        if self.is_field_visible(self.field()) {
            return;
        }
        if let Some(index) = Field::ORDER
            .iter()
            .position(|field| self.is_field_visible(*field))
        {
            self.field_cursor = index;
        }
    }

    /// Assemble the plan from the current form state.
    pub fn plan(&self) -> StreamPlan {
        StreamPlan {
            title: self
                .inputs
                .get(&Field::Title)
                .map(|i| i.value().to_string())
                .unwrap_or_default(),
            description: self
                .inputs
                .get(&Field::Description)
                .map(|i| i.value().to_string())
                .unwrap_or_default(),
            tags: StreamPlan::parse_tags(
                self.inputs
                    .get(&Field::Tags)
                    .map(|i| i.value())
                    .unwrap_or(""),
            ),
            twitch_category: self.twitch_category.clone(),
            youtube_category_id: self.youtube_category_id.clone(),
            language: self
                .inputs
                .get(&Field::Language)
                .map(|i| i.value().trim().to_lowercase())
                .unwrap_or_else(|| "en".into()),
            privacy: self.privacy,
            made_for_kids: self.made_for_kids,
            youtube_auto_start: self.auto_start,
            youtube_auto_stop: self.auto_stop,
        }
    }

    /// Append a line to the activity log, keeping the last 500.
    pub fn push_log(&mut self, level: LogLevel, message: impl Into<String>) {
        self.log.push_back(LogLine {
            level,
            message: message.into(),
            at: chrono::Local::now(),
        });
        let mut dropped_from_front = false;
        while self.log.len() > 500 {
            self.log.pop_front();
            dropped_from_front = true;
        }
        // Follow the tail unless the user has deliberately scrolled up. When
        // they have, the view stays on the lines they are reading: a new line
        // arriving at the end pushes their position one further back, and a line
        // dropping off the front pulls it one forward.
        if self.log_scroll_back > 0 {
            if !dropped_from_front {
                self.log_scroll_back += 1;
            }
            self.log_scroll_back = self.log_scroll_back.min(self.log.len().saturating_sub(1));
        }
    }

    /// Fold a message from the worker into the state.
    pub fn handle_event(&mut self, event: Event) {
        match event {
            Event::Log { level, message } => self.push_log(level, message),

            Event::Connected(results) => {
                self.busy = false;
                // The event carries the complete answer for the *current*
                // platform selection, so anything remembered from an earlier
                // connect is discarded first. Keeping old entries around let a
                // successful Twitch from a previous attempt advance the form
                // after the user had deselected Twitch and this attempt wholly
                // failed — and a later go-live then acted on the deselected
                // platform.
                self.accounts.clear();
                for (platform, outcome) in results {
                    match &outcome {
                        Ok(name) => self.push_log(
                            LogLevel::Success,
                            format!("{} connected as {name}.", platform.label()),
                        ),
                        Err(err) => self.push_log(
                            LogLevel::Error,
                            format!("{} could not connect: {err}", platform.label()),
                        ),
                    }
                    self.accounts.insert(platform, outcome);
                }
                // Move on to the form only if at least one platform worked.
                if self.accounts.values().any(|r| r.is_ok()) {
                    self.go_to(Screen::Form);
                    // The set of connected platforms decides which fields exist,
                    // so make sure the cursor is not left on a hidden one.
                    self.ensure_field_visible();
                }
            }

            Event::Categories {
                platform,
                results,
                generation,
            } => {
                // Discard an answer to a keystroke that has since been
                // superseded, otherwise the list flickers back to stale matches.
                if generation != self.search_generation {
                    return;
                }
                let field = match platform {
                    Platform::Twitch => Field::TwitchCategory,
                    Platform::YouTube => Field::YouTubeCategory,
                };

                let mut items: Vec<(String, String)> =
                    results.into_iter().map(|c| (c.id, c.name)).collect();

                // An empty reply means the API list could not be fetched: either
                // nothing is connected yet, or the search failed and the worker
                // answered with nothing so the spinner would stop. YouTube has a
                // short built-in list for exactly this case, so use it rather
                // than leaving the field looking broken. Twitch has no such
                // list — its category catalogue is far too large to embed — so
                // an empty reply there stays empty.
                let mut fallback = false;
                if items.is_empty() && field == Field::YouTubeCategory {
                    items = self.builtin_youtube_categories();
                    fallback = true;
                }

                if let Some(popup) = self.popup.as_mut() {
                    if popup.field == field {
                        popup.items = items;
                        popup.cursor = 0;
                        popup.loading = false;
                        popup.fallback = fallback;
                    }
                }
            }

            Event::WentLive {
                results,
                generation,
            } => {
                // An answer to a submission that has since been superseded:
                // acting on it would navigate the user away from whatever they
                // are doing now and overwrite the newer submission's results.
                if generation != self.go_generation {
                    return;
                }
                self.busy = false;
                let any_ok = results.iter().any(|r| r.succeeded());
                self.results = results;
                if any_ok {
                    self.go_to(Screen::Dashboard);
                    self.toast =
                        Some("Ready — start streaming in OBS whenever you like.".to_string());
                } else {
                    self.toast =
                        Some("Every platform failed. See the log below for why.".to_string());
                }
            }

            Event::Stats(stats) => {
                self.stats = stats.into_iter().collect();
            }
        }
    }

    /// Handle a key press, returning any work for the worker to do.
    pub fn handle_key(&mut self, key: KeyEvent) -> Vec<Command> {
        // Clear the toast on any key, so it behaves like a transient notice.
        self.toast = None;

        // Ctrl+C always quits, on every screen, even mid-request.
        if key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('c')) {
            self.should_quit = true;
            return vec![];
        }

        // Alt+digit switches top-level tabs from anywhere — including inside
        // a text field, because the Alt modifier keeps it unambiguous.
        if key.modifiers.contains(KeyModifiers::ALT) {
            match key.code {
                KeyCode::Char('1') => {
                    if self.tab == Tab::Chat {
                        self.chat.deactivate();
                    }
                    // Leaving the tab must not carry an armed destructive
                    // confirmation (or a half-typed chord) back in later.
                    self.chat.pending_mod = None;
                    self.chat.pending_space = false;
                    self.tab = Tab::StreamInfo;
                    return vec![];
                }
                KeyCode::Char('2') => {
                    self.chat.pending_mod = None;
                    self.chat.pending_space = false;
                    self.tab = Tab::Chat;
                    // Lazy connection happens here: entering the tab opens
                    // the selected accounts' own chats if they are not open.
                    self.chat.activate(&self.config);
                    return vec![];
                }
                _ => {}
            }
        }

        if self.tab == Tab::Chat {
            return self.key_chat(key);
        }

        match self.screen {
            Screen::Platforms => self.key_platforms(key),
            Screen::Form => self.key_form(key),
            Screen::Dashboard => self.key_dashboard(key),
        }
    }

    /// Keys on the Chat tab. Vim-flavoured, following the conventions the
    /// two reference chat TUIs establish.
    ///
    /// Normal mode: h/l (or arrows / tab) switch panes · j/k scroll (k moves
    /// back in history) · pgup/pgdn page · g/G oldest/newest · [ ] cycle the
    /// account's open chats · { } cycle account sub-tabs · < > resize the
    /// split toward/away from the focused pane · = reset split · i (or o/a)
    /// compose · space,c join a channel · space,x close the chat · ctrl+r
    /// reconnect · q quit. Compose/join modes capture typing until esc.
    fn key_chat(&mut self, key: KeyEvent) -> Vec<Command> {
        use super::chat_tab::ChatFocus;

        // Modal input first: while composing or joining, printable keys are
        // text, never commands (so typing a channel called "x" cannot close
        // anything).
        match self.chat.mode.clone() {
            ChatFocus::Compose => {
                match key.code {
                    KeyCode::Esc => self.chat.mode = ChatFocus::Normal,
                    KeyCode::Enter => self.chat.compose_send(),
                    KeyCode::Backspace => self.chat.compose_backspace(),
                    KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                        self.chat.compose_push(c)
                    }
                    _ => {}
                }
                return vec![];
            }
            ChatFocus::Join(mut buffer) => {
                match key.code {
                    KeyCode::Esc => self.chat.mode = ChatFocus::Normal,
                    KeyCode::Enter => {
                        self.chat.mode = ChatFocus::Normal;
                        self.chat.join_target(&self.config, &buffer);
                    }
                    KeyCode::Backspace => {
                        buffer.pop();
                        self.chat.mode = ChatFocus::Join(buffer);
                    }
                    KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                        buffer.push(c);
                        self.chat.mode = ChatFocus::Join(buffer);
                    }
                    _ => {}
                }
                return vec![];
            }
            ChatFocus::Search(mut buffer) => {
                match key.code {
                    KeyCode::Esc => {
                        // Esc abandons both the input and the committed query.
                        self.chat.commit_search(String::new());
                        self.chat.mode = ChatFocus::Normal;
                    }
                    KeyCode::Enter => {
                        self.chat.commit_search(buffer);
                        self.chat.mode = ChatFocus::Normal;
                    }
                    KeyCode::Backspace => {
                        buffer.pop();
                        self.chat.search_jump_newest(&buffer.clone());
                        self.chat.mode = ChatFocus::Search(buffer);
                    }
                    KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                        buffer.push(c);
                        // Incremental: every edit jumps to the newest match.
                        self.chat.search_jump_newest(&buffer.clone());
                        self.chat.mode = ChatFocus::Search(buffer);
                    }
                    _ => {}
                }
                return vec![];
            }
            ChatFocus::TimeoutPrompt(mut buffer) => {
                match key.code {
                    KeyCode::Esc => self.chat.mode = ChatFocus::Normal,
                    KeyCode::Enter => {
                        self.chat.mode = ChatFocus::Normal;
                        match super::chat_tab::parse_timeout(&buffer) {
                            Some(secs) => self.chat.timeout_selected(secs),
                            None => {
                                // An unparseable duration cancels rather than
                                // guessing a punishment length.
                            }
                        }
                    }
                    KeyCode::Backspace => {
                        buffer.pop();
                        self.chat.mode = ChatFocus::TimeoutPrompt(buffer);
                    }
                    KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                        buffer.push(c);
                        self.chat.mode = ChatFocus::TimeoutPrompt(buffer);
                    }
                    _ => {}
                }
                return vec![];
            }
            ChatFocus::Normal => {}
        }

        // The space leader chord: space then one key. Any unbound second key
        // cancels the chord instead of acting.
        if self.chat.pending_space {
            self.chat.pending_space = false;
            match key.code {
                KeyCode::Char('c') => self.chat.mode = ChatFocus::Join(String::new()),
                KeyCode::Char('x') => self.chat.close_active_chat(),
                KeyCode::Char('a') => self.chat.toggle_activity(),
                _ => {}
            }
            return vec![];
        }

        if key.modifiers.contains(KeyModifiers::CONTROL) {
            if let KeyCode::Char('r') = key.code {
                self.chat.reconnect_active();
            }
            return vec![];
        }

        // An armed moderation confirmation survives only its own key: any
        // other key cancels it instead of acting while a destructive prompt
        // is on screen.
        if self.chat.pending_mod.is_some() && !matches!(key.code, KeyCode::Char('d' | 't' | 'b')) {
            self.chat.pending_mod = None;
            return vec![];
        }

        match key.code {
            KeyCode::Char(' ') => self.chat.pending_space = true,
            KeyCode::Char('h')
            | KeyCode::Left
            | KeyCode::Char('l')
            | KeyCode::Right
            | KeyCode::Tab => self.chat.focus_other(),
            KeyCode::Char('i') | KeyCode::Char('o') | KeyCode::Char('a') => {
                if self.chat.active_key(self.chat.focus).is_some() {
                    self.chat.mode = ChatFocus::Compose;
                }
            }
            // k moves the selection back in history (bigger offset from the
            // bottom), j toward the tail — the vim direction sense over a
            // bottom-anchored log. The view follows the selection.
            KeyCode::Char('k') | KeyCode::Up => self.chat.select_move(1),
            KeyCode::Char('j') | KeyCode::Down => self.chat.select_move(-1),
            KeyCode::PageUp => self.chat.scroll_by(10),
            KeyCode::PageDown => self.chat.scroll_by(-10),
            KeyCode::Esc => self.chat.clear_selection(),
            KeyCode::Char('r') => {
                if self.chat.reply_to_selected() {
                    self.chat.mode = ChatFocus::Compose;
                }
            }
            // Moderation: first press arms, the same key confirms, anything
            // else cancels (handled by moderate() itself). YouTube only —
            // Twitch chats answer with an explanatory notice from the task.
            KeyCode::Char('d') => self.chat.moderate(super::chat_tab::ModAction::Delete),
            // t opens a duration prompt (yc's flow) instead of a fixed
            // double-press timeout; the prompt itself is the deliberate step.
            KeyCode::Char('t') => {
                if self.chat.selected_message().is_some() {
                    self.chat.mode = ChatFocus::TimeoutPrompt("5m".into());
                }
            }
            KeyCode::Char('b') => self.chat.moderate(super::chat_tab::ModAction::Ban),
            KeyCode::Char('/') => self.chat.mode = ChatFocus::Search(String::new()),
            KeyCode::Char('n') => self.chat.search_step(true),
            KeyCode::Char('N') => self.chat.search_step(false),
            KeyCode::Char(digit @ ('0' | '1' | '2' | '3' | '4')) => self.chat.toggle_filter(digit),
            KeyCode::Char('g') => self.chat.scroll_to_end(true),
            KeyCode::Char('G') => self.chat.scroll_to_end(false),
            KeyCode::Char(']') => self.chat.cycle_chat(true),
            KeyCode::Char('[') => self.chat.cycle_chat(false),
            KeyCode::Char('}') => self.chat.cycle_account(true, &self.config.clone()),
            KeyCode::Char('{') => self.chat.cycle_account(false, &self.config.clone()),
            KeyCode::Char('>') => self.chat.resize(true),
            KeyCode::Char('<') => self.chat.resize(false),
            KeyCode::Char('=') => self.chat.reset_split(),
            KeyCode::Char('q') => self.should_quit = true,
            _ => {}
        }
        vec![]
    }

    /// Fold one event from a chat task into the right chat's state.
    pub fn handle_chat_event(&mut self, key: crate::chat::ChatKey, event: crate::chat::ChatEvent) {
        self.chat.handle_event(key, event);
    }

    // -- Screen 1: choosing platforms ---------------------------------------

    fn key_platforms(&mut self, key: KeyEvent) -> Vec<Command> {
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => {
                self.should_quit = true;
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.platform_cursor = self.platform_cursor.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.platform_cursor = (self.platform_cursor + 1).min(Platform::ALL.len() - 1);
            }
            KeyCode::Char(' ') => {
                let platform = Platform::ALL[self.platform_cursor];
                if let Some(index) = self.selected.iter().position(|p| *p == platform) {
                    self.selected.remove(index);
                } else {
                    self.selected.push(platform);
                    self.selected.sort();
                }
            }
            KeyCode::Char('a') => {
                // Toggle "all": a convenience for the common both-platforms case.
                self.selected = if self.selected.len() == Platform::ALL.len() {
                    Vec::new()
                } else {
                    Platform::ALL.to_vec()
                };
            }
            KeyCode::Enter => {
                if self.selected.is_empty() {
                    self.toast = Some("Tick at least one platform with Space first.".to_string());
                    return vec![];
                }
                if self.busy {
                    return vec![];
                }
                self.busy = true;
                return vec![Command::Connect(self.selected.clone())];
            }
            _ => {}
        }
        vec![]
    }

    // -- Screen 2: the form -------------------------------------------------

    fn key_form(&mut self, key: KeyEvent) -> Vec<Command> {
        // The autocomplete list swallows navigation keys while it is open.
        if self.popup.is_some() {
            match key.code {
                KeyCode::Esc => {
                    self.popup = None;
                    return vec![];
                }
                KeyCode::Up => {
                    if let Some(popup) = self.popup.as_mut() {
                        popup.cursor = popup.cursor.saturating_sub(1);
                    }
                    return vec![];
                }
                KeyCode::Down => {
                    if let Some(popup) = self.popup.as_mut() {
                        if !popup.items.is_empty() {
                            popup.cursor = (popup.cursor + 1).min(popup.items.len() - 1);
                        }
                    }
                    return vec![];
                }
                KeyCode::Enter | KeyCode::Tab => {
                    self.accept_completion();
                    return vec![];
                }
                _ => {}
            }
        }

        // Control combinations, which work regardless of the focused field.
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            return self.key_form_control(key);
        }

        match key.code {
            KeyCode::Esc => {
                self.go_to(Screen::Platforms);
            }
            KeyCode::Tab | KeyCode::Down => {
                self.move_field(true);
                self.popup = None;
            }
            KeyCode::BackTab | KeyCode::Up => {
                self.move_field(false);
                self.popup = None;
            }
            KeyCode::Enter => {
                // On a searchable field Enter opens the list; elsewhere it
                // simply advances, which is what a form is expected to do.
                return match self.field() {
                    field @ (Field::TwitchCategory | Field::YouTubeCategory | Field::Language) => {
                        self.open_popup(field)
                    }
                    _ => {
                        self.move_field(true);
                        vec![]
                    }
                };
            }
            KeyCode::Char(' ') if !self.field().is_text_input() => {
                self.toggle_current_field();
            }
            KeyCode::Left if !self.field().is_text_input() => {
                self.cycle_current_field(false);
            }
            KeyCode::Right if !self.field().is_text_input() => {
                self.cycle_current_field(true);
            }
            KeyCode::Left => {
                if let Some(input) = self.inputs.get_mut(&self.field()) {
                    input.left();
                }
            }
            KeyCode::Right => {
                if let Some(input) = self.inputs.get_mut(&self.field()) {
                    input.right();
                }
            }
            KeyCode::Home => {
                if let Some(input) = self.inputs.get_mut(&self.field()) {
                    input.home();
                }
            }
            KeyCode::End => {
                if let Some(input) = self.inputs.get_mut(&self.field()) {
                    input.end();
                }
            }
            KeyCode::Backspace => {
                let field = self.field();
                if let Some(input) = self.inputs.get_mut(&field) {
                    input.backspace();
                }
                return self.on_text_changed(field);
            }
            KeyCode::Delete => {
                let field = self.field();
                if let Some(input) = self.inputs.get_mut(&field) {
                    input.delete();
                }
                return self.on_text_changed(field);
            }
            KeyCode::Char(c) => {
                let field = self.field();
                if field.is_text_input() {
                    if let Some(input) = self.inputs.get_mut(&field) {
                        input.insert(c);
                    }
                    return self.on_text_changed(field);
                }
            }
            _ => {}
        }

        vec![]
    }

    fn key_form_control(&mut self, key: KeyEvent) -> Vec<Command> {
        match key.code {
            // Submit. Ctrl+G for "go", and F5 as an alternative bound elsewhere.
            KeyCode::Char('g') => return self.submit(),
            // Save the current form values back to config.toml as the defaults.
            KeyCode::Char('s') => {
                self.save_preset();
            }
            KeyCode::Char('w') => {
                let field = self.field();
                if let Some(input) = self.inputs.get_mut(&field) {
                    input.delete_word_before();
                }
                return self.on_text_changed(field);
            }
            KeyCode::Char('u') => {
                let field = self.field();
                if let Some(input) = self.inputs.get_mut(&field) {
                    input.clear();
                }
                return self.on_text_changed(field);
            }
            _ => {}
        }
        vec![]
    }

    /// Called after any edit to a text field: refreshes autocomplete and clears
    /// a category selection that the user has typed over.
    fn on_text_changed(&mut self, field: Field) -> Vec<Command> {
        match field {
            Field::TwitchCategory => {
                // Typing invalidates a previously chosen category — otherwise
                // the form could submit an id that no longer matches the text.
                self.twitch_category = None;
                self.open_popup(field)
            }
            Field::YouTubeCategory => {
                // Same reasoning as Twitch above: once the text no longer matches
                // the resolved id, the id must not survive, or the form would
                // submit a category different from the one on screen.
                self.youtube_category_id.clear();
                self.open_popup(field)
            }
            Field::Language => self.open_popup(field),
            _ => vec![],
        }
    }

    /// Open (or refresh) the autocomplete list for a field.
    fn open_popup(&mut self, field: Field) -> Vec<Command> {
        let query = self
            .inputs
            .get(&field)
            .map(|i| i.value().to_string())
            .unwrap_or_default();

        match field {
            // The language list is built in, so it can be filtered instantly
            // with no network round trip at all.
            Field::Language => {
                let items = lang::search(&query);
                self.popup = Some(Popup {
                    field,
                    items,
                    cursor: 0,
                    loading: false,
                    // The language table is complete in itself, not a stand-in
                    // for something fetched from a platform.
                    fallback: false,
                });
                vec![]
            }
            Field::TwitchCategory | Field::YouTubeCategory => {
                let platform = if field == Field::TwitchCategory {
                    Platform::Twitch
                } else {
                    Platform::YouTube
                };

                // Only search platforms we are actually connected to.
                if !self.selected.contains(&platform) {
                    return vec![];
                }

                self.search_generation += 1;

                // Keep whatever is already listed on screen while the new
                // results are fetched, so the list does not blink empty.
                let carried = self.popup.as_ref().filter(|p| p.field == field);
                let carried_is_fallback = carried.map(|p| p.fallback).unwrap_or(false);
                let mut items = carried.map(|p| p.items.clone()).unwrap_or_default();

                // Seed the YouTube list from the built-in categories, so the
                // field responds to the very first keystroke instead of sitting
                // empty until a reply arrives — which, before the first login,
                // never happens at all. Re-filtering on every keystroke while
                // the fallback is what is on screen keeps the list honest;
                // real results are left alone, because those are worth keeping
                // visible until better ones replace them.
                let mut fallback = carried_is_fallback;
                if field == Field::YouTubeCategory && (items.is_empty() || carried_is_fallback) {
                    items = self.builtin_youtube_categories();
                    fallback = true;
                }

                self.popup = Some(Popup {
                    field,
                    items,
                    cursor: 0,
                    loading: true,
                    fallback,
                });

                vec![Command::SearchCategories {
                    platform,
                    query,
                    generation: self.search_generation,
                }]
            }
            _ => vec![],
        }
    }

    /// The built-in YouTube category list, filtered by whatever is typed in the
    /// YouTube category field, in the `(id, label)` shape the popup wants.
    ///
    /// This is the same arrangement the language field has always used: a short
    /// list compiled into the binary, filtered locally, needing neither a login
    /// nor any API quota. It is what the field falls back to whenever the full
    /// list fetched from YouTube is unavailable.
    fn builtin_youtube_categories(&self) -> Vec<(String, String)> {
        let query = self
            .inputs
            .get(&Field::YouTubeCategory)
            .map(|input| input.value().to_string())
            .unwrap_or_default();

        youtube::search_common(&query)
            .into_iter()
            .map(|category| (category.id, category.name))
            .collect()
    }

    /// Apply the highlighted autocomplete entry to its field.
    fn accept_completion(&mut self) {
        let Some(popup) = self.popup.take() else {
            return;
        };
        let Some((value, label)) = popup.selected().cloned() else {
            return;
        };

        match popup.field {
            Field::TwitchCategory => {
                self.twitch_category = Some(Category {
                    id: value,
                    name: label.clone(),
                });
                if let Some(input) = self.inputs.get_mut(&Field::TwitchCategory) {
                    input.set(label);
                }
            }
            Field::YouTubeCategory => {
                self.youtube_category_id = value;
                if let Some(input) = self.inputs.get_mut(&Field::YouTubeCategory) {
                    input.set(label);
                }
            }
            Field::Language => {
                // `value` is the two-letter code; the label is only for display.
                if let Some(input) = self.inputs.get_mut(&Field::Language) {
                    input.set(value);
                }
            }
            _ => {}
        }
    }

    /// Space on a boolean field flips it; on the privacy selector it advances.
    fn toggle_current_field(&mut self) {
        match self.field() {
            Field::MadeForKids => self.made_for_kids = !self.made_for_kids,
            Field::AutoStart => self.auto_start = !self.auto_start,
            Field::AutoStop => self.auto_stop = !self.auto_stop,
            Field::Privacy => self.cycle_current_field(true),
            _ => {}
        }
    }

    /// Left/Right on a selector field.
    fn cycle_current_field(&mut self, forward: bool) {
        match self.field() {
            Field::Privacy => {
                let options = Privacy::ALL;
                let current = options.iter().position(|p| *p == self.privacy).unwrap_or(0);
                let next = if forward {
                    (current + 1) % options.len()
                } else {
                    (current + options.len() - 1) % options.len()
                };
                self.privacy = options[next];
            }
            Field::MadeForKids => self.made_for_kids = !self.made_for_kids,
            Field::AutoStart => self.auto_start = !self.auto_start,
            Field::AutoStop => self.auto_stop = !self.auto_stop,
            _ => {}
        }
    }

    /// Validate and submit, or explain why it cannot be submitted.
    fn submit(&mut self) -> Vec<Command> {
        self.popup = None;

        if self.busy {
            self.toast = Some("Already working — hold on.".to_string());
            return vec![];
        }

        let plan = self.plan();
        let issues = plan.validate(&self.selected);
        let blocking: Vec<_> = issues.iter().filter(|i| i.blocking).collect();

        if !blocking.is_empty() {
            // Jump the cursor to the first problem so the fix is one keystroke
            // away rather than something to go hunting for.
            if let Some(index) = Field::ORDER.iter().position(|f| *f == blocking[0].field) {
                self.field_cursor = index;
            }
            for issue in &blocking {
                self.push_log(
                    LogLevel::Error,
                    format!("{}: {}", issue.field.label(), issue.message),
                );
            }
            self.toast = Some(blocking[0].message.clone());
            return vec![];
        }

        // Non-blocking issues are worth saying out loud but do not stop anything.
        for issue in issues.iter().filter(|i| !i.blocking) {
            self.push_log(
                LogLevel::Warning,
                format!("{}: {}", issue.field.label(), issue.message),
            );
        }

        self.busy = true;
        self.go_generation += 1;
        // The statistics on hand belong to the previous broadcast. Shown on
        // the new dashboard they would report the old stream's live status,
        // viewers and uptime for a broadcast that has not started, until the
        // next poll overwrote them.
        self.stats.clear();
        vec![Command::GoLive {
            plan: Box::new(plan),
            generation: self.go_generation,
        }]
    }

    /// Write the current form values back to `config.toml` as the new defaults.
    fn save_preset(&mut self) {
        let plan = self.plan();
        let mut config = self.config.clone();
        config.preset = PresetConfig::from_plan(&plan, &self.selected);

        match config.save() {
            Ok(()) => {
                self.config = config;
                self.push_log(LogLevel::Success, "Saved these settings as your defaults.");
                self.toast = Some("Saved to config.toml.".to_string());
            }
            Err(err) => {
                self.push_log(LogLevel::Error, format!("Could not save config: {err:#}"));
            }
        }
    }

    // -- Screen 3: the dashboard --------------------------------------------

    fn key_dashboard(&mut self, key: KeyEvent) -> Vec<Command> {
        match key.code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Char('r') => return vec![Command::PollStats],
            KeyCode::Char('k') => {
                self.reveal_key = !self.reveal_key;
                if self.reveal_key {
                    self.toast = Some(
                        "Stream key visible — press k again to hide it before you screen-share."
                            .to_string(),
                    );
                }
            }
            KeyCode::Char('o') => {
                // Open the watch page, which is the one link you actually want
                // in front of you once the stream is up — to check the delay, to
                // paste into chat, or to see what viewers see.
                match self.first_watch_url() {
                    Some(url) => {
                        self.toast = Some(format!("Opening {url}"));
                        return vec![Command::OpenUrl(url)];
                    }
                    None => {
                        self.toast = Some(
                            "No platform has a watch page yet — nothing has gone live.".to_string(),
                        );
                    }
                }
            }
            KeyCode::Char('e') | KeyCode::Esc => {
                // Back to the form to change something and resubmit. On YouTube
                // this creates a *new* broadcast rather than editing the old one.
                self.go_to(Screen::Form);
                self.ensure_field_visible();
                self.reveal_key = false;
            }
            // Up walks back into the history, Down returns towards the newest
            // line; reaching zero resumes following the tail.
            KeyCode::Up => {
                self.log_scroll_back =
                    (self.log_scroll_back + 1).min(self.log.len().saturating_sub(1));
            }
            KeyCode::Down => self.log_scroll_back = self.log_scroll_back.saturating_sub(1),
            _ => {}
        }
        vec![]
    }

    /// The successful outcome for a platform, if it has one.
    pub fn outcome_for(&self, platform: Platform) -> Option<&GoLiveOutcome> {
        self.results
            .iter()
            .find(|r| r.platform == platform)
            .and_then(|r| r.outcome.as_ref().ok())
    }

    /// The watch URL of the first platform that is ready, in the canonical
    /// platform order rather than in whichever order the replies happened to
    /// arrive — so the same key press opens the same page every time.
    ///
    /// A platform that failed, or that succeeded without reporting a watch page,
    /// is skipped rather than stopping the search.
    pub fn first_watch_url(&self) -> Option<String> {
        Platform::ALL
            .iter()
            .filter_map(|platform| self.outcome_for(*platform))
            .find_map(|outcome| outcome.watch_url.clone())
    }

    /// The statistics snapshot for a platform.
    pub fn stats_for(&self, platform: Platform) -> Option<&PlatformStats> {
        self.stats.get(&platform)
    }

    /// A character counter like `"87 / 100"`, plus whether it is over the limit.
    ///
    /// The limit shown is the *tighter* of the selected platforms' limits, since
    /// that is the one that will bite first.
    pub fn title_counter(&self) -> (String, bool) {
        let used = self
            .inputs
            .get(&Field::Title)
            .map(|i| i.len_chars())
            .unwrap_or(0);

        let limit = if self.is_selected(Platform::YouTube) {
            limits::YOUTUBE_TITLE
        } else {
            limits::TWITCH_TITLE
        };

        (format!("{used} / {limit}"), used > limit)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    fn app() -> App {
        App::new(Config::default())
    }

    /// Deliver a go-live answer stamped with the app's current submission
    /// generation, the way the worker answers the latest real submission.
    fn deliver_went_live(app: &mut App, results: Vec<PlatformResult>) {
        let generation = app.go_generation;
        app.handle_event(Event::WentLive {
            results,
            generation,
        });
    }

    /// Drive the app to the form screen without going through the worker.
    fn app_on_form() -> App {
        let mut app = app();
        app.screen = Screen::Form;
        app.selected = Platform::ALL.to_vec();
        app
    }

    fn type_text(app: &mut App, text: &str) {
        for c in text.chars() {
            app.handle_key(key(KeyCode::Char(c)));
        }
    }

    #[test]
    fn ctrl_c_quits_from_any_screen() {
        for screen in [Screen::Platforms, Screen::Form, Screen::Dashboard] {
            let mut app = app();
            app.screen = screen;
            app.handle_key(ctrl('c'));
            assert!(app.should_quit, "ctrl+c did not quit from {screen:?}");
        }
    }

    #[test]
    fn space_toggles_a_platform_on_and_off() {
        let mut app = app();
        app.selected.clear();
        app.platform_cursor = 0;

        app.handle_key(key(KeyCode::Char(' ')));
        assert!(app.is_selected(Platform::Twitch));

        app.handle_key(key(KeyCode::Char(' ')));
        assert!(!app.is_selected(Platform::Twitch));
    }

    #[test]
    fn the_platform_cursor_is_clamped_at_both_ends() {
        let mut app = app();
        app.handle_key(key(KeyCode::Up));
        assert_eq!(app.platform_cursor, 0);

        for _ in 0..10 {
            app.handle_key(key(KeyCode::Down));
        }
        assert_eq!(app.platform_cursor, Platform::ALL.len() - 1);
    }

    #[test]
    fn enter_with_nothing_selected_explains_instead_of_connecting() {
        let mut app = app();
        app.selected.clear();

        let commands = app.handle_key(key(KeyCode::Enter));
        assert!(commands.is_empty());
        assert!(app.toast.as_deref().unwrap().contains("at least one"));
        assert!(!app.busy);
    }

    #[test]
    fn enter_with_a_selection_asks_the_worker_to_connect() {
        let mut app = app();
        app.selected = vec![Platform::Twitch];

        let commands = app.handle_key(key(KeyCode::Enter));
        assert!(matches!(commands.as_slice(), [Command::Connect(p)] if p == &[Platform::Twitch]));
        assert!(app.busy, "the UI should mark itself busy while connecting");
    }

    #[test]
    fn a_second_enter_while_connecting_is_ignored() {
        let mut app = app();
        app.selected = vec![Platform::Twitch];
        app.handle_key(key(KeyCode::Enter));

        let commands = app.handle_key(key(KeyCode::Enter));
        assert!(commands.is_empty(), "must not connect twice");
    }

    #[test]
    fn tab_cycles_through_the_form_fields_and_wraps() {
        let mut app = app_on_form();
        assert_eq!(app.field(), Field::Title);

        app.handle_key(key(KeyCode::Tab));
        assert_eq!(app.field(), Field::Description);

        // Wrap around from the last field back to the first.
        for _ in 0..Field::ORDER.len() - 1 {
            app.handle_key(key(KeyCode::Tab));
        }
        assert_eq!(app.field(), Field::Title);
    }

    #[test]
    fn shift_tab_wraps_backwards_from_the_first_field() {
        let mut app = app_on_form();
        app.handle_key(key(KeyCode::BackTab));
        assert_eq!(app.field(), *Field::ORDER.last().unwrap());
    }

    #[test]
    fn tab_skips_fields_that_are_hidden_for_the_unselected_platform() {
        // Regression: field_cursor walked all 10 entries of Field::ORDER while
        // the renderer hid the YouTube-only ones, so Tab could park the cursor
        // on an invisible field. The focus marker vanished and typing went
        // nowhere visible, which reads as the form having frozen.
        let mut app = app_on_form();
        app.selected = vec![Platform::Twitch];
        app.ensure_field_visible();
        assert_eq!(app.field(), Field::Title);

        // Description is YouTube-only, so Tab must step straight over it.
        app.handle_key(key(KeyCode::Tab));
        assert_eq!(app.field(), Field::Tags);

        // Every field reachable by tabbing must be one the renderer draws.
        for _ in 0..40 {
            app.handle_key(key(KeyCode::Tab));
            assert!(
                app.is_field_visible(app.field()),
                "Tab landed on hidden field {:?}",
                app.field()
            );
        }
    }

    #[test]
    fn shift_tab_also_skips_hidden_fields() {
        let mut app = app_on_form();
        app.selected = vec![Platform::Twitch];
        app.ensure_field_visible();

        for _ in 0..40 {
            app.handle_key(key(KeyCode::BackTab));
            assert!(
                app.is_field_visible(app.field()),
                "Shift+Tab landed on hidden field {:?}",
                app.field()
            );
        }
    }

    #[test]
    fn the_cursor_is_rescued_when_its_field_becomes_hidden() {
        let mut app = app_on_form();
        app.field_cursor = Field::ORDER
            .iter()
            .position(|f| *f == Field::Privacy)
            .unwrap();

        // Untick YouTube: the Privacy field the cursor sits on is now hidden.
        app.selected = vec![Platform::Twitch];
        app.ensure_field_visible();

        assert!(app.is_field_visible(app.field()));
    }

    #[test]
    fn typing_lands_in_the_focused_text_field() {
        let mut app = app_on_form();
        type_text(&mut app, "Hello");
        assert_eq!(app.input(Field::Title).unwrap().value(), "Hello");
        assert_eq!(app.plan().title, "Hello");
    }

    #[test]
    fn typing_does_nothing_on_a_toggle_field() {
        let mut app = app_on_form();
        app.field_cursor = Field::ORDER
            .iter()
            .position(|f| *f == Field::MadeForKids)
            .unwrap();

        let before = app.made_for_kids;
        type_text(&mut app, "abc");
        assert_eq!(app.made_for_kids, before, "letters must not flip a toggle");
    }

    #[test]
    fn space_flips_a_boolean_field() {
        let mut app = app_on_form();
        app.field_cursor = Field::ORDER
            .iter()
            .position(|f| *f == Field::MadeForKids)
            .unwrap();

        assert!(!app.made_for_kids);
        app.handle_key(key(KeyCode::Char(' ')));
        assert!(app.made_for_kids);
    }

    #[test]
    fn left_and_right_cycle_the_privacy_selector() {
        let mut app = app_on_form();
        app.field_cursor = Field::ORDER
            .iter()
            .position(|f| *f == Field::Privacy)
            .unwrap();

        assert_eq!(app.privacy, Privacy::Public);
        app.handle_key(key(KeyCode::Right));
        assert_eq!(app.privacy, Privacy::Unlisted);
        app.handle_key(key(KeyCode::Left));
        assert_eq!(app.privacy, Privacy::Public);
        // And it wraps rather than sticking at the end.
        app.handle_key(key(KeyCode::Left));
        assert_eq!(app.privacy, Privacy::Private);
    }

    #[test]
    fn tags_are_parsed_from_the_comma_separated_box() {
        let mut app = app_on_form();
        app.field_cursor = Field::ORDER.iter().position(|f| *f == Field::Tags).unwrap();
        type_text(&mut app, "rust, tui , rust");

        // Duplicates removed, whitespace trimmed.
        assert_eq!(app.plan().tags, vec!["rust", "tui"]);
    }

    #[test]
    fn typing_in_the_language_field_opens_a_local_popup_with_no_network_call() {
        let mut app = app_on_form();
        app.field_cursor = Field::ORDER
            .iter()
            .position(|f| *f == Field::Language)
            .unwrap();
        app.inputs.get_mut(&Field::Language).unwrap().clear();

        let commands = type_and_collect(&mut app, "polish");
        assert!(
            commands.is_empty(),
            "the language list is built in, so it must not hit the network"
        );

        let popup = app.popup.as_ref().expect("a popup should have opened");
        assert_eq!(popup.items[0].0, "pl");
    }

    fn type_and_collect(app: &mut App, text: &str) -> Vec<Command> {
        let mut all = Vec::new();
        for c in text.chars() {
            all.extend(app.handle_key(key(KeyCode::Char(c))));
        }
        all
    }

    #[test]
    fn accepting_a_language_completion_stores_the_code_not_the_label() {
        let mut app = app_on_form();
        app.field_cursor = Field::ORDER
            .iter()
            .position(|f| *f == Field::Language)
            .unwrap();
        app.inputs.get_mut(&Field::Language).unwrap().clear();
        type_and_collect(&mut app, "polish");

        app.handle_key(key(KeyCode::Enter));

        assert_eq!(app.input(Field::Language).unwrap().value(), "pl");
        assert_eq!(app.plan().language, "pl");
        assert!(app.popup.is_none(), "accepting should close the popup");
    }

    #[test]
    fn typing_in_the_twitch_category_field_requests_a_search() {
        let mut app = app_on_form();
        app.field_cursor = Field::ORDER
            .iter()
            .position(|f| *f == Field::TwitchCategory)
            .unwrap();

        let commands = type_and_collect(&mut app, "chess");
        assert!(commands.iter().any(|c| matches!(
            c,
            Command::SearchCategories {
                platform: Platform::Twitch,
                ..
            }
        )));
    }

    #[test]
    fn no_search_is_issued_for_a_platform_that_is_not_selected() {
        let mut app = app_on_form();
        app.selected = vec![Platform::YouTube];
        app.field_cursor = Field::ORDER
            .iter()
            .position(|f| *f == Field::TwitchCategory)
            .unwrap();

        let commands = type_and_collect(&mut app, "chess");
        assert!(
            commands.is_empty(),
            "Twitch is not selected, so it must not be queried"
        );
    }

    #[test]
    fn stale_search_results_are_discarded() {
        let mut app = app_on_form();
        app.field_cursor = Field::ORDER
            .iter()
            .position(|f| *f == Field::TwitchCategory)
            .unwrap();
        type_and_collect(&mut app, "ch");

        // A reply tagged with an older generation than the current one.
        app.handle_event(Event::Categories {
            platform: Platform::Twitch,
            results: vec![Category {
                id: "1".into(),
                name: "Stale".into(),
            }],
            generation: 0,
        });

        let popup = app.popup.as_ref().unwrap();
        assert!(
            popup.items.iter().all(|(_, name)| name != "Stale"),
            "an outdated search result must not be shown"
        );
    }

    #[test]
    fn a_current_search_result_populates_the_popup() {
        let mut app = app_on_form();
        app.field_cursor = Field::ORDER
            .iter()
            .position(|f| *f == Field::TwitchCategory)
            .unwrap();
        type_and_collect(&mut app, "ch");

        let generation = app.search_generation;
        app.handle_event(Event::Categories {
            platform: Platform::Twitch,
            results: vec![Category {
                id: "743".into(),
                name: "Chess".into(),
            }],
            generation,
        });

        let popup = app.popup.as_ref().unwrap();
        assert_eq!(popup.items[0].1, "Chess");
        assert!(!popup.loading);
    }

    #[test]
    fn accepting_a_category_completion_stores_both_the_id_and_the_name() {
        let mut app = app_on_form();
        app.field_cursor = Field::ORDER
            .iter()
            .position(|f| *f == Field::TwitchCategory)
            .unwrap();
        type_and_collect(&mut app, "ch");

        let generation = app.search_generation;
        app.handle_event(Event::Categories {
            platform: Platform::Twitch,
            results: vec![Category {
                id: "743".into(),
                name: "Chess".into(),
            }],
            generation,
        });
        app.handle_key(key(KeyCode::Enter));

        let category = app.twitch_category.as_ref().expect("category was accepted");
        assert_eq!(category.id, "743");
        assert_eq!(category.name, "Chess");
        assert_eq!(app.input(Field::TwitchCategory).unwrap().value(), "Chess");
    }

    #[test]
    fn editing_the_category_text_clears_the_previously_resolved_id() {
        let mut app = app_on_form();
        app.twitch_category = Some(Category {
            id: "743".into(),
            name: "Chess".into(),
        });
        app.field_cursor = Field::ORDER
            .iter()
            .position(|f| *f == Field::TwitchCategory)
            .unwrap();

        app.handle_key(key(KeyCode::Char('x')));

        assert!(
            app.twitch_category.is_none(),
            "typing over a chosen category must invalidate the stored id, \
             otherwise the form would submit an id that no longer matches the text"
        );
    }

    #[test]
    fn the_popup_swallows_escape_without_leaving_the_form() {
        let mut app = app_on_form();
        app.field_cursor = Field::ORDER
            .iter()
            .position(|f| *f == Field::Language)
            .unwrap();
        type_and_collect(&mut app, "pol");
        assert!(app.popup.is_some());

        app.handle_key(key(KeyCode::Esc));
        assert!(app.popup.is_none());
        assert_eq!(
            app.screen,
            Screen::Form,
            "the first Esc only closes the popup"
        );

        app.handle_key(key(KeyCode::Esc));
        assert_eq!(app.screen, Screen::Platforms, "the second Esc goes back");
    }

    #[test]
    fn submitting_an_empty_title_is_blocked_and_focuses_the_offending_field() {
        let mut app = app_on_form();
        app.field_cursor = 5; // somewhere other than the title

        let commands = app.submit();
        assert!(commands.is_empty(), "an invalid plan must not be submitted");
        assert_eq!(
            app.field(),
            Field::Title,
            "the cursor should jump to the problem"
        );
        assert!(!app.busy);
    }

    #[test]
    fn submitting_without_a_twitch_category_is_blocked_when_twitch_is_selected() {
        let mut app = app_on_form();
        app.selected = vec![Platform::Twitch];
        app.inputs.get_mut(&Field::Title).unwrap().set("A title");
        app.twitch_category = None;

        assert!(app.submit().is_empty());
        assert_eq!(app.field(), Field::TwitchCategory);
    }

    #[test]
    fn a_valid_plan_is_submitted_and_marks_the_ui_busy() {
        let mut app = app_on_form();
        app.selected = vec![Platform::YouTube];
        app.inputs
            .get_mut(&Field::Title)
            .unwrap()
            .set("A good title");

        let commands = app.submit();
        assert!(matches!(commands.as_slice(), [Command::GoLive { .. }]));
        assert!(app.busy);
    }

    #[test]
    fn submitting_twice_in_a_row_only_sends_one_request() {
        let mut app = app_on_form();
        app.selected = vec![Platform::YouTube];
        app.inputs
            .get_mut(&Field::Title)
            .unwrap()
            .set("A good title");

        assert_eq!(app.submit().len(), 1);
        assert!(app.submit().is_empty(), "the second submit must be ignored");
    }

    /// An answer to a superseded submission must not act on the UI: it would
    /// navigate the user away from what they are doing and overwrite the
    /// current submission's results with older ones.
    #[test]
    fn a_stale_go_live_answer_is_discarded() {
        let mut app = app_on_form();
        app.selected = vec![Platform::YouTube];
        app.inputs.get_mut(&Field::Title).unwrap().set("A title");
        assert_eq!(app.submit().len(), 1);

        // A slow answer to some earlier submission arrives after the fact.
        app.handle_event(Event::WentLive {
            results: vec![PlatformResult {
                platform: Platform::Twitch,
                outcome: Ok(GoLiveOutcome::default()),
            }],
            generation: app.go_generation - 1,
        });

        assert_eq!(app.screen, Screen::Form, "a stale answer must not navigate");
        assert!(app.busy, "the current submission is still in flight");
        assert!(app.results.is_empty());

        // The genuine answer still lands normally.
        deliver_went_live(
            &mut app,
            vec![PlatformResult {
                platform: Platform::Twitch,
                outcome: Ok(GoLiveOutcome::default()),
            }],
        );
        assert_eq!(app.screen, Screen::Dashboard);
    }

    /// Statistics on hand always describe the previous broadcast. Left in
    /// place across a resubmit, the new dashboard opened showing the old
    /// stream as live with its viewers and uptime, for a broadcast that had
    /// not started.
    #[test]
    fn resubmitting_clears_the_previous_broadcast_statistics() {
        let mut app = app_on_form();
        app.selected = vec![Platform::YouTube];
        app.inputs.get_mut(&Field::Title).unwrap().set("A title");
        app.submit();
        deliver_went_live(
            &mut app,
            vec![PlatformResult {
                platform: Platform::Twitch,
                outcome: Ok(GoLiveOutcome::default()),
            }],
        );
        app.handle_event(Event::Stats(vec![(
            Platform::Twitch,
            PlatformStats {
                live: true,
                viewers: Some(12),
                ..Default::default()
            },
        )]));
        assert!(app.stats_for(Platform::Twitch).is_some());

        // Edit and go live again.
        app.handle_key(key(KeyCode::Char('e')));
        assert_eq!(app.screen, Screen::Form);
        app.submit();

        assert!(
            app.stats_for(Platform::Twitch).is_none(),
            "the old broadcast's numbers must not describe the new one"
        );
    }

    #[test]
    fn a_successful_go_live_moves_to_the_dashboard() {
        let mut app = app_on_form();
        app.busy = true;

        deliver_went_live(
            &mut app,
            vec![PlatformResult {
                platform: Platform::Twitch,
                outcome: Ok(GoLiveOutcome::default()),
            }],
        );

        assert_eq!(app.screen, Screen::Dashboard);
        assert!(!app.busy);
    }

    #[test]
    fn a_total_failure_keeps_you_on_the_form_to_fix_it() {
        let mut app = app_on_form();
        app.busy = true;

        deliver_went_live(
            &mut app,
            vec![PlatformResult {
                platform: Platform::Twitch,
                outcome: Err("nope".into()),
            }],
        );

        assert_eq!(
            app.screen,
            Screen::Form,
            "there is nothing to show on a dashboard"
        );
        assert!(app.toast.as_deref().unwrap().contains("failed"));
    }

    #[test]
    fn a_partial_failure_still_shows_the_dashboard() {
        let mut app = app_on_form();
        deliver_went_live(
            &mut app,
            vec![
                PlatformResult {
                    platform: Platform::Twitch,
                    outcome: Ok(GoLiveOutcome::default()),
                },
                PlatformResult {
                    platform: Platform::YouTube,
                    outcome: Err("out of quota".into()),
                },
            ],
        );

        // Twitch works, so its URL and key are worth showing.
        assert_eq!(app.screen, Screen::Dashboard);
        assert!(app.outcome_for(Platform::Twitch).is_some());
        assert!(app.outcome_for(Platform::YouTube).is_none());
    }

    #[test]
    fn connecting_advances_only_when_at_least_one_platform_succeeded() {
        let mut app = app();
        app.handle_event(Event::Connected(vec![(
            Platform::Twitch,
            Err("expired".into()),
        )]));
        assert_eq!(
            app.screen,
            Screen::Platforms,
            "nothing connected, so stay put"
        );

        app.handle_event(Event::Connected(vec![
            (Platform::Twitch, Err("expired".into())),
            (Platform::YouTube, Ok("My Channel".into())),
        ]));
        assert_eq!(app.screen, Screen::Form);
    }

    /// A reconnect answers for the *current* selection only. A success left
    /// over from an earlier attempt used to survive in `accounts`, advance the
    /// form even though this attempt wholly failed, and let a later go-live
    /// act on a platform the user had deselected.
    #[test]
    fn a_failed_reconnect_does_not_ride_on_an_earlier_success() {
        let mut app = app();
        // First attempt: Twitch connects fine.
        app.handle_event(Event::Connected(vec![(
            Platform::Twitch,
            Ok("someone".into()),
        )]));
        assert_eq!(app.screen, Screen::Form);

        // The user goes back, deselects Twitch, selects YouTube — which fails.
        app.go_to(Screen::Platforms);
        app.handle_event(Event::Connected(vec![(
            Platform::YouTube,
            Err("no credentials".into()),
        )]));

        assert_eq!(
            app.screen,
            Screen::Platforms,
            "a wholly failed connect must not advance on a stale success"
        );
        assert!(
            !app.accounts.contains_key(&Platform::Twitch),
            "the deselected platform must not linger in the account list"
        );
    }

    #[test]
    fn a_popup_does_not_survive_a_screen_change() {
        // Regression: a popup left open at submit time reappeared over the form
        // after returning from the dashboard, silently swallowing Up, Down,
        // Enter and Tab with nothing on screen to explain why.
        let mut app = app_on_form();
        app.selected = vec![Platform::YouTube];
        app.inputs.get_mut(&Field::Title).unwrap().set("A title");
        app.field_cursor = Field::ORDER
            .iter()
            .position(|f| *f == Field::Language)
            .unwrap();
        type_and_collect(&mut app, "pol");
        assert!(app.popup.is_some());

        app.submit();
        assert!(app.popup.is_none(), "submitting must close the popup");

        deliver_went_live(
            &mut app,
            vec![PlatformResult {
                platform: Platform::YouTube,
                outcome: Ok(GoLiveOutcome::default()),
            }],
        );
        assert!(app.popup.is_none());

        // Back to the form to edit and resubmit.
        app.handle_key(key(KeyCode::Char('e')));
        assert_eq!(app.screen, Screen::Form);
        assert!(app.popup.is_none(), "the stale popup must not come back");
    }

    #[test]
    fn typing_over_a_chosen_youtube_category_clears_its_id() {
        // Regression: only the Twitch field invalidated its resolved id, so the
        // form could submit a YouTube category different from the one shown.
        let mut app = app_on_form();
        app.youtube_category_id = "20".into();
        app.field_cursor = Field::ORDER
            .iter()
            .position(|f| *f == Field::YouTubeCategory)
            .unwrap();

        app.handle_key(key(KeyCode::Char('x')));

        assert!(
            app.youtube_category_id.is_empty(),
            "the stale category id must not survive an edit"
        );
        // And an unresolved category blocks submission rather than sending the
        // wrong one.
        assert!(!app.plan().is_submittable(&[Platform::YouTube]));
    }

    #[test]
    fn a_failed_category_search_clears_the_loading_spinner() {
        // The worker answers a failed search with an empty result set, so the
        // popup shows "no matches" instead of "searching…" forever.
        let mut app = app_on_form();
        app.field_cursor = Field::ORDER
            .iter()
            .position(|f| *f == Field::TwitchCategory)
            .unwrap();
        type_and_collect(&mut app, "ch");
        assert!(app.popup.as_ref().unwrap().loading);

        let generation = app.search_generation;
        app.handle_event(Event::Categories {
            platform: Platform::Twitch,
            results: vec![],
            generation,
        });

        assert!(!app.popup.as_ref().unwrap().loading);
    }

    #[test]
    fn the_stream_key_starts_hidden_and_toggles_with_k() {
        let mut app = app();
        app.screen = Screen::Dashboard;
        assert!(!app.reveal_key, "a key must never be visible by default");

        app.handle_key(key(KeyCode::Char('k')));
        assert!(app.reveal_key);
        assert!(app.toast.as_deref().unwrap().contains("screen-share"));

        app.handle_key(key(KeyCode::Char('k')));
        assert!(!app.reveal_key);
    }

    #[test]
    fn leaving_the_dashboard_re_hides_the_stream_key() {
        let mut app = app();
        app.screen = Screen::Dashboard;
        app.reveal_key = true;

        app.handle_key(key(KeyCode::Char('e')));

        assert_eq!(app.screen, Screen::Form);
        assert!(!app.reveal_key, "the key must not stay revealed");
    }

    #[test]
    fn r_on_the_dashboard_requests_a_stats_refresh() {
        let mut app = app();
        app.screen = Screen::Dashboard;
        let commands = app.handle_key(key(KeyCode::Char('r')));
        assert!(matches!(commands.as_slice(), [Command::PollStats]));
    }

    #[test]
    fn the_title_counter_uses_the_tighter_limit_when_youtube_is_selected() {
        let mut app = app_on_form();
        app.selected = vec![Platform::YouTube];
        app.inputs
            .get_mut(&Field::Title)
            .unwrap()
            .set("x".repeat(101));

        let (text, over) = app.title_counter();
        assert_eq!(text, "101 / 100");
        assert!(over);

        // Twitch alone allows 140, so the same title is fine.
        app.selected = vec![Platform::Twitch];
        let (text, over) = app.title_counter();
        assert_eq!(text, "101 / 140");
        assert!(!over);
    }

    #[test]
    fn the_log_is_capped_so_a_long_session_cannot_grow_without_bound() {
        let mut app = app();
        for i in 0..600 {
            app.push_log(LogLevel::Info, format!("line {i}"));
        }
        assert_eq!(app.log.len(), 500);
        // The oldest lines are the ones dropped.
        assert!(app.log.front().unwrap().message.contains("line 100"));
    }

    #[test]
    fn ctrl_w_deletes_a_word_in_the_focused_field() {
        let mut app = app_on_form();
        app.inputs
            .get_mut(&Field::Title)
            .unwrap()
            .set("hello brave world");

        app.handle_key(ctrl('w'));
        assert_eq!(app.input(Field::Title).unwrap().value(), "hello brave ");
    }

    #[test]
    fn the_app_starts_from_the_saved_preset() {
        let mut config = Config::default();
        config.preset.title = "Saved title".into();
        config.preset.tags = vec!["rust".into()];
        config.preset.language = "pl".into();
        config.preset.platforms = vec![Platform::Twitch];

        let app = App::new(config);
        assert_eq!(app.input(Field::Title).unwrap().value(), "Saved title");
        assert_eq!(app.input(Field::Tags).unwrap().value(), "rust");
        assert_eq!(app.plan().language, "pl");
        assert_eq!(app.selected, vec![Platform::Twitch]);
    }

    #[test]
    fn typing_in_the_youtube_category_field_offers_the_builtin_list_immediately() {
        // Regression: this field only ever showed anything once a search reply
        // arrived, so before the first login — when nothing is connected and no
        // reply ever comes — typing in it did nothing at all, with no
        // explanation on screen.
        let mut app = app_on_form();
        app.field_cursor = Field::ORDER
            .iter()
            .position(|f| *f == Field::YouTubeCategory)
            .unwrap();
        app.inputs.get_mut(&Field::YouTubeCategory).unwrap().clear();

        let commands = type_and_collect(&mut app, "gam");

        let popup = app.popup.as_ref().expect("a popup should have opened");
        assert_eq!(
            popup.items,
            vec![("20".to_string(), "Gaming".to_string())],
            "the built-in list should be filtered locally, like the language field"
        );
        // The API search is still issued: the full list replaces these as soon
        // as YouTube can be reached.
        assert!(commands.iter().any(|c| matches!(
            c,
            Command::SearchCategories {
                platform: Platform::YouTube,
                ..
            }
        )));
    }

    #[test]
    fn an_empty_youtube_search_reply_falls_back_to_the_builtin_list() {
        // The worker answers with nothing both when it cannot search at all and
        // when the search failed. Either way the field must stay usable rather
        // than showing "no matches" for a category that plainly exists.
        let mut app = app_on_form();
        app.field_cursor = Field::ORDER
            .iter()
            .position(|f| *f == Field::YouTubeCategory)
            .unwrap();
        app.inputs.get_mut(&Field::YouTubeCategory).unwrap().clear();
        type_and_collect(&mut app, "music");

        let generation = app.search_generation;
        app.handle_event(Event::Categories {
            platform: Platform::YouTube,
            results: vec![],
            generation,
        });

        let popup = app.popup.as_ref().unwrap();
        assert_eq!(popup.items, vec![("10".to_string(), "Music".to_string())]);
        assert!(!popup.loading, "the spinner must stop either way");
    }

    #[test]
    fn a_real_youtube_category_reply_replaces_the_builtin_fallback() {
        // Once YouTube can be reached its full list is the better answer, and
        // it must not be crowded out by the ten entries compiled in here.
        let mut app = app_on_form();
        app.field_cursor = Field::ORDER
            .iter()
            .position(|f| *f == Field::YouTubeCategory)
            .unwrap();
        app.inputs.get_mut(&Field::YouTubeCategory).unwrap().clear();
        type_and_collect(&mut app, "auto");

        let generation = app.search_generation;
        app.handle_event(Event::Categories {
            platform: Platform::YouTube,
            results: vec![Category {
                id: "2".into(),
                name: "Autos & Vehicles".into(),
            }],
            generation,
        });

        let popup = app.popup.as_ref().unwrap();
        assert_eq!(
            popup.items,
            vec![("2".to_string(), "Autos & Vehicles".to_string())]
        );
    }

    #[test]
    fn a_category_picked_from_the_builtin_list_is_submittable() {
        // The fallback is only worth having if what it offers can actually be
        // accepted and sent, so this drives the whole path end to end.
        let mut app = app_on_form();
        app.selected = vec![Platform::YouTube];
        app.inputs.get_mut(&Field::Title).unwrap().set("A title");
        app.field_cursor = Field::ORDER
            .iter()
            .position(|f| *f == Field::YouTubeCategory)
            .unwrap();
        app.inputs.get_mut(&Field::YouTubeCategory).unwrap().clear();

        type_and_collect(&mut app, "education");
        app.handle_key(key(KeyCode::Enter));

        assert_eq!(app.youtube_category_id, "27");
        assert_eq!(
            app.input(Field::YouTubeCategory).unwrap().value(),
            "Education"
        );
        assert!(app.plan().is_submittable(&[Platform::YouTube]));
    }

    #[test]
    fn an_empty_twitch_search_reply_is_not_given_a_builtin_fallback() {
        // Twitch's category catalogue is far too large to compile in, so there
        // is nothing honest to fall back to and "no matches" is the truth.
        let mut app = app_on_form();
        app.field_cursor = Field::ORDER
            .iter()
            .position(|f| *f == Field::TwitchCategory)
            .unwrap();
        type_and_collect(&mut app, "chess");

        let generation = app.search_generation;
        app.handle_event(Event::Categories {
            platform: Platform::Twitch,
            results: vec![],
            generation,
        });

        assert!(app.popup.as_ref().unwrap().items.is_empty());
    }

    #[test]
    fn o_on_the_dashboard_opens_the_watch_page_of_the_first_ready_platform() {
        let mut app = app();
        app.screen = Screen::Dashboard;
        app.selected = Platform::ALL.to_vec();
        app.results = vec![
            PlatformResult {
                platform: Platform::Twitch,
                outcome: Ok(GoLiveOutcome {
                    watch_url: Some("https://twitch.tv/example".into()),
                    ..Default::default()
                }),
            },
            PlatformResult {
                platform: Platform::YouTube,
                outcome: Ok(GoLiveOutcome {
                    watch_url: Some("https://youtube.com/watch?v=abc".into()),
                    ..Default::default()
                }),
            },
        ];

        let commands = app.handle_key(key(KeyCode::Char('o')));
        assert!(
            matches!(commands.as_slice(), [Command::OpenUrl(url)] if url == "https://twitch.tv/example"),
            "got {commands:?}"
        );
    }

    #[test]
    fn o_skips_a_platform_that_failed_and_opens_the_one_that_worked() {
        // Partial success is normal here, and the key should still do something
        // useful rather than giving up because the first platform is not there.
        let mut app = app();
        app.screen = Screen::Dashboard;
        app.selected = Platform::ALL.to_vec();
        app.results = vec![
            PlatformResult {
                platform: Platform::Twitch,
                outcome: Err("out of quota".into()),
            },
            PlatformResult {
                platform: Platform::YouTube,
                outcome: Ok(GoLiveOutcome {
                    watch_url: Some("https://youtube.com/watch?v=abc".into()),
                    ..Default::default()
                }),
            },
        ];

        let commands = app.handle_key(key(KeyCode::Char('o')));
        assert!(
            matches!(commands.as_slice(), [Command::OpenUrl(url)] if url == "https://youtube.com/watch?v=abc")
        );
    }

    #[test]
    fn o_with_nothing_live_explains_itself_instead_of_opening_a_blank_page() {
        let mut app = app();
        app.screen = Screen::Dashboard;

        let commands = app.handle_key(key(KeyCode::Char('o')));
        assert!(commands.is_empty());
        assert!(app.toast.as_deref().unwrap().contains("watch page"));
    }

    #[test]
    fn an_empty_saved_platform_list_falls_back_to_all_platforms() {
        let mut config = Config::default();
        config.preset.platforms = vec![];
        let app = App::new(config);
        assert_eq!(app.selected, Platform::ALL.to_vec());
    }
}
