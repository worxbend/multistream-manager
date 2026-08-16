//! The Chat tab: a Twitch pane and a YouTube pane side by side.
//!
//! Layout contract (from the integration spec): both panes are always
//! present. A pane whose platform has no logged-in accounts shows an
//! empty-state with the exact commands that add one; a pane with accounts
//! shows one sub-tab per account, and inside a sub-tab one or more open
//! chats — the account's own chat by default, any other channel on request.
//!
//! Connections are lazy: a chat's task is spawned the first time its sub-tab
//! is activated, never for every account at startup. Hidden chats keep their
//! task and ring buffer (dropping them would re-pay YouTube's resolve/prime
//! quota on every tab switch) but count toward unread only while actually
//! off screen.

use std::collections::{BTreeMap, HashMap};

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::Frame;
use tokio::sync::mpsc;
use unicode_segmentation::UnicodeSegmentation as _;

use crate::auth::store::TokenStore;
use crate::chat::notify::{high_signal, Notifier};
use crate::chat::render::{render_message, BadgeMode, MessageLayout, RenderOpts};
use crate::chat::roster::{mention_prefix, Roster};
use crate::chat::source::{self, ChatCommand, ChatHandle};
use crate::chat::state::ChatState;
use crate::chat::{
    ChatAuthor, ChatEvent, ChatKey, ChatMessage, ConnectionStatus, MessageKind, PlatformMeta,
};
use crate::config::Config;
use crate::model::Platform;

/// One account sub-tab: the token-store key it speaks as, the label shown,
/// and the target of the account's own chat (Twitch login / YouTube channel
/// id) — `None` when the stored token predates identities.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountTab {
    pub key: String,
    pub label: String,
    pub own_target: Option<String>,
    /// The account's platform user/channel id (for /clip on Twitch).
    pub user_id: Option<String>,
}

/// A live chat: its running task and the state the UI folds events into.
pub struct OpenChat {
    pub handle: ChatHandle,
    pub state: ChatState,
    /// What the chat strip shows: `#channel` / the typed target.
    pub title: String,
    /// Everyone observed speaking in this chat, for @-completion.
    pub roster: Roster,
    /// Monotonic observation sequence feeding the roster's recency order.
    pub roster_seq: u64,
}

/// A moderation action awaiting its confirming second key press.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModAction {
    Delete,
    Ban,
}

impl ModAction {
    pub fn label(self) -> &'static str {
        match self {
            Self::Delete => "delete this message",
            Self::Ban => "permanently ban the author",
        }
    }
}

/// What keyboard input currently means inside the Chat tab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatFocus {
    /// Navigation keys.
    Normal,
    /// Typing goes into the active chat's composer.
    Compose,
    /// Typing goes into the join prompt (the buffer rides in the variant so
    /// `esc` discards it wholesale).
    Join(String),
    /// Incremental message search (`/`): typing edits the query, every edit
    /// jumps the selection to the newest match, enter commits (n/N walk),
    /// esc clears it.
    Search(String),
    /// The emoji picker (`ctrl+e`): typing filters the built-in catalog,
    /// enter inserts the top match into the composer, esc returns to
    /// wherever the picker was opened from.
    EmojiPicker { query: String, from_compose: bool },
    /// A timeout duration prompt (`t` on a selected message): the buffer
    /// holds text like `5m`; enter performs the timeout, esc cancels.
    TimeoutPrompt(String),
}

/// Everything the Chat tab remembers between frames.
pub struct ChatTabState {
    /// Which pane keyboard input goes to.
    pub focus: Platform,
    pub mode: ChatFocus,
    /// `space` was pressed and the next key completes the chord.
    pub pending_space: bool,
    /// A moderation key was pressed once; the same key again confirms, any
    /// other key cancels. Destructive actions never fire on one keystroke.
    pub pending_mod: Option<ModAction>,
    /// The accounts available per platform, discovered from the token store
    /// once at startup (primary first, extras after).
    pub accounts: BTreeMap<Platform, Vec<AccountTab>>,
    /// The selected account sub-tab per platform.
    pub selected: BTreeMap<Platform, usize>,
    /// The Twitch pane's share of the width, in percent. Resizable with
    /// `<`/`>` and reset with `=` — the same keys the reference TUIs use.
    pub split_percent: u16,
    /// Panes currently showing the activity view (`space a`) instead of the
    /// message list.
    pub activity: BTreeMap<Platform, bool>,
    /// Show the inspect panel for the selected message (`K`).
    pub inspect: bool,

    /// Every open chat, keyed by (platform, account, target).
    pub open: HashMap<ChatKey, OpenChat>,
    /// Open-chat order per account key, so `[`/`]` cycle deterministically.
    pub chats: BTreeMap<String, Vec<ChatKey>>,
    /// The active chat index per account key.
    pub active_chat: BTreeMap<String, usize>,

    /// The sending half every chat task clones; the receiving half moves to
    /// the event loop in `ui::run`.
    pub events_tx: source::EventSender,
    pub events_rx: Option<mpsc::UnboundedReceiver<(ChatKey, ChatEvent)>>,
    /// One HTTP client shared by every YouTube poller, for connection reuse.
    http: reqwest::Client,
    /// The one quota estimate every YouTube poller charges against, persisted
    /// across sessions (quota.json beside the config).
    quota: crate::chat::youtube::QuotaStore,
    /// A copy of the config, so composer commands (`/chats <x>`) can open
    /// chats without threading `&Config` through every key path.
    config: Config,
    /// Presentation switches shared by every pane, cycled with
    /// ctrl+g/b/y/n. Session-only, like the references — no runtime toggle
    /// persists to config.
    pub render: RenderOpts,
    /// Desktop notifications for high-signal events in off-screen chats.
    notifier: Notifier,
    /// Opt-in JSONL chat logging.
    logger: Option<crate::chat::chatlog::ChatLogger>,
    /// Whether the Chat tab is currently on screen at all — notifications
    /// fire for events the user cannot see.
    tab_visible: bool,
}

/// The default even split.
const SPLIT_DEFAULT: u16 = 50;
/// How far the divider may be pushed either way. Beyond this a pane is too
/// narrow to render a chat line meaningfully.
const SPLIT_MIN: u16 = 20;
const SPLIT_MAX: u16 = 80;
/// How much one keypress moves the divider.
const SPLIT_STEP: u16 = 2;

impl ChatTabState {
    /// Build the tab state, reading the account list from the token store.
    ///
    /// A store that cannot be read is treated as "no accounts": the panes
    /// then show their empty-state hints, which include the commands that
    /// would also surface the underlying problem.
    pub fn new(config: &Config) -> Self {
        let accounts = match TokenStore::load() {
            Ok(store) => discover_accounts(&store),
            Err(err) => {
                tracing::warn!(error = %format!("{err:#}"), "could not read chat accounts");
                BTreeMap::new()
            }
        };
        let (events_tx, events_rx) = mpsc::unbounded_channel();
        Self {
            focus: Platform::Twitch,
            mode: ChatFocus::Normal,
            pending_space: false,
            pending_mod: None,
            accounts,
            selected: BTreeMap::new(),
            split_percent: SPLIT_DEFAULT,
            activity: BTreeMap::new(),
            inspect: false,
            open: HashMap::new(),
            chats: BTreeMap::new(),
            active_chat: BTreeMap::new(),
            events_tx,
            events_rx: Some(events_rx),
            quota: crate::chat::youtube::QuotaStore::new(
                config.chat.daily_quota_units,
                crate::paths::config_dir()
                    .ok()
                    .map(|dir| dir.join("quota.json")),
            ),
            render: RenderOpts::default(),
            notifier: Notifier::new(config.chat.notifications),
            logger: build_logger(config),
            tab_visible: false,
            config: config.clone(),
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .user_agent(concat!("multistream-manager/", env!("CARGO_PKG_VERSION")))
                .build()
                // Building only fails over TLS backend misconfiguration,
                // which the streaming engine would already have surfaced;
                // fall back to the default client rather than poisoning the
                // whole UI over a chat-only concern.
                .unwrap_or_default(),
        }
    }

    /// The accounts for one platform (empty slice when none).
    pub fn accounts_for(&self, platform: Platform) -> &[AccountTab] {
        self.accounts
            .get(&platform)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// The selected account in one pane, if the pane has any.
    pub fn selected_account(&self, platform: Platform) -> Option<&AccountTab> {
        let accounts = self.accounts_for(platform);
        accounts.get(*self.selected.get(&platform).unwrap_or(&0))
    }

    /// The active chat key of one pane's selected account.
    pub fn active_key(&self, platform: Platform) -> Option<&ChatKey> {
        let account = self.selected_account(platform)?;
        let chats = self.chats.get(&account.key)?;
        chats.get(*self.active_chat.get(&account.key).unwrap_or(&0))
    }

    /// The tab became visible (or the selection changed): make sure every
    /// logged-in account has its own chat open, and update viewed/hidden
    /// marks so unread counts stay truthful.
    ///
    /// Every account, not only the two on screen: an account whose sub-tab is
    /// not showing still collects messages into its ring buffer, so its unread
    /// badge is meaningful and switching to it is instant. The connections are
    /// still lazy in the sense that matters — nothing is opened until the Chat
    /// tab is entered for the first time.
    pub fn activate(&mut self, config: &Config) {
        for platform in Platform::ALL {
            let accounts = self.accounts.get(&platform).cloned().unwrap_or_default();
            for account in accounts {
                let has_chats = self
                    .chats
                    .get(&account.key)
                    .is_some_and(|chats| !chats.is_empty());
                if !has_chats {
                    if let Some(target) = account.own_target.clone() {
                        self.open_chat(config, platform, &account, target);
                    }
                }
            }
        }
        self.tab_visible = true;
        self.refresh_visibility();
    }

    /// The tab left the screen: everything counts as unread again.
    pub fn deactivate(&mut self) {
        self.tab_visible = false;
        for chat in self.open.values_mut() {
            chat.state.mark_hidden();
        }
    }

    /// Mark exactly the on-screen chats viewed, everything else hidden.
    fn refresh_visibility(&mut self) {
        let visible: Vec<ChatKey> = Platform::ALL
            .iter()
            .filter_map(|&platform| self.active_key(platform).cloned())
            .collect();
        for (key, chat) in self.open.iter_mut() {
            if visible.contains(key) {
                chat.state.mark_viewed();
            } else {
                chat.state.mark_hidden();
            }
        }
    }

    /// Open (or switch to) a chat on `target` through `account`.
    pub fn open_chat(
        &mut self,
        config: &Config,
        platform: Platform,
        account: &AccountTab,
        target: String,
    ) {
        let target = normalize_target(platform, &target);
        if target.is_empty() {
            return;
        }
        let key = ChatKey {
            platform,
            account: account.key.clone(),
            target: target.clone(),
        };

        let chats = self.chats.entry(account.key.clone()).or_default();
        if let Some(index) = chats.iter().position(|k| *k == key) {
            self.active_chat.insert(account.key.clone(), index);
            self.refresh_visibility();
            return;
        }

        let tokens = source::token_provider(config, &account.key);
        let handle = match platform {
            Platform::Twitch => crate::chat::twitch::spawn(crate::chat::twitch::TwitchParams {
                key: key.clone(),
                account_login: account.own_target.clone().unwrap_or_default(),
                account_user_id: account.user_id.clone().unwrap_or_default(),
                client_id: config.twitch.client_id.clone(),
                tokens,
                events: self.events_tx.clone(),
                http: self.http.clone(),
            }),
            Platform::YouTube => crate::chat::youtube::spawn(crate::chat::youtube::SpawnParams {
                key: key.clone(),
                poll_floor_ms: config.chat.poll_interval_floor_ms,
                poll_ceiling_ms: config.chat.poll_interval_ceiling_ms,
                quota: self.quota.clone(),
                quota_reserve_percent: config.chat.quota_reserve_percent,
                token: tokens,
                events: self.events_tx.clone(),
                client: self.http.clone(),
                base: None,
            }),
        };

        let title = match platform {
            Platform::Twitch => format!("#{target}"),
            Platform::YouTube => target.clone(),
        };
        self.open.insert(
            key.clone(),
            OpenChat {
                handle,
                state: ChatState::new(&config.chat),
                title,
                roster: Roster::new(),
                roster_seq: 0,
            },
        );
        let chats = self.chats.entry(account.key.clone()).or_default();
        chats.push(key);
        self.active_chat
            .insert(account.key.clone(), chats.len() - 1);
        self.refresh_visibility();
    }

    /// Close the focused pane's active chat. Dropping the handle closes the
    /// task's command channel, which ends it — no leaked connections.
    pub fn close_active_chat(&mut self) {
        let Some(account) = self.selected_account(self.focus).cloned() else {
            return;
        };
        let Some(chats) = self.chats.get_mut(&account.key) else {
            return;
        };
        let index = *self.active_chat.get(&account.key).unwrap_or(&0);
        if index >= chats.len() {
            return;
        }
        let key = chats.remove(index);
        self.open.remove(&key);
        let len = chats.len();
        self.active_chat
            .insert(account.key.clone(), index.min(len.saturating_sub(1)));
        self.refresh_visibility();
    }

    /// Cycle the focused pane's open chats.
    pub fn cycle_chat(&mut self, forward: bool) {
        let Some(account) = self.selected_account(self.focus).cloned() else {
            return;
        };
        let Some(chats) = self.chats.get(&account.key) else {
            return;
        };
        if chats.len() < 2 {
            return;
        }
        let current = *self.active_chat.get(&account.key).unwrap_or(&0);
        let next = if forward {
            (current + 1) % chats.len()
        } else {
            (current + chats.len() - 1) % chats.len()
        };
        self.active_chat.insert(account.key.clone(), next);
        self.refresh_visibility();
    }

    /// Move focus to the other pane.
    pub fn focus_other(&mut self) {
        self.focus = match self.focus {
            Platform::Twitch => Platform::YouTube,
            Platform::YouTube => Platform::Twitch,
        };
    }

    /// Cycle the focused pane's account sub-tab forward or backward, opening
    /// the newly selected account's own chat lazily.
    pub fn cycle_account(&mut self, forward: bool, config: &Config) {
        let count = self.accounts_for(self.focus).len();
        if count < 2 {
            return;
        }
        let current = *self.selected.get(&self.focus).unwrap_or(&0);
        let next = if forward {
            (current + 1) % count
        } else {
            (current + count - 1) % count
        };
        self.selected.insert(self.focus, next);
        self.activate(config);
    }

    /// Grow or shrink the focused pane by one step.
    pub fn resize(&mut self, grow_focused: bool) {
        // The split percentage names the Twitch (left) pane, so growing the
        // YouTube pane means shrinking the number.
        let grow_left = match self.focus {
            Platform::Twitch => grow_focused,
            Platform::YouTube => !grow_focused,
        };
        let next = if grow_left {
            self.split_percent.saturating_add(SPLIT_STEP)
        } else {
            self.split_percent.saturating_sub(SPLIT_STEP)
        };
        self.split_percent = next.clamp(SPLIT_MIN, SPLIT_MAX);
    }

    pub fn reset_split(&mut self) {
        self.split_percent = SPLIT_DEFAULT;
    }

    /// Fold one event from a chat task into its chat's state.
    ///
    /// An event for a chat closed meanwhile is simply dropped — its task ends
    /// as soon as it notices the closed command channel.
    pub fn handle_event(&mut self, key: ChatKey, event: ChatEvent) {
        let on_screen = self.tab_visible
            && Platform::ALL
                .iter()
                .any(|&platform| self.active_key(platform) == Some(&key));
        let Some(chat) = self.open.get_mut(&key) else {
            return;
        };
        if let ChatEvent::Message(msg) = &event {
            // Roster and log observe the stream before the state folds it,
            // so a local echo replaced later was still logged as sent.
            chat.roster_seq += 1;
            chat.roster.observe(&msg.author, chat.roster_seq);
            if let Some(logger) = self.logger.as_mut() {
                logger.append(&key.target, msg);
            }
            // Notify only for what the user cannot currently see.
            if !on_screen {
                if let Some((title, body)) = high_signal(msg) {
                    self.notifier.notify(&title, &body);
                }
            }
        }
        chat.state.apply(event);
    }

    /// The focused pane's active chat, mutably.
    fn active_chat_mut(&mut self) -> Option<&mut OpenChat> {
        let key = self.active_key(self.focus)?.clone();
        self.open.get_mut(&key)
    }

    /// Scroll the focused chat by `delta` *visible* messages (positive =
    /// further back). View scrolling drops the selection (and any armed
    /// confirmation): a selection the view has scrolled away from would let
    /// moderation act on a message the user cannot see. With filters active
    /// the walk skips hidden rows, so paging can never strand the view in a
    /// span with nothing to draw.
    pub fn scroll_by(&mut self, delta: i64) {
        self.pending_mod = None;
        let login = self
            .selected_account(self.focus)
            .and_then(|account| account.own_target.clone())
            .unwrap_or_default();
        if let Some(chat) = self.active_chat_mut() {
            chat.state.cursor = None;
            let len = chat.state.messages.len();
            if len == 0 {
                return;
            }
            let filters = chat.state.filters;
            let step: i64 = if delta > 0 { 1 } else { -1 };
            let mut offset = chat.state.scroll as i64;
            for _ in 0..delta.abs() {
                let mut next = offset;
                loop {
                    next += step;
                    if next < 0 || next as usize >= len {
                        next = offset;
                        break;
                    }
                    let index = len - 1 - next as usize;
                    let visible = chat
                        .state
                        .messages
                        .get(index)
                        .is_some_and(|msg| filters.matches(msg, &login));
                    if visible {
                        break;
                    }
                }
                if next == offset {
                    break;
                }
                offset = next;
            }
            chat.state.scroll = offset.clamp(0, (len - 1) as i64) as usize;
        }
    }

    /// Jump to the oldest (`g`) or newest (`G`) *visible* message.
    pub fn scroll_to_end(&mut self, oldest: bool) {
        self.pending_mod = None;
        let login = self
            .selected_account(self.focus)
            .and_then(|account| account.own_target.clone())
            .unwrap_or_default();
        if let Some(chat) = self.active_chat_mut() {
            chat.state.cursor = None;
            let len = chat.state.messages.len();
            if len == 0 {
                return;
            }
            let filters = chat.state.filters;
            let offsets: Vec<usize> = if oldest {
                (0..len).rev().collect()
            } else {
                (0..len).collect()
            };
            for offset in offsets {
                let index = len - 1 - offset;
                let visible = chat
                    .state
                    .messages
                    .get(index)
                    .is_some_and(|msg| filters.matches(msg, &login));
                if visible {
                    chat.state.scroll = offset;
                    return;
                }
            }
            chat.state.scroll = 0;
        }
    }

    /// Move the selection cursor by `delta` messages (positive = older).
    /// Starting a selection picks the newest visible message; the view
    /// follows the cursor so it can never leave the screen.
    pub fn select_move(&mut self, delta: i64) {
        if let Some(chat) = self.active_chat_mut() {
            let len = chat.state.messages.len();
            if len == 0 {
                return;
            }
            let max = (len - 1) as i64;
            // Starting a selection picks the row at the current view's
            // bottom edge — not the newest message, which would yank a
            // scrolled-back reader to the tail.
            let current = chat
                .state
                .cursor
                .map(|c| c as i64)
                .unwrap_or(chat.state.scroll as i64 - delta);
            let next = (current + delta).clamp(0, max) as usize;
            chat.state.cursor = Some(next);
            // The view follows the selection: anchoring the selected row at
            // the bottom of the pane keeps it on screen at every pane height
            // without the state layer having to know the height.
            chat.state.scroll = next;
        }
    }

    /// Drop the selection, any pending moderation confirmation, and an armed
    /// reply — esc means "stop what I was lining up", all of it.
    pub fn clear_selection(&mut self) {
        self.pending_mod = None;
        if let Some(chat) = self.active_chat_mut() {
            chat.state.cursor = None;
            chat.state.reply_to = None;
        }
    }

    /// The selected message of the focused chat, if any.
    pub fn selected_message(&self) -> Option<&ChatMessage> {
        let key = self.active_key(self.focus)?;
        let chat = self.open.get(key)?;
        let cursor = chat.state.cursor?;
        let len = chat.state.messages.len();
        chat.state.messages.get(len.checked_sub(1 + cursor)?)
    }

    /// Arm a reply to the selected message. The composer opens with the
    /// context; Twitch threads by id, YouTube gets an `@Name ` prefix at send
    /// time (its API has no reply field — the prefix IS the convention).
    pub fn reply_to_selected(&mut self) -> bool {
        let Some(msg) = self.selected_message() else {
            return false;
        };
        let context = (msg.id.clone(), msg.author.display_name.clone());
        if context.0.is_empty() {
            return false;
        }
        if let Some(chat) = self.active_chat_mut() {
            chat.state.reply_to = Some(context);
            return true;
        }
        false
    }

    /// First press arms a moderation action against the selected message;
    /// the same key again performs it. Returns the armed action for the
    /// status line.
    pub fn moderate(&mut self, action: ModAction) {
        if self.pending_mod != Some(action) {
            // Arming (or switching to a different action) never acts.
            self.pending_mod = self.selected_message().map(|_| action);
            return;
        }
        self.pending_mod = None;
        let Some(msg) = self.selected_message() else {
            return;
        };
        let command = match action {
            ModAction::Delete => ChatCommand::Delete {
                message_id: msg.id.clone(),
            },
            ModAction::Ban => ChatCommand::Ban {
                channel_id: msg.author.id.clone(),
                timeout_secs: None,
            },
        };
        if let Some(chat) = self.active_chat_mut() {
            if chat.handle.commands.try_send(command).is_err() {
                chat.state.apply(ChatEvent::Message(Box::new(local_notice(
                    "the chat task is busy — press the key again to retry",
                ))));
            }
        }
    }

    /// Append typed text to the focused chat's composer draft.
    pub fn compose_push(&mut self, c: char) {
        if let Some(chat) = self.active_chat_mut() {
            if c == '\n' || c == '\r' {
                return;
            }
            chat.state.draft.push(c);
        }
    }

    /// Delete one grapheme cluster from the composer (never half an emoji).
    pub fn compose_backspace(&mut self) {
        if let Some(chat) = self.active_chat_mut() {
            let boundary = chat
                .state
                .draft
                .grapheme_indices(true)
                .next_back()
                .map(|(i, _)| i);
            if let Some(i) = boundary {
                chat.state.draft.truncate(i);
            }
        }
    }

    /// Send the composer draft to the focused chat's task.
    ///
    /// A refused send (the per-chat command queue is momentarily full) keeps
    /// the draft and says so, rather than losing what was typed.
    pub fn compose_send(&mut self) {
        let Some(chat) = self.active_chat_mut() else {
            return;
        };
        let text = chat.state.draft.trim().to_string();
        if text.is_empty() {
            return;
        }
        // Composer commands that never reach the platform (twi's /channels,
        // yc's /chats): bare opens the join prompt, with an argument joins
        // directly. Everything else — including other /commands — is sent
        // verbatim; /me is handled by the Twitch adapter.
        if text == "/clip" {
            chat.state.draft.clear();
            self.mode = ChatFocus::Normal;
            if let Some(chat) = self.active_chat_mut() {
                let _ = chat.handle.commands.try_send(ChatCommand::Clip);
            }
            return;
        }
        // The command must be the whole word: "/chatstats" is a message, not
        // a request to join the channel "tats".
        let command_rest = ["/chats", "/channels", "/channel"]
            .iter()
            .find_map(|command| text.strip_prefix(command))
            .filter(|rest| rest.is_empty() || rest.starts_with(' '));
        if let Some(rest) = command_rest {
            let target = rest.trim().to_string();
            chat.state.draft.clear();
            if target.is_empty() {
                self.mode = ChatFocus::Join(String::new());
            } else {
                self.mode = ChatFocus::Normal;
                self.join_target(&self.config.clone(), &target);
            }
            return;
        }
        let reply_to = chat.state.reply_to.as_ref().map(|(id, _)| id.clone());
        match chat
            .handle
            .commands
            .try_send(ChatCommand::Send { text, reply_to })
        {
            Ok(()) => {
                chat.state.draft.clear();
                // The reply is only spent once the chat task has actually
                // accepted the message; clearing it earlier meant a refused
                // send silently dropped the threading, and the retry went out
                // as an ordinary message with no indication anything changed.
                chat.state.reply_to = None;
            }
            Err(_) => {
                chat.state.apply(ChatEvent::Message(Box::new(local_notice(
                    "the chat task is busy — the message was kept in the composer, try again",
                ))));
            }
        }
    }

    /// Cycle the message layout (ctrl+g): inline → grouped → compact.
    pub fn cycle_layout(&mut self) {
        self.render.layout = match self.render.layout {
            MessageLayout::Inline => MessageLayout::Grouped,
            MessageLayout::Grouped => MessageLayout::Compact,
            MessageLayout::Compact => MessageLayout::Inline,
        };
    }

    /// Cycle badge rendering (ctrl+b): glyph → text → off.
    pub fn cycle_badges(&mut self) {
        self.render.badge_mode = match self.render.badge_mode {
            BadgeMode::Glyph => BadgeMode::Text,
            BadgeMode::Text => BadgeMode::Off,
            BadgeMode::Off => BadgeMode::Glyph,
        };
    }

    pub fn toggle_highlight(&mut self) {
        self.render.highlight_emotes = !self.render.highlight_emotes;
    }

    pub fn toggle_full_username(&mut self) {
        self.render.full_username = !self.render.full_username;
    }

    /// Complete the trailing @mention in the composer from the roster
    /// (tab in compose mode). Prefix matches outrank substring matches;
    /// the best candidate replaces the typed prefix as `@DisplayName `.
    pub fn complete_mention(&mut self) {
        let Some(chat) = self.active_chat_mut() else {
            return;
        };
        let Some(prefix) = mention_prefix(&chat.state.draft) else {
            return;
        };
        let typed = prefix.trim_start_matches('@').to_string();
        let Some(entry) = chat.roster.complete(&typed, 1).into_iter().next() else {
            return;
        };
        let completion = format!("@{} ", entry.name());
        // mention_prefix may return the word with or without its @ — cut the
        // @ too either way, since the completion re-adds it.
        let mut cut = chat.state.draft.len() - prefix.len();
        if !prefix.starts_with('@') && chat.state.draft[..cut].ends_with('@') {
            cut -= 1;
        }
        chat.state.draft.truncate(cut);
        chat.state.draft.push_str(&completion);
    }

    /// Insert an emoji from the picker into the composer draft.
    pub fn insert_emoji(&mut self, emoji: &str) {
        if let Some(chat) = self.active_chat_mut() {
            chat.state.draft.push_str(emoji);
        }
    }

    /// Ask the focused chat's task to reconnect (also the manual override for
    /// a quota pause or an ended chat).
    pub fn reconnect_active(&mut self) {
        if let Some(chat) = self.active_chat_mut() {
            let _ = chat.handle.commands.try_send(ChatCommand::Reconnect);
        }
    }

    /// Toggle the focused pane between messages and the activity view.
    pub fn toggle_activity(&mut self) {
        let entry = self.activity.entry(self.focus).or_insert(false);
        *entry = !*entry;
    }

    /// Toggle one numbered filter (1–4) on the focused chat; 0 resets.
    pub fn toggle_filter(&mut self, digit: char) {
        if let Some(chat) = self.active_chat_mut() {
            let filters = &mut chat.state.filters;
            match digit {
                '1' => filters.mentions = !filters.mentions,
                '2' => filters.roles = !filters.roles,
                '3' => filters.events = !filters.events,
                '4' => filters.notices = !filters.notices,
                _ => filters.reset(),
            }
        }
    }

    /// Apply an in-progress or committed search: move the selection to the
    /// newest message matching `query` (text or author, case-insensitive).
    pub fn search_jump_newest(&mut self, query: &str) {
        if query.is_empty() {
            return;
        }
        let needle = query.to_lowercase();
        let login = self
            .selected_account(self.focus)
            .and_then(|account| account.own_target.clone())
            .unwrap_or_default();
        if let Some(chat) = self.active_chat_mut() {
            let len = chat.state.messages.len();
            let filters = chat.state.filters;
            for offset in 0..len {
                let msg = chat
                    .state
                    .messages
                    .get(len - 1 - offset)
                    .expect("offset < len by construction");
                // A filtered-out message must not become the selection: the
                // jump would land on an invisible row.
                if search_matches(msg, &needle) && filters.matches(msg, &login) {
                    chat.state.cursor = Some(offset);
                    chat.state.scroll = offset;
                    return;
                }
            }
        }
    }

    /// Walk the committed search to the next match, older (`n`) or newer
    /// (`N`). Stops at the ends rather than wrapping — "how far back was
    /// that" must stay answerable (yc's rule).
    pub fn search_step(&mut self, older: bool) {
        let query = self
            .active_key(self.focus)
            .and_then(|key| self.open.get(key))
            .map(|chat| chat.state.search.clone())
            .unwrap_or_default();
        if query.is_empty() {
            return;
        }
        let needle = query.to_lowercase();
        let login = self
            .selected_account(self.focus)
            .and_then(|account| account.own_target.clone())
            .unwrap_or_default();
        if let Some(chat) = self.active_chat_mut() {
            let len = chat.state.messages.len();
            if len == 0 {
                return;
            }
            let filters = chat.state.filters;
            // With no selection the walk starts *at* the newest row rather
            // than past it: after clearing the selection (esc, or any scroll),
            // pressing n used to skip a match sitting on the newest message.
            let mut offset = chat.state.cursor.unwrap_or(0);
            let mut test_current = chat.state.cursor.is_none();
            loop {
                if test_current {
                    test_current = false;
                } else if older {
                    offset += 1;
                    if offset >= len {
                        return; // no wrap
                    }
                } else {
                    if offset == 0 {
                        return;
                    }
                    offset -= 1;
                }
                let msg = chat
                    .state
                    .messages
                    .get(len - 1 - offset)
                    .expect("offset < len by loop guard");
                if search_matches(msg, &needle) && filters.matches(msg, &login) {
                    chat.state.cursor = Some(offset);
                    chat.state.scroll = offset;
                    return;
                }
            }
        }
    }

    /// Store the committed query on the focused chat.
    pub fn commit_search(&mut self, query: String) {
        if let Some(chat) = self.active_chat_mut() {
            chat.state.search = query;
        }
    }

    /// Perform a timeout of the selected author for a parsed duration.
    pub fn timeout_selected(&mut self, duration_secs: u64) {
        let Some(msg) = self.selected_message() else {
            return;
        };
        let channel_id = msg.author.id.clone();
        if channel_id.is_empty() {
            return;
        }
        if let Some(chat) = self.active_chat_mut() {
            let command = ChatCommand::Ban {
                channel_id,
                timeout_secs: Some(duration_secs),
            };
            if chat.handle.commands.try_send(command).is_err() {
                chat.state.apply(ChatEvent::Message(Box::new(local_notice(
                    "the chat task is busy — try the timeout again",
                ))));
            }
        }
    }

    /// Open a chat on the target typed into the join prompt.
    pub fn join_target(&mut self, config: &Config, raw: &str) {
        let raw = raw.trim().to_string();
        if raw.is_empty() {
            return;
        }
        if let Some(account) = self.selected_account(self.focus).cloned() {
            self.open_chat(config, self.focus, &account, raw);
        }
    }
}

/// Whether a message matches a lowercase search needle (text or author).
fn search_matches(msg: &ChatMessage, needle: &str) -> bool {
    msg.text.to_lowercase().contains(needle)
        || msg.author.display_name.to_lowercase().contains(needle)
        || msg.author.login.to_lowercase().contains(needle)
}

/// Parse a timeout duration like `45s`, `5m` or `2h` (bare numbers are
/// minutes). Capped at 24 hours — YouTube's own maximum for a temporary ban.
pub fn parse_timeout(text: &str) -> Option<u64> {
    let text = text.trim().to_lowercase();
    if text.is_empty() {
        return None;
    }
    let (number, unit) = match text.strip_suffix(['s', 'm', 'h']) {
        Some(number) => (number, text.chars().last().expect("non-empty by strip")),
        None => (text.as_str(), 'm'),
    };
    let value: u64 = number.trim().parse().ok()?;
    let secs = match unit {
        's' => value,
        'm' => value.checked_mul(60)?,
        'h' => value.checked_mul(3600)?,
        _ => return None,
    };
    if secs == 0 {
        return None;
    }
    Some(secs.min(24 * 3600))
}

/// A locally generated notice row (never sent anywhere).
fn local_notice(text: &str) -> ChatMessage {
    ChatMessage {
        id: String::new(),
        timestamp: None,
        author: ChatAuthor {
            display_name: "notice".into(),
            ..Default::default()
        },
        text: text.into(),
        kind: MessageKind::Notice,
        deleted: false,
        historical: false,
        local_echo: false,
        meta: None,
    }
}

/// Twitch targets are lowercase logins without the `#`; YouTube targets are
/// passed through for the adapter's own parser (it accepts ids, handles and
/// URLs).
fn normalize_target(platform: Platform, target: &str) -> String {
    match platform {
        Platform::Twitch => target.trim().trim_start_matches('#').to_lowercase(),
        Platform::YouTube => target.trim().to_string(),
    }
}

/// The chat logger, when the config opts in.
fn build_logger(config: &Config) -> Option<crate::chat::chatlog::ChatLogger> {
    if !config.chat.chat_logging {
        return None;
    }
    let dir = if config.chat.chat_log_dir.is_empty() {
        match crate::paths::chat_log_dir() {
            Ok(dir) => dir,
            Err(err) => {
                tracing::warn!(error = %format!("{err:#}"), "chat logging disabled");
                return None;
            }
        }
    } else {
        std::path::PathBuf::from(&config.chat.chat_log_dir)
    };
    Some(crate::chat::chatlog::ChatLogger::new(
        dir,
        config.chat.chat_log_max_bytes,
        config.chat.chat_log_max_files as usize,
    ))
}

/// Turn the token store into per-platform account tab lists.
fn discover_accounts(store: &TokenStore) -> BTreeMap<Platform, Vec<AccountTab>> {
    let mut out = BTreeMap::new();
    for platform in Platform::ALL {
        let tabs: Vec<AccountTab> = store
            .accounts(platform)
            .into_iter()
            .map(|(key, tokens)| {
                let identity = tokens.identity.as_ref();
                AccountTab {
                    key: key.to_string(),
                    label: identity
                        .map(|identity| {
                            if identity.display_name.is_empty() {
                                identity.login.clone()
                            } else {
                                identity.display_name.clone()
                            }
                        })
                        // Tokens saved before identities existed have no name
                        // to show; the store key is at least unambiguous.
                        .unwrap_or_else(|| key.to_string()),
                    own_target: identity.map(|identity| match platform {
                        Platform::Twitch => identity.login.clone(),
                        Platform::YouTube => identity.id.clone(),
                    }),
                    user_id: identity.map(|identity| identity.id.clone()),
                }
            })
            .collect();
        if !tabs.is_empty() {
            out.insert(platform, tabs);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Drawing
// ---------------------------------------------------------------------------

/// Draw the whole Chat tab into `area`.
pub fn draw(frame: &mut Frame, area: Rect, state: &ChatTabState, config: &Config) {
    let panes = Layout::horizontal([
        Constraint::Percentage(state.split_percent),
        Constraint::Percentage(100 - state.split_percent),
    ])
    .split(area);

    draw_pane(frame, panes[0], state, config, Platform::Twitch);
    draw_pane(frame, panes[1], state, config, Platform::YouTube);
}

fn connection_color(status: ConnectionStatus) -> Color {
    let sk = crate::theme::skin();
    match status {
        ConnectionStatus::Connected => sk.success,
        ConnectionStatus::Connecting | ConnectionStatus::Reconnecting => sk.warning,
        ConnectionStatus::QuotaPaused => sk.accent,
        ConnectionStatus::Closed | ConnectionStatus::Disconnected => sk.muted,
        ConnectionStatus::Failed => sk.error,
    }
}

/// One chat pane, drawn without a border of its own.
///
/// The Combined tab places panels itself and draws its own frames, so it
/// needs the contents of a pane separately from the pane.
pub fn draw_single(
    frame: &mut Frame,
    area: Rect,
    state: &ChatTabState,
    config: &Config,
    platform: Platform,
) {
    draw_pane_inner(frame, area, state, config, platform);
}

fn draw_pane(
    frame: &mut Frame,
    area: Rect,
    state: &ChatTabState,
    config: &Config,
    platform: Platform,
) {
    let sk = crate::theme::skin();
    let focused = state.focus == platform;
    let border_style = if focused {
        Style::default().fg(sk.accent)
    } else {
        Style::default().fg(sk.muted)
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style)
        .title(format!(" {} ", platform.label()));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    draw_pane_inner(frame, inner, state, config, platform);
}

/// Everything inside a chat pane's border.
fn draw_pane_inner(
    frame: &mut Frame,
    inner: Rect,
    state: &ChatTabState,
    config: &Config,
    platform: Platform,
) {
    let focused = state.focus == platform;
    let accounts = state.accounts_for(platform);
    if accounts.is_empty() {
        draw_empty_state(frame, inner, config, platform);
        return;
    }

    let rows = Layout::vertical([
        Constraint::Length(1), // account sub-tabs
        Constraint::Length(1), // chat strip + connection state
        Constraint::Min(0),    // messages
        Constraint::Length(1), // composer / hints
    ])
    .split(inner);

    draw_account_strip(frame, rows[0], state, platform);
    draw_chat_strip(frame, rows[1], state, platform);
    if state.inspect && state.focus == platform {
        draw_inspect(frame, rows[2], state);
    } else if state.activity.get(&platform).copied().unwrap_or(false) {
        draw_activity(frame, rows[2], state, platform);
    } else {
        draw_messages(frame, rows[2], state, platform);
    }
    draw_composer(frame, rows[3], state, platform, focused);
}

fn draw_account_strip(frame: &mut Frame, area: Rect, state: &ChatTabState, platform: Platform) {
    let sk = crate::theme::skin();
    let accounts = state.accounts_for(platform);
    let selected = *state.selected.get(&platform).unwrap_or(&0);
    let mut spans = Vec::new();
    for (index, account) in accounts.iter().enumerate() {
        // The unread total across the account's chats, so a busy background
        // account is visible from the strip.
        let unread: usize = state
            .chats
            .get(&account.key)
            .map(|chats| {
                chats
                    .iter()
                    .filter_map(|key| state.open.get(key))
                    .map(|chat| chat.state.unread)
                    .sum()
            })
            .unwrap_or(0);
        let label = if unread > 0 {
            format!(" {} ({unread}) ", account.label)
        } else {
            format!(" {} ", account.label)
        };
        let style = if index == selected {
            Style::default()
                .fg(sk.on_accent)
                .bg(sk.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(sk.muted)
        };
        spans.push(Span::styled(label, style));
        spans.push(Span::raw(" "));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_chat_strip(frame: &mut Frame, area: Rect, state: &ChatTabState, platform: Platform) {
    let sk = crate::theme::skin();
    let Some(account) = state.selected_account(platform) else {
        return;
    };
    let chats = state
        .chats
        .get(&account.key)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let active = *state.active_chat.get(&account.key).unwrap_or(&0);

    let mut spans = Vec::new();
    for (index, key) in chats.iter().enumerate() {
        let Some(chat) = state.open.get(key) else {
            continue;
        };
        let style = if index == active {
            Style::default().add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(sk.muted)
        };
        spans.push(Span::styled(chat.title.clone(), style));
        spans.push(Span::raw("  "));
    }
    // The active chat's connection state closes the line.
    if let Some(chat) = chats.get(active).and_then(|key| state.open.get(key)) {
        let (status, detail) = &chat.state.connection;
        spans.push(Span::styled(
            format!("· {}", status.label()),
            Style::default().fg(connection_color(*status)),
        ));
        if !detail.is_empty() {
            spans.push(Span::styled(
                format!(" — {detail}"),
                Style::default().fg(sk.muted),
            ));
        }
        if chat.state.filters.any() {
            spans.push(Span::styled(
                format!(" · filter: {}", chat.state.filters.summary()),
                Style::default().fg(sk.warning),
            ));
        }
        if !chat.state.search.is_empty() {
            spans.push(Span::styled(
                format!(" · /{}", chat.state.search),
                Style::default().fg(sk.warning),
            ));
        }
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// One activity line for a message, if it is activity at all. A projection
/// of history (yc activity.go): no second data store to drift or clear.
fn activity_line(msg: &ChatMessage) -> Option<Line<'static>> {
    let sk = crate::theme::skin();
    let (glyph, color, text) = if msg.deleted {
        (
            "✖",
            sk.error,
            format!("a message by {} was removed", msg.author.display_name),
        )
    } else {
        match (&msg.kind, &msg.meta) {
            (MessageKind::Paid, Some(PlatformMeta::YouTube(meta))) => {
                let amount = meta
                    .paid
                    .as_ref()
                    .map(|paid| paid.display.clone())
                    .unwrap_or_default();
                (
                    "◈",
                    sk.warning,
                    format!("{} {amount}", msg.author.display_name),
                )
            }
            (MessageKind::Membership, _) => (
                "★",
                sk.accent,
                format!("{} — {}", msg.author.display_name, msg.text),
            ),
            (_, Some(PlatformMeta::Twitch(meta))) if meta.bits > 0 => (
                "◈",
                sk.warning,
                format!("{} cheered {} bits", msg.author.display_name, meta.bits),
            ),
            (_, Some(PlatformMeta::Twitch(meta))) if !meta.system_event.is_empty() => {
                ("★", sk.accent, msg.text.clone())
            }
            (MessageKind::Notice, _) => ("·", sk.muted, msg.text.clone()),
            _ => return None,
        }
    };
    let timestamp = msg
        .timestamp
        .map(|t| t.with_timezone(&chrono::Local).format("%H:%M").to_string())
        .unwrap_or_else(|| "--:--".into());
    Some(Line::from(vec![
        Span::styled(timestamp, Style::default().fg(sk.muted)),
        Span::raw(" "),
        Span::styled(glyph.to_string(), Style::default().fg(color)),
        Span::raw(" "),
        Span::raw(text),
    ]))
}

/// The activity view: the last high-signal events of the active chat,
/// newest at the bottom, scanning at most the newest 400 messages (yc's
/// bound) and showing at most 200 rows.
fn draw_activity(frame: &mut Frame, area: Rect, state: &ChatTabState, platform: Platform) {
    let sk = crate::theme::skin();
    let Some(key) = state.active_key(platform) else {
        return;
    };
    let Some(chat) = state.open.get(key) else {
        return;
    };
    let len = chat.state.messages.len();
    let mut lines: Vec<Line> = Vec::new();
    for index in (len.saturating_sub(400)..len).rev() {
        if lines.len() >= 200 || lines.len() >= area.height as usize {
            break;
        }
        if let Some(line) = chat.state.messages.get(index).and_then(activity_line) {
            lines.push(line);
        }
    }
    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            "No activity yet — cheers, Super Chats, memberships and removals land here.",
            Style::default().fg(sk.muted),
        )));
    }
    lines.reverse();
    frame.render_widget(Paragraph::new(lines), area);
}

/// The inspect panel (`K`, twi inspect.go / yc's equivalent): the normalized
/// message behind the selected row. A deleted message prints `text: removed`
/// — the words never reappear, not even here (these terminals are often on
/// stream).
fn draw_inspect(frame: &mut Frame, area: Rect, state: &ChatTabState) {
    let sk = crate::theme::skin();
    let Some(msg) = state.selected_message() else {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "Nothing selected — j/k picks a message to inspect.",
                Style::default().fg(sk.muted),
            ))),
            area,
        );
        return;
    };
    let label = |text: &str| Span::styled(format!("{text:<10}"), Style::default().fg(sk.accent));
    let mut lines = vec![
        Line::from(vec![label("id"), Span::raw(msg.id.clone())]),
        Line::from(vec![label("kind"), Span::raw(format!("{:?}", msg.kind))]),
        Line::from(vec![
            label("author"),
            Span::raw(format!(
                "{} (login {:?}, id {})",
                msg.author.display_name, msg.author.login, msg.author.id
            )),
        ]),
    ];
    if !msg.author.badges.is_empty() {
        let badges = msg
            .author
            .badges
            .iter()
            .map(|b| {
                if b.info.is_empty() {
                    format!("{}/{}", b.set, b.id)
                } else {
                    format!("{}/{} ({})", b.set, b.id, b.info)
                }
            })
            .collect::<Vec<_>>()
            .join(", ");
        lines.push(Line::from(vec![label("badges"), Span::raw(badges)]));
    }
    if let Some(timestamp) = msg.timestamp {
        lines.push(Line::from(vec![
            label("time"),
            Span::raw(timestamp.to_rfc3339()),
        ]));
    }
    match &msg.meta {
        Some(PlatformMeta::Twitch(meta)) => {
            if meta.bits > 0 {
                lines.push(Line::from(vec![
                    label("bits"),
                    Span::raw(meta.bits.to_string()),
                ]));
            }
            if !meta.system_event.is_empty() {
                lines.push(Line::from(vec![
                    label("event"),
                    Span::raw(meta.system_event.clone()),
                ]));
            }
            if meta.first_message {
                lines.push(Line::from(vec![label("first"), Span::raw("yes")]));
            }
        }
        Some(PlatformMeta::YouTube(meta)) => {
            lines.push(Line::from(vec![
                label("wire type"),
                Span::raw(meta.raw_type.clone()),
            ]));
            if let Some(paid) = &meta.paid {
                lines.push(Line::from(vec![
                    label("amount"),
                    Span::raw(format!(
                        "{} ({} micros {}, tier {})",
                        paid.display, paid.micros, paid.currency, paid.tier
                    )),
                ]));
            }
            if let Some(membership) = &meta.membership {
                lines.push(Line::from(vec![
                    label("member"),
                    Span::raw(format!("{:?} {}", membership.kind, membership.level)),
                ]));
            }
        }
        None => {}
    }
    let text = if msg.deleted {
        Span::styled("removed", Style::default().fg(sk.muted))
    } else {
        Span::raw(msg.text.clone())
    };
    lines.push(Line::from(vec![label("text"), text]));
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "K or esc closes the inspector.",
        Style::default().fg(sk.muted),
    )));
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), area);
}

fn draw_messages(frame: &mut Frame, area: Rect, state: &ChatTabState, platform: Platform) {
    let sk = crate::theme::skin();
    let Some(key) = state.active_key(platform) else {
        let hint = Paragraph::new(Line::from(Span::styled(
            "No chat open — press space then c to join one.",
            Style::default().fg(sk.muted),
        )));
        frame.render_widget(hint, area);
        return;
    };
    let Some(chat) = state.open.get(key) else {
        return;
    };

    let mut opts = state.render.clone();
    let height = area.height as usize;
    let len = chat.state.messages.len();
    let newest_visible = len.saturating_sub(chat.state.scroll);
    // The mentions filter needs to know who "you" are in this chat.
    let self_login = state
        .selected_account(platform)
        .and_then(|account| account.own_target.clone())
        .unwrap_or_default();
    let filters = chat.state.filters;

    // Walk backwards from the newest visible message, rendering (and
    // wrapping) until the pane is full, then flip the order — the cheap way
    // to keep the newest rows glued to the bottom whatever each message's
    // wrapped height is.
    let selected_index = chat
        .state
        .cursor
        .and_then(|cursor| len.checked_sub(1 + cursor));
    let mut lines: Vec<Line> = Vec::new();
    for index in (0..newest_visible).rev() {
        if lines.len() >= height {
            break;
        }
        let Some(msg) = chat.state.messages.get(index) else {
            break;
        };
        // Filters decide what is drawn, never what is retained — except the
        // selected row, which stays visible even when filtered out so
        // reply/moderation targets cannot be invisible (twi's rule).
        if !filters.matches(msg, &self_login) && Some(index) != selected_index {
            continue;
        }
        // Grouped layout: suppress the header when the previous (older)
        // message is from the same author — computed here because only the
        // draw pass knows the final visible order.
        // "Previous" means the previous *visible* row, not simply the one
        // before it in the ring: with a filter on, the header-carrying message
        // can be hidden, and suppressing the header of the row that is still
        // on screen left an authorless message with nothing above it.
        opts.continues_group = (0..index)
            .rev()
            .find(|older| {
                chat.state.messages.get(*older).is_some_and(|m| {
                    filters.matches(m, &self_login) || Some(*older) == selected_index
                })
            })
            .and_then(|prev| chat.state.messages.get(prev))
            .is_some_and(|prev| {
                !prev.author.id.is_empty() && prev.author.id == msg.author.id && !prev.deleted
            });
        let mut rendered = render_message(msg, area.width, &opts);
        if Some(index) == selected_index {
            // The selection is a background wash over the whole message so
            // reply/moderation targets are unmistakable.
            for line in &mut rendered {
                line.style = line.style.bg(sk.selection);
            }
        }
        rendered.reverse();
        lines.extend(rendered);
    }
    lines.truncate(height);
    lines.reverse();

    frame.render_widget(Paragraph::new(lines), area);
}

fn draw_composer(
    frame: &mut Frame,
    area: Rect,
    state: &ChatTabState,
    platform: Platform,
    focused: bool,
) {
    let sk = crate::theme::skin();
    let line = if focused {
        match &state.mode {
            ChatFocus::Join(buffer) => Line::from(vec![
                Span::styled("join: ", Style::default().fg(sk.accent)),
                Span::raw(buffer.clone()),
                Span::styled("▏", Style::default().fg(sk.accent)),
            ]),
            ChatFocus::EmojiPicker { query: buffer, .. } => {
                let mut spans = vec![
                    Span::styled("emoji: ", Style::default().fg(sk.accent)),
                    Span::raw(buffer.clone()),
                    Span::styled("▏ ", Style::default().fg(sk.accent)),
                ];
                for entry in crate::chat::emoji::search(buffer, 6) {
                    spans.push(Span::raw(format!("{} ", entry.emoji)));
                }
                spans.push(Span::styled(
                    " enter inserts the first",
                    Style::default().fg(sk.muted),
                ));
                Line::from(spans)
            }
            ChatFocus::Search(buffer) => Line::from(vec![
                Span::styled("/", Style::default().fg(sk.warning)),
                Span::raw(buffer.clone()),
                Span::styled("▏", Style::default().fg(sk.warning)),
                Span::styled(
                    "  enter keeps the query for n/N",
                    Style::default().fg(sk.muted),
                ),
            ]),
            ChatFocus::TimeoutPrompt(buffer) => Line::from(vec![
                Span::styled(
                    "timeout for: ",
                    Style::default().fg(sk.error).add_modifier(Modifier::BOLD),
                ),
                Span::raw(buffer.clone()),
                Span::styled("▏", Style::default().fg(sk.error)),
                Span::styled(
                    "  (45s / 5m / 2h, max 24h — enter applies, esc cancels)",
                    Style::default().fg(sk.muted),
                ),
            ]),
            ChatFocus::Compose => {
                let chat = state
                    .active_key(platform)
                    .and_then(|key| state.open.get(key));
                let draft = chat.map(|c| c.state.draft.clone()).unwrap_or_default();
                let mut spans = Vec::new();
                if let Some((_, name)) = chat.and_then(|c| c.state.reply_to.as_ref()) {
                    spans.push(Span::styled(
                        format!("↳ {name} "),
                        Style::default().fg(sk.accent),
                    ));
                }
                spans.push(Span::styled("> ", Style::default().fg(sk.accent)));
                spans.push(Span::raw(draft));
                spans.push(Span::styled("▏", Style::default().fg(sk.accent)));
                Line::from(spans)
            }
            ChatFocus::Normal => {
                if let Some(action) = state.pending_mod {
                    Line::from(Span::styled(
                        format!(
                            "press the same key again to {} — any other key cancels",
                            action.label()
                        ),
                        Style::default().fg(sk.error).add_modifier(Modifier::BOLD),
                    ))
                } else {
                    Line::from(Span::styled(
                        "i compose · j/k select · r reply · / search · 1-4 filters · d/t/b moderate · [ ] chats · space-c join",
                        Style::default().fg(sk.muted),
                    ))
                }
            }
        }
    } else {
        Line::from(Span::styled(
            "h/l to focus this pane",
            Style::default().fg(sk.muted),
        ))
    };
    frame.render_widget(Paragraph::new(line), area);
}

/// The per-pane empty state: what is missing and the exact commands that fix
/// it. The commands are the real ones this repository ships — discovered in
/// recon, not invented.
fn draw_empty_state(frame: &mut Frame, area: Rect, config: &Config, platform: Platform) {
    let sk = crate::theme::skin();
    let credentials_ready = config.check_credentials(&[platform]).is_ok();

    let mut lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            format!("No {} chat accounts yet.", platform.label()),
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
    ];
    // Everything named here happens inside the interface, because there is
    // nowhere else for it to happen: telling somebody to quit and run a
    // command was already poor advice and is now impossible advice.
    if !credentials_ready {
        lines.push(Line::from(format!(
            "1. Enter the {} API credentials on the setup screen.",
            platform.label()
        )));
        lines.push(Line::from(
            "2. Log in under Config → Accounts (alt+5), or on the",
        ));
        lines.push(Line::from("   Authorise your accounts screen."));
    } else {
        lines.push(Line::from(
            "Log in under Config → Accounts (alt+5), or on the",
        ));
        lines.push(Line::from("Authorise your accounts screen."));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(
        "Every logged-in account appears here as its own sub-tab.",
    ));

    let paragraph = Paragraph::new(lines)
        .style(Style::default().fg(sk.muted))
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, area);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tab_state(twitch: usize, youtube: usize) -> ChatTabState {
        let mut state = ChatTabState {
            focus: Platform::Twitch,
            mode: ChatFocus::Normal,
            pending_space: false,
            pending_mod: None,
            accounts: BTreeMap::new(),
            selected: BTreeMap::new(),
            split_percent: SPLIT_DEFAULT,
            activity: BTreeMap::new(),
            inspect: false,
            open: HashMap::new(),
            chats: BTreeMap::new(),
            active_chat: BTreeMap::new(),
            events_tx: mpsc::unbounded_channel().0,
            events_rx: None,
            http: reqwest::Client::new(),
            quota: crate::chat::youtube::QuotaStore::new(0, None),
            config: Config::default(),
            render: RenderOpts::default(),
            notifier: Notifier::new(false),
            logger: None,
            tab_visible: true,
        };
        for (platform, count) in [(Platform::Twitch, twitch), (Platform::YouTube, youtube)] {
            if count > 0 {
                state.accounts.insert(
                    platform,
                    (0..count)
                        .map(|i| AccountTab {
                            key: format!("{}:{i}", platform.slug()),
                            label: format!("acct{i}"),
                            own_target: Some(format!("own{i}")),
                            user_id: Some(format!("id{i}")),
                        })
                        .collect(),
                );
            }
        }
        state
    }

    #[test]
    fn focus_toggles_between_the_two_panes() {
        let mut state = tab_state(1, 1);
        assert_eq!(state.focus, Platform::Twitch);
        state.focus_other();
        assert_eq!(state.focus, Platform::YouTube);
        state.focus_other();
        assert_eq!(state.focus, Platform::Twitch);
    }

    // cycle_account lazily opens the newly selected account's own chat,
    // which spawns a task — hence the runtime.
    #[tokio::test]
    async fn cycling_accounts_wraps_and_ignores_single_account_panes() {
        let config = Config::default();
        let mut state = tab_state(3, 1);
        state.cycle_account(true, &config);
        state.cycle_account(true, &config);
        assert_eq!(state.selected[&Platform::Twitch], 2);
        state.cycle_account(true, &config);
        assert_eq!(state.selected[&Platform::Twitch], 0, "wraps forward");
        state.cycle_account(false, &config);
        assert_eq!(state.selected[&Platform::Twitch], 2, "wraps backward");

        // A single-account pane has nothing to cycle.
        state.focus = Platform::YouTube;
        state.cycle_account(true, &config);
        assert_eq!(*state.selected.get(&Platform::YouTube).unwrap_or(&0), 0);
    }

    #[test]
    fn the_split_resizes_toward_the_focused_pane_and_clamps() {
        let mut state = tab_state(1, 1);
        // Growing the focused (Twitch, left) pane raises the percentage.
        state.resize(true);
        assert_eq!(state.split_percent, SPLIT_DEFAULT + SPLIT_STEP);
        // From the YouTube pane, "grow" means shrinking the left share.
        state.focus = Platform::YouTube;
        for _ in 0..100 {
            state.resize(true);
        }
        assert_eq!(state.split_percent, SPLIT_MIN, "clamped at the minimum");
        state.focus = Platform::Twitch;
        for _ in 0..100 {
            state.resize(true);
        }
        assert_eq!(state.split_percent, SPLIT_MAX, "clamped at the maximum");
        state.reset_split();
        assert_eq!(state.split_percent, SPLIT_DEFAULT);
    }

    #[test]
    fn selected_account_is_none_only_when_the_pane_is_empty() {
        let state = tab_state(2, 0);
        assert!(state.selected_account(Platform::Twitch).is_some());
        assert!(state.selected_account(Platform::YouTube).is_none());
    }

    /// Activation lazily opens the selected accounts' own chats — and only
    /// those, not every account's.
    #[tokio::test]
    async fn activation_opens_the_own_chat_of_every_logged_in_account() {
        let config = Config::default();
        let mut state = tab_state(2, 1);
        assert!(state.open.is_empty(), "nothing connects at startup");

        state.activate(&config);

        assert_eq!(state.open.len(), 3, "two Twitch accounts and one YouTube");
        assert_eq!(
            state.active_key(Platform::Twitch).unwrap().target,
            "own0",
            "the first account is the one on screen"
        );
        assert!(
            state.open.keys().any(|key| key.account == "twitch:1"),
            "an off-screen account still collects its messages, so its unread \
             count means something"
        );
    }

    /// Opening the same target twice switches to it instead of spawning a
    /// second connection.
    #[tokio::test]
    async fn opening_the_same_chat_twice_does_not_duplicate_it() {
        let config = Config::default();
        let mut state = tab_state(1, 0);
        let account = state.selected_account(Platform::Twitch).unwrap().clone();
        state.open_chat(&config, Platform::Twitch, &account, "SomeChannel".into());
        state.open_chat(&config, Platform::Twitch, &account, "#somechannel".into());
        assert_eq!(state.open.len(), 1, "the two spellings are one channel");
    }

    /// Closing the active chat drops its handle, and the chat strip index
    /// stays in bounds.
    #[tokio::test]
    async fn closing_the_active_chat_drops_it_and_clamps_the_index() {
        let config = Config::default();
        let mut state = tab_state(1, 0);
        let account = state.selected_account(Platform::Twitch).unwrap().clone();
        state.open_chat(&config, Platform::Twitch, &account, "one".into());
        state.open_chat(&config, Platform::Twitch, &account, "two".into());
        assert_eq!(state.open.len(), 2);

        state.close_active_chat();
        assert_eq!(state.open.len(), 1);
        assert!(state.active_key(Platform::Twitch).is_some());

        state.close_active_chat();
        assert!(state.open.is_empty());
        assert!(state.active_key(Platform::Twitch).is_none());
        // Closing with nothing open must be harmless.
        state.close_active_chat();
    }

    /// The composer edits per chat, deletes whole graphemes, and a delivered
    /// event lands in the right chat's state.
    #[tokio::test]
    async fn composer_and_events_operate_on_the_active_chat() {
        let config = Config::default();
        let mut state = tab_state(1, 0);
        let account = state.selected_account(Platform::Twitch).unwrap().clone();
        state.open_chat(&config, Platform::Twitch, &account, "chan".into());

        for c in "hi 👋".chars() {
            state.compose_push(c);
        }
        state.compose_backspace();
        let key = state.active_key(Platform::Twitch).unwrap().clone();
        assert_eq!(state.open[&key].state.draft, "hi ", "one grapheme removed");

        state.handle_event(
            key.clone(),
            ChatEvent::Connection {
                status: ConnectionStatus::Connected,
                detail: String::new(),
            },
        );
        assert_eq!(
            state.open[&key].state.connection.0,
            ConnectionStatus::Connected
        );
    }

    /// Selection follows vim keys, the view follows the selection, and
    /// reply picks up the selected message's id and author.
    #[tokio::test]
    async fn selection_moves_and_arms_replies() {
        let config = Config::default();
        let mut state = tab_state(1, 0);
        let account = state.selected_account(Platform::Twitch).unwrap().clone();
        state.open_chat(&config, Platform::Twitch, &account, "chan".into());
        let key = state.active_key(Platform::Twitch).unwrap().clone();
        for i in 0..3 {
            let mut m = local_notice("hello");
            m.id = format!("m{i}");
            m.author.display_name = format!("author{i}");
            state.handle_event(key.clone(), ChatEvent::Message(Box::new(m)));
        }

        state.select_move(1); // newest first
        state.select_move(1); // one older
        assert_eq!(state.open[&key].state.cursor, Some(1));
        assert_eq!(state.open[&key].state.scroll, 1, "the view follows");
        assert_eq!(state.selected_message().unwrap().id, "m1");

        assert!(state.reply_to_selected());
        assert_eq!(
            state.open[&key].state.reply_to.as_ref().unwrap().1,
            "author1"
        );

        state.clear_selection();
        assert!(state.open[&key].state.cursor.is_none());
    }

    /// Moderation never fires on one keystroke: the first press arms, the
    /// same key confirms, and the confirmed command carries the selected
    /// message's identifiers.
    #[tokio::test]
    async fn moderation_requires_a_confirming_second_press() {
        let config = Config::default();
        let mut state = tab_state(1, 0);
        let account = state.selected_account(Platform::Twitch).unwrap().clone();
        state.open_chat(&config, Platform::Twitch, &account, "chan".into());
        let key = state.active_key(Platform::Twitch).unwrap().clone();

        // Swap the spawned task's channel for a captive one so the test can
        // see exactly which commands leave the UI.
        let (tx, mut rx) = mpsc::channel(8);
        state.open.get_mut(&key).unwrap().handle.commands = tx;

        let mut m = local_notice("bad message");
        m.id = "target".into();
        m.author.id = "UCbad".into();
        state.handle_event(key.clone(), ChatEvent::Message(Box::new(m)));
        state.select_move(1);

        state.moderate(ModAction::Delete);
        assert_eq!(
            state.pending_mod,
            Some(ModAction::Delete),
            "armed, not fired"
        );
        assert!(rx.try_recv().is_err(), "nothing sent on the first press");

        state.moderate(ModAction::Delete);
        assert!(state.pending_mod.is_none());
        match rx.try_recv().expect("the confirmed action must be sent") {
            ChatCommand::Delete { message_id } => assert_eq!(message_id, "target"),
            other => panic!("expected a delete, got {other:?}"),
        }

        // A timeout goes through the duration prompt; the parsed duration
        // reaches the wire with the author's channel id.
        state.select_move(1);
        state.timeout_selected(parse_timeout("10m").unwrap());
        match rx.try_recv().expect("the timeout must be sent") {
            ChatCommand::Ban {
                channel_id,
                timeout_secs,
            } => {
                assert_eq!(channel_id, "UCbad");
                assert_eq!(timeout_secs, Some(600));
            }
            other => panic!("expected a ban, got {other:?}"),
        }
    }

    /// The armed-moderation race: a message arriving between the arming and
    /// the confirming press must not shift what the confirmation acts on.
    #[tokio::test]
    async fn a_message_arriving_mid_confirmation_cannot_change_the_target() {
        let config = Config::default();
        let mut state = tab_state(1, 0);
        let account = state.selected_account(Platform::Twitch).unwrap().clone();
        state.open_chat(&config, Platform::Twitch, &account, "chan".into());
        let key = state.active_key(Platform::Twitch).unwrap().clone();
        let (tx, mut rx) = mpsc::channel(8);
        state.open.get_mut(&key).unwrap().handle.commands = tx;

        let mut troll = local_notice("bad");
        troll.id = "troll-msg".into();
        troll.author.id = "UCtroll".into();
        state.handle_event(key.clone(), ChatEvent::Message(Box::new(troll)));
        state.select_move(1);
        state.moderate(ModAction::Ban);
        assert_eq!(state.pending_mod, Some(ModAction::Ban));

        // An innocent viewer speaks between the two presses.
        let mut innocent = local_notice("hi!");
        innocent.id = "innocent-msg".into();
        innocent.author.id = "UCinnocent".into();
        state.handle_event(key.clone(), ChatEvent::Message(Box::new(innocent)));

        state.moderate(ModAction::Ban);
        match rx.try_recv().expect("the ban must be sent") {
            ChatCommand::Ban { channel_id, .. } => {
                assert_eq!(channel_id, "UCtroll", "the ban must hit the armed target");
            }
            other => panic!("expected a ban, got {other:?}"),
        }
    }

    /// Starting a selection while scrolled back picks the row at the view's
    /// edge instead of yanking the reader to the newest message.
    #[tokio::test]
    async fn starting_a_selection_respects_the_scroll_position() {
        let config = Config::default();
        let mut state = tab_state(1, 0);
        let account = state.selected_account(Platform::Twitch).unwrap().clone();
        state.open_chat(&config, Platform::Twitch, &account, "chan".into());
        let key = state.active_key(Platform::Twitch).unwrap().clone();
        for i in 0..10 {
            let mut m = local_notice("x");
            m.id = format!("m{i}");
            state.handle_event(key.clone(), ChatEvent::Message(Box::new(m)));
        }
        state.scroll_by(5);
        state.select_move(1);
        assert_eq!(
            state.open[&key].state.cursor,
            Some(5),
            "selection starts at the scrolled view's edge, not the newest row"
        );
    }

    #[test]
    fn timeout_durations_parse_with_units_and_cap() {
        assert_eq!(parse_timeout("45s"), Some(45));
        assert_eq!(parse_timeout("5m"), Some(300));
        assert_eq!(parse_timeout("2h"), Some(7200));
        assert_eq!(parse_timeout("7"), Some(420), "bare numbers are minutes");
        assert_eq!(parse_timeout("48h"), Some(24 * 3600), "capped at a day");
        assert_eq!(parse_timeout("0m"), None, "zero is not a punishment");
        assert_eq!(parse_timeout("soon"), None);
        assert_eq!(parse_timeout(""), None);
    }

    /// Search jumps to the newest match and n walks older without wrapping.
    #[tokio::test]
    async fn search_finds_matches_and_never_wraps() {
        let config = Config::default();
        let mut state = tab_state(1, 0);
        let account = state.selected_account(Platform::Twitch).unwrap().clone();
        state.open_chat(&config, Platform::Twitch, &account, "chan".into());
        let key = state.active_key(Platform::Twitch).unwrap().clone();
        for (i, text) in ["hello", "cake time", "bye", "more cake"]
            .iter()
            .enumerate()
        {
            let mut m = local_notice(text);
            m.id = format!("m{i}");
            state.handle_event(key.clone(), ChatEvent::Message(Box::new(m)));
        }

        state.search_jump_newest("cake");
        assert_eq!(state.open[&key].state.cursor, Some(0), "newest match first");
        state.commit_search("cake".into());

        state.search_step(true); // n → older match
        assert_eq!(state.open[&key].state.cursor, Some(2));
        state.search_step(true); // no older match: stay put, never wrap
        assert_eq!(state.open[&key].state.cursor, Some(2));
        state.search_step(false); // N → newer again
        assert_eq!(state.open[&key].state.cursor, Some(0));
    }

    /// After clearing the selection, `n` must still consider the newest row:
    /// the walk used to step past it before testing anything, so a match on
    /// the very last message was unreachable.
    #[tokio::test]
    async fn search_step_tests_the_newest_row_when_nothing_is_selected() {
        let config = Config::default();
        let mut state = tab_state(1, 0);
        let account = state.selected_account(Platform::Twitch).unwrap().clone();
        state.open_chat(&config, Platform::Twitch, &account, "chan".into());
        let key = state.active_key(Platform::Twitch).unwrap().clone();
        for (i, text) in ["hello", "cake time"].iter().enumerate() {
            let mut m = local_notice(text);
            m.id = format!("m{i}");
            state.handle_event(key.clone(), ChatEvent::Message(Box::new(m)));
        }

        state.commit_search("cake".into());
        state.clear_selection();
        state.search_step(true);

        assert_eq!(
            state.open[&key].state.cursor,
            Some(0),
            "the newest row holds the only match"
        );
    }

    /// A send the chat task refuses keeps the draft; it must keep the reply
    /// target too, or the retry silently goes out as an ordinary message.
    #[tokio::test]
    async fn a_refused_send_keeps_the_reply_target() {
        let config = Config::default();
        let mut state = tab_state(1, 0);
        let account = state.selected_account(Platform::Twitch).unwrap().clone();
        state.open_chat(&config, Platform::Twitch, &account, "chan".into());
        let key = state.active_key(Platform::Twitch).unwrap().clone();

        {
            let chat = state.open.get_mut(&key).unwrap();
            chat.state.reply_to = Some(("parent-id".into(), "someone".into()));
            chat.state.draft = "sure thing".into();
            // Fill the command queue so the next send cannot be accepted.
            while chat
                .handle
                .commands
                .try_send(ChatCommand::Send {
                    text: "filler".into(),
                    reply_to: None,
                })
                .is_ok()
            {}
        }

        state.compose_send();

        let chat = &state.open[&key];
        assert_eq!(chat.state.draft, "sure thing", "the draft is kept");
        assert!(
            chat.state.reply_to.is_some(),
            "the reply target must survive a refused send"
        );
    }

    /// Filters are view predicates with union semantics; the mentions filter
    /// is word-anchored.
    #[test]
    fn filters_union_and_anchor_mentions() {
        use crate::chat::state::Filters;
        let mut chat = local_notice("hi @streamer!");
        chat.kind = MessageKind::Chat;
        let mut filters = Filters::default();
        assert!(filters.matches(&chat, "streamer"), "no filters passes all");

        filters.mentions = true;
        assert!(filters.matches(&chat, "streamer"));
        let longer = {
            let mut m = local_notice("hey @streamer_two");
            m.kind = MessageKind::Chat;
            m
        };
        assert!(
            !filters.matches(&longer, "streamer"),
            "@streamer_two must not match @streamer"
        );

        // Union: adding the notices filter lets a notice through even though
        // it mentions nobody.
        filters.notices = true;
        assert!(filters.matches(&local_notice("plain notice"), "streamer"));
    }

    /// Tab completes the trailing @mention from the roster, prefix-first.
    #[tokio::test]
    async fn mention_completion_replaces_the_typed_prefix() {
        let config = Config::default();
        let mut state = tab_state(1, 0);
        let account = state.selected_account(Platform::Twitch).unwrap().clone();
        state.open_chat(&config, Platform::Twitch, &account, "chan".into());
        let key = state.active_key(Platform::Twitch).unwrap().clone();
        let mut m = local_notice("hello");
        m.author.id = "42".into();
        m.author.login = "chatperson".into();
        m.author.display_name = "ChatPerson".into();
        state.handle_event(key.clone(), ChatEvent::Message(Box::new(m)));

        for c in "hey @chatp".chars() {
            state.compose_push(c);
        }
        state.complete_mention();
        assert_eq!(state.open[&key].state.draft, "hey @ChatPerson ");

        // No @word at the caret: completion must not touch the draft.
        state.compose_push('!');
        state.complete_mention();
        assert_eq!(state.open[&key].state.draft, "hey @ChatPerson !");
    }

    /// The display toggles cycle through every mode and back.
    #[test]
    fn display_toggles_cycle() {
        let mut state = tab_state(1, 0);
        assert_eq!(state.render.layout, MessageLayout::Inline);
        state.cycle_layout();
        assert_eq!(state.render.layout, MessageLayout::Grouped);
        state.cycle_layout();
        state.cycle_layout();
        assert_eq!(state.render.layout, MessageLayout::Inline);

        assert_eq!(state.render.badge_mode, BadgeMode::Glyph);
        state.cycle_badges();
        state.cycle_badges();
        state.cycle_badges();
        assert_eq!(state.render.badge_mode, BadgeMode::Glyph);

        state.toggle_highlight();
        assert!(!state.render.highlight_emotes);
        state.toggle_full_username();
        assert!(state.render.full_username);
    }

    /// A command must be the whole word: "/chatstats" is a message.
    #[tokio::test]
    async fn a_slash_word_that_merely_starts_with_a_command_is_not_hijacked() {
        let config = Config::default();
        let mut state = tab_state(1, 0);
        let account = state.selected_account(Platform::Twitch).unwrap().clone();
        state.open_chat(&config, Platform::Twitch, &account, "chan".into());
        let key = state.active_key(Platform::Twitch).unwrap().clone();
        let (tx, mut rx) = mpsc::channel(8);
        state.open.get_mut(&key).unwrap().handle.commands = tx;

        for ch in "/chatstats".chars() {
            state.compose_push(ch);
        }
        state.compose_send();
        match rx.try_recv().expect("the text must be sent, not hijacked") {
            ChatCommand::Send { text, .. } => assert_eq!(text, "/chatstats"),
            other => panic!("expected a send, got {other:?}"),
        }
        assert_eq!(state.open.len(), 1, "no bogus channel was joined");
    }

    /// View scrolling drops the selection so moderation can never act on an
    /// off-screen row; the armed confirmation goes with it.
    #[tokio::test]
    async fn view_scrolling_clears_the_selection_and_armed_confirmation() {
        let config = Config::default();
        let mut state = tab_state(1, 0);
        let account = state.selected_account(Platform::Twitch).unwrap().clone();
        state.open_chat(&config, Platform::Twitch, &account, "chan".into());
        let key = state.active_key(Platform::Twitch).unwrap().clone();
        for i in 0..10 {
            let mut m = local_notice("x");
            m.id = format!("m{i}");
            state.handle_event(key.clone(), ChatEvent::Message(Box::new(m)));
        }
        state.select_move(1);
        state.moderate(ModAction::Delete);
        assert!(state.pending_mod.is_some());

        state.scroll_by(5);
        assert!(state.open[&key].state.cursor.is_none(), "selection dropped");
        assert!(state.pending_mod.is_none(), "confirmation disarmed");
    }

    /// With a filter active, g jumps to the oldest *visible* message and
    /// search never selects a hidden row.
    #[tokio::test]
    async fn filtered_scrolling_and_search_land_on_visible_rows() {
        let config = Config::default();
        let mut state = tab_state(1, 0);
        let account = state.selected_account(Platform::Twitch).unwrap().clone();
        state.open_chat(&config, Platform::Twitch, &account, "chan".into());
        let key = state.active_key(Platform::Twitch).unwrap().clone();
        // Oldest message is plain chat; only the middle one is a notice.
        for (i, kind) in [MessageKind::Chat, MessageKind::Notice, MessageKind::Chat]
            .into_iter()
            .enumerate()
        {
            let mut m = local_notice(&format!("row {i} cake"));
            m.id = format!("m{i}");
            m.kind = kind;
            state.handle_event(key.clone(), ChatEvent::Message(Box::new(m)));
        }
        state.toggle_filter('4'); // notices only

        state.scroll_to_end(true);
        assert_eq!(
            state.open[&key].state.scroll, 1,
            "g lands on the oldest visible (the notice), not the hidden oldest"
        );

        state.search_jump_newest("cake");
        assert_eq!(
            state.open[&key].state.cursor,
            Some(1),
            "search must not select a filtered-out match"
        );
    }

    /// Scrolling clamps at both ends and g/G jump to them.
    #[tokio::test]
    async fn scrolling_is_bounded() {
        let config = Config::default();
        let mut state = tab_state(1, 0);
        let account = state.selected_account(Platform::Twitch).unwrap().clone();
        state.open_chat(&config, Platform::Twitch, &account, "chan".into());
        let key = state.active_key(Platform::Twitch).unwrap().clone();
        for i in 0..5 {
            state.handle_event(
                key.clone(),
                ChatEvent::Message(Box::new(local_notice(&format!("m{i}")))),
            );
        }
        state.scroll_by(100);
        assert_eq!(state.open[&key].state.scroll, 4, "clamped at the oldest");
        state.scroll_by(-100);
        assert_eq!(state.open[&key].state.scroll, 0, "clamped at the newest");
        state.scroll_to_end(true);
        assert_eq!(state.open[&key].state.scroll, 4);
        state.scroll_to_end(false);
        assert_eq!(state.open[&key].state.scroll, 0);
    }
}
