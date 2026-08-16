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
    /// Without this, `--config ~/streams/coding.toml` would load from that file
    /// but save to the default one — quietly writing the preset, and a copy of
    /// the client secrets, into the wrong place.
    #[serde(skip)]
    pub source_path: Option<std::path::PathBuf>,
    pub twitch: TwitchConfig,
    pub youtube: YouTubeConfig,
    pub general: GeneralConfig,
    /// Chat pane settings.
    pub chat: ChatConfig,
    /// Colours, motion, and the optional interface extras.
    pub appearance: AppearanceConfig,
    /// Controlling OBS Studio from the OBS tab.
    pub obs: ObsConfig,
    /// The saved stream settings the form starts from.
    pub preset: PresetConfig,
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
    /// Run `msm profile list` to see every name. An unrecognised name
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

    /// Show pop-up notifications for things that happen while you are looking
    /// elsewhere.
    pub toasts: bool,

    /// How long a pop-up notification stays on screen, in seconds.
    pub toast_seconds: u64,

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
        crate::anim::Mode::parse(&self.animations).unwrap_or_default()
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

    /// The port from OBS's WebSocket Server Settings.
    pub port: u16,

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
    /// `brb = "Be Right Back"` lets `msm obs scene brb` do the obvious thing,
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
            // obs-websocket's own default port.
            port: 4455,
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
    /// The WebSocket address to connect to.
    pub fn url(&self) -> String {
        let host = self.host.trim();
        let host = if host.is_empty() { "127.0.0.1" } else { host };
        // An IPv6 literal has to be bracketed in a URL, or the colons in the
        // address are read as the port separator.
        if host.contains(':') && !host.starts_with('[') {
            format!("ws://[{host}]:{}", self.port)
        } else {
            format!("ws://{host}:{}", self.port)
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
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
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
}

/// Google/YouTube OAuth client credentials, from
/// <https://console.cloud.google.com/apis/credentials>. Create an OAuth client
/// of type **Desktop app**.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct YouTubeConfig {
    pub client_id: String,
    pub client_secret: String,

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

/// Settings for the Chat tab.
///
/// The defaults mirror the two reference implementations this feature was
/// ported from (`twi` for Twitch, `yc` for YouTube), except `scrollback_limit`
/// which defaults to 1000 messages per chat.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ChatConfig {
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
    /// directory (`msm paths` prints where that lives).
    pub chat_log_dir: String,
    /// Rotate a log file once it reaches this many bytes.
    pub chat_log_max_bytes: u64,
    /// Keep at most this many rotated files; older ones are pruned.
    pub chat_log_max_files: u64,
}

impl Default for ChatConfig {
    fn default() -> Self {
        Self {
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
            platforms: platforms.to_vec(),
        }
    }
}

impl Default for YouTubeConfig {
    fn default() -> Self {
        Self {
            client_id: String::new(),
            client_secret: String::new(),
            // Defaulting to `true` is the whole point — see the field docs.
            reuse_stream: true,
            stream_id: String::new(),
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

    /// Load from an explicit path instead of the default location. Used by
    /// `--config`, which lets you keep one preset file per kind of stream.
    pub fn load_from(path: &std::path::Path) -> Result<Self> {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let mut config: Config =
            toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        config.source_path = Some(path.to_path_buf());
        Ok(config)
    }

    /// Write the config back out, keeping whatever comments are already there.
    ///
    /// Uses owner-only file permissions because the file contains client
    /// secrets.
    ///
    /// The obvious implementation — serialise the struct to TOML and write that
    /// — loses every comment in the file, because comments are not fields.
    /// `msm init` writes about forty lines of them explaining where to get each
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
            // No file yet, or a file we cannot parse. Either way the only thing
            // to write is a complete new one; there are no comments to keep.
            _ => format!("{}\n{generated}", CONFIG_HEADER),
        };

        paths::write_secret_file(&path, &text)?;
        Ok(())
    }

    /// Check that the credentials needed for the given platforms are present,
    /// and explain precisely how to obtain them if they are not.
    pub fn check_credentials(&self, platforms: &[Platform]) -> Result<()> {
        for platform in platforms {
            match platform {
                Platform::Twitch => {
                    if self.twitch.client_id.is_empty() || self.twitch.client_secret.is_empty() {
                        bail!(
                            "Twitch credentials are missing.\n\n\
                             To fix this:\n  \
                             1. Go to https://dev.twitch.tv/console/apps and click \"Register Your Application\".\n  \
                             2. Set the OAuth Redirect URL to exactly: http://localhost:{port}/callback\n  \
                             3. Choose category \"Application Integration\" and click Create.\n  \
                             4. Open the app, copy the Client ID, then press \"New Secret\" and copy that too.\n  \
                             5. Put both into {path} under the [twitch] section.",
                            port = self.general.oauth_port,
                            path = paths::config_file()?.display(),
                        );
                    }
                }
                Platform::YouTube => {
                    if self.youtube.client_id.is_empty() || self.youtube.client_secret.is_empty() {
                        bail!(
                            "YouTube credentials are missing.\n\n\
                             To fix this:\n  \
                             1. Go to https://console.cloud.google.com/ and create a project.\n  \
                             2. Under \"APIs & Services\" enable the \"YouTube Data API v3\".\n  \
                             3. Under \"Credentials\" create an OAuth client ID of type \"Desktop app\".\n  \
                             4. Add http://localhost:{port}/callback as an authorised redirect URI.\n  \
                             5. Copy the client ID and client secret into {path} under [youtube].\n\n\
                             Note: while your app is in \"Testing\" mode you must also add your own\n\
                             Google account under \"OAuth consent screen\" > \"Test users\", or Google\n\
                             will refuse the login.",
                            port = self.general.oauth_port,
                            path = paths::config_file()?.display(),
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
        std::time::Duration::from_secs(self.general.poll_interval_secs.clamp(5, 3600))
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
        format!("http://localhost:{}/callback", self.general.oauth_port)
    }
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
#                         edit this section by hand and run `msm go --yes`.
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

    /// Pressing Ctrl+S in the form used to replace the whole file with a
    /// serialisation of the struct, deleting the setup guidance `msm init`
    /// wrote and any notes the user had added.
    #[test]
    fn saving_keeps_the_comments_already_in_the_file() {
        let dir = std::env::temp_dir().join(format!("msm-config-save-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");

        std::fs::write(
            &path,
            "# my own note about this file\n\
             [twitch]\n\
             # get this from https://dev.twitch.tv/console/apps\n\
             client_id = \"abc\"\n\
             client_secret = \"shh\"\n\
             \n\
             [preset]\n\
             # what the stream is usually called\n\
             title = \"old title\"\n",
        )
        .unwrap();

        let mut config = Config::load_from(&path).unwrap();
        config.preset.title = "new title".into();
        config.save().unwrap();

        let saved = std::fs::read_to_string(&path).unwrap();
        assert!(saved.contains("# my own note about this file"), "{saved}");
        assert!(
            saved.contains("# get this from https://dev.twitch.tv/console/apps"),
            "{saved}"
        );
        assert!(
            saved.contains("# what the stream is usually called"),
            "{saved}"
        );

        // The value really was updated, and the untouched ones are intact.
        assert!(saved.contains("new title"), "{saved}");
        assert!(!saved.contains("old title"), "{saved}");
        assert_eq!(Config::load_from(&path).unwrap().twitch.client_id, "abc");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// With no file to start from there are no comments to keep, so a complete
    /// one is written — header included.
    #[test]
    fn saving_with_no_existing_file_writes_the_explanatory_header() {
        let dir = std::env::temp_dir().join(format!("msm-config-new-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");

        let config = Config {
            source_path: Some(path.clone()),
            ..Default::default()
        };
        config.save().unwrap();

        let saved = std::fs::read_to_string(&path).unwrap();
        assert!(
            saved.starts_with("# multistream-manager configuration"),
            "{saved}"
        );
        Config::load_from(&path).expect("what was written must parse back");

        let _ = std::fs::remove_dir_all(&dir);
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
            ..Default::default()
        };
        assert_eq!(config.url(), "ws://127.0.0.1:4455");
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
}
