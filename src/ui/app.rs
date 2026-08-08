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
}

impl Popup {
    fn selected(&self) -> Option<&(String, String)> {
        self.items.get(self.cursor)
    }
}

/// The whole UI state.
pub struct App {
    pub screen: Screen,
    pub config: Config,

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
    /// Which log line is at the top of the visible area, for scrolling.
    pub log_scroll: usize,
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
            log: VecDeque::new(),
            accounts: BTreeMap::new(),
            results: Vec::new(),
            stats: BTreeMap::new(),
            reveal_key: false,
            busy: false,
            should_quit: false,
            toast: None,
            log_scroll: 0,
        }
    }

    /// The currently focused form field.
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
        while self.log.len() > 500 {
            self.log.pop_front();
        }
        // Follow the tail unless the user has deliberately scrolled up.
        self.log_scroll = self.log.len().saturating_sub(1);
    }

    /// Fold a message from the worker into the state.
    pub fn handle_event(&mut self, event: Event) {
        match event {
            Event::Log { level, message } => self.push_log(level, message),

            Event::Connected(results) => {
                self.busy = false;
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
                    self.screen = Screen::Form;
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
                if let Some(popup) = self.popup.as_mut() {
                    if popup.field == field {
                        popup.items = results.into_iter().map(|c| (c.id, c.name)).collect();
                        popup.cursor = 0;
                        popup.loading = false;
                    }
                }
            }

            Event::WentLive(results) => {
                self.busy = false;
                let any_ok = results.iter().any(|r| r.succeeded());
                self.results = results;
                if any_ok {
                    self.screen = Screen::Dashboard;
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

        match self.screen {
            Screen::Platforms => self.key_platforms(key),
            Screen::Form => self.key_form(key),
            Screen::Dashboard => self.key_dashboard(key),
        }
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
                self.screen = Screen::Platforms;
            }
            KeyCode::Tab | KeyCode::Down => {
                self.field_cursor = (self.field_cursor + 1) % Field::ORDER.len();
                self.popup = None;
            }
            KeyCode::BackTab | KeyCode::Up => {
                self.field_cursor = if self.field_cursor == 0 {
                    Field::ORDER.len() - 1
                } else {
                    self.field_cursor - 1
                };
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
                        self.field_cursor = (self.field_cursor + 1) % Field::ORDER.len();
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
            Field::YouTubeCategory | Field::Language => self.open_popup(field),
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
                let existing = self
                    .popup
                    .as_ref()
                    .filter(|p| p.field == field)
                    .map(|p| p.items.clone())
                    .unwrap_or_default();

                self.popup = Some(Popup {
                    field,
                    items: existing,
                    cursor: 0,
                    loading: true,
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
        vec![Command::GoLive(Box::new(plan))]
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
            KeyCode::Char('e') | KeyCode::Esc => {
                // Back to the form to change something and resubmit. On YouTube
                // this creates a *new* broadcast rather than editing the old one.
                self.screen = Screen::Form;
                self.reveal_key = false;
            }
            KeyCode::Up => self.log_scroll = self.log_scroll.saturating_sub(1),
            KeyCode::Down => {
                self.log_scroll = (self.log_scroll + 1).min(self.log.len().saturating_sub(1));
            }
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
        assert!(matches!(commands.as_slice(), [Command::GoLive(_)]));
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

    #[test]
    fn a_successful_go_live_moves_to_the_dashboard() {
        let mut app = app_on_form();
        app.busy = true;

        app.handle_event(Event::WentLive(vec![PlatformResult {
            platform: Platform::Twitch,
            outcome: Ok(GoLiveOutcome::default()),
        }]));

        assert_eq!(app.screen, Screen::Dashboard);
        assert!(!app.busy);
    }

    #[test]
    fn a_total_failure_keeps_you_on_the_form_to_fix_it() {
        let mut app = app_on_form();
        app.busy = true;

        app.handle_event(Event::WentLive(vec![PlatformResult {
            platform: Platform::Twitch,
            outcome: Err("nope".into()),
        }]));

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
        app.handle_event(Event::WentLive(vec![
            PlatformResult {
                platform: Platform::Twitch,
                outcome: Ok(GoLiveOutcome::default()),
            },
            PlatformResult {
                platform: Platform::YouTube,
                outcome: Err("out of quota".into()),
            },
        ]));

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
    fn an_empty_saved_platform_list_falls_back_to_all_platforms() {
        let mut config = Config::default();
        config.preset.platforms = vec![];
        let app = App::new(config);
        assert_eq!(app.selected, Platform::ALL.to_vec());
    }
}
