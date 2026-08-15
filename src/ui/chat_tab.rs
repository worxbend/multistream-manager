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
use crate::chat::render::{render_message, RenderOpts};
use crate::chat::source::{self, ChatCommand, ChatHandle};
use crate::chat::state::ChatState;
use crate::chat::{ChatAuthor, ChatEvent, ChatKey, ChatMessage, ConnectionStatus, MessageKind};
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
}

/// A live chat: its running task and the state the UI folds events into.
pub struct OpenChat {
    pub handle: ChatHandle,
    pub state: ChatState,
    /// What the chat strip shows: `#channel` / the typed target.
    pub title: String,
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
}

/// Everything the Chat tab remembers between frames.
pub struct ChatTabState {
    /// Which pane keyboard input goes to.
    pub focus: Platform,
    pub mode: ChatFocus,
    /// `space` was pressed and the next key completes the chord.
    pub pending_space: bool,
    /// The accounts available per platform, discovered from the token store
    /// once at startup (primary first, extras after).
    pub accounts: BTreeMap<Platform, Vec<AccountTab>>,
    /// The selected account sub-tab per platform.
    pub selected: BTreeMap<Platform, usize>,
    /// The Twitch pane's share of the width, in percent. Resizable with
    /// `<`/`>` and reset with `=` — the same keys the reference TUIs use.
    pub split_percent: u16,

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
    pub fn new() -> Self {
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
            accounts,
            selected: BTreeMap::new(),
            split_percent: SPLIT_DEFAULT,
            open: HashMap::new(),
            chats: BTreeMap::new(),
            active_chat: BTreeMap::new(),
            events_tx,
            events_rx: Some(events_rx),
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

    /// The tab became visible (or the selection changed): make sure both
    /// panes' selected accounts have their own chat open, and update
    /// viewed/hidden marks so unread counts stay truthful.
    pub fn activate(&mut self, config: &Config) {
        for platform in Platform::ALL {
            if let Some(account) = self.selected_account(platform).cloned() {
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
        self.refresh_visibility();
    }

    /// The tab left the screen: everything counts as unread again.
    pub fn deactivate(&mut self) {
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
            Platform::Twitch => crate::chat::twitch::spawn(
                key.clone(),
                account.own_target.clone().unwrap_or_default(),
                tokens,
                self.events_tx.clone(),
            ),
            Platform::YouTube => crate::chat::youtube::spawn(crate::chat::youtube::SpawnParams {
                key: key.clone(),
                poll_floor_ms: config.chat.poll_interval_floor_ms,
                poll_ceiling_ms: config.chat.poll_interval_ceiling_ms,
                daily_quota_units: config.chat.daily_quota_units,
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
        if let Some(chat) = self.open.get_mut(&key) {
            chat.state.apply(event);
        }
    }

    /// The focused pane's active chat, mutably.
    fn active_chat_mut(&mut self) -> Option<&mut OpenChat> {
        let key = self.active_key(self.focus)?.clone();
        self.open.get_mut(&key)
    }

    /// Scroll the focused chat by `delta` messages (positive = further back).
    pub fn scroll_by(&mut self, delta: i64) {
        if let Some(chat) = self.active_chat_mut() {
            let max = chat.state.messages.len().saturating_sub(1);
            let next = chat.state.scroll as i64 + delta;
            chat.state.scroll = next.clamp(0, max as i64) as usize;
        }
    }

    /// Jump to the oldest (`g`) or newest (`G`) message.
    pub fn scroll_to_end(&mut self, oldest: bool) {
        if let Some(chat) = self.active_chat_mut() {
            chat.state.scroll = if oldest {
                chat.state.messages.len().saturating_sub(1)
            } else {
                0
            };
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
        let reply_to = chat.state.reply_to.take().map(|(id, _)| id);
        match chat
            .handle
            .commands
            .try_send(ChatCommand::Send { text, reply_to })
        {
            Ok(()) => chat.state.draft.clear(),
            Err(_) => {
                chat.state.apply(ChatEvent::Message(Box::new(local_notice(
                    "the chat task is busy — the message was kept in the composer, try again",
                ))));
            }
        }
    }

    /// Ask the focused chat's task to reconnect (also the manual override for
    /// a quota pause or an ended chat).
    pub fn reconnect_active(&mut self) {
        if let Some(chat) = self.active_chat_mut() {
            let _ = chat.handle.commands.try_send(ChatCommand::Reconnect);
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
    match status {
        ConnectionStatus::Connected => Color::Green,
        ConnectionStatus::Connecting | ConnectionStatus::Reconnecting => Color::Yellow,
        ConnectionStatus::QuotaPaused => Color::Magenta,
        ConnectionStatus::Closed | ConnectionStatus::Disconnected => Color::DarkGray,
        ConnectionStatus::Failed => Color::Red,
    }
}

fn draw_pane(
    frame: &mut Frame,
    area: Rect,
    state: &ChatTabState,
    config: &Config,
    platform: Platform,
) {
    let focused = state.focus == platform;
    let border_style = if focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style)
        .title(format!(" {} ", platform.label()));
    let inner = block.inner(area);
    frame.render_widget(block, area);

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
    draw_messages(frame, rows[2], state, platform);
    draw_composer(frame, rows[3], state, platform, focused);
}

fn draw_account_strip(frame: &mut Frame, area: Rect, state: &ChatTabState, platform: Platform) {
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
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Gray)
        };
        spans.push(Span::styled(label, style));
        spans.push(Span::raw(" "));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_chat_strip(frame: &mut Frame, area: Rect, state: &ChatTabState, platform: Platform) {
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
            Style::default().fg(Color::DarkGray)
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
                Style::default().fg(Color::DarkGray),
            ));
        }
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_messages(frame: &mut Frame, area: Rect, state: &ChatTabState, platform: Platform) {
    let Some(key) = state.active_key(platform) else {
        let hint = Paragraph::new(Line::from(Span::styled(
            "No chat open — press space then c to join one.",
            Style::default().fg(Color::DarkGray),
        )));
        frame.render_widget(hint, area);
        return;
    };
    let Some(chat) = state.open.get(key) else {
        return;
    };

    let opts = RenderOpts::default();
    let height = area.height as usize;
    let len = chat.state.messages.len();
    let newest_visible = len.saturating_sub(chat.state.scroll);

    // Walk backwards from the newest visible message, rendering (and
    // wrapping) until the pane is full, then flip the order — the cheap way
    // to keep the newest rows glued to the bottom whatever each message's
    // wrapped height is.
    let mut lines: Vec<Line> = Vec::new();
    for index in (0..newest_visible).rev() {
        if lines.len() >= height {
            break;
        }
        let Some(msg) = chat.state.messages.get(index) else {
            break;
        };
        let mut rendered = render_message(msg, area.width, &opts);
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
    let line = if focused {
        match &state.mode {
            ChatFocus::Join(buffer) => Line::from(vec![
                Span::styled("join: ", Style::default().fg(Color::Cyan)),
                Span::raw(buffer.clone()),
                Span::styled("▏", Style::default().fg(Color::Cyan)),
            ]),
            ChatFocus::Compose => {
                let draft = state
                    .active_key(platform)
                    .and_then(|key| state.open.get(key))
                    .map(|chat| chat.state.draft.clone())
                    .unwrap_or_default();
                Line::from(vec![
                    Span::styled("> ", Style::default().fg(Color::Cyan)),
                    Span::raw(draft),
                    Span::styled("▏", Style::default().fg(Color::Cyan)),
                ])
            }
            ChatFocus::Normal => Line::from(Span::styled(
                "i compose · j/k scroll · [ ] chats · { } accounts · space-c join · space-x close · ctrl+r reconnect",
                Style::default().fg(Color::DarkGray),
            )),
        }
    } else {
        Line::from(Span::styled(
            "h/l to focus this pane",
            Style::default().fg(Color::DarkGray),
        ))
    };
    frame.render_widget(Paragraph::new(line), area);
}

/// The per-pane empty state: what is missing and the exact commands that fix
/// it. The commands are the real ones this repository ships — discovered in
/// recon, not invented.
fn draw_empty_state(frame: &mut Frame, area: Rect, config: &Config, platform: Platform) {
    let slug = platform.slug();
    let credentials_ready = config.check_credentials(&[platform]).is_ok();

    let mut lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            format!("No {} chat accounts yet.", platform.label()),
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
    ];
    if !credentials_ready {
        lines.push(Line::from(
            "1. Create API credentials first: run `msm init`, then fill in the",
        ));
        lines.push(Line::from(format!(
            "   [{slug}] section of config.toml (`msm paths` shows where it lives)."
        )));
        lines.push(Line::from(format!(
            "2. Log in: quit and run `msm login {slug}`."
        )));
    } else {
        lines.push(Line::from(format!(
            "Log in: quit and run `msm login {slug}`."
        )));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(format!(
        "Add more accounts any time with `msm login {slug} --add`;"
    )));
    lines.push(Line::from("each appears here as its own sub-tab."));

    let paragraph = Paragraph::new(lines)
        .style(Style::default().fg(Color::Gray))
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
            accounts: BTreeMap::new(),
            selected: BTreeMap::new(),
            split_percent: SPLIT_DEFAULT,
            open: HashMap::new(),
            chats: BTreeMap::new(),
            active_chat: BTreeMap::new(),
            events_tx: mpsc::unbounded_channel().0,
            events_rx: None,
            http: reqwest::Client::new(),
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
    async fn activation_opens_own_chats_for_the_selected_accounts_only() {
        let config = Config::default();
        let mut state = tab_state(2, 1);
        assert!(state.open.is_empty(), "nothing connects at startup");

        state.activate(&config);

        assert_eq!(state.open.len(), 2, "one chat per visible pane");
        assert_eq!(state.active_key(Platform::Twitch).unwrap().target, "own0");
        assert!(
            !state.open.keys().any(|key| key.account == "twitch:1"),
            "the unselected account stays unconnected"
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
