//! The connection to OBS: one task, commands in, events out.
//!
//! This is the same shape as the chat tasks. A single task owns the WebSocket
//! — nothing else can write to it, so no lock is needed and no two requests
//! can interleave halfway through a message. The interface sends it commands
//! and receives updates; it never blocks the interface and the interface never
//! blocks it.
//!
//! Reconnection is the part worth designing rather than improvising, because
//! OBS restarting mid-stream is *ordinary*. A crash, an update, someone
//! closing it by accident — the pane should notice and come back on its own
//! within a few seconds, without anyone typing anything. The delay backs off
//! so a machine with no OBS at all is not reconnecting ten times a second
//! forever, and it is capped so a machine whose OBS comes back after an hour
//! still reconnects promptly.

use anyhow::{bail, Context, Result};
use futures_util::{SinkExt as _, StreamExt as _};
use std::collections::HashMap;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message as WsMessage;

use super::event::Event;
use super::protocol::{self, Hello, Identify, Message, Request, RequestResponse};
use super::requests;
use super::state::{AudioInput, Connection, ObsState, Scene, Stats};

/// How long to wait for the TCP connection and the handshake.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// How long to wait for a request to be answered.
///
/// OBS answers in single-digit milliseconds when it is healthy. Five seconds
/// is not a guess at how long it takes; it is long enough that a machine
/// briefly thrashing does not produce a spurious failure, and short enough
/// that a wedged connection is noticed rather than waited on forever.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

/// The first reconnection delay, and the ceiling it backs off to.
const RECONNECT_INITIAL: Duration = Duration::from_secs(1);
const RECONNECT_MAX: Duration = Duration::from_secs(30);

/// How often to ask for the figures that no event announces.
///
/// The statistics, and the duration and bitrate of a running stream, have no
/// events — OBS only reports them when asked. Once a second matches what OBS's
/// own status bar does, and the request is cheap and local.
const POLL_INTERVAL: Duration = Duration::from_secs(1);

/// What the interface can ask of OBS.
#[derive(Debug, Clone)]
pub enum Command {
    SetScene(String),
    ToggleMute(String),
    SetMute {
        input: String,
        muted: bool,
    },
    /// Set a volume as a multiplier of unity gain.
    SetVolume {
        input: String,
        multiplier: f64,
    },
    SetProfile(String),
    SetSceneCollection(String),
    ToggleStream,
    ToggleRecord,
    ToggleRecordPause,
    /// Give up on the current connection and start again immediately, rather
    /// than waiting out the backoff.
    Reconnect,
    /// Ask for everything, for when the pane is opened after a spell away.
    Refresh,
}

/// What the connection reports back.
#[derive(Debug, Clone)]
pub enum Update {
    /// The connection's state changed.
    Connection(Connection),
    /// A complete replacement for the cached state, after a refresh.
    Snapshot(Box<ObsState>),
    /// One change, from an OBS event.
    Event(Event),
    /// A command could not be carried out. The interface shows this; it is
    /// never a reason to drop the connection.
    CommandFailed(String),
}

/// How to reach OBS.
#[derive(Debug, Clone)]
pub struct Params {
    pub url: String,
    pub password: Option<String>,
    /// Aliases and shortcuts for scenes, keyed by the OBS scene name.
    pub scene_labels: HashMap<String, (Option<String>, Option<String>)>,
    /// The same for audio inputs.
    pub audio_labels: HashMap<String, (Option<String>, Option<String>)>,
}

impl Params {
    /// A description safe to log. The password is never part of it.
    pub fn describe(&self) -> String {
        format!(
            "{} ({})",
            self.url,
            if self.password.is_some() {
                "with a password"
            } else {
                "no password"
            }
        )
    }
}

/// A handle to the running connection.
pub struct Handle {
    pub commands: mpsc::Sender<Command>,
    pub task: tokio::task::JoinHandle<()>,
}

/// Start the connection task.
///
/// It runs until its command channel is dropped, reconnecting on its own for
/// as long as that takes.
pub fn spawn(params: Params, updates: mpsc::UnboundedSender<Update>) -> Handle {
    let (command_tx, command_rx) = mpsc::channel(32);
    let task = tokio::spawn(run(params, command_rx, updates));
    Handle {
        commands: command_tx,
        task,
    }
}

/// The reconnect loop: connect, serve, wait, repeat.
async fn run(
    params: Params,
    mut commands: mpsc::Receiver<Command>,
    updates: mpsc::UnboundedSender<Update>,
) {
    let mut delay = RECONNECT_INITIAL;

    loop {
        let _ = updates.send(Update::Connection(Connection::Connecting));

        match session(&params, &mut commands, &updates).await {
            Ok(Outcome::Closed) => {
                // The interface dropped its sender: this is a shutdown, not a
                // failure, so nothing is reported and nothing is retried.
                return;
            }
            Ok(Outcome::Disconnected) => {
                tracing::info!("OBS disconnected; will retry");
                // A connection that worked once will very likely work again
                // as soon as OBS is back, so the backoff starts over rather
                // than continuing from wherever the last outage left it.
                delay = RECONNECT_INITIAL;
            }
            Err(err) => {
                tracing::debug!(error = %format!("{err:#}"), "OBS connection failed");
                let _ = updates.send(Update::Connection(Connection::Failed(format!("{err:#}"))));
            }
        }

        let _ = updates.send(Update::Connection(Connection::Reconnecting));

        // Wait, but stay responsive: an explicit reconnect must not have to
        // sit through the backoff, and a shutdown must not either.
        tokio::select! {
            command = commands.recv() => match command {
                // The channel closed: the interface is going away.
                None => return,
                Some(Command::Reconnect) => {
                    delay = RECONNECT_INITIAL;
                    continue;
                }
                // Anything else cannot be carried out with no connection.
                Some(_) => {
                    let _ = updates.send(Update::CommandFailed(
                        "OBS is not connected.".to_string(),
                    ));
                }
            },
            _ = tokio::time::sleep(delay) => {}
        }

        delay = (delay * 2).min(RECONNECT_MAX);
    }
}

/// Why a session ended.
enum Outcome {
    /// The interface is shutting down.
    Closed,
    /// OBS went away.
    Disconnected,
}

/// One connection, from handshake to disconnect.
async fn session(
    params: &Params,
    commands: &mut mpsc::Receiver<Command>,
    updates: &mpsc::UnboundedSender<Update>,
) -> Result<Outcome> {
    let (stream, _) = tokio::time::timeout(
        CONNECT_TIMEOUT,
        tokio_tungstenite::connect_async(&params.url),
    )
    .await
    .with_context(|| format!("connecting to OBS at {} timed out", params.url))?
    .with_context(|| format!("connecting to OBS at {}", params.url))?;

    let (mut sink, mut source) = stream.split();

    // --- handshake ---------------------------------------------------------
    let hello = read_message(&mut source)
        .await
        .context("waiting for OBS to say hello")?;
    if hello.op != protocol::OPCODE_HELLO {
        bail!("OBS sent opcode {} where a hello belongs", hello.op);
    }
    let hello: Hello = serde_json::from_value(hello.d).context("reading OBS's hello")?;

    let authentication = match (&hello.authentication, &params.password) {
        (Some(challenge), Some(password)) => Some(super::auth::compute(
            password,
            &challenge.salt,
            &challenge.challenge,
        )),
        (Some(_), None) => bail!(
            "OBS is asking for a password. Put it in `[obs] password` in config.toml, or in \
             the environment variable named by `password_env`."
        ),
        // OBS is not asking for one. A configured password is simply unused;
        // saying so would be noise, since turning authentication off in OBS
        // is a deliberate act.
        (None, _) => None,
    };

    let identify = Message {
        op: protocol::OPCODE_IDENTIFY,
        d: serde_json::to_value(Identify {
            rpc_version: protocol::RPC_VERSION,
            authentication,
            event_subscriptions: Some(protocol::subscriptions::DEFAULT),
        })
        .context("building the identify message")?,
    };
    send(&mut sink, &identify).await?;

    // What OBS does with a wrong password is close the connection without
    // answering — so the failure arrives as "the socket went away", which
    // says nothing about the actual mistake. When a password was in play,
    // that is overwhelmingly what happened, and saying so beats making
    // somebody guess. The wording stays hedged because a genuine network
    // failure at exactly this moment looks identical from here.
    let authenticated = hello.authentication.is_some();
    let identified = match read_message(&mut source).await {
        Ok(message) => message,
        Err(err) if authenticated => {
            return Err(err.context(
                "OBS closed the connection during authentication — the password is probably wrong",
            ));
        }
        Err(err) => return Err(err.context("waiting for OBS to accept the connection")),
    };
    if identified.op != protocol::OPCODE_IDENTIFIED {
        bail!(
            "OBS refused the connection{}",
            if authenticated {
                " — the password is probably wrong"
            } else {
                ""
            }
        );
    }

    let _ = updates.send(Update::Connection(Connection::Connected));

    // --- serve -------------------------------------------------------------
    let mut pending: HashMap<String, oneshot::Sender<Result<serde_json::Value>>> = HashMap::new();
    let mut poll = tokio::time::interval(POLL_INTERVAL);
    poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    // Everything, once, before the first frame that shows the pane.
    let mut state = ObsState {
        connection: Connection::Connected,
        ..Default::default()
    };
    refresh_all(&mut sink, &mut source, &mut pending, params, &mut state).await?;
    let _ = updates.send(Update::Snapshot(Box::new(state.clone())));

    loop {
        tokio::select! {
            command = commands.recv() => {
                let Some(command) = command else { return Ok(Outcome::Closed) };
                if let Err(err) =
                    run_command(command, &mut sink, &mut source, &mut pending, params, &mut state, updates).await
                {
                    // A command failing is not a reason to drop the
                    // connection — a scene that no longer exists is a
                    // mistake, not a broken socket. Only an error that says
                    // the transport itself is gone ends the session.
                    if is_transport_failure(&err) {
                        return Ok(Outcome::Disconnected);
                    }
                    let _ = updates.send(Update::CommandFailed(format!("{err:#}")));
                }
            }

            incoming = source.next() => {
                let Some(incoming) = incoming else { return Ok(Outcome::Disconnected) };
                let Ok(message) = incoming else { return Ok(Outcome::Disconnected) };
                let Some(message) = decode(message) else { continue };
                let event = handle_incoming(message, &mut pending, &mut state, updates);

                // Some events only say *that* a list changed — a scene was
                // added, an input removed, a whole scene collection swapped.
                // The new contents are not in the event, so the only way to
                // show them is to go and ask. Without this, a scene added in
                // OBS would never appear in the pane.
                if event.is_some_and(|event| event.needs_refresh()) && connected(&state) {
                    match refresh_all(&mut sink, &mut source, &mut pending, params, &mut state).await
                    {
                        Ok(()) => {
                            let _ = updates.send(Update::Snapshot(Box::new(state.clone())));
                        }
                        Err(err) if is_transport_failure(&err) => {
                            return Ok(Outcome::Disconnected);
                        }
                        Err(err) => {
                            tracing::debug!(
                                error = %format!("{err:#}"),
                                "refreshing after an OBS event failed"
                            );
                        }
                    }
                }
            }

            _ = poll.tick() => {
                // The figures no event announces. A failure here is not worth
                // reporting — the next tick tries again a second later — but
                // a dead transport still has to end the session.
                if let Err(err) = poll_status(&mut sink, &mut source, &mut pending, &mut state).await {
                    if is_transport_failure(&err) {
                        return Ok(Outcome::Disconnected);
                    }
                    tracing::debug!(error = %format!("{err:#}"), "OBS status poll failed");
                } else {
                    let _ = updates.send(Update::Snapshot(Box::new(state.clone())));
                }
            }
        }
    }
}

/// Whether an error means the connection itself has gone.
fn is_transport_failure(error: &anyhow::Error) -> bool {
    error
        .chain()
        .any(|cause| cause.downcast_ref::<TransportGone>().is_some())
}

/// Marker for "the socket is gone", so a command failure can be told from a
/// connection failure without matching on message text.
#[derive(Debug)]
struct TransportGone;

impl std::fmt::Display for TransportGone {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "the connection to OBS was lost")
    }
}

impl std::error::Error for TransportGone {}

type Sink = futures_util::stream::SplitSink<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    WsMessage,
>;
type Source = futures_util::stream::SplitStream<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
>;

async fn send(sink: &mut Sink, message: &Message) -> Result<()> {
    let text = serde_json::to_string(message).context("serialising a message for OBS")?;
    sink.send(WsMessage::Text(text))
        .await
        .map_err(|_| anyhow::Error::new(TransportGone))?;
    Ok(())
}

/// Read the next protocol message, ignoring anything that is not one.
async fn read_message(source: &mut Source) -> Result<Message> {
    loop {
        let next = source.next().await.ok_or(TransportGone)?;
        let message = next.map_err(|_| TransportGone)?;
        if let Some(decoded) = decode(message) {
            return Ok(decoded);
        }
    }
}

/// Turn a WebSocket frame into a protocol message, or `None` for the frames
/// that carry no protocol content (pings, and the close frame).
fn decode(message: WsMessage) -> Option<Message> {
    let text = match message {
        WsMessage::Text(text) => text,
        WsMessage::Binary(bytes) => String::from_utf8(bytes).ok()?,
        _ => return None,
    };
    match serde_json::from_str(&text) {
        Ok(message) => Some(message),
        Err(err) => {
            tracing::debug!(error = %err, "ignoring an unreadable message from OBS");
            None
        }
    }
}

/// Deal with a message that arrived unprompted.
///
/// Two kinds land here: the answer to a request (matched to whoever is waiting
/// by its id) and an event.
fn handle_incoming(
    message: Message,
    pending: &mut HashMap<String, oneshot::Sender<Result<serde_json::Value>>>,
    state: &mut ObsState,
    updates: &mpsc::UnboundedSender<Update>,
) -> Option<Event> {
    match message.op {
        protocol::OPCODE_REQUEST_RESPONSE => {
            let Ok(response) = serde_json::from_value::<RequestResponse>(message.d) else {
                return None;
            };
            let Some(waiting) = pending.remove(&response.request_id) else {
                // An answer to a request that has already timed out. Dropping
                // it is right: whoever asked has given up and been told so.
                return None;
            };
            let result = if response.request_status.result {
                Ok(response.response_data.unwrap_or(serde_json::Value::Null))
            } else {
                Err(anyhow::anyhow!("{}", response.request_status.describe()))
            };
            let _ = waiting.send(result);
            None
        }
        protocol::OPCODE_EVENT => {
            let event = serde_json::from_value::<protocol::Event>(message.d).ok()?;
            let parsed = Event::from_raw(&event.event_type, event.event_data.as_ref())?;
            parsed.apply(state);
            let _ = updates.send(Update::Event(parsed.clone()));
            Some(parsed)
        }
        _ => None,
    }
}

/// Send a request and wait for its answer.
///
/// The reply is matched by request id, so several may be outstanding at once.
/// While waiting, incoming events are still applied — a handshake that ignored
/// events would drop any that arrived during the initial burst of requests.
async fn ask(
    sink: &mut Sink,
    source: &mut Source,
    pending: &mut HashMap<String, oneshot::Sender<Result<serde_json::Value>>>,
    state: &mut ObsState,
    request: Request,
) -> Result<serde_json::Value> {
    let id = request.request_id.clone();
    let request_type = request.request_type.clone();
    let (reply_tx, mut reply_rx) = oneshot::channel();
    pending.insert(id.clone(), reply_tx);

    let message = Message {
        op: protocol::OPCODE_REQUEST,
        d: serde_json::to_value(&request).context("serialising a request")?,
    };
    if let Err(err) = send(sink, &message).await {
        pending.remove(&id);
        return Err(err);
    }

    let deadline = tokio::time::Instant::now() + REQUEST_TIMEOUT;
    loop {
        tokio::select! {
            reply = &mut reply_rx => {
                return match reply {
                    Ok(result) => result.with_context(|| format!("OBS request {request_type}")),
                    Err(_) => Err(anyhow::Error::new(TransportGone)),
                };
            }
            incoming = source.next() => {
                let Some(incoming) = incoming else {
                    pending.remove(&id);
                    return Err(anyhow::Error::new(TransportGone));
                };
                let Ok(message) = incoming else {
                    pending.remove(&id);
                    return Err(anyhow::Error::new(TransportGone));
                };
                if let Some(message) = decode(message) {
                    // Events arriving mid-request are applied but not
                    // announced: the caller is about to send a fresh snapshot
                    // anyway, and announcing both would show the same change
                    // twice.
                    handle_incoming_quietly(message, pending, state);
                }
            }
            _ = tokio::time::sleep_until(deadline) => {
                pending.remove(&id);
                anyhow::bail!("OBS did not answer {request_type} within {REQUEST_TIMEOUT:?}");
            }
        }
    }
}

/// [`handle_incoming`] without the announcements, for use while a request is
/// in flight.
fn handle_incoming_quietly(
    message: Message,
    pending: &mut HashMap<String, oneshot::Sender<Result<serde_json::Value>>>,
    state: &mut ObsState,
) {
    let (dummy_tx, _dummy_rx) = mpsc::unbounded_channel();
    handle_incoming(message, pending, state, &dummy_tx);
}

/// Whether the connection is up, from the state the task itself keeps.
fn connected(state: &ObsState) -> bool {
    state.is_connected()
}

/// Ask OBS for everything, and rebuild the cached state from the answers.
async fn refresh_all(
    sink: &mut Sink,
    source: &mut Source,
    pending: &mut HashMap<String, oneshot::Sender<Result<serde_json::Value>>>,
    params: &Params,
    state: &mut ObsState,
) -> Result<()> {
    let version = ask(sink, source, pending, state, requests::get_version()).await?;
    state.obs_version = version
        .get("obsVersion")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    state.websocket_version = version
        .get("obsWebSocketVersion")
        .and_then(|value| value.as_str())
        .map(str::to_string);

    refresh_scenes(sink, source, pending, params, state).await?;
    refresh_audio(sink, source, pending, params, state).await?;
    refresh_profiles(sink, source, pending, state).await?;
    poll_status(sink, source, pending, state).await?;
    Ok(())
}

async fn refresh_scenes(
    sink: &mut Sink,
    source: &mut Source,
    pending: &mut HashMap<String, oneshot::Sender<Result<serde_json::Value>>>,
    params: &Params,
    state: &mut ObsState,
) -> Result<()> {
    let scenes = ask(sink, source, pending, state, requests::get_scene_list()).await?;
    state.current_scene = scenes
        .get("currentProgramSceneName")
        .and_then(|value| value.as_str())
        .map(str::to_string);

    let list = scenes
        .get("scenes")
        .and_then(|value| value.as_array())
        .cloned()
        .unwrap_or_default();

    // OBS returns scenes bottom-first, matching the order they are stacked in
    // its own list. Reversing puts them in the order they are *shown* there,
    // so the pane reads the same way as the window beside it.
    state.scenes = list
        .iter()
        .rev()
        .filter_map(|entry| entry.get("sceneName")?.as_str())
        .map(|name| {
            let (alias, shortcut) = params.scene_labels.get(name).cloned().unwrap_or_default();
            Scene {
                name: name.to_string(),
                alias,
                shortcut,
            }
        })
        .collect();
    Ok(())
}

/// OBS input kinds that carry audio.
///
/// There is no request for "the audio inputs", so they are picked out of the
/// full input list by kind. Matching on a substring rather than an exact list
/// is deliberate: the kinds are platform-specific (`pulse_input_capture`,
/// `coreaudio_input_capture`, `wasapi_input_capture`) and new ones appear, so
/// an exact list would quietly stop showing a microphone on the next platform.
fn is_audio_kind(kind: &str) -> bool {
    const MARKERS: [&str; 6] = ["audio", "wasapi", "coreaudio", "pulse", "alsa", "jack"];
    let kind = kind.to_ascii_lowercase();
    MARKERS.iter().any(|marker| kind.contains(marker))
}

async fn refresh_audio(
    sink: &mut Sink,
    source: &mut Source,
    pending: &mut HashMap<String, oneshot::Sender<Result<serde_json::Value>>>,
    params: &Params,
    state: &mut ObsState,
) -> Result<()> {
    let inputs = ask(sink, source, pending, state, requests::get_input_list()).await?;
    let list = inputs
        .get("inputs")
        .and_then(|value| value.as_array())
        .cloned()
        .unwrap_or_default();

    let mut audio = Vec::new();
    for entry in list {
        let Some(name) = entry.get("inputName").and_then(|value| value.as_str()) else {
            continue;
        };
        let kind = entry
            .get("inputKind")
            .and_then(|value| value.as_str())
            .map(str::to_string);
        if !kind.as_deref().is_some_and(is_audio_kind) {
            continue;
        }

        let (alias, shortcut) = params.audio_labels.get(name).cloned().unwrap_or_default();
        let mut input = AudioInput {
            name: name.to_string(),
            alias,
            shortcut,
            kind,
            muted: None,
            volume_mul: None,
            volume_db: None,
        };

        // Mute and volume come one input at a time. A source that refuses
        // either — some kinds have no volume at all — is still listed, with
        // that value left unknown rather than guessed at.
        if let Ok(mute) = ask(sink, source, pending, state, requests::get_input_mute(name)).await {
            input.muted = mute.get("inputMuted").and_then(|value| value.as_bool());
        }
        if let Ok(volume) = ask(
            sink,
            source,
            pending,
            state,
            requests::get_input_volume(name),
        )
        .await
        {
            input.volume_mul = volume
                .get("inputVolumeMul")
                .and_then(|value| value.as_f64());
            input.volume_db = volume.get("inputVolumeDb").and_then(|value| value.as_f64());
        }
        audio.push(input);
    }
    state.audio = audio;
    Ok(())
}

async fn refresh_profiles(
    sink: &mut Sink,
    source: &mut Source,
    pending: &mut HashMap<String, oneshot::Sender<Result<serde_json::Value>>>,
    state: &mut ObsState,
) -> Result<()> {
    let profiles = ask(sink, source, pending, state, requests::get_profile_list()).await?;
    state.current_profile = profiles
        .get("currentProfileName")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    state.profiles = string_list(&profiles, "profiles");

    let collections = ask(
        sink,
        source,
        pending,
        state,
        requests::get_scene_collection_list(),
    )
    .await?;
    state.current_scene_collection = collections
        .get("currentSceneCollectionName")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    state.scene_collections = string_list(&collections, "sceneCollections");
    Ok(())
}

fn string_list(value: &serde_json::Value, key: &str) -> Vec<String> {
    value
        .get(key)
        .and_then(|value| value.as_array())
        .map(|list| {
            list.iter()
                .filter_map(|entry| entry.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// The figures no event announces: stream and record status, and statistics.
async fn poll_status(
    sink: &mut Sink,
    source: &mut Source,
    pending: &mut HashMap<String, oneshot::Sender<Result<serde_json::Value>>>,
    state: &mut ObsState,
) -> Result<()> {
    let stream = ask(sink, source, pending, state, requests::get_stream_status()).await?;
    state.streaming = stream
        .get("outputActive")
        .and_then(|value| value.as_bool())
        .unwrap_or(false);
    state.stream_duration = stream
        .get("outputDuration")
        .and_then(|value| value.as_f64())
        .filter(|duration| *duration > 0.0)
        .map(|duration| Duration::from_millis(duration as u64));
    // OBS reports bytes; the figure people talk about is kilobits per second.
    state.stream_bitrate_kbps = match (
        stream.get("outputBytes").and_then(|value| value.as_f64()),
        state.stream_duration,
    ) {
        (Some(bytes), Some(duration)) if duration.as_secs_f64() > 0.0 => {
            Some(bytes * 8.0 / 1000.0 / duration.as_secs_f64())
        }
        _ => None,
    };

    let record = ask(sink, source, pending, state, requests::get_record_status()).await?;
    state.recording = record
        .get("outputActive")
        .and_then(|value| value.as_bool())
        .unwrap_or(false);
    state.record_paused = record
        .get("outputPaused")
        .and_then(|value| value.as_bool())
        .unwrap_or(false);
    state.record_duration = record
        .get("outputDuration")
        .and_then(|value| value.as_f64())
        .filter(|duration| *duration > 0.0)
        .map(|duration| Duration::from_millis(duration as u64));

    let stats = ask(sink, source, pending, state, requests::get_stats()).await?;
    let number = |key: &str| {
        stats
            .get(key)
            .and_then(|value| value.as_f64())
            .unwrap_or(0.0)
    };
    state.stats = Some(Stats {
        cpu_usage_percent: number("cpuUsage"),
        memory_usage_mb: number("memoryUsage"),
        available_disk_space_mb: number("availableDiskSpace"),
        active_fps: number("activeFps"),
        average_frame_render_time_ms: number("averageFrameRenderTime"),
        render_skipped_frames: number("renderSkippedFrames") as u64,
        render_total_frames: number("renderTotalFrames") as u64,
        output_skipped_frames: number("outputSkippedFrames") as u64,
        output_total_frames: number("outputTotalFrames") as u64,
    });
    Ok(())
}

/// Carry out one command from the interface.
#[allow(clippy::too_many_arguments)]
async fn run_command(
    command: Command,
    sink: &mut Sink,
    source: &mut Source,
    pending: &mut HashMap<String, oneshot::Sender<Result<serde_json::Value>>>,
    params: &Params,
    state: &mut ObsState,
    updates: &mpsc::UnboundedSender<Update>,
) -> Result<()> {
    match command {
        Command::SetScene(target) => {
            let name = resolve_scene(state, &target)?;
            ask(
                sink,
                source,
                pending,
                state,
                requests::set_current_program_scene(&name),
            )
            .await?;
        }
        Command::ToggleMute(target) => {
            let name = resolve_audio(state, &target)?;
            ask(
                sink,
                source,
                pending,
                state,
                requests::toggle_input_mute(&name),
            )
            .await?;
        }
        Command::SetMute { input, muted } => {
            let name = resolve_audio(state, &input)?;
            ask(
                sink,
                source,
                pending,
                state,
                requests::set_input_mute(&name, muted),
            )
            .await?;
        }
        Command::SetVolume { input, multiplier } => {
            let name = resolve_audio(state, &input)?;
            ask(
                sink,
                source,
                pending,
                state,
                requests::set_input_volume(&name, multiplier),
            )
            .await?;
        }
        Command::SetProfile(name) => {
            ask(
                sink,
                source,
                pending,
                state,
                requests::set_current_profile(&name),
            )
            .await?;
        }
        Command::SetSceneCollection(name) => {
            ask(
                sink,
                source,
                pending,
                state,
                requests::set_current_scene_collection(&name),
            )
            .await?;
            // A different collection means different scenes and inputs.
            refresh_scenes(sink, source, pending, params, state).await?;
            refresh_audio(sink, source, pending, params, state).await?;
        }
        Command::ToggleStream => {
            ask(sink, source, pending, state, requests::toggle_stream()).await?;
        }
        Command::ToggleRecord => {
            ask(sink, source, pending, state, requests::toggle_record()).await?;
        }
        Command::ToggleRecordPause => {
            ask(
                sink,
                source,
                pending,
                state,
                requests::toggle_record_pause(),
            )
            .await?;
        }
        Command::Refresh => {
            refresh_all(sink, source, pending, params, state).await?;
        }
        // Handled by ending the session, which the reconnect loop then
        // notices and acts on.
        Command::Reconnect => return Err(anyhow::Error::new(TransportGone)),
    }

    let _ = updates.send(Update::Snapshot(Box::new(state.clone())));
    Ok(())
}

/// Turn what somebody typed into the scene name OBS knows.
fn resolve_scene(state: &ObsState, target: &str) -> Result<String> {
    match state.find_scene(target) {
        Some(scene) => Ok(scene.name.clone()),
        None => {
            let known = state
                .scenes
                .iter()
                .map(|scene| scene.label())
                .collect::<Vec<_>>()
                .join(", ");
            if known.is_empty() {
                bail!("no scene called {target:?}, and OBS has not reported any scenes yet")
            }
            bail!("no scene called {target:?}. Try one of: {known}")
        }
    }
}

fn resolve_audio(state: &ObsState, target: &str) -> Result<String> {
    match state.find_audio(target) {
        Some(input) => Ok(input.name.clone()),
        None => {
            let known = state
                .audio
                .iter()
                .map(|input| input.label())
                .collect::<Vec<_>>()
                .join(", ");
            if known.is_empty() {
                bail!("no audio input called {target:?}, and OBS has not reported any")
            }
            bail!("no audio input called {target:?}. Try one of: {known}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_connection_description_never_includes_the_password() {
        let params = Params {
            url: "ws://127.0.0.1:4455".to_string(),
            password: Some("hunter2".to_string()),
            scene_labels: HashMap::new(),
            audio_labels: HashMap::new(),
        };
        let described = params.describe();
        assert!(!described.contains("hunter2"));
        assert!(described.contains("with a password"));

        let without = Params {
            password: None,
            ..params
        };
        assert!(without.describe().contains("no password"));
    }

    /// Audio inputs are picked out of the full input list by kind, and the
    /// kinds are platform-specific. Matching too narrowly would silently stop
    /// showing a microphone on some platform.
    #[test]
    fn audio_inputs_are_recognised_on_every_platform() {
        for kind in [
            "pulse_input_capture",
            "pulse_output_capture",
            "coreaudio_input_capture",
            "wasapi_input_capture",
            "wasapi_output_capture",
            "wasapi_process_output_capture",
            "alsa_input_capture",
            "jack_output_capture",
            "audio_line",
        ] {
            assert!(is_audio_kind(kind), "{kind} should count as audio");
        }
    }

    #[test]
    fn things_that_are_not_audio_are_left_out() {
        for kind in [
            "browser_source",
            "image_source",
            "text_ft2_source_v2",
            "v4l2_input",
            "xcomposite_input",
            "window_capture",
        ] {
            assert!(!is_audio_kind(kind), "{kind} should not count as audio");
        }
    }

    #[test]
    fn resolving_names_an_alternative_when_the_target_is_unknown() {
        let state = ObsState {
            scenes: vec![
                Scene {
                    name: "Main".into(),
                    alias: Some("cam".into()),
                    shortcut: None,
                },
                Scene {
                    name: "Break".into(),
                    alias: None,
                    shortcut: None,
                },
            ],
            ..Default::default()
        };

        let error = resolve_scene(&state, "nope").expect_err("an unknown scene fails");
        let message = format!("{error}");
        assert!(message.contains("cam"), "got {message}");
        assert!(message.contains("Break"), "got {message}");
    }

    /// Before OBS has answered, "try one of: " with nothing after it would be
    /// worse than saying plainly that nothing is known yet.
    #[test]
    fn resolving_before_obs_has_answered_says_so() {
        let state = ObsState::default();
        let error = resolve_scene(&state, "cam").expect_err("nothing to resolve against");
        assert!(format!("{error}").contains("has not reported any scenes yet"));
    }

    #[test]
    fn a_transport_failure_is_told_apart_from_a_command_failure() {
        let transport = anyhow::Error::new(TransportGone).context("sending a request");
        assert!(is_transport_failure(&transport));

        let refused = anyhow::anyhow!("No source was found by the name of 'Mic'");
        assert!(!is_transport_failure(&refused));
    }

    /// The backoff has to grow and then stop growing: without a ceiling, a
    /// machine that never runs OBS would end up retrying once an hour.
    #[test]
    fn the_reconnect_delay_backs_off_to_a_ceiling() {
        let mut delay = RECONNECT_INITIAL;
        let mut seen = vec![delay];
        for _ in 0..10 {
            delay = (delay * 2).min(RECONNECT_MAX);
            seen.push(delay);
        }
        assert!(
            seen.windows(2).all(|pair| pair[1] >= pair[0]),
            "must not shrink"
        );
        assert_eq!(*seen.last().expect("a last delay"), RECONNECT_MAX);
    }

    // -----------------------------------------------------------------------
    // A fake OBS, so the handshake and the request/response machinery are
    // exercised for real rather than assumed. It speaks just enough of
    // obs-websocket 5 to be indistinguishable from the real thing for the
    // things this program does.
    // -----------------------------------------------------------------------

    use tokio::net::TcpListener;

    /// Start a fake OBS on a port the operating system picks, and return the
    /// URL to reach it on.
    ///
    /// `password` being `Some` makes it demand authentication, exactly as OBS
    /// does when one is set in its settings window.
    async fn fake_obs(password: Option<&'static str>) -> (String, tokio::task::JoinHandle<()>) {
        // Port zero asks the operating system for a free one, so tests can
        // run in parallel and on a machine that happens to run real OBS.
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("a free port");
        let port = listener.local_addr().expect("an address").port();

        let handle = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let password = password;
                tokio::spawn(async move {
                    let Ok(ws) = tokio_tungstenite::accept_async(stream).await else {
                        return;
                    };
                    let _ = serve_fake_obs(ws, password).await;
                });
            }
        });

        (format!("ws://127.0.0.1:{port}"), handle)
    }

    async fn serve_fake_obs(
        ws: tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
        password: Option<&str>,
    ) -> Result<()> {
        use futures_util::SinkExt as _;
        let (mut tx, mut rx) = ws.split();

        let authentication = password.map(|_| {
            serde_json::json!({
                "challenge": "challenge-value",
                "salt": "salt-value",
            })
        });
        let hello = serde_json::json!({
            "op": 0,
            "d": {
                "obsWebSocketVersion": "5.5.4",
                "rpcVersion": 1,
                "authentication": authentication,
            }
        });
        tx.send(WsMessage::Text(hello.to_string())).await.ok();

        // The Identify, which is checked when a password is in play.
        let Some(Ok(WsMessage::Text(identify))) = rx.next().await else {
            return Ok(());
        };
        let identify: serde_json::Value = serde_json::from_str(&identify).expect("valid JSON");
        if let Some(password) = password {
            let expected = super::super::auth::compute(password, "salt-value", "challenge-value");
            if identify["d"]["authentication"].as_str() != Some(expected.as_str()) {
                // What OBS does with a wrong password: close, without
                // answering.
                return Ok(());
            }
        }
        tx.send(WsMessage::Text(
            serde_json::json!({ "op": 2, "d": { "negotiatedRpcVersion": 1 } }).to_string(),
        ))
        .await
        .ok();

        // Answer requests for as long as the client is there.
        while let Some(Ok(WsMessage::Text(raw))) = rx.next().await {
            let message: serde_json::Value = match serde_json::from_str(&raw) {
                Ok(message) => message,
                Err(_) => continue,
            };
            if message["op"].as_u64() != Some(6) {
                continue;
            }
            let request_type = message["d"]["requestType"].as_str().unwrap_or_default();
            let request_id = message["d"]["requestId"].as_str().unwrap_or_default();

            let data = match request_type {
                "GetVersion" => serde_json::json!({
                    "obsVersion": "30.1.2",
                    "obsWebSocketVersion": "5.5.4",
                }),
                "GetSceneList" => serde_json::json!({
                    "currentProgramSceneName": "Main",
                    // OBS lists scenes bottom-first; the client reverses them.
                    "scenes": [
                        { "sceneName": "Break" },
                        { "sceneName": "Main" },
                    ],
                }),
                "GetInputList" => serde_json::json!({
                    "inputs": [
                        { "inputName": "Mic/Aux", "inputKind": "pulse_input_capture" },
                        { "inputName": "Logo", "inputKind": "image_source" },
                    ],
                }),
                "GetInputMute" => serde_json::json!({ "inputMuted": false }),
                "GetInputVolume" => {
                    serde_json::json!({ "inputVolumeMul": 0.75, "inputVolumeDb": -2.5 })
                }
                "GetStreamStatus" => serde_json::json!({
                    "outputActive": true,
                    "outputDuration": 60_000.0,
                    "outputBytes": 45_000_000.0,
                }),
                "GetRecordStatus" => serde_json::json!({
                    "outputActive": false,
                    "outputPaused": false,
                    "outputDuration": 0.0,
                }),
                "GetStats" => serde_json::json!({
                    "cpuUsage": 12.5,
                    "memoryUsage": 500.0,
                    "availableDiskSpace": 100_000.0,
                    "activeFps": 60.0,
                    "averageFrameRenderTime": 3.2,
                    "renderSkippedFrames": 1.0,
                    "renderTotalFrames": 1000.0,
                    "outputSkippedFrames": 0.0,
                    "outputTotalFrames": 900.0,
                }),
                "GetProfileList" => serde_json::json!({
                    "currentProfileName": "Streaming",
                    "profiles": ["Streaming", "Recording"],
                }),
                "GetSceneCollectionList" => serde_json::json!({
                    "currentSceneCollectionName": "Default",
                    "sceneCollections": ["Default", "Podcast"],
                }),
                "SetCurrentProgramScene" => {
                    // Answer, then announce it the way OBS does — which is
                    // how the pane learns that the switch actually happened.
                    let scene = message["d"]["requestData"]["sceneName"].clone();
                    let response = serde_json::json!({
                        "op": 7,
                        "d": {
                            "requestType": request_type,
                            "requestId": request_id,
                            "requestStatus": { "result": true, "code": 100 },
                            "responseData": null,
                        }
                    });
                    tx.send(WsMessage::Text(response.to_string())).await.ok();
                    let event = serde_json::json!({
                        "op": 5,
                        "d": {
                            "eventType": "CurrentProgramSceneChanged",
                            "eventData": { "sceneName": scene },
                        }
                    });
                    tx.send(WsMessage::Text(event.to_string())).await.ok();
                    continue;
                }
                // Anything not modelled is refused the way OBS refuses a
                // request naming something that does not exist.
                _ => {
                    let response = serde_json::json!({
                        "op": 7,
                        "d": {
                            "requestType": request_type,
                            "requestId": request_id,
                            "requestStatus": {
                                "result": false,
                                "code": 600,
                                "comment": "No source was found by the name of 'nope'",
                            },
                            "responseData": null,
                        }
                    });
                    tx.send(WsMessage::Text(response.to_string())).await.ok();
                    continue;
                }
            };

            let response = serde_json::json!({
                "op": 7,
                "d": {
                    "requestType": request_type,
                    "requestId": request_id,
                    "requestStatus": { "result": true, "code": 100 },
                    "responseData": data,
                }
            });
            tx.send(WsMessage::Text(response.to_string())).await.ok();
        }
        Ok(())
    }

    fn params(url: String, password: Option<&str>) -> Params {
        Params {
            url,
            password: password.map(str::to_string),
            scene_labels: HashMap::new(),
            audio_labels: HashMap::new(),
        }
    }

    /// Wait for the first snapshot, or give up. Anything slower than this on
    /// a loopback connection means something is wrong rather than slow.
    async fn first_snapshot(
        updates: &mut mpsc::UnboundedReceiver<Update>,
    ) -> Option<Box<ObsState>> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return None;
            }
            match tokio::time::timeout(remaining, updates.recv()).await {
                Ok(Some(Update::Snapshot(state))) => return Some(state),
                Ok(Some(_)) => continue,
                Ok(None) | Err(_) => return None,
            }
        }
    }

    /// The whole path: connect, handshake, ask for everything, and build a
    /// picture of OBS from the answers.
    #[tokio::test]
    async fn connecting_to_obs_produces_a_complete_picture_of_it() {
        let (url, server) = fake_obs(None).await;
        let (tx, mut updates) = mpsc::unbounded_channel();
        let handle = spawn(params(url, None), tx);

        let state = first_snapshot(&mut updates).await.expect("a snapshot");

        assert_eq!(state.obs_version.as_deref(), Some("30.1.2"));
        // Scenes come back in the order OBS shows them, which is the reverse
        // of the order it sends them in.
        assert_eq!(
            state
                .scenes
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>(),
            vec!["Main", "Break"]
        );
        assert_eq!(state.current_scene.as_deref(), Some("Main"));

        // Only the audio input, not the image source beside it.
        assert_eq!(state.audio.len(), 1);
        assert_eq!(state.audio[0].name, "Mic/Aux");
        assert_eq!(state.audio[0].muted, Some(false));
        assert_eq!(state.audio[0].volume_percent(), Some(75));

        assert!(state.streaming);
        assert!(!state.recording);
        assert_eq!(state.current_profile.as_deref(), Some("Streaming"));
        assert_eq!(state.scene_collections, vec!["Default", "Podcast"]);
        assert_eq!(state.stats.expect("stats").active_fps, 60.0);

        drop(handle.commands);
        server.abort();
    }

    /// The password is proved rather than sent, and the answer has to match
    /// what OBS computes or nothing works at all.
    #[tokio::test]
    async fn a_password_protected_obs_accepts_the_right_answer() {
        let (url, server) = fake_obs(Some("hunter2")).await;
        let (tx, mut updates) = mpsc::unbounded_channel();
        let handle = spawn(params(url, Some("hunter2")), tx);

        let state = first_snapshot(&mut updates).await.expect("a snapshot");
        assert_eq!(state.obs_version.as_deref(), Some("30.1.2"));

        drop(handle.commands);
        server.abort();
    }

    /// A wrong password must be reported as such rather than retried
    /// silently forever, since no amount of retrying will fix it.
    #[tokio::test]
    async fn a_wrong_password_is_reported() {
        let (url, server) = fake_obs(Some("hunter2")).await;
        let (tx, mut updates) = mpsc::unbounded_channel();
        let handle = spawn(params(url, Some("wrong")), tx);

        let mut reported = None;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while tokio::time::Instant::now() < deadline {
            match tokio::time::timeout(Duration::from_secs(2), updates.recv()).await {
                Ok(Some(Update::Connection(Connection::Failed(reason)))) => {
                    reported = Some(reason);
                    break;
                }
                Ok(Some(_)) => continue,
                _ => break,
            }
        }

        let reported = reported.expect("a failure is reported");
        assert!(reported.contains("password"), "got {reported}");

        drop(handle.commands);
        server.abort();
    }

    /// Switching a scene has to reach OBS, and the event OBS sends back has
    /// to update what the pane shows.
    #[tokio::test]
    async fn switching_a_scene_reaches_obs_and_comes_back_as_an_event() {
        let (url, server) = fake_obs(None).await;
        let (tx, mut updates) = mpsc::unbounded_channel();
        let handle = spawn(params(url, None), tx);

        first_snapshot(&mut updates)
            .await
            .expect("a first snapshot");
        handle
            .commands
            .send(Command::SetScene("Break".to_string()))
            .await
            .expect("the command is accepted");

        let mut switched = false;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while tokio::time::Instant::now() < deadline {
            match tokio::time::timeout(Duration::from_secs(2), updates.recv()).await {
                Ok(Some(Update::Event(Event::SceneChanged { scene }))) if scene == "Break" => {
                    switched = true;
                    break;
                }
                Ok(Some(_)) => continue,
                _ => break,
            }
        }
        assert!(switched, "the scene change was never announced");

        drop(handle.commands);
        server.abort();
    }

    /// A command naming something that does not exist is reported, and the
    /// connection carries on — a mistyped scene name is not a broken socket.
    #[tokio::test]
    async fn a_refused_command_is_reported_without_dropping_the_connection() {
        let (url, server) = fake_obs(None).await;
        let (tx, mut updates) = mpsc::unbounded_channel();
        let handle = spawn(params(url, None), tx);

        first_snapshot(&mut updates)
            .await
            .expect("a first snapshot");
        handle
            .commands
            .send(Command::SetScene("no such scene".to_string()))
            .await
            .expect("the command is accepted");

        let mut failure = None;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while tokio::time::Instant::now() < deadline {
            match tokio::time::timeout(Duration::from_secs(2), updates.recv()).await {
                Ok(Some(Update::CommandFailed(reason))) => {
                    failure = Some(reason);
                    break;
                }
                Ok(Some(_)) => continue,
                _ => break,
            }
        }
        let failure = failure.expect("the refusal is reported");
        assert!(failure.contains("no such scene"), "got {failure}");

        // And the connection is still up: another command still works.
        handle
            .commands
            .send(Command::Refresh)
            .await
            .expect("the connection is still there");
        assert!(
            first_snapshot(&mut updates).await.is_some(),
            "the connection should have survived a refused command"
        );

        drop(handle.commands);
        server.abort();
    }

    /// Nothing to connect to is the ordinary case for most people. It must
    /// not panic, and it must keep trying rather than giving up.
    #[tokio::test]
    async fn no_obs_at_all_is_reported_and_retried() {
        // Port 1 is reserved and nothing listens on it.
        let (tx, mut updates) = mpsc::unbounded_channel();
        let handle = spawn(params("ws://127.0.0.1:1".to_string(), None), tx);

        let mut failures = 0;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while tokio::time::Instant::now() < deadline && failures < 1 {
            if let Ok(Some(Update::Connection(Connection::Failed(_)))) =
                tokio::time::timeout(Duration::from_secs(3), updates.recv()).await
            {
                failures += 1;
            }
        }
        assert!(failures >= 1, "a failure to connect should be reported");

        drop(handle.commands);
    }
}
