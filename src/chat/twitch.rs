//! Twitch chat adapter: the `twitch-irc` crate does the wire work, this module
//! ports twi's app-level behavior on top of it.
//!
//! Provenance: ported from the Go reference `twi` (github.com/worxbend/twi,
//! commit 7c6ad6b), files `internal/twitch/irc_client.go` (connect / auth
//! refresh / send entry points), `internal/twitch/irc.go` (event
//! normalization), `internal/twitch/rate_limit.go` (send allowance),
//! `internal/twitch/irc_sanitize.go` (outbound sanitizing), and
//! `internal/app/live_chat.go` (reconnect backoff, auth-notice
//! classification, local echo from USERSTATE).
//!
//! Division of labor, mirroring twi (which sits on gempir/go-twitch-irc):
//! the crate owns TLS, IRC parsing, PING/PONG, and channel membership; this
//! module owns reconnect policy, send limits, sanitizing, normalization into
//! [`ChatMessage`], and local echo. One deliberate difference from twi: the
//! crate hard-codes its capability request to `twitch.tv/tags` and
//! `twitch.tv/commands` (no `twitch.tv/membership`), so JOIN/PART roster
//! events are unavailable through it. Recorded as a deviation; the roster
//! feature (PLAN.md T21) will need either a crate change or speaker-only data.
//!
//! Local echo is load-bearing, not cosmetic: Twitch never echoes your own
//! PRIVMSG back to you, so the message built here from the remembered
//! USERSTATE identity is the only copy of your message you will ever see.

use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use tokio::sync::mpsc;
use twitch_irc::login::StaticLoginCredentials;
use twitch_irc::message::{
    ClearChatAction, ClearChatMessage, ClearMsgMessage, NoticeMessage, PrivmsgMessage, RGBColor,
    ServerMessage, UserNoticeMessage, UserStateMessage,
};
use twitch_irc::{ClientConfig, SecureTCPTransport, TwitchIRCClient};
use unicode_segmentation::UnicodeSegmentation;

use crate::chat::ratelimit::{
    DuplicateSuppressor, SendDenied, SlidingWindow, TWITCH_SEND_LIMIT, TWITCH_SEND_WINDOW,
};
use crate::chat::source::{ChatCommand, ChatHandle, EventSender, TokenProvider, COMMAND_QUEUE};
use crate::chat::{
    Badge, ChatAuthor, ChatEvent, ChatKey, ChatMessage, ConnectionStatus, MessageKind,
    PlatformMeta, ReplyContext, TwitchMeta,
};

type Client = TwitchIRCClient<SecureTCPTransport, StaticLoginCredentials>;

/// Twitch's per-message length limit, counted here in grapheme clusters so a
/// truncation can never split a composed emoji or a combining sequence.
/// (twi counts runes; graphemes are the stricter, safer unit.)
pub const MAX_MESSAGE_GRAPHEMES: usize = 500;

/// The CTCP delimiter that frames a `/me` ACTION. The one C0 control that is
/// meaningful in chat, so sanitizing preserves it while dropping the rest.
const CTCP_DELIMITER: char = '\u{1}';

/// twi's app-level reconnect ladder (live_chat.go): 2s doubling to 60s, ten
/// attempts, then give up and wait for a manual reconnect.
const RECONNECT_INITIAL: Duration = Duration::from_secs(2);
const RECONNECT_MAX_DELAY: Duration = Duration::from_secs(60);
pub(crate) const RECONNECT_MAX_ATTEMPTS: u32 = 10;

/// The NOTICE bodies Twitch sends when login credentials are rejected. These
/// notices carry no msg-id tag, so the text is the only signal (twi:
/// live_chat.go `authNoticeTexts`). Every *tagged* notice — msg_banned,
/// no_permission, and the rest — describes channel state on a connection that
/// authenticated fine, and must not trigger a token refresh.
const AUTH_NOTICE_TEXTS: [&str; 3] = [
    "login authentication failed",
    "login unsuccessful",
    "improperly formatted auth",
];

/// Spawn the chat task for one Twitch channel. `key.target` must already be
/// the lowercase channel login; `account_login` is the authenticated user the
/// connection speaks as (and the local-echo fallback display name).
/// Everything one Twitch chat task needs beyond the channel itself: the
/// Helix pieces exist only for `/clip`.
pub struct TwitchParams {
    pub key: ChatKey,
    /// The authenticated account's login (local-echo fallback name).
    pub account_login: String,
    /// The authenticated account's user id — the broadcaster id `/clip`
    /// clips. Empty disables clipping with an explanatory notice.
    pub account_user_id: String,
    /// The app's Twitch client id, required in every Helix request header.
    pub client_id: String,
    pub tokens: TokenProvider,
    pub events: EventSender,
    pub http: reqwest::Client,
}

pub fn spawn(params: TwitchParams) -> ChatHandle {
    let TwitchParams {
        key,
        account_login,
        account_user_id,
        client_id,
        tokens,
        events,
        http,
    } = params;
    let (commands, command_rx) = mpsc::channel(COMMAND_QUEUE);
    let state = TaskState {
        channel: key.target.clone(),
        key: key.clone(),
        account_login,
        account_user_id,
        client_id,
        http,
        tokens,
        events,
        window: SlidingWindow::twitch(),
        dupes: DuplicateSuppressor::new(),
        identity: None,
        echo_counter: 0,
    };
    let task = tokio::spawn(run(state, command_rx));
    ChatHandle {
        key,
        commands,
        task,
    }
}

/// Everything the running task owns. Kept apart from the pure normalizers
/// below so those stay testable without a network or a runtime.
struct TaskState {
    key: ChatKey,
    channel: String,
    account_login: String,
    /// See [`TwitchParams::account_user_id`].
    account_user_id: String,
    client_id: String,
    http: reqwest::Client,
    tokens: TokenProvider,
    events: EventSender,
    window: SlidingWindow,
    dupes: DuplicateSuppressor,
    /// Our own identity as last reported by USERSTATE, used to build local
    /// echoes. `None` until the first USERSTATE of the session arrives.
    identity: Option<EchoIdentity>,
    echo_counter: u64,
}

impl TaskState {
    fn emit(&self, event: ChatEvent) {
        // A send error means the UI dropped its receiver; the task will end
        // when the command channel closes, so nothing to do here.
        let _ = self.events.send((self.key.clone(), event));
    }

    fn emit_status(&self, status: ConnectionStatus, detail: impl Into<String>) {
        self.emit(ChatEvent::Connection {
            status,
            detail: detail.into(),
        });
    }

    /// `/clip` (twi: internal/app/clip.go): Helix Create Clip on the
    /// authenticated user's *own* stream — the API clips a broadcaster, and
    /// the token can only clip channels it may; twi clips self, so do we.
    /// Needs the `clips:edit` scope; logins predating it get told to
    /// re-login. A 404 means "you are not live", per twi.
    async fn handle_clip(&self) {
        if self.account_user_id.is_empty() {
            self.emit_notice(
                "clipping needs to know your user id; re-run `msm login twitch` \
                 to refresh the saved account identity",
            );
            return;
        }
        let token = match (self.tokens)().await {
            Ok(token) => token,
            Err(err) => {
                self.emit_notice(format!(
                    "could not get a Twitch token for the clip: {err:#}"
                ));
                return;
            }
        };
        let url = format!(
            "https://api.twitch.tv/helix/clips?broadcaster_id={}",
            urlencoding::encode(&self.account_user_id)
        );
        let sent = self
            .http
            .post(&url)
            .header("Client-Id", &self.client_id)
            .bearer_auth(&token)
            .send()
            .await;
        let response = match sent {
            Ok(response) => response,
            Err(_) => {
                self.emit_notice("could not reach Twitch to create the clip");
                return;
            }
        };
        match response.status().as_u16() {
            200..=299 => {
                #[derive(serde::Deserialize, Default)]
                struct ClipData {
                    #[serde(default)]
                    data: Vec<ClipEntry>,
                }
                #[derive(serde::Deserialize, Default)]
                struct ClipEntry {
                    #[serde(default)]
                    edit_url: String,
                }
                let body = response.json::<ClipData>().await.unwrap_or_default();
                match body.data.first() {
                    Some(entry) if !entry.edit_url.is_empty() => self.emit_notice(format!(
                        "clip created — trim and publish it at {}",
                        entry.edit_url
                    )),
                    _ => self.emit_notice("clip created"),
                }
            }
            401 | 403 => self.emit_notice(
                "your Twitch login is missing the clips:edit permission; \
                 run `msm login twitch` again to grant it",
            ),
            404 => self.emit_notice("clips aren't available: you are not currently live"),
            status => self.emit_notice(format!("clip failed (HTTP {status})")),
        }
    }

    fn emit_notice(&self, text: impl Into<String>) {
        self.emit(ChatEvent::Message(Box::new(notice_row(
            String::new(),
            text.into(),
            Some(Utc::now()),
        ))));
    }
}

/// What ended a wait-for-manual-reconnect park.
enum ParkOutcome {
    Reconnect,
    Shutdown,
}

/// Wait until the user asks for a reconnect (or the handle is dropped).
/// Sends attempted while parked are declined with a notice that names the
/// way out, so the composer can restore the draft.
async fn park(commands: &mut mpsc::Receiver<ChatCommand>, state: &TaskState) -> ParkOutcome {
    loop {
        match commands.recv().await {
            None => return ParkOutcome::Shutdown,
            Some(ChatCommand::Reconnect) => return ParkOutcome::Reconnect,
            Some(ChatCommand::Delete { .. }) | Some(ChatCommand::Ban { .. }) => {
                state.emit_notice(
                    "moderation from here works for YouTube chats only; \
                     use the Twitch mod tools for this channel",
                );
            }
            Some(ChatCommand::Clip) => state.handle_clip().await,
            Some(ChatCommand::Send { .. }) => {
                state.emit_notice("not connected to Twitch chat; press ctrl+r to reconnect");
            }
        }
    }
}

async fn run(mut state: TaskState, mut commands: mpsc::Receiver<ChatCommand>) {
    // Completed failed attempts since the last successful registration.
    let mut attempt: u32 = 0;
    // One token re-fetch per outage (twi refreshes once per connect attempt):
    // when Twitch rejects the login, we reconnect immediately with a fresh
    // token; if that fresh token is rejected too, retrying would spin.
    let mut auth_refresh_available = true;

    'connect: loop {
        if attempt == 0 {
            state.emit_status(ConnectionStatus::Connecting, "");
        } else {
            let Some(delay) = reconnect_delay(attempt) else {
                state.emit_status(
                    ConnectionStatus::Failed,
                    "automatic reconnect gave up; press ctrl+r",
                );
                match park(&mut commands, &state).await {
                    ParkOutcome::Shutdown => return,
                    ParkOutcome::Reconnect => {
                        attempt = 0;
                        auth_refresh_available = true;
                        continue 'connect;
                    }
                }
            };
            state.emit_status(
                ConnectionStatus::Reconnecting,
                format!(
                    "retry {attempt} of {RECONNECT_MAX_ATTEMPTS} in {}s",
                    delay.as_secs()
                ),
            );
            // Sleep, but stay responsive: a manual reconnect skips the wait,
            // and a dropped handle must still end the task promptly.
            let sleep = tokio::time::sleep(delay);
            tokio::pin!(sleep);
            loop {
                tokio::select! {
                    _ = &mut sleep => break,
                    cmd = commands.recv() => match cmd {
                        None => return,
                        Some(ChatCommand::Reconnect) => {
                            attempt = 0;
                            auth_refresh_available = true;
                            continue 'connect;
                        }
                        Some(ChatCommand::Send { .. }) => {
                            state.emit_notice(
                                "not connected to Twitch chat; reconnecting — try again shortly",
                            );
                        }
                        Some(ChatCommand::Delete { .. }) | Some(ChatCommand::Ban { .. }) => {
                            state.emit_notice(
                                "moderation from here works for YouTube chats only; \
                                 use the Twitch mod tools for this channel",
                            );
                        }
                        Some(ChatCommand::Clip) => state.handle_clip().await,
                    }
                }
            }
        }

        // A fresh token every (re)connect is what makes the crate's static
        // credentials safe here: a token that expired mid-session is replaced
        // the moment the reconnect path runs.
        let token = match (state.tokens)().await {
            Ok(token) => token,
            Err(err) => {
                state.emit_status(
                    ConnectionStatus::Disconnected,
                    format!("could not get a Twitch access token: {err:#}"),
                );
                attempt = attempt.saturating_add(1);
                continue 'connect;
            }
        };
        // The crate prepends `oauth:` itself; a stored token that already
        // carries the prefix would otherwise be sent as `oauth:oauth:...`.
        let token = token.strip_prefix("oauth:").unwrap_or(&token).to_string();

        let config = ClientConfig::new_simple(StaticLoginCredentials::new(
            state.account_login.clone(),
            Some(token),
        ));
        let (mut incoming, client) = Client::new(config);
        if let Err(err) = client.join(state.channel.clone()) {
            // The crate validates channel logins before joining; a rejected
            // name will never succeed on retry, so this parks rather than
            // burning reconnect attempts on it.
            state.emit_status(
                ConnectionStatus::Failed,
                format!(
                    "Twitch refused the channel name {:?} ({err}); check the spelling and press ctrl+r",
                    state.channel
                ),
            );
            match park(&mut commands, &state).await {
                ParkOutcome::Shutdown => return,
                ParkOutcome::Reconnect => {
                    attempt = 0;
                    auth_refresh_available = true;
                    continue 'connect;
                }
            }
        }

        // "Connected" is announced on the first server message rather than
        // after join(): the crate connects lazily, and the first thing a dead
        // login produces is the auth-failure NOTICE handled below.
        let mut connected = false;
        loop {
            tokio::select! {
                cmd = commands.recv() => match cmd {
                    None => return,
                    Some(ChatCommand::Reconnect) => {
                        attempt = 0;
                        auth_refresh_available = true;
                        state.emit_status(ConnectionStatus::Reconnecting, "manual reconnect");
                        continue 'connect;
                    }
                    Some(ChatCommand::Send { text, reply_to }) => {
                        state.handle_send(&client, connected, text, reply_to).await;
                    }
                    Some(ChatCommand::Delete { .. }) | Some(ChatCommand::Ban { .. }) => {
                        // Deliberate parity with twi, which performs no
                        // client-side moderation: Twitch removed moderation
                        // commands from IRC in 2023, and the Helix moderation
                        // endpoints are outside this chat transport's scope.
                        state.emit_notice(
                            "moderation from here works for YouTube chats only; \
                             use the Twitch mod tools for this channel",
                        );
                    }
                    Some(ChatCommand::Clip) => state.handle_clip().await,
                },
                msg = incoming.recv() => match msg {
                    None => {
                        state.emit_status(ConnectionStatus::Disconnected, "connection lost");
                        attempt = attempt.saturating_add(1);
                        continue 'connect;
                    }
                    Some(msg) => {
                        if let ServerMessage::Notice(notice) = &msg {
                            if is_auth_failure_notice(&notice.message_text) {
                                if auth_refresh_available {
                                    auth_refresh_available = false;
                                    state.emit_status(
                                        ConnectionStatus::Reconnecting,
                                        "Twitch rejected the login token; fetching a fresh one",
                                    );
                                    attempt = 0;
                                    continue 'connect;
                                }
                                state.emit_status(
                                    ConnectionStatus::Failed,
                                    "Twitch rejected the login token even after refreshing it; \
                                     run `msm login twitch` to sign in again, then press ctrl+r",
                                );
                                match park(&mut commands, &state).await {
                                    ParkOutcome::Shutdown => return,
                                    ParkOutcome::Reconnect => {
                                        attempt = 0;
                                        auth_refresh_available = true;
                                        continue 'connect;
                                    }
                                }
                            }
                        }
                        if !connected {
                            connected = true;
                            attempt = 0;
                            auth_refresh_available = true;
                            state.emit_status(ConnectionStatus::Connected, "");
                        }
                        state.handle_message(msg);
                    }
                }
            }
        }
    }
}

impl TaskState {
    fn handle_message(&mut self, msg: ServerMessage) {
        match msg {
            ServerMessage::Privmsg(m) => {
                self.emit(ChatEvent::Message(Box::new(normalize_privmsg(&m))));
            }
            ServerMessage::UserNotice(m) => {
                self.emit(ChatEvent::Message(Box::new(normalize_usernotice(&m))));
            }
            ServerMessage::ClearChat(m) => self.emit(normalize_clearchat(&m)),
            ServerMessage::ClearMsg(m) => self.emit(normalize_clearmsg(&m)),
            ServerMessage::Notice(m) => {
                // Auth failures were intercepted in the session loop; what is
                // left is channel/account state ("slow mode is on", ...).
                self.emit(ChatEvent::Message(Box::new(normalize_notice(&m))));
            }
            ServerMessage::UserState(m) => {
                // Emits nothing: USERSTATE is Twitch telling *us* who we are
                // in this channel, and it only matters for local echo.
                self.identity = Some(remember_userstate(&m));
            }
            other => {
                // ROOMSTATE, GLOBALUSERSTATE, JOIN/PART, PING/PONG, RECONNECT
                // and unrecognized lines: nothing to render, but worth a debug
                // line when diagnosing a misbehaving channel.
                tracing::debug!(channel = %self.channel, message = ?other, "ignoring Twitch IRC message");
            }
        }
    }

    async fn handle_send(
        &mut self,
        client: &Client,
        connected: bool,
        text: String,
        reply_to: Option<String>,
    ) {
        if !connected {
            self.emit_notice("not connected to Twitch chat; press ctrl+r to reconnect");
            return;
        }
        let trimmed = text.trim();
        if trimmed.is_empty() {
            self.emit_notice("cannot send an empty message");
            return;
        }
        // `/me` becomes an ACTION; the crate does the CTCP framing.
        let (body, is_action) = match trimmed.strip_prefix("/me") {
            Some(rest) if rest.is_empty() || rest.starts_with(' ') => (rest.trim_start(), true),
            _ => (trimmed, false),
        };
        let wire = sanitize_message(body);
        // The crate frames a `/me` as "\u{1}ACTION <body>\u{1}" *after* this
        // cap, and Twitch's 500-character limit applies to the framed wire
        // text (twi caps after framing for the same reason). Without this, a
        // maximal /me went out at up to 509 characters — silently dropped by
        // the server while the local echo made it look delivered.
        let wire = if is_action {
            cap_graphemes(&wire, MAX_MESSAGE_GRAPHEMES - ACTION_FRAMING_GRAPHEMES)
        } else {
            wire
        };
        if wire.trim().is_empty() {
            self.emit_notice("cannot send an empty message");
            return;
        }

        let now = Instant::now();
        // Window first, duplicates second. The window check records the send
        // on success, so checking duplicates first would poison the duplicate
        // map on a rate-limited attempt and then refuse the user's retry of
        // the very same text. Wasting one window slot on a duplicate attempt
        // is the cheaper mistake.
        if let Err(denied) = self.window.acquire(now) {
            let wait = match denied {
                SendDenied::RateLimited(wait) => wait,
                // The window can only refuse for rate, never for duplication.
                SendDenied::Duplicate => Duration::ZERO,
            };
            self.emit_notice(format!(
                "sending too fast: Twitch allows {TWITCH_SEND_LIMIT} messages per {}s; \
                 wait {}s and try again",
                TWITCH_SEND_WINDOW.as_secs(),
                wait.as_secs().max(1),
            ));
            return;
        }
        // A reply to a different message is not a duplicate even with
        // identical text, so the parent id is folded into the key (twi:
        // irc_client.go Reply).
        let dupe_target = match &reply_to {
            Some(parent) => format!("{}\0{}", self.channel, parent),
            None => self.channel.clone(),
        };
        if self.dupes.acquire(&dupe_target, &wire, now).is_err() {
            self.emit_notice(format!(
                "Twitch rejects identical repeated messages; wait {}s or change the text",
                crate::chat::ratelimit::DUPLICATE_WINDOW.as_secs(),
            ));
            return;
        }

        let result = match (&reply_to, is_action) {
            (Some(parent), true) => {
                client
                    .me_in_reply_to(&(self.channel.as_str(), parent.as_str()), wire.clone())
                    .await
            }
            (Some(parent), false) => {
                client
                    .say_in_reply_to(&(self.channel.as_str(), parent.as_str()), wire.clone())
                    .await
            }
            (None, true) => client.me(self.channel.clone(), wire.clone()).await,
            (None, false) => client.say(self.channel.clone(), wire.clone()).await,
        };
        if let Err(err) = result {
            self.emit_notice(format!("send failed: {err}; the message was not delivered"));
            return;
        }

        // Twitch never echoes your own PRIVMSG back, so this locally built
        // message is the only way the sender ever sees what they said.
        self.echo_counter += 1;
        let echo = build_local_echo(
            self.identity.as_ref(),
            &self.account_login,
            &wire,
            is_action,
            self.echo_counter,
            Some(Utc::now()),
        );
        self.emit(ChatEvent::Message(Box::new(echo)));
    }
}

/// The identity USERSTATE reported for the authenticated user in this
/// channel; the source of a local echo's badges, name, and color.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct EchoIdentity {
    pub display_name: String,
    pub badges: Vec<Badge>,
    pub color_hint: Option<String>,
}

/// Delay before reconnect attempt `attempt` (1-based). `None` once the
/// attempt budget is spent — the caller must stop and wait for a manual
/// reconnect. Sequence: 2, 4, 8, 16, 32, then 60 for the rest.
pub(crate) fn reconnect_delay(attempt: u32) -> Option<Duration> {
    if attempt == 0 || attempt > RECONNECT_MAX_ATTEMPTS {
        return None;
    }
    // Capping the exponent before shifting keeps the arithmetic in range for
    // any attempt count; the min() below applies the actual 60s ceiling.
    let doubled = RECONNECT_INITIAL * 2u32.pow(attempt.min(6) - 1);
    Some(doubled.min(RECONNECT_MAX_DELAY))
}

/// Make text safe to put on the wire as a single IRC message
/// (twi: irc_sanitize.go `sanitizeIRCText`).
///
/// IRC frames messages with CRLF, so a carriage return or newline inside the
/// text would end the message early and the remainder would be parsed as a
/// new IRC command — letting pasted text issue arbitrary commands as the
/// authenticated user. Both become spaces so the visible message survives.
/// Other C0 controls and DEL are dropped (not typeable, interpreted by
/// terminals downstream); the CTCP delimiter survives because `/me` is built
/// from it. The result is capped at Twitch's length limit on grapheme
/// boundaries, and a CTCP-framed ACTION keeps its closing delimiter across
/// truncation.
pub fn sanitize_message(text: &str) -> String {
    let mut cleaned = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '\r' | '\n' => cleaned.push(' '),
            CTCP_DELIMITER => cleaned.push(ch),
            c if (c as u32) < 0x20 || c == '\u{7f}' => {}
            c => cleaned.push(c),
        }
    }
    truncate_graphemes(&cleaned)
}

/// The CTCP ACTION wrapper the transport adds around a `/me` body —
/// `\u{1}ACTION ` before and `\u{1}` after, nine characters that count
/// toward Twitch's limit like any others.
const ACTION_FRAMING_GRAPHEMES: usize = 9;

/// Cap `text` at `limit` grapheme clusters.
fn cap_graphemes(text: &str, limit: usize) -> String {
    let graphemes: Vec<&str> = text.graphemes(true).collect();
    if graphemes.len() <= limit {
        text.to_string()
    } else {
        graphemes[..limit].concat()
    }
}

fn truncate_graphemes(text: &str) -> String {
    let graphemes: Vec<&str> = text.graphemes(true).collect();
    if graphemes.len() <= MAX_MESSAGE_GRAPHEMES {
        return text.to_string();
    }
    let ctcp = CTCP_DELIMITER.to_string();
    let action = graphemes.first() == Some(&ctcp.as_str())
        && graphemes.last() == Some(&ctcp.as_str())
        && graphemes.len() > 1;
    if !action {
        return graphemes[..MAX_MESSAGE_GRAPHEMES].concat();
    }
    // Losing the closing delimiter would make every client render the raw
    // CTCP text, so truncation leaves room to close the wrapper.
    let mut out = graphemes[..MAX_MESSAGE_GRAPHEMES - 1].concat();
    out.push(CTCP_DELIMITER);
    out
}

/// Whether a NOTICE body means the credentials were rejected
/// (twi: live_chat.go `isAuthNotice`).
pub(crate) fn is_auth_failure_notice(text: &str) -> bool {
    let text = text.trim().to_lowercase();
    if text.is_empty() {
        return false;
    }
    AUTH_NOTICE_TEXTS.iter().any(|known| text.contains(known))
}

/// PRIVMSG → normalized chat row (twi: irc.go `normalizeIRCChatMessage`).
pub(crate) fn normalize_privmsg(msg: &PrivmsgMessage) -> ChatMessage {
    ChatMessage {
        id: msg.message_id.clone(),
        timestamp: Some(msg.server_timestamp),
        author: ChatAuthor {
            id: msg.sender.id.clone(),
            login: msg.sender.login.clone(),
            display_name: msg.sender.name.clone(),
            badges: merge_badges(&msg.badges, &msg.badge_info),
            color_hint: color_hex(msg.name_color),
        },
        text: msg.message_text.clone(),
        kind: if msg.is_action {
            MessageKind::Action
        } else {
            MessageKind::Chat
        },
        deleted: false,
        historical: false,
        local_echo: false,
        meta: Some(PlatformMeta::Twitch(TwitchMeta {
            bits: msg.bits.unwrap_or(0),
            // The typed struct has no first-msg field, so the raw tag is read
            // from the source message. Anything other than "1" — missing tag,
            // "0", or a future value — means "not first": over-highlighting
            // regulars is worse than missing the occasional newcomer.
            first_message: msg.source.tags.0.get("first-msg").map(String::as_str) == Some("1"),
            system_event: String::new(),
            reply: msg.reply_parent.as_ref().map(|reply| ReplyContext {
                parent_id: reply.message_id.clone(),
                parent_author: reply.reply_parent_user.name.clone(),
                parent_text: reply.message_text.clone(),
            }),
        })),
    }
}

/// USERNOTICE (sub, resub, raid, gift, announcement, …) → notice row
/// (twi: live_chat.go `messageFromUserNotice`). The rendered text is the
/// system message plus the user's optional accompanying message; the raw
/// msg-id rides in `system_event` so filters can find event classes.
pub(crate) fn normalize_usernotice(msg: &UserNoticeMessage) -> ChatMessage {
    let mut parts: Vec<&str> = Vec::with_capacity(2);
    let system = msg.system_message.trim();
    if !system.is_empty() {
        parts.push(system);
    }
    if let Some(user_text) = msg.message_text.as_deref() {
        let user_text = user_text.trim();
        if !user_text.is_empty() {
            parts.push(user_text);
        }
    }
    let text = if parts.is_empty() {
        // A notice with no text at all still deserves a readable row; the
        // event id ("sub", "raid", …) is the best available description.
        msg.event_id.clone()
    } else {
        parts.join(" ")
    };

    ChatMessage {
        id: msg.message_id.clone(),
        timestamp: Some(msg.server_timestamp),
        author: ChatAuthor {
            id: msg.sender.id.clone(),
            login: msg.sender.login.clone(),
            display_name: msg.sender.name.clone(),
            badges: merge_badges(&msg.badges, &msg.badge_info),
            color_hint: color_hex(msg.name_color),
        },
        text,
        kind: MessageKind::Notice,
        deleted: false,
        historical: false,
        local_echo: false,
        meta: Some(PlatformMeta::Twitch(TwitchMeta {
            system_event: msg.event_id.clone(),
            ..TwitchMeta::default()
        })),
    }
}

/// CLEARCHAT → moderation event (twi: irc.go `NormalizeIRCClearChatMessage`).
/// No target user means the whole chat was cleared; a target with a
/// ban-duration is a timeout; a target without one is a permanent ban.
pub(crate) fn normalize_clearchat(msg: &ClearChatMessage) -> ChatEvent {
    match &msg.action {
        ClearChatAction::ChatCleared => ChatEvent::ChatCleared,
        ClearChatAction::UserBanned { user_login, .. } => ChatEvent::UserPurged {
            author_login: user_login.clone(),
            timeout_secs: None,
        },
        ClearChatAction::UserTimedOut {
            user_login,
            timeout_length,
            ..
        } => ChatEvent::UserPurged {
            author_login: user_login.clone(),
            timeout_secs: Some(timeout_length.as_secs()),
        },
    }
}

/// CLEARMSG → single-message deletion. Only the id travels: the deleted text
/// must never be reprinted, so it is deliberately not forwarded.
pub(crate) fn normalize_clearmsg(msg: &ClearMsgMessage) -> ChatEvent {
    ChatEvent::MessageDeleted {
        message_id: msg.message_id.clone(),
    }
}

/// NOTICE → notice row (twi: live_chat.go `messageFromNotice`).
pub(crate) fn normalize_notice(msg: &NoticeMessage) -> ChatMessage {
    notice_row(
        msg.message_id.clone().unwrap_or_default(),
        msg.message_text.clone(),
        None,
    )
}

/// USERSTATE → the identity used for local echoes. Twitch sends USERSTATE
/// after join and after every message we send; it is the only source of our
/// own badges, because Twitch never echoes our PRIVMSGs back.
pub(crate) fn remember_userstate(msg: &UserStateMessage) -> EchoIdentity {
    EchoIdentity {
        display_name: msg.user_name.clone(),
        badges: merge_badges(&msg.badges, &msg.badge_info),
        color_hint: color_hex(msg.name_color),
    }
}

/// Build the optimistic local copy of a message we just sent successfully.
/// The id is synthetic (`local-<n>`) because Twitch never tells us the real
/// one — there is no echo to reconcile against.
pub(crate) fn build_local_echo(
    identity: Option<&EchoIdentity>,
    account_login: &str,
    text: &str,
    is_action: bool,
    counter: u64,
    timestamp: Option<DateTime<Utc>>,
) -> ChatMessage {
    let display_name = identity
        .map(|identity| identity.display_name.clone())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| account_login.to_string());
    ChatMessage {
        id: format!("local-{counter}"),
        timestamp,
        author: ChatAuthor {
            // Twitch never told us our numeric user id on this path; the
            // empty id is "unknown", never a claim about identity.
            id: String::new(),
            login: account_login.to_string(),
            display_name,
            badges: identity.map(|i| i.badges.clone()).unwrap_or_default(),
            color_hint: identity.and_then(|i| i.color_hint.clone()),
        },
        text: text.to_string(),
        kind: if is_action {
            MessageKind::Action
        } else {
            MessageKind::Chat
        },
        deleted: false,
        historical: false,
        local_echo: true,
        meta: Some(PlatformMeta::Twitch(TwitchMeta::default())),
    }
}

/// Pair each badge with its badge-info payload (subscriber tenure months and
/// the like), matched by set name (twi: irc.go `normalizeIRCBadges`).
fn merge_badges(
    badges: &[twitch_irc::message::Badge],
    badge_info: &[twitch_irc::message::Badge],
) -> Vec<Badge> {
    badges
        .iter()
        .map(|badge| Badge {
            set: badge.name.clone(),
            id: badge.version.clone(),
            info: badge_info
                .iter()
                .find(|info| info.name == badge.name)
                .map(|info| info.version.clone())
                .unwrap_or_default(),
        })
        .collect()
}

fn color_hex(color: Option<RGBColor>) -> Option<String> {
    color.map(|c| format!("#{:02X}{:02X}{:02X}", c.r, c.g, c.b))
}

/// A `[notice]` row spoken by the synthetic "notice" author — used both for
/// server NOTICEs and for locally declined sends (rate limit, duplicate),
/// so the user always learns *why* nothing was sent.
fn notice_row(id: String, text: String, timestamp: Option<DateTime<Utc>>) -> ChatMessage {
    ChatMessage {
        id,
        timestamp,
        author: ChatAuthor {
            id: String::new(),
            login: "notice".to_string(),
            display_name: "notice".to_string(),
            badges: Vec::new(),
            color_hint: None,
        },
        text,
        kind: MessageKind::Notice,
        deleted: false,
        historical: false,
        local_echo: false,
        meta: Some(PlatformMeta::Twitch(TwitchMeta::default())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A maximal `/me` must fit Twitch's limit *including* the CTCP framing
    /// the transport adds afterwards — otherwise the server silently drops
    /// it while the local echo claims delivery.
    #[test]
    fn an_action_body_leaves_room_for_its_ctcp_framing() {
        let body = "x".repeat(600);
        let capped = cap_graphemes(
            &sanitize_message(&body),
            MAX_MESSAGE_GRAPHEMES - ACTION_FRAMING_GRAPHEMES,
        );
        let framed = format!("\u{1}ACTION {capped}\u{1}");
        assert!(
            framed.chars().count() <= MAX_MESSAGE_GRAPHEMES,
            "framed length {} exceeds the wire limit",
            framed.chars().count()
        );
    }

    use twitch_irc::message::IRCMessage;

    /// Parse a raw IRC line the way the wire would deliver it.
    fn server_message(line: &str) -> ServerMessage {
        let irc = IRCMessage::parse(line).expect("test line must be valid IRC");
        ServerMessage::try_from(irc).expect("test line must be a known server message")
    }

    fn privmsg(line: &str) -> PrivmsgMessage {
        match server_message(line) {
            ServerMessage::Privmsg(msg) => msg,
            other => panic!("expected PRIVMSG, got {other:?}"),
        }
    }

    fn twitch_meta(message: &ChatMessage) -> &TwitchMeta {
        match message.meta.as_ref() {
            Some(PlatformMeta::Twitch(meta)) => meta,
            other => panic!("expected Twitch meta, got {other:?}"),
        }
    }

    // --- sanitizer -------------------------------------------------------

    #[test]
    fn sanitize_replaces_line_breaks_with_spaces() {
        assert_eq!(sanitize_message("a\r\nb\nc"), "a  b c");
    }

    #[test]
    fn sanitize_drops_c0_controls_and_del_but_keeps_the_ctcp_delimiter() {
        assert_eq!(
            sanitize_message("a\u{0}b\tc\u{1b}d\u{7f}e"),
            "abcde",
            "NUL, TAB, ESC, and DEL must vanish"
        );
        assert_eq!(
            sanitize_message("\u{1}ACTION waves\u{1}"),
            "\u{1}ACTION waves\u{1}"
        );
    }

    #[test]
    fn sanitize_leaves_short_text_alone() {
        let text = "a".repeat(MAX_MESSAGE_GRAPHEMES);
        assert_eq!(sanitize_message(&text), text);
    }

    #[test]
    fn sanitize_caps_at_the_grapheme_limit() {
        let text = "a".repeat(MAX_MESSAGE_GRAPHEMES + 7);
        assert_eq!(sanitize_message(&text), "a".repeat(MAX_MESSAGE_GRAPHEMES));
    }

    #[test]
    fn sanitize_never_splits_a_grapheme_at_the_cap() {
        // A family emoji is one grapheme built from several code points; when
        // it sits exactly at the cap it must survive whole, and the character
        // after it must be dropped.
        let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F466}";
        let text = format!("{}{}x", "a".repeat(MAX_MESSAGE_GRAPHEMES - 1), family);
        let capped = sanitize_message(&text);
        assert_eq!(
            capped,
            format!("{}{}", "a".repeat(MAX_MESSAGE_GRAPHEMES - 1), family)
        );
        assert_eq!(capped.graphemes(true).count(), MAX_MESSAGE_GRAPHEMES);
    }

    #[test]
    fn sanitize_keeps_the_closing_ctcp_delimiter_when_truncating_an_action() {
        let body = "b".repeat(MAX_MESSAGE_GRAPHEMES + 50);
        let action = format!("\u{1}ACTION {body}\u{1}");
        let capped = sanitize_message(&action);
        assert_eq!(capped.graphemes(true).count(), MAX_MESSAGE_GRAPHEMES);
        assert!(capped.starts_with('\u{1}'));
        assert!(capped.ends_with('\u{1}'));
    }

    // --- PRIVMSG ---------------------------------------------------------

    const PLAIN_PRIVMSG: &str = "@badge-info=;badges=moderator/1;color=#FF0000;display-name=Someone;emotes=;id=abc;mod=1;room-id=9;tmi-sent-ts=1700000000000;user-id=1 :someone!someone@someone.tmi.twitch.tv PRIVMSG #chan :hello";

    #[test]
    fn a_plain_privmsg_normalizes_to_a_chat_row() {
        let message = normalize_privmsg(&privmsg(PLAIN_PRIVMSG));
        assert_eq!(message.id, "abc");
        assert_eq!(message.kind, MessageKind::Chat);
        assert_eq!(message.text, "hello");
        assert_eq!(message.author.id, "1");
        assert_eq!(message.author.login, "someone");
        assert_eq!(message.author.display_name, "Someone");
        assert_eq!(message.author.color_hint.as_deref(), Some("#FF0000"));
        assert_eq!(
            message.author.badges,
            vec![Badge {
                set: "moderator".into(),
                id: "1".into(),
                info: String::new(),
            }]
        );
        assert_eq!(
            message.timestamp.map(|t| t.timestamp_millis()),
            Some(1_700_000_000_000)
        );
        let meta = twitch_meta(&message);
        assert_eq!(meta.bits, 0);
        assert!(!meta.first_message);
        assert!(meta.reply.is_none());
        assert!(!message.deleted && !message.historical && !message.local_echo);
    }

    #[test]
    fn badge_info_months_land_on_the_matching_badge() {
        let line = "@badge-info=subscriber/14;badges=subscriber/12;color=;display-name=Fan;emotes=;id=b1;room-id=9;tmi-sent-ts=1700000000000;user-id=2 :fan!fan@fan.tmi.twitch.tv PRIVMSG #chan :hi";
        let message = normalize_privmsg(&privmsg(line));
        assert_eq!(
            message.author.badges,
            vec![Badge {
                set: "subscriber".into(),
                id: "12".into(),
                info: "14".into(),
            }]
        );
        assert_eq!(message.author.color_hint, None);
    }

    #[test]
    fn an_action_privmsg_normalizes_to_an_action_row_with_the_framing_stripped() {
        let line = "@badge-info=;badges=;color=;display-name=Someone;emotes=;id=act1;room-id=9;tmi-sent-ts=1700000000000;user-id=1 :someone!someone@someone.tmi.twitch.tv PRIVMSG #chan :\u{1}ACTION waves hello\u{1}";
        let message = normalize_privmsg(&privmsg(line));
        assert_eq!(message.kind, MessageKind::Action);
        assert_eq!(message.text, "waves hello");
    }

    #[test]
    fn a_cheer_privmsg_carries_its_bits() {
        let line = "@badge-info=;badges=;bits=100;color=;display-name=Someone;emotes=;id=c1;room-id=9;tmi-sent-ts=1700000000000;user-id=1 :someone!someone@someone.tmi.twitch.tv PRIVMSG #chan :Cheer100 nice";
        let message = normalize_privmsg(&privmsg(line));
        assert_eq!(twitch_meta(&message).bits, 100);
    }

    #[test]
    fn the_first_msg_tag_marks_a_first_message_and_only_the_value_one_counts() {
        let first = "@badge-info=;badges=;color=;display-name=New;emotes=;first-msg=1;id=f1;room-id=9;tmi-sent-ts=1700000000000;user-id=3 :new!new@new.tmi.twitch.tv PRIVMSG #chan :hi all";
        assert!(twitch_meta(&normalize_privmsg(&privmsg(first))).first_message);

        let not_first = "@badge-info=;badges=;color=;display-name=Old;emotes=;first-msg=0;id=f2;room-id=9;tmi-sent-ts=1700000000000;user-id=4 :old!old@old.tmi.twitch.tv PRIVMSG #chan :hi";
        assert!(!twitch_meta(&normalize_privmsg(&privmsg(not_first))).first_message);
    }

    #[test]
    fn reply_parent_tags_become_a_reply_context() {
        let line = "@badge-info=;badges=;color=;display-name=LeftSwing;emotes=;first-msg=0;id=5b4f63a9;mod=0;reply-parent-display-name=Retoon;reply-parent-msg-body=hello;reply-parent-msg-id=6b13e51b;reply-parent-user-id=37940952;reply-parent-user-login=retoon;room-id=37940952;tmi-sent-ts=1673925983585;user-id=133651738 :leftswing!leftswing@leftswing.tmi.twitch.tv PRIVMSG #retoon :@Retoon yes";
        let message = normalize_privmsg(&privmsg(line));
        let reply = twitch_meta(&message)
            .reply
            .as_ref()
            .expect("the reply context must be present");
        assert_eq!(reply.parent_id, "6b13e51b");
        assert_eq!(reply.parent_author, "Retoon");
        assert_eq!(reply.parent_text, "hello");
    }

    // --- USERNOTICE ------------------------------------------------------

    #[test]
    fn a_sub_usernotice_becomes_a_notice_row_with_the_system_text() {
        let line = "@badge-info=subscriber/0;badges=subscriber/0,premium/1;color=;display-name=fallenseraphhh;emotes=;flags=;id=2a9bea11;login=fallenseraphhh;mod=0;msg-id=sub;msg-param-cumulative-months=1;msg-param-months=0;msg-param-should-share-streak=0;msg-param-sub-plan-name=Channel\\sSubscription\\s(xqcow);msg-param-sub-plan=Prime;room-id=71092938;subscriber=1;system-msg=fallenseraphhh\\ssubscribed\\swith\\sTwitch\\sPrime.;tmi-sent-ts=1582685713242;user-id=224005980;user-type= :tmi.twitch.tv USERNOTICE #xqcow";
        let ServerMessage::UserNotice(msg) = server_message(line) else {
            panic!("expected USERNOTICE");
        };
        let message = normalize_usernotice(&msg);
        assert_eq!(message.kind, MessageKind::Notice);
        assert_eq!(message.text, "fallenseraphhh subscribed with Twitch Prime.");
        assert_eq!(twitch_meta(&message).system_event, "sub");
        assert_eq!(message.author.login, "fallenseraphhh");
    }

    #[test]
    fn a_raid_usernotice_keeps_its_msg_id_and_raider_identity() {
        let line = "@badge-info=;badges=glhf-pledge/1;color=#FF69B4;display-name=iamelisabete;emotes=;flags=;id=bb99dda7;login=iamelisabete;mod=0;msg-id=raid;msg-param-displayName=iamelisabete;msg-param-login=iamelisabete;msg-param-profileImageURL=https://example.invalid/avatar.png;msg-param-viewerCount=430;room-id=71092938;subscriber=0;system-msg=430\\sraiders\\sfrom\\siamelisabete\\shave\\sjoined!;tmi-sent-ts=1594517796120;user-id=155874595;user-type= :tmi.twitch.tv USERNOTICE #xqcow";
        let ServerMessage::UserNotice(msg) = server_message(line) else {
            panic!("expected USERNOTICE");
        };
        let message = normalize_usernotice(&msg);
        assert_eq!(twitch_meta(&message).system_event, "raid");
        assert_eq!(message.text, "430 raiders from iamelisabete have joined!");
        assert_eq!(message.author.display_name, "iamelisabete");
        assert_eq!(message.author.color_hint.as_deref(), Some("#FF69B4"));
    }

    // --- CLEARCHAT / CLEARMSG -------------------------------------------

    #[test]
    fn a_clearchat_with_a_ban_duration_is_a_timeout_purge() {
        let line = "@ban-duration=600;room-id=11148817;target-user-id=148973258;tmi-sent-ts=1594553828245 :tmi.twitch.tv CLEARCHAT #pajlada :fabzeef";
        let ServerMessage::ClearChat(msg) = server_message(line) else {
            panic!("expected CLEARCHAT");
        };
        match normalize_clearchat(&msg) {
            ChatEvent::UserPurged {
                author_login,
                timeout_secs,
            } => {
                assert_eq!(author_login, "fabzeef");
                assert_eq!(timeout_secs, Some(600));
            }
            other => panic!("expected UserPurged, got {other:?}"),
        }
    }

    #[test]
    fn a_clearchat_without_a_duration_is_a_permanent_ban_purge() {
        let line = "@room-id=11148817;target-user-id=70948394;tmi-sent-ts=1594561360331 :tmi.twitch.tv CLEARCHAT #pajlada :weeb123";
        let ServerMessage::ClearChat(msg) = server_message(line) else {
            panic!("expected CLEARCHAT");
        };
        match normalize_clearchat(&msg) {
            ChatEvent::UserPurged {
                author_login,
                timeout_secs,
            } => {
                assert_eq!(author_login, "weeb123");
                assert_eq!(timeout_secs, None);
            }
            other => panic!("expected UserPurged, got {other:?}"),
        }
    }

    #[test]
    fn a_clearchat_without_a_target_clears_the_whole_chat() {
        let line = "@room-id=40286300;tmi-sent-ts=1594561392337 :tmi.twitch.tv CLEARCHAT #randers";
        let ServerMessage::ClearChat(msg) = server_message(line) else {
            panic!("expected CLEARCHAT");
        };
        assert!(matches!(normalize_clearchat(&msg), ChatEvent::ChatCleared));
    }

    #[test]
    fn a_clearmsg_deletes_exactly_the_targeted_message() {
        let line = "@login=alazymeme;room-id=;target-msg-id=3c92014f-340a-4dc3-a9c9-e5cf182f4a84;tmi-sent-ts=1594561955611 :tmi.twitch.tv CLEARMSG #pajlada :bad text";
        let ServerMessage::ClearMsg(msg) = server_message(line) else {
            panic!("expected CLEARMSG");
        };
        match normalize_clearmsg(&msg) {
            ChatEvent::MessageDeleted { message_id } => {
                assert_eq!(message_id, "3c92014f-340a-4dc3-a9c9-e5cf182f4a84");
            }
            other => panic!("expected MessageDeleted, got {other:?}"),
        }
    }

    // --- NOTICE ----------------------------------------------------------

    #[test]
    fn the_three_auth_failure_notice_bodies_are_recognized_case_insensitively() {
        for text in [
            "Login authentication failed",
            "login unsuccessful",
            "Improperly formatted auth",
        ] {
            assert!(
                is_auth_failure_notice(text),
                "{text:?} must classify as auth failure"
            );
        }
    }

    #[test]
    fn channel_state_notices_are_not_auth_failures() {
        assert!(!is_auth_failure_notice(
            "You are permanently banned from talking in forsen."
        ));
        assert!(!is_auth_failure_notice("This room is now in slow mode."));
        assert!(!is_auth_failure_notice(""));
    }

    #[test]
    fn a_pre_login_auth_notice_parses_and_classifies_from_the_wire() {
        let line = ":tmi.twitch.tv NOTICE * :Login authentication failed";
        let ServerMessage::Notice(msg) = server_message(line) else {
            panic!("expected NOTICE");
        };
        assert!(is_auth_failure_notice(&msg.message_text));
        let row = normalize_notice(&msg);
        assert_eq!(row.kind, MessageKind::Notice);
        assert_eq!(row.text, "Login authentication failed");
        assert_eq!(row.author.login, "notice");
    }

    // --- backoff ---------------------------------------------------------

    #[test]
    fn the_reconnect_backoff_doubles_from_two_seconds_and_caps_at_sixty() {
        let seconds: Vec<u64> = (1..=RECONNECT_MAX_ATTEMPTS)
            .map(|attempt| {
                reconnect_delay(attempt)
                    .expect("attempts within the budget must have a delay")
                    .as_secs()
            })
            .collect();
        assert_eq!(seconds, vec![2, 4, 8, 16, 32, 60, 60, 60, 60, 60]);
    }

    #[test]
    fn the_reconnect_backoff_gives_up_after_the_attempt_budget() {
        assert_eq!(reconnect_delay(0), None, "attempt numbering is 1-based");
        assert_eq!(reconnect_delay(RECONNECT_MAX_ATTEMPTS + 1), None);
        assert_eq!(reconnect_delay(u32::MAX), None);
    }

    // --- local echo ------------------------------------------------------

    #[test]
    fn a_local_echo_wears_the_identity_remembered_from_userstate() {
        let line = "@badge-info=subscriber/8;badges=broadcaster/1,subscriber/6;color=#FF0000;display-name=TESTUSER;emote-sets=0;mod=0;subscriber=1;user-type= :tmi.twitch.tv USERSTATE #randers";
        let ServerMessage::UserState(msg) = server_message(line) else {
            panic!("expected USERSTATE");
        };
        let identity = remember_userstate(&msg);
        assert_eq!(identity.display_name, "TESTUSER");
        assert_eq!(identity.color_hint.as_deref(), Some("#FF0000"));
        assert_eq!(
            identity.badges,
            vec![
                Badge {
                    set: "broadcaster".into(),
                    id: "1".into(),
                    info: String::new(),
                },
                Badge {
                    set: "subscriber".into(),
                    id: "6".into(),
                    info: "8".into(),
                },
            ]
        );

        let echo = build_local_echo(Some(&identity), "testuser", "hello there", false, 1, None);
        assert_eq!(echo.id, "local-1");
        assert!(echo.local_echo);
        assert_eq!(echo.kind, MessageKind::Chat);
        assert_eq!(echo.text, "hello there");
        assert_eq!(echo.author.login, "testuser");
        assert_eq!(echo.author.display_name, "TESTUSER");
        assert_eq!(echo.author.badges, identity.badges);
        assert_eq!(echo.author.color_hint.as_deref(), Some("#FF0000"));
    }

    #[test]
    fn a_local_echo_without_a_userstate_falls_back_to_the_account_login() {
        let echo = build_local_echo(None, "someuser", "waves", true, 7, None);
        assert_eq!(echo.id, "local-7");
        assert_eq!(echo.kind, MessageKind::Action);
        assert_eq!(echo.author.display_name, "someuser");
        assert!(echo.author.badges.is_empty());
        assert_eq!(echo.author.color_hint, None);
    }
}
