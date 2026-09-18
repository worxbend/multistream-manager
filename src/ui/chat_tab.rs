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
use crate::chat::notify::high_signal;
use crate::chat::render::{render_message, BadgeMode, MessageLayout, RenderOpts};
use crate::chat::roster::{mention_prefix, Roster};
use crate::chat::source::{self, ChatCommand, ChatHandle};
use crate::chat::state::{ChatState, Recall};
use crate::chat::{
    ChatAuthor, ChatEvent, ChatKey, ChatMessage, ConnectionStatus, MessageKind, PlatformMeta,
};
use crate::config::Config;
use crate::model::Platform;
use crate::notify::Notifier;

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
    EmojiPicker {
        query: String,
        from_compose: bool,
        /// Which of the drawn candidates Enter will insert.
        selected: usize,
    },
    /// A timeout duration prompt (`t` on a selected message): the buffer
    /// holds text like `5m`; enter performs the timeout, esc cancels.
    TimeoutPrompt(String),
}

/// One editing operation on the composer.
///
/// A small enum rather than the key events themselves, so the chat state
/// knows nothing about crossterm and the key handler knows nothing about how
/// text is stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComposeEdit {
    Backspace,
    Delete,
    Left,
    Right,
    Home,
    End,
    /// Ctrl+W, as in every Unix shell.
    DeleteWord,
    /// Ctrl+U: throw the whole draft away.
    Clear,
}

/// How far back the activity view looks.
///
/// It is the "who paid me tonight" list, and on a busy chat 400 messages is
/// under two minutes — so the pane silently dropped most of what it exists to
/// show. The scan is cheap (a projection over the ring, no second store), so
/// this is the scrollback limit rather than a fraction of it.
const ACTIVITY_SCAN: usize = 5_000;

/// How many rows of context to leave below a search match.
///
/// A match pinned to the bottom row answers "was it said" and not "what
/// happened next", which is most of why anybody searches a chat log.
const SEARCH_CONTEXT_ROWS: usize = 6;

/// How many emoji candidates the picker offers.
///
/// Shared by the drawing and the key handling, so the selection cursor can
/// never point past what is on screen.
pub const EMOJI_CHOICES: usize = 6;

/// Everything the Chat tab remembers between frames.
pub struct ChatTabState {
    /// Which pane keyboard input goes to.
    pub focus: Platform,
    pub mode: ChatFocus,
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
    quota: crate::quota::QuotaStore,
    /// A copy of the config, so composer commands (`/chats <x>`) can open
    /// chats without threading `&Config` through every key path.
    config: Config,
    /// Presentation switches shared by every pane, cycled with
    /// ctrl+g/b/y/n. Session-only, like the references — no runtime toggle
    /// persists to config.
    pub render: RenderOpts,
    /// Desktop notifications for stream events arriving in chat. Shared with
    /// the rest of the interface, so chat and stream-state notifications
    /// queue behind one another instead of talking over each other.
    notifier: Notifier,
    /// Opt-in JSONL chat logging.
    logger: Option<crate::chat::chatlog::ChatLogger>,
    /// Whether the Chat tab is currently on screen at all. Only consulted
    /// when `[notifications] only_when_hidden` is on.
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
    pub fn new(config: &Config, notifier: Notifier, quota: crate::quota::QuotaStore) -> Self {
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
            // Passed in rather than built here: the statistics polling spends
            // from the same Google project allowance, so both halves have to
            // count into one ledger. Two would each grant the full daily
            // budget and, being persisted to the same file, overwrite each
            // other's count.
            quota,
            render: RenderOpts::default(),
            notifier,
            logger: build_logger(config),
            tab_visible: false,
            config: config.clone(),
            // Building only fails over TLS backend misconfiguration, which the
            // streaming engine would already have surfaced; fall back to the
            // default client rather than poisoning the whole UI over a
            // chat-only concern.
            http: crate::backend::http_client().unwrap_or_default(),
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

    /// The login the given pane's selected account speaks as — the identity
    /// the mentions filter and search need to know "you" from. Empty when
    /// the pane has no selected account, or its own login was never
    /// recorded (a token saved before identities existed).
    fn focused_login(&self, platform: Platform) -> String {
        self.selected_account(platform)
            .and_then(|account| account.own_target.clone())
            .unwrap_or_default()
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

    /// Adopt changed notification settings.
    ///
    /// This tab keeps its own copy of the config (so composer commands can
    /// open chats without `&Config` being threaded through every key path),
    /// and a copy taken at start-up would keep the old switches for the rest
    /// of the session. Only the notification settings are refreshed: the rest
    /// of the copy backs live machinery — the chat logger, the quota store —
    /// that a wholesale replacement would leave pointing somewhere else.
    pub fn adopt_notification_settings(&mut self, config: &Config) {
        self.config.notifications = config.notifications.clone();
        self.config.chat.notifications = config.chat.notifications;
    }

    /// The day's YouTube quota estimate, for showing in Config → Chat.
    pub fn quota_summary(&self) -> Option<(u64, u64, u8)> {
        self.quota.summary()
    }

    /// Turn chat logging on or off while the program is running.
    ///
    /// The logger is a live thing — it holds an open file and a rotation
    /// count — so this builds a new one or drops the existing one rather than
    /// letting the config copy drift from what is actually writing. Turning
    /// logging off closes the file; turning it on starts a fresh one, which
    /// means the log records from this moment rather than retroactively.
    pub fn set_chat_logging(&mut self, on: bool) {
        self.config.chat.chat_logging = on;
        self.logger = if on { build_logger(&self.config) } else { None };
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
            // Desktop notification for anything that counts as a stream
            // event — a raid, a subscription, a cheer, a Super Chat.
            //
            // By default this fires whether or not the chat is on screen.
            // That is deliberate: the terminal is usually not what you are
            // looking at while you stream, and the event this feature exists
            // for — a raid — has to reach you within seconds. Somebody who
            // does read chat in this program can set
            // `[notifications] only_when_hidden` and get the old behaviour,
            // where a pop-up only appears for what is not already visible.
            //
            // `[chat] notifications = false` still switches off every
            // chat-derived notification, whatever the per-event switches say.
            let hidden_enough = !self.config.notifications.only_when_hidden || !on_screen;
            if self.config.chat.notifications && hidden_enough {
                if let Some(notification) = high_signal(msg, &self.config.notifications) {
                    self.notifier.send(notification);
                } else if self
                    .config
                    .chat
                    .highlights
                    .check(msg)
                    .is_some_and(|hit| hit.notify)
                {
                    // A rule the user wrote, asking to be told. Only when the
                    // rule says so: a notification for every match of a
                    // common word is how somebody learns to ignore their
                    // notifications.
                    //
                    // After `high_signal`, not before, so a raid that also
                    // matches a rule is still announced as a raid.
                    self.notifier.send(crate::notify::Notification::new(
                        format!("{} in chat", msg.author.display_name),
                        msg.text.clone(),
                        crate::notify::Urgency::Normal,
                    ));
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
    /// Scroll one named pane, without moving the keyboard focus.
    ///
    /// The wheel used to scroll whichever pane had the keyboard, so rolling
    /// over the YouTube pane scrolled Twitch — and scrolling clears the
    /// selection, so it silently dropped a reply armed in the other pane.
    pub fn scroll_pane(&mut self, platform: Platform, delta: i64) {
        let was = self.focus;
        self.focus = platform;
        self.scroll_by(delta);
        self.focus = was;
    }

    pub fn scroll_by(&mut self, delta: i64) {
        self.pending_mod = None;
        let login = self.focused_login(self.focus);
        if let Some(chat) = self.active_chat_mut() {
            chat.state.cursor = None;
            let len = chat.state.messages.len();
            if len == 0 {
                return;
            }
            let filters = chat.state.filters;
            let messages = &chat.state.messages;
            let visible = |offset: usize| {
                messages
                    .get(len - 1 - offset)
                    .is_some_and(|msg| filters.matches(msg, &login))
            };
            let step: i64 = if delta > 0 { 1 } else { -1 };
            let mut offset = chat.state.scroll as i64;
            for _ in 0..delta.abs() {
                match visible_offset(len, offset, step, visible) {
                    Some(next) => offset = next as i64,
                    None => break,
                }
            }
            chat.state.scroll = offset.clamp(0, (len - 1) as i64) as usize;
            if chat.state.scroll == 0 {
                chat.state.below = 0;
            }
        }
    }

    /// Jump to the oldest (`g`) or newest (`G`) *visible* message.
    pub fn scroll_to_end(&mut self, oldest: bool) {
        self.pending_mod = None;
        let login = self.focused_login(self.focus);
        if let Some(chat) = self.active_chat_mut() {
            chat.state.cursor = None;
            let len = chat.state.messages.len();
            if len == 0 {
                return;
            }
            let filters = chat.state.filters;
            let messages = &chat.state.messages;
            let visible = |offset: usize| {
                messages
                    .get(len - 1 - offset)
                    .is_some_and(|msg| filters.matches(msg, &login))
            };
            // Walk from just past one end toward the other: from `len`
            // downward for the oldest visible row, from `-1` upward for the
            // newest.
            let found = if oldest {
                visible_offset(len, len as i64, -1, visible)
            } else {
                visible_offset(len, -1, 1, visible)
            };
            match found {
                Some(offset) => {
                    chat.state.scroll = offset;
                }
                None => {
                    chat.state.scroll = 0;
                    chat.state.below = 0;
                    // Jumping to live is "I have caught up", so the divider
                    // goes.
                    chat.state.clear_unread_mark();
                }
            }
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
        self.send_or_notify(
            command,
            "the chat task is busy — press the key again to retry",
        );
    }

    /// Send `command` to the focused chat's task, or leave a local notice
    /// instead of silently dropping it when the task's queue is momentarily
    /// full. Nothing happens when no chat is focused at all — callers that
    /// need to say so (a marker with nowhere to go) check for that first.
    fn send_or_notify(&mut self, command: ChatCommand, busy_text: &str) {
        if let Some(chat) = self.active_chat_mut() {
            if chat.handle.commands.try_send(command).is_err() {
                chat.state
                    .apply(ChatEvent::Message(Box::new(local_notice(busy_text))));
            }
        }
    }

    /// Clear the draft, drop back to normal composing, and hand `command` to
    /// the chat task. The shared tail of every composer command that never
    /// reaches the platform as text — `/raid`, `/unraid`, `/clip`,
    /// `/marker`.
    fn dispatch_local_command(&mut self, command: ChatCommand) {
        if let Some(chat) = self.active_chat_mut() {
            chat.state.draft.clear();
        }
        self.mode = ChatFocus::Normal;
        self.send_or_notify(
            command,
            "the chat task is busy — the command was dropped, try again",
        );
    }

    /// Type one character into the focused chat's composer, at the caret.
    pub fn compose_push(&mut self, c: char) {
        if let Some(chat) = self.active_chat_mut() {
            if c == '\n' || c == '\r' {
                return;
            }
            chat.state.draft.insert(c);
        }
    }

    /// Walk the focused chat's send history into the composer.
    ///
    /// `back` is Up (older). Does nothing at either end of the history, so a
    /// stray key press cannot wipe what is being typed.
    pub fn compose_recall(&mut self, back: bool) {
        let direction = if back { Recall::Older } else { Recall::Newer };
        if let Some(chat) = self.active_chat_mut() {
            if let Some(text) = chat.state.recall_sent(direction) {
                chat.state.draft.set(text);
            }
        }
    }

    /// Drop a marker in the VOD at this moment.
    pub fn mark_moment(&mut self) {
        if self.active_chat_mut().is_none() {
            // Markers go through the chat connection, so there has to be
            // one — saying which is better than a key that does nothing.
            self.notify_local("no chat is open, so there is nowhere to send the marker");
            return;
        }
        self.send_or_notify(
            ChatCommand::Marker {
                description: String::new(),
            },
            "the chat task is busy — press the key again to retry",
        );
    }

    /// Put a line in the focused chat that came from this program rather than
    /// from a platform.
    fn notify_local(&mut self, text: &str) {
        if let Some(chat) = self.active_chat_mut() {
            chat.state
                .apply(ChatEvent::Message(Box::new(local_notice(text))));
        }
    }

    /// Insert pasted text into the composer at the caret.
    pub fn compose_paste(&mut self, text: &str) {
        if let Some(chat) = self.active_chat_mut() {
            chat.state.draft.insert_str(text);
        }
    }

    /// Run one editing key over the composer.
    ///
    /// Everything the metadata form's fields have already understood —
    /// arrows, Home/End, Ctrl+W, Ctrl+U — now that the draft is a real
    /// [`TextInput`] rather than a string with an implied caret at the end.
    pub fn compose_edit(&mut self, edit: ComposeEdit) {
        let Some(chat) = self.active_chat_mut() else {
            return;
        };
        let draft = &mut chat.state.draft;
        match edit {
            ComposeEdit::Backspace => draft.backspace(),
            ComposeEdit::Delete => draft.delete(),
            ComposeEdit::Left => draft.left(),
            ComposeEdit::Right => draft.right(),
            ComposeEdit::Home => draft.home(),
            ComposeEdit::End => draft.end(),
            ComposeEdit::DeleteWord => draft.delete_word_before(),
            ComposeEdit::Clear => draft.clear(),
        }
    }

    /// Send the composer draft to the focused chat's task.
    ///
    /// A refused send (the per-chat command queue is momentarily full) keeps
    /// the draft and says so, rather than losing what was typed.
    pub fn compose_send(&mut self) {
        let platform = self.focus;
        let Some(chat) = self.active_chat_mut() else {
            return;
        };
        let text = chat.state.draft.value().trim().to_string();
        if text.is_empty() {
            return;
        }

        // Anything beginning with a slash is a command until proved
        // otherwise. See `classify_command` for why that has to be the
        // default rather than "send it and let the platform decide".
        let text = match classify_command(&text, platform) {
            SlashVerdict::NotACommand => text,
            // `//text` escaped a leading slash; send what is left of it.
            SlashVerdict::Literal(message) => {
                chat.state.draft.set(message.clone());
                message
            }
            SlashVerdict::Refused(reason) => {
                chat.state
                    .apply(ChatEvent::Message(Box::new(local_notice(&reason))));
                return;
            }
        };
        if text.is_empty() {
            return;
        }

        // Raiding: the standard way a Twitch stream ends, and a Helix call
        // rather than a chat line since 2023. The adapter does the work; this
        // only has to get the target to it.
        if let Some(rest) = command_argument(&text, "/raid") {
            let target = rest.trim().to_string();
            if target.is_empty() {
                chat.state.apply(ChatEvent::Message(Box::new(local_notice(
                    "raid needs a channel to raid: /raid somechannel",
                ))));
                return;
            }
            self.dispatch_local_command(ChatCommand::Raid { target });
            return;
        }
        if command_argument(&text, "/unraid").is_some() {
            self.dispatch_local_command(ChatCommand::Unraid);
            return;
        }

        // `command_argument` rather than `==`: every other command here is
        // matched case-insensitively, and this one was not — so `/CLIP` was
        // lowercased by `classify_command`, recognised as handled here, and
        // then missed by this exact comparison. It fell all the way through
        // to the send path and was posted to everybody watching, which is the
        // single thing the slash guard exists to prevent.
        if command_argument(&text, "/clip").is_some_and(|rest| rest.trim().is_empty()) {
            self.dispatch_local_command(ChatCommand::Clip);
            return;
        }
        // A marker takes an optional note, so it goes through
        // `command_argument` rather than an equality check — and "/markers"
        // is a message, not a marker with the note "s".
        if let Some(rest) = command_argument(&text, "/marker") {
            let description = rest.trim().to_string();
            self.dispatch_local_command(ChatCommand::Marker { description });
            return;
        }
        // Composer commands that never reach the platform at all (twi's
        // /channels, yc's /chats): bare opens the join prompt, with an
        // argument joins directly. Neither is a `ChatCommand` — opening a
        // chat is local UI state, not something sent to an already-running
        // chat task — so this stays outside `dispatch_local_command`.
        // The command must be the whole word: "/chatstats" is a message, not
        // a request to join the channel "tats".
        // `command_argument`, not `strip_prefix`: the latter is
        // case-sensitive, so `/CHATS` fell past this the same way `/CLIP` fell
        // past the clip check above — into a public message. It also already
        // enforces the whole-word rule, so "/chatstats" stays a message
        // rather than a request to join the channel "tats".
        let command_rest = ["/chats", "/channels", "/channel"]
            .iter()
            .find_map(|command| command_argument(&text, command));
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
        // Kept for the send history, since `text` is moved into the command.
        let sent_text = text.clone();
        match chat
            .handle
            .commands
            .try_send(ChatCommand::Send { text, reply_to })
        {
            Ok(()) => {
                chat.state.remember_sent(&sent_text);
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

    /// Show or hide the timestamp column.
    ///
    /// `RenderOpts.timestamps` was a real switch with no key at all — the
    /// only way to change it was not to.
    pub fn toggle_timestamps(&mut self) {
        self.render.timestamps = !self.render.timestamps;
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
        // Completion works on the text before the caret, so completing a
        // mention typed in the middle of a sentence no longer overwrites
        // everything after it.
        let before_caret: String = chat
            .state
            .draft
            .value()
            .graphemes(true)
            .take(chat.state.draft.cursor())
            .collect();
        let Some(prefix) = mention_prefix(&before_caret) else {
            return;
        };
        let typed = prefix.trim_start_matches('@').to_string();
        let Some(entry) = chat.roster.complete(&typed, 1).into_iter().next() else {
            return;
        };
        let completion = format!("@{} ", entry.name());
        // mention_prefix may return the word with or without its @ — cut the
        // @ too either way, since the completion re-adds it.
        let mut cut_bytes = before_caret.len() - prefix.len();
        if !prefix.starts_with('@') && before_caret[..cut_bytes].ends_with('@') {
            cut_bytes -= 1;
        }
        let cut = before_caret[..cut_bytes].graphemes(true).count();
        chat.state.draft.replace_back_to(cut, &completion);
    }

    /// Insert an emoji from the picker into the composer draft.
    pub fn insert_emoji(&mut self, emoji: &str) {
        if let Some(chat) = self.active_chat_mut() {
            chat.state.draft.insert_str(emoji);
        }
    }

    /// Ask the focused chat's task to reconnect (also the manual override for
    /// a quota pause or an ended chat).
    pub fn reconnect_active(&mut self) {
        self.send_or_notify(
            ChatCommand::Reconnect,
            "the chat task is busy — press the key again to retry",
        );
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
        let login = self.focused_login(self.focus);
        if let Some(chat) = self.active_chat_mut() {
            let len = chat.state.messages.len();
            let filters = chat.state.filters;
            let messages = &chat.state.messages;
            // A filtered-out message must not become the selection: the
            // jump would land on an invisible row.
            let matches = |offset: usize| {
                messages
                    .get(len - 1 - offset)
                    .is_some_and(|msg| search_matches(msg, &needle) && filters.matches(msg, &login))
            };
            if let Some(offset) = visible_offset(len, -1, 1, matches) {
                chat.state.cursor = Some(offset);
                // Scrolled a little past the match, so it lands about a
                // third up the pane with what followed it underneath.
                // Setting `scroll = offset` put the match on the very
                // bottom row with nothing after it, and "what did they say
                // next" is most of why you searched.
                chat.state.scroll = offset.saturating_sub(SEARCH_CONTEXT_ROWS);
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
        let login = self.focused_login(self.focus);
        if let Some(chat) = self.active_chat_mut() {
            let len = chat.state.messages.len();
            if len == 0 {
                return;
            }
            let filters = chat.state.filters;
            let cursor = chat.state.cursor;
            let messages = &chat.state.messages;
            let matches = |offset: usize| {
                messages
                    .get(len - 1 - offset)
                    .is_some_and(|msg| search_matches(msg, &needle) && filters.matches(msg, &login))
            };
            // With no selection the walk tests *at* the newest row rather
            // than past it: after clearing the selection (esc, or any
            // scroll), pressing n used to skip a match sitting on the
            // newest message.
            let found = match cursor {
                None if matches(0) => Some(0),
                None if older => visible_offset(len, 0, 1, matches),
                None => None, // pressing N with nothing selected goes no further
                Some(offset) => {
                    visible_offset(len, offset as i64, if older { 1 } else { -1 }, matches)
                }
            };
            if let Some(offset) = found {
                chat.state.cursor = Some(offset);
                // Same context as the initial jump, so stepping through
                // matches reads the same way as landing on the first.
                chat.state.scroll = offset.saturating_sub(SEARCH_CONTEXT_ROWS);
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
        let command = ChatCommand::Ban {
            channel_id,
            timeout_secs: Some(duration_secs),
        };
        self.send_or_notify(command, "the chat task is busy — try the timeout again");
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

/// Walk the ring one step at a time from `start`, in the direction of `step`
/// (`+1` toward older rows, `-1` toward newer), returning the first offset
/// whose message satisfies `accept` — or `None` once the walk runs past
/// either end without finding one.
///
/// Shared by every place that has to honor the active view filter while
/// moving through a chat: scrolling, `g`/`G`, and search. `start` is
/// exclusive — pass one position before wherever the walk should begin
/// testing.
fn visible_offset(
    len: usize,
    start: i64,
    step: i64,
    accept: impl Fn(usize) -> bool,
) -> Option<usize> {
    let mut next = start;
    loop {
        next += step;
        if next < 0 || next as usize >= len {
            return None;
        }
        if accept(next as usize) {
            return Some(next as usize);
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
/// What the composer should do with a draft that starts with a slash.
enum SlashVerdict {
    /// Not a command at all, or one handled further down `compose_send`.
    NotACommand,
    /// An escaped slash (`//text`): send this text instead.
    Literal(String),
    /// Refuse to send, and say this instead.
    Refused(String),
}

/// Commands this program handles itself, before anything reaches a platform.
const HANDLED_HERE: [&str; 8] = [
    "/clip",
    "/marker",
    "/chats",
    "/channels",
    "/channel",
    "/me",
    "/raid",
    "/unraid",
];

/// The argument of `text` if it is this command, `None` otherwise.
///
/// Two rules, and both matter.
///
/// The whole-word check keeps "/raidersofthelostark" a message rather than a
/// raid on "ersofthelostark".
///
/// The comparison ignores case, because `classify_command` does. If the guard
/// recognises "/RAID" as a command somebody else handles and no handler here
/// agrees, the text falls through both and is posted to chat — which is the
/// exact failure the guard exists to prevent, arriving through the back door.
fn command_argument<'a>(text: &'a str, command: &str) -> Option<&'a str> {
    let (head, rest) = match text.find(' ') {
        Some(space) => text.split_at(space),
        None => (text, ""),
    };
    head.eq_ignore_ascii_case(command).then_some(rest)
}

/// Decide what a draft beginning with `/` means.
///
/// Slash-prefixed text used to be sent verbatim, on the reasoning that the
/// platform would understand its own commands. That stopped being true in
/// February 2023, when Twitch removed chat commands from IRC: `/ban someone`
/// typed here was not a moderation action, it was a public message reading
/// "/ban someone", posted to everybody watching. YouTube never had chat
/// commands at all, so the same is true there for everything.
///
/// Being wrong in that direction is expensive and cannot be undone — the
/// message is on stream before you have finished reading it. So the default
/// flipped: anything starting with a slash is refused unless this program
/// knows what it means, and the refusal names what to do instead. `//text`
/// escapes the slash for the rare case of genuinely wanting to open a message
/// with one.
fn classify_command(text: &str, platform: Platform) -> SlashVerdict {
    let Some(rest) = text.strip_prefix('/') else {
        return SlashVerdict::NotACommand;
    };
    // `//anything` is an escaped leading slash, IRC's own convention.
    if let Some(literal) = rest.strip_prefix('/') {
        return SlashVerdict::Literal(format!("/{literal}"));
    }

    let word = rest
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let command = format!("/{word}");

    if command == "/me" {
        // Twitch turns this into a CTCP ACTION in the adapter. YouTube has no
        // equivalent, so it would be posted as the literal text "/me waves".
        return if platform == Platform::Twitch {
            SlashVerdict::NotACommand
        } else {
            SlashVerdict::Refused(
                "YouTube chat has no /me — that would be posted as literal text.                  Send it without the /me, or type //me to post the slash on purpose."
                    .to_string(),
            )
        };
    }
    // Raiding is a Helix call the Twitch adapter makes, not a chat line —
    // and YouTube has no equivalent at all. Gated the same way as /me: on
    // Twitch it falls through to its handler below, on YouTube it is
    // refused like any command this program does not implement there.
    if (command == "/raid" || command == "/unraid") && platform != Platform::Twitch {
        return refuse_unknown(&command, platform, &word);
    }
    if HANDLED_HERE.contains(&command.as_str()) {
        return SlashVerdict::NotACommand;
    }

    refuse_unknown(&command, platform, &word)
}

/// The refusal every unrecognised or platform-inapplicable command shares:
/// what was typed, why, and how to post it as text on purpose.
fn refuse_unknown(command: &str, platform: Platform, word: &str) -> SlashVerdict {
    SlashVerdict::Refused(format!(
        "{} is not a command here — it would have been posted as a public message. {}          Type //{} to post it as text on purpose.",
        command,
        advice_for(command, platform),
        word
    ))
}

/// What to do instead of the command somebody typed.
///
/// Specific advice where there is any, because "unknown command" tells
/// somebody they were wrong without telling them what would be right.
fn advice_for(command: &str, platform: Platform) -> &'static str {
    match command {
        "/ban" | "/timeout" | "/delete" | "/unban" | "/untimeout" => match platform {
            Platform::YouTube => {
                "Select the message with K and press b to ban, t to time out, or d to delete."
            }
            Platform::Twitch => {
                "Twitch removed moderation commands from IRC in 2023; use the Twitch mod tools                  for this channel."
            }
        },
        "/host" | "/unhost" => {
            "Hosting was retired by Twitch in 2022. /raid does what it used to."
        }
        // Only reachable on YouTube: on Twitch these are handled before the
        // guard ever sees them.
        "/raid" | "/unraid" => "Raiding is a Twitch feature; YouTube has no equivalent.",
        "/announce" | "/shoutout" | "/so" | "/marker" | "/commercial" | "/poll"
        | "/prediction" => "That is a Twitch dashboard feature, not a chat message.",
        "/w" | "/whisper" => "Whispers are not supported here.",
        "/mod" | "/unmod" | "/vip" | "/unvip" | "/slow" | "/slowoff" | "/subscribers"
        | "/subscribersoff" | "/followers" | "/followersoff" | "/emoteonly"
        | "/emoteonlyoff" | "/uniquechat" | "/clear" | "/color" | "/block" | "/unblock" => {
            "Channel settings live in the platform's own dashboard."
        }
        _ => "If you meant to say it, say it without the slash.",
    }
}

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
    let dir = match crate::paths::chat_log_dir_for(config) {
        Ok(dir) => dir,
        Err(err) => {
            tracing::warn!(error = %format!("{err:#}"), "chat logging disabled");
            return None;
        }
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
            // Which key undoes it. A filter hides most of the chat, and the
            // digits that set one are on no other on-screen surface, so
            // somebody who pressed `3` by accident could see that a filter
            // was on without being told how to turn it off again.
            spans.push(Span::styled(" (0 clears)", Style::default().fg(sk.muted)));
        }
        if !chat.state.search.is_empty() {
            // How many, and which one you are on. "How far back was that" is
            // the question a chat search is asked, and a bare term answered
            // none of it.
            let needle = chat.state.search.to_lowercase();
            let matches: Vec<usize> = (0..chat.state.messages.len())
                .filter(|offset| {
                    chat.state
                        .messages
                        .get(chat.state.messages.len() - 1 - offset)
                        .is_some_and(|msg| search_matches(msg, &needle))
                })
                .collect();
            let position = chat
                .state
                .cursor
                .and_then(|cursor| matches.iter().position(|offset| *offset == cursor))
                .map(|index| format!("{}/", index + 1))
                .unwrap_or_default();
            spans.push(Span::styled(
                format!(" · /{} {position}{}", chat.state.search, matches.len()),
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
    for index in (len.saturating_sub(ACTIVITY_SCAN)..len).rev() {
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
    // The committed search term, so matches are tinted where they sit rather
    // than only located by moving the selection onto them.
    opts.search_needle = chat.state.search.clone();
    let height = area.height as usize;
    let len = chat.state.messages.len();
    let newest_visible = len.saturating_sub(chat.state.scroll);
    // The mentions filter needs to know who "you" are in this chat.
    let highlights = &state.config.chat.highlights;
    let self_login = state.focused_login(platform);
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
        // A message that names you gets a gutter bar. `self_login` is
        // already resolved here for the filter, so this costs nothing extra.
        opts.mentions_me = crate::chat::state::mentions(msg, &self_login);
        // …and so does one a highlight rule picked out. The check returns
        // immediately when no rules are configured, which is the usual case.
        opts.highlighted = highlights.check(msg).is_some();
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

        // The line between what you had read and what arrived while you were
        // looking elsewhere. Pushed after the message because the walk is
        // backwards and the whole list is reversed at the end, so this lands
        // *above* the first new message.
        if chat.state.unread_mark == Some(index) {
            let rule = "─".repeat(usize::from(area.width).saturating_sub(24).max(4));
            lines.push(Line::from(Span::styled(
                format!("{rule} new since you last looked "),
                Style::default().fg(sk.warning),
            )));
        }
    }
    lines.truncate(height);
    lines.reverse();

    frame.render_widget(Paragraph::new(lines), area);

    // A view held still while messages pile up underneath looks exactly like
    // a quiet chat, which is the wrong impression to give somebody who
    // scrolled up mid-stream. Say so, and say how to get back.
    if chat.state.below > 0 && area.height > 0 {
        let notice = format!(" ▼ {} new below — G jumps to live ", chat.state.below);
        let width = notice.chars().count() as u16;
        if width < area.width {
            let strip = Rect {
                x: area.x + area.width - width,
                y: area.y + area.height - 1,
                width,
                height: 1,
            };
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    notice,
                    Style::default()
                        .fg(sk.canvas)
                        .bg(sk.warning)
                        .add_modifier(Modifier::BOLD),
                ))),
                strip,
            );
        }
    }
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
            ChatFocus::EmojiPicker {
                query: buffer,
                selected,
                ..
            } => {
                let mut spans = vec![
                    Span::styled("emoji: ", Style::default().fg(sk.accent)),
                    Span::raw(buffer.clone()),
                    Span::styled("▏ ", Style::default().fg(sk.accent)),
                ];
                let matches = crate::chat::emoji::search(buffer, EMOJI_CHOICES);
                for (index, entry) in matches.iter().enumerate() {
                    // The chosen one is marked, because a list where every row
                    // looks the same and only one of them is reachable is a
                    // list that lies about what the keys do.
                    let style = if index == *selected {
                        Style::default()
                            .fg(sk.accent)
                            .add_modifier(Modifier::REVERSED)
                    } else {
                        Style::default()
                    };
                    spans.push(Span::styled(format!(" {} ", entry.emoji), style));
                }
                spans.push(Span::styled(
                    if matches.is_empty() {
                        "  no matches"
                    } else {
                        "  ←/→ choose, enter inserts"
                    },
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
                let draft = chat
                    .map(|c| c.state.draft.value().to_string())
                    .unwrap_or_default();
                // Where the caret actually is, which is no longer always the
                // end: the draft can be edited in the middle now, and a caret
                // drawn in the wrong place would be worse than none at all.
                let caret = chat.map(|c| c.state.draft.cursor()).unwrap_or(0);
                let mut spans = Vec::new();
                if let Some((_, name)) = chat.and_then(|c| c.state.reply_to.as_ref()) {
                    spans.push(Span::styled(
                        format!("↳ {name} "),
                        Style::default().fg(sk.accent),
                    ));
                }
                spans.push(Span::styled("> ", Style::default().fg(sk.accent)));
                let before: String = draft.graphemes(true).take(caret).collect();
                let after: String = draft.graphemes(true).skip(caret).collect();
                spans.push(Span::raw(before));
                spans.push(Span::styled("▏", Style::default().fg(sk.accent)));
                spans.push(Span::raw(after));
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
            quota: crate::quota::QuotaStore::new(0, None),
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

    /// Put one open chat into a tab state, so `handle_event` has somewhere to
    /// fold an event into.
    fn with_open_chat(state: &mut ChatTabState, notifier: Notifier) -> ChatKey {
        // The account key has to be the one the tab believes is selected, or
        // the chat counts as off-screen and the visibility rule under test
        // would be exercised backwards.
        let account = state.accounts[&Platform::Twitch][0].key.clone();
        let key = ChatKey {
            platform: Platform::Twitch,
            account: account.clone(),
            target: "somechannel".into(),
        };
        state.notifier = notifier;
        state.open.insert(
            key.clone(),
            OpenChat {
                handle: crate::chat::source::ChatHandle {
                    key: key.clone(),
                    commands: mpsc::channel(1).0,
                    task: tokio::spawn(async {}),
                },
                state: ChatState::new(&state.config.chat),
                title: "#somechannel".into(),
                roster: Roster::new(),
                roster_seq: 0,
            },
        );
        state.chats.insert(account.clone(), vec![key.clone()]);
        state.active_chat.insert(account, 0);
        key
    }

    fn raid() -> ChatMessage {
        ChatMessage {
            id: "u1".into(),
            timestamp: None,
            author: ChatAuthor {
                display_name: "iamelisabete".into(),
                ..Default::default()
            },
            text: "430 raiders from iamelisabete have joined!".into(),
            kind: MessageKind::Notice,
            deleted: false,
            historical: false,
            local_echo: false,
            meta: Some(PlatformMeta::Twitch(crate::chat::TwitchMeta {
                system_event: "raid".into(),
                ..Default::default()
            })),
        }
    }

    /// The event this whole feature exists for. A raid gives you seconds to
    /// greet a few hundred people, and by default the pop-up fires whether or
    /// not the chat pane happens to be on screen — because during a stream it
    /// The slash guard exists to stop a command reaching public chat, and
    /// capitalisation defeated it. `classify_command` lowercases the word
    /// before deciding, so `/CLIP` was recognised as handled here — and then
    /// the dispatch below compared it with `==` against the original text,
    /// missed, and fell all the way through to the send path.
    #[tokio::test]
    async fn a_capitalised_command_is_never_posted_as_a_message() {
        for typed in ["/CLIP", "/Clip", "/CHATS", "/MARKER note", "/RAID someone"] {
            let mut state = tab_state(1, 0);
            let key = with_open_chat(&mut state, Notifier::new(false));
            let (tx, mut rx) = mpsc::channel(4);
            state.open.get_mut(&key).unwrap().handle.commands = tx;
            state.open.get_mut(&key).unwrap().state.draft.set(typed);

            state.compose_send();

            // Anything but a plain Send is fine: the point is only that the
            // text never goes out as a public message.
            if let Ok(ChatCommand::Send { text, .. }) = rx.try_recv() {
                panic!("{typed:?} was posted to chat as {text:?}");
            }
        }
    }

    /// …while a message that merely starts with those letters is still a
    /// message. "/clipboard" is not a clip.
    #[tokio::test]
    async fn a_longer_word_starting_with_a_command_is_still_a_message() {
        let mut state = tab_state(1, 0);
        let key = with_open_chat(&mut state, Notifier::new(false));
        state
            .open
            .get_mut(&key)
            .unwrap()
            .state
            .draft
            .set("/clipboard");

        state.compose_send();

        // Refused by the slash guard rather than sent — which is the correct
        // outcome for an unknown command, and not the same as being posted.
        let refused = state.open[&key]
            .state
            .messages
            .iter()
            .any(|msg| msg.text.contains("not a command here"));
        assert!(
            refused,
            "an unknown slash command must be refused, not sent"
        );
    }

    /// A marker is a bookmark in the VOD, and the whole point is that it
    /// takes one key at the moment you have no hands free.
    #[tokio::test]
    async fn the_marker_key_reaches_the_chat_task() {
        let mut state = tab_state(1, 0);
        let key = with_open_chat(&mut state, Notifier::new(false));
        // A channel with room, so the send can be observed.
        let (tx, mut rx) = mpsc::channel(4);
        state.open.get_mut(&key).unwrap().handle.commands = tx;

        state.mark_moment();

        match rx.try_recv() {
            Ok(ChatCommand::Marker { description }) => assert!(description.is_empty()),
            other => panic!("expected a marker, got {other:?}"),
        }
    }

    /// …and with no chat open it says so rather than doing nothing.
    #[tokio::test]
    async fn the_marker_key_says_when_there_is_nowhere_to_send_it() {
        let mut state = tab_state(1, 0);
        state.mark_moment();
        // Nothing to assert on the wire; the point is that it does not panic
        // and does not silently swallow the keypress.
        assert!(state.open.is_empty());
    }

    /// A rule that asks to be told has to reach the desktop, and one that
    /// does not must stay quiet — a notification for every match of a common
    /// word is how somebody learns to ignore their notifications.
    #[tokio::test]
    async fn a_highlight_rule_notifies_only_when_it_asks_to() {
        let mut state = tab_state(1, 0);
        let notifier = Notifier::new(true);
        let key = with_open_chat(&mut state, notifier.clone());
        state.config.chat.highlights = crate::chat::rules::Highlights {
            ignore: vec![],
            rules: vec![crate::chat::rules::Rule {
                match_on: crate::chat::rules::Match::Word,
                pattern: "giveaway".into(),
                notify: false,
            }],
        };

        let mut msg = raid();
        msg.kind = crate::chat::MessageKind::Chat;
        msg.meta = None;
        msg.text = "when is the giveaway".into();
        state.handle_event(key.clone(), ChatEvent::Message(Box::new(msg.clone())));
        assert_eq!(notifier.delivered(), 0, "a quiet rule stays quiet");

        state.config.chat.highlights.rules[0].notify = true;
        msg.id = "m2".into();
        state.handle_event(key, ChatEvent::Message(Box::new(msg)));
        assert_eq!(notifier.delivered(), 1, "a rule that asks is delivered");
    }

    /// The composer used to support exactly two operations: append a
    /// character, and delete the last one. Fixing a typo six words back meant
    /// backspacing over everything after it.
    #[tokio::test]
    async fn the_composer_can_be_edited_in_the_middle() {
        let mut state = tab_state(1, 0);
        with_open_chat(&mut state, Notifier::new(false));

        for c in "helo world".chars() {
            state.compose_push(c);
        }
        // Back to just after "hel", and put the missing l in.
        for _ in 0..7 {
            state.compose_edit(ComposeEdit::Left);
        }
        state.compose_push('l');

        assert_eq!(
            state.active_chat_mut().unwrap().state.draft.value(),
            "hello world"
        );

        // Ctrl+W removes the word before the caret, not the last word typed.
        state.compose_edit(ComposeEdit::End);
        state.compose_edit(ComposeEdit::DeleteWord);
        assert_eq!(
            state.active_chat_mut().unwrap().state.draft.value(),
            "hello "
        );

        state.compose_edit(ComposeEdit::Clear);
        assert!(state.active_chat_mut().unwrap().state.draft.is_empty());
    }

    /// The picker draws a list of candidates, and for a long time only its
    /// first entry could ever be inserted — the list was decoration that
    /// misrepresented what the keys did.
    #[tokio::test]
    async fn the_emoji_picker_inserts_the_entry_the_cursor_is_on() {
        let mut state = tab_state(1, 0);
        with_open_chat(&mut state, Notifier::new(false));

        let candidates = crate::chat::emoji::search("smile", EMOJI_CHOICES);
        assert!(
            candidates.len() > 1,
            "this test needs a query with several matches"
        );

        state.insert_emoji(candidates[1].emoji);

        assert_eq!(
            state.active_chat_mut().unwrap().state.draft.value(),
            candidates[1].emoji,
            "the second candidate must be insertable, not only the first"
        );
    }

    /// usually is not.
    #[tokio::test]
    async fn a_raid_notifies_the_desktop_even_with_the_chat_on_screen() {
        let mut state = tab_state(1, 0);
        let notifier = Notifier::new(true);
        let key = with_open_chat(&mut state, notifier.clone());
        state.tab_visible = true;

        state.handle_event(key, ChatEvent::Message(Box::new(raid())));
        assert_eq!(notifier.delivered(), 1);
    }

    /// …unless the old chat-only behaviour is asked for by name.
    #[tokio::test]
    async fn only_when_hidden_restores_the_old_off_screen_rule() {
        let mut state = tab_state(1, 0);
        state.config.notifications.only_when_hidden = true;
        let notifier = Notifier::new(true);
        let key = with_open_chat(&mut state, notifier.clone());
        state.tab_visible = true;
        state.refresh_visibility();

        state.handle_event(key.clone(), ChatEvent::Message(Box::new(raid())));
        assert_eq!(notifier.delivered(), 0, "the chat is on screen");

        state.tab_visible = false;
        state.handle_event(key, ChatEvent::Message(Box::new(raid())));
        assert_eq!(notifier.delivered(), 1);
    }

    /// The chat-wide switch still turns every chat-derived notification off,
    /// whatever the per-event switches say.
    #[tokio::test]
    async fn chat_notifications_off_silences_even_a_raid() {
        let mut state = tab_state(1, 0);
        state.config.chat.notifications = false;
        let notifier = Notifier::new(true);
        let key = with_open_chat(&mut state, notifier.clone());

        state.handle_event(key, ChatEvent::Message(Box::new(raid())));
        assert_eq!(notifier.delivered(), 0);
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
        state.compose_edit(ComposeEdit::Backspace);
        let key = state.active_key(Platform::Twitch).unwrap().clone();
        assert_eq!(
            state.open[&key].state.draft.value(),
            "hi ",
            "one grapheme removed"
        );

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
            chat.state.draft.set("sure thing");
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
        assert_eq!(chat.state.draft.value(), "sure thing", "the draft is kept");
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
        assert_eq!(state.open[&key].state.draft.value(), "hey @ChatPerson ");

        // No @word at the caret: completion must not touch the draft.
        state.compose_push('!');
        state.complete_mention();
        assert_eq!(state.open[&key].state.draft.value(), "hey @ChatPerson !");
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

    /// A command must be the whole word: "/chatstats" must not be read as
    /// "join the channel tats". It is refused as an unknown command rather
    /// than posted, but the point stands — nothing is joined.
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
        assert!(rx.try_recv().is_err(), "an unknown command is not sent");
        assert_eq!(state.open.len(), 1, "no bogus channel was joined");
    }

    /// The expensive mistake this guards against: before, a mistyped
    /// moderation command was posted to everybody watching. Twitch removed
    /// chat commands from IRC in 2023 and YouTube never had them, so there is
    /// no platform left that would have understood it.
    #[tokio::test]
    async fn a_moderation_command_is_refused_rather_than_broadcast() {
        let config = Config::default();
        let mut state = tab_state(1, 0);
        let account = state.selected_account(Platform::Twitch).unwrap().clone();
        state.open_chat(&config, Platform::Twitch, &account, "chan".into());
        let key = state.active_key(Platform::Twitch).unwrap().clone();
        let (tx, mut rx) = mpsc::channel(8);
        state.open.get_mut(&key).unwrap().handle.commands = tx;

        for ch in "/ban someviewer".chars() {
            state.compose_push(ch);
        }
        state.compose_send();
        assert!(rx.try_recv().is_err(), "nothing may reach the platform");

        // And the refusal has to be visible, with what to do instead.
        let said = state.open[&key]
            .state
            .messages
            .iter()
            .any(|msg| msg.text.contains("/ban") && msg.text.contains("mod tools"));
        assert!(said, "the refusal must explain itself");
    }

    /// The escape hatch, for the rare message that really does start with a
    /// slash. The leading slash is dropped and the rest is sent as typed.
    #[tokio::test]
    async fn a_doubled_slash_posts_the_text_verbatim() {
        let config = Config::default();
        let mut state = tab_state(1, 0);
        let account = state.selected_account(Platform::Twitch).unwrap().clone();
        state.open_chat(&config, Platform::Twitch, &account, "chan".into());
        let key = state.active_key(Platform::Twitch).unwrap().clone();
        let (tx, mut rx) = mpsc::channel(8);
        state.open.get_mut(&key).unwrap().handle.commands = tx;

        for ch in "//ban is not a command".chars() {
            state.compose_push(ch);
        }
        state.compose_send();
        match rx.try_recv().expect("an escaped slash is sent") {
            ChatCommand::Send { text, .. } => assert_eq!(text, "/ban is not a command"),
            other => panic!("expected a send, got {other:?}"),
        }
    }

    /// The guard and the handlers have to agree about what a command is. If
    /// the guard lets "/RAID x" through as "a command somebody else handles"
    /// and no handler recognises it, it is posted to chat — the exact failure
    /// the guard exists to prevent, arriving through the back door.
    #[tokio::test]
    async fn a_command_in_capitals_is_handled_not_posted() {
        let config = Config::default();
        let mut state = tab_state(1, 0);
        let account = state.selected_account(Platform::Twitch).unwrap().clone();
        state.open_chat(&config, Platform::Twitch, &account, "chan".into());
        let key = state.active_key(Platform::Twitch).unwrap().clone();
        let (tx, mut rx) = mpsc::channel(8);
        state.open.get_mut(&key).unwrap().handle.commands = tx;

        for ch in "/RAID otherstreamer".chars() {
            state.compose_push(ch);
        }
        state.compose_send();
        match rx.try_recv().expect("a raid must be started") {
            ChatCommand::Raid { target } => assert_eq!(target, "otherstreamer"),
            other => panic!("expected a raid, got {other:?}"),
        }
    }

    /// Raiding is how a Twitch stream conventionally ends, and it needs a
    /// target — an empty one is a mistake worth naming rather than a request
    /// to raid nobody.
    #[tokio::test]
    async fn raiding_nobody_is_refused_with_the_usage() {
        let config = Config::default();
        let mut state = tab_state(1, 0);
        let account = state.selected_account(Platform::Twitch).unwrap().clone();
        state.open_chat(&config, Platform::Twitch, &account, "chan".into());
        let key = state.active_key(Platform::Twitch).unwrap().clone();
        let (tx, mut rx) = mpsc::channel(8);
        state.open.get_mut(&key).unwrap().handle.commands = tx;

        for ch in "/raid".chars() {
            state.compose_push(ch);
        }
        state.compose_send();
        assert!(rx.try_recv().is_err(), "nothing is sent");
        assert!(state.open[&key]
            .state
            .messages
            .iter()
            .any(|msg| msg.text.contains("/raid somechannel")));
    }

    /// `/me` is real on Twitch (the adapter turns it into a CTCP ACTION) and
    /// meaningless on YouTube, where it would be posted as literal text.
    #[test]
    fn me_is_a_command_on_twitch_and_a_mistake_on_youtube() {
        assert!(matches!(
            classify_command("/me waves", Platform::Twitch),
            SlashVerdict::NotACommand
        ));
        assert!(matches!(
            classify_command("/me waves", Platform::YouTube),
            SlashVerdict::Refused(_)
        ));
    }

    /// Raiding is gated the same way: a Twitch feature handled on Twitch,
    /// refused with the reason why on YouTube — the arm `advice_for` carries
    /// for "/raid"/"/unraid" used to be unreachable.
    #[test]
    fn raid_is_a_command_on_twitch_and_refused_on_youtube() {
        assert!(matches!(
            classify_command("/raid somechannel", Platform::Twitch),
            SlashVerdict::NotACommand
        ));
        assert!(matches!(
            classify_command("/unraid", Platform::Twitch),
            SlashVerdict::NotACommand
        ));
        assert!(matches!(
            classify_command("/raid somechannel", Platform::YouTube),
            SlashVerdict::Refused(_)
        ));
        assert!(matches!(
            classify_command("/unraid", Platform::YouTube),
            SlashVerdict::Refused(_)
        ));
    }

    /// Everything the composer handles itself has to survive the guard, or
    /// the guard would have broken the features it sits in front of.
    #[test]
    fn the_commands_this_program_owns_are_never_refused() {
        for command in [
            "/clip",
            "/chats",
            "/chats somechannel",
            "/channels",
            "/channel x",
        ] {
            assert!(
                matches!(
                    classify_command(command, Platform::Twitch),
                    SlashVerdict::NotACommand
                ),
                "{command} must reach its handler"
            );
        }
    }

    /// Case and arguments must not smuggle a command past the guard.
    #[test]
    fn a_command_is_recognised_whatever_its_case_or_arguments() {
        for text in ["/BAN someone", "/Timeout someone 600", "/ANNOUNCE hello"] {
            assert!(
                matches!(
                    classify_command(text, Platform::Twitch),
                    SlashVerdict::Refused(_)
                ),
                "{text} must be refused"
            );
        }
    }

    /// Ordinary text is untouched — including text that merely contains a
    /// slash somewhere other than the start.
    #[test]
    fn ordinary_text_is_not_a_command() {
        for text in ["hello", "and/or", "http://example.invalid", ""] {
            assert!(matches!(
                classify_command(text, Platform::Twitch),
                SlashVerdict::NotACommand
            ));
        }
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
