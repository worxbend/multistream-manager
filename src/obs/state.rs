//! What OBS is currently doing, as this program understands it.
//!
//! This is a *cache*, and treating it as one matters. OBS is driven by other
//! things too — its own window, a stream deck, a hotkey — so anything here can
//! be a moment out of date. Two consequences run through the design: actions
//! are expressed as toggles wherever OBS offers one, so they cannot act on a
//! stale belief about which way round something is; and every event OBS sends
//! is applied, so the cache catches up on its own rather than waiting for the
//! next poll.

use std::time::Duration;

/// A scene, and whether it is the one on air.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scene {
    pub name: String,
    /// A short name from the config, shown in the OBS pane rather
    /// than typing whatever the scene is really called.
    pub alias: Option<String>,
    /// A single key that switches to this scene from the OBS pane.
    pub shortcut: Option<String>,
}

/// An audio input, with whatever is currently known about it.
///
/// The mute state and volume are `Option` because they arrive in separate
/// requests after the input list itself: an input is known to exist before it
/// is known to be muted, and drawing "unmuted" in that gap would be a
/// confident lie. The interface draws a dash instead.
#[derive(Debug, Clone, PartialEq)]
pub struct AudioInput {
    pub name: String,
    pub alias: Option<String>,
    pub shortcut: Option<String>,
    /// The OBS input kind, e.g. `pulse_input_capture`.
    pub kind: Option<String>,
    pub muted: Option<bool>,
    /// The volume as a multiplier, where 1.0 is unity gain.
    pub volume_mul: Option<f64>,
    /// The same volume in decibels, as OBS reports it. Kept as sent rather
    /// than derived, so what is shown is what OBS believes.
    pub volume_db: Option<f64>,
}

impl AudioInput {
    /// The volume as a percentage of unity gain, for display and for the
    /// keys that nudge it.
    ///
    /// OBS allows a multiplier above 1.0 (amplification), so this is not
    /// clamped to 100 — a source boosted to 120% should say so.
    pub fn volume_percent(&self) -> Option<u32> {
        self.volume_mul
            .map(|multiplier| (multiplier * 100.0).round().max(0.0) as u32)
    }

    /// What to call this input in the interface: the alias when there is one,
    /// since that is the name the person chose.
    pub fn label(&self) -> &str {
        self.alias.as_deref().unwrap_or(&self.name)
    }
}

impl Scene {
    pub fn label(&self) -> &str {
        self.alias.as_deref().unwrap_or(&self.name)
    }
}

/// OBS's own performance figures.
///
/// These are the numbers that say whether a stream is in trouble. Skipped
/// frames in particular are the difference between "the machine is busy" and
/// "viewers are seeing stutter".
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Stats {
    pub cpu_usage_percent: f64,
    pub memory_usage_mb: f64,
    pub available_disk_space_mb: f64,
    pub active_fps: f64,
    pub average_frame_render_time_ms: f64,
    /// Frames the renderer could not draw in time.
    pub render_skipped_frames: u64,
    pub render_total_frames: u64,
    /// Frames dropped on the way out — usually the network, not the machine.
    pub output_skipped_frames: u64,
    pub output_total_frames: u64,
}

impl Stats {
    /// The share of frames lost at the encoder, as a percentage.
    pub fn render_skipped_percent(&self) -> f64 {
        percentage(self.render_skipped_frames, self.render_total_frames)
    }

    /// The share of frames lost on the way out, as a percentage. This is the
    /// one that usually means "the upload is not keeping up".
    pub fn output_skipped_percent(&self) -> f64 {
        percentage(self.output_skipped_frames, self.output_total_frames)
    }
}

fn percentage(part: u64, whole: u64) -> f64 {
    if whole == 0 {
        return 0.0;
    }
    part as f64 / whole as f64 * 100.0
}

/// How the connection to OBS stands.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Connection {
    /// Not configured, or deliberately not connected.
    #[default]
    Idle,
    Connecting,
    Connected,
    /// The connection dropped and another attempt is coming.
    Reconnecting,
    /// Something went wrong that retrying will not fix — a wrong password,
    /// most often. The reason is kept so it can be shown rather than logged
    /// and forgotten.
    Failed(String),
}

impl Connection {
    pub fn label(&self) -> &str {
        match self {
            Connection::Idle => "not connected",
            Connection::Connecting => "connecting",
            Connection::Connected => "connected",
            Connection::Reconnecting => "reconnecting",
            Connection::Failed(_) => "failed",
        }
    }
}

/// Everything known about OBS at this moment.
#[derive(Debug, Clone, Default)]
pub struct ObsState {
    pub connection: Connection,
    pub obs_version: Option<String>,
    pub websocket_version: Option<String>,

    pub scenes: Vec<Scene>,
    pub current_scene: Option<String>,

    pub audio: Vec<AudioInput>,

    pub profiles: Vec<String>,
    pub current_profile: Option<String>,
    pub scene_collections: Vec<String>,
    pub current_scene_collection: Option<String>,

    pub streaming: bool,
    pub recording: bool,
    pub record_paused: bool,
    pub stream_duration: Option<Duration>,
    pub record_duration: Option<Duration>,
    /// The outgoing bitrate in kilobits per second, when streaming.
    pub stream_bitrate_kbps: Option<f64>,

    pub stats: Option<Stats>,
}

impl ObsState {
    pub fn is_connected(&self) -> bool {
        self.connection == Connection::Connected
    }

    /// Find a scene by its OBS name, its alias, or its shortcut.
    pub fn find_scene(&self, target: &str) -> Option<&Scene> {
        find(target, &self.scenes)
    }

    /// Find an audio input by its OBS name, its alias, or its shortcut.
    pub fn find_audio(&self, target: &str) -> Option<&AudioInput> {
        find(target, &self.audio)
    }

    /// Forget everything that only makes sense while connected.
    ///
    /// Called when the connection drops. Leaving a scene list on screen after
    /// OBS has gone would invite pressing a key that cannot work, and showing
    /// "streaming" for a program that is no longer being talked to is worse
    /// than showing nothing.
    pub fn clear_live_data(&mut self) {
        self.scenes.clear();
        self.current_scene = None;
        self.audio.clear();
        self.profiles.clear();
        self.current_profile = None;
        self.scene_collections.clear();
        self.current_scene_collection = None;
        self.streaming = false;
        self.recording = false;
        self.record_paused = false;
        self.stream_duration = None;
        self.record_duration = None;
        self.stream_bitrate_kbps = None;
        self.stats = None;
        self.obs_version = None;
        self.websocket_version = None;
    }
}

/// Something that can be referred to by three different names.
///
/// Scenes and audio inputs are looked up in exactly the same way, so the
/// lookup is written once against this rather than twice against each.
trait Addressable {
    fn obs_name(&self) -> &str;
    fn alias(&self) -> Option<&str>;
    fn shortcut(&self) -> Option<&str>;
}

impl Addressable for Scene {
    fn obs_name(&self) -> &str {
        &self.name
    }
    fn alias(&self) -> Option<&str> {
        self.alias.as_deref()
    }
    fn shortcut(&self) -> Option<&str> {
        self.shortcut.as_deref()
    }
}

impl Addressable for AudioInput {
    fn obs_name(&self) -> &str {
        &self.name
    }
    fn alias(&self) -> Option<&str> {
        self.alias.as_deref()
    }
    fn shortcut(&self) -> Option<&str> {
        self.shortcut.as_deref()
    }
}

/// Resolve a target against a list, trying the most deliberate match first.
///
/// The order is what makes this predictable: an exact shortcut beats an exact
/// alias, which beats an exact name, and only then are case-insensitive
/// matches tried in the same order. Without a fixed order, adding a scene
/// could silently change what an existing alias resolves to.
fn find<'a, T: Addressable>(target: &str, items: &'a [T]) -> Option<&'a T> {
    let target = target.trim();
    if target.is_empty() {
        return None;
    }

    /// One attempt: which of the three names to compare, and whether the
    /// comparison is exact or case-insensitive.
    type Matcher<T> = (fn(&T) -> Option<&str>, bool);

    let matchers: [Matcher<T>; 6] = [
        (T::shortcut, true),
        (T::alias, true),
        (|item| Some(item.obs_name()), true),
        (T::shortcut, false),
        (T::alias, false),
        (|item| Some(item.obs_name()), false),
    ];

    matchers.into_iter().find_map(|(pick, exact)| {
        items.iter().find(|item| {
            pick(item).is_some_and(|value| {
                if exact {
                    value == target
                } else {
                    value.eq_ignore_ascii_case(target)
                }
            })
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scene(name: &str, alias: Option<&str>, shortcut: Option<&str>) -> Scene {
        Scene {
            name: name.to_string(),
            alias: alias.map(str::to_string),
            shortcut: shortcut.map(str::to_string),
        }
    }

    fn state() -> ObsState {
        ObsState {
            scenes: vec![
                scene("Starting Soon", Some("intro"), Some("1")),
                scene("Main Camera", Some("cam"), Some("2")),
                scene("Be Right Back", Some("brb"), None),
            ],
            audio: vec![AudioInput {
                name: "Mic/Aux".to_string(),
                alias: Some("mic".to_string()),
                shortcut: Some("m".to_string()),
                kind: None,
                muted: Some(false),
                volume_mul: Some(1.0),
                volume_db: Some(0.0),
            }],
            ..Default::default()
        }
    }

    #[test]
    fn a_scene_is_found_by_name_alias_or_shortcut() {
        let state = state();
        assert_eq!(
            state.find_scene("Be Right Back").map(|s| s.name.as_str()),
            Some("Be Right Back")
        );
        assert_eq!(
            state.find_scene("brb").map(|s| s.name.as_str()),
            Some("Be Right Back")
        );
        assert_eq!(
            state.find_scene("2").map(|s| s.name.as_str()),
            Some("Main Camera")
        );
    }

    #[test]
    fn finding_ignores_case_and_surrounding_space() {
        let state = state();
        assert_eq!(
            state.find_scene("  BRB  ").map(|s| s.name.as_str()),
            Some("Be Right Back")
        );
        assert_eq!(
            state.find_scene("main camera").map(|s| s.name.as_str()),
            Some("Main Camera")
        );
    }

    /// The order has to be fixed, or adding a scene could change what an
    /// existing alias means. An exact shortcut wins over anything else.
    #[test]
    fn an_exact_shortcut_wins_over_a_name_that_looks_like_one() {
        let state = ObsState {
            scenes: vec![
                // A scene literally called "1"...
                scene("1", None, None),
                // ...and another whose shortcut is "1".
                scene("Camera", None, Some("1")),
            ],
            ..Default::default()
        };
        assert_eq!(
            state.find_scene("1").map(|s| s.name.as_str()),
            Some("Camera")
        );
    }

    #[test]
    fn an_exact_match_wins_over_one_that_differs_in_case() {
        let state = ObsState {
            scenes: vec![scene("CAMERA", None, None), scene("camera", None, None)],
            ..Default::default()
        };
        assert_eq!(
            state.find_scene("camera").map(|s| s.name.as_str()),
            Some("camera")
        );
    }

    #[test]
    fn nothing_matches_an_empty_or_unknown_target() {
        let state = state();
        assert!(state.find_scene("").is_none());
        assert!(state.find_scene("   ").is_none());
        assert!(state.find_scene("no such scene").is_none());
    }

    #[test]
    fn audio_is_found_the_same_way() {
        let state = state();
        assert_eq!(
            state.find_audio("mic").map(|i| i.name.as_str()),
            Some("Mic/Aux")
        );
        assert_eq!(
            state.find_audio("m").map(|i| i.name.as_str()),
            Some("Mic/Aux")
        );
        assert_eq!(
            state.find_audio("Mic/Aux").map(|i| i.name.as_str()),
            Some("Mic/Aux")
        );
    }

    #[test]
    fn a_label_prefers_the_alias_someone_chose() {
        let state = state();
        assert_eq!(state.scenes[0].label(), "intro");
        assert_eq!(state.scenes[2].label(), "brb");
        assert_eq!(state.audio[0].label(), "mic");

        let unnamed = scene("Raw Name", None, None);
        assert_eq!(unnamed.label(), "Raw Name");
    }

    /// A boosted source should say 120%, not be quietly clamped to 100.
    #[test]
    fn volume_is_a_percentage_of_unity_and_is_not_capped() {
        let input = |mul: f64| AudioInput {
            name: "x".into(),
            alias: None,
            shortcut: None,
            kind: None,
            muted: None,
            volume_mul: Some(mul),
            volume_db: None,
        };
        assert_eq!(input(1.0).volume_percent(), Some(100));
        assert_eq!(input(0.0).volume_percent(), Some(0));
        assert_eq!(input(0.5).volume_percent(), Some(50));
        assert_eq!(input(1.2).volume_percent(), Some(120));
    }

    #[test]
    fn an_unknown_volume_stays_unknown_rather_than_reading_as_zero() {
        let input = AudioInput {
            name: "x".into(),
            alias: None,
            shortcut: None,
            kind: None,
            muted: None,
            volume_mul: None,
            volume_db: None,
        };
        assert_eq!(input.volume_percent(), None);
    }

    #[test]
    fn skipped_frame_percentages_handle_a_stream_that_has_not_started() {
        let stats = Stats::default();
        assert_eq!(stats.render_skipped_percent(), 0.0);
        assert_eq!(stats.output_skipped_percent(), 0.0);

        let busy = Stats {
            render_skipped_frames: 5,
            render_total_frames: 200,
            output_skipped_frames: 3,
            output_total_frames: 100,
            ..Default::default()
        };
        assert!((busy.render_skipped_percent() - 2.5).abs() < f64::EPSILON);
        assert!((busy.output_skipped_percent() - 3.0).abs() < f64::EPSILON);
    }

    /// Losing the connection must not leave stale state on screen — showing
    /// "streaming" for a program that is no longer being talked to is worse
    /// than showing nothing.
    #[test]
    fn dropping_the_connection_clears_everything_it_was_showing() {
        let mut state = state();
        state.streaming = true;
        state.recording = true;
        state.current_scene = Some("Main Camera".into());
        state.stats = Some(Stats::default());

        state.clear_live_data();

        assert!(state.scenes.is_empty());
        assert!(state.audio.is_empty());
        assert!(state.current_scene.is_none());
        assert!(!state.streaming);
        assert!(!state.recording);
        assert!(state.stats.is_none());
    }
}
