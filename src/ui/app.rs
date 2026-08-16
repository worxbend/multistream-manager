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

/// Which platforms already have a saved login, read once at start-up.
///
/// Reading the token store is a single small file read, and it happens before
/// the interface starts drawing, so it does not belong on the worker. A store
/// that cannot be read at all is reported as "nothing is logged in", which is
/// the state the login screen is there to fix anyway.
fn saved_logins() -> BTreeMap<Platform, bool> {
    let store = crate::auth::store::TokenStore::load().unwrap_or_default();
    Platform::ALL
        .iter()
        .map(|platform| (*platform, store.get(*platform).is_some()))
        .collect()
}

/// Whether a key press should be treated as typed text in a modal input.
///
/// A bare character is text; a character carrying Ctrl or Alt is a command.
/// Alt matters here because the tab switcher above only consumes the alt+digit
/// combinations it knows about — without this guard, any other alt combination
/// (alt+3, or a habit from another terminal program) fell through and typed
/// its bare letter into whatever chat input was open.
fn is_typed_text(key: &KeyEvent) -> bool {
    !key.modifiers.contains(KeyModifiers::CONTROL) && !key.modifiers.contains(KeyModifiers::ALT)
}

/// Which list on the OBS tab has the keyboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObsFocus {
    Scenes,
    Audio,
}

/// Which screen is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    /// First run: type in the API credentials, because nothing works without
    /// them and quitting to hand-edit a file is a poor welcome.
    Setup,
    /// Authorise Twitch, YouTube or both in the browser.
    Login,
    /// Pick Twitch, YouTube, or both.
    Platforms,
    /// Fill in the title, tags, category and everything else.
    Form,
    /// After a successful go-live: URLs, stream keys and live statistics.
    Dashboard,
}

/// One box on the credential setup screen.
///
/// "Client id" and "client secret" are what each platform's developer console
/// calls the two halves of an application's identity: the id names the
/// application, the secret proves the request really comes from it. They are
/// per-application, not per-account — logging in comes afterwards.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SetupField {
    TwitchId,
    TwitchSecret,
    YouTubeId,
    YouTubeSecret,
}

impl SetupField {
    pub const ORDER: [SetupField; 4] = [
        SetupField::TwitchId,
        SetupField::TwitchSecret,
        SetupField::YouTubeId,
        SetupField::YouTubeSecret,
    ];

    pub fn label(self) -> &'static str {
        match self {
            SetupField::TwitchId => "Twitch client id",
            SetupField::TwitchSecret => "Twitch client secret",
            SetupField::YouTubeId => "YouTube client id",
            SetupField::YouTubeSecret => "YouTube client secret",
        }
    }

    /// Whether the value must be drawn as dots. A secret on screen is a secret
    /// on any recording of that screen.
    pub fn is_secret(self) -> bool {
        matches!(self, SetupField::TwitchSecret | SetupField::YouTubeSecret)
    }

    pub fn platform(self) -> Platform {
        match self {
            SetupField::TwitchId | SetupField::TwitchSecret => Platform::Twitch,
            SetupField::YouTubeId | SetupField::YouTubeSecret => Platform::YouTube,
        }
    }
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
    /// Both at once: a compact strip of channel state above the two chat
    /// panes, for a second monitor where nothing should need switching.
    Combined,
    /// OBS Studio: scenes, microphones, streaming and recording.
    Obs,
}

/// Which half of the combined tab the keyboard is talking to.
///
/// The two halves want the same letters (`r` refreshes statistics on one side
/// and starts a reply on the other), so one of them holds the keyboard at a
/// time and `alt+w` swaps. Alt keeps the swap reachable even from inside the
/// message composer, where every plain letter is text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CombinedFocus {
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
    /// Which half of the combined tab has the keyboard.
    pub combined_focus: CombinedFocus,

    /// The credential boxes on the setup screen, and which one has focus.
    pub setup_inputs: BTreeMap<SetupField, TextInput>,
    pub setup_cursor: usize,
    /// Which platforms the login screen has ticked, and which platforms
    /// already have a saved login.
    pub login_selection: Vec<Platform>,
    pub login_cursor: usize,
    pub logged_in: BTreeMap<Platform, bool>,

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

    /// `true` while the worker is busy, so the UI can show a spinner and refuse
    /// to submit the same plan twice.
    pub busy: bool,
    pub should_quit: bool,
    /// Notifications: what is popped up now, and everything said so far.
    pub toasts: super::toast::Toasts,
    /// How much the interface animates.
    pub animation: crate::anim::Mode,
    /// When this run started.
    ///
    /// Every animation is a function of how long ago this was, so one value
    /// drives all of them and they stay in step with each other for free.
    pub started_at: std::time::Instant,
    /// Set once the start-up splash has been dismissed by a keypress.
    pub splash_skipped: bool,

    /// Which half of the OBS tab has the keyboard, and where each list's
    /// cursor is.
    pub obs_focus: ObsFocus,
    pub obs_scene_cursor: usize,
    pub obs_audio_cursor: usize,

    /// What OBS is doing, as far as this knows.
    pub obs: crate::obs::state::ObsState,
    /// The connection to OBS, when one has been started.
    pub obs_handle: Option<crate::obs::task::Handle>,
    /// Updates from that connection. Taken out by the event loop, which is
    /// the only place allowed to await on it.
    pub obs_updates: Option<tokio::sync::mpsc::UnboundedReceiver<crate::obs::task::Update>>,

    /// What this process is costing the machine, when it is being shown.
    pub telemetry: crate::telemetry::Telemetry,
    /// The command palette, while it is open.
    pub command_palette: Option<super::command_palette::CommandPalette>,
    /// The theme picker, while it is open.
    pub theme_picker: Option<super::theme_picker::ThemePicker>,
    /// The palette every surface is drawn from.
    ///
    /// Held here as well as in the shared skin because the theme picker needs
    /// the colours as text — to show a swatch, to name what is selected, and
    /// to put back what was there if the picker is cancelled.
    pub palette: crate::theme::Palette,
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

        // Where the interface opens depends on how far setup has got. With no
        // API credentials nothing can work, so the credential form comes
        // first; with credentials but no authorised account, the login screen;
        // otherwise straight into the streaming flow.
        let credentials_missing = Platform::ALL
            .iter()
            .all(|platform| config.check_credentials(&[*platform]).is_err());
        let logged_in = saved_logins();
        let screen = if credentials_missing {
            Screen::Setup
        } else if !logged_in.values().any(|yes| *yes) {
            Screen::Login
        } else {
            Screen::Platforms
        };

        let mut setup_inputs = BTreeMap::new();
        for field in SetupField::ORDER {
            let existing = match field {
                SetupField::TwitchId => config.twitch.client_id.clone(),
                SetupField::TwitchSecret => config.twitch.client_secret.clone(),
                SetupField::YouTubeId => config.youtube.client_id.clone(),
                SetupField::YouTubeSecret => config.youtube.client_secret.clone(),
            };
            setup_inputs.insert(field, TextInput::new(existing));
        }

        // Resolve the theme up front. Publishing it is `draw`'s job — it does
        // that once per frame from whatever palette this `App` holds — so
        // there is exactly one place in the program that changes the colours
        // anything is drawn with. An unrecognised name is worth a line in the
        // log but not worth refusing to start over.
        let (palette, recognised) = config.appearance.palette();
        if !recognised {
            tracing::warn!(
                theme = %config.appearance.theme,
                "unknown theme name; using the default palette"
            );
        }
        Self {
            obs_focus: ObsFocus::Scenes,
            obs_scene_cursor: 0,
            obs_audio_cursor: 0,
            obs: crate::obs::state::ObsState::default(),
            obs_handle: None,
            obs_updates: None,
            telemetry: crate::telemetry::Telemetry::default(),
            command_palette: None,
            animation: config.appearance.animation_mode(),
            started_at: std::time::Instant::now(),
            splash_skipped: false,
            theme_picker: None,
            palette,
            tab: Tab::StreamInfo,
            chat: super::chat_tab::ChatTabState::new(&config),
            combined_focus: CombinedFocus::Chat,
            screen,
            config,
            setup_inputs,
            setup_cursor: 0,
            login_selection: Platform::ALL.to_vec(),
            login_cursor: 0,
            logged_in,
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
            busy: false,
            should_quit: false,
            toasts: super::toast::Toasts::default(),
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
    /// Start talking to OBS, if it is configured.
    ///
    /// Deliberately not part of `App::new`. Spawning a task needs a running
    /// async runtime, and a constructor that only works inside one is a trap
    /// — every test that builds an `App` would have to become an async test
    /// to make a connection it does not want. The event loop calls this once,
    /// from inside the runtime, which is the one place that is true.
    ///
    /// There is nothing to wait for: the connection either succeeds in a
    /// millisecond on the local machine, or fails and retries quietly in the
    /// background.
    pub fn connect_obs(&mut self) {
        if !self.config.obs.enabled || self.obs_handle.is_some() {
            return;
        }
        let (updates_tx, updates_rx) = tokio::sync::mpsc::unbounded_channel();
        let params = crate::obs::task::Params {
            url: self.config.obs.url(),
            password: self.config.obs.password(),
            scene_labels: self.config.obs.scene_labels(),
            audio_labels: self.config.obs.audio_labels(),
        };
        tracing::info!(obs = %params.describe(), "connecting to OBS");
        self.obs_handle = Some(crate::obs::task::spawn(params, updates_tx));
        self.obs_updates = Some(updates_rx);
        self.obs.connection = crate::obs::state::Connection::Connecting;
    }

    /// Keys on the OBS tab.
    ///
    /// Vim-flavoured, like the chat panes: `j`/`k` move, `tab` swaps lists,
    /// `enter` acts on the selection. The letters that do something to OBS
    /// are chosen to say what they do — `s` for stream, `r` for record — and
    /// the destructive-looking ones are all toggles, so nothing here can act
    /// on a stale idea of which way round something is.
    fn key_obs(&mut self, key: KeyEvent) -> Vec<Command> {
        use crate::obs::task::Command as ObsCommand;

        // A scene or audio shortcut from the config beats the built-in keys,
        // because somebody who bound `3` to a scene means that scene.
        if is_typed_text(&key) {
            if let KeyCode::Char(c) = key.code {
                let typed = c.to_string();
                if let Some(scene) = self
                    .obs
                    .scenes
                    .iter()
                    .find(|scene| scene.shortcut.as_deref() == Some(typed.as_str()))
                {
                    let name = scene.name.clone();
                    self.obs_command(ObsCommand::SetScene(name));
                    return vec![];
                }
                if let Some(input) = self
                    .obs
                    .audio
                    .iter()
                    .find(|input| input.shortcut.as_deref() == Some(typed.as_str()))
                {
                    let name = input.name.clone();
                    self.obs_command(ObsCommand::ToggleMute(name));
                    return vec![];
                }
            }
        }

        match key.code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Tab
            | KeyCode::Char('h')
            | KeyCode::Char('l')
            | KeyCode::Left
            | KeyCode::Right => {
                self.obs_focus = match self.obs_focus {
                    ObsFocus::Scenes => ObsFocus::Audio,
                    ObsFocus::Audio => ObsFocus::Scenes,
                };
            }
            KeyCode::Char('j') | KeyCode::Down => self.move_obs_cursor(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_obs_cursor(-1),

            // Act on the selection: switch to the scene, or toggle the mute.
            KeyCode::Enter | KeyCode::Char(' ') => match self.obs_focus {
                ObsFocus::Scenes => {
                    if let Some(scene) = self.obs.scenes.get(self.obs_scene_cursor) {
                        let name = scene.name.clone();
                        self.obs_command(ObsCommand::SetScene(name));
                    }
                }
                ObsFocus::Audio => {
                    if let Some(input) = self.obs.audio.get(self.obs_audio_cursor) {
                        let name = input.name.clone();
                        self.obs_command(ObsCommand::ToggleMute(name));
                    }
                }
            },
            // `m` mutes whatever audio input is selected, from either list,
            // because reaching for the microphone should not first require
            // moving the focus to the audio column.
            KeyCode::Char('m') => {
                if let Some(input) = self.obs.audio.get(self.obs_audio_cursor) {
                    let name = input.name.clone();
                    self.obs_command(ObsCommand::ToggleMute(name));
                }
            }
            // Volume, in steps of five: fine enough to be useful, coarse
            // enough that holding the key does something visible.
            KeyCode::Char('+') | KeyCode::Char('=') => self.nudge_obs_volume(0.05),
            KeyCode::Char('-') | KeyCode::Char('_') => self.nudge_obs_volume(-0.05),

            // Mute everything at once, or unmute everything if nothing is
            // live. This is the panic key: saying something into an open
            // microphone that should not have been open is the mistake with
            // the largest consequences available here, and recovering from it
            // should not involve finding the right row first.
            KeyCode::Char('M') => self.mute_all_obs_audio(),

            // Cycle through profiles and scene collections. Cycling rather
            // than a picker because there are usually two or three of each,
            // and a list to choose from would be more interface than the
            // choice deserves.
            KeyCode::Char('P') => self.cycle_obs(true),
            KeyCode::Char('C') => self.cycle_obs(false),

            KeyCode::Char('s') => self.obs_command(ObsCommand::ToggleStream),
            KeyCode::Char('r') => self.obs_command(ObsCommand::ToggleRecord),
            KeyCode::Char('p') => self.obs_command(ObsCommand::ToggleRecordPause),
            KeyCode::Char('R') => {
                self.obs_command(ObsCommand::Reconnect);
                self.push_log(LogLevel::Info, "Reconnecting to OBS…");
            }
            KeyCode::Char('u') => self.obs_command(ObsCommand::Refresh),
            _ => {}
        }
        vec![]
    }

    /// Mute every audio input, or unmute them all if none is live.
    fn mute_all_obs_audio(&mut self) {
        if self.obs.audio.is_empty() {
            return;
        }
        // If anything can still be heard, the intent is silence. Only when
        // everything is already muted does this become an unmute.
        let any_live = self
            .obs
            .audio
            .iter()
            .any(|input| input.muted == Some(false));
        let names: Vec<String> = self
            .obs
            .audio
            .iter()
            .map(|input| input.name.clone())
            .collect();
        for name in names {
            self.obs_command(crate::obs::task::Command::SetMute {
                input: name,
                muted: any_live,
            });
        }
        self.notify(
            super::toast::Level::Info,
            if any_live {
                "OBS: everything muted."
            } else {
                "OBS: everything unmuted."
            },
        );
    }

    /// Move to the next profile, or the next scene collection.
    fn cycle_obs(&mut self, profile: bool) {
        let (list, current) = if profile {
            (&self.obs.profiles, &self.obs.current_profile)
        } else {
            (
                &self.obs.scene_collections,
                &self.obs.current_scene_collection,
            )
        };
        if list.len() < 2 {
            self.notify(
                super::toast::Level::Warning,
                if profile {
                    "OBS has only one profile."
                } else {
                    "OBS has only one scene collection."
                },
            );
            return;
        }
        let index = current
            .as_deref()
            .and_then(|name| list.iter().position(|entry| entry == name))
            .unwrap_or(0);
        let next = list[(index + 1) % list.len()].clone();
        self.obs_command(if profile {
            crate::obs::task::Command::SetProfile(next)
        } else {
            crate::obs::task::Command::SetSceneCollection(next)
        });
    }

    fn move_obs_cursor(&mut self, delta: isize) {
        let (cursor, length) = match self.obs_focus {
            ObsFocus::Scenes => (&mut self.obs_scene_cursor, self.obs.scenes.len()),
            ObsFocus::Audio => (&mut self.obs_audio_cursor, self.obs.audio.len()),
        };
        if length == 0 {
            *cursor = 0;
            return;
        }
        // Wrapping, like every other list in this program.
        *cursor = (*cursor as isize + delta).rem_euclid(length as isize) as usize;
    }

    /// Change the selected input's volume by `delta` of unity gain.
    fn nudge_obs_volume(&mut self, delta: f64) {
        let Some(input) = self.obs.audio.get(self.obs_audio_cursor) else {
            return;
        };
        let Some(current) = input.volume_mul else {
            self.notify(
                super::toast::Level::Warning,
                "That input's volume is not known yet.",
            );
            return;
        };
        let name = input.name.clone();
        // Clamped at the top to unity gain. Amplifying past 100% in OBS is a
        // deliberate act with real consequences for how a stream sounds, and
        // it should not be reachable by leaning on a key.
        let next = (current + delta).clamp(0.0, 1.0);
        self.obs_command(crate::obs::task::Command::SetVolume {
            input: name,
            multiplier: next,
        });
    }

    /// Keep the OBS list cursors inside the lists they point into.
    ///
    /// The lists change underneath them: a scene collection switch replaces
    /// every scene at once, and an input can disappear while its row is
    /// selected. Clamping here means the drawing code never has to.
    fn clamp_obs_cursors(&mut self) {
        self.obs_scene_cursor = self
            .obs_scene_cursor
            .min(self.obs.scenes.len().saturating_sub(1));
        self.obs_audio_cursor = self
            .obs_audio_cursor
            .min(self.obs.audio.len().saturating_sub(1));
    }

    /// Send a command to OBS, if there is a connection to send it to.
    ///
    /// `try_send` rather than an awaited send: this runs on the thread that
    /// draws the screen, and a full queue means the connection task is
    /// already behind. Dropping the command and saying so beats freezing the
    /// interface until OBS answers.
    pub fn obs_command(&mut self, command: crate::obs::task::Command) {
        let Some(handle) = &self.obs_handle else {
            self.notify(
                super::toast::Level::Warning,
                "OBS control is turned off in config.toml.",
            );
            return;
        };
        if handle.commands.try_send(command).is_err() {
            self.notify(
                super::toast::Level::Warning,
                "OBS is busy or not connected — that did nothing.",
            );
        }
    }

    /// Apply one update from the OBS connection.
    ///
    /// Connection changes are worth a line in the activity log; individual
    /// events mostly are not, which is why [`crate::obs::event::Event::describe`]
    /// returns `None` for the frequent ones.
    pub fn handle_obs_update(&mut self, update: crate::obs::task::Update) {
        use crate::obs::state::Connection;
        use crate::obs::task::Update;

        match update {
            Update::Connection(connection) => {
                // Only say something when the state actually changes.
                // Reconnecting every few seconds against a machine with no
                // OBS on it would otherwise fill the log with the same line.
                if self.obs.connection == connection {
                    return;
                }
                let previously_connected = self.obs.connection == Connection::Connected;
                self.obs.connection = connection.clone();

                match &connection {
                    Connection::Connected => {
                        self.push_log(LogLevel::Success, "OBS connected.");
                    }
                    Connection::Failed(reason) => {
                        // Only worth telling somebody about if OBS had been
                        // working: a failure at start-up usually just means
                        // OBS is not running yet, which is not news.
                        if previously_connected {
                            self.push_log(LogLevel::Warning, format!("OBS: {reason}"));
                        } else {
                            tracing::debug!(reason = %reason, "OBS not reachable");
                        }
                        self.obs.clear_live_data();
                        self.obs.connection = connection;
                    }
                    Connection::Reconnecting | Connection::Idle | Connection::Connecting => {
                        if previously_connected {
                            self.push_log(LogLevel::Warning, "OBS disconnected.");
                            self.obs.clear_live_data();
                            self.obs.connection = connection;
                        }
                    }
                }
            }
            Update::Snapshot(state) | Update::CommandDone(state) => {
                let connection = self.obs.connection.clone();
                self.obs = *state;
                // The snapshot is built by the connection task, which knows
                // it is connected; the interface's view of the connection is
                // the authority on anything else.
                if connection != Connection::Connected {
                    self.obs.connection = Connection::Connected;
                }
                self.clamp_obs_cursors();
            }
            Update::Event(event) => {
                event.apply(&mut self.obs);
                if let Some(line) = event.describe() {
                    self.push_log(LogLevel::Info, line);
                }
                self.clamp_obs_cursors();
            }
            Update::CommandFailed(reason) => {
                self.push_log(LogLevel::Error, format!("OBS: {reason}"));
            }
        }
    }

    /// Raise a notification.
    ///
    /// Notifications and the activity log answer two different questions.
    /// The log is the record of what the program did, in order, and it is
    /// there to be read after the fact. A notification is for the moment it
    /// happens — it appears over whatever you are looking at, whether that is
    /// the dashboard, a chat pane or the combined tab, and it goes away on
    /// its own.
    pub fn notify(&mut self, level: super::toast::Level, text: impl Into<String>) {
        if !self.config.appearance.toasts {
            return;
        }
        self.toasts
            .push(level, text, self.config.appearance.toast_duration());
    }

    pub fn push_log(&mut self, level: LogLevel, message: impl Into<String>) {
        let message = message.into();

        // Anything that went wrong is also raised as a notification. The log
        // lives at the bottom of the Stream Info tab, so on the Chat or
        // Combined tab it is not on screen at all — without this, a failure
        // while you were reading chat would be silent until you went looking
        // for it. Ordinary progress stays in the log only: a notification for
        // every routine step would train you to ignore them.
        match level {
            LogLevel::Error => self.notify(super::toast::Level::Error, message.clone()),
            LogLevel::Warning => self.notify(super::toast::Level::Warning, message.clone()),
            LogLevel::Info | LogLevel::Success => {}
        }

        self.log.push_back(LogLine {
            level,
            message,
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

    /// Fold a message from the worker into the state, returning any follow-up
    /// work. Finishing a login, for instance, immediately connects — the point
    /// of logging in was to get to the main view.
    pub fn handle_event(&mut self, event: Event) -> Vec<Command> {
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
                // With at least one platform connected, open the main view: it
                // shows each channel's current state (live or not, viewers,
                // audience) alongside the stream info that would be applied,
                // and `e` from there opens the form to edit it. The form used
                // to open directly, which meant the state of the channel you
                // were about to overwrite was never shown.
                if self.accounts.values().any(|r| r.is_ok()) {
                    // The set of connected platforms decides which form fields
                    // exist, so make sure the cursor is not left on a hidden
                    // one before the form is ever opened.
                    self.ensure_field_visible();
                    self.go_to(Screen::Dashboard);
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
                    return vec![];
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
                    return vec![];
                }
                self.busy = false;
                let any_ok = results.iter().any(|r| r.succeeded());
                self.results = results;
                if any_ok {
                    self.go_to(Screen::Dashboard);
                    self.notify(
                        super::toast::Level::Success,
                        "Ready — start streaming in OBS whenever you like.",
                    );
                } else {
                    self.notify(
                        super::toast::Level::Error,
                        "Every platform failed. See the log below for why.",
                    );
                }
            }

            Event::LoggedIn { platform, result } => {
                self.busy = false;
                match result {
                    Ok(_) => {
                        self.logged_in.insert(platform, true);
                        self.push_log(
                            LogLevel::Success,
                            format!("{} authorised.", platform.label()),
                        );
                    }
                    Err(err) => {
                        self.push_log(
                            LogLevel::Error,
                            format!("{} login failed: {err}", platform.label()),
                        );
                    }
                }

                // Once something is authorised, go straight to the main view
                // rather than making the user re-pick platforms: connecting is
                // what shows the channel's current state.
                if self.screen == Screen::Login && self.logged_in.values().any(|yes| *yes) {
                    let connected: Vec<Platform> = Platform::ALL
                        .iter()
                        .copied()
                        .filter(|p| self.logged_in.get(p).copied().unwrap_or(false))
                        .collect();
                    self.selected = connected.clone();
                    self.ensure_field_visible();
                    self.go_to(Screen::Platforms);
                    self.busy = true;
                    return vec![Command::Connect(connected)];
                }
            }

            Event::Stats(stats) => {
                self.stats = stats.into_iter().collect();
            }
        }
        vec![]
    }

    /// Handle a key press, returning any work for the worker to do.
    pub fn handle_key(&mut self, key: KeyEvent) -> Vec<Command> {
        // Notifications expire on their own timer rather than on the next
        // keypress. A message that disappears the moment you touch a key is a
        // message you cannot read while you are working, which is exactly
        // when they arrive.
        self.toasts.expire(std::time::Instant::now());

        // Ctrl+C always quits, on every screen, even mid-request.
        if key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('c')) {
            self.should_quit = true;
            return vec![];
        }

        // Any key dismisses the start-up splash and is then swallowed. The
        // key is deliberately not passed on to whatever is underneath: at the
        // moment it was pressed the user was looking at the splash, not at
        // the screen behind it, so acting on it would act on something they
        // could not see. Ctrl+C is the exception, handled just above, because
        // "stop" must always mean stop.
        if self.splash_is_showing() {
            self.splash_skipped = true;
            return vec![];
        }

        // The command palette owns the keyboard while it is open, because
        // every letter is part of the query rather than a shortcut.
        if self.command_palette.is_some() {
            return self.key_command_palette(key);
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('p')) {
            self.command_palette = Some(super::command_palette::CommandPalette::default());
            return vec![];
        }

        // The message history is modal: while it is open it owns the screen
        // and every key, exactly like a vim `:messages` listing.
        if self.toasts.history_open {
            return self.key_message_history(key);
        }

        // The theme picker takes the whole screen and every key while it is
        // open, so it is handled before anything else can claim a key. It is
        // checked ahead of the text fields deliberately: ctrl+t has to work
        // while a message is half-typed, and a control-modified key is never
        // text anyway.
        if self.theme_picker.is_some() {
            return self.key_theme_picker(key);
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('t')) {
            self.theme_picker = Some(super::theme_picker::ThemePicker::open(
                &self.config.appearance.theme,
                &self.palette,
            ));
            return vec![];
        }

        // Alt+digit switches top-level tabs from anywhere — including inside
        // a text field, because the Alt modifier keeps it unambiguous.
        if key.modifiers.contains(KeyModifiers::ALT) {
            match key.code {
                // Swap which half of the combined tab the keyboard talks to.
                KeyCode::Char('w') if self.tab == Tab::Combined => {
                    self.combined_focus = match self.combined_focus {
                        CombinedFocus::Chat => CombinedFocus::StreamInfo,
                        CombinedFocus::StreamInfo => CombinedFocus::Chat,
                    };
                    self.chat.pending_mod = None;
                    self.chat.pending_space = false;
                    return vec![];
                }
                // Cycle how much the interface animates, without going to
                // the config file for it: whether motion is comfortable is
                // something you find out by looking at it.
                KeyCode::Char('a') => {
                    self.chat.pending_mod = None;
                    self.chat.pending_space = false;
                    self.animation = self.animation.next();
                    self.config.appearance.animations = self.animation.name().to_string();
                    let mode = self.animation.name();
                    self.notify(super::toast::Level::Info, format!("Animations: {mode}"));
                    return self.save_appearance();
                }
                // Show or hide the process telemetry in the header.
                KeyCode::Char('t') => {
                    self.chat.pending_mod = None;
                    self.chat.pending_space = false;
                    self.config.appearance.telemetry = !self.config.appearance.telemetry;
                    let state = if self.config.appearance.telemetry {
                        "on"
                    } else {
                        "off"
                    };
                    self.notify(super::toast::Level::Info, format!("Telemetry: {state}"));
                    return self.save_appearance();
                }
                // Open the message history — vim's `:messages`, on a key.
                KeyCode::Char('m') => {
                    self.chat.pending_mod = None;
                    self.chat.pending_space = false;
                    // Opening the history takes the pop-ups off the screen:
                    // every one of them is in the list you are now looking
                    // at, so leaving them stacked on top of it would only
                    // cover the entries they duplicate.
                    self.toasts.dismiss_all();
                    self.toasts.open_history();
                    return vec![];
                }
                // The OBS tab. Entering it asks for a fresh look at OBS,
                // since anything may have changed there while it was not on
                // screen and no events arrive for a scene collection swap.
                KeyCode::Char('4') => {
                    self.chat.pending_mod = None;
                    self.chat.pending_space = false;
                    if self.chat_is_showing() {
                        self.chat.deactivate();
                    }
                    self.tab = Tab::Obs;
                    self.obs_command(crate::obs::task::Command::Refresh);
                    return vec![];
                }
                KeyCode::Char('3') => {
                    self.chat.pending_mod = None;
                    self.chat.pending_space = false;
                    self.tab = Tab::Combined;
                    self.combined_focus = CombinedFocus::Chat;
                    self.chat.activate(&self.config);
                    return vec![];
                }
                KeyCode::Char('1') => {
                    if self.chat_is_showing() {
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

        if self.chat_has_the_keyboard() {
            return self.key_chat(key);
        }

        if self.tab == Tab::Obs {
            return self.key_obs(key);
        }

        match self.screen {
            Screen::Setup => self.key_setup(key),
            Screen::Login => self.key_login(key),
            Screen::Platforms => self.key_platforms(key),
            Screen::Form => self.key_form(key),
            Screen::Dashboard => self.key_dashboard(key),
        }
    }

    /// Whether the chat panes are on screen at all.
    pub fn chat_is_showing(&self) -> bool {
        matches!(self.tab, Tab::Chat | Tab::Combined)
    }

    /// Whether key presses belong to the chat panes. On the Chat tab they
    /// always do; on the combined tab only while that half has the focus.
    fn chat_has_the_keyboard(&self) -> bool {
        match self.tab {
            Tab::Chat => true,
            Tab::Combined => self.combined_focus == CombinedFocus::Chat,
            Tab::StreamInfo | Tab::Obs => false,
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
                    // Tab completes a trailing @mention from the roster.
                    KeyCode::Tab => self.chat.complete_mention(),
                    KeyCode::Char('e') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        self.chat.mode = ChatFocus::EmojiPicker {
                            query: String::new(),
                            from_compose: true,
                        };
                    }
                    KeyCode::Char(c) if is_typed_text(&key) => self.chat.compose_push(c),
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
                    KeyCode::Char(c) if is_typed_text(&key) => {
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
                    KeyCode::Char(c) if is_typed_text(&key) => {
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
                    KeyCode::Char(c) if is_typed_text(&key) => {
                        buffer.push(c);
                        self.chat.mode = ChatFocus::TimeoutPrompt(buffer);
                    }
                    _ => {}
                }
                return vec![];
            }
            ChatFocus::EmojiPicker {
                query: mut buffer,
                from_compose,
            } => {
                match key.code {
                    // Esc goes back where the picker came from — a Normal-mode
                    // user must not land in the composer uninvited.
                    KeyCode::Esc => {
                        self.chat.mode = if from_compose {
                            ChatFocus::Compose
                        } else {
                            ChatFocus::Normal
                        };
                    }
                    KeyCode::Enter | KeyCode::Tab => {
                        if let Some(entry) =
                            crate::chat::emoji::search(&buffer, 1).into_iter().next()
                        {
                            self.chat.insert_emoji(entry.emoji);
                        }
                        // Inserting is a composing act: the draft now holds
                        // the emoji, so the composer is where it can be seen.
                        self.chat.mode = ChatFocus::Compose;
                    }
                    KeyCode::Backspace => {
                        buffer.pop();
                        self.chat.mode = ChatFocus::EmojiPicker {
                            query: buffer,
                            from_compose,
                        };
                    }
                    KeyCode::Char(c) if is_typed_text(&key) => {
                        buffer.push(c);
                        self.chat.mode = ChatFocus::EmojiPicker {
                            query: buffer,
                            from_compose,
                        };
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
            match key.code {
                KeyCode::Char('r') => self.chat.reconnect_active(),
                KeyCode::Char('g') => self.chat.cycle_layout(),
                KeyCode::Char('b') => self.chat.cycle_badges(),
                KeyCode::Char('y') => self.chat.toggle_highlight(),
                KeyCode::Char('n') => self.chat.toggle_full_username(),
                // Same guard as entering the composer: with no chat open
                // there is no draft an emoji could land in.
                KeyCode::Char('e') if self.chat.active_key(self.chat.focus).is_some() => {
                    self.chat.mode = ChatFocus::EmojiPicker {
                        query: String::new(),
                        from_compose: false,
                    };
                }
                _ => {}
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
                } else {
                    // There is nowhere to type. Saying so beats doing
                    // nothing: a key that silently ignores you is
                    // indistinguishable from a key that is broken.
                    let platform = self.chat.focus.label();
                    self.notify(
                        super::toast::Level::Warning,
                        format!("No {platform} chat is open to write to yet."),
                    );
                }
            }
            // k moves the selection back in history (bigger offset from the
            // bottom), j toward the tail — the vim direction sense over a
            // bottom-anchored log. The view follows the selection.
            KeyCode::Char('k') | KeyCode::Up => self.chat.select_move(1),
            KeyCode::Char('j') | KeyCode::Down => self.chat.select_move(-1),
            KeyCode::PageUp => self.chat.scroll_by(10),
            KeyCode::PageDown => self.chat.scroll_by(-10),
            KeyCode::Esc => {
                if self.chat.inspect {
                    self.chat.inspect = false;
                } else {
                    self.chat.clear_selection();
                }
            }
            KeyCode::Char('K') => self.chat.inspect = !self.chat.inspect,
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
                    // A confirmation armed before the prompt must not survive
                    // the modal round-trip and fire on one later keypress.
                    self.chat.pending_mod = None;
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

    // -- Screen 0a: typing in the API credentials ---------------------------

    /// The focused credential box.
    pub fn setup_field(&self) -> SetupField {
        SetupField::ORDER[self.setup_cursor.min(SetupField::ORDER.len() - 1)]
    }

    /// Whether enough has been typed in for at least one platform to work.
    ///
    /// A platform needs *both* halves; one on its own is not a usable
    /// configuration, so the form does not accept it as one.
    pub fn setup_is_complete(&self) -> bool {
        Platform::ALL.iter().any(|platform| {
            SetupField::ORDER
                .iter()
                .filter(|field| field.platform() == *platform)
                .all(|field| {
                    self.setup_inputs
                        .get(field)
                        .is_some_and(|input| !input.value().trim().is_empty())
                })
        })
    }

    fn key_setup(&mut self, key: KeyEvent) -> Vec<Command> {
        match key.code {
            KeyCode::Tab | KeyCode::Down => {
                self.setup_cursor = (self.setup_cursor + 1) % SetupField::ORDER.len();
            }
            KeyCode::BackTab | KeyCode::Up => {
                self.setup_cursor =
                    (self.setup_cursor + SetupField::ORDER.len() - 1) % SetupField::ORDER.len();
            }
            KeyCode::Enter => return self.save_credentials(),
            KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                return self.save_credentials()
            }
            KeyCode::Esc => {
                // Leaving without saving is only allowed when there is
                // somewhere to go: on a first run there is no configured state
                // to return to, so Esc quits rather than stranding the user on
                // an empty picker.
                if self.config.check_credentials(&[Platform::Twitch]).is_ok()
                    || self.config.check_credentials(&[Platform::YouTube]).is_ok()
                {
                    self.go_to(Screen::Login);
                } else {
                    self.should_quit = true;
                }
            }
            KeyCode::Backspace => {
                let field = self.setup_field();
                if let Some(input) = self.setup_inputs.get_mut(&field) {
                    input.backspace();
                }
            }
            KeyCode::Left => {
                let field = self.setup_field();
                if let Some(input) = self.setup_inputs.get_mut(&field) {
                    input.left();
                }
            }
            KeyCode::Right => {
                let field = self.setup_field();
                if let Some(input) = self.setup_inputs.get_mut(&field) {
                    input.right();
                }
            }
            KeyCode::Char(c) if is_typed_text(&key) => {
                let field = self.setup_field();
                if let Some(input) = self.setup_inputs.get_mut(&field) {
                    input.insert(c);
                }
            }
            _ => {}
        }
        vec![]
    }

    /// Copy the typed credentials into the config, save it, and move on.
    ///
    /// The saved file keeps its comments (the setup guidance `msm init`
    /// writes) because saving goes through the same comment-preserving path
    /// the form's "save defaults" uses. Nothing here ever logs a secret.
    fn save_credentials(&mut self) -> Vec<Command> {
        if !self.setup_is_complete() {
            self.notify(
                super::toast::Level::Warning,
                "Fill in both the client id and the client secret for at least one platform.",
            );
            return vec![];
        }

        for field in SetupField::ORDER {
            let value = self
                .setup_inputs
                .get(&field)
                .map(|input| input.value().trim().to_string())
                .unwrap_or_default();
            match field {
                SetupField::TwitchId => self.config.twitch.client_id = value,
                SetupField::TwitchSecret => self.config.twitch.client_secret = value,
                SetupField::YouTubeId => self.config.youtube.client_id = value,
                SetupField::YouTubeSecret => self.config.youtube.client_secret = value,
            }
        }

        match self.config.save() {
            Ok(()) => {
                self.push_log(LogLevel::Success, "API credentials saved to config.toml.");
                self.go_to(Screen::Login);
                // The worker holds its own copy of the config and would
                // otherwise keep building backends from the old credentials.
                vec![Command::ReloadConfig(Box::new(self.config.clone()))]
            }
            Err(err) => {
                self.push_log(
                    LogLevel::Error,
                    format!("Could not save the credentials: {err:#}"),
                );
                vec![]
            }
        }
    }

    // -- Screen 0b: logging in ----------------------------------------------

    /// The platforms the login screen would authorise right now: the ticked
    /// ones that actually have credentials configured.
    pub fn login_targets(&self) -> Vec<Platform> {
        self.login_selection
            .iter()
            .copied()
            .filter(|platform| self.config.check_credentials(&[*platform]).is_ok())
            .collect()
    }

    /// A mouse click or wheel movement.
    ///
    /// The mouse does a deliberately small number of things — pick a tab,
    /// pick a pane, scroll — and everything it does has a key that does the
    /// same. It is routed through the same handlers as those keys rather than
    /// acting directly, for the same reason the command palette is: two
    /// implementations of one action drift apart.
    pub fn handle_mouse(
        &mut self,
        event: crossterm::event::MouseEvent,
        area: ratatui::layout::Rect,
    ) -> Vec<Command> {
        use super::mouse::Action;

        if !self.config.appearance.mouse {
            return vec![];
        }
        // While the splash or a modal overlay is up, the thing under the
        // pointer is not the thing being drawn there. Scrolling still works,
        // since a long list is exactly what a wheel is for, but a click would
        // land on whatever happened to be underneath.
        let overlay_open =
            self.splash_is_showing() || self.toasts.history_open || self.theme_picker.is_some();

        let action = super::mouse::action_for(
            event,
            area,
            self.chat_is_showing(),
            self.tab == Tab::Combined,
            self.chat.split_percent,
        );
        let Some(action) = action else { return vec![] };

        match action {
            Action::SelectTab(_) | Action::FocusChat(_) | Action::FocusStreamInfo
                if overlay_open =>
            {
                vec![]
            }
            Action::SelectTab(index) => {
                let key = match index {
                    0 => '1',
                    1 => '2',
                    _ => '3',
                };
                self.handle_key(KeyEvent::new(KeyCode::Char(key), KeyModifiers::ALT))
            }
            Action::FocusChat(platform) => {
                // On the combined tab the keyboard may be on the stream-info
                // half, so clicking a chat pane has to move it across as well
                // as choose the pane.
                if self.tab == Tab::Combined {
                    self.combined_focus = CombinedFocus::Chat;
                }
                self.chat.focus = platform;
                vec![]
            }
            Action::FocusStreamInfo => {
                self.combined_focus = CombinedFocus::StreamInfo;
                vec![]
            }
            Action::ScrollBack => self.scroll(true),
            Action::ScrollForward => self.scroll(false),
        }
    }

    /// Scroll whatever is currently scrollable, in the direction the wheel
    /// turned: the message history when it is open, the chat when it is
    /// showing, and otherwise the activity log.
    fn scroll(&mut self, back: bool) -> Vec<Command> {
        const WHEEL_LINES: isize = 3;
        if self.toasts.history_open {
            self.toasts
                .scroll_history(if back { WHEEL_LINES } else { -WHEEL_LINES });
        } else if self.chat_has_the_keyboard() {
            for _ in 0..WHEEL_LINES {
                self.chat.select_move(if back { 1 } else { -1 });
            }
        } else if back {
            self.log_scroll_back =
                (self.log_scroll_back + WHEEL_LINES as usize).min(self.log.len().saturating_sub(1));
        } else {
            self.log_scroll_back = self.log_scroll_back.saturating_sub(WHEEL_LINES as usize);
        }
        vec![]
    }

    /// Keys while the command palette is open.
    ///
    /// Choosing an entry replays the keys that entry stands for, through the
    /// same `handle_key` everything else goes through. The palette therefore
    /// cannot do anything a key could not, and cannot drift away from what
    /// the key actually does.
    fn key_command_palette(&mut self, key: KeyEvent) -> Vec<Command> {
        let Some(palette) = self.command_palette.as_mut() else {
            return vec![];
        };
        match key.code {
            KeyCode::Esc => {
                self.command_palette = None;
            }
            KeyCode::Enter => {
                let keys: Vec<KeyEvent> = palette
                    .chosen()
                    .map(|entry| entry.keys.iter().map(|key| key.event()).collect())
                    .unwrap_or_default();
                // Close the palette *before* replaying, or the replayed key
                // would be typed straight back into the query box.
                self.command_palette = None;
                let mut commands = Vec::new();
                for key in keys {
                    commands.extend(self.handle_key(key));
                }
                return commands;
            }
            KeyCode::Up => palette.move_by(-1),
            KeyCode::Down | KeyCode::Tab => palette.move_by(1),
            KeyCode::Backspace => palette.backspace(),
            // Text, but only text. A control- or alt-modified key is a
            // shortcut somebody pressed out of habit, not a letter they meant
            // to search for — typing "m" into the query because they reached
            // for alt+m would be baffling.
            KeyCode::Char(c) if is_typed_text(&key) => palette.push(c),
            _ => {}
        }
        vec![]
    }

    /// Keys while the modal message history is open.
    fn key_message_history(&mut self, key: KeyEvent) -> Vec<Command> {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.toasts.close_history(),
            KeyCode::Up | KeyCode::Char('k') => self.toasts.scroll_history(1),
            KeyCode::Down | KeyCode::Char('j') => self.toasts.scroll_history(-1),
            KeyCode::PageUp => self.toasts.scroll_history(10),
            KeyCode::PageDown => self.toasts.scroll_history(-10),
            // `g` jumps to the newest, the way `G` jumps to the end of a file.
            KeyCode::Char('g') => self.toasts.scroll_history(isize::MIN / 2),
            KeyCode::Char('G') => self.toasts.scroll_history(isize::MAX / 2),
            _ => {}
        }
        vec![]
    }

    /// Keys while the theme picker is open.
    ///
    /// Every movement applies the theme under the cursor immediately, which is
    /// what makes this a preview rather than a list of names. `Enter` keeps
    /// it and writes it to the config file; `Esc` restores whatever was in use
    /// before the picker opened.
    fn key_theme_picker(&mut self, key: KeyEvent) -> Vec<Command> {
        let Some(picker) = self.theme_picker.as_mut() else {
            return vec![];
        };
        match key.code {
            KeyCode::Esc => {
                let palette = picker.original_palette.clone();
                self.theme_picker = None;
                self.apply_palette(palette);
                return vec![];
            }
            KeyCode::Enter => return self.save_theme(),
            KeyCode::Up | KeyCode::Char('k') => picker.move_by(-1),
            KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => picker.move_by(1),
            KeyCode::PageUp => picker.move_by(-10),
            KeyCode::PageDown => picker.move_by(10),
            KeyCode::Home => picker.move_to(0),
            KeyCode::End => {
                let last = picker.last_index();
                picker.move_to(last);
            }
            _ => return vec![],
        }
        self.preview_selected_theme();
        vec![]
    }

    /// Apply the palette under the picker's cursor, without saving it.
    fn preview_selected_theme(&mut self) {
        let Some(picker) = self.theme_picker.as_ref() else {
            return;
        };
        let custom = self.config.appearance.custom_theme.to_palette();
        let (palette, _) = crate::theme::resolve(&picker.selected_name(), &custom);
        self.apply_palette(palette);
    }

    /// How long this run has been going, which is what every animation is a
    /// function of.
    pub fn elapsed(&self) -> std::time::Duration {
        self.started_at.elapsed()
    }

    /// Whether anything on screen is currently moving.
    ///
    /// The event loop uses this to decide whether to run the ten-a-second
    /// animation clock at all. When nothing is animating it stays parked, so
    /// an idle interface costs two redraws a second rather than ten.
    pub fn is_animating(&self) -> bool {
        if self.animation == crate::anim::Mode::Off {
            // Notifications still have to disappear when their time is up, so
            // even with motion turned off there has to be a tick while any
            // are showing — it simply removes them rather than fading them.
            return self.toasts.showing();
        }
        self.splash_is_showing() || self.toasts.showing()
    }

    /// Whether the start-up splash is still covering the interface.
    pub fn splash_is_showing(&self) -> bool {
        super::splash::is_showing(
            self.elapsed(),
            self.splash_skipped,
            self.config.appearance.splash,
        )
    }

    /// Make `palette` the one every subsequent frame is drawn from.
    ///
    /// Storing it is all that is needed: the next frame publishes it.
    fn apply_palette(&mut self, palette: crate::theme::Palette) {
        self.palette = palette;
    }

    /// Write an `[appearance]` change back to the config file.
    ///
    /// A setting toggled from a key has to survive a restart, or it is not a
    /// setting — it is a thing you have to redo every session. A failed write
    /// is reported and the change stays in effect for this run.
    fn save_appearance(&mut self) -> Vec<Command> {
        match self.config.save() {
            Ok(()) => vec![Command::ReloadConfig(Box::new(self.config.clone()))],
            Err(err) => {
                self.notify(
                    super::toast::Level::Error,
                    format!("Could not save that setting: {err:#}"),
                );
                vec![]
            }
        }
    }

    /// Keep the previewed theme: write the name into the config file and close
    /// the picker.
    ///
    /// A failed write leaves the picker open showing why. Closing it on a
    /// failure would look exactly like success and then quietly forget the
    /// choice at the next start-up.
    fn save_theme(&mut self) -> Vec<Command> {
        let Some(picker) = self.theme_picker.as_mut() else {
            return vec![];
        };
        let chosen = picker.selected_name();
        let previous = self.config.appearance.theme.clone();
        self.config.appearance.theme = chosen.clone();
        match self.config.save() {
            Ok(()) => {
                self.theme_picker = None;
                self.push_log(LogLevel::Info, format!("Theme saved: {chosen}"));
                // The worker holds its own copy of the config, so it has to be
                // told as well or the next thing it writes would put the old
                // theme name back.
                vec![Command::ReloadConfig(Box::new(self.config.clone()))]
            }
            Err(err) => {
                self.config.appearance.theme = previous;
                picker.save_error = Some(format!("{err:#}"));
                vec![]
            }
        }
    }

    fn key_login(&mut self, key: KeyEvent) -> Vec<Command> {
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.login_cursor = self.login_cursor.saturating_sub(1)
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.login_cursor = (self.login_cursor + 1).min(Platform::ALL.len() - 1)
            }
            KeyCode::Char(' ') => {
                let platform = Platform::ALL[self.login_cursor];
                if let Some(index) = self.login_selection.iter().position(|p| *p == platform) {
                    self.login_selection.remove(index);
                } else {
                    self.login_selection.push(platform);
                    self.login_selection.sort();
                }
            }
            KeyCode::Char('c') => {
                // Back to the credential form to correct a typo in an id or
                // secret without quitting.
                self.go_to(Screen::Setup);
            }
            KeyCode::Char('s') => {
                // Skip: carry on with whatever logins already exist.
                if self.logged_in.values().any(|yes| *yes) {
                    self.go_to(Screen::Platforms);
                } else {
                    self.notify(
                        super::toast::Level::Warning,
                        "Nothing is authorised yet, so there is nothing to skip to.",
                    );
                }
            }
            KeyCode::Enter => {
                if self.busy {
                    return vec![];
                }
                let targets = self.login_targets();
                if targets.is_empty() {
                    self.notify(
                        super::toast::Level::Warning,
                        "Tick a platform whose credentials are configured (Space), or press c to \
                         enter credentials.",
                    );
                    return vec![];
                }
                self.busy = true;
                self.push_log(
                    LogLevel::Info,
                    "Your browser will open — approve the access there, then come back.",
                );
                return vec![Command::Login(targets)];
            }
            KeyCode::Char('q') | KeyCode::Esc => self.should_quit = true,
            _ => {}
        }
        vec![]
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
                    self.notify(
                        super::toast::Level::Warning,
                        "Tick at least one platform with Space first.",
                    );
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
            self.notify(super::toast::Level::Warning, "Already working — hold on.");
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
            let message = blocking[0].message.clone();
            self.notify(super::toast::Level::Warning, message);
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
                self.notify(super::toast::Level::Success, "Saved to config.toml.");
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
            // A stream key is never shown, only copied: this window is very
            // often part of the broadcast, and anyone who reads the key can
            // stream to your channel. `y` follows the vim convention for
            // "yank"; the platform is chosen by the case rather than by a
            // prompt so there is no mode to get stuck in.
            KeyCode::Char('y') => return self.copy_stream_key(Platform::Twitch),
            KeyCode::Char('Y') => return self.copy_stream_key(Platform::YouTube),
            KeyCode::Char('o') => {
                // Open the watch page, which is the one link you actually want
                // in front of you once the stream is up — to check the delay, to
                // paste into chat, or to see what viewers see.
                match self.first_watch_url() {
                    Some(url) => {
                        self.notify(super::toast::Level::Info, format!("Opening {url}"));
                        return vec![Command::OpenUrl(url)];
                    }
                    None => {
                        self.notify(
                            super::toast::Level::Warning,
                            "No platform has a watch page yet — nothing has gone live.",
                        );
                    }
                }
            }
            KeyCode::Char('e') | KeyCode::Esc => {
                // Back to the form to change something and resubmit. On YouTube
                // this creates a *new* broadcast rather than editing the old one.
                self.go_to(Screen::Form);
                self.ensure_field_visible();
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

    /// Ask the worker to copy a platform's stream key to the clipboard.
    ///
    /// The key is fetched and copied entirely inside the worker; this half
    /// only says which platform, and hears back through the activity log
    /// whether it worked.
    fn copy_stream_key(&mut self, platform: Platform) -> Vec<Command> {
        if !self.is_selected(platform) {
            self.notify(
                super::toast::Level::Warning,
                format!("{} is not one of the selected platforms.", platform.label()),
            );
            return vec![];
        }
        self.notify(
            super::toast::Level::Info,
            format!("Copying the {} stream key…", platform.label()),
        );
        vec![Command::CopyStreamKey(platform)]
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

    /// A config whose `save()` writes to a scratch file of its own.
    ///
    /// Some keys — `alt+a` and `alt+t` — persist a setting as part of doing
    /// their job, which is correct behaviour and a hazard in a test: without
    /// this, `save()` falls back to the real per-user config path and a test
    /// run would rewrite the config file of whoever is running it. Pointing
    /// `source_path` at a scratch file keeps every write inside the test.
    ///
    /// Deliberately not the `MSM_CONFIG_DIR` override: that is an environment
    /// variable, which is process-wide, and these tests run in parallel.
    fn scratch_config() -> Config {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let mut config = Config::default();
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("msm-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a scratch directory");
        config.source_path = Some(dir.join(format!("config-{unique}.toml")));
        config
    }

    fn app() -> App {
        // `App::new` opens on the setup or login screen when nothing is
        // configured, which is not what most of these tests are about; they
        // drive the streaming flow, so they start at the platform picker.
        let mut app = App::new(scratch_config());
        // The start-up splash would otherwise cover the screen these tests
        // are looking at, and swallow the keys they send.
        app.splash_skipped = true;
        app.screen = Screen::Platforms;
        // The start-up splash covers the interface and swallows the first
        // keypress, which is right for a real session and wrong for a test
        // that wants to drive the screen underneath it.
        app.splash_skipped = true;
        app
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
        assert!(app
            .toasts
            .visible_text()
            .iter()
            .any(|text| text.contains("at least one")));
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
        assert!(app
            .toasts
            .visible_text()
            .iter()
            .any(|text| text.contains("failed")));
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
        assert_eq!(app.screen, Screen::Dashboard);
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
        assert_eq!(app.screen, Screen::Dashboard);

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

    /// Only alt+1 and alt+2 are consumed by the tab switcher; every other alt
    /// combination used to fall through and type its bare character into the
    /// open chat input, so a stray "3" landed in the message being written.
    #[test]
    fn alt_modified_keys_are_not_typed_into_a_chat_input() {
        let mut app = App::new(Config::default());
        // The start-up splash would otherwise cover the screen these tests
        // are looking at, and swallow the keys they send.
        app.splash_skipped = true;
        app.tab = Tab::Chat;
        app.chat.mode = super::super::chat_tab::ChatFocus::Join(String::new());

        app.handle_key(KeyEvent::new(KeyCode::Char('3'), KeyModifiers::ALT));

        match &app.chat.mode {
            super::super::chat_tab::ChatFocus::Join(buffer) => {
                assert!(buffer.is_empty(), "alt+3 must not type a 3: {buffer:?}");
            }
            other => panic!("the join prompt should still be open, got {other:?}"),
        }
    }

    /// The combined tab shows both halves, and `alt+w` decides which one the
    /// keyboard is talking to — the two halves want the same letters.
    #[test]
    fn the_combined_tab_hands_the_keyboard_to_one_half_at_a_time() {
        let mut app = app();
        app.screen = Screen::Dashboard;
        app.selected = vec![Platform::Twitch];

        app.handle_key(KeyEvent::new(KeyCode::Char('3'), KeyModifiers::ALT));
        assert_eq!(app.tab, Tab::Combined);
        assert_eq!(app.combined_focus, CombinedFocus::Chat);

        // With the chat half focused, dashboard keys do not fire: `y` is a
        // chat key there, not "copy the stream key".
        let split = app.chat.split_percent;
        assert!(app.handle_key(key(KeyCode::Char('y'))).is_empty());
        // …and a chat key does act: `<` narrows the focused pane.
        app.handle_key(key(KeyCode::Char('<')));
        assert_ne!(app.chat.split_percent, split);

        app.handle_key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::ALT));
        assert_eq!(app.combined_focus, CombinedFocus::StreamInfo);

        // Now the same keys reach the dashboard: `y` copies the stream key.
        let commands = app.handle_key(key(KeyCode::Char('y')));
        assert!(matches!(
            commands.as_slice(),
            [Command::CopyStreamKey(Platform::Twitch)]
        ));
    }

    /// Leaving a tab that shows chat must mark the chats hidden, whichever of
    /// the two chat-showing tabs it was.
    #[test]
    fn leaving_the_combined_tab_hides_the_chats() {
        let mut app = app();
        app.handle_key(KeyEvent::new(KeyCode::Char('3'), KeyModifiers::ALT));
        assert!(app.chat_is_showing());

        app.handle_key(KeyEvent::new(KeyCode::Char('1'), KeyModifiers::ALT));

        assert_eq!(app.tab, Tab::StreamInfo);
        assert!(!app.chat_is_showing());
    }

    /// A fresh install opens on the credential form rather than on a picker
    /// whose choices cannot work, and typing both halves of one platform is
    /// enough to save and move on to the login screen.
    #[test]
    fn setup_saves_credentials_and_moves_to_the_login_screen() {
        let scratch = crate::paths::test_support::ScratchConfigDir::new("app-setup-save");
        let mut app = App::new(Config::default());
        // The start-up splash would otherwise cover the screen these tests
        // are looking at, and swallow the keys they send.
        app.splash_skipped = true;
        assert_eq!(app.screen, Screen::Setup);

        // Enter is refused until at least one platform is complete.
        for c in "abc".chars() {
            app.handle_key(key(KeyCode::Char(c)));
        }
        let commands = app.handle_key(key(KeyCode::Enter));
        assert!(commands.is_empty());
        assert_eq!(app.screen, Screen::Setup, "half a platform is not enough");

        app.handle_key(key(KeyCode::Tab));
        for c in "shh".chars() {
            app.handle_key(key(KeyCode::Char(c)));
        }
        let commands = app.handle_key(key(KeyCode::Enter));

        assert_eq!(app.screen, Screen::Login);
        assert_eq!(app.config.twitch.client_id, "abc");
        assert!(
            matches!(commands.as_slice(), [Command::ReloadConfig(_)],),
            "the worker must be told about the new credentials"
        );
        let saved = std::fs::read_to_string(scratch.path().join("config.toml")).unwrap();
        assert!(saved.contains("abc"), "the credentials reached the file");
    }

    /// The login screen only offers platforms whose credentials exist, and
    /// Enter asks the worker to run the browser flow for them.
    #[test]
    fn the_login_screen_authorises_the_configured_platforms() {
        let _scratch = crate::paths::test_support::ScratchConfigDir::new("app-login");
        let mut config = Config::default();
        config.twitch.client_id = "id".into();
        config.twitch.client_secret = "secret".into();
        let mut app = App::new(config);
        // The start-up splash would otherwise cover the screen these tests
        // are looking at, and swallow the keys they send.
        app.splash_skipped = true;
        assert_eq!(app.screen, Screen::Login);

        let commands = app.handle_key(key(KeyCode::Enter));

        // YouTube is ticked by default but has no credentials, so it is left
        // out rather than sent on a login that could only fail.
        assert!(matches!(
            commands.as_slice(),
            [Command::Login(platforms)] if platforms == &[Platform::Twitch]
        ));
        assert!(app.busy);
    }

    /// Finishing a login goes straight to the main view — which is what the
    /// login was for — rather than back to a picker.
    #[test]
    fn a_finished_login_connects_immediately() {
        let _scratch = crate::paths::test_support::ScratchConfigDir::new("app-login-done");
        let mut config = Config::default();
        config.twitch.client_id = "id".into();
        config.twitch.client_secret = "secret".into();
        let mut app = App::new(config);
        // The start-up splash would otherwise cover the screen these tests
        // are looking at, and swallow the keys they send.
        app.splash_skipped = true;

        let commands = app.handle_event(Event::LoggedIn {
            platform: Platform::Twitch,
            result: Ok("twitch".into()),
        });

        assert!(matches!(
            commands.as_slice(),
            [Command::Connect(platforms)] if platforms == &[Platform::Twitch]
        ));
        assert_eq!(app.selected, vec![Platform::Twitch]);
    }

    /// A login that fails leaves the user on the login screen with the reason
    /// in the log, not stuck on a spinner.
    #[test]
    fn a_failed_login_stays_put_and_explains() {
        let _scratch = crate::paths::test_support::ScratchConfigDir::new("app-login-fail");
        let mut config = Config::default();
        config.twitch.client_id = "id".into();
        config.twitch.client_secret = "secret".into();
        let mut app = App::new(config);
        // The start-up splash would otherwise cover the screen these tests
        // are looking at, and swallow the keys they send.
        app.splash_skipped = true;
        app.busy = true;

        let commands = app.handle_event(Event::LoggedIn {
            platform: Platform::Twitch,
            result: Err("the browser window was closed".into()),
        });

        assert!(commands.is_empty());
        assert!(!app.busy);
        assert_eq!(app.screen, Screen::Login);
        assert!(app
            .log
            .iter()
            .any(|line| line.message.contains("the browser window was closed")));
    }

    /// A stream key can only ever be copied, never shown: this window is
    /// routinely visible on the broadcast itself.
    #[test]
    fn the_stream_key_is_copied_rather_than_revealed() {
        let mut app = app();
        app.screen = Screen::Dashboard;
        app.selected = vec![Platform::Twitch, Platform::YouTube];

        let commands = app.handle_key(key(KeyCode::Char('y')));
        assert!(matches!(
            commands.as_slice(),
            [Command::CopyStreamKey(Platform::Twitch)]
        ));

        let commands = app.handle_key(key(KeyCode::Char('Y')));
        assert!(matches!(
            commands.as_slice(),
            [Command::CopyStreamKey(Platform::YouTube)]
        ));
    }

    /// Asking for the key of a platform that is not part of this session says
    /// so rather than sending the worker on a pointless errand.
    #[test]
    fn copying_a_key_for_an_unselected_platform_explains_itself() {
        let mut app = app();
        app.screen = Screen::Dashboard;
        app.selected = vec![Platform::Twitch];

        let commands = app.handle_key(key(KeyCode::Char('Y')));

        assert!(commands.is_empty());
        assert!(app
            .toasts
            .visible_text()
            .iter()
            .any(|text| text.contains("YouTube")));
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

        let mut app = App::new(config);
        // The start-up splash would otherwise cover the screen these tests
        // are looking at, and swallow the keys they send.
        app.splash_skipped = true;
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
        assert!(app
            .toasts
            .visible_text()
            .iter()
            .any(|text| text.contains("watch page")));
    }

    #[test]
    fn an_empty_saved_platform_list_falls_back_to_all_platforms() {
        let mut config = Config::default();
        config.preset.platforms = vec![];
        let mut app = App::new(config);
        // The start-up splash would otherwise cover the screen these tests
        // are looking at, and swallow the keys they send.
        app.splash_skipped = true;
        assert_eq!(app.selected, Platform::ALL.to_vec());
    }

    /// The splash covers the interface, so a key pressed while it is up must
    /// dismiss it and go no further: the user was looking at the splash, not
    /// at the screen behind it, and acting on that key would act on something
    /// they could not see.
    #[test]
    fn a_key_during_the_splash_dismisses_it_without_reaching_the_screen_behind() {
        let mut app = App::new(Config::default());
        app.screen = Screen::Platforms;
        assert!(app.splash_is_showing(), "the splash starts covering things");

        // `q` quits from the platform picker. It must not, here.
        app.handle_key(KeyEvent::from(KeyCode::Char('q')));
        assert!(!app.should_quit, "the splash must swallow the key");
        assert!(!app.splash_is_showing(), "and be dismissed by it");

        // The next key reaches the screen underneath as usual.
        app.handle_key(KeyEvent::from(KeyCode::Char('q')));
        assert!(app.should_quit);
    }

    /// Stop always has to mean stop, even over a decorative start-up screen.
    #[test]
    fn ctrl_c_quits_even_while_the_splash_is_up() {
        let mut app = App::new(Config::default());
        assert!(app.splash_is_showing());
        app.handle_key(ctrl('c'));
        assert!(app.should_quit);
    }

    /// Turning the splash off in the config has to actually turn it off, or
    /// the setting is a lie that costs the user a keypress at every start-up.
    #[test]
    fn the_splash_can_be_turned_off_in_the_config() {
        let mut config = Config::default();
        config.appearance.splash = false;
        let app = App::new(config);
        assert!(!app.splash_is_showing());
        assert!(
            !app.is_animating(),
            "nothing is moving, so no clock is needed"
        );
    }

    /// The animation clock must stay parked when animation is off, however
    /// much is on screen — that is the whole point of the setting.
    #[test]
    fn animation_off_parks_the_clock_even_during_the_splash() {
        let mut config = Config::default();
        config.appearance.animations = "off".into();
        let app = App::new(config);
        assert!(app.splash_is_showing());
        assert!(!app.is_animating());
    }

    /// A failure while you are reading chat has to reach you. The activity
    /// log is on the Stream Info tab, so on any other tab a logged error
    /// would otherwise be invisible until you went looking for it.
    #[test]
    fn a_logged_failure_is_also_raised_as_a_notification() {
        let mut app = app();
        app.push_log(LogLevel::Error, "the token could not be refreshed");
        assert!(app
            .toasts
            .visible_text()
            .iter()
            .any(|text| text.contains("token could not be refreshed")));
    }

    /// Routine progress stays in the log. A pop-up for every ordinary step
    /// would train you to ignore pop-ups, which costs you the one that
    /// mattered.
    #[test]
    fn ordinary_progress_stays_in_the_log_without_popping_up() {
        let mut app = app();
        app.push_log(LogLevel::Info, "connecting to Twitch");
        app.push_log(LogLevel::Success, "connected");
        assert!(app.toasts.visible_text().is_empty());
        assert_eq!(app.log.len(), 2);
    }

    #[test]
    fn notifications_can_be_turned_off_entirely() {
        let mut config = Config::default();
        config.appearance.toasts = false;
        let mut app = App::new(config);
        app.splash_skipped = true;
        app.push_log(LogLevel::Error, "something broke");
        assert!(app.toasts.visible_text().is_empty());
        assert_eq!(app.log.len(), 1, "the log still records it");
    }

    /// Notifications must not disappear the moment a key is pressed: they
    /// arrive precisely while you are working, and a message you cannot read
    /// without stopping typing is a message you cannot read.
    #[test]
    fn typing_does_not_clear_a_notification() {
        let mut app = app();
        app.push_log(LogLevel::Error, "something broke");
        app.handle_key(KeyEvent::from(KeyCode::Char('j')));
        assert!(!app.toasts.visible_text().is_empty());
    }

    /// The modal history takes every key while it is open, so a key that
    /// would otherwise quit or moderate has to do nothing but scroll.
    #[test]
    fn the_message_history_is_modal_while_it_is_open() {
        let mut app = app();
        app.push_log(LogLevel::Error, "one");
        app.push_log(LogLevel::Error, "two");
        app.handle_key(KeyEvent::new(KeyCode::Char('m'), KeyModifiers::ALT));
        assert!(app.toasts.history_open);
        assert!(
            app.toasts.visible_text().is_empty(),
            "the pop-ups are in the list now, so they come off the screen"
        );

        // `q` would quit from the platform picker underneath. Here it closes
        // the history and nothing more.
        app.handle_key(KeyEvent::from(KeyCode::Char('q')));
        assert!(!app.should_quit);
        assert!(!app.toasts.history_open);
    }

    #[test]
    fn the_message_history_scrolls_and_stops_at_both_ends() {
        let mut app = app();
        for index in 0..5 {
            app.push_log(LogLevel::Error, format!("message {index}"));
        }
        app.handle_key(KeyEvent::new(KeyCode::Char('m'), KeyModifiers::ALT));
        app.handle_key(KeyEvent::from(KeyCode::Char('k')));
        assert_eq!(app.toasts.history_scroll, 1);
        app.handle_key(KeyEvent::from(KeyCode::Char('G')));
        assert_eq!(app.toasts.history_scroll, 4, "G reaches the oldest");
        app.handle_key(KeyEvent::from(KeyCode::Char('g')));
        assert_eq!(app.toasts.history_scroll, 0, "g returns to the newest");
    }

    /// The palette's promise is that choosing an entry does exactly what its
    /// key does, so an entry naming a key that nothing handles would be a lie
    /// printed in a list of instructions.
    ///
    /// This replays every entry's keys and requires each to change something
    /// — the screen, a tab, a mode, a setting, a queued command, or a
    /// notification explaining why not. Each is tried from all three tabs,
    /// because an action is allowed to be a no-op where it does not apply
    /// ("go to the Chat tab" while already on it) but not everywhere.
    #[test]
    fn every_command_palette_entry_does_something_when_its_keys_are_replayed() {
        /// Everything a key is allowed to have changed. Compared as a whole
        /// rather than field by field, so a new kind of state does not
        /// silently fall outside the check.
        fn snapshot(app: &App) -> String {
            format!(
                "{:?}|{:?}|{:?}|{}|{}|{}|{:?}|{}|{}|{:?}|{}|{}",
                app.screen,
                app.tab,
                app.combined_focus,
                app.should_quit,
                app.toasts.history_open,
                app.theme_picker.is_some(),
                app.animation,
                app.config.appearance.telemetry,
                app.chat.split_percent,
                app.chat.mode,
                app.log.len(),
                app.toasts.visible_text().len(),
            )
        }

        for entry in super::super::command_palette::ENTRIES {
            // Entries that act on an open chat have nothing to act on in a
            // fresh session, which is correct rather than broken. They are
            // covered by the chat tab's own tests.
            if entry.needs_chat {
                continue;
            }
            let did_something = [Tab::StreamInfo, Tab::Chat, Tab::Combined]
                .into_iter()
                .flat_map(|tab| {
                    [Screen::Platforms, Screen::Form, Screen::Dashboard]
                        .into_iter()
                        .map(move |screen| (tab, screen))
                })
                .any(|(tab, screen)| {
                    let mut app = app();
                    app.tab = tab;
                    app.screen = screen;
                    // Nudge the pane split off its default, so an action
                    // whose job is to put something back has something to
                    // put back.
                    app.chat.split_percent = 70;
                    let before = snapshot(&app);
                    let mut commands = Vec::new();
                    for key in entry.keys {
                        commands.extend(app.handle_key(key.event()));
                    }
                    snapshot(&app) != before || !commands.is_empty()
                });

            assert!(
                did_something,
                "the palette offers \"{}\" ({}), but replaying that key changed \
                 nothing on any tab or screen",
                entry.title, entry.shortcut
            );
        }
    }

    #[test]
    fn the_command_palette_opens_and_filters_as_you_type() {
        let mut app = app();
        app.handle_key(ctrl('p'));
        let palette = app.command_palette.as_ref().expect("the palette is open");
        assert_eq!(
            palette.matches().len(),
            super::super::command_palette::ENTRIES.len()
        );

        for c in "theme".chars() {
            app.handle_key(KeyEvent::from(KeyCode::Char(c)));
        }
        let palette = app.command_palette.as_ref().expect("still open");
        assert_eq!(palette.query, "theme");
        assert_eq!(palette.matches().len(), 1);
    }

    /// Choosing an entry has to run the action, not type its key into the
    /// query box it was chosen from.
    #[test]
    fn choosing_an_entry_closes_the_palette_and_runs_the_action() {
        let mut app = app();
        app.handle_key(ctrl('p'));
        for c in "combined".chars() {
            app.handle_key(KeyEvent::from(KeyCode::Char(c)));
        }
        app.handle_key(KeyEvent::from(KeyCode::Enter));
        assert!(app.command_palette.is_none(), "the palette must close");
        assert_eq!(app.tab, Tab::Combined);
    }

    #[test]
    fn escape_closes_the_command_palette_without_running_anything() {
        let mut app = app();
        app.handle_key(ctrl('p'));
        app.handle_key(KeyEvent::from(KeyCode::Esc));
        assert!(app.command_palette.is_none());
        assert_eq!(app.tab, Tab::StreamInfo);
    }

    /// Letters typed into the palette are query text, not shortcuts — `q`
    /// must search rather than quit.
    #[test]
    fn typing_in_the_palette_does_not_trigger_shortcuts() {
        let mut app = app();
        app.handle_key(ctrl('p'));
        app.handle_key(KeyEvent::from(KeyCode::Char('q')));
        assert!(!app.should_quit);
        assert_eq!(
            app.command_palette.as_ref().map(|p| p.query.as_str()),
            Some("q")
        );
    }

    fn area() -> ratatui::layout::Rect {
        ratatui::layout::Rect {
            x: 0,
            y: 0,
            width: 100,
            height: 30,
        }
    }

    fn mouse_click(column: u16, row: u16) -> crossterm::event::MouseEvent {
        crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    fn wheel(up: bool) -> crossterm::event::MouseEvent {
        crossterm::event::MouseEvent {
            kind: if up {
                crossterm::event::MouseEventKind::ScrollUp
            } else {
                crossterm::event::MouseEventKind::ScrollDown
            },
            column: 10,
            row: 10,
            modifiers: KeyModifiers::NONE,
        }
    }

    #[test]
    fn clicking_a_tab_switches_to_it() {
        let mut app = app();
        app.handle_mouse(mouse_click(17, 0), area());
        assert_eq!(app.tab, Tab::Chat);
        app.handle_mouse(mouse_click(28, 0), area());
        assert_eq!(app.tab, Tab::Combined);
        app.handle_mouse(mouse_click(2, 0), area());
        assert_eq!(app.tab, Tab::StreamInfo);
    }

    #[test]
    fn clicking_a_chat_pane_gives_it_the_keyboard() {
        let mut app = app();
        app.tab = Tab::Chat;
        app.handle_mouse(mouse_click(90, 10), area());
        assert_eq!(app.chat.focus, Platform::YouTube);
        app.handle_mouse(mouse_click(10, 10), area());
        assert_eq!(app.chat.focus, Platform::Twitch);
    }

    /// On the combined tab the keyboard may be on the stream-info half, so
    /// clicking a chat pane has to move it across as well as pick the pane —
    /// otherwise the pane looks focused and does not answer to the keyboard.
    #[test]
    fn clicking_a_chat_pane_on_the_combined_tab_moves_the_keyboard_to_the_chats() {
        let mut app = app();
        app.tab = Tab::Combined;
        app.combined_focus = CombinedFocus::StreamInfo;
        app.handle_mouse(mouse_click(10, 20), area());
        assert_eq!(app.combined_focus, CombinedFocus::Chat);
        assert_eq!(app.chat.focus, Platform::Twitch);

        app.handle_mouse(mouse_click(10, 6), area());
        assert_eq!(app.combined_focus, CombinedFocus::StreamInfo);
    }

    #[test]
    fn the_wheel_scrolls_the_activity_log_on_the_stream_info_tab() {
        let mut app = app();
        for index in 0..20 {
            app.push_log(LogLevel::Info, format!("line {index}"));
        }
        app.handle_mouse(wheel(true), area());
        assert!(app.log_scroll_back > 0, "the wheel must scroll back");
        app.handle_mouse(wheel(false), area());
        assert_eq!(app.log_scroll_back, 0, "and forward again");
    }

    #[test]
    fn the_wheel_scrolls_the_message_history_while_it_is_open() {
        let mut app = app();
        for index in 0..20 {
            app.push_log(LogLevel::Error, format!("line {index}"));
        }
        app.handle_key(KeyEvent::new(KeyCode::Char('m'), KeyModifiers::ALT));
        app.handle_mouse(wheel(true), area());
        assert!(app.toasts.history_scroll > 0);
    }

    /// A click while a modal overlay is up would land on whatever happened to
    /// be underneath it, which is not what the user is looking at.
    #[test]
    fn clicks_are_ignored_while_a_modal_overlay_is_open() {
        let mut app = app();
        app.toasts.open_history();
        app.handle_mouse(mouse_click(17, 0), area());
        assert_eq!(app.tab, Tab::StreamInfo, "the tab must not have changed");
    }

    /// Turning mouse reporting off has to actually turn it off, since the
    /// reason to turn it off is to get the terminal's own selection back.
    #[test]
    fn the_mouse_can_be_turned_off_entirely() {
        let mut config = Config::default();
        config.appearance.mouse = false;
        let mut app = App::new(config);
        app.splash_skipped = true;
        app.handle_mouse(mouse_click(17, 0), area());
        assert_eq!(app.tab, Tab::StreamInfo);
    }

    /// A key pressed out of habit while the palette is open is a shortcut,
    /// not a letter. Typing "m" into the search box because someone reached
    /// for alt+m would be baffling.
    #[test]
    fn modified_keys_are_not_typed_into_the_palette_query() {
        let mut app = app();
        app.handle_key(ctrl('p'));
        app.handle_key(KeyEvent::new(KeyCode::Char('m'), KeyModifiers::ALT));
        assert_eq!(
            app.command_palette.as_ref().map(|p| p.query.as_str()),
            Some("")
        );
    }
}
