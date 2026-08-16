//! What OBS says has changed.
//!
//! OBS pushes an event whenever something happens, whether this program asked
//! for it or somebody pressed a button in OBS's own window. Applying them is
//! what lets the pane show the truth without polling: switch a scene on a
//! stream deck and the pane follows within milliseconds, having sent nothing.
//!
//! Only the events the pane can actually show are modelled. The rest arrive,
//! are recognised as uninteresting, and are dropped — [`Event::from_raw`]
//! returning `None` is the normal case, not a failure.

use serde_json::Value;

use super::state::{ObsState, Scene};

/// A change in OBS worth acting on.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    SceneChanged {
        scene: String,
    },
    /// The scene list itself changed — one was added, removed or renamed. The
    /// new list is not in the event, so this is a prompt to go and ask.
    SceneListChanged,
    InputCreated {
        input: String,
    },
    InputRemoved {
        input: String,
    },
    InputMuteChanged {
        input: String,
        muted: bool,
    },
    InputVolumeChanged {
        input: String,
        mul: f64,
        db: f64,
    },
    StreamStateChanged {
        active: bool,
    },
    RecordStateChanged {
        active: bool,
        paused: bool,
    },
    ProfileChanged {
        profile: String,
    },
    ProfileListChanged,
    SceneCollectionChanged {
        collection: String,
    },
    SceneCollectionListChanged,
}

impl Event {
    /// Turn a raw obs-websocket event into one of ours, or `None` for the
    /// many kinds nothing here displays.
    pub fn from_raw(event_type: &str, data: Option<&Value>) -> Option<Self> {
        let string = |key: &str| -> Option<String> { data?.get(key)?.as_str().map(str::to_string) };
        let boolean = |key: &str| -> Option<bool> { data?.get(key)?.as_bool() };
        let number = |key: &str| -> Option<f64> { data?.get(key)?.as_f64() };

        match event_type {
            "CurrentProgramSceneChanged" => Some(Event::SceneChanged {
                scene: string("sceneName")?,
            }),
            "SceneListChanged" | "SceneCreated" | "SceneRemoved" | "SceneNameChanged" => {
                Some(Event::SceneListChanged)
            }
            "InputCreated" => Some(Event::InputCreated {
                input: string("inputName")?,
            }),
            "InputRemoved" => Some(Event::InputRemoved {
                input: string("inputName")?,
            }),
            "InputMuteStateChanged" => Some(Event::InputMuteChanged {
                input: string("inputName")?,
                muted: boolean("inputMuted")?,
            }),
            "InputVolumeChanged" => Some(Event::InputVolumeChanged {
                input: string("inputName")?,
                mul: number("inputVolumeMul")?,
                db: number("inputVolumeDb").unwrap_or(f64::NEG_INFINITY),
            }),
            // OBS reports the state of an output as it moves through
            // starting → started → stopping → stopped. Only the settled
            // states are acted on: treating "starting" as live would show a
            // stream as up a second before it is, and treating "stopping" as
            // down would hide the last second of one.
            "StreamStateChanged" => match string("outputState")?.as_str() {
                "OBS_WEBSOCKET_OUTPUT_STARTED" => Some(Event::StreamStateChanged { active: true }),
                "OBS_WEBSOCKET_OUTPUT_STOPPED" => Some(Event::StreamStateChanged { active: false }),
                _ => None,
            },
            "RecordStateChanged" => match string("outputState")?.as_str() {
                "OBS_WEBSOCKET_OUTPUT_STARTED" => Some(Event::RecordStateChanged {
                    active: true,
                    paused: false,
                }),
                "OBS_WEBSOCKET_OUTPUT_PAUSED" => Some(Event::RecordStateChanged {
                    active: true,
                    paused: true,
                }),
                "OBS_WEBSOCKET_OUTPUT_RESUMED" => Some(Event::RecordStateChanged {
                    active: true,
                    paused: false,
                }),
                "OBS_WEBSOCKET_OUTPUT_STOPPED" => Some(Event::RecordStateChanged {
                    active: false,
                    paused: false,
                }),
                _ => None,
            },
            "CurrentProfileChanged" => Some(Event::ProfileChanged {
                profile: string("profileName")?,
            }),
            "ProfileListChanged" => Some(Event::ProfileListChanged),
            "CurrentSceneCollectionChanged" => Some(Event::SceneCollectionChanged {
                collection: string("sceneCollectionName")?,
            }),
            "SceneCollectionListChanged" => Some(Event::SceneCollectionListChanged),
            _ => None,
        }
    }

    /// Whether acting on this event means going back to OBS for a fresh list.
    ///
    /// Some events carry the new value and some only say that a list changed;
    /// the second kind cannot be applied without asking.
    pub fn needs_refresh(&self) -> bool {
        matches!(
            self,
            Event::SceneListChanged
                | Event::InputCreated { .. }
                | Event::InputRemoved { .. }
                | Event::ProfileListChanged
                | Event::SceneCollectionListChanged
                // A scene collection is a whole different set of scenes and
                // inputs, so switching one invalidates everything.
                | Event::SceneCollectionChanged { .. }
        )
    }

    /// Update `state` with what this event says.
    ///
    /// Events that only announce that a list changed do nothing here — they
    /// are handled by [`Self::needs_refresh`] instead.
    pub fn apply(&self, state: &mut ObsState) {
        match self {
            Event::SceneChanged { scene } => {
                state.current_scene = Some(scene.clone());
                // A scene switched from OBS's own window may be one this
                // program has never seen, if the list changed at the same
                // time. Adding it keeps the pane honest until the refresh
                // arrives.
                if !state.scenes.iter().any(|known| known.name == *scene) {
                    state.scenes.push(Scene {
                        name: scene.clone(),
                        alias: None,
                        shortcut: None,
                    });
                }
            }
            Event::InputMuteChanged { input, muted } => {
                if let Some(found) = state.audio.iter_mut().find(|item| item.name == *input) {
                    found.muted = Some(*muted);
                }
            }
            Event::InputVolumeChanged { input, mul, db } => {
                if let Some(found) = state.audio.iter_mut().find(|item| item.name == *input) {
                    found.volume_mul = Some(*mul);
                    found.volume_db = Some(*db);
                }
            }
            Event::StreamStateChanged { active } => {
                state.streaming = *active;
                if !*active {
                    state.stream_duration = None;
                    state.stream_bitrate_kbps = None;
                }
            }
            Event::RecordStateChanged { active, paused } => {
                state.recording = *active;
                state.record_paused = *paused;
                if !*active {
                    state.record_duration = None;
                }
            }
            Event::ProfileChanged { profile } => {
                state.current_profile = Some(profile.clone());
            }
            Event::SceneCollectionChanged { collection } => {
                state.current_scene_collection = Some(collection.clone());
            }
            Event::SceneListChanged
            | Event::InputCreated { .. }
            | Event::InputRemoved { .. }
            | Event::ProfileListChanged
            | Event::SceneCollectionListChanged => {}
        }
    }

    /// A short line for the activity log, or `None` for the events that are
    /// too frequent or too dull to be worth one.
    ///
    /// Volume changes are deliberately silent: dragging a fader in OBS sends
    /// a stream of them, and a log full of "Mic 47%, Mic 46%, Mic 45%" would
    /// bury everything else.
    pub fn describe(&self) -> Option<String> {
        match self {
            Event::SceneChanged { scene } => Some(format!("OBS scene: {scene}")),
            Event::StreamStateChanged { active } => Some(
                if *active {
                    "OBS started streaming"
                } else {
                    "OBS stopped streaming"
                }
                .to_string(),
            ),
            Event::RecordStateChanged { active, paused } => Some(
                match (active, paused) {
                    (true, true) => "OBS paused recording",
                    (true, false) => "OBS started recording",
                    (false, _) => "OBS stopped recording",
                }
                .to_string(),
            ),
            Event::ProfileChanged { profile } => Some(format!("OBS profile: {profile}")),
            Event::SceneCollectionChanged { collection } => {
                Some(format!("OBS scene collection: {collection}"))
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn parse(event_type: &str, data: serde_json::Value) -> Option<Event> {
        Event::from_raw(event_type, Some(&data))
    }

    #[test]
    fn a_scene_change_is_recognised() {
        assert_eq!(
            parse("CurrentProgramSceneChanged", json!({ "sceneName": "Main" })),
            Some(Event::SceneChanged {
                scene: "Main".to_string()
            })
        );
    }

    /// OBS moves an output through four states. Acting on the transitional
    /// two would show a stream as live a second before it is.
    #[test]
    fn only_the_settled_output_states_are_acted_on() {
        let started = parse(
            "StreamStateChanged",
            json!({ "outputActive": true, "outputState": "OBS_WEBSOCKET_OUTPUT_STARTED" }),
        );
        assert_eq!(started, Some(Event::StreamStateChanged { active: true }));

        let stopped = parse(
            "StreamStateChanged",
            json!({ "outputState": "OBS_WEBSOCKET_OUTPUT_STOPPED" }),
        );
        assert_eq!(stopped, Some(Event::StreamStateChanged { active: false }));

        for transitional in [
            "OBS_WEBSOCKET_OUTPUT_STARTING",
            "OBS_WEBSOCKET_OUTPUT_STOPPING",
        ] {
            assert_eq!(
                parse("StreamStateChanged", json!({ "outputState": transitional })),
                None,
                "{transitional} should not be acted on"
            );
        }
    }

    #[test]
    fn pausing_a_recording_keeps_it_active() {
        assert_eq!(
            parse(
                "RecordStateChanged",
                json!({ "outputState": "OBS_WEBSOCKET_OUTPUT_PAUSED" })
            ),
            Some(Event::RecordStateChanged {
                active: true,
                paused: true
            })
        );
        assert_eq!(
            parse(
                "RecordStateChanged",
                json!({ "outputState": "OBS_WEBSOCKET_OUTPUT_RESUMED" })
            ),
            Some(Event::RecordStateChanged {
                active: true,
                paused: false
            })
        );
    }

    /// Most of what OBS sends is of no interest here, and that has to be the
    /// quiet path rather than an error.
    #[test]
    fn an_uninteresting_event_is_dropped_rather_than_failing() {
        assert_eq!(parse("SceneItemEnableStateChanged", json!({})), None);
        assert_eq!(parse("MediaInputPlaybackStarted", json!({})), None);
        assert_eq!(Event::from_raw("CurrentProgramSceneChanged", None), None);
    }

    /// A malformed event — the right type with the wrong shape — must be
    /// dropped rather than panicking or inventing a value.
    #[test]
    fn an_event_missing_its_fields_is_dropped() {
        assert_eq!(parse("CurrentProgramSceneChanged", json!({})), None);
        assert_eq!(
            parse("InputMuteStateChanged", json!({ "inputName": "Mic" })),
            None
        );
        assert_eq!(
            parse(
                "InputMuteStateChanged",
                json!({ "inputName": "Mic", "inputMuted": "yes" })
            ),
            None,
            "a string where a boolean belongs is not a mute state"
        );
    }

    #[test]
    fn applying_a_mute_event_updates_only_that_input() {
        let mut state = ObsState {
            audio: vec![
                crate::obs::state::AudioInput {
                    name: "Mic".into(),
                    alias: None,
                    shortcut: None,
                    kind: None,
                    muted: Some(false),
                    volume_mul: Some(1.0),
                    volume_db: Some(0.0),
                },
                crate::obs::state::AudioInput {
                    name: "Desktop".into(),
                    alias: None,
                    shortcut: None,
                    kind: None,
                    muted: Some(false),
                    volume_mul: Some(1.0),
                    volume_db: Some(0.0),
                },
            ],
            ..Default::default()
        };

        Event::InputMuteChanged {
            input: "Mic".into(),
            muted: true,
        }
        .apply(&mut state);

        assert_eq!(state.audio[0].muted, Some(true));
        assert_eq!(
            state.audio[1].muted,
            Some(false),
            "the other input is untouched"
        );
    }

    /// An event about something this program has never heard of must not
    /// create it out of nowhere or panic.
    #[test]
    fn an_event_about_an_unknown_input_changes_nothing() {
        let mut state = ObsState::default();
        Event::InputMuteChanged {
            input: "Ghost".into(),
            muted: true,
        }
        .apply(&mut state);
        assert!(state.audio.is_empty());
    }

    /// A scene switched from OBS's own window may be one this program has
    /// not seen yet. The pane has to show it as live regardless.
    #[test]
    fn switching_to_an_unknown_scene_adds_it_rather_than_showing_nothing() {
        let mut state = ObsState::default();
        Event::SceneChanged {
            scene: "Brand New".into(),
        }
        .apply(&mut state);
        assert_eq!(state.current_scene.as_deref(), Some("Brand New"));
        assert_eq!(state.scenes.len(), 1);

        // And applying it twice must not add it twice.
        Event::SceneChanged {
            scene: "Brand New".into(),
        }
        .apply(&mut state);
        assert_eq!(state.scenes.len(), 1);
    }

    #[test]
    fn stopping_a_stream_clears_the_figures_that_only_apply_while_it_runs() {
        let mut state = ObsState {
            streaming: true,
            stream_duration: Some(std::time::Duration::from_secs(60)),
            stream_bitrate_kbps: Some(6000.0),
            ..Default::default()
        };
        Event::StreamStateChanged { active: false }.apply(&mut state);
        assert!(!state.streaming);
        assert!(state.stream_duration.is_none());
        assert!(state.stream_bitrate_kbps.is_none());
    }

    /// A scene collection is a whole different set of scenes and inputs, so
    /// switching one invalidates everything the pane is showing.
    #[test]
    fn the_events_that_need_a_fresh_look_say_so() {
        assert!(Event::SceneListChanged.needs_refresh());
        assert!(Event::SceneCollectionChanged {
            collection: "Podcast".into()
        }
        .needs_refresh());
        assert!(Event::InputCreated {
            input: "Mic".into()
        }
        .needs_refresh());

        assert!(!Event::SceneChanged {
            scene: "Main".into()
        }
        .needs_refresh());
        assert!(!Event::StreamStateChanged { active: true }.needs_refresh());
    }

    /// Dragging a fader in OBS sends a stream of volume events. Logging each
    /// would bury everything else in the activity log.
    #[test]
    fn volume_changes_are_not_worth_a_log_line() {
        assert!(Event::InputVolumeChanged {
            input: "Mic".into(),
            mul: 0.5,
            db: -6.0
        }
        .describe()
        .is_none());

        assert!(Event::SceneChanged {
            scene: "Main".into()
        }
        .describe()
        .is_some());
    }
}
