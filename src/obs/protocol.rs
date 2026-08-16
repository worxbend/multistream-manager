//! The obs-websocket 5 wire format.
//!
//! OBS Studio does not expose a REST API. It has a WebSocket server (built in
//! since OBS 28, and a plugin before that) which speaks a small protocol of
//! its own: every message is a JSON object with an `op` — an opcode saying
//! what kind of message it is — and a `d` holding that kind's payload.
//!
//! Only six of the opcodes matter here:
//!
//! | op | name | direction | meaning |
//! |----|------|-----------|---------|
//! | 0 | Hello | from OBS | "here I am, and here is my authentication challenge" |
//! | 1 | Identify | to OBS | "here is my answer, and the events I want" |
//! | 2 | Identified | from OBS | "you are in" |
//! | 5 | Event | from OBS | something changed |
//! | 6 | Request | to OBS | do this |
//! | 7 | RequestResponse | from OBS | the result of a request |
//!
//! The two batch opcodes (8 and 9) are not used: nothing here needs to send
//! several requests as one atomic unit, and doing so would make a failure
//! harder to report than sending them one at a time.

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const OPCODE_HELLO: u8 = 0;
pub const OPCODE_IDENTIFY: u8 = 1;
pub const OPCODE_IDENTIFIED: u8 = 2;
pub const OPCODE_EVENT: u8 = 5;
pub const OPCODE_REQUEST: u8 = 6;
pub const OPCODE_REQUEST_RESPONSE: u8 = 7;

/// The version of the remote-procedure-call protocol this speaks.
///
/// obs-websocket negotiates this: it answers with the version it settled on,
/// which lets a newer OBS keep talking to an older client. Version 1 is the
/// only one obs-websocket 5 has ever had.
pub const RPC_VERSION: u32 = 1;

/// Which categories of event to subscribe to.
///
/// This is a bitmask, and asking for less means OBS sends less. The
/// categories deliberately left out are the expensive ones nothing here
/// displays: scene *item* changes (every source moving inside a scene),
/// transitions, filters, and the media playback events.
pub mod subscriptions {
    /// Connection-level events.
    pub const GENERAL: u32 = 1 << 0;
    /// Profile and scene-collection changes.
    pub const CONFIG: u32 = 1 << 1;
    /// Scene list and current-scene changes.
    pub const SCENES: u32 = 1 << 2;
    /// Audio input creation, removal, mute and volume.
    pub const INPUTS: u32 = 1 << 3;
    /// Streaming and recording starting and stopping.
    pub const OUTPUTS: u32 = 1 << 6;

    /// Everything the OBS pane needs, and nothing else.
    ///
    /// Note what is *not* here: `INPUT_VOLUME_METERS` (bit 16) sends the
    /// live audio level of every input around sixty times a second. That is
    /// the right trade for a dedicated OBS meter bridge; for a pane that
    /// shares a terminal with two chats it would mean waking the interface
    /// sixty times a second forever, to move a bar most people are not
    /// looking at.
    pub const DEFAULT: u32 = GENERAL | CONFIG | SCENES | INPUTS | OUTPUTS;
}

/// Any message on the wire.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub op: u8,
    pub d: Value,
}

/// `Hello` (opcode 0) — the first thing OBS sends.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hello {
    #[serde(rename = "obsWebSocketVersion")]
    pub obs_websocket_version: String,
    #[serde(rename = "rpcVersion")]
    pub rpc_version: u32,
    /// Present only when OBS has authentication turned on.
    pub authentication: Option<HelloAuthentication>,
}

/// The challenge OBS issues when it wants a password.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HelloAuthentication {
    pub challenge: String,
    pub salt: String,
}

/// `Identify` (opcode 1) — the answer to `Hello`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Identify {
    #[serde(rename = "rpcVersion")]
    pub rpc_version: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub authentication: Option<String>,
    #[serde(rename = "eventSubscriptions", skip_serializing_if = "Option::is_none")]
    pub event_subscriptions: Option<u32>,
}

/// `Request` (opcode 6) — ask OBS to do something.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    #[serde(rename = "requestType")]
    pub request_type: String,
    /// A value chosen by this program and echoed back in the response, which
    /// is what lets several requests be in flight at once without their
    /// answers being confused for one another.
    #[serde(rename = "requestId")]
    pub request_id: String,
    #[serde(rename = "requestData", skip_serializing_if = "Option::is_none")]
    pub request_data: Option<Value>,
}

/// `RequestResponse` (opcode 7) — how a request turned out.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestResponse {
    #[serde(rename = "requestType")]
    pub request_type: String,
    #[serde(rename = "requestId")]
    pub request_id: String,
    #[serde(rename = "requestStatus")]
    pub request_status: RequestStatus,
    #[serde(rename = "responseData")]
    pub response_data: Option<Value>,
}

/// Whether a request worked, and why not if it did not.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestStatus {
    pub result: bool,
    pub code: u32,
    pub comment: Option<String>,
}

impl RequestStatus {
    /// A human-readable reason for a failure.
    ///
    /// OBS sends a numeric code and usually a comment. The comment is the
    /// useful half — "No source was found by the name of 'Mic'" beats
    /// "code 600" — so it is preferred, with the code as a fallback for the
    /// rare response that omits it.
    pub fn describe(&self) -> String {
        match self.comment.as_deref() {
            Some(comment) if !comment.trim().is_empty() => comment.trim().to_string(),
            _ => format!("OBS refused the request (code {})", self.code),
        }
    }
}

/// `Event` (opcode 5) — something changed in OBS.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    #[serde(rename = "eventType")]
    pub event_type: String,
    #[serde(rename = "eventData")]
    pub event_data: Option<Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The field names are the protocol. A rename that looks harmless here
    /// would produce a message OBS silently ignores, so they are pinned.
    #[test]
    fn identify_serialises_with_the_names_obs_expects() {
        let identify = Identify {
            rpc_version: RPC_VERSION,
            authentication: Some("hash".to_string()),
            event_subscriptions: Some(subscriptions::DEFAULT),
        };
        let json = serde_json::to_value(&identify).expect("serialisable");
        assert_eq!(json["rpcVersion"], 1);
        assert_eq!(json["authentication"], "hash");
        assert_eq!(json["eventSubscriptions"], subscriptions::DEFAULT);
    }

    /// With no password configured the field must be absent rather than null:
    /// obs-websocket rejects a null authentication where it expects none.
    #[test]
    fn identify_omits_authentication_when_there_is_none() {
        let identify = Identify {
            rpc_version: RPC_VERSION,
            authentication: None,
            event_subscriptions: None,
        };
        let json = serde_json::to_string(&identify).expect("serialisable");
        assert_eq!(json, r#"{"rpcVersion":1}"#);
    }

    #[test]
    fn a_hello_with_authentication_is_parsed() {
        let raw = serde_json::json!({
            "obsWebSocketVersion": "5.5.4",
            "rpcVersion": 1,
            "authentication": { "challenge": "c", "salt": "s" }
        });
        let hello: Hello = serde_json::from_value(raw).expect("a valid Hello");
        assert_eq!(hello.obs_websocket_version, "5.5.4");
        let auth = hello.authentication.expect("authentication present");
        assert_eq!(auth.challenge, "c");
        assert_eq!(auth.salt, "s");
    }

    /// OBS with authentication turned off sends no `authentication` at all.
    #[test]
    fn a_hello_without_authentication_is_parsed() {
        let raw = serde_json::json!({ "obsWebSocketVersion": "5.5.4", "rpcVersion": 1 });
        let hello: Hello = serde_json::from_value(raw).expect("a valid Hello");
        assert!(hello.authentication.is_none());
    }

    #[test]
    fn a_failure_prefers_the_comment_to_the_code() {
        let with_comment = RequestStatus {
            result: false,
            code: 600,
            comment: Some("No source was found by the name of 'Mic'".to_string()),
        };
        assert_eq!(
            with_comment.describe(),
            "No source was found by the name of 'Mic'"
        );

        let bare = RequestStatus {
            result: false,
            code: 600,
            comment: None,
        };
        assert!(bare.describe().contains("600"));

        // An empty comment is no better than none.
        let empty = RequestStatus {
            result: false,
            code: 601,
            comment: Some("   ".to_string()),
        };
        assert!(empty.describe().contains("601"));
    }

    /// The volume-meter category is expensive and deliberately not requested;
    /// if it ever creeps into the default, the interface would start waking
    /// sixty times a second.
    #[test]
    fn the_default_subscriptions_exclude_the_high_frequency_meters() {
        const INPUT_VOLUME_METERS: u32 = 1 << 16;
        assert_eq!(subscriptions::DEFAULT & INPUT_VOLUME_METERS, 0);
        // And do include the five categories the pane actually shows.
        for category in [
            subscriptions::GENERAL,
            subscriptions::CONFIG,
            subscriptions::SCENES,
            subscriptions::INPUTS,
            subscriptions::OUTPUTS,
        ] {
            assert_eq!(subscriptions::DEFAULT & category, category);
        }
    }
}
