//! The requests this program sends to OBS.
//!
//! Each function builds one message. They are kept together, and kept as data
//! rather than as calls, so the whole surface this program uses of OBS is one
//! list — nineteen requests, all of them here.
//!
//! Request ids are allocated from a counter shared by every connection. The
//! id only has to be unique among the requests currently in flight, which a
//! monotonic counter guarantees far more simply than a random value would.

use serde_json::json;
use std::sync::atomic::{AtomicU64, Ordering};

use super::protocol::Request;

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

fn request(request_type: &str) -> Request {
    Request {
        request_type: request_type.to_string(),
        request_id: next_id(),
        request_data: None,
    }
}

fn request_with(request_type: &str, data: serde_json::Value) -> Request {
    Request {
        request_type: request_type.to_string(),
        request_id: next_id(),
        request_data: Some(data),
    }
}

fn next_id() -> String {
    format!("msm-{:06}", NEXT_ID.fetch_add(1, Ordering::Relaxed))
}

// --- reading state ---------------------------------------------------------

/// OBS Studio and obs-websocket version strings.
pub fn get_version() -> Request {
    request("GetVersion")
}

/// Every scene, and which one is live.
pub fn get_scene_list() -> Request {
    request("GetSceneList")
}

/// Every input. Only the audio ones are of interest here, but OBS has no
/// request for "audio inputs" alone — the kind is read off each input.
pub fn get_input_list() -> Request {
    request("GetInputList")
}

pub fn get_input_mute(input: &str) -> Request {
    request_with("GetInputMute", json!({ "inputName": input }))
}

pub fn get_input_volume(input: &str) -> Request {
    request_with("GetInputVolume", json!({ "inputName": input }))
}

/// Whether streaming is active, for how long, and at what bitrate.
pub fn get_stream_status() -> Request {
    request("GetStreamStatus")
}

pub fn get_record_status() -> Request {
    request("GetRecordStatus")
}

/// Processor, memory, disk and frame statistics.
pub fn get_stats() -> Request {
    request("GetStats")
}

pub fn get_profile_list() -> Request {
    request("GetProfileList")
}

pub fn get_scene_collection_list() -> Request {
    request("GetSceneCollectionList")
}

// --- changing things -------------------------------------------------------

pub fn set_current_program_scene(scene: &str) -> Request {
    request_with("SetCurrentProgramScene", json!({ "sceneName": scene }))
}

pub fn set_input_mute(input: &str, muted: bool) -> Request {
    request_with(
        "SetInputMute",
        json!({ "inputName": input, "inputMuted": muted }),
    )
}

pub fn toggle_input_mute(input: &str) -> Request {
    request_with("ToggleInputMute", json!({ "inputName": input }))
}

/// Set an input's volume as a multiplier, where 1.0 is unity gain.
///
/// OBS also accepts decibels, but a multiplier is what the percentage the
/// interface shows converts to directly, and mixing the two units in one
/// program is a good way to eventually set a level to the wrong thing.
pub fn set_input_volume(input: &str, multiplier: f64) -> Request {
    request_with(
        "SetInputVolume",
        json!({ "inputName": input, "inputVolumeMul": multiplier }),
    )
}

pub fn set_current_profile(profile: &str) -> Request {
    request_with("SetCurrentProfile", json!({ "profileName": profile }))
}

pub fn set_current_scene_collection(collection: &str) -> Request {
    request_with(
        "SetCurrentSceneCollection",
        json!({ "sceneCollectionName": collection }),
    )
}

/// Start streaming if stopped, stop it if started.
///
/// A toggle rather than separate start and stop requests, because the state
/// this program holds can always be a moment out of date — somebody may have
/// pressed the button in OBS itself — and a toggle cannot act on a stale
/// belief about which way round things are.
pub fn toggle_stream() -> Request {
    request("ToggleStream")
}

pub fn toggle_record() -> Request {
    request("ToggleRecord")
}

/// Pause or resume an in-progress recording.
pub fn toggle_record_pause() -> Request {
    request("ToggleRecordPause")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_request_gets_its_own_id() {
        let first = get_version().request_id;
        let second = get_version().request_id;
        assert_ne!(first, second);
    }

    /// The request type and the field names inside it are the protocol —
    /// OBS matches on them exactly, and a typo produces a request it will
    /// refuse rather than one it misunderstands.
    #[test]
    fn requests_carry_the_names_obs_expects() {
        let scene = set_current_program_scene("Starting soon");
        assert_eq!(scene.request_type, "SetCurrentProgramScene");
        assert_eq!(
            scene.request_data.expect("data")["sceneName"],
            "Starting soon"
        );

        let mute = set_input_mute("Mic/Aux", true);
        assert_eq!(mute.request_type, "SetInputMute");
        let data = mute.request_data.expect("data");
        assert_eq!(data["inputName"], "Mic/Aux");
        assert_eq!(data["inputMuted"], true);

        let volume = set_input_volume("Mic/Aux", 0.5);
        let data = volume.request_data.expect("data");
        assert_eq!(data["inputVolumeMul"], 0.5);

        let collection = set_current_scene_collection("Podcast");
        let data = collection.request_data.expect("data");
        assert_eq!(data["sceneCollectionName"], "Podcast");
    }

    /// A request with nothing to say must not send an empty object: OBS
    /// treats a present-but-empty `requestData` differently from an absent
    /// one for some request types.
    #[test]
    fn a_request_with_no_arguments_carries_no_data_field() {
        let json = serde_json::to_string(&get_stats()).expect("serialisable");
        assert!(!json.contains("requestData"), "got {json}");
    }

    /// A scene called `"` or containing a newline is legal in OBS. Building
    /// the message through serde rather than by formatting strings is what
    /// keeps that from producing malformed JSON.
    #[test]
    fn awkward_names_survive_being_put_in_a_request() {
        let awkward = "scene \"quoted\" \n with \\ backslash";
        let request = set_current_program_scene(awkward);
        let json = serde_json::to_string(&request).expect("serialisable");
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
        assert_eq!(parsed["requestData"]["sceneName"], awkward);
    }
}
