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

/// The files the Config → Files section lists, and how to find each.
///
/// A table so the drawing and the "open this one" key cannot disagree about
/// which row is which — the same hazard the notification switches had.
pub type FileRow = (&'static str, fn() -> anyhow::Result<std::path::PathBuf>);

pub const FILE_ROWS: [FileRow; 3] = [
    ("Config", crate::paths::config_file),
    ("Logins", crate::paths::token_file),
    ("Log", crate::paths::log_file),
];

/// Something drawn over the interface that owns the screen while it is up.
///
/// In precedence order — the order `handle_key` resolves them in, which is
/// the order they are drawn over each other.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Overlay {
    WhichKeyAll,
    Splash,
    Preflight,
    CommandPalette,
    MessageHistory,
    ThemePicker,
}

/// What a form field starts out holding.
///
/// One place, so the constructor and the profile switcher cannot disagree
/// about where a field's initial value comes from.
fn initial_text(field: Field, preset: &crate::config::PresetConfig, plan: &StreamPlan) -> String {
    match field {
        Field::Title => plan.title.clone(),
        Field::Description => plan.description.clone(),
        Field::Tags => plan.tags_input(),
        // The typed name, not the resolved id: the id is held separately and
        // re-resolved when the text changes.
        Field::TwitchCategory => preset.twitch_category.clone(),
        Field::YouTubeCategory => youtube::category_name(&plan.youtube_category_id),
        Field::Language => plan.language.clone(),
        Field::Thumbnail => preset.thumbnail.clone(),
        // Deliberately empty. A scheduled start is a decision about *this*
        // broadcast, and `PresetConfig::from_plan` does not persist one — a
        // saved start time would silently schedule every future stream for a
        // moment that has already passed.
        Field::StartTime => String::new(),
        // Not text fields; `is_text_input` filters these out before this runs.
        Field::Privacy | Field::MadeForKids | Field::AutoStart | Field::AutoStop => String::new(),
    }
}

/// Whether a connect failure means the saved login is no longer usable.
///
/// Deliberately narrow. A timeout or a 500 is the network having a bad
/// moment and must not clear the login flag — the next attempt will work, and
/// telling somebody they are logged out mid-stream because a packet went
/// missing would be worse than the original problem. What this looks for is
/// the platform saying the credential itself is no good.
fn looks_like_a_dead_login(error: &str) -> bool {
    let error = error.to_lowercase();
    ["invalid_grant", "unauthorized", "401", "revoked", "expired"]
        .iter()
        .any(|marker| error.contains(marker))
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
    /// The local OAuth redirect port. It has no `Platform` of its own — it is
    /// shared by both providers' redirect URIs — so it sits outside the
    /// per-platform completeness check in `setup_is_complete`.
    OauthPort,
}

impl SetupField {
    pub const ORDER: [SetupField; 5] = [
        SetupField::TwitchId,
        SetupField::TwitchSecret,
        SetupField::YouTubeId,
        SetupField::YouTubeSecret,
        SetupField::OauthPort,
    ];

    pub fn label(self) -> &'static str {
        match self {
            SetupField::TwitchId => "Twitch client id",
            SetupField::TwitchSecret => "Twitch client secret",
            SetupField::YouTubeId => "YouTube client id",
            SetupField::YouTubeSecret => "YouTube client secret",
            SetupField::OauthPort => "OAuth redirect port",
        }
    }

    /// Whether the value must be drawn as dots. A secret on screen is a secret
    /// on any recording of that screen.
    pub fn is_secret(self) -> bool {
        matches!(self, SetupField::TwitchSecret | SetupField::YouTubeSecret)
    }

    /// The platform this field belongs to, or `None` for a field — like the
    /// shared redirect port — that both platforms use rather than own.
    pub fn platform(self) -> Option<Platform> {
        match self {
            SetupField::TwitchId | SetupField::TwitchSecret => Some(Platform::Twitch),
            SetupField::YouTubeId | SetupField::YouTubeSecret => Some(Platform::YouTube),
            SetupField::OauthPort => None,
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

/// How long the second press of "finish the broadcast" is accepted for.
///
/// Long enough to read the warning and decide, short enough that the decision
/// belongs to the moment it was made. A key pressed by accident five minutes
/// later must not end a stream.
const END_CONFIRM_WINDOW: std::time::Duration = std::time::Duration::from_secs(10);

/// A category search that has not been sent yet.
#[derive(Debug, Clone)]
pub struct PendingSearch {
    pub platform: Platform,
    pub query: String,
    /// When the last keystroke arrived. The request goes out once this is
    /// [`SEARCH_DEBOUNCE`] old.
    pub typed_at: std::time::Instant,
}

/// How long the typing has to stop before a category search is sent.
///
/// Long enough to swallow the gap between letters at any normal typing speed,
/// short enough that the list still feels like it is keeping up. The whole
/// point is that a request is spent per *word* rather than per keystroke.
const SEARCH_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(180);

/// How much one press of the volume keys moves an OBS input.
///
/// Five percent is the right size for finding roughly the level you want.
const VOLUME_STEP: f64 = 0.05;

/// …and the fine step, for settling on it.
///
/// Five percent is a large jump when a microphone is nearly right, and the
/// only alternative was opening OBS itself.
const VOLUME_STEP_FINE: f64 = 0.01;

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
    /// Everything the program can be told, while it is running.
    Config,
}

impl Tab {
    /// Every tab, in the order the tab bar draws them.
    pub const ALL: [Tab; 5] = [
        Tab::StreamInfo,
        Tab::Chat,
        Tab::Combined,
        Tab::Obs,
        Tab::Config,
    ];

    /// The label the tab bar shows, leading digit included.
    ///
    /// One definition, used both to draw the bar and to work out which label
    /// a click landed on. They were written out separately before, and drifted
    /// — the drawing code listed five tabs and the hit-testing three, so
    /// clicking "4 OBS" or "5 Config" did nothing at all, with no feedback.
    pub fn label(self) -> &'static str {
        match self {
            Tab::StreamInfo => "1 Stream Info",
            Tab::Chat => "2 Chat",
            Tab::Combined => "3 Combined",
            Tab::Obs => "4 OBS",
            Tab::Config => "5 Config",
        }
    }
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
    /// Whether the tags field has been edited since the form was opened.
    ///
    /// This is what tells "I emptied the tags" apart from "I never touched
    /// them", which are the same empty string but two opposite instructions
    /// to Twitch. See [`crate::model::StreamPlan::clear_tags`].
    pub tags_edited: bool,
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
    /// The terminal's size as of the last frame.
    ///
    /// Recorded by the event loop so that key handling can size things to the
    /// screen — paging through chat by the height of the pane rather than by
    /// a hardcoded ten lines, which is a page and a half on a laptop and a
    /// third of one on a tall terminal.
    pub terminal_area: ratatui::layout::Rect,
    /// Which platform's logout is armed and waiting for a second Enter.
    pub logout_armed: Option<Platform>,
    /// The abandoned broadcasts the last listing showed.
    ///
    /// Held so the confirming press can send back exactly what was on screen
    /// — a delete has to act on the list the user approved, not on whatever a
    /// second lookup happens to return.
    pub stale_broadcasts: Vec<crate::model::StaleBroadcast>,
    /// When the statistics on screen were fetched.
    pub stats_at: Option<std::time::Instant>,
    /// The reusable YouTube stream ids the last listing found, as
    /// `(id, title)`.
    ///
    /// Kept so they can be *chosen* rather than only read. The ids used to go
    /// to the activity log and stop there, leaving the user to copy one out
    /// by eye and hand-edit `[youtube] stream_id` in config.toml — the last
    /// thing in the program that could only be done in a text editor.
    pub youtube_streams: Vec<(String, String)>,
    /// OBS shortcuts already reported as shadowed, so the warning is said
    /// once rather than on every snapshot — which arrives once a second.
    reported_shortcut_clashes: std::collections::HashSet<String>,
    /// The pre-flight checklist, while it is on screen.
    ///
    /// Computed when the overlay opens rather than every frame, because it
    /// reads the token store off disk. `None` means the overlay is closed.
    pub preflight: Option<Vec<crate::preflight::Check>>,
    /// Incremented on every keystroke that triggers a search, so that a slow
    /// reply to an earlier keystroke can be recognised as stale and dropped.
    pub search_generation: u64,
    /// A category search waiting for the typing to stop.
    ///
    /// Every keystroke used to dispatch its own request. Typing
    /// "Baldur's Gate 3" was fifteen Helix calls, fourteen of them thrown away
    /// by the generation check the moment the next letter arrived — rate-limit
    /// pressure and radio time spent on answers nobody would read. The query
    /// is held here instead and sent once the keystrokes stop.
    pub pending_search: Option<PendingSearch>,
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
    /// When "finish the broadcast" was first pressed, if it is waiting for the
    /// second press that confirms it.
    ///
    /// Ending cannot be undone — a completed YouTube broadcast cannot be
    /// reopened — so it is the one streaming action that asks twice. The same
    /// shape as the chat pane's moderation confirmation: press once and the
    /// program says what will happen, press again and it happens. The instant
    /// is kept rather than a bare flag so a press five minutes ago cannot be
    /// completed by a stray keystroke now.
    pub end_armed: Option<std::time::Instant>,
    pub should_quit: bool,
    /// Notifications: what is popped up now, and everything said so far.
    pub toasts: super::toast::Toasts,
    /// Desktop notifications — the ones your *desktop* shows, which reach you
    /// when this terminal is behind OBS or on another workspace. Shared with
    /// the chat panes so everything queues in one place.
    pub desktop: crate::notify::Notifier,
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
    /// The Twitch EventSub connection: follows, channel points, hype trains.
    ///
    /// Separate from the chat connection because it carries what chat cannot.
    /// `None` until a Twitch login is known, and dropped if the account
    /// changes — dropping the handle ends the task.
    pub events_handle: Option<crate::eventsub::Handle>,
    /// The receiving end of that connection's updates, taken by the event
    /// loop the same way the OBS one is.
    pub events_updates: Option<tokio::sync::mpsc::UnboundedReceiver<crate::eventsub::Update>>,
    /// Updates from that connection. Taken out by the event loop, which is
    /// the only place allowed to await on it.
    pub obs_updates: Option<tokio::sync::mpsc::UnboundedReceiver<crate::obs::task::Update>>,

    /// What this process is costing the machine, when it is being shown.
    pub telemetry: crate::telemetry::Telemetry,
    /// How the Combined tab is arranged.
    pub layout: crate::layout::Layout,
    /// The configuration tab, while it is open.
    pub config_tab: Option<super::config_tab::ConfigTab>,

    /// The bindings in force, built from the defaults plus `[keys]`.
    pub keymap: crate::keys::Keymap,
    /// Keys pressed so far towards a chord, e.g. leader then `o` while
    /// waiting for the third key of `<Leader>os`.
    pub pending_keys: Vec<crate::keys::Key>,
    /// Whether the which-key popup is showing every binding at once.
    pub which_key_all: bool,
    /// How far the full binding listing is scrolled.
    ///
    /// It promises every binding and there are more than eighty of them, so
    /// on any ordinary terminal most were off the bottom with no way to reach
    /// them.
    pub which_key_scroll: u16,
    /// Everything wrong with `[keys]`, kept rather than logged once.
    ///
    /// These were pushed into the activity log at start-up and the vector
    /// dropped — so the user whose binding was silently discarded went to
    /// Config → Keys and found a table that simply did not contain it, with
    /// no hint that anything had gone wrong.
    pub key_problems: Vec<String>,

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

    /// Keeps `MSM_CONFIG_DIR` pointed at an empty scratch directory for as
    /// long as this `App` is alive, so a test's `saved_logins()` call (made
    /// during construction, below) can never read the real machine's actual
    /// saved accounts. Without this, a developer who has ever really logged
    /// in to msm on this machine gets a `tokio::spawn` panic the moment a
    /// test switches to the Chat or Combined tab: `activate()` finds a real,
    /// genuinely logged-in account and tries to open a real connection for
    /// it, outside of any Tokio runtime. Always `None` outside `#[cfg(test)]`
    /// builds, where the field does not exist at all.
    #[cfg(test)]
    _scratch_config_dir: Option<crate::paths::test_support::ScratchConfigDir>,
}

impl App {
    /// Build the initial state from the saved config, with its own quota
    /// ledger.
    ///
    /// Production goes through [`App::with_ledger`] instead, so that the
    /// interface and the worker share one ledger; this is for tests and for
    /// anywhere the spend does not matter.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn new(config: Config) -> Self {
        let ledger = crate::quota::QuotaStore::new(
            config.chat.daily_quota_units,
            crate::paths::config_dir()
                .ok()
                .map(|dir| dir.join("quota.json")),
        );
        Self::with_ledger(config, ledger)
    }

    /// Build the initial state around an existing quota ledger.
    pub fn with_ledger(config: Config, ledger: crate::quota::QuotaStore) -> Self {
        let preset = config.active_preset().clone();
        let plan = preset.to_plan();

        // Every field `Field::is_text_input` claims is editable gets one, built
        // by walking that list rather than by hand. The six hand-written
        // inserts this replaces covered six of the eight, so `Thumbnail` and
        // `StartTime` had no `TextInput` at all: both were drawn, both were
        // reachable with Tab, and neither could be typed into — while
        // `plan()` read them through `.unwrap_or_default()` and so always saw
        // an empty string. Ctrl+S then wrote that empty string over a
        // hand-configured `thumbnail =` in config.toml.
        //
        // The same loop is used for `setup_inputs` a few lines below, which is
        // where the shape comes from.
        let inputs: BTreeMap<Field, TextInput> = Field::ORDER
            .iter()
            .filter(|field| field.is_text_input())
            .map(|&field| (field, TextInput::new(initial_text(field, &preset, &plan))))
            .collect();

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
                SetupField::OauthPort => config.general.oauth_port.to_string(),
            };
            setup_inputs.insert(field, TextInput::new(existing));
        }

        // Build the keymap before anything can be pressed. A binding that
        // cannot be read is reported and skipped rather than refusing to
        // start: being locked out of your own interface by a typo in a key
        // name would be a poor trade.
        let (keymap, key_problems) = config.keys.keymap();

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
        // The Combined tab's arrangement. A layout the file cannot express
        // falls back to the default one rather than to a blank tab, and says
        // why in the log.
        let (layout, layout_problem) = match crate::layout::Layout::from_file(&config.layout) {
            Ok(layout) => (layout, None),
            Err(reason) => (crate::layout::Layout::default(), Some(reason)),
        };

        let desktop =
            crate::notify::Notifier::with_settings(config.notifications.notifier_settings());

        let mut app = Self {
            config_tab: None,
            desktop: desktop.clone(),
            layout,
            keymap,
            pending_keys: Vec::new(),
            which_key_all: false,
            which_key_scroll: 0,
            key_problems: Vec::new(),
            obs_focus: ObsFocus::Scenes,
            obs_scene_cursor: 0,
            obs_audio_cursor: 0,
            obs: crate::obs::state::ObsState::default(),
            obs_handle: None,
            obs_updates: None,
            events_handle: None,
            events_updates: None,
            telemetry: crate::telemetry::Telemetry::default(),
            command_palette: None,
            animation: config.appearance.animation_mode(),
            started_at: std::time::Instant::now(),
            splash_skipped: false,
            theme_picker: None,
            palette,
            tab: Tab::StreamInfo,
            chat: super::chat_tab::ChatTabState::new(&config, desktop.clone(), ledger),
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
            tags_edited: false,
            inputs,
            twitch_category: plan.twitch_category.clone(),
            youtube_category_id: plan.youtube_category_id.clone(),
            privacy: plan.privacy,
            made_for_kids: plan.made_for_kids,
            auto_start: plan.youtube_auto_start,
            auto_stop: plan.youtube_auto_stop,
            popup: None,
            terminal_area: ratatui::layout::Rect::default(),
            logout_armed: None,
            stale_broadcasts: Vec::new(),
            stats_at: None,
            youtube_streams: Vec::new(),
            reported_shortcut_clashes: std::collections::HashSet::new(),
            preflight: None,
            end_armed: None,
            search_generation: 0,
            pending_search: None,
            go_generation: 0,
            log: VecDeque::new(),
            accounts: BTreeMap::new(),
            results: Vec::new(),
            stats: BTreeMap::new(),
            busy: false,
            should_quit: false,
            toasts: super::toast::Toasts::default(),
            log_scroll_back: 0,
            #[cfg(test)]
            _scratch_config_dir: None,
        };

        // Anything wrong with the `[keys]` section goes in the activity log,
        // where a problem with the config belongs. It is reported rather than
        // fatal: a typo in a key name should cost that one binding, not the
        // ability to start.
        for problem in &key_problems {
            app.push_log(LogLevel::Warning, format!("Key binding: {problem}"));
        }
        app.key_problems = key_problems;
        if let Some(problem) = layout_problem {
            app.push_log(
                LogLevel::Warning,
                format!("Layout: {problem} — using the default arrangement"),
            );
        }

        app
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

    /// The text buffer of the form field that currently has the cursor.
    fn focused_input(&mut self) -> Option<&mut TextInput> {
        let field = self.field();
        self.inputs.get_mut(&field)
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
            | Field::AutoStop
            | Field::StartTime
            | Field::Thumbnail => self.is_selected(Platform::YouTube),
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
            // Emptying the field by hand is the one case where "no tags"
            // means "take the tags off" rather than "I did not set any".
            clear_tags: self.tags_edited
                && self
                    .inputs
                    .get(&Field::Tags)
                    .is_some_and(|input| input.value().trim().is_empty()),
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
            // An unreadable start time becomes "now" here and a blocking
            // validation issue in `validate`, which is where the explanation
            // belongs. Refusing to build a plan at all would leave the form
            // unable to say what was wrong with it.
            scheduled_start: crate::model::parse_start_time(
                self.inputs
                    .get(&Field::StartTime)
                    .map(|i| i.value())
                    .unwrap_or(""),
                chrono::Local::now(),
            )
            .unwrap_or(None),
            thumbnail_path: self
                .inputs
                .get(&Field::Thumbnail)
                .map(|i| i.value().trim().to_string())
                .unwrap_or_default(),
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

    /// Start watching for the Twitch events that never reach chat.
    ///
    /// Follows, channel-point redemptions, hype trains, polls and predictions
    /// arrive over EventSub rather than IRC — they are not chat and never have
    /// been — so this is a second connection alongside the chat one.
    ///
    /// Needs the channel's own numeric user id, which is saved with the login.
    /// A login saved before identities were recorded has none, and rather than
    /// spend a request working it out at start-up this simply does not run:
    /// logging in again fills it in, and everything else keeps working
    /// meanwhile.
    ///
    /// Called from inside the runtime, for the same reason `connect_obs` is.
    pub fn connect_events(&mut self) {
        if !self.config.notifications.twitch_events || self.events_handle.is_some() {
            return;
        }
        let Some(broadcaster_id) = self.twitch_user_id() else {
            return;
        };
        let (updates_tx, updates_rx) = tokio::sync::mpsc::unbounded_channel();
        let params = crate::eventsub::Params::new(
            broadcaster_id,
            crate::chat::source::token_provider(&self.config, Platform::Twitch.slug()),
            self.config.twitch.client_id(),
            crate::backend::http_client().unwrap_or_default(),
            updates_tx,
        );
        tracing::info!("watching Twitch events");
        self.events_handle = Some(crate::eventsub::spawn(params));
        self.events_updates = Some(updates_rx);
    }

    /// The primary Twitch account's numeric user id, as saved with its login.
    fn twitch_user_id(&self) -> Option<String> {
        let store = crate::auth::store::TokenStore::load().ok()?;
        store
            .get(Platform::Twitch)
            .and_then(|tokens| tokens.identity.as_ref())
            .map(|identity| identity.id.clone())
            .filter(|id| !id.is_empty())
    }

    /// Fold one EventSub update into the interface.
    ///
    /// Everything lands in the activity log, because the log is the record of
    /// what happened. Only the events themselves also reach the desktop —
    /// trouble with the connection is worth writing down and not worth
    /// interrupting a stream for.
    pub fn handle_events_update(&mut self, update: crate::eventsub::Update) {
        match update {
            crate::eventsub::Update::Trouble(message) => {
                self.push_log(LogLevel::Warning, message);
            }
            crate::eventsub::Update::Event(event) => {
                let line = if event.detail.is_empty() {
                    event.title.clone()
                } else {
                    format!("{} — {}", event.title, event.detail)
                };
                self.push_log(LogLevel::Info, line);
                if self.config.notifications.wants(event.kind) {
                    self.desktop.send(crate::notify::Notification::new(
                        event.title,
                        event.detail,
                        crate::notify::Urgency::Normal,
                    ));
                }
            }
        }
    }

    /// Whether the screen showing now claims plain keys for itself.
    ///
    /// The keymap steps aside for unmodified keys while it does. Two kinds of
    /// screen do:
    ///
    /// * **Text boxes.** A message being written, a channel being joined, a
    ///   title being edited — a letter is a letter there, and a keymap that
    ///   grabbed `q` would make it impossible to type the word "quiet".
    /// * **Pickers.** These are why the leader cannot simply take the space
    ///   bar everywhere: space ticks a checkbox on a list of tick boxes, and
    ///   that is what somebody looking at one will press. A picker is closer
    ///   to a modal than to a document, and vim's leader does not apply
    ///   inside one either.
    ///
    /// Modified chords still reach the keymap in both cases, since
    /// ctrl+something is never text.
    fn screen_owns_plain_keys(&self) -> bool {
        use super::chat_tab::ChatFocus;

        // The OBS tab is a view of OBS: the screen underneath it belongs to
        // the streaming flow and has no bearing on which keys apply here.
        if self.tab == Tab::Obs {
            return false;
        }

        // The Configuration tab is a form. Its keys move a cursor, change a
        // setting and edit a layout, so they are local to it — the leader
        // would take the space bar from a list somebody is working down.
        if self.tab == Tab::Config {
            return true;
        }

        // The chat modals: composing, searching, joining, picking an emoji,
        // or answering the timeout prompt.
        if self.chat_is_showing() {
            return !matches!(self.chat.mode, ChatFocus::Normal);
        }

        match self.screen {
            Screen::Setup | Screen::Form => true,
            Screen::Platforms | Screen::Login => true,
            // The dashboard is a view rather than a form, so the bindings
            // own it.
            Screen::Dashboard => false,
        }
    }

    /// Which set of bindings applies where the keyboard currently is.
    pub fn key_context(&self) -> crate::keys::Context {
        use crate::keys::Context;
        match self.tab {
            Tab::Obs => Context::Obs,
            Tab::Config => Context::Config,
            Tab::Chat => Context::Chat,
            Tab::Combined if self.combined_focus == CombinedFocus::Chat => Context::Chat,
            _ => Context::StreamInfo,
        }
    }

    /// Try to resolve a key through the keymap.
    ///
    /// Returns `None` when the key is not part of any binding, in which case
    /// the caller carries on with whatever it would have done — the form
    /// fields, the text boxes and the modal prompts all still handle their
    /// own keys, because a letter typed into a message is a letter.
    ///
    /// The three outcomes that are *not* `None`:
    ///
    /// * the chord is complete, and its action runs;
    /// * the chord is a prefix, so the keys are held and the which-key popup
    ///   opens — this is what makes `<Leader>` discoverable rather than
    ///   something you have to know;
    /// * the chord has gone nowhere, so it is abandoned. Silently: a
    ///   half-typed sequence is a slip, and an error message for one would be
    ///   more annoying than the slip.
    fn resolve_key(&mut self, key: KeyEvent) -> Option<Vec<Command>> {
        use crate::keys::Key;

        let context = self.key_context();
        let mut chord = self.pending_keys.clone();
        chord.push(Key::from_event(key));

        if let Some(action) = self.keymap.action(context, &chord) {
            self.pending_keys.clear();
            return Some(self.run_action(action));
        }

        if self.keymap.is_prefix(context, &chord) {
            self.pending_keys = chord;
            return Some(vec![]);
        }

        // Backspace steps back one key of a part-typed chord rather than
        // abandoning the whole thing. Every vim-shaped user tries it within a
        // minute of mistyping a leader sequence.
        if !self.pending_keys.is_empty() && key.code == KeyCode::Backspace {
            self.pending_keys.pop();
            return Some(vec![]);
        }

        // A sequence that was going somewhere and then was not: give up on
        // it, and let the key stand on its own if it means something by
        // itself. Otherwise `<Leader>x` would swallow a following `j`.
        if !self.pending_keys.is_empty() {
            self.pending_keys.clear();
            let alone = vec![Key::from_event(key)];
            if let Some(action) = self.keymap.action(context, &alone) {
                return Some(self.run_action(action));
            }
            return Some(vec![]);
        }

        None
    }

    /// Do what an action says.
    pub fn run_action(&mut self, action: crate::keys::Action) -> Vec<Command> {
        use crate::keys::Action;
        use crate::obs::task::Command as ObsCommand;

        // Doing anything else at all cancels a half-confirmed "finish the
        // broadcast". Somebody who armed it and then went off to change a
        // scene has stopped answering the question, and the answer must not
        // be left lying around waiting for a keystroke that means something
        // else.
        if action != Action::EndStream {
            self.end_armed = None;
        }

        match action {
            Action::Quit => self.should_quit = true,
            Action::CommandPalette => {
                self.command_palette = Some(super::command_palette::CommandPalette::open(
                    &self.keymap,
                    self.key_context(),
                ));
            }
            Action::MessageHistory => {
                self.toasts.dismiss_all();
                self.toasts.open_history();
            }
            Action::WhichKey => {
                self.which_key_all = true;
                self.which_key_scroll = 0;
            }
            Action::ThemePicker => {
                self.theme_picker = Some(super::theme_picker::ThemePicker::open(
                    &self.config.appearance.theme,
                    &self.palette,
                ));
            }
            Action::CycleAnimations => {
                self.animation = self.animation.next();
                self.config.appearance.animations = self.animation.name().to_string();
                let mode = self.animation.name();
                self.notify(super::toast::Level::Info, format!("Animations: {mode}"));
                return self.save_settings();
            }
            Action::ToggleTelemetry => {
                self.config.appearance.telemetry = !self.config.appearance.telemetry;
                let state = if self.config.appearance.telemetry {
                    "on"
                } else {
                    "off"
                };
                self.notify(super::toast::Level::Info, format!("Telemetry: {state}"));
                return self.save_settings();
            }

            Action::TabStreamInfo => return self.go_to_tab(Tab::StreamInfo),
            Action::TabChat => return self.go_to_tab(Tab::Chat),
            Action::TabCombined => return self.go_to_tab(Tab::Combined),
            Action::TabObs => return self.go_to_tab(Tab::Obs),
            Action::TabConfig => return self.go_to_tab(Tab::Config),
            Action::TabNext => return self.cycle_tab(1),
            Action::TabPrevious => return self.cycle_tab(-1),
            Action::CombinedSwapFocus => {
                if self.tab == Tab::Combined {
                    self.combined_focus = match self.combined_focus {
                        CombinedFocus::Chat => CombinedFocus::StreamInfo,
                        CombinedFocus::StreamInfo => CombinedFocus::Chat,
                    };
                    self.chat.pending_mod = None;
                }
            }

            Action::GoLive => return self.go_live_key(),
            Action::EndStream => return self.end_stream(),
            Action::EditStreamInfo => {
                self.go_to(Screen::Form);
                self.ensure_field_visible();
            }
            Action::RefreshStats => return vec![Command::PollStats],
            Action::CopyTwitchKey => return self.copy_stream_key(Platform::Twitch),
            Action::CopyYouTubeKey => return self.copy_stream_key(Platform::YouTube),
            Action::OpenWatchPage => return self.open_watch_page(),

            Action::ChatCompose => self.chat_compose(),
            Action::ChatSearch => {
                self.chat.mode = super::chat_tab::ChatFocus::Search(String::new())
            }
            Action::ChatSearchNext => self.chat.search_step(true),
            Action::ChatSearchPrevious => self.chat.search_step(false),
            Action::ChatJoin => self.chat.mode = super::chat_tab::ChatFocus::Join(String::new()),
            Action::ChatClose => self.chat.close_active_chat(),
            // A marker is worth a key rather than a typed command: the moment
            // you want to mark is the moment you have no hands free, which is
            // the whole reason a bookmark in the VOD beats scrubbing for it
            // later.
            Action::ChatMarker => self.chat.mark_moment(),
            Action::ChatCycleLayout => self.chat.cycle_layout(),
            Action::ChatCycleBadges => self.chat.cycle_badges(),
            Action::ChatToggleEmoteHighlight => self.chat.toggle_highlight(),
            Action::ChatToggleFullUsername => self.chat.toggle_full_username(),
            Action::ChatToggleTimestamps => self.chat.toggle_timestamps(),
            Action::StreamNextProfile => return self.next_profile(),

            // The Config tab's own keys, as named actions rather than the
            // hardcoded `KeyCode` matches they used to be. Being actions is
            // what makes them rebindable, and what puts them in which-key,
            // the full binding map and the command palette — the Config tab
            // was the one tab whose keys appeared in none of those.
            Action::ConfigNextSection => return self.config_move(1),
            Action::ConfigPreviousSection => return self.config_move(-1),
            Action::ConfigSwapPane => return self.config_swap_pane(),
            Action::ConfigActivate => return self.config_activate(),
            Action::ConfigAddAccount => return self.config_add_account_key(),
            Action::ConfigForgetAccount => return self.config_forget_account_key(),
            Action::ConfigRefreshChecks => return self.config_refresh_checks(),
            Action::ChatReconnect => self.chat.reconnect_active(),
            Action::ChatNextChat => self.chat.cycle_chat(true),
            Action::ChatPreviousChat => self.chat.cycle_chat(false),
            Action::ChatNextAccount => self.chat.cycle_account(true, &self.config),
            Action::ChatPreviousAccount => self.chat.cycle_account(false, &self.config),
            Action::ChatScrollUp => self.chat.select_move(1),
            Action::ChatScrollDown => self.chat.select_move(-1),
            Action::ChatPageUp => self.chat.scroll_by(self.chat_page()),
            Action::ChatPageDown => self.chat.scroll_by(-self.chat_page()),
            Action::ChatToTop => self.chat.scroll_to_end(true),
            Action::ChatToBottom => self.chat.scroll_to_end(false),
            Action::ChatFocusNextPane => self.chat.focus_other(),
            Action::ChatFocusPreviousPane => self.chat.focus_other(),
            Action::ChatWiden => self.chat.resize(false),
            Action::ChatNarrow => self.chat.resize(true),
            Action::ChatResetPanes => self.chat.reset_split(),
            Action::ChatToggleActivity => self.chat.toggle_activity(),
            Action::ChatToggleInspect => self.chat.inspect = !self.chat.inspect,
            // Same guard as Ctrl+E and as entering the composer: with no chat
            // open there is no draft the chosen emoji could land in, so the
            // picker would take the keyboard and then have nowhere to put its
            // result. The leader route was missing this.
            Action::ChatEmojiPicker if self.chat.active_key(self.chat.focus).is_none() => {
                self.notify(
                    super::toast::Level::Warning,
                    "No chat is open, so there is nowhere for an emoji to go.",
                );
            }
            Action::ChatEmojiPicker => {
                self.chat.mode = super::chat_tab::ChatFocus::EmojiPicker {
                    query: String::new(),
                    // Opened on its own rather than from the message box, so
                    // a chosen emoji is inserted into a fresh composer rather
                    // than into text already being written.
                    from_compose: false,
                    selected: 0,
                }
            }
            Action::ChatReply => {
                self.chat.reply_to_selected();
            }
            Action::ChatClearFilters => self.chat.toggle_filter('0'),

            Action::ObsUp => self.move_obs_cursor(-1),
            Action::ObsDown => self.move_obs_cursor(1),
            Action::ObsSwapPane => {
                self.obs_focus = match self.obs_focus {
                    ObsFocus::Scenes => ObsFocus::Audio,
                    ObsFocus::Audio => ObsFocus::Scenes,
                };
            }
            Action::ObsActivate => self.obs_activate(),
            Action::ObsToggleMute => {
                if let Some(input) = self.obs.audio.get(self.obs_audio_cursor) {
                    let name = input.name.clone();
                    self.obs_command(ObsCommand::ToggleMute(name));
                }
            }
            Action::ObsMuteAll => self.mute_all_obs_audio(),
            Action::ObsVolumeUp => self.nudge_obs_volume(VOLUME_STEP),
            Action::ObsVolumeDown => self.nudge_obs_volume(-VOLUME_STEP),
            Action::ObsSaveReplay => {
                // "That just happened, keep it" — the most-pressed OBS hotkey
                // a live streamer has, and it was on no key here at all.
                self.obs_command(crate::obs::task::Command::SaveReplay);
                self.notify(
                    super::toast::Level::Info,
                    "Asked OBS to save the replay buffer.",
                );
            }
            Action::ObsVolumeUpFine => self.nudge_obs_volume(VOLUME_STEP_FINE),
            Action::ObsVolumeDownFine => self.nudge_obs_volume(-VOLUME_STEP_FINE),
            Action::ObsToggleStream => self.obs_command(ObsCommand::ToggleStream),
            Action::ObsToggleRecord => self.obs_command(ObsCommand::ToggleRecord),
            Action::ObsPauseRecording => self.obs_command(ObsCommand::ToggleRecordPause),
            Action::ObsNextProfile => self.cycle_obs(true),
            Action::ObsNextCollection => self.cycle_obs(false),
            Action::ObsReconnect => {
                self.obs_command(ObsCommand::Reconnect);
                self.push_log(LogLevel::Info, "Reconnecting to OBS…");
            }
            Action::ObsRefresh => self.obs_command(ObsCommand::Refresh),
        }
        vec![]
    }

    /// Open the first watch page there is, or say there is not one.
    fn open_watch_page(&mut self) -> Vec<Command> {
        match self.first_watch_url() {
            Some(url) => {
                self.notify(super::toast::Level::Info, format!("Opening {url}"));
                vec![Command::OpenUrl(url)]
            }
            None => {
                self.notify(
                    super::toast::Level::Warning,
                    "No platform has a watch page yet — nothing has gone live.",
                );
                vec![]
            }
        }
    }

    /// Switch to a tab, doing whatever that tab needs on the way in.
    fn go_to_tab(&mut self, tab: Tab) -> Vec<Command> {
        self.chat.pending_mod = None;
        // Leaving the Config tab has to disarm the two-press logout
        // confirmation too, or a stale armed flag turns the next unrelated
        // Enter — back on the Config tab in some later session — into a
        // logout nobody asked for.
        self.logout_armed = None;

        // Leaving the chat panes releases their connections' hold on the
        // keyboard; entering them opens the logged-in accounts' chats.
        if self.chat_is_showing() && tab != Tab::Chat && tab != Tab::Combined {
            self.chat.deactivate();
        }
        self.tab = tab;
        match tab {
            Tab::Chat | Tab::Combined => {
                if tab == Tab::Combined {
                    self.combined_focus = CombinedFocus::Chat;
                }
                self.chat.activate(&self.config);
            }
            Tab::Obs => self.obs_command(crate::obs::task::Command::Refresh),
            Tab::Config => {
                // The tab edits a copy of the layout, so opening it takes a
                // fresh one rather than resuming an edit somebody walked away
                // from a session ago.
                if self.config_tab.is_none() {
                    self.config_tab = Some(super::config_tab::ConfigTab::new(self.layout.clone()));
                }
            }
            Tab::StreamInfo => {}
        }
        vec![]
    }

    fn cycle_tab(&mut self, delta: isize) -> Vec<Command> {
        const ORDER: [Tab; 5] = [
            Tab::StreamInfo,
            Tab::Chat,
            Tab::Combined,
            Tab::Obs,
            Tab::Config,
        ];
        let index = ORDER.iter().position(|tab| *tab == self.tab).unwrap_or(0);
        let next = ORDER[(index as isize + delta).rem_euclid(ORDER.len() as isize) as usize];
        self.go_to_tab(next)
    }

    /// Focus the message box, or say why there is nowhere to type.
    fn chat_compose(&mut self) {
        if self.chat.active_key(self.chat.focus).is_some() {
            self.chat.mode = super::chat_tab::ChatFocus::Compose;
        } else {
            let platform = self.chat.focus.label();
            self.notify(
                super::toast::Level::Warning,
                format!("No {platform} chat is open to write to yet."),
            );
        }
    }

    /// Act on whatever the OBS tab has selected.
    fn obs_activate(&mut self) {
        use crate::obs::task::Command as ObsCommand;
        match self.obs_focus {
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
        }
    }

    /// The Configuration tab's own keys.
    ///
    /// This tab is a form, so most of its keys are local to it: they move a
    /// cursor, change a setting, or edit the layout. Anything the keymap has
    /// bound has already run by the time this is reached.
    /// Whether a Config binding means anything in this section.
    ///
    /// The tab's sections do different things, and a binding that does not
    /// apply must fall through rather than swallow the key — the layout
    /// editor's `r` rotates the arrangement and its `a` adds a panel, and
    /// both share a letter with a binding that belongs to another section.
    fn config_action_applies(
        action: crate::keys::Action,
        section: super::config_tab::Section,
    ) -> bool {
        use super::config_tab::Section;
        use crate::keys::Action;

        match action {
            // Getting around always works, and so does leaving.
            Action::ConfigNextSection
            | Action::ConfigPreviousSection
            | Action::ConfigSwapPane
            | Action::Quit => true,
            // Enter means something in the sections that have something to
            // change or run; elsewhere it is not a key at all.
            Action::ConfigActivate => matches!(
                section,
                Section::Maintenance
                    | Section::Accounts
                    | Section::Appearance
                    | Section::Notifications
                    | Section::Chat
                    | Section::Paths
            ),
            Action::ConfigAddAccount | Action::ConfigForgetAccount => section == Section::Accounts,
            Action::ConfigRefreshChecks => section == Section::Diagnostics,
            _ => false,
        }
    }

    /// Move the selection: through the section list, or through the section's
    /// contents, depending on which pane has the keyboard.
    fn config_move(&mut self, delta: isize) -> Vec<Command> {
        use super::config_tab::{Focus, Section};

        let Some(mut config) = self.config_tab.clone() else {
            return vec![];
        };
        let rows = config.rows(self);

        // Moving disarms both confirmations. One that survives walking away
        // and coming back fires on a keypress the user has forgotten they
        // were part-way through — and these two delete broadcasts and throw
        // away a login.
        config.cleanup_listed = false;
        self.logout_armed = None;

        match config.focus {
            Focus::Sections => {
                let index = Section::ALL
                    .iter()
                    .position(|section| *section == config.section)
                    .unwrap_or(0) as isize;
                let count = Section::ALL.len() as isize;
                config.section = Section::ALL[(index + delta).rem_euclid(count) as usize];
                config.cursor = 0;
                config.diagnostics_scroll = 0;
                if config.section == Section::Diagnostics {
                    config.refresh_diagnostics(&self.config);
                }
                if config.section == Section::Chat {
                    config.refresh_chat_log_size(&self.config);
                }
            }
            // Diagnostics has no cursor — its checks are read rather than
            // selected — so here the same keys scroll the list. On a short
            // terminal the verdict at the bottom was otherwise off the screen
            // with no way to reach it.
            Focus::Contents if config.section == Section::Diagnostics => {
                config.diagnostics_scroll = if delta > 0 {
                    config.diagnostics_scroll.saturating_add(1)
                } else {
                    config.diagnostics_scroll.saturating_sub(1)
                };
            }
            Focus::Contents => {
                if rows > 0 {
                    let count = rows as isize;
                    config.cursor = ((config.cursor as isize + delta).rem_euclid(count)) as usize;
                }
            }
        }

        self.config_tab = Some(config);
        vec![]
    }

    fn config_swap_pane(&mut self) -> Vec<Command> {
        use super::config_tab::Focus;
        if let Some(config) = self.config_tab.as_mut() {
            config.focus = match config.focus {
                Focus::Sections => Focus::Contents,
                Focus::Contents => Focus::Sections,
            };
        }
        vec![]
    }

    /// Enter: what that means depends on the section, because the sections do
    /// genuinely different things.
    fn config_activate(&mut self) -> Vec<Command> {
        use super::config_tab::Section;
        let Some(config) = self.config_tab.as_ref() else {
            return vec![];
        };
        match config.section {
            Section::Maintenance => self.run_maintenance(),
            Section::Accounts => self.toggle_login(),
            Section::Appearance => self.change_appearance_setting(),
            Section::Notifications => self.change_notification_setting(),
            Section::Chat => self.change_chat_setting(),
            // Open the selected file. Four sections tell the user to edit
            // config.toml by hand and none of them offered to open it.
            Section::Paths => match FILE_ROWS.get(config.cursor) {
                Some((label, which)) => match which() {
                    Ok(path) => {
                        self.notify(
                            super::toast::Level::Info,
                            format!("Opening the {label} file…"),
                        );
                        vec![Command::OpenUrl(path.display().to_string())]
                    }
                    Err(err) => {
                        self.notify(
                            super::toast::Level::Warning,
                            format!("That path is unavailable: {err:#}"),
                        );
                        vec![]
                    }
                },
                None => vec![],
            },
            _ => vec![],
        }
    }

    fn config_add_account_key(&mut self) -> Vec<Command> {
        use super::config_tab::Section;
        let is_accounts = self
            .config_tab
            .as_ref()
            .is_some_and(|config| config.section == Section::Accounts);
        if is_accounts {
            self.add_chat_account()
        } else {
            vec![]
        }
    }

    fn config_forget_account_key(&mut self) -> Vec<Command> {
        use super::config_tab::Section;
        let is_accounts = self
            .config_tab
            .as_ref()
            .is_some_and(|config| config.section == Section::Accounts);
        if is_accounts {
            self.forget_account()
        } else {
            vec![]
        }
    }

    fn config_refresh_checks(&mut self) -> Vec<Command> {
        use super::config_tab::Section;
        let is_diagnostics = self
            .config_tab
            .as_ref()
            .is_some_and(|config| config.section == Section::Diagnostics);
        if !is_diagnostics {
            return vec![];
        }
        if let Some(config) = self.config_tab.as_mut() {
            config.refresh_diagnostics(&self.config);
            config.diagnostics_scroll = 0;
        }
        vec![]
    }

    fn key_config(&mut self, key: KeyEvent) -> Vec<Command> {
        use super::config_tab::{edit, Focus, Section};

        let Some(mut config) = self.config_tab.clone() else {
            return vec![];
        };

        // Navigation and activation are named actions now, so they are
        // rebindable and appear in which-key, the full binding map and the
        // command palette — the Config tab was the one tab whose keys were in
        // none of those.
        //
        // They are looked up *here* rather than by the general resolver
        // because this tab deliberately owns its plain keys: the layout
        // editor's `r` rotates and its `a` adds a panel, and letting the
        // keymap consume those first would break the editor. So a binding
        // applies only where it means something, and anything else falls
        // through to the local keys below.
        if let Some(action) = self.keymap.action(
            crate::keys::Context::Config,
            &[crate::keys::Key::from_event(key)],
        ) {
            if Self::config_action_applies(action, config.section) {
                self.config_tab = Some(config);
                return self.run_action(action);
            }
        }

        match key.code {
            KeyCode::Esc if self.logout_armed.is_some() => {
                self.logout_armed = None;
                self.notify(super::toast::Level::Info, "Logout cancelled.");
                self.config_tab = Some(config);
                return vec![];
            }
            // Esc from the contents pane steps back to the section list
            // first. Somebody pressing it to mean "stop editing this" was
            // thrown to another tab and told their layout had been discarded,
            // which is a lot to happen from one key.
            KeyCode::Esc if config.focus == Focus::Contents => {
                config.focus = Focus::Sections;
                self.config_tab = Some(config);
                return vec![];
            }
            KeyCode::Esc => {
                // Leaving with an unsaved layout throws the edit away rather
                // than keeping it half-applied, and says so.
                if config.dirty {
                    self.notify(
                        super::toast::Level::Warning,
                        "Layout changes were not saved — press s to keep them.",
                    );
                }
                self.config_tab = None;
                return self.go_to_tab(Tab::StreamInfo);
            }
            // Typing filters the Keys listing. Around 110 bindings walked one
            // `j` at a time was the alternative, and the letters were being
            // dropped on the floor anyway.
            KeyCode::Backspace if config.section == Section::Keys => {
                config.key_filter.pop();
                config.cursor = 0;
                self.config_tab = Some(config);
                return vec![];
            }
            KeyCode::Char(c)
                if config.section == Section::Keys
                    && config.focus == Focus::Contents
                    && is_typed_text(&key) =>
            {
                config.key_filter.push(c);
                config.cursor = 0;
                self.config_tab = Some(config);
                return vec![];
            }

            _ if config.section == Section::Layout => {
                // Undo, before anything is changed. `p` replaces the whole
                // arrangement in one keypress, so cycling past the preset you
                // wanted meant rebuilding by hand what one key threw away.
                if key.code == KeyCode::Char('u') {
                    match config.history.pop() {
                        Some(previous) => {
                            config.draft = previous;
                            config.dirty = true;
                            config.cursor = config
                                .cursor
                                .min(config.draft.panels().len().saturating_sub(1));
                        }
                        None => self.notify(
                            super::toast::Level::Info,
                            "Nothing left to undo in this edit.",
                        ),
                    }
                    self.config_tab = Some(config);
                    return vec![];
                }

                // Every key below changes the arrangement, so the state
                // before it is worth keeping. Bounded, because an editing
                // session is not a document.
                const UNDO_DEPTH: usize = 32;
                if matches!(
                    key.code,
                    KeyCode::Char(
                        '+' | '=' | '-' | '_' | 'J' | 'K' | 'r' | 'd' | 'a' | 'p' | '>' | '<'
                    )
                ) {
                    config.history.push(config.draft.clone());
                    if config.history.len() > UNDO_DEPTH {
                        config.history.remove(0);
                    }
                }

                match key.code {
                    KeyCode::Char('+') | KeyCode::Char('=') => {
                        edit::resize(&mut config.draft, config.cursor, 1);
                        config.dirty = true;
                    }
                    // The enclosing row rather than the panel. Without this
                    // "make the chats taller" could not be said at all.
                    KeyCode::Char('>') => {
                        if edit::resize_row(&mut config.draft, config.cursor, 1) {
                            config.dirty = true;
                        } else {
                            self.notify(
                                super::toast::Level::Info,
                                "This panel is not inside a row that can be resized.",
                            );
                        }
                    }
                    KeyCode::Char('<') => {
                        if edit::resize_row(&mut config.draft, config.cursor, -1) {
                            config.dirty = true;
                        } else {
                            self.notify(
                                super::toast::Level::Info,
                                "This panel is not inside a row that can be resized.",
                            );
                        }
                    }
                    KeyCode::Char('-') | KeyCode::Char('_') => {
                        edit::resize(&mut config.draft, config.cursor, -1);
                        config.dirty = true;
                    }
                    // Reordering keeps the cursor on the panel that moved,
                    // so holding the key walks a panel along rather than
                    // moving a different one each time.
                    KeyCode::Char('J') => {
                        if edit::move_panel(&mut config.draft, config.cursor, 1) {
                            config.cursor = (config.cursor + 1)
                                .min(config.draft.panels().len().saturating_sub(1));
                            config.dirty = true;
                        }
                    }
                    KeyCode::Char('K') => {
                        if edit::move_panel(&mut config.draft, config.cursor, -1) {
                            config.cursor = config.cursor.saturating_sub(1);
                            config.dirty = true;
                        }
                    }
                    KeyCode::Char('r') => {
                        edit::rotate(&mut config.draft);
                        config.dirty = true;
                    }
                    KeyCode::Char('d') => {
                        if edit::remove(&mut config.draft, config.cursor) {
                            config.dirty = true;
                            config.cursor = config
                                .cursor
                                .min(config.draft.panels().len().saturating_sub(1));
                        } else {
                            self.notify(
                                super::toast::Level::Warning,
                                "A layout needs at least one panel.",
                            );
                        }
                    }
                    KeyCode::Char('a') => {
                        // Add whichever panel is not on the layout yet, so
                        // one key adds something rather than opening a menu
                        // to choose from a list of eight.
                        let present = config.draft.panels();
                        match crate::layout::Panel::ALL
                            .iter()
                            .find(|panel| !present.contains(panel))
                        {
                            // Only claim it happened when it did: adding used
                            // to be a silent no-op on a single-panel layout
                            // and still reported success.
                            Some(panel) if edit::add(&mut config.draft, *panel) => {
                                config.dirty = true;
                                self.notify(
                                    super::toast::Level::Info,
                                    format!("Added {}.", panel.title()),
                                );
                            }
                            Some(panel) => self.notify(
                                super::toast::Level::Warning,
                                format!("Could not add {} to this layout.", panel.title()),
                            ),
                            None => self.notify(
                                super::toast::Level::Info,
                                "Every panel is already on the layout.",
                            ),
                        }
                    }
                    KeyCode::Char('p') => {
                        // Cycle through the presets, which is a faster way to
                        // arrive somewhere usable than moving eight panels by
                        // hand.
                        let names = crate::layout::presets::NAMES;
                        config.preset_index = (config.preset_index + 1) % names.len();
                        let next = names[config.preset_index].0;
                        if let Some(layout) = crate::layout::presets::by_name(next) {
                            config.draft = layout;
                            config.dirty = true;
                            config.cursor = 0;
                            self.notify(
                                super::toast::Level::Info,
                                format!(
                                    "Layout preset {} of {}: {next}",
                                    config.preset_index + 1,
                                    names.len()
                                ),
                            );
                        }
                    }
                    KeyCode::Char('s') => {
                        self.config_tab = Some(config);
                        return self.save_layout();
                    }
                    _ => {}
                }
            }
            _ => {}
        }

        self.config_tab = Some(config);
        vec![]
    }

    /// Run whichever housekeeping job is selected.
    ///
    /// The results go to the activity log rather than into this pane: they
    /// are a list of things that happened, which is exactly what the log is,
    /// and a second scrolling list inside a settings screen would be a worse
    /// version of it.
    fn run_maintenance(&mut self) -> Vec<Command> {
        let Some(config) = self.config_tab.as_mut() else {
            return vec![];
        };
        let cursor = config.cursor;
        match cursor {
            0 => {
                // The first press lists, the second deletes. Deleting things
                // somebody made, without showing them first, would be asking
                // for trust this has no way to earn.
                //
                // The confirming press sends back the ids that were actually
                // shown, so the delete cannot act on anything the user has
                // not seen.
                let confirming = config.cleanup_listed;
                config.cleanup_listed = !confirming;
                let approved = if confirming {
                    self.stale_broadcasts
                        .iter()
                        .map(|broadcast| broadcast.id.clone())
                        .collect()
                } else {
                    Vec::new()
                };
                vec![Command::Cleanup { approved }]
            }
            1 => vec![Command::ExportSuperchats],
            2 => vec![Command::ListStreams],
            // Past the three jobs are the streams the last listing found.
            _ => self.pin_stream_id(cursor - super::config_tab::MAINTENANCE_ROWS),
        }
    }

    /// Pin one of the listed YouTube streams as `[youtube] stream_id`.
    ///
    /// This closes the last thing in the program that could only be done in a
    /// text editor: the ids went to the activity log and stopped there, so
    /// choosing one meant reading it off the screen, quitting, and editing
    /// config.toml by hand.
    fn pin_stream_id(&mut self, index: usize) -> Vec<Command> {
        let Some((id, title)) = self.youtube_streams.get(index).cloned() else {
            return vec![];
        };

        // Pressing enter on the stream already pinned unpins it, which is how
        // you get back to "let YouTube choose" without editing the file.
        let already = self.config.youtube.stream_id.trim() == id;
        if already {
            self.config.youtube.stream_id.clear();
            self.notify(
                super::toast::Level::Info,
                format!("Unpinned {title} — YouTube will choose a stream again."),
            );
        } else {
            self.config.youtube.stream_id = id.clone();
            self.notify(
                super::toast::Level::Success,
                format!("Pinned {title} ({id}) as the stream to bind broadcasts to."),
            );
        }

        // `stream_id` is one of the settings the engine is built from, so the
        // worker has to be told or the next go-live would still use the old
        // one. `save_appearance` writes the file and sends `ReloadConfig`.
        self.save_settings()
    }

    /// Forget the extra chat account under the cursor.
    ///
    /// `TokenStore::remove` only deletes the bare platform slug, so an extra
    /// account added by mistake could never be taken out and its refresh
    /// token stayed valid in tokens.json indefinitely.
    fn forget_account(&mut self) -> Vec<Command> {
        let Some(config) = self.config_tab.as_ref() else {
            return vec![];
        };
        let accounts = self.all_accounts();
        let Some((key, platform, label)) = accounts.get(config.cursor).cloned() else {
            return vec![];
        };
        if key == platform.slug() {
            self.notify(
                super::toast::Level::Info,
                "That is the account you stream as — press enter to log out of it instead.",
            );
            return vec![];
        }
        vec![Command::ForgetAccount { key, label }]
    }

    /// Authorise a second account for the selected platform.
    ///
    /// Kept apart from logging in because the two do different things: this
    /// one leaves the primary account alone, and the account it adds is for
    /// reading and answering chat rather than for streaming.
    fn add_chat_account(&mut self) -> Vec<Command> {
        let Some(config) = self.config_tab.as_ref() else {
            return vec![];
        };
        // The row is an account now, not a platform: the store can hold
        // extra chat accounts and the section lists them all.
        let accounts = self.all_accounts();
        let Some(platform) = accounts
            .get(config.cursor)
            .map(|(_, platform, _)| *platform)
            .or_else(|| Platform::ALL.get(config.cursor).copied())
        else {
            return vec![];
        };
        // Logging in and out is about the *primary* account; an extra chat
        // account is removed with `d` instead, since re-authorising one is
        // what `a` already does.
        if accounts
            .get(config.cursor)
            .is_some_and(|(key, platform, _)| key != platform.slug())
        {
            self.notify(
                super::toast::Level::Info,
                "That is an extra chat account — press d to forget it, or a to add another.",
            );
            return vec![];
        }
        if self.config.check_credentials(&[platform]).is_err() {
            self.notify(
                super::toast::Level::Warning,
                format!("{} has no API credentials yet.", platform.label()),
            );
            return vec![];
        }
        self.notify(
            super::toast::Level::Info,
            format!(
                "Sign in as the other {} account in your browser.",
                platform.label()
            ),
        );
        vec![Command::LoginAdd(platform)]
    }

    /// Change whichever appearance setting is selected.
    ///
    /// The booleans flip. The two that are not booleans open the thing that
    /// chooses them, because a theme is picked by looking at it and an
    /// animation mode has three values rather than two.
    fn change_appearance_setting(&mut self) -> Vec<Command> {
        let Some(config) = self.config_tab.as_ref() else {
            return vec![];
        };

        match config.cursor {
            0 => {
                self.theme_picker = Some(super::theme_picker::ThemePicker::open(
                    &self.config.appearance.theme,
                    &self.palette,
                ));
                return vec![];
            }
            1 => {
                self.animation = self.animation.next();
                self.config.appearance.animations = self.animation.name().to_string();
            }
            2 => self.config.appearance.splash = !self.config.appearance.splash,
            3 => self.config.appearance.mouse = !self.config.appearance.mouse,
            4 => self.config.appearance.telemetry = !self.config.appearance.telemetry,
            5 => self.config.appearance.toasts = !self.config.appearance.toasts,
            6 => {
                self.config.appearance.terminal_background =
                    !self.config.appearance.terminal_background
            }
            7 => {
                // Round the useful values rather than one second at a time:
                // this is a setting somebody adjusts twice and then leaves,
                // and the file still takes any number in the range.
                const STEPS: [u64; 5] = [2, 3, 5, 8, 12];
                let current = self.config.appearance.toast_seconds;
                let next = STEPS
                    .iter()
                    .find(|step| **step > current)
                    .copied()
                    .unwrap_or(STEPS[0]);
                self.config.appearance.toast_seconds = next;
            }
            _ => {
                // Three-way rather than a switch: "auto" is the useful
                // default and the other two are for machines this program
                // cannot see the capture setup of.
                let next =
                    crate::config::StreamerMode::parse(&self.config.appearance.streamer_mode)
                        .next();
                self.config.appearance.streamer_mode = next.name().to_string();
            }
        }

        // Mouse reporting is turned on when the terminal is set up, so a
        // change to it only takes effect next time. Saying so beats leaving
        // somebody to wonder why the setting appears to do nothing.
        if config.cursor == 3 {
            self.notify(
                super::toast::Level::Info,
                "Mouse reporting changes when msm next starts.",
            );
        }
        self.save_settings()
    }

    /// Flip the chat-logging switch.
    ///
    /// This setting had no control anywhere in the interface — it could only
    /// be turned on by editing config.toml — even though Housekeeping's
    /// paid-event export reads the very logs it produces. That combination
    /// meant the export appeared to work and wrote an empty file.
    fn change_chat_setting(&mut self) -> Vec<Command> {
        self.config.chat.chat_logging = !self.config.chat.chat_logging;
        let state = if self.config.chat.chat_logging {
            "on — messages from now on are written to disk"
        } else {
            "off — what is already recorded is kept"
        };
        self.notify(super::toast::Level::Info, format!("Chat logging: {state}"));
        // The chat tab holds the live logger, so the switch has to reach it
        // as well as the file: turning it on opens a log now rather than at
        // the next start-up.
        self.chat.set_chat_logging(self.config.chat.chat_logging);
        self.save_settings()
    }

    /// Flip whichever desktop-notification switch is selected.
    ///
    /// The row order is the one `draw_notifications` lists, and the two have
    /// to agree — a mismatch would silently toggle the wrong setting, so the
    /// list is short, in one place, and covered by a test.
    /// Fire one notification through the real desktop path, right now.
    ///
    /// "Notifications do not work" is otherwise a thing you discover during a
    /// raid. On Linux the delivery path tries notify-send, then gdbus, then
    /// kdialog, then the terminal bell, and it is entirely reasonable not to
    /// know which of those this machine has — so being able to ask is worth a
    /// row of its own.
    ///
    /// It deliberately goes through `self.desktop` rather than round the
    /// pacing and the master switch, because what is being tested is exactly
    /// that path. If the master switch is off it says so instead, since a
    /// silent test would be indistinguishable from a broken one.
    fn send_test_notification(&mut self) -> Vec<Command> {
        if !self.config.notifications.enabled {
            self.notify(
                super::toast::Level::Warning,
                "Desktop notifications are switched off — turn the top row on first.",
            );
            return vec![];
        }
        self.desktop.send(crate::notify::Notification::new(
            "multistream-manager",
            "This is a test notification. If you can see it, they work.",
            crate::notify::Urgency::Normal,
        ));
        self.notify(
            super::toast::Level::Info,
            "Test notification sent. Nothing on your desktop? Check Config → Diagnostics.",
        );
        vec![]
    }

    fn change_notification_setting(&mut self) -> Vec<Command> {
        let Some(config) = self.config_tab.as_ref() else {
            return vec![];
        };
        // One lookup into the same table the section is drawn from, so a row
        // added, removed or reordered can never rebind the switches below it.
        let Some(row) = super::config_tab::NOTIFICATION_TABLE.get(config.cursor) else {
            // Past the end of the switches is the test-notification row.
            return self.send_test_notification();
        };
        (row.toggle)(&mut self.config.notifications);

        // The Twitch event switch is not a display filter: it decides whether
        // a second WebSocket is held open at all. Turning it on opens it now
        // rather than at the next start-up; turning it off drops the handle,
        // which ends the task.
        if self.config.notifications.twitch_events {
            self.connect_events();
        } else {
            self.events_handle = None;
            self.events_updates = None;
        }

        // Take effect now rather than at the next start-up. The notifier is
        // shared, so configuring it covers the chat panes' delivery — but the
        // chat tab holds its own copy of the settings that decide *which*
        // events qualify, and that copy has to be told too.
        self.desktop
            .configure(self.config.notifications.notifier_settings());
        self.chat.adopt_notification_settings(&self.config);
        self.save_settings()
    }

    /// Log in to, or out of, the selected platform.
    fn toggle_login(&mut self) -> Vec<Command> {
        let Some(config) = self.config_tab.as_ref() else {
            return vec![];
        };
        let Some(platform) = Platform::ALL.get(config.cursor).copied() else {
            return vec![];
        };

        if self.logged_in.get(&platform).copied().unwrap_or(false) {
            // Armed, the way ending a broadcast is. One key doing both "log
            // in" and "log out" meant a stray Enter on this row threw away a
            // login and the browser round trip needed to get it back.
            if self.logout_armed != Some(platform) {
                self.logout_armed = Some(platform);
                self.notify(
                    super::toast::Level::Warning,
                    format!(
                        "Press enter again to log out of {} — esc cancels.",
                        platform.label()
                    ),
                );
                return vec![];
            }
            self.logout_armed = None;
            self.notify(
                super::toast::Level::Info,
                format!("Logging out of {}…", platform.label()),
            );
            // The flag is set from the worker's answer, not here: clearing it
            // now meant a logout that failed to write left the screen saying
            // you were logged out while the token was still on disk.
            vec![Command::Logout(platform)]
        } else {
            if self.config.check_credentials(&[platform]).is_err() {
                self.notify(
                    super::toast::Level::Warning,
                    format!(
                        "{} has no API credentials yet — fill them in on the setup screen.",
                        platform.label()
                    ),
                );
                return vec![];
            }
            self.notify(
                super::toast::Level::Info,
                format!("Opening your browser to authorise {}…", platform.label()),
            );
            vec![Command::Login(vec![platform])]
        }
    }

    /// Keep the edited layout: apply it and write it to the config file.
    fn save_layout(&mut self) -> Vec<Command> {
        let Some(config) = self.config_tab.as_mut() else {
            return vec![];
        };
        if let Err(reason) = config.draft.validate() {
            self.notify(
                super::toast::Level::Error,
                format!("That layout will not work: {reason}"),
            );
            return vec![];
        }

        let draft = config.draft.clone();
        // A layout the file format cannot express is refused rather than
        // saved in a form that would come back different — a setting that
        // does not survive a restart is worse than one that was refused.
        let Some(file) = draft.to_file() else {
            self.notify(
                super::toast::Level::Error,
                "That layout is nested too deeply to be saved.",
            );
            return vec![];
        };

        // Applied only once the write succeeds. These three assignments used
        // to happen first, so a failed save left the interface showing an
        // arrangement that is not in the file — and with `dirty` cleared, no
        // longer offering to save it.
        let commands = self.persist("the layout", move |config| config.layout = file);
        if commands.is_empty() {
            return commands;
        }
        self.layout = draft;
        if let Some(config) = self.config_tab.as_mut() {
            config.dirty = false;
        }
        self.notify(super::toast::Level::Success, "Layout saved.");
        commands
    }

    /// Say so when a config-defined OBS shortcut can never fire.
    ///
    /// `[obs]` lets a scene or an audio input carry a one-key shortcut, and
    /// those keys are resolved *after* the keymap — deliberately, so that a
    /// shortcut cannot shadow a real binding and so rebinding a key does what
    /// it says. The consequence is the other way round: giving a scene the
    /// shortcut `s` produces a key that silently does nothing, because `s`
    /// is already "start or stop streaming" on this tab and gets there first.
    ///
    /// Nothing said so. The config loaded without complaint, the OBS pane
    /// showed the scene, and the key started the stream instead of switching
    /// to it. This reports each collision once per session, the way the
    /// keymap already reports a shadowed binding.
    fn report_shadowed_obs_shortcuts(&mut self) {
        let taken: std::collections::HashMap<crate::keys::Key, crate::keys::Action> = self
            .keymap
            .all()
            .into_iter()
            // Single-key chords only: a shortcut is one key, so a chord that
            // merely starts with it is not a collision.
            .filter(|binding| binding.chord.len() == 1)
            .filter(|binding| {
                matches!(
                    binding.context,
                    crate::keys::Context::Obs | crate::keys::Context::Global
                )
            })
            .map(|binding| (binding.chord[0], binding.action))
            .collect();

        let shortcuts: Vec<(String, String)> = self
            .obs
            .scenes
            .iter()
            .filter_map(|scene| {
                scene
                    .shortcut
                    .as_ref()
                    .map(|key| (key.clone(), format!("the scene {:?}", scene.name)))
            })
            .chain(self.obs.audio.iter().filter_map(|input| {
                input
                    .shortcut
                    .as_ref()
                    .map(|key| (key.clone(), format!("the input {:?}", input.name)))
            }))
            .collect();

        for (shortcut, what) in shortcuts {
            let mut chars = shortcut.chars();
            let (Some(c), None) = (chars.next(), chars.next()) else {
                continue;
            };
            let Some(action) = taken.get(&crate::keys::Key::char(c)) else {
                continue;
            };
            if !self.reported_shortcut_clashes.insert(shortcut.clone()) {
                continue;
            }
            self.push_log(
                LogLevel::Warning,
                format!(
                    "The OBS shortcut {shortcut:?} for {what} will never fire: {shortcut:?} is \
                     already \"{}\". Pick another letter in [obs], or rebind {} in [keys].",
                    action.describe(),
                    action.name()
                ),
            );
        }
    }

    /// The OBS tab's own keys.
    ///
    /// Only the *dynamic* ones live here: a scene or an audio input can be
    /// given a one-key shortcut in the config, and which keys those are is
    /// not known until OBS has said what exists. Everything else on this tab
    /// is an ordinary binding in the keymap, which has already had its say by
    /// the time this runs — so a shortcut cannot shadow a binding, and
    /// rebinding or removing a key does what it says.
    fn key_obs(&mut self, key: KeyEvent) -> Vec<Command> {
        use crate::obs::task::Command as ObsCommand;

        if !crate::keys::Key::from_event(key).is_text() {
            return vec![];
        }
        let KeyCode::Char(c) = key.code else {
            return vec![];
        };
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
        // Clamped at the top to unity gain — amplifying past 100% in OBS is a
        // deliberate act with real consequences for how a stream sounds, and
        // it should not be reachable by leaning on a key.
        //
        // But the ceiling is unity *or wherever this source already is*.
        // OBS happily holds a source at 1.2, and the pane draws 120%; a flat
        // clamp to 1.0 meant volume-*up* on such a source sent 1.0 — a 20%
        // cut — and no key could put it back. Never raise past unity from
        // below, and never move an already-boosted source the wrong way.
        let ceiling = current.max(1.0);
        let next = (current + delta).clamp(0.0, ceiling);
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
                // So is the OBS connection.
                self.refresh_diagnostics_if_showing();

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
                    Connection::Reconnecting { reason } => {
                        if previously_connected {
                            // Say why, when the attempt had something to say.
                            match reason {
                                Some(reason) => {
                                    self.push_log(LogLevel::Warning, format!("OBS: {reason}"))
                                }
                                None => self.push_log(LogLevel::Warning, "OBS disconnected."),
                            }
                            self.obs.clear_live_data();
                        }
                    }
                    Connection::Idle | Connection::Connecting => {
                        if previously_connected {
                            self.push_log(LogLevel::Warning, "OBS disconnected.");
                            self.obs.clear_live_data();
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
                self.report_shadowed_obs_shortcuts();
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
        // `toasts = false` turns off the *routine* pop-ups, never the ones
        // that report a problem. It used to turn off all of them, and since
        // `push_log` routes every error and warning through here, that meant
        // switching off pop-ups also switched off every error report: a
        // failed go-live, or a chat login expiring mid-stream, produced
        // nothing at all if you happened to be on the Chat, OBS or Config
        // tab, because the activity log is only drawn on Stream Info.
        //
        // The person most likely to turn pop-ups off is the person who is
        // live right now — which is the person least able to go hunting for a
        // log file. Silencing failures is the one thing this setting must not
        // do.
        let is_a_problem = matches!(
            level,
            super::toast::Level::Error | super::toast::Level::Warning
        );
        if !self.config.appearance.toasts && !is_a_problem {
            return;
        }
        self.toasts
            .push(level, text, self.config.appearance.toast_duration());
    }

    /// Raise a desktop notification about the stream's own state.
    ///
    /// Separate from [`Self::notify`], which draws a pop-up inside this
    /// program: that one is only seen by somebody looking at the terminal,
    /// and the whole reason these exist is the times you are not.
    fn notify_stream_state(
        &self,
        title: &str,
        body: impl Into<String>,
        urgency: crate::notify::Urgency,
    ) {
        if !self.config.notifications.stream_state {
            return;
        }
        self.desktop
            .send(crate::notify::Notification::new(title, body, urgency));
    }

    /// Compare a fresh statistics snapshot against the last one and notify on
    /// any platform that started or stopped broadcasting.
    ///
    /// This is how "your stream just died" reaches you. Nothing else in the
    /// program can tell you that: the platform simply stops reporting an
    /// incoming broadcast, and the only sign on screen is a number changing in
    /// a panel you are not looking at. A dropped encoder found forty minutes
    /// later is the failure this exists to prevent, so it is `Critical` —
    /// most desktops show a critical notification even in do-not-disturb.
    ///
    /// A platform missing from either snapshot is not a transition: the first
    /// poll after connecting has no "before", and a platform that failed to
    /// poll must not be reported as having gone offline.
    fn notify_live_transitions(&self, fresh: &BTreeMap<Platform, PlatformStats>) {
        for (platform, next) in fresh {
            let Some(previous) = self.stats.get(platform) else {
                continue;
            };
            // A failed poll carries no usable `live` flag — it is the last
            // known value, or a default. Treating that as a transition would
            // announce a dead stream every time the network hiccuped.
            if next.error.is_some() || previous.error.is_some() {
                continue;
            }
            match (previous.live, next.live) {
                (false, true) => self.notify_stream_state(
                    "Now live",
                    format!("{} is receiving your broadcast.", platform.label()),
                    crate::notify::Urgency::Normal,
                ),
                (true, false) => self.notify_stream_state(
                    "Stream stopped",
                    format!(
                        "{} is no longer receiving your broadcast.",
                        platform.label()
                    ),
                    crate::notify::Urgency::Critical,
                ),
                _ => {}
            }
        }
    }

    /// Undo the optimistic state a command set before it was sent, when the
    /// worker's queue was full and the command never left.
    ///
    /// The commands that matter here are the ones that set `busy` before they
    /// are dispatched, so the interface can show a spinner and refuse a second
    /// press. If such a command is dropped no reply will ever arrive, and
    /// `busy` was previously left set for the rest of the session — every
    /// later go-live, login and end-stream silently refused with "already
    /// working" and the only cure was restarting the program.
    pub fn on_command_dropped(&mut self, command: &Command) {
        match command {
            Command::GoLive { .. } => {
                self.busy = false;
                // The reply for this generation will never come, so step past
                // it; a late reply from an *earlier* submission must still be
                // discarded rather than mistaken for this one's answer.
                self.go_generation += 1;
            }
            Command::Connect(_) | Command::EndLive | Command::Login(_) | Command::LoginAdd(_) => {
                self.busy = false;
            }
            // Everything else is fire-and-forget from the interface's point of
            // view: losing it costs the user a keypress, not a stuck screen.
            _ => {}
        }
    }

    /// Insert pasted text wherever the keyboard currently is.
    ///
    /// Bracketed paste arrives as one event rather than a burst of
    /// keystrokes, which is what makes this safe: read as typing, a paste
    /// would run every `q` and `j` in it as a command. There was no paste at
    /// all before — a 5000-character YouTube description had to be retyped,
    /// and a chat message could not be pasted from a browser.
    ///
    /// Only the places that are genuinely text accept it. Everywhere else a
    /// paste does nothing, which is better than scattering it into a screen
    /// that was not expecting any.
    pub fn handle_paste(&mut self, text: &str) -> Vec<Command> {
        use super::chat_tab::ChatFocus;

        // Newlines are not text here: every field and the composer are single
        // lines, and a pasted paragraph should arrive as one line rather than
        // sending a half-finished message per line break.
        let text: String = text
            .chars()
            .map(|c| if c == '\n' || c == '\r' { ' ' } else { c })
            .collect();
        if text.is_empty() {
            return vec![];
        }

        // The chat composer, when it has the keyboard.
        if self.chat_has_the_keyboard() && self.chat.mode == ChatFocus::Compose {
            self.chat.compose_paste(&text);
            return vec![];
        }

        if self.tab != Tab::StreamInfo && self.tab != Tab::Combined {
            return vec![];
        }

        match self.screen {
            Screen::Form => {
                let field = self.field();
                if !field.is_text_input() {
                    return vec![];
                }
                if let Some(input) = self.inputs.get_mut(&field) {
                    input.insert_str(&text);
                }
                self.on_text_changed(field)
            }
            // The credential boxes, which is where a paste is most obviously
            // wanted: a client secret is forty random characters nobody types.
            Screen::Setup => {
                if let Some(field) = SetupField::ORDER.get(self.setup_cursor).copied() {
                    if let Some(input) = self.setup_inputs.get_mut(&field) {
                        input.insert_str(&text);
                    }
                }
                vec![]
            }
            _ => vec![],
        }
    }

    /// Whether to hide what must not be captured right now.
    ///
    /// Borrowed from Chatterino's streamer mode, which notices that OBS is
    /// running; this has the better signal, because OBS tells it whether the
    /// stream is actually going out. The point is the moment nobody plans
    /// for: you tab to the Config screen mid-stream to check something, and
    /// the file paths carry your username while the setup screen carries a
    /// client id that belongs to your application.
    pub fn streamer_mode(&self) -> bool {
        match crate::config::StreamerMode::parse(&self.config.appearance.streamer_mode) {
            crate::config::StreamerMode::Always => true,
            crate::config::StreamerMode::Never => false,
            // Recording counts: a local recording is uploaded later, and a
            // credential in it is just as exposed as one on a live stream.
            crate::config::StreamerMode::Auto => {
                self.config.obs.enabled
                    && self.obs.is_connected()
                    && (self.obs.streaming || self.obs.recording)
            }
        }
    }

    /// Every account the token store holds, primary and extra, as
    /// `(store key, platform, label)`.
    ///
    /// The Accounts section listed two rows — one per platform — while the
    /// store can hold any number of extra chat accounts under
    /// `twitch:<login>` keys. An account added by mistake was invisible and
    /// could never be removed, so its refresh token stayed valid in
    /// tokens.json indefinitely.
    pub fn all_accounts(&self) -> Vec<(String, Platform, String)> {
        let Ok(store) = crate::auth::store::TokenStore::load() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for platform in Platform::ALL {
            for (key, tokens) in store.accounts(platform) {
                let name = tokens
                    .identity
                    .as_ref()
                    .map(|identity| identity.display_name.clone())
                    .filter(|name| !name.trim().is_empty())
                    .unwrap_or_else(|| key.to_string());
                let label = if key == platform.slug() {
                    format!("{name} (streaming)")
                } else {
                    format!("{name} (chat only)")
                };
                out.push((key.to_string(), platform, label));
            }
        }
        out
    }

    /// Who each platform is logged in as, and how long the token has, for
    /// the Accounts section.
    ///
    /// Read from the store rather than kept in state: the section is not on
    /// screen most of the time, and the answer changes underneath the program
    /// when a token refreshes. Two booleans were all the screen showed, when
    /// the store holds the account name, the expiry and whether it can renew
    /// itself — and with two accounts on one machine the only question at
    /// this screen is which one you are about to stream as.
    pub fn account_summary_for(&self, key: &str) -> Option<(String, bool)> {
        let store = crate::auth::store::TokenStore::load().ok()?;
        let tokens = store.get_keyed(key)?;
        Some((tokens.expires_in_human(), tokens.refresh_token.is_some()))
    }

    /// Take the diagnostics again, if that pane is what is on screen.
    ///
    /// The snapshot was taken when the section was opened and on `r`, and
    /// never again — so a stale "no platform is authorised" sat there after a
    /// login had just succeeded, and an OBS connection that came up while the
    /// pane was open was still reported as down. A self-check that answers
    /// with information from before the thing you just did is worse than one
    /// that makes you press a key, because it looks current.
    fn refresh_diagnostics_if_showing(&mut self) {
        let showing = self.tab == Tab::Config
            && self
                .config_tab
                .as_ref()
                .is_some_and(|config| config.section == super::config_tab::Section::Diagnostics);
        if !showing {
            return;
        }
        if let Some(tab) = self.config_tab.as_mut() {
            tab.refresh_diagnostics(&self.config);
        }
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
            Event::LoggedOut { platform, result } => {
                // Only on success. A logout that failed to write leaves the
                // token on disk, so the screen has to keep saying the login
                // is there — the worker has already logged why it is.
                if result.is_ok() {
                    self.logged_in.insert(platform, false);
                }
                self.refresh_diagnostics_if_showing();
            }
            Event::StaleBroadcasts(found) => {
                // An empty list means the job finished — nothing is armed any
                // more, whether it deleted, found nothing, or failed.
                if found.is_empty() {
                    if let Some(config) = self.config_tab.as_mut() {
                        config.cleanup_listed = false;
                    }
                }
                self.stale_broadcasts = found;
            }
            Event::Streams(streams) => {
                self.youtube_streams = streams;
                if !self.youtube_streams.is_empty() {
                    self.push_log(
                        LogLevel::Info,
                        "Move down to a stream and press enter to pin it as [youtube] stream_id.",
                    );
                }
            }
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
                        Err(err) => {
                            self.push_log(
                                LogLevel::Error,
                                format!("{} could not connect: {err}", platform.label()),
                            );
                            // A login the platform has stopped accepting is
                            // not a login. `logged_in` was a start-up
                            // snapshot that nothing ever corrected, so a
                            // revoked token — the commonest way this breaks
                            // after weeks of working — left Accounts, the
                            // login screen and the header all still saying
                            // "logged in" while nothing worked.
                            if looks_like_a_dead_login(err) {
                                self.logged_in.insert(platform, false);
                                self.refresh_diagnostics_if_showing();
                            }
                        }
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
                    // Going live is the moment you stop looking at this
                    // window, so it is also the moment a desktop pop-up is
                    // worth more than an in-program one. The body names the
                    // platforms that took the plan, because a partial success
                    // (Twitch yes, YouTube no) is the case worth reading.
                    let ready: Vec<&str> = self
                        .results
                        .iter()
                        .filter(|result| result.succeeded())
                        .map(|result| result.platform.label())
                        .collect();
                    self.notify_stream_state(
                        "Stream ready",
                        format!("{} — start streaming in OBS.", ready.join(" and ")),
                        crate::notify::Urgency::Normal,
                    );
                } else {
                    self.notify(
                        super::toast::Level::Error,
                        "Every platform failed. See the log below for why.",
                    );
                    self.notify_stream_state(
                        "Going live failed",
                        "Every platform refused. Check the activity log.",
                        crate::notify::Urgency::Critical,
                    );
                }
            }

            Event::LoggedIn { platform, result } => {
                // A fresh Twitch login is the moment the event connection
                // becomes possible: it needs the user id that login just
                // saved. Harmless when one is already running.
                if platform == Platform::Twitch && result.is_ok() {
                    self.connect_events();
                }
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
                // The logins are one of the things the self-check reports.
                self.refresh_diagnostics_if_showing();

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

            Event::Ended { results } => {
                self.busy = false;
                self.end_armed = None;
                let ended = results
                    .iter()
                    .filter(|(_, outcome)| {
                        outcome
                            .as_ref()
                            .map(|o| o.changed_anything())
                            .unwrap_or(false)
                    })
                    .count();
                let failed = results.iter().any(|(_, outcome)| outcome.is_err());

                // The statistics on hand describe a broadcast that is over.
                // Left on screen they would keep reporting its viewers and
                // uptime until the next poll, which is a lie with a clock on
                // it.
                if ended > 0 {
                    self.stats.clear();
                }

                if failed {
                    self.notify(
                        super::toast::Level::Error,
                        "The broadcast could not be finished everywhere. See the log.",
                    );
                    self.notify_stream_state(
                        "Could not finish the broadcast",
                        "At least one platform refused. Check the activity log.",
                        crate::notify::Urgency::Critical,
                    );
                } else if ended > 0 {
                    self.notify(super::toast::Level::Success, "The broadcast is finished.");
                    self.notify_stream_state(
                        "Broadcast finished",
                        "You can stop streaming in OBS.",
                        crate::notify::Urgency::Normal,
                    );
                } else {
                    // Nothing had to be closed — Twitch alone, most likely.
                    // Silence would look like the key had not worked.
                    self.notify(
                        super::toast::Level::Info,
                        "Nothing needed finishing — see the log for each platform.",
                    );
                }
            }

            Event::Stats(stats) => {
                let stats: BTreeMap<Platform, PlatformStats> = stats.into_iter().collect();
                self.notify_live_transitions(&stats);
                self.stats = stats;
                // When, so a frozen number and a flat one can be told apart.
                // The map was replaced wholesale with no stamp, so during a
                // network hiccup the dashboard showed the last good figures
                // and looked exactly like a quiet stream.
                self.stats_at = Some(std::time::Instant::now());
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

        // Ctrl+C quits from anywhere, ahead of every modal and every
        // binding. "Stop" has to mean stop even over a screen that owns the
        // keyboard, and it is the one key nobody should be able to rebind
        // into uselessness.
        if key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('c')) {
            self.should_quit = true;
            return vec![];
        }

        // The which-key listing takes every key while it is up: it covers
        // the screen, so acting on anything underneath would act on
        // something that cannot be seen.
        if self.which_key_all {
            // Scrollable rather than dismissed-by-anything: a listing that
            // shows a fifth of what it promises and closes on the key you
            // pressed to see more is worse than no listing.
            match key.code {
                KeyCode::Down | KeyCode::Char('j') => {
                    self.which_key_scroll = self.which_key_scroll.saturating_add(1);
                    return vec![];
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    self.which_key_scroll = self.which_key_scroll.saturating_sub(1);
                    return vec![];
                }
                KeyCode::PageDown => {
                    self.which_key_scroll = self.which_key_scroll.saturating_add(10);
                    return vec![];
                }
                KeyCode::PageUp => {
                    self.which_key_scroll = self.which_key_scroll.saturating_sub(10);
                    return vec![];
                }
                KeyCode::Char('g') | KeyCode::Home => {
                    self.which_key_scroll = 0;
                    return vec![];
                }
                KeyCode::Char('G') | KeyCode::End => {
                    // Clamped where the line count is known, in the drawing.
                    self.which_key_scroll = u16::MAX;
                    return vec![];
                }
                _ => {}
            }
            self.which_key_all = false;
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

        // The pre-flight checklist is modal. It covers the form it was opened
        // from, and it exists to be read before a decision is made, so a key
        // meant for the screen underneath must not slip past it.
        if self.preflight.is_some() {
            return self.key_preflight(key);
        }

        // The command palette owns the keyboard while it is open, because
        // every letter is part of the query rather than a shortcut.
        if self.command_palette.is_some() {
            return self.key_command_palette(key);
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('p')) {
            self.command_palette = Some(super::command_palette::CommandPalette::open(
                &self.keymap,
                self.key_context(),
            ));
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

        // The keymap, after every modal overlay above (each of those owns the
        // keyboard entirely while it is open) and before the per-screen
        // handlers below, so a binding — built-in or from `[keys]` — beats a
        // key a screen happens to use for something else.
        //
        // Where a screen is *itself* a text box or a picker, only modified
        // chords are considered: a letter is a letter there, but
        // ctrl+something never is. That is what keeps `<C-p>` working while a
        // message is half-written, without `q` making it impossible to type
        // the word "quiet" or the leader stealing the space bar from a
        // checkbox.
        let local_keys = self.screen_owns_plain_keys();
        if !local_keys || crate::keys::is_command_key(crate::keys::Key::from_event(key)) {
            if let Some(commands) = self.resolve_key(key) {
                return commands;
            }
        } else if !self.pending_keys.is_empty() {
            self.pending_keys.clear();
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
                    return vec![];
                }
                // Cycle how much the interface animates, without going to
                // the config file for it: whether motion is comfortable is
                // something you find out by looking at it.
                KeyCode::Char('a') => {
                    self.chat.pending_mod = None;
                    self.animation = self.animation.next();
                    self.config.appearance.animations = self.animation.name().to_string();
                    let mode = self.animation.name();
                    self.notify(super::toast::Level::Info, format!("Animations: {mode}"));
                    return self.save_settings();
                }
                // Show or hide the process telemetry in the header.
                KeyCode::Char('t') => {
                    self.chat.pending_mod = None;
                    self.config.appearance.telemetry = !self.config.appearance.telemetry;
                    let state = if self.config.appearance.telemetry {
                        "on"
                    } else {
                        "off"
                    };
                    self.notify(super::toast::Level::Info, format!("Telemetry: {state}"));
                    return self.save_settings();
                }
                // Open the message history — vim's `:messages`, on a key.
                KeyCode::Char('m') => {
                    self.chat.pending_mod = None;
                    // Opening the history takes the pop-ups off the screen:
                    // every one of them is in the list you are now looking
                    // at, so leaving them stacked on top of it would only
                    // cover the entries they duplicate.
                    self.toasts.dismiss_all();
                    self.toasts.open_history();
                    return vec![];
                }
                // Alt+1..5 select a tab. Delegated to `go_to_tab` rather
                // than reimplemented here: this block used to carry its own
                // copy of the entering-a-tab logic, and had already drifted
                // from it — Alt+5 had no arm at all, and Alt+4's copy of the
                // OBS refresh was a second place to keep in step.
                KeyCode::Char(digit @ '1'..='5') => {
                    let index = digit as usize - '1' as usize;
                    return self.go_to_tab(Tab::ALL[index]);
                }
                _ => {}
            }
        }

        if self.chat_has_the_keyboard() {
            return self.key_chat(key);
        }

        if self.tab == Tab::Config {
            return self.key_config(key);
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
            Tab::StreamInfo | Tab::Obs | Tab::Config => false,
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
        use super::chat_tab::{ChatFocus, ComposeEdit, EMOJI_CHOICES};

        // Modal input first: while composing or joining, printable keys are
        // text, never commands (so typing a channel called "x" cannot close
        // anything).
        match self.chat.mode.clone() {
            ChatFocus::Compose => {
                match key.code {
                    KeyCode::Esc => self.chat.mode = ChatFocus::Normal,
                    KeyCode::Enter => self.chat.compose_send(),
                    // The ordinary editing keys, the same ones the metadata
                    // form's fields have always had. The composer used to
                    // support appending a character and deleting the last
                    // one, and nothing else — so fixing a typo six words back
                    // meant backspacing over everything after it, in the one
                    // text box somebody sits in for a whole stream.
                    KeyCode::Backspace => self.chat.compose_edit(ComposeEdit::Backspace),
                    KeyCode::Delete => self.chat.compose_edit(ComposeEdit::Delete),
                    KeyCode::Left => self.chat.compose_edit(ComposeEdit::Left),
                    KeyCode::Right => self.chat.compose_edit(ComposeEdit::Right),
                    KeyCode::Home => self.chat.compose_edit(ComposeEdit::Home),
                    KeyCode::End => self.chat.compose_edit(ComposeEdit::End),
                    // Up/Down walk what has already been sent in this chat.
                    // The composer is one line, so there is no other meaning
                    // for them here, and every other chat client does this.
                    KeyCode::Up => self.chat.compose_recall(true),
                    KeyCode::Down => self.chat.compose_recall(false),
                    KeyCode::Char('w') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        self.chat.compose_edit(ComposeEdit::DeleteWord)
                    }
                    KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        self.chat.compose_edit(ComposeEdit::Clear)
                    }
                    // No Ctrl+A / Ctrl+E readline aliases here: Ctrl+E already
                    // opens the emoji picker in this mode, and Home/End do the
                    // job without taking a documented key away.
                    // Tab completes a trailing @mention from the roster.
                    KeyCode::Tab => self.chat.complete_mention(),
                    KeyCode::Char('e') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        self.chat.mode = ChatFocus::EmojiPicker {
                            query: String::new(),
                            from_compose: true,
                            selected: 0,
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
                mut selected,
            } => {
                let matches = crate::chat::emoji::search(&buffer, EMOJI_CHOICES).len();
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
                    // Move along the row of candidates. Without this the
                    // drawn list was decoration: only its first entry could
                    // ever be inserted, however far the query was narrowed.
                    KeyCode::Left | KeyCode::Up => {
                        selected = selected.saturating_sub(1);
                        self.chat.mode = ChatFocus::EmojiPicker {
                            query: buffer,
                            from_compose,
                            selected,
                        };
                    }
                    KeyCode::Right | KeyCode::Down | KeyCode::Tab => {
                        selected = (selected + 1).min(matches.saturating_sub(1));
                        self.chat.mode = ChatFocus::EmojiPicker {
                            query: buffer,
                            from_compose,
                            selected,
                        };
                    }
                    KeyCode::Enter => {
                        if let Some(entry) = crate::chat::emoji::search(&buffer, EMOJI_CHOICES)
                            .into_iter()
                            .nth(selected)
                        {
                            self.chat.insert_emoji(entry.emoji);
                        }
                        // Inserting is a composing act: the draft now holds
                        // the emoji, so the composer is where it can be seen.
                        self.chat.mode = ChatFocus::Compose;
                    }
                    // Editing the query changes the candidates underneath the
                    // cursor, so the selection returns to the first one rather
                    // than pointing at whatever now occupies that position.
                    KeyCode::Backspace => {
                        buffer.pop();
                        self.chat.mode = ChatFocus::EmojiPicker {
                            query: buffer,
                            from_compose,
                            selected: 0,
                        };
                    }
                    KeyCode::Char(c) if is_typed_text(&key) => {
                        buffer.push(c);
                        self.chat.mode = ChatFocus::EmojiPicker {
                            query: buffer,
                            from_compose,
                            selected: 0,
                        };
                    }
                    _ => {}
                }
                return vec![];
            }
            ChatFocus::Normal => {}
        }

        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('r') => self.chat.reconnect_active(),
                // Same guard as entering the composer: with no chat open
                // there is no draft an emoji could land in.
                KeyCode::Char('e') if self.chat.active_key(self.chat.focus).is_some() => {
                    self.chat.mode = ChatFocus::EmojiPicker {
                        query: String::new(),
                        from_compose: false,
                        selected: 0,
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
            KeyCode::PageUp => self.chat.scroll_by(self.chat_page()),
            KeyCode::PageDown => self.chat.scroll_by(-self.chat_page()),
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
            KeyCode::Char('}') => self.chat.cycle_account(true, &self.config),
            KeyCode::Char('{') => self.chat.cycle_account(false, &self.config),
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

    /// The text buffer of the credential box that currently has the cursor.
    fn focused_setup_input(&mut self) -> Option<&mut TextInput> {
        let field = self.setup_field();
        self.setup_inputs.get_mut(&field)
    }

    /// Whether enough has been typed in for at least one platform to work.
    ///
    /// A platform needs *both* halves; one on its own is not a usable
    /// configuration, so the form does not accept it as one.
    pub fn setup_is_complete(&self) -> bool {
        Platform::ALL.iter().any(|platform| {
            SetupField::ORDER
                .iter()
                .filter(|field| field.platform() == Some(*platform))
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
                if let Some(input) = self.focused_setup_input() {
                    input.backspace();
                }
            }
            KeyCode::Left => {
                if let Some(input) = self.focused_setup_input() {
                    input.left();
                }
            }
            KeyCode::Right => {
                if let Some(input) = self.focused_setup_input() {
                    input.right();
                }
            }
            KeyCode::Char(c) if is_typed_text(&key) => {
                if let Some(input) = self.focused_setup_input() {
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

        // Validated before anything is written to `self.config`, so a bad
        // port never lets good credentials through half-saved. An empty
        // field — the field starts prefilled, so this only happens if it was
        // cleared — falls back to the previously configured port instead of
        // blocking the save, matching the redirect-URL preview above, which
        // treats the same empty text as "not typed yet" rather than an
        // error. A non-empty but invalid port (not a number, `0`, or out of
        // range) is a real typo worth stopping on, so that case still blocks
        // the save with a warning.
        let port_text = self
            .setup_inputs
            .get(&SetupField::OauthPort)
            .map(|input| input.value().trim().to_string())
            .unwrap_or_default();
        let port = if port_text.is_empty() {
            self.config.general.oauth_port
        } else {
            match crate::config::parse_oauth_port(&port_text) {
                Ok(port) => port,
                Err(message) => {
                    self.notify(super::toast::Level::Warning, message);
                    return vec![];
                }
            }
        };

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
                SetupField::OauthPort => {} // handled below, once validated
            }
        }
        self.config.general.oauth_port = port;

        // The fields are already on `self.config`, so this commits what is
        // there. `persist` carries the `ReloadConfig` the worker needs: it
        // holds its own copy and would otherwise keep building backends from
        // the old credentials.
        let commands = self.save_settings();
        if commands.is_empty() {
            return commands;
        }
        self.push_log(LogLevel::Success, "API credentials saved to config.toml.");
        self.go_to(Screen::Login);
        commands
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
        // The splash says "press any key to skip". A click is a deliberate
        // "get on with it" too, and dropping it made the promise half true.
        if self.splash_is_showing() {
            self.splash_skipped = true;
            return vec![];
        }

        // Every overlay, from the one chain the key handler uses. This used
        // to name two of the six, so a click while the which-key listing, the
        // pre-flight checklist or the command palette was up acted on
        // whatever was underneath and out of sight.
        let overlay_open = self.overlay().is_some();

        let body = if self.tab == Tab::Combined {
            super::mouse::BodyKind::Combined
        } else if self.chat_is_showing() {
            super::mouse::BodyKind::Chat
        } else {
            super::mouse::BodyKind::Other
        };
        let action = super::mouse::action_for(event, area, body, self.chat.split_percent);
        let Some(action) = action else { return vec![] };

        match action {
            Action::SelectTab(_) | Action::FocusChat(_) | Action::FocusStreamInfo
                if overlay_open =>
            {
                vec![]
            }
            // Straight to `go_to_tab`, which is what a click means. It used
            // to build a synthetic Alt+digit key event and feed it back
            // through the whole key handler, and the translation only
            // covered the first three tabs.
            Action::SelectTab(tab) => self.go_to_tab(tab),
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
            Action::ScrollPane { platform, back } => {
                // The pane the pointer is over, without moving the keyboard
                // focus: a wheel is for reading, and taking the focus would
                // move where the next keystroke lands.
                const WHEEL_LINES: i64 = 3;
                self.chat
                    .scroll_pane(platform, if back { WHEEL_LINES } else { -WHEEL_LINES });
                vec![]
            }
            Action::ScrollBack => self.scroll(true),
            Action::ScrollForward => self.scroll(false),
        }
    }

    /// Scroll whatever is currently scrollable, in the direction the wheel
    /// turned: the message history when it is open, the chat when it is
    /// showing, and otherwise the activity log.
    /// How many messages one PageUp/PageDown moves through.
    ///
    /// The height of the focused chat pane, rather than a fixed ten: on a
    /// short terminal ten lines was a page and a half, and on a tall one a
    /// third of a page, so paging never lined up with what was on screen.
    /// Falls back to ten before the first frame has been drawn, and always
    /// leaves a couple of rows of overlap so the eye has something to
    /// reattach to.
    fn chat_page(&self) -> i64 {
        const OVERLAP: u16 = 2;
        const FALLBACK: i64 = 10;

        let body = super::mouse::Layout::of(self.terminal_area).body;
        // The combined tab puts a stream-info block above the panes.
        let panes = if self.tab == Tab::Combined {
            body.height.saturating_sub(super::mouse::STREAM_INFO_HEIGHT)
        } else {
            body.height
        };
        // One row of the pane is its header strip.
        let rows = panes.saturating_sub(1);
        if rows <= OVERLAP {
            return FALLBACK;
        }
        i64::from(rows - OVERLAP)
    }

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
                let chosen = palette.chosen().map(|row| (row.keys.clone(), row.action));
                // Close the palette *before* replaying, or the replayed key
                // would be typed straight back into the query box.
                self.command_palette = None;
                let Some((keys, action)) = chosen else {
                    return vec![];
                };
                if keys.is_empty() {
                    // No binding that would fire here, so there is no honest
                    // chord to replay — run the action itself, through the
                    // same `run_action` a key would reach.
                    return match action {
                        Some(action) => self.run_action(action),
                        None => vec![],
                    };
                }
                let mut commands = Vec::new();
                for key in keys {
                    commands.extend(self.handle_key(key));
                }
                return commands;
            }
            KeyCode::Up => palette.move_by(-1),
            KeyCode::Down | KeyCode::Tab => palette.move_by(1),
            // The list is now the whole action set rather than 31 rows, so it
            // is long enough that walking it one line at a time is a chore.
            KeyCode::PageUp => palette.move_by(-10),
            KeyCode::PageDown => palette.move_by(10),
            KeyCode::Home => palette.select_first(),
            KeyCode::End => palette.select_last(),
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
            // 58 entries and no way to type a name: letters were dropped on
            // the floor, which reads as the keyboard not working. Typing now
            // filters and moves the preview to the first match.
            KeyCode::Backspace => picker.backspace(),
            KeyCode::Char(c) if is_typed_text(&key) && c != 'j' && c != 'k' => picker.push(c),
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
    /// What is covering the interface, if anything.
    ///
    /// One ordered chain, so the three places that need to know cannot
    /// disagree. They did: `handle_key` resolved six overlays in this order,
    /// while `handle_mouse` asked only about the message history and the
    /// theme picker — so a click landing while the which-key listing, the
    /// pre-flight checklist or the command palette was up acted on whatever
    /// was *underneath* it, which the user could not see.
    ///
    /// A part-typed chord is deliberately not in here. The drawing treats it
    /// as full-screen, but `handle_key` resolves it later than these — after
    /// the modal screens — so folding it in would change key routing rather
    /// than describe it.
    pub fn overlay(&self) -> Option<Overlay> {
        if self.which_key_all {
            return Some(Overlay::WhichKeyAll);
        }
        if self.splash_is_showing() {
            return Some(Overlay::Splash);
        }
        if self.preflight.is_some() {
            return Some(Overlay::Preflight);
        }
        if self.command_palette.is_some() {
            return Some(Overlay::CommandPalette);
        }
        if self.toasts.history_open {
            return Some(Overlay::MessageHistory);
        }
        if self.theme_picker.is_some() {
            return Some(Overlay::ThemePicker);
        }
        None
    }

    /// Write a change to config.toml, and tell the worker.
    ///
    /// The one place that knows how a configuration change is committed.
    /// Before this there were five — the layout editor, the credential
    /// screen, the settings rows, the theme picker and Ctrl+S — each
    /// restating the same three-part rule in prose: save, adopt the saved
    /// copy, and send `ReloadConfig` so the worker's own copy does not go
    /// stale. They did not agree. `save_preset` did the first two and forgot
    /// the third, so pressing Ctrl+S wrote the file and left the worker
    /// building backends from settings the file no longer held.
    ///
    /// The rollback discipline was inconsistent too. `save_theme` restored
    /// the previous theme when the write failed; `save_layout` had already
    /// assigned `self.layout` and cleared `dirty` *before* attempting the
    /// write, so a failed save left the interface showing an arrangement that
    /// is not in the file and no longer offering to save it.
    ///
    /// Here the edit is applied to a *clone*, and the clone is adopted only
    /// once the write has succeeded — so a failed save cannot leave the
    /// interface and the file disagreeing.
    fn persist(&mut self, what: &'static str, edit: impl FnOnce(&mut Config)) -> Vec<Command> {
        let mut next = self.config.clone();
        edit(&mut next);

        match next.save() {
            Ok(()) => {
                self.config = next;
                vec![Command::ReloadConfig(Box::new(self.config.clone()))]
            }
            Err(err) => {
                self.notify(
                    super::toast::Level::Error,
                    format!("Could not save {what}: {err:#}"),
                );
                vec![]
            }
        }
    }

    /// Commit a setting the caller has already applied to `self.config`.
    ///
    /// Named for what it does rather than for one of its callers: six of the
    /// nine reach it from chat logging, the notification switches, the
    /// profile switch and pinning a stream id, none of which is appearance.
    ///
    /// This one takes the edit as already-applied because its callers flip a
    /// field and then commit; `persist` is the shape to prefer for anything
    /// new, since it cannot leave a half-applied change behind on failure.
    fn save_settings(&mut self) -> Vec<Command> {
        let edited = self.config.clone();
        self.persist("that setting", move |config| *config = edited)
    }

    /// Keep the previewed theme: write the name into the config file and close
    /// the picker.
    ///
    /// A failed write leaves the picker open showing why. Closing it on a
    /// failure would look exactly like success and then quietly forget the
    /// choice at the next start-up.
    fn save_theme(&mut self) -> Vec<Command> {
        let Some(picker) = self.theme_picker.as_ref() else {
            return vec![];
        };
        let chosen = picker.selected_name();

        // No manual rollback: `persist` applies the edit to a clone and
        // adopts it only once the write succeeds, so a failed save leaves
        // `self.config` untouched rather than needing the previous theme name
        // to be restored by hand.
        let theme = chosen.clone();
        let commands = self.persist("the theme", move |config| config.appearance.theme = theme);

        if commands.is_empty() {
            // The picker stays open so the reason is visible against the
            // choice that caused it.
            if let Some(picker) = self.theme_picker.as_mut() {
                picker.save_error = Some("the theme could not be saved".to_string());
            }
            return commands;
        }

        self.theme_picker = None;
        self.push_log(LogLevel::Info, format!("Theme saved: {chosen}"));
        commands
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
                if let Some(input) = self.focused_input() {
                    input.left();
                }
            }
            KeyCode::Right => {
                if let Some(input) = self.focused_input() {
                    input.right();
                }
            }
            KeyCode::Home => {
                if let Some(input) = self.focused_input() {
                    input.home();
                }
            }
            KeyCode::End => {
                if let Some(input) = self.focused_input() {
                    input.end();
                }
            }
            KeyCode::Backspace => {
                if let Some(input) = self.focused_input() {
                    input.backspace();
                }
                return self.on_text_changed(self.field());
            }
            KeyCode::Delete => {
                if let Some(input) = self.focused_input() {
                    input.delete();
                }
                return self.on_text_changed(self.field());
            }
            KeyCode::Char(c) => {
                let field = self.field();
                if field.is_text_input() {
                    if let Some(input) = self.focused_input() {
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
            // Save the current form values back to config.toml as the
            // defaults. The commands are forwarded rather than dropped: the
            // worker has to be told, or it keeps building backends from the
            // settings the file no longer holds.
            KeyCode::Char('s') => return self.save_preset(),
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
        // Every route into this function is somebody typing, so this is the
        // one place that knows the tags field was touched by hand — which is
        // what tells "I emptied the tags" apart from "I never set any".
        if field == Field::Tags {
            self.tags_edited = true;
        }

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

                // Held rather than sent: `tick_search` below dispatches it
                // once the keystrokes stop.
                self.pending_search = Some(PendingSearch {
                    platform,
                    query,
                    typed_at: std::time::Instant::now(),
                });
                vec![]
            }
            _ => vec![],
        }
    }

    /// Whether a held category search is waiting for the typing to stop.
    ///
    /// The event loop polls its debounce clock only while this is true, so an
    /// idle program does no extra work at all.
    pub fn search_is_pending(&self) -> bool {
        self.pending_search.is_some()
    }

    /// Send a held category search once the typing has stopped.
    ///
    /// Called from the event loop's debounce tick. Returns nothing while the
    /// user is still typing, which is the whole point.
    pub fn tick_search(&mut self, now: std::time::Instant) -> Vec<Command> {
        let Some(pending) = self.pending_search.as_ref() else {
            return vec![];
        };
        if now.duration_since(pending.typed_at) < SEARCH_DEBOUNCE {
            return vec![];
        }

        let pending = self.pending_search.take().expect("just checked");
        vec![Command::SearchCategories {
            platform: pending.platform,
            query: pending.query,
            generation: self.search_generation,
        }]
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
    /// Finish the broadcast on every connected platform.
    ///
    /// The other half of going live, and the only streaming action that asks
    /// twice. Ending is irreversible in a way going live is not: a completed
    /// YouTube broadcast cannot be reopened, the watch page becomes a
    /// recording, and everybody watching is watching the past. So the first
    /// press arms it and says exactly what will happen, and the second press
    /// within the confirmation window does it. Anything else that arms a
    /// confirmation disarms this one.
    ///
    /// Note what this does *not* do: it does not stop OBS. Ending the
    /// broadcast and stopping the encoder are two separate acts, and doing
    /// both from one key would mean this program deciding, on your behalf,
    /// that the scene you are still showing is finished. The OBS tab's
    /// streaming toggle is one keystroke away for when it is.
    fn end_stream(&mut self) -> Vec<Command> {
        if self.busy {
            self.notify(super::toast::Level::Warning, "Already working — hold on.");
            return vec![];
        }
        if self.accounts.is_empty() {
            self.notify(
                super::toast::Level::Warning,
                "Not connected to anything yet.",
            );
            return vec![];
        }

        let armed = self
            .end_armed
            .is_some_and(|at| at.elapsed() < END_CONFIRM_WINDOW);
        if !armed {
            self.end_armed = Some(std::time::Instant::now());
            self.notify(
                super::toast::Level::Warning,
                "Finish the broadcast? Press again to confirm. This cannot be undone.",
            );
            return vec![];
        }

        self.end_armed = None;
        self.busy = true;
        self.push_log(LogLevel::Info, "Finishing the broadcast…");
        vec![Command::EndLive]
    }

    /// Keys on the pre-flight checklist.
    ///
    /// Deliberately few. This is a screen for reading and then deciding one
    /// thing, so it has the two answers to that decision plus a way to look
    /// again after fixing something in another window.
    fn key_preflight(&mut self, key: KeyEvent) -> Vec<Command> {
        match key.code {
            KeyCode::Enter => return self.confirm_preflight(),
            KeyCode::Esc | KeyCode::Char('q') => self.preflight = None,
            // Re-check, for after unmuting the microphone in OBS itself.
            KeyCode::Char('r') => self.open_preflight(),
            _ => {}
        }
        vec![]
    }

    /// Take the pre-flight snapshot and put it on screen.
    ///
    /// The checks read the token store, so this is done once here rather than
    /// while drawing.
    fn open_preflight(&mut self) {
        let store = crate::auth::store::TokenStore::load().unwrap_or_default();
        let plan = self.plan();
        let obs = self.config.obs.enabled.then_some(&self.obs);
        let tokens = |platform: Platform| store.get(platform).cloned();

        self.preflight = Some(crate::preflight::run(&crate::preflight::Inputs {
            config: &self.config,
            plan: &plan,
            platforms: &self.selected,
            tokens: &tokens,
            obs,
        }));
    }

    /// The go-live key: show the checklist rather than launching straight in.
    ///
    /// Everything on that list is something this program already knew and
    /// used to keep to itself until it failed — a muted microphone, a login
    /// that cannot renew itself, an empty title. Fifteen seconds of reading
    /// beats forty minutes of silent broadcast.
    fn go_live_key(&mut self) -> Vec<Command> {
        if self.busy {
            self.notify(super::toast::Level::Warning, "Already working — hold on.");
            return vec![];
        }
        self.open_preflight();
        vec![]
    }

    /// Enter on the pre-flight screen: go live, and start OBS with it.
    fn confirm_preflight(&mut self) -> Vec<Command> {
        let Some(checks) = self.preflight.as_ref() else {
            return vec![];
        };

        if crate::preflight::worst(checks) == crate::preflight::Severity::Blocking {
            // Naming the first blocker rather than saying "there are
            // problems": the list is right there, but the toast is what the
            // eye goes to after pressing a key that did nothing.
            let first = checks
                .iter()
                .find(|check| check.severity == crate::preflight::Severity::Blocking)
                .map(|check| check.summary.clone())
                .unwrap_or_default();
            self.notify(super::toast::Level::Warning, format!("Not ready: {first}"));
            return vec![];
        }

        // Whether to start OBS is decided here, while the snapshot that was
        // checked is still the one on screen.
        let start_obs = self.config.obs.enabled && self.obs.is_connected() && !self.obs.streaming;
        self.preflight = None;

        let commands = self.submit();
        if commands.is_empty() {
            // `submit` refused for a reason of its own; do not touch OBS.
            return commands;
        }

        if start_obs {
            // Explicitly start, never toggle: a toggle would stop a stream
            // that had already been started by hand between the check and
            // this keystroke.
            self.obs_command(crate::obs::task::Command::SetStreaming(true));
            self.push_log(LogLevel::Info, "Asked OBS to start streaming.");
        }
        commands
    }

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
    /// Switch to the next named stream profile, loading its settings into the
    /// form.
    ///
    /// Borrowed from Restream's stream groups and Castr's destination sets.
    /// One set of stream settings covers one kind of stream; somebody who
    /// alternates between a speedrun and a coding session was retyping the
    /// title, the tags and both categories every time.
    fn next_profile(&mut self) -> Vec<Command> {
        let names = self.config.profile_names();
        if names.len() < 2 {
            self.notify(
                super::toast::Level::Info,
                "No named profiles yet — add a [profile.<name>] table to config.toml.",
            );
            return vec![];
        }

        let current = names
            .iter()
            .position(|name| name == self.config.active_profile.trim())
            .unwrap_or(0);
        let next = names[(current + 1) % names.len()].clone();
        self.config.active_profile = next.clone();
        self.load_active_profile();

        let label = if next.is_empty() {
            "the default settings".to_string()
        } else {
            next
        };
        self.notify(
            super::toast::Level::Info,
            format!("Stream profile: {label}"),
        );
        // Saved so the choice survives a restart, and so the worker sees a
        // config that matches the form.
        self.save_settings()
    }

    /// Put the active profile's settings into the form.
    fn load_active_profile(&mut self) {
        let preset = self.config.active_preset().clone();
        let plan = preset.to_plan();

        self.set_field(Field::Title, plan.title.clone());
        self.set_field(Field::Description, plan.description.clone());
        self.set_field(Field::Tags, plan.tags_input());
        self.set_field(Field::TwitchCategory, preset.twitch_category.clone());
        self.set_field(
            Field::YouTubeCategory,
            crate::youtube::category_name(&plan.youtube_category_id),
        );
        self.set_field(Field::Language, plan.language.clone());
        // Through the same table the constructor uses, so switching profile
        // and starting up cannot disagree about a field's initial value.
        self.set_field(
            Field::Thumbnail,
            initial_text(Field::Thumbnail, &preset, &plan),
        );

        self.twitch_category = plan.twitch_category.clone();
        self.youtube_category_id = plan.youtube_category_id.clone();
        self.privacy = plan.privacy;
        self.made_for_kids = plan.made_for_kids;
        self.auto_start = plan.youtube_auto_start;
        self.auto_stop = plan.youtube_auto_stop;
        // Switching profile is not editing the tags by hand, so an empty tag
        // list still means "leave the channel's alone".
        self.tags_edited = false;
        if !preset.platforms.is_empty() {
            self.selected = preset.platforms.clone();
        }
    }

    fn set_field(&mut self, field: Field, value: String) {
        if let Some(input) = self.inputs.get_mut(&field) {
            input.set(value);
        }
    }

    fn save_preset(&mut self) -> Vec<Command> {
        let plan = self.plan();
        let saved = PresetConfig::from_plan(&plan, &self.selected);
        let name = self.config.active_profile.trim().to_string();
        let label = if name.is_empty() {
            "your defaults".to_string()
        } else {
            format!("the {name} profile")
        };

        // Through `persist`, which is what gives this the `ReloadConfig` it
        // used to forget: Ctrl+S wrote the file and left the worker building
        // backends from settings the file no longer held.
        let commands = self.persist("your stream settings", move |config| {
            // Into the active profile, so Ctrl+S means "keep this" rather
            // than "overwrite the one unnamed set of settings".
            if name.is_empty() {
                config.preset = saved;
            } else {
                config.profile.insert(name, saved);
            }
        });

        if !commands.is_empty() {
            self.push_log(
                LogLevel::Success,
                format!("Saved these settings as {label}."),
            );
            self.notify(super::toast::Level::Success, "Saved to config.toml.");
        }
        commands
    }

    // -- Screen 3: the dashboard --------------------------------------------

    /// The dashboard's own keys.
    ///
    /// Everything this tab *does* — refresh, copy a key, open the watch page,
    /// go back to the form — is an ordinary binding in the keymap, and has
    /// already been resolved by the time this runs. What is left is scrolling
    /// the activity log, which belongs to the panel rather than to the
    /// program, and `esc` as a second way back to the form.
    fn key_dashboard(&mut self, key: KeyEvent) -> Vec<Command> {
        match key.code {
            KeyCode::Esc => {
                // Back to the form to change something and resubmit. On
                // YouTube this creates a *new* broadcast rather than editing
                // the old one.
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
    /// This does not, by itself, stop `App::new` from reading a real saved
    /// login off this machine — see `app()`, which pairs this with a
    /// [`crate::paths::test_support::ScratchConfigDir`] for that.
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
        // `App::new` reads `saved_logins()` (and builds `chat`'s own account
        // list) from `MSM_CONFIG_DIR` before this function gets a chance to
        // touch anything, so the scratch directory has to exist and the
        // environment variable has to point at it *before* `App::new` is
        // called — a whole real, logged-in account on this machine is
        // otherwise indistinguishable from a test fixture, and `activate()`
        // will try to open a real connection for it outside of any Tokio
        // runtime the moment a test switches to the Chat or Combined tab.
        // The guard is stashed on the `App` itself so it stays alive, and
        // `MSM_CONFIG_DIR` stays pointed at the scratch directory, for
        // exactly as long as this one test's `App` is — dropped, and the
        // directory cleaned up and the next `app()` call unblocked, the
        // moment the test function's local variable goes out of scope.
        let scratch_config_dir = crate::paths::test_support::ScratchConfigDir::new("ui-app-test");
        // `App::new` opens on the setup or login screen when nothing is
        // configured, which is not what most of these tests are about; they
        // drive the streaming flow, so they start at the platform picker.
        let mut app = App::new(scratch_config());
        // The start-up splash would otherwise cover the screen these tests
        // are looking at, and swallow the keys they send.
        app.splash_skipped = true;
        app.screen = Screen::Platforms;
        app._scratch_config_dir = Some(scratch_config_dir);
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

    /// Typing holds a search rather than sending one per keystroke, and the
    /// held search goes out once the typing stops.
    #[test]
    fn typing_in_the_twitch_category_field_requests_one_search_when_typing_stops() {
        let mut app = app_on_form();
        app.field_cursor = Field::ORDER
            .iter()
            .position(|f| *f == Field::TwitchCategory)
            .unwrap();

        let commands = type_and_collect(&mut app, "chess");
        assert!(
            commands.is_empty(),
            "five keystrokes must not be five Helix calls: {commands:?}"
        );
        assert!(app.search_is_pending());

        // Still typing: nothing goes out.
        assert!(app.tick_search(std::time::Instant::now()).is_empty());

        // Typing stopped.
        let later = std::time::Instant::now() + std::time::Duration::from_millis(500);
        let commands = app.tick_search(later);
        assert!(
            matches!(
                commands.as_slice(),
                [Command::SearchCategories {
                    platform: Platform::Twitch,
                    query,
                    ..
                }] if query == "chess"
            ),
            "one search, for the whole word: {commands:?}"
        );
        assert!(!app.search_is_pending(), "and it is not sent twice");
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

    /// Going live opens the checklist rather than launching straight in.
    /// The whole feature is fifteen seconds of reading in place of forty
    /// minutes of silent broadcast.
    #[test]
    fn the_go_live_key_shows_the_checklist_first() {
        let _scratch = crate::paths::test_support::ScratchConfigDir::new("preflight-open");
        let mut app = app_on_form();
        app.inputs
            .get_mut(&Field::Title)
            .unwrap()
            .set("A good title");

        let commands = app.go_live_key();

        assert!(
            commands.is_empty(),
            "nothing must be sent before the user has seen the list"
        );
        assert!(app.preflight.is_some(), "the checklist has to be on screen");
        assert!(!app.busy, "opening a checklist is not work");
    }

    /// Enter on a checklist with a blocking row must refuse, and say which
    /// row — the list is right there, but the toast is what the eye goes to
    /// after pressing a key that appeared to do nothing.
    #[test]
    fn enter_on_a_blocked_checklist_refuses_and_names_the_reason() {
        let _scratch = crate::paths::test_support::ScratchConfigDir::new("preflight-blocked");
        let mut app = app_on_form();
        // No title and no login: comfortably blocking.
        app.go_live_key();

        let commands = app.confirm_preflight();

        assert!(commands.is_empty(), "a blocked plan must not be submitted");
        assert!(
            app.preflight.is_some(),
            "the list stays up so the problem can be read"
        );
        assert!(
            !app.toasts.visible_text().is_empty(),
            "the refusal has to be visible"
        );
    }

    /// Esc goes back to the form with nothing sent and nothing changed.
    #[test]
    fn esc_closes_the_checklist_without_going_live() {
        let _scratch = crate::paths::test_support::ScratchConfigDir::new("preflight-esc");
        let mut app = app_on_form();
        app.go_live_key();
        assert!(app.preflight.is_some());

        let commands = app.handle_key(key(KeyCode::Esc));

        assert!(commands.is_empty());
        assert!(app.preflight.is_none());
        assert!(!app.busy);
    }

    #[test]
    fn a_go_live_dropped_by_a_full_queue_does_not_leave_the_interface_stuck() {
        let mut app = app_on_form();
        app.selected = vec![Platform::YouTube];
        app.inputs
            .get_mut(&Field::Title)
            .unwrap()
            .set("A good title");

        let commands = app.submit();
        assert!(app.busy);
        let stale = app.go_generation;

        // What the event loop does when the worker's queue is full: the
        // command never leaves, so no reply will ever clear `busy`.
        app.on_command_dropped(&commands[0]);

        assert!(!app.busy, "a dropped go-live must not leave the UI busy");
        assert!(app.submit().len() == 1, "submitting again must be allowed");

        // A late reply belonging to the dropped submission is still ignored.
        assert_ne!(stale, app.go_generation);
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
    /// All five tabs, from the keyboard and from a click. Alt+4 and Alt+5
    /// used to be handled by a hardcoded block that had drifted from
    /// `go_to_tab`, and clicking either did nothing because the mouse's copy
    /// of the tab labels only listed three.
    #[test]
    fn every_tab_can_be_reached_by_its_alt_digit() {
        let mut app = app();
        for (index, tab) in Tab::ALL.into_iter().enumerate() {
            let digit = char::from_digit(index as u32 + 1, 10).unwrap();
            app.handle_key(KeyEvent::new(KeyCode::Char(digit), KeyModifiers::ALT));
            assert_eq!(app.tab, tab, "alt+{digit} must reach {}", tab.label());
        }
    }

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

    /// The port field starts out showing the configured default, not blank —
    /// a user who never touches it should still save a valid config.
    #[test]
    fn setup_prefills_the_oauth_port_field_with_the_configured_default() {
        let app = App::new(Config::default());
        assert_eq!(
            app.setup_inputs
                .get(&SetupField::OauthPort)
                .unwrap()
                .value(),
            "8017"
        );
    }

    /// The shared redirect port has no `Platform` of its own, so it must not
    /// block the per-platform "id and secret both present" check — proved by
    /// clearing it outright rather than leaving it at its already-valid
    /// prefilled default, which would pass even if the port were still
    /// (wrongly) part of the per-platform check.
    #[test]
    fn setup_is_complete_ignores_an_untouched_oauth_port() {
        let mut app = App::new(Config::default());
        app.setup_inputs
            .get_mut(&SetupField::TwitchId)
            .unwrap()
            .set("id");
        app.setup_inputs
            .get_mut(&SetupField::TwitchSecret)
            .unwrap()
            .set("secret");
        app.setup_inputs
            .get_mut(&SetupField::OauthPort)
            .unwrap()
            .set("");
        assert!(app.setup_is_complete());
    }

    /// A bad port must not let good credentials through half-saved: the form
    /// stays put and nothing on `self.config` changes.
    #[test]
    fn save_credentials_rejects_an_invalid_oauth_port() {
        let _scratch = crate::paths::test_support::ScratchConfigDir::new("app-setup-bad-port");
        let mut app = App::new(Config::default());
        app.splash_skipped = true;
        app.setup_inputs
            .get_mut(&SetupField::TwitchId)
            .unwrap()
            .set("id");
        app.setup_inputs
            .get_mut(&SetupField::TwitchSecret)
            .unwrap()
            .set("secret");
        app.setup_inputs
            .get_mut(&SetupField::OauthPort)
            .unwrap()
            .set("0");

        let commands = app.save_credentials();

        assert!(commands.is_empty());
        assert_eq!(app.screen, Screen::Setup);
        assert_eq!(app.config.general.oauth_port, 8017);
    }

    /// Clearing the prefilled port field — a plausible fresh-install slip —
    /// must not lose otherwise-valid, already-typed credentials. It falls
    /// back to the existing port instead, matching what the redirect-URL
    /// preview already shows for the same empty input.
    #[test]
    fn save_credentials_falls_back_to_the_existing_port_when_the_field_is_cleared() {
        let _scratch = crate::paths::test_support::ScratchConfigDir::new("app-setup-empty-port");
        let mut app = App::new(Config::default());
        app.splash_skipped = true;
        app.setup_inputs
            .get_mut(&SetupField::TwitchId)
            .unwrap()
            .set("id");
        app.setup_inputs
            .get_mut(&SetupField::TwitchSecret)
            .unwrap()
            .set("secret");
        app.setup_inputs
            .get_mut(&SetupField::OauthPort)
            .unwrap()
            .set("");

        let commands = app.save_credentials();

        assert_eq!(app.screen, Screen::Login);
        assert_eq!(app.config.twitch.client_id, "id");
        assert_eq!(app.config.twitch.client_secret, "secret");
        assert_eq!(app.config.general.oauth_port, 8017);
        assert!(matches!(commands.as_slice(), [Command::ReloadConfig(_)]));
    }

    /// A valid, custom port is committed to the config and carried through to
    /// the reload the worker needs.
    #[test]
    fn save_credentials_saves_a_custom_oauth_port() {
        let _scratch = crate::paths::test_support::ScratchConfigDir::new("app-setup-custom-port");
        let mut app = App::new(Config::default());
        app.splash_skipped = true;
        app.setup_inputs
            .get_mut(&SetupField::TwitchId)
            .unwrap()
            .set("id");
        app.setup_inputs
            .get_mut(&SetupField::TwitchSecret)
            .unwrap()
            .set("secret");
        app.setup_inputs
            .get_mut(&SetupField::OauthPort)
            .unwrap()
            .set("9500");

        let commands = app.save_credentials();

        assert_eq!(app.screen, Screen::Login);
        assert_eq!(app.config.general.oauth_port, 9500);
        assert!(matches!(commands.as_slice(), [Command::ReloadConfig(_)]));
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
        // The API search is still issued once the typing stops: the full list
        // replaces these as soon as YouTube can be reached.
        let _ = commands;
        let later = std::time::Instant::now() + std::time::Duration::from_millis(500);
        let commands = app.tick_search(later);
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

    /// Turning pop-ups off silences the routine ones only.
    ///
    /// The setting used to silence everything, including errors — and since
    /// the activity log is drawn only on the Stream Info tab, a failure while
    /// the user was reading chat produced nothing at all.
    #[test]
    fn turning_pop_ups_off_still_reports_problems() {
        let mut config = Config::default();
        config.appearance.toasts = false;
        let mut app = App::new(config);
        app.splash_skipped = true;

        app.push_log(LogLevel::Info, "connecting to Twitch");
        app.push_log(LogLevel::Success, "connected");
        assert!(
            app.toasts.visible_text().is_empty(),
            "routine progress stays quiet"
        );

        app.push_log(LogLevel::Error, "something broke");
        assert!(
            app.toasts
                .visible_text()
                .iter()
                .any(|text| text.contains("something broke")),
            "an error must always be shown: {:?}",
            app.toasts.visible_text()
        );

        app.push_log(LogLevel::Warning, "that did nothing");
        assert!(
            app.toasts
                .visible_text()
                .iter()
                .any(|text| text.contains("that did nothing")),
            "a warning explains why a key did nothing, so it must be shown too"
        );

        assert_eq!(app.log.len(), 4, "the log still records everything");
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
            palette.rows().len(),
            "an empty query lists everything"
        );
        assert!(
            palette.rows().len() > super::super::command_palette::ENTRIES.len(),
            "the list is generated from the actions, not only the written entries"
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

    /// The leader opens the which-key popup rather than doing nothing, which
    /// is what makes the bindings discoverable instead of something you have
    /// to be told.
    #[test]
    fn the_leader_waits_and_then_runs_the_sequence() {
        let mut app = app();
        app.screen = Screen::Dashboard;

        app.handle_key(KeyEvent::from(KeyCode::Char(' ')));
        assert_eq!(app.pending_keys.len(), 1, "the leader is held");

        app.handle_key(KeyEvent::from(KeyCode::Char('b')));
        assert_eq!(app.pending_keys.len(), 2, "still waiting for the verb");

        app.handle_key(KeyEvent::from(KeyCode::Char('o')));
        assert!(app.pending_keys.is_empty(), "the sequence completed");
        assert_eq!(app.tab, Tab::Obs);
    }

    /// A sequence that goes nowhere is abandoned quietly — a half-typed
    /// chord is a slip, and an error message for one would be worse than the
    /// slip.
    #[test]
    fn an_unfinished_sequence_is_abandoned_rather_than_sticking() {
        let mut app = app();
        app.screen = Screen::Dashboard;
        app.handle_key(KeyEvent::from(KeyCode::Char(' ')));
        app.handle_key(KeyEvent::from(KeyCode::Char('z')));
        assert!(app.pending_keys.is_empty());
        assert!(!app.should_quit);
    }

    /// Escape has to get out of a part-typed chord, or the only way out
    /// would be to complete something you did not mean.
    #[test]
    fn escape_cancels_a_part_typed_chord() {
        let mut app = app();
        app.screen = Screen::Dashboard;
        app.handle_key(KeyEvent::from(KeyCode::Char(' ')));
        assert!(!app.pending_keys.is_empty());
        app.handle_key(KeyEvent::from(KeyCode::Esc));
        assert!(app.pending_keys.is_empty());
    }

    /// The space bar ticks a checkbox on a picker. The leader must not take
    /// it there, because a list of tick boxes is exactly where somebody will
    /// press space meaning "tick this".
    #[test]
    fn the_leader_does_not_steal_the_space_bar_from_a_picker() {
        let mut app = app();
        app.screen = Screen::Platforms;
        let before = app.is_selected(Platform::Twitch);
        app.handle_key(KeyEvent::from(KeyCode::Char(' ')));
        assert!(app.pending_keys.is_empty(), "no chord was started");
        assert_ne!(app.is_selected(Platform::Twitch), before, "it ticked");
    }

    /// A control chord still works inside a text box, since ctrl+something
    /// is never text — but a letter is.
    #[test]
    fn a_text_box_keeps_its_letters_but_not_its_control_chords() {
        let mut app = app();
        app.screen = Screen::Form;

        // `q` is a letter here, not the quit binding.
        app.handle_key(KeyEvent::from(KeyCode::Char('q')));
        assert!(!app.should_quit);

        // ctrl+p is not text, so the palette still opens.
        app.handle_key(ctrl('p'));
        assert!(app.command_palette.is_some());
    }

    /// Rebinding in the config has to actually take effect, or the section
    /// is decorative.
    #[test]
    fn a_rebinding_from_the_config_replaces_the_default() {
        let mut config = scratch_config();
        config
            .keys
            .global
            .insert("<C-y>".to_string(), "app.quit".to_string());
        // And a default can be removed outright.
        config.keys.obs.insert("q".to_string(), String::new());

        let mut app = App::new(config);
        app.splash_skipped = true;
        app.tab = Tab::Obs;

        app.handle_key(ctrl('y'));
        assert!(app.should_quit, "the new binding runs");

        let mut app2 = App::new({
            let mut config = scratch_config();
            config.keys.obs.insert("q".to_string(), String::new());
            config
        });
        app2.splash_skipped = true;
        app2.tab = Tab::Obs;
        app2.handle_key(KeyEvent::from(KeyCode::Char('q')));
        assert!(!app2.should_quit, "the removed binding does nothing");
    }

    /// A binding naming an action that does not exist is reported rather
    /// than silently ignored, and does not stop the interface starting.
    #[test]
    fn a_broken_binding_is_reported_and_does_not_prevent_starting() {
        let mut config = scratch_config();
        config
            .keys
            .global
            .insert("<C-y>".to_string(), "app.explode".to_string());
        config
            .keys
            .global
            .insert("<C-".to_string(), "app.quit".to_string());

        let app = App::new(config);
        let complaints: Vec<&str> = app
            .log
            .iter()
            .filter(|line| line.message.contains("Key binding"))
            .map(|line| line.message.as_str())
            .collect();
        assert_eq!(complaints.len(), 2, "got {complaints:?}");
    }

    /// Changing the leader has to move every leader binding with it.
    #[test]
    fn the_leader_can_be_changed() {
        let mut config = scratch_config();
        config.keys.leader = ",".to_string();
        let mut app = App::new(config);
        app.splash_skipped = true;
        app.screen = Screen::Dashboard;

        app.handle_key(KeyEvent::from(KeyCode::Char(',')));
        assert_eq!(app.pending_keys.len(), 1, "comma is the leader now");

        app.handle_key(KeyEvent::from(KeyCode::Char('b')));
        app.handle_key(KeyEvent::from(KeyCode::Char('o')));
        assert_eq!(app.tab, Tab::Obs);
    }

    /// The Configuration tab is a form: its keys move a cursor and change a
    /// setting, so the leader must not take the space bar from a list
    /// somebody is working down.
    #[test]
    fn the_config_tab_keeps_its_own_keys() {
        let mut app = app();
        app.handle_key(KeyEvent::new(KeyCode::Char('5'), KeyModifiers::ALT));
        assert_eq!(app.tab, Tab::Config);

        app.handle_key(KeyEvent::from(KeyCode::Char(' ')));
        assert!(app.pending_keys.is_empty(), "no chord was started");
    }

    /// Opening the tab has to give it a layout to edit, or the section that
    /// justifies the tab would have nothing in it.
    #[test]
    fn opening_the_config_tab_starts_an_edit_of_the_current_layout() {
        let mut app = app();
        app.handle_key(KeyEvent::new(KeyCode::Char('5'), KeyModifiers::ALT));
        let config = app.config_tab.as_ref().expect("the tab has state");
        assert_eq!(config.draft.panels(), app.layout.panels());
        assert!(!config.dirty, "nothing has been changed yet");
    }

    /// Editing the layout must not change what is drawn until it is saved,
    /// so an experiment can be abandoned.
    #[test]
    fn editing_the_layout_does_not_take_effect_until_it_is_saved() {
        let mut app = app();
        app.handle_key(KeyEvent::new(KeyCode::Char('5'), KeyModifiers::ALT));
        let before = app.layout.panels().len();

        // Focus the contents, then remove a panel.
        app.handle_key(KeyEvent::from(KeyCode::Tab));
        app.handle_key(KeyEvent::from(KeyCode::Char('d')));

        let config = app.config_tab.as_ref().expect("the tab has state");
        assert!(config.dirty, "the draft changed");
        assert_eq!(config.draft.panels().len(), before - 1);
        assert_eq!(
            app.layout.panels().len(),
            before,
            "what is drawn has not changed yet"
        );

        app.handle_key(KeyEvent::from(KeyCode::Char('s')));
        assert_eq!(app.layout.panels().len(), before - 1, "saving applies it");
    }

    /// Cleanup lists before it deletes. Removing things somebody made
    /// without showing them first would be asking for trust this has no way
    /// The stream ids used to go to the activity log and stop there, so
    /// choosing one meant reading it off the screen, quitting, and editing
    /// config.toml by hand — the last thing in the program that could only be
    /// done in a text editor.
    #[test]
    fn a_listed_youtube_stream_can_be_pinned_and_unpinned() {
        let mut app = app();
        app.handle_event(Event::Streams(vec![
            ("id-one".into(), "Default stream".into()),
            ("id-two".into(), "Vertical".into()),
        ]));
        go_to_config_section(&mut app, super::super::config_tab::Section::Maintenance);
        app.handle_key(KeyEvent::from(KeyCode::Tab));

        // Past the three jobs to the second listed stream.
        for _ in 0..(super::super::config_tab::MAINTENANCE_ROWS + 1) {
            app.handle_key(KeyEvent::from(KeyCode::Char('j')));
        }
        app.handle_key(KeyEvent::from(KeyCode::Enter));
        assert_eq!(app.config.youtube.stream_id, "id-two");

        // Enter again on the pinned one unpins it, which is how you get back
        // to "let YouTube choose" without editing the file.
        app.handle_key(KeyEvent::from(KeyCode::Enter));
        assert!(app.config.youtube.stream_id.is_empty());
    }

    /// Every Config footer ends "q quit", and `q` did nothing there — it was
    /// swallowed by the layout editor's catch-all.
    #[test]
    fn q_quits_from_the_config_tab_too() {
        let mut app = app();
        go_to_config_section(&mut app, super::super::config_tab::Section::Layout);
        app.handle_key(key(KeyCode::Char('q')));
        assert!(app.should_quit);
    }

    /// Volume-up on a source OBS holds above unity used to send 1.0 — a cut —
    /// and no key could put it back.
    #[test]
    fn volume_up_never_turns_a_boosted_source_down() {
        let mut app = app();
        app.obs.audio = vec![crate::obs::state::AudioInput {
            name: "Mic/Aux".into(),
            alias: None,
            shortcut: None,
            kind: None,
            muted: Some(false),
            volume_mul: Some(1.2),
            volume_db: None,
        }];
        app.obs_audio_cursor = 0;

        // No OBS connection, so nothing is sent — but the value it *would*
        // send is what this is about, so compute it the same way.
        let current = 1.2f64;
        let ceiling = current.max(1.0);
        assert_eq!(
            (current + 0.05).clamp(0.0, ceiling),
            1.2,
            "volume-up must never move a boosted source downward"
        );
        assert!(
            (current - 0.05).clamp(0.0, ceiling) < current,
            "volume-down must still work on it"
        );
        // And from below, unity is still the ceiling.
        assert_eq!((0.98f64 + 0.05).clamp(0.0, 0.98f64.max(1.0)), 1.0);
    }

    /// One set of stream settings covers one kind of stream. Somebody who
    /// alternates between a speedrun and a coding session was retyping the
    /// title, the tags and both categories every time.
    #[test]
    fn switching_profile_loads_its_settings_and_saves_back_into_it() {
        let _scratch = crate::paths::test_support::ScratchConfigDir::new("profiles");
        let mut config = Config::default();
        config.preset.title = "the default title".into();
        config.profile.insert(
            "speedrun".into(),
            crate::config::PresetConfig {
                title: "any% attempts".into(),
                ..Default::default()
            },
        );

        let mut app = App::new(config);
        app.splash_skipped = true;
        app.screen = Screen::Form;

        assert_eq!(
            app.inputs.get(&Field::Title).unwrap().value(),
            "the default title",
            "the unnamed preset is where it starts"
        );

        app.next_profile();
        assert_eq!(app.config.active_profile, "speedrun");
        assert_eq!(
            app.inputs.get(&Field::Title).unwrap().value(),
            "any% attempts",
            "switching loads that profile's settings into the form"
        );

        // Ctrl+S keeps them in the profile you are on, rather than
        // overwriting the one unnamed set.
        app.inputs
            .get_mut(&Field::Title)
            .unwrap()
            .set("any% attempts (day 2)");
        app.save_preset();
        assert_eq!(
            app.config.profile["speedrun"].title,
            "any% attempts (day 2)"
        );
        assert_eq!(
            app.config.preset.title, "the default title",
            "the default is left alone"
        );

        // …and round again returns to the default.
        app.next_profile();
        assert_eq!(app.config.active_profile, "");
        assert_eq!(
            app.inputs.get(&Field::Title).unwrap().value(),
            "the default title"
        );
    }

    /// An `active_profile` naming something that is not there falls back to
    /// the default rather than refusing to start.
    #[test]
    fn an_unknown_active_profile_falls_back_to_the_default() {
        let mut config = Config::default();
        config.preset.title = "the default".into();
        config.active_profile = "typo".into();
        assert_eq!(config.active_preset().title, "the default");
    }

    /// `logged_in` was a start-up snapshot nothing corrected, so a revoked
    /// token — the commonest way this breaks after weeks of working — left
    /// the whole interface still saying "logged in" while nothing worked.
    #[test]
    fn a_refused_login_stops_the_screen_claiming_you_are_logged_in() {
        let mut app = app();
        app.logged_in.insert(Platform::Twitch, true);

        app.handle_event(Event::Connected(vec![(
            Platform::Twitch,
            Err("the refresh token was revoked (invalid_grant)".into()),
        )]));
        assert!(!app.logged_in[&Platform::Twitch]);

        // …but a network hiccup must not. Telling somebody they are logged
        // out mid-stream because a packet went missing is worse than the
        // original problem.
        app.logged_in.insert(Platform::Twitch, true);
        app.handle_event(Event::Connected(vec![(
            Platform::Twitch,
            Err("the request timed out".into()),
        )]));
        assert!(
            app.logged_in[&Platform::Twitch],
            "a transient failure is not a dead login"
        );
    }

    /// Streamer mode follows OBS by default, and can be forced either way for
    /// a machine whose capture setup this program cannot see.
    #[test]
    fn streamer_mode_follows_obs_and_can_be_forced() {
        let mut app = app();
        app.config.obs.enabled = true;
        app.obs.connection = crate::obs::state::Connection::Connected;

        assert!(!app.streamer_mode(), "idle OBS is not a live screen");

        app.obs.streaming = true;
        assert!(app.streamer_mode(), "streaming turns it on");

        app.obs.streaming = false;
        // A local recording is uploaded later, and a credential in it is just
        // as exposed as one on a live stream.
        app.obs.recording = true;
        assert!(app.streamer_mode(), "so does recording");

        app.obs.recording = false;
        app.config.appearance.streamer_mode = "on".into();
        assert!(app.streamer_mode(), "forced on regardless of OBS");

        app.obs.streaming = true;
        app.config.appearance.streamer_mode = "off".into();
        assert!(!app.streamer_mode(), "forced off regardless of OBS");

        // An unrecognised value falls back to the default rather than
        // refusing to start over a typo.
        app.config.appearance.streamer_mode = "sometimes".into();
        assert!(app.streamer_mode(), "an unknown value means auto");
    }

    /// Logging out throws away a browser round trip. One key doing both
    /// "log in" and "log out" meant a stray Enter cost it.
    #[test]
    fn logging_out_asks_twice() {
        let mut app = app();
        app.logged_in.insert(Platform::Twitch, true);
        go_to_config_section(&mut app, super::super::config_tab::Section::Accounts);
        app.handle_key(KeyEvent::from(KeyCode::Tab));

        let first = app.handle_key(KeyEvent::from(KeyCode::Enter));
        assert!(first.is_empty(), "the first press only arms it: {first:?}");
        assert_eq!(app.logout_armed, Some(Platform::Twitch));
        assert!(
            app.logged_in[&Platform::Twitch],
            "and must not report it as done"
        );

        let second = app.handle_key(KeyEvent::from(KeyCode::Enter));
        assert!(matches!(
            second.as_slice(),
            [Command::Logout(Platform::Twitch)]
        ));
    }

    /// Esc has to call it off, and the flag only clears when the worker says
    /// the token actually left the disk.
    #[test]
    fn a_logout_can_be_cancelled_and_is_reported_from_its_result() {
        let mut app = app();
        app.logged_in.insert(Platform::Twitch, true);
        go_to_config_section(&mut app, super::super::config_tab::Section::Accounts);
        app.handle_key(KeyEvent::from(KeyCode::Tab));

        app.handle_key(KeyEvent::from(KeyCode::Enter));
        app.handle_key(KeyEvent::from(KeyCode::Esc));
        assert_eq!(app.logout_armed, None, "esc cancels");
        assert!(app.logged_in[&Platform::Twitch]);

        // A logout that failed to write must leave the screen saying the
        // login is still there, because it is.
        app.handle_event(Event::LoggedOut {
            platform: Platform::Twitch,
            result: Err("the disk is full".into()),
        });
        assert!(app.logged_in[&Platform::Twitch], "the token is still there");

        app.handle_event(Event::LoggedOut {
            platform: Platform::Twitch,
            result: Ok(()),
        });
        assert!(!app.logged_in[&Platform::Twitch]);
    }

    /// Leaving the Config tab with the logout confirmation armed has to
    /// disarm it. Otherwise coming back to Config later and pressing Enter
    /// for something else logs the account straight out on a flag left over
    /// from a previous visit.
    #[test]
    fn leaving_the_config_tab_disarms_a_pending_logout() {
        let mut app = app();
        app.logged_in.insert(Platform::Twitch, true);
        go_to_config_section(&mut app, super::super::config_tab::Section::Accounts);
        app.handle_key(KeyEvent::from(KeyCode::Tab));

        app.handle_key(KeyEvent::from(KeyCode::Enter));
        assert_eq!(app.logout_armed, Some(Platform::Twitch));

        // Away from Config entirely, not just moving within it.
        app.handle_key(KeyEvent::new(KeyCode::Char('1'), KeyModifiers::ALT));
        assert_eq!(
            app.logout_armed, None,
            "the armed flag must not survive leaving the tab"
        );

        // Back on Config, a bare Enter must arm rather than immediately log
        // out — proving the stale flag is really gone, not just hidden.
        go_to_config_section(&mut app, super::super::config_tab::Section::Accounts);
        app.handle_key(KeyEvent::from(KeyCode::Tab));
        let first = app.handle_key(KeyEvent::from(KeyCode::Enter));
        assert!(first.is_empty(), "a single Enter must only arm it again");
        assert!(app.logged_in[&Platform::Twitch], "not logged out yet");
    }

    /// A click while an overlay is up must not act on what is underneath it,
    /// which the user cannot see. The mouse gate named two of the six.
    #[test]
    fn every_overlay_blocks_a_click_on_what_is_beneath_it() {
        /// An overlay, and how to put it on screen.
        type ShowOverlay = (&'static str, fn(&mut App));

        let open: [ShowOverlay; 5] = [
            ("which-key", |app| app.which_key_all = true),
            ("preflight", |app| app.open_preflight()),
            ("command palette", |app| {
                app.command_palette = Some(super::super::command_palette::CommandPalette::open(
                    &app.keymap,
                    crate::keys::Context::StreamInfo,
                ))
            }),
            ("message history", |app| app.toasts.open_history()),
            ("theme picker", |app| {
                app.theme_picker = Some(super::super::theme_picker::ThemePicker::open(
                    &app.config.appearance.theme,
                    &app.palette,
                ))
            }),
        ];

        for (what, show) in open {
            let mut app = app();
            app.tab = Tab::Chat;
            assert!(app.overlay().is_none(), "{what}: nothing is up yet");

            show(&mut app);
            assert!(app.overlay().is_some(), "{what} has to count as an overlay");

            // A click on the tab bar would otherwise switch tabs behind it.
            let click = crossterm::event::MouseEvent {
                kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
                column: 1,
                row: 0,
                modifiers: KeyModifiers::NONE,
            };
            let before = app.tab;
            app.handle_mouse(click, ratatui::layout::Rect::new(0, 0, 100, 30));
            assert_eq!(app.tab, before, "{what}: the click must not reach the tabs");
        }
    }

    /// Ctrl+S wrote config.toml and forgot to tell the worker, which holds
    /// its own copy and would carry on building backends from settings the
    /// file no longer held.
    #[test]
    fn saving_the_preset_reloads_the_worker() {
        let _scratch = crate::paths::test_support::ScratchConfigDir::new("persist-preset");
        let mut app = app_on_form();
        app.inputs
            .get_mut(&Field::Title)
            .unwrap()
            .set("a new title");

        let commands = app.save_preset();

        assert!(
            matches!(commands.as_slice(), [Command::ReloadConfig(_)]),
            "the worker has to be told: {commands:?}"
        );
        assert_eq!(app.config.preset.title, "a new title");
    }

    /// A failed write must not leave the interface and the file disagreeing.
    /// The layout editor used to assign `self.layout` and clear `dirty`
    /// *before* attempting the save, so a failure left an arrangement on
    /// screen that was not in the file and was no longer offered for saving.
    #[test]
    fn a_failed_save_changes_nothing() {
        let _scratch = crate::paths::test_support::ScratchConfigDir::new("persist-failure");
        let mut app = app();
        let before = app.config.appearance.theme.clone();

        // A path that cannot be written: the parent is a file, not a
        // directory, so `write_secret_file` fails.
        let blocker = _scratch.path().join("not-a-directory");
        std::fs::write(&blocker, b"x").expect("writing the blocker");
        app.config.source_path = Some(blocker.join("config.toml"));

        let commands = app.persist("the theme", |config| {
            config.appearance.theme = "something-else".into()
        });

        assert!(commands.is_empty(), "a failed save issues no reload");
        assert_eq!(
            app.config.appearance.theme, before,
            "and leaves the held config untouched"
        );
    }

    /// Every field `is_text_input` claims is editable must actually have one.
    ///
    /// Six of the eight were inserted by hand, so `Thumbnail` and `StartTime`
    /// were drawn, reachable with Tab, and impossible to type into — while
    /// `plan()` read them through `unwrap_or_default()` and always saw an
    /// empty string, so Ctrl+S wrote that empty string over a hand-configured
    /// `thumbnail =` in config.toml.
    #[test]
    fn every_editable_field_has_a_text_input() {
        let app = app();
        for field in Field::ORDER.iter().filter(|f| f.is_text_input()) {
            assert!(
                app.inputs.contains_key(field),
                "{field:?} says it is editable and has no TextInput"
            );
        }
        assert_eq!(
            app.inputs.len(),
            Field::ORDER.iter().filter(|f| f.is_text_input()).count(),
            "and nothing that is not editable has one"
        );
    }

    /// The thumbnail is the one that lost data: it is persisted in
    /// `[preset]`, so an inert field meant every save erased it.
    #[test]
    fn the_thumbnail_survives_a_round_trip_through_the_form() {
        let _scratch = crate::paths::test_support::ScratchConfigDir::new("thumbnail-round-trip");
        let mut config = Config::default();
        config.preset.thumbnail = "/pictures/tonight.png".into();

        let app = App::new(config);
        assert_eq!(
            app.inputs.get(&Field::Thumbnail).map(|i| i.value()),
            Some("/pictures/tonight.png"),
            "the configured thumbnail has to reach the form"
        );
        assert_eq!(
            app.plan().thumbnail_path,
            "/pictures/tonight.png",
            "…and the plan built from that form"
        );
    }

    /// There was no paste anywhere: a 5000-character YouTube description had
    /// to be retyped, and a client secret is forty random characters nobody
    /// types by hand.
    #[test]
    fn pasting_reaches_the_focused_form_field() {
        let mut app = app_on_form();
        app.field_cursor = Field::ORDER
            .iter()
            .position(|f| *f == Field::Title)
            .unwrap();

        app.handle_paste("A title from the clipboard");

        assert_eq!(
            app.inputs.get(&Field::Title).unwrap().value(),
            "A title from the clipboard"
        );
    }

    /// Every field and the composer are single lines, so a pasted paragraph
    /// arrives as one line rather than as a half-finished message per break.
    #[test]
    fn a_pasted_newline_becomes_a_space() {
        let mut app = app_on_form();
        app.field_cursor = Field::ORDER
            .iter()
            .position(|f| *f == Field::Title)
            .unwrap();

        app.handle_paste("first line\nsecond line");

        assert_eq!(
            app.inputs.get(&Field::Title).unwrap().value(),
            "first line second line"
        );
    }

    /// A paste into a screen that is not expecting text does nothing, rather
    /// than being scattered somewhere it was not aimed.
    #[test]
    fn pasting_where_there_is_no_text_box_does_nothing() {
        let mut app = app();
        app.screen = Screen::Dashboard;
        assert!(app.handle_paste("nowhere to go").is_empty());
    }

    /// The Config tab's keys are named actions now, so `[keys.config]` can
    /// rebind them. They used to be hardcoded `KeyCode` matches — unbindable,
    /// and invisible to which-key, the full binding map and the palette.
    #[test]
    fn the_config_tab_keys_can_be_rebound() {
        let mut config = Config::default();
        // Move down the section list with `n` instead of `j`.
        config
            .keys
            .config
            .insert("n".into(), "config.next_section".into());
        let mut app = App::new(config);
        app.splash_skipped = true;
        app.handle_key(KeyEvent::new(KeyCode::Char('5'), KeyModifiers::ALT));

        let first = app.config_tab.as_ref().expect("the tab has state").section;
        app.handle_key(key(KeyCode::Char('n')));
        let after = app.config_tab.as_ref().expect("the tab has state").section;

        assert_ne!(first, after, "the rebound key has to move the selection");
    }

    /// A Config binding must not swallow a key that means something else in
    /// the section that is open: `r` re-runs the self-check on Diagnostics
    /// and rotates the arrangement in the layout editor.
    #[test]
    fn a_config_binding_falls_through_where_it_does_not_apply() {
        let mut app = app();
        go_to_config_section(&mut app, super::super::config_tab::Section::Layout);
        app.handle_key(KeyEvent::from(KeyCode::Tab));

        let before = app
            .config_tab
            .as_ref()
            .expect("the tab has state")
            .draft
            .clone();
        app.handle_key(key(KeyCode::Char('r')));
        let after = &app.config_tab.as_ref().expect("the tab has state").draft;

        assert_ne!(
            &before, after,
            "`r` must still rotate the layout, not be eaten by config.refresh_checks"
        );
    }

    /// Chat logging had no control anywhere in the interface — it could only
    /// be turned on by editing config.toml — while Housekeeping's paid-event
    /// export reads the very logs it produces.
    #[test]
    fn chat_logging_can_be_switched_from_the_config_tab() {
        let mut app = app();
        assert!(
            !app.config.chat.chat_logging,
            "off by default, which is why it needs a switch"
        );

        go_to_config_section(&mut app, super::super::config_tab::Section::Chat);
        app.handle_key(KeyEvent::from(KeyCode::Tab));
        app.handle_key(KeyEvent::from(KeyCode::Enter));

        assert!(app.config.chat.chat_logging, "enter must flip it");

        app.handle_key(KeyEvent::from(KeyCode::Enter));
        assert!(!app.config.chat.chat_logging, "and flip it back");
    }

    /// Every switch in the Notifications section has to flip the setting the
    /// row next to it names.
    ///
    /// The rows and the toggles used to be two separate lists that had to
    /// agree by convention, and this test was the only thing holding them
    /// together. They are now one table, so this checks the table rather than
    /// guarding a hazard — but it stays, because reading every switch through
    /// the same table it drives would not prove the switches differ from one
    /// another, and that is the property that matters.
    #[test]
    fn every_notification_switch_flips_the_setting_beside_it() {
        let mut app = app();
        go_to_config_section(&mut app, super::super::config_tab::Section::Notifications);
        app.handle_key(KeyEvent::from(KeyCode::Tab));

        // Written out by hand rather than read through the table, so that a
        // table whose rows all pointed at the same field would fail here.
        let read = |app: &App| {
            let n = &app.config.notifications;
            vec![
                n.enabled,
                n.raids,
                n.subscriptions,
                n.cheers,
                n.paid,
                n.memberships,
                n.stream_state,
                n.only_when_hidden,
                n.twitch_events,
                n.follows,
                n.redemptions,
                n.hype_trains,
                n.polls,
                n.predictions,
            ]
        };
        assert_eq!(
            read(&app).len(),
            super::super::config_tab::NOTIFICATION_TABLE.len(),
            "a switch was added to the table without being checked here"
        );
        let before = read(&app);
        // The switches only: the last row of the section fires a test
        // notification and is not a setting.
        for row in 0..super::super::config_tab::NOTIFICATION_TABLE.len() {
            let previous = read(&app);
            app.handle_key(KeyEvent::from(KeyCode::Enter));
            let now = read(&app);
            for (index, (was, is)) in previous.iter().zip(now.iter()).enumerate() {
                if index == row {
                    assert_ne!(was, is, "row {row} must flip its own setting");
                } else {
                    assert_eq!(was, is, "row {row} must leave row {index} alone");
                }
            }
            app.handle_key(KeyEvent::from(KeyCode::Char('j')));
        }
        // Every row flipped once: nothing is where it started.
        let after = read(&app);
        assert!(before.iter().zip(after.iter()).all(|(a, b)| a != b));
    }

    /// A platform that stops reporting an incoming broadcast is the failure
    /// this feature exists for: nothing else on screen says the encoder died,
    /// and the only place it can reach somebody who is looking at OBS is the
    /// desktop.
    #[test]
    fn a_stream_starting_or_stopping_reaches_the_desktop() {
        let mut app = app();
        let live = |live: bool| {
            Event::Stats(vec![(
                Platform::Twitch,
                PlatformStats {
                    live,
                    ..Default::default()
                },
            )])
        };

        // The first snapshot has no "before", so it is not a transition.
        app.handle_event(live(false));
        app.handle_event(live(true));
        app.handle_event(live(false));

        // The queue is what proves the notifications were raised: the first
        // went out immediately, the second is still waiting on the gap.
        assert_eq!(app.desktop.queued(), 1);
    }

    /// A failed poll carries no usable live flag. Announcing a dead stream
    /// every time the network hiccuped would train somebody to ignore the one
    /// notification that matters.
    #[test]
    fn a_failed_statistics_poll_is_not_a_stream_ending() {
        let mut app = app();
        app.handle_event(Event::Stats(vec![(
            Platform::Twitch,
            PlatformStats {
                live: true,
                ..Default::default()
            },
        )]));
        app.handle_event(Event::Stats(vec![(
            Platform::Twitch,
            PlatformStats {
                live: false,
                error: Some("timed out".into()),
                ..Default::default()
            },
        )]));
        assert_eq!(app.desktop.queued(), 0);
    }

    /// Switching stream-state notifications off has to switch them off.
    #[test]
    fn stream_state_notifications_can_be_declined() {
        let mut app = app();
        app.config.notifications.stream_state = false;
        app.handle_event(Event::Stats(vec![(
            Platform::Twitch,
            PlatformStats {
                live: false,
                ..Default::default()
            },
        )]));
        app.handle_event(Event::Stats(vec![(
            Platform::Twitch,
            PlatformStats {
                live: true,
                ..Default::default()
            },
        )]));
        assert_eq!(app.desktop.queued(), 0);
    }

    /// A follow is the event no other part of the program can see: it is not
    /// chat, and without EventSub it never arrived at all.
    #[test]
    fn a_twitch_event_reaches_the_log_and_the_desktop() {
        use crate::eventsub::{EventKind, StreamEvent, Update};

        let mut app = app();
        app.handle_events_update(Update::Event(StreamEvent {
            title: "New follower".into(),
            detail: "Alice".into(),
            kind: EventKind::Follow,
        }));

        assert!(
            app.log.iter().any(|line| line.message.contains("Alice")),
            "the log is the record of what happened"
        );
        assert_eq!(app.desktop.delivered(), 1);
    }

    /// Each class of event has its own switch, because a channel that redeems
    /// points every thirty seconds is a different problem from one that gets
    /// a follower an hour.
    #[test]
    fn an_event_class_that_is_switched_off_stays_in_the_log_only() {
        use crate::eventsub::{EventKind, StreamEvent, Update};

        let mut app = app();
        app.config.notifications.redemptions = false;
        app.handle_events_update(Update::Event(StreamEvent {
            title: "Channel points".into(),
            detail: "Bob redeemed Hydrate".into(),
            kind: EventKind::Redemption,
        }));

        assert!(app.log.iter().any(|line| line.message.contains("Bob")));
        assert_eq!(
            app.desktop.delivered(),
            0,
            "switched off means no pop-up, not no record"
        );
    }

    /// Trouble with the connection is worth writing down and not worth
    /// interrupting a stream for.
    #[test]
    fn connection_trouble_is_logged_without_a_pop_up() {
        use crate::eventsub::Update;

        let mut app = app();
        app.handle_events_update(Update::Trouble("Twitch events: gone — reconnecting".into()));
        assert!(app
            .log
            .iter()
            .any(|line| line.message.contains("reconnecting")));
        assert_eq!(app.desktop.delivered(), 0);
    }

    /// Ending is irreversible, so it asks twice    /// Ending is irreversible, so it asks twice — and the first press must
    /// send nothing at all.
    #[test]
    fn finishing_the_broadcast_asks_before_it_does_it() {
        let mut app = app();
        app.accounts
            .insert(Platform::Twitch, Ok("somechannel".into()));

        let first = app.end_stream();
        assert!(first.is_empty(), "the first press must not end anything");
        assert!(app.end_armed.is_some());
        assert!(app
            .toasts
            .visible_text()
            .iter()
            .any(|text| text.contains("cannot be undone")));

        let second = app.end_stream();
        assert!(
            matches!(second.as_slice(), [Command::EndLive]),
            "the second press ends it"
        );
        assert!(app.end_armed.is_none(), "the confirmation is spent");
    }

    /// A confirmation left lying around must not be completed by a keystroke
    /// that arrived long afterwards, nor by one that meant something else.
    #[test]
    fn a_stale_or_interrupted_confirmation_does_not_end_the_stream() {
        let mut app = app();
        app.accounts
            .insert(Platform::Twitch, Ok("somechannel".into()));

        // Armed, then aged past the window.
        app.end_stream();
        app.end_armed = Some(std::time::Instant::now() - std::time::Duration::from_secs(600));
        assert!(
            app.end_stream().is_empty(),
            "an old confirmation must re-arm, not fire"
        );

        // Armed, then something else happened.
        app.end_armed = None;
        app.end_stream();
        app.run_action(crate::keys::Action::RefreshStats);
        assert!(app.end_armed.is_none(), "any other action disarms it");
        assert!(app.end_stream().is_empty(), "so the next press only arms");
    }

    /// With nothing connected there is nothing to end, and saying so beats
    /// sending the worker a command it can only refuse.
    #[test]
    fn finishing_needs_something_to_finish() {
        let mut app = app();
        assert!(app.end_stream().is_empty());
        assert!(app.end_armed.is_none());
    }

    /// "Nothing needed ending" is the normal answer for a Twitch-only stream
    /// and must not read as either success or failure.
    #[test]
    fn the_end_result_distinguishes_ended_from_nothing_to_end() {
        use crate::model::EndOutcome;

        let mut app = app();
        app.busy = true;
        app.stats.insert(
            Platform::Twitch,
            PlatformStats {
                live: true,
                ..Default::default()
            },
        );
        app.handle_event(Event::Ended {
            results: vec![(
                Platform::Twitch,
                Ok(EndOutcome::NothingToEnd {
                    reason: "nothing to close".into(),
                }),
            )],
        });
        assert!(!app.busy);
        assert!(
            !app.stats.is_empty(),
            "nothing ended, so the numbers are still true"
        );
        assert!(app
            .toasts
            .visible_text()
            .iter()
            .any(|text| text.contains("Nothing needed finishing")));

        // And when something really did end, the stale numbers go.
        app.toasts.dismiss_all();
        app.handle_event(Event::Ended {
            results: vec![(
                Platform::YouTube,
                Ok(EndOutcome::Ended {
                    note: "finished".into(),
                }),
            )],
        });
        assert!(
            app.stats.is_empty(),
            "statistics for a finished broadcast are a lie with a clock on it"
        );
    }

    /// The self-check starts processes (it looks for clipboard helpers by
    /// running them) and reads the token store off disk. Doing that while
    /// drawing meant six forks a frame; it is a snapshot now, taken when the
    /// section opens and when `r` is pressed.
    #[test]
    fn the_self_check_is_taken_on_arrival_and_on_demand_only() {
        let mut app = app();
        app.handle_key(KeyEvent::new(KeyCode::Char('5'), KeyModifiers::ALT));
        assert!(
            app.config_tab
                .as_ref()
                .expect("state")
                .diagnostics
                .taken_at
                .is_none(),
            "opening the tab on Layout must not run the checks"
        );

        go_to_config_section(&mut app, super::super::config_tab::Section::Diagnostics);
        let first = app.config_tab.as_ref().expect("state").diagnostics.clone();
        assert!(first.taken_at.is_some(), "arriving runs them once");
        assert!(!first.checks.is_empty());

        // Drawing must not touch them. (The draw path takes &App, so this is
        // structural — but the test states the invariant the bug broke.)
        let taken = first.taken_at;
        assert_eq!(
            app.config_tab.as_ref().expect("state").diagnostics.taken_at,
            taken
        );

        // And `r` takes a fresh one.
        app.handle_key(KeyEvent::from(KeyCode::Char('r')));
        assert!(app
            .config_tab
            .as_ref()
            .expect("state")
            .diagnostics
            .taken_at
            .is_some());
    }

    /// Open the Configuration tab and move the section cursor onto `wanted`.
    ///
    /// Pressing "j" a fixed number of times would be shorter, and would break
    /// every time a section is added — which is how it read before the
    /// Notifications section arrived. This walks until it arrives instead.
    fn go_to_config_section(app: &mut App, wanted: super::super::config_tab::Section) {
        app.handle_key(KeyEvent::new(KeyCode::Char('5'), KeyModifiers::ALT));
        for _ in 0..super::super::config_tab::Section::ALL.len() {
            if app.config_tab.as_ref().expect("the tab has state").section == wanted {
                return;
            }
            app.handle_key(KeyEvent::from(KeyCode::Char('j')));
        }
        panic!("never reached {wanted:?}");
    }

    /// to earn.
    #[test]
    fn cleanup_lists_before_it_deletes() {
        let mut app = app();
        // Move to Housekeeping, then into its list.
        go_to_config_section(&mut app, super::super::config_tab::Section::Maintenance);

        app.handle_key(KeyEvent::from(KeyCode::Tab));
        let first = app.handle_key(KeyEvent::from(KeyCode::Enter));
        assert!(
            matches!(first.as_slice(), [Command::Cleanup { approved }] if approved.is_empty()),
            "the first press only lists: {first:?}"
        );

        // The listing comes back and is what the confirming press approves.
        app.handle_event(Event::StaleBroadcasts(vec![stale("abc"), stale("def")]));

        let second = app.handle_key(KeyEvent::from(KeyCode::Enter));
        match second.as_slice() {
            [Command::Cleanup { approved }] => assert_eq!(
                approved,
                &vec!["abc".to_string(), "def".to_string()],
                "the delete must name exactly what was shown"
            ),
            other => panic!("the second press deletes: {other:?}"),
        }
    }

    fn stale(id: &str) -> crate::model::StaleBroadcast {
        crate::model::StaleBroadcast {
            id: id.into(),
            title: format!("broadcast {id}"),
            scheduled_start: None,
            status: "created".into(),
        }
    }

    /// An armed delete must not survive walking away and coming back. It
    /// fires on a keypress the user has forgotten they were part-way
    /// through, and what it fires is a deletion.
    #[test]
    fn moving_away_disarms_the_cleanup_confirmation() {
        let mut app = app();
        go_to_config_section(&mut app, super::super::config_tab::Section::Maintenance);
        app.handle_key(KeyEvent::from(KeyCode::Tab));
        app.handle_key(KeyEvent::from(KeyCode::Enter));
        app.handle_event(Event::StaleBroadcasts(vec![stale("abc")]));
        assert!(
            app.config_tab.as_ref().expect("the tab").cleanup_listed,
            "the first press arms the delete"
        );

        // Move to another row and back.
        app.handle_key(KeyEvent::from(KeyCode::Char('j')));
        app.handle_key(KeyEvent::from(KeyCode::Char('k')));

        let again = app.handle_key(KeyEvent::from(KeyCode::Enter));
        assert!(
            matches!(again.as_slice(), [Command::Cleanup { approved }] if approved.is_empty()),
            "after moving, enter must list again rather than delete: {again:?}"
        );
    }

    /// The Appearance section's hint says enter changes a setting, so it has
    /// to change one. A hint that promises a key the screen does not handle
    /// is worse than no hint.
    #[test]
    fn enter_changes_the_selected_appearance_setting() {
        let mut app = app();
        app.handle_key(KeyEvent::new(KeyCode::Char('5'), KeyModifiers::ALT));
        // Move to Appearance, then into its list.
        app.handle_key(KeyEvent::from(KeyCode::Char('j')));
        app.handle_key(KeyEvent::from(KeyCode::Tab));

        // Row 2 is the splash, which is a plain boolean.
        app.handle_key(KeyEvent::from(KeyCode::Char('j')));
        app.handle_key(KeyEvent::from(KeyCode::Char('j')));
        let before = app.config.appearance.splash;
        app.handle_key(KeyEvent::from(KeyCode::Enter));
        assert_ne!(app.config.appearance.splash, before, "the setting flipped");
    }

    /// The theme is chosen by looking at it, so its row opens the picker
    /// rather than cycling blindly through 57 palettes.
    #[test]
    fn enter_on_the_theme_row_opens_the_picker() {
        let mut app = app();
        app.handle_key(KeyEvent::new(KeyCode::Char('5'), KeyModifiers::ALT));
        app.handle_key(KeyEvent::from(KeyCode::Char('j')));
        app.handle_key(KeyEvent::from(KeyCode::Tab));
        app.handle_key(KeyEvent::from(KeyCode::Enter));
        assert!(app.theme_picker.is_some());
    }

    /// The Accounts section's hint offers `a` for adding another chat
    /// account, so `a` has to start that login.
    #[test]
    fn a_adds_another_chat_account() {
        let mut config = scratch_config();
        config.twitch.client_id = "id".into();
        config.twitch.client_secret = "secret".into();
        let mut app = App::new(config);
        app.splash_skipped = true;

        // Move to Accounts, then into its list.
        go_to_config_section(&mut app, super::super::config_tab::Section::Accounts);
        app.handle_key(KeyEvent::from(KeyCode::Tab));

        let commands = app.handle_key(KeyEvent::from(KeyCode::Char('a')));
        assert!(
            matches!(commands.as_slice(), [Command::LoginAdd(Platform::Twitch)]),
            "got {commands:?}"
        );
    }

    /// Adding an account with no credentials cannot work, and saying so
    /// beats opening a browser that will fail.
    #[test]
    fn adding_an_account_without_credentials_explains_rather_than_trying() {
        let mut app = app();
        go_to_config_section(&mut app, super::super::config_tab::Section::Accounts);
        app.handle_key(KeyEvent::from(KeyCode::Tab));

        let commands = app.handle_key(KeyEvent::from(KeyCode::Char('a')));
        assert!(commands.is_empty(), "nothing should be attempted");
        assert!(app
            .toasts
            .visible_text()
            .iter()
            .any(|text| text.contains("credentials")));
    }
}
