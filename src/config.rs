//! Reading and writing `config.toml`.
//!
//! The config file holds two quite different kinds of thing:
//!
//! 1. **API credentials** — the client id and secret you get from the Twitch and
//!    Google developer consoles. These are set up once and then forgotten about.
//! 2. **Stream presets** — the title, tags, category and so on. This is the
//!    "or open the file with configuration" half of what you asked for: you can
//!    edit these by hand instead of using the form, and the form loads them as
//!    its starting values.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::model::{Category, Platform, Privacy, StreamPlan};
use crate::paths;

/// The whole `config.toml`, deserialised.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Where this config was read from, so `save()` writes back to the same
    /// file. Not part of the file format itself.
    ///
    /// In production this is always the path `paths::config_file()` names, and
    /// `MSM_CONFIG_DIR` is how that is redirected. It is a field rather than a
    /// call so that a save can never write somewhere other than where the load
    /// read — which is how a preset, and a copy of the client secrets, would
    /// quietly end up in the wrong file — and so the tests can point a whole
    /// load/save cycle at a scratch file.
    #[serde(skip)]
    pub source_path: Option<std::path::PathBuf>,
    pub twitch: TwitchConfig,
    pub youtube: YouTubeConfig,
    pub general: GeneralConfig,
    /// Chat pane settings.
    pub chat: ChatConfig,
    /// Desktop notifications: which stream events reach the system tray.
    pub notifications: NotificationsConfig,
    /// Colours, motion, and the optional interface extras.
    pub appearance: AppearanceConfig,
    /// Controlling OBS Studio from the OBS tab.
    pub obs: ObsConfig,
    /// Key bindings. Anything left out keeps its default.
    pub keys: KeysConfig,
    /// How the Combined tab is arranged.
    pub layout: crate::layout::LayoutFile,
    /// The saved stream settings the form starts from.
    pub preset: PresetConfig,

    /// Named alternatives to `[preset]`, as `[profile.speedrun]` and so on.
    ///
    /// One set of stream settings covers one kind of stream. Somebody who
    /// alternates between a speedrun and a coding session was retyping the
    /// title, the tags and both categories every time — or keeping two config
    /// files and swapping them.
    ///
    /// `[preset]` stays the unnamed default, so a config written before this
    /// existed keeps working exactly as it did.
    #[serde(default)]
    pub profile: std::collections::BTreeMap<String, PresetConfig>,

    /// Which named profile is in use. Empty means the unnamed `[preset]`.
    #[serde(default)]
    pub active_profile: String,
}

/// How the interface looks and how much it moves.
///
/// Everything here is cosmetic: nothing in this section can stop a stream
/// going live, and every value falls back to a sensible default rather than
/// refusing to start, because being locked out of your own stream by a
/// mistyped colour would be an absurd trade.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AppearanceConfig {
    /// Which palette to draw with: one of the 57 built-in names, or `custom`
    /// to use the `[appearance.custom_theme]` colours below.
    ///
    /// Config → Appearance lists every name, and ctrl+t previews them. An
    /// unrecognised name
    /// falls back to the default palette.
    pub theme: String,

    /// The nine colours used when `theme = "custom"`.
    ///
    /// Any role left blank falls back to the default palette's colour for
    /// that role, so a custom theme can override one colour without having to
    /// restate the other eight.
    pub custom_theme: CustomTheme,

    /// How much the interface animates: `fast`, `reduced`, or `off`.
    ///
    /// `reduced` keeps every effect but runs it in fewer, bigger steps, which
    /// is the setting to use if motion is uncomfortable or the terminal is on
    /// the far end of a slow connection. `off` renders every animated element
    /// at its finished frame — nothing is hidden, it simply does not move.
    pub animations: String,

    /// Show the animated start-up splash.
    pub splash: bool,

    /// React to mouse clicks and the scroll wheel.
    ///
    /// Turning this off gives the terminal back its own text selection, which
    /// some people would rather have than clickable tabs.
    pub mouse: bool,

    /// Show the process telemetry segment (cpu, memory, frame rate) in the
    /// status bar.
    pub telemetry: bool,

    /// Show routine pop-up notifications — progress, confirmations, "copied
    /// to the clipboard".
    ///
    /// Problems always produce a pop-up regardless of this setting. The
    /// activity log is drawn only at the bottom of the Stream Info tab, so
    /// an error raised while you are reading chat would otherwise be
    /// completely silent.
    pub toasts: bool,

    /// How long a pop-up notification stays on screen, in seconds.
    pub toast_seconds: u64,

    /// Hide things that must not be captured while you are live.
    ///
    /// `"auto"` (the default) turns it on whenever OBS reports that it is
    /// streaming or recording, and off again when it stops. `"on"` and
    /// `"off"` force it either way — `"on"` is for anybody whose capture
    /// setup this program cannot see, and `"off"` for a machine that never
    /// shares its screen.
    ///
    /// Borrowed from Chatterino, which does the same thing by noticing that
    /// OBS is running. This has a better signal than that: OBS tells it
    /// whether the stream is actually going out.
    pub streamer_mode: String,

    /// Repaint the terminal emulator's own background to match the theme.
    ///
    /// This uses an escape sequence (OSC 11) that changes the colour of the
    /// whole window rather than only the cells this program draws, and it is
    /// undone on exit. Turn it off if your terminal's own background is
    /// deliberately transparent or blurred, because the override replaces
    /// that with a solid colour.
    pub terminal_background: bool,
}

impl Default for AppearanceConfig {
    fn default() -> Self {
        Self {
            theme: crate::theme::DEFAULT_PRESET.to_string(),
            custom_theme: CustomTheme::default(),
            animations: "fast".to_string(),
            splash: true,
            mouse: true,
            telemetry: false,
            toasts: true,
            toast_seconds: 5,
            terminal_background: false,
            streamer_mode: "auto".to_string(),
        }
    }
}

/// A hand-written palette, one `#rrggbb` value per role.
///
/// Every field is optional — an empty string means "use the default
/// palette's colour for this role".
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct CustomTheme {
    pub background: String,
    pub foreground: String,
    pub accent: String,
    pub muted: String,
    pub border: String,
    pub surface: String,
    pub warning: String,
    pub error: String,
    pub success: String,
}

impl CustomTheme {
    /// Fill the blanks from the default palette and return a complete one.
    pub fn to_palette(&self) -> crate::theme::Palette {
        let base = crate::theme::default_palette();
        let pick = |value: &str, fallback: &str| {
            if value.trim().is_empty() {
                fallback.to_string()
            } else {
                value.trim().to_string()
            }
        };
        crate::theme::Palette {
            background: pick(&self.background, &base.background),
            foreground: pick(&self.foreground, &base.foreground),
            accent: pick(&self.accent, &base.accent),
            muted: pick(&self.muted, &base.muted),
            border: pick(&self.border, &base.border),
            surface: pick(&self.surface, &base.surface),
            warning: pick(&self.warning, &base.warning),
            error: pick(&self.error, &base.error),
            success: pick(&self.success, &base.success),
        }
    }
}

impl AppearanceConfig {
    /// How much motion the interface should use.
    ///
    /// An unrecognised value means the same as the default: something is
    /// wrong with the config file, but the interface still has to draw.
    pub fn animation_mode(&self) -> crate::anim::Mode {
        match crate::anim::Mode::parse(&self.animations) {
            Some(mode) => mode,
            None => {
                tracing::warn!(
                    animations = %self.animations,
                    "unknown animation mode; using the default"
                );
                crate::anim::Mode::default()
            }
        }
    }

    /// How long a pop-up notification stays up.
    ///
    /// Clamped rather than validated: a zero would make notifications vanish
    /// before they could be read, and a very large value would leave them
    /// stuck on screen with no way to clear them but a keypress.
    pub fn toast_duration(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.toast_seconds.clamp(1, 60))
    }

    /// The palette to draw with, plus whether the configured name was
    /// recognised. A `false` here is worth logging, not worth failing on.
    pub fn palette(&self) -> (crate::theme::Palette, bool) {
        crate::theme::resolve(&self.theme, &self.custom_theme.to_palette())
    }
}

/// Key bindings.
///
/// The defaults are shaped the way AstroNvim shapes Neovim's — a space
/// leader, two-letter mnemonic groups, a which-key popup — because that is a
/// shape many people who live in a terminal already have in their fingers.
/// Everything here changes that.
///
/// Bindings are written in vim's notation, which is the notation anyone who
/// would want to rebind a key already knows: `<Leader>os`, `<C-p>`, `]t`,
/// `<CR>`. A literal `<` is written `<lt>`, as in vim.
///
/// ```toml
/// [keys]
/// leader = "<Space>"
///
/// [keys.global]
/// "<Leader>os" = "obs.stream"
/// "<C-g>" = "stream.go_live"
/// "<Leader>q" = ""              # remove a default binding
///
/// [keys.chat]
/// "<C-j>" = "chat.next"
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct KeysConfig {
    /// The key every mnemonic sequence starts with.
    ///
    /// Space, as in AstroNvim. Changing it moves every `<Leader>…` binding at
    /// once, including the built-in ones, so nothing has to be rewritten.
    pub leader: String,

    /// Bindings that apply everywhere.
    pub global: std::collections::BTreeMap<String, String>,

    /// Bindings for the Stream Info tab.
    pub stream_info: std::collections::BTreeMap<String, String>,

    /// Bindings for the chat panes, on either the Chat or the Combined tab.
    pub chat: std::collections::BTreeMap<String, String>,

    /// Bindings for the OBS tab.
    pub obs: std::collections::BTreeMap<String, String>,

    /// Bindings for the Config tab.
    pub config: std::collections::BTreeMap<String, String>,
}

impl Default for KeysConfig {
    fn default() -> Self {
        Self {
            leader: "<Space>".to_string(),
            global: std::collections::BTreeMap::new(),
            stream_info: std::collections::BTreeMap::new(),
            chat: std::collections::BTreeMap::new(),
            obs: std::collections::BTreeMap::new(),
            config: std::collections::BTreeMap::new(),
        }
    }
}

impl KeysConfig {
    /// Build the keymap: the built-in bindings, with this file's changes
    /// applied on top.
    ///
    /// Returns the map and a list of anything wrong with the config. A bad
    /// binding is reported and skipped rather than refusing to start — being
    /// unable to open your own interface because of a typo in a key name
    /// would be a poor trade, and the report says exactly what to fix.
    pub fn keymap(&self) -> (crate::keys::Keymap, Vec<String>) {
        use crate::keys::{parse_chord, parse_leader, Action, Context, Keymap};

        let mut problems = Vec::new();

        let leader = match parse_leader(&self.leader) {
            Ok(leader) => leader,
            Err(err) => {
                problems.push(format!("leader {err}; using the default"));
                crate::keys::default_leader()
            }
        };

        let mut keymap = Keymap::defaults(leader);

        // Every context, in a fixed order, so a binding that appears in two
        // of them resolves the same way every run.
        for context in Context::ALL {
            let table = match context {
                Context::Global => &self.global,
                Context::StreamInfo => &self.stream_info,
                Context::Chat => &self.chat,
                Context::Obs => &self.obs,
                Context::Config => &self.config,
            };
            for (written, action_name) in table {
                let chord = match parse_chord(written, leader) {
                    Ok(chord) => chord,
                    Err(err) => {
                        problems.push(format!("[keys.{}] {err}", context.name()));
                        continue;
                    }
                };
                // An empty action removes the binding, which is how a default
                // gets turned off. There has to be a way to say "nothing
                // here" that is not "bind it to something harmless".
                if action_name.trim().is_empty() {
                    if !keymap.unbind(context, &chord) {
                        // Name where it *is* bound. Unbinding a chord that is
                        // not in this context is silent otherwise, and the
                        // key carries on working — which reads as the setting
                        // being ignored.
                        let elsewhere = keymap.contexts_binding(&chord);
                        let where_it_lives = if elsewhere.is_empty() {
                            "it is not bound anywhere".to_string()
                        } else {
                            format!(
                                "it is bound under {}",
                                elsewhere
                                    .iter()
                                    .map(|context| format!("[keys.{}]", context.name()))
                                    .collect::<Vec<_>>()
                                    .join(" and ")
                            )
                        };
                        problems.push(format!(
                            "[keys.{}] {written:?} is not bound there, so removing it did \
                             nothing — {where_it_lives}",
                            context.name()
                        ));
                    }
                    continue;
                }
                match Action::parse(action_name) {
                    Some(action) => keymap.bind(context, chord, action),
                    None => {
                        // `Action::ALL` is right here, so a near miss can be
                        // named rather than leaving somebody to search the
                        // documentation for a typo.
                        let suggestion = closest_action(action_name)
                            .map(|name| format!("; did you mean {name:?}?"))
                            .unwrap_or_default();
                        problems.push(format!(
                            "[keys.{}] {written:?}: there is no action called \
                             {action_name:?}{suggestion}",
                            context.name()
                        ));
                    }
                }
            }
        }

        problems.extend(shadowed_binding_problems(&keymap));
        problems.extend(unhandled_config_action_problems(&self.config));
        problems.extend(swallowed_binding_problems(&keymap));

        (keymap, problems)
    }
}

/// A binding that a tab-local one hides — worth mentioning because it is how
/// a tab gives a key its own meaning, and also how somebody's new global
/// binding quietly fails to work on one tab.
fn shadowed_binding_problems(keymap: &crate::keys::Keymap) -> Vec<String> {
    let mut problems = Vec::new();
    for (context, chord, local, global) in keymap.shadowed() {
        if local == global {
            continue;
        }
        problems.push(format!(
            "{chord} runs {local} on the {} tab, which hides the global {global}",
            context.name()
        ));
    }
    problems
}

/// A `[keys.config]` binding to something the Config tab does not handle. It
/// parses, stores and is then silently ignored: that tab owns its plain keys
/// and resolves only its own actions, so anything else reaches nothing at
/// all. Better to say so than to leave somebody wondering why their binding
/// does nothing there.
fn unhandled_config_action_problems(
    config: &std::collections::BTreeMap<String, String>,
) -> Vec<String> {
    use crate::keys::Action;

    let mut problems = Vec::new();
    for (written, action_name) in config {
        if action_name.trim().is_empty() {
            continue;
        }
        let Some(action) = Action::parse(action_name) else {
            continue;
        };
        let handled = action.name().starts_with("config.") || action == Action::Quit;
        if !handled {
            problems.push(format!(
                "[keys.config] {written:?} is bound to {action_name:?}, which the Config \
                 tab does not handle — that tab only runs its own config.* actions and \
                 app.quit"
            ));
        }
    }
    problems
}

/// A binding that buries a whole group under it. This one is worse than a
/// shadow: the keys do not do something else, they stop existing, and
/// `shadowed` above cannot see it because it only compares identical chords.
fn swallowed_binding_problems(keymap: &crate::keys::Keymap) -> Vec<String> {
    let mut problems = Vec::new();
    for (chord, action, buried) in keymap.swallowed() {
        problems.push(format!(
            "{chord} runs {action} and so makes {buried} longer binding(s) starting with it \
             unreachable"
        ));
    }
    problems
}

/// Talking to OBS Studio.
///
/// OBS has a WebSocket server built in (Tools → WebSocket Server Settings).
/// Turn it on there, and this can drive scenes, microphones, streaming and
/// recording without leaving the terminal.
///
/// Every value has a working default, and OBS not being there is not an
/// error: with `enabled = true` and no OBS running, the pane says it is not
/// connected and keeps trying quietly. Nothing here can stop a stream going
/// live through Twitch or YouTube.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ObsConfig {
    /// Whether to connect to OBS at all.
    ///
    /// On by default: connecting costs one local socket, and someone who has
    /// OBS running almost certainly wants the pane. With no OBS listening the
    /// attempt fails quietly and retries in the background, which is
    /// indistinguishable from having it turned off.
    pub enabled: bool,

    /// The host OBS is on. Almost always this machine.
    pub host: String,

    /// The name of an environment variable holding the host instead.
    ///
    /// Used when `host` is empty. Every credential in this file can already
    /// come from the environment; the address could not, which made one
    /// config file per machine the only way to point a shared dotfiles
    /// repository at a laptop's own OBS one day and a studio machine's the
    /// next.
    pub host_env: String,

    /// The port from OBS's WebSocket Server Settings.
    pub port: u16,

    /// The name of an environment variable holding the port instead.
    ///
    /// Used when `port` is 0, which is not a port anything can listen on and
    /// therefore an unambiguous "not set".
    pub port_env: String,

    /// The password from that same settings window, if one is set.
    ///
    /// Prefer `password_env` to putting it here: this file holds your API
    /// credentials too, but a password sitting in a file is one more place it
    /// can be read from, and an OBS password lets anyone who has it control
    /// your stream.
    pub password: String,

    /// The name of an environment variable holding the password.
    ///
    /// Used when `password` is empty. This is the better way round: the value
    /// lives in your shell profile or your password manager's exec wrapper
    /// rather than in a file that gets copied about.
    pub password_env: String,

    /// Short names for scenes, as `alias = "OBS scene name"`.
    ///
    /// `brb = "Be Right Back"` names the scene "brb" in the OBS pane and lets a
    /// shortcut reach it,
    /// and shows "brb" in the pane. Without one, the scene's real name is
    /// used for both.
    pub scene_aliases: std::collections::BTreeMap<String, String>,

    /// One-key shortcuts for scenes, as `key = "OBS scene name"`.
    ///
    /// Pressing that key in the OBS tab switches to the scene. Keep them to
    /// single characters; anything longer can never be typed as a shortcut.
    pub scene_shortcuts: std::collections::BTreeMap<String, String>,

    /// Short names for audio inputs, as `alias = "OBS input name"`.
    pub audio_aliases: std::collections::BTreeMap<String, String>,

    /// One-key shortcuts for muting audio inputs, as `key = "OBS input name"`.
    pub audio_shortcuts: std::collections::BTreeMap<String, String>,
}

impl Default for ObsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            host: "127.0.0.1".to_string(),
            host_env: "OBS_WEBSOCKET_HOST".to_string(),
            // obs-websocket's own default port.
            port: 4455,
            port_env: "OBS_WEBSOCKET_PORT".to_string(),
            password: String::new(),
            password_env: "OBS_WEBSOCKET_PASSWORD".to_string(),
            scene_aliases: std::collections::BTreeMap::new(),
            scene_shortcuts: std::collections::BTreeMap::new(),
            audio_aliases: std::collections::BTreeMap::new(),
            audio_shortcuts: std::collections::BTreeMap::new(),
        }
    }
}

impl ObsConfig {
    /// The host to connect to: the file's value, the environment's, or this
    /// machine.
    ///
    /// Same precedence as every credential — the file wins when both are set,
    /// because naming a value explicitly should beat inheriting one.
    pub fn host(&self) -> String {
        let literal = self.host.trim();
        if !literal.is_empty() {
            return literal.to_string();
        }
        env_value(&self.host_env).unwrap_or_else(|| "127.0.0.1".to_string())
    }

    /// The port to connect to.
    ///
    /// A zero in the file means "not set" — nothing can listen on port zero,
    /// so it cannot be a real answer — and the environment is consulted next.
    /// A variable holding something that is not a port is ignored with a log
    /// line rather than crashing the interface at start-up; obs-websocket's
    /// own default is a far better answer than refusing to run.
    pub fn port(&self) -> u16 {
        if self.port != 0 {
            return self.port;
        }
        let Some(raw) = env_value(&self.port_env) else {
            return 4455;
        };
        match raw.parse::<u16>() {
            Ok(0) | Err(_) => {
                tracing::warn!(
                    variable = %self.port_env,
                    value = %raw,
                    "ignoring an OBS port that is not a number between 1 and 65535"
                );
                4455
            }
            Ok(port) => port,
        }
    }

    /// The WebSocket address to connect to.
    pub fn url(&self) -> String {
        let host = self.host();
        // An IPv6 literal has to be bracketed in a URL, or the colons in the
        // address are read as the port separator.
        if host.contains(':') && !host.starts_with('[') {
            format!("ws://[{host}]:{}", self.port())
        } else {
            format!("ws://{host}:{}", self.port())
        }
    }

    /// The password to authenticate with, if there is one.
    ///
    /// The config file wins over the environment when both are set, because
    /// naming a value explicitly should beat inheriting one. An environment
    /// variable that is set but empty counts as unset — that is what an
    /// unfilled shell variable looks like, and treating it as a real empty
    /// password would fail in a way nobody could read.
    pub fn password(&self) -> Option<String> {
        let literal = self.password.trim();
        if !literal.is_empty() {
            return Some(literal.to_string());
        }
        let name = self.password_env.trim();
        if name.is_empty() {
            return None;
        }
        std::env::var(name)
            .ok()
            .filter(|value| !value.trim().is_empty())
    }

    /// Aliases and shortcuts keyed by OBS scene name, the way the connection
    /// task wants them.
    ///
    /// The config is written the other way round — `brb = "Be Right Back"` —
    /// because that is the readable direction for a person. Two people
    /// pointing different aliases at the same scene is a config mistake; the
    /// first in file order wins, and it is a BTreeMap so "first" is stable
    /// rather than depending on hashing.
    pub fn scene_labels(
        &self,
    ) -> std::collections::HashMap<String, (Option<String>, Option<String>)> {
        labels_by_target(&self.scene_aliases, &self.scene_shortcuts)
    }

    pub fn audio_labels(
        &self,
    ) -> std::collections::HashMap<String, (Option<String>, Option<String>)> {
        labels_by_target(&self.audio_aliases, &self.audio_shortcuts)
    }
}

/// Invert `alias -> target` and `shortcut -> target` into
/// `target -> (alias, shortcut)`.
fn labels_by_target(
    aliases: &std::collections::BTreeMap<String, String>,
    shortcuts: &std::collections::BTreeMap<String, String>,
) -> std::collections::HashMap<String, (Option<String>, Option<String>)> {
    let mut labels: std::collections::HashMap<String, (Option<String>, Option<String>)> =
        std::collections::HashMap::new();
    for (alias, target) in aliases {
        let entry = labels.entry(target.trim().to_string()).or_default();
        if entry.0.is_none() {
            entry.0 = Some(alias.trim().to_string());
        }
    }
    for (shortcut, target) in shortcuts {
        let entry = labels.entry(target.trim().to_string()).or_default();
        if entry.1.is_none() {
            entry.1 = Some(shortcut.trim().to_string());
        }
    }
    labels
}

/// Twitch application credentials, from <https://dev.twitch.tv/console/apps>.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TwitchConfig {
    /// The "Client ID" shown on your application's page.
    pub client_id: String,
    /// The "Client Secret" — press "New Secret" to generate one.
    ///
    /// Twitch requires this even for a desktop application; there is no
    /// secret-less flow for the scopes we need. It is stored locally and only
    /// ever sent to Twitch's own token endpoint.
    pub client_secret: String,

    /// The name of an environment variable holding the client id, used when
    /// `client_id` is empty.
    pub client_id_env: String,
    /// The name of an environment variable holding the client secret, used
    /// when `client_secret` is empty.
    ///
    /// Worth preferring to the file for the secret in particular: a value in
    /// the environment is not copied when the config file is, does not end up
    /// in a dotfiles repository by accident, and can come from a password
    /// manager at the moment the program starts.
    pub client_secret_env: String,
}

impl Default for TwitchConfig {
    fn default() -> Self {
        Self {
            client_id: String::new(),
            client_secret: String::new(),
            client_id_env: "MSM_TWITCH_CLIENT_ID".to_string(),
            client_secret_env: "MSM_TWITCH_CLIENT_SECRET".to_string(),
        }
    }
}

impl TwitchConfig {
    /// The client id to use, from the file or the environment.
    pub fn client_id(&self) -> String {
        resolve_credential(&self.client_id, &self.client_id_env)
    }

    /// The client secret to use, from the file or the environment.
    pub fn client_secret(&self) -> String {
        resolve_credential(&self.client_secret, &self.client_secret_env)
    }
}

/// Where a credential's value came from.
///
/// The documented footgun with these is a shell-profile variable that a
/// desktop launcher does not see, and that is precisely a question of
/// *source*: "the client id is present" is not the answer somebody debugging
/// it needs, because it is present in their terminal and absent in the
/// launcher.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CredentialSource {
    /// Written in config.toml.
    File,
    /// Read from the named environment variable.
    Environment(String),
    /// Neither place has it.
    Missing,
}

impl CredentialSource {
    /// A phrase to put after a credential's name.
    pub fn describe(&self) -> String {
        match self {
            CredentialSource::File => "from config.toml".to_string(),
            CredentialSource::Environment(name) => format!("from ${name}"),
            CredentialSource::Missing => "not set".to_string(),
        }
    }
}

/// The value of an environment variable, trimmed — or `None` if the variable
/// name is blank, the variable is unset, or its value is blank.
///
/// An environment variable that exists but is empty counts as unset: that is
/// what an unfilled shell variable looks like, and treating it as a real
/// value would fail later in a way nobody could read.
fn env_value(variable: &str) -> Option<String> {
    let variable = variable.trim();
    if variable.is_empty() {
        return None;
    }
    std::env::var(variable)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// Where [`resolve_credential`] would find this credential, without
/// producing the value itself.
fn credential_source(literal: &str, variable: &str) -> CredentialSource {
    if !literal.trim().is_empty() {
        return CredentialSource::File;
    }
    match env_value(variable) {
        Some(_) => CredentialSource::Environment(variable.trim().to_string()),
        None => CredentialSource::Missing,
    }
}

/// Read a credential from the file, falling back to an environment variable.
///
/// The file wins when both are set, because naming a value explicitly should
/// beat inheriting one. A variable that exists but is empty counts as unset:
/// that is what an unfilled shell variable looks like, and treating it as a
/// real empty credential would fail later in a way nobody could read.
fn resolve_credential(literal: &str, variable: &str) -> String {
    let literal = literal.trim();
    if !literal.is_empty() {
        return literal.to_string();
    }
    env_value(variable).unwrap_or_default()
}

/// Google/YouTube OAuth client credentials, from
/// <https://console.cloud.google.com/apis/credentials>. Create an OAuth client
/// of type **Desktop app**.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct YouTubeConfig {
    pub client_id: String,
    pub client_secret: String,

    /// The name of an environment variable holding the client id, used when
    /// `client_id` is empty.
    pub client_id_env: String,
    /// The name of an environment variable holding the client secret, used
    /// when `client_secret` is empty.
    pub client_secret_env: String,

    /// Reuse an existing reusable stream key instead of creating a new one for
    /// every broadcast.
    ///
    /// **This defaults to `true` and you almost certainly want to leave it on.**
    /// Creating a new YouTube "live stream" object mints a brand new stream key,
    /// which would mean re-configuring OBS (or the Aitum multistream plugin)
    /// before every single broadcast. Reusing one keeps your key stable forever.
    pub reuse_stream: bool,

    /// Pin a specific reusable stream by its API id. Leave empty to let the
    /// application pick the first reusable stream on your channel, which is what
    /// you want if you only have one.
    pub stream_id: String,
}

impl YouTubeConfig {
    /// The client id to use, from the file or the environment.
    pub fn client_id(&self) -> String {
        resolve_credential(&self.client_id, &self.client_id_env)
    }

    /// The client secret to use, from the file or the environment.
    pub fn client_secret(&self) -> String {
        resolve_credential(&self.client_secret, &self.client_secret_env)
    }
}

/// Settings for the Chat tab.
///
/// The defaults mirror the two reference implementations this feature was
/// ported from (`twi` for Twitch, `yc` for YouTube), except `scrollback_limit`
/// which defaults to 1000 messages per chat.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ChatConfig {
    /// Rules that pick a message out of the stream, and the logins never
    /// picked out. See [`crate::chat::rules`].
    #[serde(flatten)]
    pub highlights: crate::chat::rules::Highlights,

    /// How many messages each chat keeps in memory. Older messages are
    /// discarded; this is what keeps a chat open for hours from growing
    /// memory without bound.
    pub scrollback_limit: usize,

    /// The fastest the YouTube chat poller may ever poll, in milliseconds.
    /// YouTube's own `pollingIntervalMillis` still wins when it is higher —
    /// the server's floor is absolute.
    pub poll_interval_floor_ms: u64,

    /// An optional ceiling on the YouTube poll interval, in milliseconds.
    /// 0 means no ceiling. Raising the floor and ceiling stretches the daily
    /// API quota over a longer session at the cost of chat latency.
    pub poll_interval_ceiling_ms: u64,

    /// Your project's daily YouTube API quota, in units. The default matches
    /// Google's default allocation.
    pub daily_quota_units: u64,

    /// Stop polling when the estimated remaining quota falls below this
    /// percentage, keeping a reserve so message *sending* still works.
    pub quota_reserve_percent: u8,

    /// Desktop notifications for high-signal chat events (Super Chats,
    /// memberships) arriving in chats that are not on screen.
    pub notifications: bool,

    /// Write every chat message to append-only JSON Lines files.
    pub chat_logging: bool,
    /// Where those files go. Empty means `chatlog/` under the config
    /// directory (Config → Files shows where that is).
    pub chat_log_dir: String,
    /// Rotate a log file once it reaches this many bytes.
    pub chat_log_max_bytes: u64,
    /// Keep at most this many rotated files; older ones are pruned.
    pub chat_log_max_files: u64,
}

impl Default for ChatConfig {
    fn default() -> Self {
        Self {
            highlights: crate::chat::rules::Highlights::default(),
            scrollback_limit: 1000,
            poll_interval_floor_ms: 1000,
            poll_interval_ceiling_ms: 0,
            daily_quota_units: 10_000,
            quota_reserve_percent: 10,
            notifications: true,
            chat_logging: false,
            chat_log_dir: String::new(),
            chat_log_max_bytes: 10 * 1024 * 1024,
            chat_log_max_files: 5,
        }
    }
}

/// Desktop notifications — the pop-ups your desktop shows, not the ones drawn
/// inside this program.
///
/// The distinction matters, because there are two kinds and they answer
/// different questions. The in-program pop-ups (`[appearance] toasts`) tell you
/// what *this program* just did, and you only see them while you are looking at
/// the terminal. These are for what *your stream* just did, and they exist
/// precisely for the times you are not looking at the terminal — because you
/// are in OBS, or in the game, or making tea. A raid is the case that makes the
/// difference concrete: you have about ten seconds to greet four hundred
/// people, and a message in a chat pane on another workspace will not reach you
/// in ten seconds.
///
/// On Linux this needs no configuration and no extra package in the common
/// case: the program tries `notify-send`, then talks to the desktop's
/// notification service over D-Bus with `gdbus`, then tries `kdialog`, then
/// falls back to the terminal bell. See [`crate::notify`] for why the chain is
/// shaped that way.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct NotificationsConfig {
    /// The master switch for desktop notifications.
    pub enabled: bool,

    /// The shortest gap between two pop-ups, in milliseconds.
    ///
    /// Events arriving faster than this are queued, not discarded, and
    /// released one per gap — a gift drop delivers one event per recipient,
    /// and burying the raid that arrived in the middle of it would defeat the
    /// point. Zero means no pacing at all.
    pub min_gap_ms: u64,

    /// Only notify about chat events when that chat is not on screen.
    ///
    /// Off by default, which is a deliberate change from how this behaved when
    /// it was a chat-only feature. The old rule assumed you were reading chat
    /// in this program; in practice the terminal is on a second monitor and a
    /// raid still needs to reach you. Turn this on if you *do* watch chat here
    /// and find the pop-ups redundant. Stream-state notifications ignore it.
    pub only_when_hidden: bool,

    /// Raids: another streamer sending their audience to your channel.
    pub raids: bool,
    /// Subscriptions, renewals, tier upgrades and gifted subs.
    pub subscriptions: bool,
    /// Twitch cheers (bits) and bits-badge milestones.
    pub cheers: bool,
    /// YouTube Super Chats and Super Stickers.
    pub paid: bool,
    /// YouTube channel memberships.
    pub memberships: bool,
    /// Stream state: going live, a platform failing to go live, and a
    /// broadcast that stops while the program is watching it.
    pub stream_state: bool,

    /// Watch Twitch for the events that never reach chat.
    ///
    /// Raids, subscriptions and cheers arrive over the chat connection, so
    /// they need nothing extra. Follows, channel-point redemptions, hype
    /// trains, polls and predictions are not chat and never have been: they
    /// come over a second connection (EventSub), which this switch opens.
    ///
    /// Off means the connection is never made at all — this is not a display
    /// filter, it is whether to hold a second WebSocket open.
    pub twitch_events: bool,
    /// New followers.
    pub follows: bool,
    /// Channel-point redemptions, with whatever the viewer typed.
    pub redemptions: bool,
    /// Hype trains starting and finishing.
    pub hype_trains: bool,
    /// Polls starting.
    pub polls: bool,
    /// Predictions starting.
    ///
    /// Its own switch rather than sharing the poll one. A poll is a bit of
    /// fun; a prediction has channel points staked on it and has to be
    /// resolved before it locks, which is a different level of "tell me about
    /// this now".
    pub predictions: bool,
}

impl Default for NotificationsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            min_gap_ms: 2000,
            only_when_hidden: false,
            raids: true,
            subscriptions: true,
            cheers: true,
            paid: true,
            memberships: true,
            stream_state: true,
            twitch_events: true,
            follows: true,
            // On, but quieter by nature: a busy channel redeems points
            // constantly, and this is the first switch anybody will want.
            redemptions: true,
            hype_trains: true,
            polls: true,
            predictions: true,
        }
    }
}

impl NotificationsConfig {
    /// Whether an EventSub event class is wanted on the desktop.
    pub fn wants(&self, kind: crate::eventsub::EventKind) -> bool {
        use crate::eventsub::EventKind;
        match kind {
            // The same switch as the poll-driven notice, so somebody who
            // turned stream-state notifications off does not start getting
            // them again from a second source.
            EventKind::StreamState => self.stream_state,
            EventKind::Follow => self.follows,
            EventKind::Redemption => self.redemptions,
            EventKind::HypeTrain => self.hype_trains,
            EventKind::Poll => self.polls,
            EventKind::Prediction => self.predictions,
        }
    }

    /// The settings the notifier itself needs, with the pacing clamped to
    /// something a desktop can survive.
    ///
    /// The ceiling is five minutes: a gap longer than that turns the queue
    /// into a place notifications go to be forgotten. The floor is zero, which
    /// is honest — somebody who asks for no pacing gets no pacing.
    pub fn notifier_settings(&self) -> crate::notify::Settings {
        crate::notify::Settings {
            enabled: self.enabled,
            min_gap: std::time::Duration::from_millis(self.min_gap_ms.min(300_000)),
        }
    }
}

/// Settings that are not specific to one platform.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct GeneralConfig {
    /// How often to refresh the live statistics, in seconds.
    ///
    /// Do not set this too low. YouTube's Data API has a daily quota and every
    /// poll spends a slice of it; 15 seconds is a good balance and is well below
    /// the rate at which either platform updates its viewer numbers anyway.
    pub poll_interval_secs: u64,

    /// The local TCP port the application listens on to catch the OAuth redirect.
    ///
    /// This must match the redirect URI you register in both developer consoles:
    /// `http://localhost:<port>/callback`.
    pub oauth_port: u16,
}

impl Default for GeneralConfig {
    fn default() -> Self {
        Self {
            poll_interval_secs: 15,
            oauth_port: 8017,
        }
    }
}

/// The saved stream settings, in a shape that is pleasant to hand-edit.
///
/// This is deliberately *not* [`StreamPlan`] itself: the plan stores the Twitch
/// category as a resolved `{id, name}` pair, but a human editing a file wants to
/// write `twitch_category = "Software and Game Development"` and have the
/// application look the id up.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PresetConfig {
    pub title: String,
    pub description: String,
    pub tags: Vec<String>,
    /// Category *name* as you would type it into Twitch's own UI. Resolved to an
    /// id at submit time by searching Twitch's category list.
    pub twitch_category: String,
    /// Cached id for the name above, so that a repeat run does not need to spend
    /// an API call re-resolving a category that has not changed.
    pub twitch_category_id: String,
    /// YouTube's numeric category id. 20 = Gaming, 28 = Science & Technology.
    pub youtube_category_id: String,
    pub language: String,
    pub privacy: Privacy,
    pub made_for_kids: bool,
    pub youtube_auto_start: bool,
    pub youtube_auto_stop: bool,
    /// A picture file to upload as the YouTube thumbnail, if you use the same
    /// one every time. Empty leaves whatever YouTube generates.
    ///
    /// The *start time* deliberately has no equivalent here. A saved absolute
    /// time would be in the past by the next session, and "always start in two
    /// hours" is not a default anybody wants — it is a decision made per
    /// stream, so the field starts empty every time.
    pub thumbnail: String,
    /// Which platforms are ticked when the application starts.
    pub platforms: Vec<Platform>,
}

impl Default for PresetConfig {
    fn default() -> Self {
        let plan = StreamPlan::default();
        Self {
            title: plan.title,
            description: plan.description,
            tags: plan.tags,
            twitch_category: String::new(),
            twitch_category_id: String::new(),
            youtube_category_id: plan.youtube_category_id,
            language: plan.language,
            privacy: plan.privacy,
            made_for_kids: plan.made_for_kids,
            youtube_auto_start: plan.youtube_auto_start,
            youtube_auto_stop: plan.youtube_auto_stop,
            thumbnail: plan.thumbnail_path,
            platforms: vec![Platform::Twitch, Platform::YouTube],
        }
    }
}

impl PresetConfig {
    /// Convert the file representation into the in-memory plan the rest of the
    /// application works with.
    pub fn to_plan(&self) -> StreamPlan {
        StreamPlan {
            title: self.title.clone(),
            description: self.description.clone(),
            tags: self.tags.clone(),
            // A preset says what to set, never what to remove: a file with no
            // tags in it must not wipe the tags on the channel.
            clear_tags: false,
            // Only treat the category as resolved when we have *both* halves of
            // the pair; a name with no id still needs a lookup.
            twitch_category: if self.twitch_category_id.is_empty()
                || self.twitch_category.is_empty()
            {
                None
            } else {
                Some(Category {
                    id: self.twitch_category_id.clone(),
                    name: self.twitch_category.clone(),
                })
            },
            youtube_category_id: self.youtube_category_id.clone(),
            language: self.language.clone(),
            privacy: self.privacy,
            made_for_kids: self.made_for_kids,
            youtube_auto_start: self.youtube_auto_start,
            youtube_auto_stop: self.youtube_auto_stop,
            scheduled_start: None,
            thumbnail_path: self.thumbnail.clone(),
        }
    }

    /// Convert a plan back into the file representation, so the TUI can save
    /// whatever the user just typed as the new defaults.
    pub fn from_plan(plan: &StreamPlan, platforms: &[Platform]) -> Self {
        Self {
            title: plan.title.clone(),
            description: plan.description.clone(),
            tags: plan.tags.clone(),
            twitch_category: plan
                .twitch_category
                .as_ref()
                .map(|c| c.name.clone())
                .unwrap_or_default(),
            twitch_category_id: plan
                .twitch_category
                .as_ref()
                .map(|c| c.id.clone())
                .unwrap_or_default(),
            youtube_category_id: plan.youtube_category_id.clone(),
            language: plan.language.clone(),
            privacy: plan.privacy,
            made_for_kids: plan.made_for_kids,
            youtube_auto_start: plan.youtube_auto_start,
            youtube_auto_stop: plan.youtube_auto_stop,
            thumbnail: plan.thumbnail_path.clone(),
            platforms: platforms.to_vec(),
        }
    }
}

impl Default for YouTubeConfig {
    fn default() -> Self {
        Self {
            client_id: String::new(),
            client_secret: String::new(),
            client_id_env: "MSM_YOUTUBE_CLIENT_ID".to_string(),
            client_secret_env: "MSM_YOUTUBE_CLIENT_SECRET".to_string(),
            // Defaulting to `true` is the whole point — see the field docs.
            reuse_stream: true,
            stream_id: String::new(),
        }
    }
}

/// The fastest the statistics may be polled.
///
/// Below this the polling spends API budget faster than the numbers change,
/// and on YouTube it spends scarce daily quota to do it.
const POLL_INTERVAL_MIN_SECS: u64 = 5;

/// …and the slowest, beyond which the dashboard is not really live.
const POLL_INTERVAL_MAX_SECS: u64 = 3600;

/// The action name closest to `typed`, when one is close enough to be worth
/// suggesting.
///
/// Levenshtein distance, capped at a third of the name's length so a wild
/// guess is not offered as a correction — "did you mean" is only useful when
/// it usually is.
fn closest_action(typed: &str) -> Option<&'static str> {
    let typed = typed.trim().to_ascii_lowercase();
    crate::keys::Action::ALL
        .iter()
        .map(|action| (edit_distance(&typed, action.name()), action.name()))
        .filter(|(distance, name)| *distance <= (name.len() / 3).max(2))
        .min_by_key(|(distance, _)| *distance)
        .map(|(_, name)| name)
}

/// Levenshtein distance, two rows at a time.
fn edit_distance(a: &str, b: &str) -> usize {
    let b_chars: Vec<char> = b.chars().collect();
    let mut previous: Vec<usize> = (0..=b_chars.len()).collect();
    let mut current = vec![0usize; b_chars.len() + 1];

    for (i, ca) in a.chars().enumerate() {
        current[0] = i + 1;
        for (j, cb) in b_chars.iter().enumerate() {
            let cost = usize::from(ca != *cb);
            current[j + 1] = (previous[j] + cost)
                .min(previous[j + 1] + 1)
                .min(current[j] + 1);
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[b_chars.len()]
}

/// When to hide what must not be captured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamerMode {
    /// On whenever OBS says it is streaming or recording.
    Auto,
    Always,
    Never,
}

impl StreamerMode {
    /// Read the setting, treating anything unrecognised as the default rather
    /// than refusing to start over a typo.
    pub fn parse(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "on" | "always" | "true" => StreamerMode::Always,
            "off" | "never" | "false" => StreamerMode::Never,
            _ => StreamerMode::Auto,
        }
    }

    pub fn next(self) -> Self {
        match self {
            StreamerMode::Auto => StreamerMode::Always,
            StreamerMode::Always => StreamerMode::Never,
            StreamerMode::Never => StreamerMode::Auto,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            StreamerMode::Auto => "auto",
            StreamerMode::Always => "on",
            StreamerMode::Never => "off",
        }
    }
}

impl Config {
    /// Load `config.toml`, or return defaults if it does not exist yet.
    pub fn load() -> Result<Self> {
        let path = paths::config_file()?;
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        let mut config: Config = toml::from_str(&text).with_context(|| {
            format!(
                "parsing {}. If you have been editing it by hand, check for a missing quote or bracket.",
                path.display()
            )
        })?;
        config.source_path = Some(path);
        Ok(config)
    }

    /// Write the config back out, keeping whatever comments are already there.
    ///
    /// Uses owner-only file permissions because the file contains client
    /// secrets.
    ///
    /// The obvious implementation — serialise the struct to TOML and write that
    /// — loses every comment in the file, because comments are not fields.
    /// The starter file written on first run carries about forty lines of them
    /// explaining where to get each
    /// client id and secret, what `reuse_stream` does and what the YouTube
    /// category numbers mean, and users add their own notes. Pressing Ctrl+S
    /// once in the form used to delete all of it.
    ///
    /// So the existing file is parsed with `toml_edit`, which keeps comments and
    /// layout, and only the values are updated in place. A file that does not
    /// exist yet, or one too damaged to parse, falls back to a freshly generated
    /// file with the standard header.
    pub fn save(&self) -> Result<()> {
        // Write back to wherever this config came from, not to the default path.
        let path = match &self.source_path {
            Some(path) => path.clone(),
            None => paths::config_file()?,
        };

        let generated: toml_edit::DocumentMut = toml::to_string_pretty(self)
            .context("serialising config to TOML")?
            .parse()
            .context("re-reading the config this program just serialised")?;

        let existing = std::fs::read_to_string(&path).ok();
        let text = match existing
            .as_deref()
            .map(str::parse::<toml_edit::DocumentMut>)
        {
            Some(Ok(mut document)) => {
                merge_values(&mut document, &generated);
                document.to_string()
            }
            // No file yet, or a file we cannot parse. Either way the only
            // thing to write is a complete new one; there are no comments to
            // keep.
            //
            // The unparseable case is destructive: the user's whole file —
            // every hand-written comment in it — is about to be replaced
            // because of one stray character. It is kept beside the new one
            // rather than thrown away, so the fix is renaming a file back
            // rather than rewriting it from memory.
            other => {
                if matches!(other, Some(Err(_))) {
                    let backup = path.with_extension("toml.bak");
                    // Best effort: failing to keep a copy is not a reason to
                    // refuse to save, and the copy is a convenience rather
                    // than a guarantee.
                    if let Some(text) = existing.as_deref() {
                        let _ = paths::write_secret_file(&backup, text);
                        tracing::warn!(
                            backup = %backup.display(),
                            "the config file could not be parsed; the previous one was kept"
                        );
                    }
                }
                format!("{}\n{generated}", CONFIG_HEADER)
            }
        };

        paths::write_secret_file(&path, &text)?;
        Ok(())
    }

    /// The stream settings in force: the active named profile, or the
    /// unnamed `[preset]` when none is chosen.
    ///
    /// An `active_profile` naming a profile that is not there falls back to
    /// the default rather than refusing to start — a typo in a name should
    /// cost you the right settings, not the program.
    pub fn active_preset(&self) -> &PresetConfig {
        let name = self.active_profile.trim();
        if let Some(preset) = self.profile.get(name) {
            return preset;
        }
        if !name.is_empty() {
            tracing::warn!(
                profile = %self.active_profile,
                "unknown active profile; using the default preset"
            );
        }
        &self.preset
    }

    /// Every profile name, plus the unnamed default first.
    ///
    /// The empty string stands for `[preset]`, which is what
    /// `active_profile` uses for it.
    pub fn profile_names(&self) -> Vec<String> {
        let mut names = vec![String::new()];
        names.extend(self.profile.keys().cloned());
        names
    }

    /// Everything [`crate::engine::Engine::build`] reads out of the
    /// configuration, in a form two configurations can be compared by.
    ///
    /// The engine holds live access tokens and cached platform identities, and
    /// rebuilding it means a fresh round of token work. It only ever has to be
    /// rebuilt when something it was actually built *from* changed — the API
    /// credentials, and the two YouTube settings that decide which stream key
    /// a broadcast is bound to. Changing a theme or a keybinding cannot
    /// invalidate it.
    ///
    /// Resolved values are compared rather than raw fields, so moving a
    /// secret from the config file into its environment variable — which
    /// leaves the credential itself identical — is correctly seen as no
    /// change at all.
    /// Where each platform's client id and secret came from, for
    /// diagnostics.
    pub fn credential_sources(&self, platform: Platform) -> (CredentialSource, CredentialSource) {
        match platform {
            Platform::Twitch => (
                credential_source(&self.twitch.client_id, &self.twitch.client_id_env),
                credential_source(&self.twitch.client_secret, &self.twitch.client_secret_env),
            ),
            Platform::YouTube => (
                credential_source(&self.youtube.client_id, &self.youtube.client_id_env),
                credential_source(&self.youtube.client_secret, &self.youtube.client_secret_env),
            ),
        }
    }

    pub fn credentials_fingerprint(&self) -> Vec<String> {
        vec![
            self.twitch.client_id(),
            self.twitch.client_secret(),
            self.youtube.client_id(),
            self.youtube.client_secret(),
            self.youtube.reuse_stream.to_string(),
            self.youtube.stream_id.clone(),
        ]
    }

    /// Check that the credentials needed for the given platforms are present,
    /// and explain precisely how to obtain them if they are not.
    pub fn check_credentials(&self, platforms: &[Platform]) -> Result<()> {
        for platform in platforms {
            match platform {
                Platform::Twitch => {
                    if self.twitch.client_id().is_empty() || self.twitch.client_secret().is_empty()
                    {
                        bail!(
                            "Twitch credentials are missing.\n\n\
                             To fix this:\n  \
                             1. Go to https://dev.twitch.tv/console/apps and click \"Register Your Application\".\n  \
                             2. Set the OAuth Redirect URL to exactly: http://localhost:{port}/callback\n  \
                             3. Choose category \"Application Integration\" and click Create.\n  \
                             4. Open the app, copy the Client ID, then press \"New Secret\" and copy that too.\n  \
                             5. Enter both on the setup screen, or put them into {path} under\n     \
                                the [twitch] section — or set {id_var} and {secret_var} in\n     \
                                the environment.",
                            port = self.general.oauth_port,
                            path = paths::config_file()?.display(),
                            id_var = self.twitch.client_id_env,
                            secret_var = self.twitch.client_secret_env,
                        );
                    }
                }
                Platform::YouTube => {
                    if self.youtube.client_id().is_empty()
                        || self.youtube.client_secret().is_empty()
                    {
                        bail!(
                            "YouTube credentials are missing.\n\n\
                             To fix this:\n  \
                             1. Go to https://console.cloud.google.com/ and create a project.\n  \
                             2. Under \"APIs & Services\" enable the \"YouTube Data API v3\".\n  \
                             3. Under \"Credentials\" create an OAuth client ID of type \"Desktop app\".\n  \
                             4. Add http://localhost:{port}/callback as an authorised redirect URI.\n  \
                             5. Enter both on the setup screen, or copy them into {path} under\n     \
                                [youtube] — or set {id_var} and {secret_var} in the environment.\n\n\
                             Note: while your app is in \"Testing\" mode you must also add your own\n\
                             Google account under \"OAuth consent screen\" > \"Test users\", or Google\n\
                             will refuse the login.",
                            port = self.general.oauth_port,
                            path = paths::config_file()?.display(),
                            id_var = self.youtube.client_id_env,
                            secret_var = self.youtube.client_secret_env,
                        );
                    }
                }
            }
        }
        Ok(())
    }

    /// How often the dashboard refreshes its statistics.
    ///
    /// Clamped rather than trusted: anything under five seconds burns YouTube's
    /// daily API quota for no benefit, since neither platform updates its viewer
    /// count that often, and an hour is already far longer than anyone wants.
    pub fn poll_interval(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.clamped_poll_interval_secs())
    }

    /// The poll interval as it will actually be used, and the raw value if
    /// they differ.
    ///
    /// The clamp was silent: writing `poll_interval_secs = 1` gave you five
    /// with nothing anywhere saying the value had been ignored, so the
    /// setting looked broken rather than bounded.
    pub fn poll_interval_clamped_from(&self) -> Option<u64> {
        let raw = self.general.poll_interval_secs;
        (raw != self.clamped_poll_interval_secs()).then_some(raw)
    }

    fn clamped_poll_interval_secs(&self) -> u64 {
        self.general
            .poll_interval_secs
            .clamp(POLL_INTERVAL_MIN_SECS, POLL_INTERVAL_MAX_SECS)
    }

    /// The redirect URI registered with both providers.
    ///
    /// Both halves of the OAuth exchange must send the byte-identical string,
    /// so it is built in one place.
    ///
    /// This deliberately uses the name `localhost` rather than a literal IP:
    /// Twitch documents `localhost` as the host permitted with a plain `http`
    /// redirect, and a bare address risks being refused at registration. The
    /// consequence is that `localhost` may resolve to either `127.0.0.1` or
    /// `::1` depending on the machine, so the callback listener binds *both*
    /// loopback families — see `oauth::Loopback`.
    pub fn redirect_uri(&self) -> String {
        redirect_uri_for_port(self.general.oauth_port)
    }
}

/// The redirect URI for an arbitrary port, independent of any saved config —
/// used by the setup wizard to preview the URL from what is currently typed,
/// before it is saved.
pub fn redirect_uri_for_port(port: u16) -> String {
    format!("http://localhost:{port}/callback")
}

/// Parses and validates a hand-typed OAuth redirect port.
///
/// `0` is rejected outright: nothing can meaningfully bind it for this
/// purpose, and the registered redirect URI needs one fixed, known port.
pub fn parse_oauth_port(raw: &str) -> Result<u16, String> {
    let trimmed = raw.trim();
    // Parsed as a wider integer first so a value that is merely too big for a
    // `u16` — 70000, say — gets its own "out of range" message instead of
    // being lumped in with text that is not a number at all.
    let wide: u64 = trimmed
        .parse()
        .map_err(|_| format!("{trimmed:?} is not a port number — try something like 8017."))?;
    let port: u16 = wide
        .try_into()
        .map_err(|_| format!("{trimmed} is too big for a port — the highest is 65535."))?;
    if port == 0 {
        return Err("Port 0 cannot be used — pick a port between 1 and 65535.".to_string());
    }
    Ok(port)
}

/// Explanatory comment block written at the top of every saved config file.
/// Copy every value from `from` into `into`, leaving comments and layout alone.
///
/// Assigning into an existing key replaces the value only, so the comment
/// written above that key — which `toml_edit` stores as part of the key, not the
/// value — survives. Tables are walked recursively so `[preset]` is updated
/// key by key rather than replaced wholesale.
fn merge_values(into: &mut toml_edit::Table, from: &toml_edit::Table) {
    for (key, incoming) in from.iter() {
        match (into.get_mut(key), incoming.as_table()) {
            // Both sides are tables: recurse, so untouched keys keep their place
            // in the file and their comments.
            (Some(existing), Some(incoming_table)) if existing.is_table() => {
                if let Some(existing_table) = existing.as_table_mut() {
                    merge_values(existing_table, incoming_table);
                }
            }
            // The key is already there: overwrite just its value.
            (Some(existing), _) => *existing = incoming.clone(),
            // A key the file has never had — for example a setting added by a
            // newer version of the program. Append it.
            (None, _) => {
                into.insert(key, incoming.clone());
            }
        }
    }
}

const CONFIG_HEADER: &str = r#"# multistream-manager configuration
#
# This file has two halves:
#
#   [twitch] / [youtube]  API credentials. Set these up once. See the README for
#                         step-by-step instructions on obtaining them.
#
#   [preset]              Your default stream settings. The TUI form starts from
#                         these values, and pressing Ctrl+S in the form saves
#                         whatever you have typed back here. You can also just
#                         edit this section by hand; the interface picks it up.
#
# Anything you omit falls back to a sensible default, so a partial file is fine.
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_file_produces_usable_defaults() {
        let config: Config = toml::from_str("").expect("empty TOML should parse");
        assert_eq!(config.general.poll_interval_secs, 15);
        assert_eq!(config.general.oauth_port, 8017);
        // Reusing the YouTube stream key must default to on, otherwise every
        // broadcast would hand the user a new key and break their OBS setup.
        assert!(config.youtube.reuse_stream);
    }

    #[test]
    fn a_partial_file_keeps_defaults_for_everything_it_omits() {
        let config: Config = toml::from_str(
            r#"
            [twitch]
            client_id = "abc123"
            "#,
        )
        .expect("partial TOML should parse");

        assert_eq!(config.twitch.client_id, "abc123");
        assert!(config.twitch.client_secret.is_empty());
        assert_eq!(config.general.poll_interval_secs, 15);
    }

    #[test]
    fn preset_round_trips_through_a_plan_without_losing_anything() {
        let preset = PresetConfig {
            title: "Building a TUI in Rust".into(),
            tags: vec!["rust".into(), "tui".into()],
            twitch_category: "Software and Game Development".into(),
            twitch_category_id: "1469308723".into(),
            ..Default::default()
        };

        let plan = preset.to_plan();
        assert_eq!(
            plan.twitch_category.as_ref().map(|c| c.id.as_str()),
            Some("1469308723")
        );

        let back = PresetConfig::from_plan(&plan, &[Platform::Twitch]);
        assert_eq!(back.title, preset.title);
        assert_eq!(back.tags, preset.tags);
        assert_eq!(back.twitch_category_id, preset.twitch_category_id);
    }

    #[test]
    fn a_category_name_without_an_id_is_treated_as_unresolved() {
        let preset = PresetConfig {
            twitch_category: "Just Chatting".into(),
            twitch_category_id: String::new(),
            ..Default::default()
        };
        // The name alone is not enough to call the API, so it must not be
        // presented as an already-resolved category.
        assert!(preset.to_plan().twitch_category.is_none());
    }

    #[test]
    fn missing_credentials_produce_an_actionable_error() {
        let config = Config::default();
        let err = config
            .check_credentials(&[Platform::Twitch])
            .expect_err("empty credentials must be rejected");
        let text = err.to_string();
        assert!(text.contains("dev.twitch.tv/console/apps"));
        assert!(text.contains("localhost:8017/callback"));
    }

    #[test]
    fn the_poll_interval_is_clamped_to_a_sensible_range() {
        let mut config = Config::default();

        config.general.poll_interval_secs = 0;
        assert_eq!(config.poll_interval().as_secs(), 5);

        config.general.poll_interval_secs = 15;
        assert_eq!(config.poll_interval().as_secs(), 15);

        config.general.poll_interval_secs = 99_999;
        assert_eq!(config.poll_interval().as_secs(), 3600);
    }

    #[test]
    fn redirect_uri_follows_the_configured_port() {
        let mut config = Config::default();
        config.general.oauth_port = 9999;
        assert_eq!(config.redirect_uri(), "http://localhost:9999/callback");
    }

    #[test]
    fn parse_oauth_port_accepts_a_valid_port() {
        assert_eq!(parse_oauth_port("9000"), Ok(9000));
        assert_eq!(parse_oauth_port(" 8017 "), Ok(8017));
    }

    #[test]
    fn parse_oauth_port_rejects_zero() {
        assert!(parse_oauth_port("0").is_err());
    }

    #[test]
    fn parse_oauth_port_rejects_non_numeric_text() {
        assert!(parse_oauth_port("abc").is_err());
    }

    #[test]
    fn parse_oauth_port_rejects_out_of_range_values() {
        let error = parse_oauth_port("99999999").unwrap_err();
        assert!(
            error.contains("too big"),
            "a number that is merely too large for a port should say so, not be reported as \
             non-numeric text: {error:?}"
        );
    }

    /// A config file from before the Chat tab existed has no [chat] table;
    /// it must load with the documented defaults rather than failing.
    #[test]
    fn a_config_without_a_chat_table_gets_chat_defaults() {
        let config: Config = toml::from_str("[twitch]\nclient_id = \"x\"\n").unwrap();
        assert_eq!(config.chat.scrollback_limit, 1000);
        assert_eq!(config.chat.poll_interval_floor_ms, 1000);
        assert_eq!(config.chat.daily_quota_units, 10_000);
        assert_eq!(config.chat.quota_reserve_percent, 10);
    }

    /// An IPv6 address has colons in it, so an unbracketed one would be read
    /// as a host with the port in the middle of it.
    #[test]
    fn the_obs_url_brackets_an_ipv6_address() {
        let config = ObsConfig {
            host: "::1".into(),
            port: 4455,
            ..Default::default()
        };
        assert_eq!(config.url(), "ws://[::1]:4455");

        // Already bracketed, and left alone.
        let bracketed = ObsConfig {
            host: "[::1]".into(),
            ..Default::default()
        };
        assert_eq!(bracketed.url(), "ws://[::1]:4455");
    }

    #[test]
    fn the_obs_url_uses_the_host_and_port_as_written() {
        let config = ObsConfig {
            host: "obs.local".into(),
            port: 9999,
            ..Default::default()
        };
        assert_eq!(config.url(), "ws://obs.local:9999");
    }

    /// An empty host is a half-edited config, not a request to connect to
    /// nowhere.
    #[test]
    fn an_empty_obs_host_falls_back_to_this_machine() {
        let config = ObsConfig {
            host: "   ".into(),
            // Named explicitly and left unset, so the test does not depend on
            // whether whoever is running it happens to have the real variable
            // exported.
            host_env: "MSM_TEST_OBS_HOST_NOT_SET".into(),
            port_env: "MSM_TEST_OBS_PORT_NOT_SET".into(),
            ..Default::default()
        };
        assert_eq!(config.url(), "ws://127.0.0.1:4455");
    }

    /// The address can come from the environment too, so one dotfiles
    /// repository can point at a laptop's own OBS one day and a studio
    /// machine's the next without editing anything.
    #[test]
    fn the_obs_address_can_come_from_the_environment() {
        let config = ObsConfig {
            host: String::new(),
            port: 0,
            host_env: "MSM_TEST_OBS_HOST".into(),
            port_env: "MSM_TEST_OBS_PORT".into(),
            ..Default::default()
        };
        with_envs(
            &[
                ("MSM_TEST_OBS_HOST", Some("studio.local")),
                ("MSM_TEST_OBS_PORT", Some("4466")),
            ],
            || assert_eq!(config.url(), "ws://studio.local:4466"),
        );
    }

    /// Same precedence as every credential: naming a value explicitly beats
    /// inheriting one.
    #[test]
    fn an_obs_address_in_the_file_wins_over_the_environment() {
        let config = ObsConfig {
            host: "written.local".into(),
            port: 1234,
            host_env: "MSM_TEST_OBS_HOST".into(),
            port_env: "MSM_TEST_OBS_PORT".into(),
            ..Default::default()
        };
        with_envs(
            &[
                ("MSM_TEST_OBS_HOST", Some("studio.local")),
                ("MSM_TEST_OBS_PORT", Some("4466")),
            ],
            || assert_eq!(config.url(), "ws://written.local:1234"),
        );
    }

    /// A port that is not a port must not stop the interface starting. OBS's
    /// own default is a far better answer than refusing to run.
    #[test]
    fn an_unusable_obs_port_falls_back_rather_than_failing() {
        for value in ["not-a-number", "0", "70000", "-1"] {
            let config = ObsConfig {
                host: "localhost".into(),
                port: 0,
                host_env: "MSM_TEST_OBS_HOST_NOT_SET".into(),
                port_env: "MSM_TEST_OBS_PORT".into(),
                ..Default::default()
            };
            with_env("MSM_TEST_OBS_PORT", Some(value), || {
                assert_eq!(config.url(), "ws://localhost:4455", "{value}");
            });
        }
    }

    /// An unrecognised `animations` value must not stop the interface
    /// drawing; the default mode is a far better answer than refusing to
    /// run.
    #[test]
    fn an_unusable_animation_mode_falls_back_rather_than_failing() {
        let appearance = AppearanceConfig {
            animations: "reduce".into(),
            ..Default::default()
        };
        assert_eq!(appearance.animation_mode(), crate::anim::Mode::default());
    }

    /// An `active_profile` naming a profile that is not there must not stop
    /// the interface starting; the unnamed `[preset]` is a far better
    /// answer than refusing to run.
    #[test]
    fn an_unknown_active_profile_falls_back_rather_than_failing() {
        let config = Config {
            active_profile: "missing".into(),
            ..Default::default()
        };
        assert!(std::ptr::eq(config.active_preset(), &config.preset));
    }

    /// An IPv6 literal has to survive coming from the environment as well as
    /// from the file, brackets and all.
    #[test]
    fn an_ipv6_host_from_the_environment_is_bracketed() {
        let config = ObsConfig {
            host: String::new(),
            port: 4455,
            host_env: "MSM_TEST_OBS_HOST".into(),
            ..Default::default()
        };
        with_env("MSM_TEST_OBS_HOST", Some("::1"), || {
            assert_eq!(config.url(), "ws://[::1]:4455");
        });
    }

    #[test]
    fn a_password_in_the_file_is_used_as_written() {
        let config = ObsConfig {
            password: "  hunter2  ".into(),
            ..Default::default()
        };
        assert_eq!(config.password().as_deref(), Some("hunter2"));
    }

    #[test]
    fn no_password_anywhere_means_none() {
        let config = ObsConfig {
            password: String::new(),
            password_env: "MSM_TEST_OBS_PASSWORD_THAT_IS_NOT_SET".into(),
            ..Default::default()
        };
        assert!(config.password().is_none());
    }

    /// An alias and a shortcut for the same scene have to end up on the same
    /// entry, or one of them would silently do nothing.
    #[test]
    fn aliases_and_shortcuts_are_collected_per_target() {
        let mut config = ObsConfig::default();
        config
            .scene_aliases
            .insert("brb".into(), "Be Right Back".into());
        config
            .scene_shortcuts
            .insert("3".into(), "Be Right Back".into());
        config
            .scene_aliases
            .insert("cam".into(), "Main Camera".into());

        let labels = config.scene_labels();
        assert_eq!(
            labels.get("Be Right Back"),
            Some(&(Some("brb".to_string()), Some("3".to_string())))
        );
        assert_eq!(
            labels.get("Main Camera"),
            Some(&(Some("cam".to_string()), None))
        );
    }

    /// Two aliases for one scene is a mistake in the file. Which one wins
    /// must at least be stable, rather than changing between runs.
    #[test]
    fn a_duplicate_alias_resolves_the_same_way_every_time() {
        let mut config = ObsConfig::default();
        config.scene_aliases.insert("zzz".into(), "Main".into());
        config.scene_aliases.insert("aaa".into(), "Main".into());

        let first = config.scene_labels();
        for _ in 0..20 {
            assert_eq!(config.scene_labels(), first);
        }
        // Sorted order, so the answer does not depend on hashing.
        assert_eq!(first["Main"].0.as_deref(), Some("aaa"));
    }

    #[test]
    fn obs_is_on_by_default_and_points_at_the_usual_place() {
        let config = ObsConfig::default();
        assert!(config.enabled);
        assert_eq!(config.url(), "ws://127.0.0.1:4455");
    }

    /// Guards the credential environment variables, which are process-wide
    /// state that more than one test here reads and writes.
    static CREDENTIAL_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Run `body` with `name` set to `value`, then put it back.
    fn with_env<R>(name: &str, value: Option<&str>, body: impl FnOnce() -> R) -> R {
        with_envs(&[(name, value)], body)
    }

    /// The same for several variables at once.
    ///
    /// Taking them together rather than nesting calls is deliberate: the lock
    /// below is not reentrant, so a nested call on one thread would wait for
    /// a lock that same thread already holds. That hangs the test run rather
    /// than failing it, which is a great deal harder to work out from the
    /// outside than an assertion would be.
    fn with_envs<R>(pairs: &[(&str, Option<&str>)], body: impl FnOnce() -> R) -> R {
        let _guard = CREDENTIAL_ENV_LOCK
            .lock()
            .unwrap_or_else(|err| err.into_inner());

        let previous: Vec<(&str, Option<std::ffi::OsString>)> = pairs
            .iter()
            .map(|(name, _)| (*name, std::env::var_os(name)))
            .collect();

        // SAFETY: the lock above makes this the only thread touching these
        // variables, and every one is restored before it is released.
        unsafe {
            for (name, value) in pairs {
                match value {
                    Some(value) => std::env::set_var(name, value),
                    None => std::env::remove_var(name),
                }
            }
        }

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(body));

        unsafe {
            for (name, value) in previous {
                match value {
                    Some(value) => std::env::set_var(name, value),
                    None => std::env::remove_var(name),
                }
            }
        }

        match result {
            Ok(value) => value,
            Err(panic) => std::panic::resume_unwind(panic),
        }
    }

    #[test]
    fn a_credential_can_come_from_the_environment() {
        let config = TwitchConfig {
            client_secret_env: "MSM_TEST_TWITCH_SECRET".to_string(),
            ..Default::default()
        };
        with_env(
            "MSM_TEST_TWITCH_SECRET",
            Some("from-the-environment"),
            || {
                assert_eq!(config.client_secret(), "from-the-environment");
            },
        );
    }

    /// Naming a value explicitly should beat inheriting one, so a credential
    /// written in the file wins over the same one in the environment.
    #[test]
    fn a_credential_in_the_file_wins_over_the_environment() {
        let config = TwitchConfig {
            client_secret: "from-the-file".to_string(),
            client_secret_env: "MSM_TEST_TWITCH_SECRET".to_string(),
            ..Default::default()
        };
        with_env(
            "MSM_TEST_TWITCH_SECRET",
            Some("from-the-environment"),
            || {
                assert_eq!(config.client_secret(), "from-the-file");
            },
        );
    }

    /// An unfilled shell variable looks exactly like this, and treating it as
    /// a real empty credential would fail later in a way nobody could read.
    #[test]
    fn an_empty_environment_variable_counts_as_unset() {
        let config = YouTubeConfig {
            client_id_env: "MSM_TEST_YT_ID".to_string(),
            ..Default::default()
        };
        with_env("MSM_TEST_YT_ID", Some("   "), || {
            assert!(config.client_id().is_empty());
        });
    }

    #[test]
    fn credentials_from_the_environment_satisfy_the_check() {
        let mut config = Config {
            twitch: TwitchConfig {
                client_id_env: "MSM_TEST_TWITCH_ID".to_string(),
                client_secret_env: "MSM_TEST_TWITCH_SECRET".to_string(),
                ..Default::default()
            },
            ..Default::default()
        };
        config.general.oauth_port = 8017;

        // Both variables in one call. Nesting `with_env` inside itself would
        // wait for a lock the same thread already holds, which hangs the run
        // rather than failing it.
        with_envs(
            &[
                ("MSM_TEST_TWITCH_ID", Some("an-id")),
                ("MSM_TEST_TWITCH_SECRET", Some("a-secret")),
            ],
            || {
                assert!(config.check_credentials(&[Platform::Twitch]).is_ok());
            },
        );
    }

    /// The message somebody sees when a credential is missing has to name
    /// every way of supplying one, or the environment route is a secret.
    #[test]
    fn the_missing_credential_message_names_the_environment_variables() {
        let config = Config::default();
        let error = config
            .check_credentials(&[Platform::Twitch])
            .expect_err("no credentials are configured");
        let message = format!("{error}");
        assert!(message.contains("MSM_TWITCH_CLIENT_ID"), "got {message}");
        assert!(message.contains("setup screen"), "got {message}");
    }

    /// `[keys.global] "q" = ""` looks like it turns off quitting, and does
    /// nothing at all: `q` is bound per tab rather than globally, so the
    /// removal hits nothing and the key carries on working.
    #[test]
    fn unbinding_a_chord_that_is_not_there_says_where_it_is() {
        let mut keys = KeysConfig::default();
        keys.global.insert("q".into(), String::new());
        let (_, problems) = keys.keymap();

        let complaint = problems
            .iter()
            .find(|problem| problem.contains("did nothing"))
            .unwrap_or_else(|| panic!("expected a complaint, got {problems:?}"));
        assert!(complaint.contains("[keys.chat]"), "{complaint}");
    }

    /// A typo in an action name is one letter from a real one often enough
    /// that offering the real one is worth doing — `Action::ALL` is right
    /// there.
    #[test]
    fn a_mistyped_action_name_suggests_the_real_one() {
        let mut keys = KeysConfig::default();
        keys.global.insert("<F9>".into(), "obs.strem".into());
        let (_, problems) = keys.keymap();

        let complaint = problems
            .iter()
            .find(|problem| problem.contains("no action called"))
            .unwrap_or_else(|| panic!("expected a complaint, got {problems:?}"));
        assert!(complaint.contains("obs.stream"), "{complaint}");
    }

    /// Binding a prefix makes every longer binding under it unreachable —
    /// `resolve_key` checks for an exact match before it checks for a prefix.
    /// The whole OBS group vanishing is worth a word.
    #[test]
    fn a_binding_that_buries_a_whole_group_is_reported() {
        let mut keys = KeysConfig::default();
        keys.global.insert("<Leader>o".into(), "app.quit".into());
        let (_, problems) = keys.keymap();

        assert!(
            problems
                .iter()
                .any(|problem| problem.contains("unreachable")),
            "expected a complaint, got {problems:?}"
        );
    }

    /// A `[keys.config]` binding to something that tab does not handle
    /// parses, stores and is silently ignored — that tab owns its plain keys
    /// and resolves only its own actions.
    #[test]
    fn a_config_binding_to_an_unhandled_action_is_reported() {
        let mut keys = KeysConfig::default();
        keys.config.insert("z".into(), "obs.stream".into());
        let (_, problems) = keys.keymap();

        assert!(
            problems
                .iter()
                .any(|problem| problem.contains("does not handle")),
            "expected a complaint, got {problems:?}"
        );

        // …and its own actions are fine.
        let mut fine = KeysConfig::default();
        fine.config.insert("z".into(), "config.activate".into());
        let (_, problems) = fine.keymap();
        assert!(
            !problems
                .iter()
                .any(|problem| problem.contains("does not handle")),
            "a config.* action is handled: {problems:?}"
        );
    }

    /// The documented footgun with credentials is a shell-profile variable a
    /// desktop launcher cannot see. "The client id is present" is no help
    /// with that — it *is* present, in the terminal — so the source is what
    /// the self-check has to report.
    #[test]
    fn the_credential_source_says_where_the_value_came_from() {
        let _scratch = crate::paths::test_support::ScratchConfigDir::new("credential-source");
        let mut config = Config::default();

        assert_eq!(
            config.credential_sources(Platform::Twitch).0,
            CredentialSource::Missing
        );

        config.twitch.client_id = "written-here".into();
        assert_eq!(
            config.credential_sources(Platform::Twitch).0,
            CredentialSource::File
        );

        config.twitch.client_id = String::new();
        std::env::set_var("MSM_TEST_TWITCH_ID", "from-the-environment");
        config.twitch.client_id_env = "MSM_TEST_TWITCH_ID".into();
        assert_eq!(
            config.credential_sources(Platform::Twitch).0,
            CredentialSource::Environment("MSM_TEST_TWITCH_ID".into())
        );

        // An environment variable that exists but is empty counts as unset,
        // matching how the value itself resolves.
        std::env::set_var("MSM_TEST_TWITCH_ID", "   ");
        assert_eq!(
            config.credential_sources(Platform::Twitch).0,
            CredentialSource::Missing
        );
        std::env::remove_var("MSM_TEST_TWITCH_ID");
    }

    /// The engine is expensive to rebuild and holds live tokens, so a save
    /// that changed only presentation must not invalidate it.
    #[test]
    fn the_credentials_fingerprint_ignores_everything_the_engine_never_reads() {
        let mut config = Config::default();
        config.twitch.client_id = "abc".into();
        let before = config.credentials_fingerprint();

        config.appearance.theme = "some-other-theme".into();
        config.general.poll_interval_secs = 42;
        assert_eq!(
            before,
            config.credentials_fingerprint(),
            "a theme or poll-interval change must not force a rebuild"
        );

        config.twitch.client_id = "def".into();
        assert_ne!(
            before,
            config.credentials_fingerprint(),
            "a changed client id must force a rebuild"
        );
    }

    /// Characterization tests for the comment-preserving save.
    ///
    /// `save` is the one place where hand-written configuration and generated
    /// configuration meet, and it is the reason a user can keep their own
    /// comments in `config.toml` and still press Ctrl+S in the form. Nothing
    /// about that behaviour was pinned by a test, so these lock down what it
    /// does today before anything else in this file is moved around.
    mod saving {
        use super::*;

        /// Build a config pointed at a scratch file that already contains
        /// `existing`, run `change` over it, save, and hand back what landed
        /// on disk.
        fn save_over(name: &str, existing: &str, change: impl FnOnce(&mut Config)) -> String {
            let dir = std::env::temp_dir().join(format!("msm-save-{name}-{}", std::process::id()));
            std::fs::create_dir_all(&dir).expect("creating the scratch directory");
            let path = dir.join("config.toml");
            std::fs::write(&path, existing).expect("writing the starting file");

            // Parsed from the file first, exactly as production does: `save`
            // writes every field of the struct, so a config that was never
            // loaded would overwrite the file with defaults. An unparseable
            // file falls back to defaults, which is what `Config::load` does.
            let mut config: Config = toml::from_str(existing).unwrap_or_default();
            config.source_path = Some(path.clone());
            change(&mut config);
            config.save().expect("saving must work");

            let text = std::fs::read_to_string(&path).expect("reading the saved file");
            let _ = std::fs::remove_dir_all(&dir);
            text
        }

        #[test]
        fn comments_and_unknown_keys_survive_a_save() {
            let saved = save_over(
                "comments",
                "\
# my own note about credentials
[twitch]
# the id from the developer console
client_id = \"old-id\"
something_this_version_never_heard_of = 7

[preset]
# what I usually stream
title = \"old title\"
",
                |config| {
                    config.twitch.client_id = "new-id".into();
                    config.preset.title = "new title".into();
                },
            );

            assert!(
                saved.contains("# my own note about credentials"),
                "a top-level comment must survive: {saved}"
            );
            assert!(
                saved.contains("# the id from the developer console"),
                "a comment above a changed key must survive: {saved}"
            );
            assert!(
                saved.contains("# what I usually stream"),
                "a comment inside a table must survive: {saved}"
            );
            assert!(
                saved.contains("something_this_version_never_heard_of = 7"),
                "a key this version does not know must not be pruned: {saved}"
            );
            assert!(
                saved.contains("client_id = \"new-id\""),
                "the changed value must be written: {saved}"
            );
            assert!(
                !saved.contains("old-id"),
                "the old value must be gone: {saved}"
            );
        }

        #[test]
        fn a_setting_the_file_has_never_had_is_appended() {
            let saved = save_over("append", "[twitch]\nclient_id = \"id\"\n", |config| {
                config.twitch.client_secret = "brand-new".into();
            });

            assert!(
                saved.contains("brand-new"),
                "a key absent from the file must be added: {saved}"
            );
            assert!(saved.contains("client_id = \"id\""), "and the old one kept");
        }

        /// The documented fallback: a file that cannot be parsed has nothing
        /// worth merging into, so a complete new one is written. This is
        /// destructive — it is recorded here so that it is a decision rather
        /// than a surprise.
        /// The documented fallback: a file that cannot be parsed has nothing
        /// worth merging into, so a complete new one is written. That is
        /// destructive — every hand-written comment goes — so the previous
        /// file is kept beside it rather than thrown away, and the fix is
        /// renaming a file back rather than rewriting it from memory.
        #[test]
        fn an_unparseable_file_is_replaced_but_kept_as_a_backup() {
            let dir = std::env::temp_dir().join(format!("msm-save-damaged-{}", std::process::id()));
            std::fs::create_dir_all(&dir).expect("creating the scratch directory");
            let path = dir.join("config.toml");
            let damaged = "# my careful notes\nthis is not = = toml at [[ all";
            std::fs::write(&path, damaged).expect("writing the starting file");

            let config = Config {
                source_path: Some(path.clone()),
                ..Default::default()
            };
            config.save().expect("saving must work");

            let saved = std::fs::read_to_string(&path).expect("reading the saved file");
            assert!(
                saved.starts_with(CONFIG_HEADER),
                "the replacement must carry the explanatory header: {saved}"
            );
            assert!(
                !saved.contains("not = = toml"),
                "the damaged content is not preserved in place: {saved}"
            );

            let backup = std::fs::read_to_string(path.with_extension("toml.bak"))
                .expect("the damaged file has to be kept");
            assert_eq!(
                backup, damaged,
                "the backup must be the file exactly as it was"
            );

            let _ = std::fs::remove_dir_all(&dir);
        }

        /// The clamp is right; applying it silently was not. A value outside
        /// the range made the setting look broken rather than bounded.
        #[test]
        fn a_poll_interval_outside_the_range_is_clamped_and_reported() {
            let mut config = Config::default();
            assert_eq!(config.poll_interval_clamped_from(), None);

            config.general.poll_interval_secs = 1;
            assert_eq!(config.poll_interval().as_secs(), 5);
            assert_eq!(config.poll_interval_clamped_from(), Some(1));

            config.general.poll_interval_secs = 100_000;
            assert_eq!(config.poll_interval().as_secs(), 3600);
            assert_eq!(config.poll_interval_clamped_from(), Some(100_000));
        }
    }
}
