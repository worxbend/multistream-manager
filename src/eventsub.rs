//! Twitch EventSub: the stream events that never touch chat.
//!
//! ## Why this exists
//!
//! Most of what happens to a Twitch stream arrives as chat traffic. A raid, a
//! subscription, a gift drop and a cheer are all lines on the IRC connection
//! the Chat tab already holds, which is why they reach the desktop
//! notifications without anything like this module.
//!
//! Follows do not. Neither do channel-point redemptions, hype trains, polls or
//! predictions. They are not chat, they have never been chat, and the only way
//! to hear about them is to ask Twitch to tell you — which is what EventSub
//! is. Somebody following you is one of the two or three things a streamer
//! most wants to notice while it is happening, and this program could not see
//! it at all.
//!
//! ## How it works
//!
//! EventSub over WebSocket, which is the transport meant for exactly this
//! case: a program on somebody's own machine, with no public HTTPS endpoint
//! for Twitch to post to.
//!
//! 1. Connect to `wss://eventsub.wss.twitch.tv/ws`.
//! 2. Twitch sends `session_welcome` carrying a session id.
//! 3. For each event type, a normal Helix request creates a subscription that
//!    names that session as its transport. This is the part that catches
//!    people out: the subscriptions are made over HTTP, not over the socket.
//! 4. Notifications arrive on the socket until it closes.
//!
//! Twitch also cycles the connection every so often on purpose. It sends
//! `session_reconnect` with a URL, and the old socket keeps working until the
//! new one has taken over — so a scheduled reconnect loses nothing, provided
//! the program actually implements it, which is why [`Wake::Reconnect`] exists
//! rather than treating it as a disconnection.
//!
//! ## What it deliberately does not subscribe to
//!
//! Subscriptions, gifts, cheers and raids, all of which EventSub can deliver
//! and all of which already arrive over IRC. Subscribing here as well would
//! mean two notifications for one event, which is worse than none: the second
//! one teaches you to distrust the first.

use std::time::Duration;

use futures_util::{SinkExt as _, StreamExt as _};
use serde::Deserialize;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message as WsMessage;

use crate::chat::source::TokenProvider;

/// Twitch's own default WebSocket endpoint.
const DEFAULT_URL: &str = "wss://eventsub.wss.twitch.tv/ws";
/// Where subscriptions are created. Over HTTP, not over the socket.
const SUBSCRIPTIONS_URL: &str = "https://api.twitch.tv/helix/eventsub/subscriptions";

/// How long to wait for the socket to open and for the welcome to arrive.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// The first reconnection delay, and the ceiling it backs off to.
///
/// Same shape as the OBS pane's reconnection, and for the same reason: a
/// machine with no network must not retry ten times a second forever, and a
/// network that comes back after an hour must still be noticed promptly.
const RECONNECT_INITIAL: Duration = Duration::from_secs(2);
const RECONNECT_MAX: Duration = Duration::from_secs(60);

/// How long silence is allowed to last before the connection is assumed dead.
///
/// Twitch promises a keepalive every ten seconds by default and this is the
/// only way to notice a socket that has stopped delivering without closing —
/// which is what a dropped connection usually looks like from this side.
const SILENCE_LIMIT: Duration = Duration::from_secs(45);

/// One thing that happened to the channel.
///
/// Deliberately not a mirror of Twitch's payloads: this is what the interface
/// needs to show, which is a title, a line of detail, and how much it matters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamEvent {
    /// Short label: "New follower", "Hype train".
    pub title: String,
    /// The detail line: who, how much, which reward.
    pub detail: String,
    /// Which class of event this is, so the config can switch classes on and
    /// off independently.
    pub kind: EventKind,
}

/// The classes of event this module can report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventKind {
    Follow,
    Redemption,
    HypeTrain,
    Poll,
    Prediction,
}

/// What the interface is told.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Update {
    Event(StreamEvent),
    /// A problem worth putting in the activity log, with no pop-up.
    Trouble(String),
}

/// Everything the task needs to run.
pub struct Params {
    /// The channel to watch, as a Twitch user id.
    pub broadcaster_id: String,
    /// A currently-valid access token, fetched afresh for each connection.
    pub tokens: TokenProvider,
    pub client_id: String,
    pub http: reqwest::Client,
    pub updates: mpsc::UnboundedSender<Update>,
    /// Overridable so tests can point the task at a local socket.
    pub url: String,
}

impl Params {
    pub fn new(
        broadcaster_id: String,
        tokens: TokenProvider,
        client_id: String,
        http: reqwest::Client,
        updates: mpsc::UnboundedSender<Update>,
    ) -> Self {
        Self {
            broadcaster_id,
            tokens,
            client_id,
            http,
            updates,
            url: DEFAULT_URL.to_string(),
        }
    }
}

/// A running EventSub connection.
///
/// Dropping this ends the task, which is the whole interface: there is no
/// command channel, because there is nothing to ask it. It connects,
/// reconnects, and reports. Switching Twitch events off in the Config tab
/// therefore means dropping this, and nothing else.
pub struct Handle {
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Handle {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Start watching the channel.
pub fn spawn(params: Params) -> Handle {
    Handle {
        task: tokio::spawn(run(params)),
    }
}

/// What ended one connection.
enum Wake {
    /// Twitch asked us to move to a new URL, and the old socket stays up until
    /// we have. Not a failure and not backed off.
    Reconnect(String),
    /// The socket closed, went silent, or never opened.
    Lost(String),
}

async fn run(params: Params) {
    let mut url = params.url.clone();
    let mut attempt: u32 = 0;

    loop {
        match session(&params, &url).await {
            Wake::Reconnect(next) => {
                tracing::debug!(%next, "EventSub asked us to reconnect");
                url = next;
                attempt = 0;
            }
            Wake::Lost(reason) => {
                let delay = backoff(attempt);
                attempt = attempt.saturating_add(1);
                // Only the first loss of a run is reported. A network that has
                // been down for an hour must not fill the activity log with
                // one line per retry — the state has not changed since the
                // first line said so.
                if attempt == 1 {
                    let _ = params.updates.send(Update::Trouble(format!(
                        "Twitch events: {reason} — reconnecting"
                    )));
                }
                // Back at the start URL: a reconnect URL is single-use and
                // will not work twice.
                url = params.url.clone();
                tokio::time::sleep(delay).await;
            }
        }
    }
}

/// Doubling backoff, capped.
fn backoff(attempt: u32) -> Duration {
    let doubled = RECONNECT_INITIAL
        .checked_mul(1u32 << attempt.min(6))
        .unwrap_or(RECONNECT_MAX);
    doubled.min(RECONNECT_MAX)
}

/// One connection, from opening the socket to losing it.
async fn session(params: &Params, url: &str) -> Wake {
    let connected = tokio::time::timeout(CONNECT_TIMEOUT, tokio_tungstenite::connect_async(url));
    let stream = match connected.await {
        Ok(Ok((stream, _))) => stream,
        Ok(Err(err)) => return Wake::Lost(format!("could not connect ({err})")),
        Err(_) => return Wake::Lost("connecting timed out".to_string()),
    };
    let (mut sink, mut source) = stream.split();

    // The welcome carries the session id every subscription has to name.
    let welcome = match next_message(&mut source).await {
        Some(Envelope {
            metadata,
            payload: Payload { session, .. },
        }) if metadata.message_type == "session_welcome" => session,
        Some(_) => return Wake::Lost("Twitch opened with something other than a welcome".into()),
        None => return Wake::Lost("the connection closed during the handshake".into()),
    };
    let Some(session_id) = welcome
        .map(|session| session.id)
        .filter(|id| !id.is_empty())
    else {
        return Wake::Lost("Twitch's welcome carried no session id".into());
    };

    // A reconnect keeps its subscriptions: they belong to the *session*, and
    // Twitch carries them across when it hands out a reconnect URL. Only a
    // fresh session needs them created, and a fresh session is exactly one
    // whose welcome we just answered — so this runs either way, and Twitch
    // answers "already exists" harmlessly for the carried-over case.
    match subscribe_all(params, &session_id).await {
        Ok(0) => {
            return Wake::Lost(
                "no Twitch event subscriptions could be created — log in again under \
                 Config → Accounts to grant the new permissions"
                    .into(),
            )
        }
        Ok(_) => {}
        Err(err) => return Wake::Lost(err),
    }

    loop {
        let next = tokio::time::timeout(SILENCE_LIMIT, next_message(&mut source)).await;
        let envelope = match next {
            Err(_) => {
                // Twitch keeps the socket busy with keepalives, so silence
                // this long means it is gone whatever the socket believes.
                let _ = sink.close().await;
                return Wake::Lost("the connection went quiet".into());
            }
            Ok(None) => return Wake::Lost("the connection closed".into()),
            Ok(Some(envelope)) => envelope,
        };

        match envelope.metadata.message_type.as_str() {
            "session_keepalive" => {}
            "notification" => {
                if let Some(event) = interpret(&envelope) {
                    let _ = params.updates.send(Update::Event(event));
                }
            }
            "session_reconnect" => {
                if let Some(url) = envelope
                    .payload
                    .session
                    .and_then(|session| session.reconnect_url)
                {
                    return Wake::Reconnect(url);
                }
                return Wake::Lost("Twitch asked for a reconnect but gave no URL".into());
            }
            "revocation" => {
                // A subscription was withdrawn: the token lost a scope, or the
                // login was revoked. Saying which one is the difference
                // between a mystery and a fix.
                let name = envelope
                    .payload
                    .subscription
                    .map(|subscription| subscription.subscription_type)
                    .unwrap_or_else(|| "an event subscription".into());
                let _ = params.updates.send(Update::Trouble(format!(
                    "Twitch withdrew {name} — log in again under Config → Accounts"
                )));
            }
            other => tracing::debug!(message_type = other, "ignoring an EventSub message"),
        }
    }
}

/// Read the next JSON message, skipping pings and anything unparseable.
async fn next_message(source: &mut Source) -> Option<Envelope> {
    loop {
        match source.next().await {
            None => return None,
            Some(Err(_)) => return None,
            Some(Ok(WsMessage::Text(text))) => match serde_json::from_str::<Envelope>(&text) {
                Ok(envelope) => return Some(envelope),
                Err(err) => {
                    // A message this program cannot read is not a reason to
                    // drop a working connection: Twitch adds fields, and the
                    // next message is probably fine.
                    tracing::debug!(%err, "could not read an EventSub message");
                }
            },
            Some(Ok(WsMessage::Close(_))) => return None,
            // Ping/pong are handled by the library; binary never occurs here.
            Some(Ok(_)) => {}
        }
    }
}

type Source = futures_util::stream::SplitStream<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
>;

/// The subscriptions this program asks for, with the scope each one needs.
///
/// Deliberately short. Everything here is something no other part of the
/// program can see; anything IRC already delivers is left to IRC, because two
/// notifications for one event is worse than one.
const SUBSCRIPTIONS: &[Subscription] = &[
    Subscription {
        name: "channel.follow",
        version: "2",
        // The only one that needs naming us as well as the channel: Twitch
        // treats reading followers as a moderator capability.
        needs_moderator: true,
    },
    Subscription {
        name: "channel.channel_points_custom_reward_redemption.add",
        version: "1",
        needs_moderator: false,
    },
    Subscription {
        name: "channel.hype_train.begin",
        version: "1",
        needs_moderator: false,
    },
    Subscription {
        name: "channel.hype_train.end",
        version: "1",
        needs_moderator: false,
    },
    Subscription {
        name: "channel.poll.begin",
        version: "1",
        needs_moderator: false,
    },
    Subscription {
        name: "channel.prediction.begin",
        version: "1",
        needs_moderator: false,
    },
];

struct Subscription {
    name: &'static str,
    version: &'static str,
    needs_moderator: bool,
}

/// Create every subscription for this session, and report how many took.
///
/// A partial answer is the useful one. Each subscription needs its own scope,
/// an existing login may have some and not others, and a program that refused
/// to report follows because it could not also report hype trains would be
/// worse than one that reports what it can.
async fn subscribe_all(params: &Params, session_id: &str) -> Result<usize, String> {
    let token = (params.tokens)()
        .await
        .map_err(|err| format!("could not get a Twitch token ({err:#})"))?;

    let mut created = 0;
    let mut refused: Vec<&str> = Vec::new();
    for subscription in SUBSCRIPTIONS {
        let mut condition = serde_json::json!({
            "broadcaster_user_id": params.broadcaster_id,
        });
        if subscription.needs_moderator {
            condition["moderator_user_id"] = serde_json::json!(params.broadcaster_id);
        }
        let body = serde_json::json!({
            "type": subscription.name,
            "version": subscription.version,
            "condition": condition,
            "transport": { "method": "websocket", "session_id": session_id },
        });

        let sent = params
            .http
            .post(SUBSCRIPTIONS_URL)
            .header("Client-Id", &params.client_id)
            .bearer_auth(&token)
            .json(&body)
            .send()
            .await;
        match sent {
            Ok(response) if response.status().is_success() => created += 1,
            // 409 is "you already have this one", which is a success as far as
            // anybody here is concerned.
            Ok(response) if response.status().as_u16() == 409 => created += 1,
            Ok(response) => {
                tracing::debug!(
                    subscription = subscription.name,
                    status = response.status().as_u16(),
                    "Twitch refused an EventSub subscription"
                );
                refused.push(subscription.name);
            }
            Err(err) => {
                return Err(format!("could not reach Twitch to subscribe ({err})"));
            }
        }
    }

    if !refused.is_empty() && created > 0 {
        let _ = params.updates.send(Update::Trouble(format!(
            "Twitch declined some event subscriptions ({}) — usually a permission the saved \
             login does not have. Log in again under Config → Accounts to grant them.",
            refused.join(", ")
        )));
    }
    Ok(created)
}

/// Turn one notification into something worth showing, or nothing.
fn interpret(envelope: &Envelope) -> Option<StreamEvent> {
    let subscription = envelope.payload.subscription.as_ref()?;
    let event = envelope.payload.event.as_ref()?;
    let name = |value: &Option<String>| value.clone().unwrap_or_else(|| "somebody".to_string());

    let built = match subscription.subscription_type.as_str() {
        "channel.follow" => StreamEvent {
            title: "New follower".to_string(),
            detail: name(&event.user_name),
            kind: EventKind::Follow,
        },
        "channel.channel_points_custom_reward_redemption.add" => {
            let reward = event
                .reward
                .as_ref()
                .map(|reward| reward.title.clone())
                .unwrap_or_else(|| "a reward".to_string());
            let mut detail = format!("{} redeemed {reward}", name(&event.user_name));
            // The viewer's own text is the whole point of the redemptions
            // that have one, and it is what needs answering on stream.
            if let Some(input) = event.user_input.as_ref().filter(|text| !text.is_empty()) {
                detail.push_str(": ");
                detail.push_str(input);
            }
            StreamEvent {
                title: "Channel points".to_string(),
                detail,
                kind: EventKind::Redemption,
            }
        }
        "channel.hype_train.begin" => StreamEvent {
            title: "Hype train started".to_string(),
            detail: match event.level {
                Some(level) => format!("level {level}"),
                None => "it has begun".to_string(),
            },
            kind: EventKind::HypeTrain,
        },
        "channel.hype_train.end" => StreamEvent {
            title: "Hype train over".to_string(),
            detail: match event.level {
                Some(level) => format!("it reached level {level}"),
                None => "it has ended".to_string(),
            },
            kind: EventKind::HypeTrain,
        },
        "channel.poll.begin" => StreamEvent {
            title: "Poll started".to_string(),
            detail: event.title.clone().unwrap_or_default(),
            kind: EventKind::Poll,
        },
        "channel.prediction.begin" => StreamEvent {
            title: "Prediction started".to_string(),
            detail: event.title.clone().unwrap_or_default(),
            kind: EventKind::Prediction,
        },
        other => {
            tracing::debug!(subscription = other, "ignoring an unrecognised event");
            return None;
        }
    };
    Some(built)
}

// --- the wire ---------------------------------------------------------------
//
// Only the fields this program uses. Twitch adds fields to these payloads
// regularly and every one of them is optional here, so a new field is a
// no-op rather than a parse failure that would take the connection down.

#[derive(Debug, Deserialize)]
struct Envelope {
    metadata: Metadata,
    #[serde(default)]
    payload: Payload,
}

#[derive(Debug, Deserialize)]
struct Metadata {
    #[serde(default)]
    message_type: String,
}

#[derive(Debug, Default, Deserialize)]
struct Payload {
    #[serde(default)]
    session: Option<Session>,
    #[serde(default)]
    subscription: Option<SubscriptionInfo>,
    #[serde(default)]
    event: Option<Event>,
}

#[derive(Debug, Deserialize)]
struct Session {
    #[serde(default)]
    id: String,
    #[serde(default)]
    reconnect_url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SubscriptionInfo {
    #[serde(default, rename = "type")]
    subscription_type: String,
}

#[derive(Debug, Deserialize)]
struct Event {
    #[serde(default)]
    user_name: Option<String>,
    #[serde(default)]
    user_input: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    level: Option<u32>,
    #[serde(default)]
    reward: Option<Reward>,
}

#[derive(Debug, Deserialize)]
struct Reward {
    #[serde(default)]
    title: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn envelope(json: &str) -> Envelope {
        serde_json::from_str(json).expect("the fixture must parse")
    }

    /// The event this module exists for. Nothing else in the program can see
    /// a follow: it is not chat and never has been.
    #[test]
    fn a_follow_becomes_a_named_event() {
        let event = interpret(&envelope(
            r#"{
                "metadata": {"message_type": "notification"},
                "payload": {
                    "subscription": {"type": "channel.follow"},
                    "event": {"user_name": "Alice", "user_id": "1"}
                }
            }"#,
        ))
        .expect("a follow must be reported");
        assert_eq!(event.title, "New follower");
        assert_eq!(event.detail, "Alice");
        assert_eq!(event.kind, EventKind::Follow);
    }

    /// A redemption's whole point is often the text the viewer typed with it,
    /// which is the thing that needs answering on stream.
    #[test]
    fn a_redemption_carries_the_reward_and_what_was_typed() {
        let event = interpret(&envelope(
            r#"{
                "metadata": {"message_type": "notification"},
                "payload": {
                    "subscription": {"type": "channel.channel_points_custom_reward_redemption.add"},
                    "event": {
                        "user_name": "Bob",
                        "user_input": "play the song again",
                        "reward": {"title": "Song request"}
                    }
                }
            }"#,
        ))
        .expect("a redemption must be reported");
        assert_eq!(event.title, "Channel points");
        assert_eq!(
            event.detail,
            "Bob redeemed Song request: play the song again"
        );
        assert_eq!(event.kind, EventKind::Redemption);
    }

    #[test]
    fn a_redemption_without_text_says_only_what_happened() {
        let event = interpret(&envelope(
            r#"{
                "metadata": {"message_type": "notification"},
                "payload": {
                    "subscription": {"type": "channel.channel_points_custom_reward_redemption.add"},
                    "event": {"user_name": "Bob", "user_input": "", "reward": {"title": "Hydrate"}}
                }
            }"#,
        ))
        .expect("a redemption must be reported");
        assert_eq!(event.detail, "Bob redeemed Hydrate");
    }

    #[test]
    fn hype_trains_report_their_level_at_both_ends() {
        for (kind, expected) in [
            ("channel.hype_train.begin", "level 1"),
            ("channel.hype_train.end", "it reached level 1"),
        ] {
            let json = format!(
                r#"{{
                    "metadata": {{"message_type": "notification"}},
                    "payload": {{
                        "subscription": {{"type": "{kind}"}},
                        "event": {{"level": 1}}
                    }}
                }}"#
            );
            let event = interpret(&envelope(&json)).expect("a hype train must be reported");
            assert_eq!(event.detail, expected);
            assert_eq!(event.kind, EventKind::HypeTrain);
        }
    }

    /// Twitch adds event types, and a payload this program does not know is
    /// not an error — it is simply not shown.
    #[test]
    fn an_unrecognised_event_is_ignored_rather_than_guessed_at() {
        assert!(interpret(&envelope(
            r#"{
                "metadata": {"message_type": "notification"},
                "payload": {"subscription": {"type": "channel.something.new"}, "event": {}}
            }"#,
        ))
        .is_none());
    }

    /// New fields appear in these payloads regularly. Every field this module
    /// reads is optional precisely so that one cannot take the connection
    /// down mid-stream.
    #[test]
    fn unknown_and_missing_fields_do_not_break_parsing() {
        let event = interpret(&envelope(
            r#"{
                "metadata": {"message_type": "notification", "message_id": "x", "brand_new": 7},
                "payload": {
                    "subscription": {"type": "channel.follow", "cost": 0},
                    "event": {"followed_at": "2026-01-01T00:00:00Z"}
                }
            }"#,
        ))
        .expect("a follow with no name is still a follow");
        assert_eq!(event.detail, "somebody");
    }

    /// Keepalives and welcomes are not notifications and must not be read as
    /// empty ones.
    #[test]
    fn only_notifications_are_interpreted() {
        assert!(interpret(&envelope(
            r#"{"metadata": {"message_type": "session_keepalive"}, "payload": {}}"#
        ))
        .is_none());
    }

    /// A reconnect that backed off would defeat the point: Twitch cycles the
    /// connection on purpose and hands over a working replacement.
    #[test]
    fn backoff_doubles_and_stops_at_the_ceiling() {
        assert_eq!(backoff(0), RECONNECT_INITIAL);
        assert_eq!(backoff(1), RECONNECT_INITIAL * 2);
        assert_eq!(backoff(2), RECONNECT_INITIAL * 4);
        assert_eq!(backoff(30), RECONNECT_MAX);
    }

    /// Anything IRC already delivers must not be subscribed to here, or every
    /// subscription and cheer would be announced twice.
    #[test]
    fn nothing_chat_already_delivers_is_subscribed_to() {
        for subscription in SUBSCRIPTIONS {
            for chat_delivers in [
                "channel.subscribe",
                "channel.subscription.gift",
                "channel.subscription.message",
                "channel.cheer",
                "channel.raid",
            ] {
                assert_ne!(
                    subscription.name, chat_delivers,
                    "{chat_delivers} arrives over IRC already"
                );
            }
        }
    }
}
