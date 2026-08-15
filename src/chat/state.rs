//! Per-chat UI state: messages, unread count, scroll position, composer.
//!
//! Ported behavior from `twi` (commit 7c6ad6bbbc3dec1b6af2ddbd03b55f96c0c162cf,
//! internal/ui channel state: unread counts, scroll anchoring, local-echo
//! reconciliation) and `yc` (commit 9e67efd10c0790ec22df2c944bcee6be1bc37cf8,
//! internal/ui chat state: the same reconcile rule, deletion tombstones).
//!
//! Two invariants both source apps share are enforced here:
//!
//! * **Deleted text is never removed, only marked.** A deletion flips the
//!   `deleted` flag so the renderer replaces the body; dropping the row would
//!   shift scroll positions and lose the fact that moderation happened.
//! * **A local echo and its authoritative copy are one message.** Our own
//!   send renders immediately as an echo; when the platform delivers the real
//!   copy under the same id, it replaces the echo in place instead of
//!   appearing twice.

use crate::chat::ring::Ring;
use crate::chat::{ChatEvent, ChatMessage, ConnectionStatus};
use crate::config::ChatConfig;

/// Everything the UI tracks for one open chat.
pub struct ChatState {
    /// Bounded scrollback; capacity comes from `[chat] scrollback_limit`.
    pub messages: Ring<ChatMessage>,
    /// Messages that arrived while this chat was not on screen. Historical
    /// backlog and our own echoes never count — neither is news.
    pub unread: usize,
    /// Scroll offset in rows *from the bottom*: 0 means follow the tail as
    /// messages arrive; anything else means the user is reading history and
    /// the view must hold still while new messages append below.
    pub scroll: usize,
    /// The connection state shown in the pane header, with a human-readable
    /// detail line ("out of API quota until 09:00", …).
    pub connection: (ConnectionStatus, String),
    /// The composer's in-progress text, kept per chat so switching tabs does
    /// not lose a half-typed message.
    pub draft: String,
    /// The message being replied to: (platform message id, author display
    /// name). Twitch threads by id; YouTube prefixes `@Name ` in the UI.
    pub reply_to: Option<(String, String)>,
    /// The selected message, as an offset from the bottom (like `scroll`).
    /// `None` = nothing selected. Selection is what reply and moderation act
    /// on.
    pub cursor: Option<usize>,
    /// Whether this chat is currently on screen. While it is, nothing counts
    /// as unread.
    viewed: bool,
}

impl ChatState {
    pub fn new(config: &ChatConfig) -> Self {
        Self {
            messages: Ring::new(config.scrollback_limit),
            unread: 0,
            scroll: 0,
            connection: (ConnectionStatus::Connecting, String::new()),
            draft: String::new(),
            reply_to: None,
            cursor: None,
            viewed: false,
        }
    }

    /// The chat became visible: everything is considered read from here on,
    /// until [`Self::mark_hidden`].
    pub fn mark_viewed(&mut self) {
        self.viewed = true;
        self.unread = 0;
    }

    /// The chat left the screen: new messages count as unread again.
    pub fn mark_hidden(&mut self) {
        self.viewed = false;
    }

    /// Applies one event from the chat task to this state.
    pub fn apply(&mut self, event: ChatEvent) {
        match event {
            ChatEvent::Message(msg) => self.apply_message(*msg),
            ChatEvent::MessageDeleted { message_id } => {
                // Mark, never remove: the renderer substitutes the body and
                // the original text stays off screen forever.
                for existing in self.messages.iter_mut() {
                    if existing.id == message_id {
                        existing.deleted = true;
                    }
                }
            }
            ChatEvent::UserPurged { author_login, .. } => {
                // A ban or timeout tombstones everything the author said.
                // Matched on login *or* id because Twitch's CLEARCHAT carries
                // the login while YouTube's ban events carry the channel id.
                for existing in self.messages.iter_mut() {
                    if existing.author.login.eq_ignore_ascii_case(&author_login)
                        || existing.author.id == author_login
                    {
                        existing.deleted = true;
                    }
                }
            }
            ChatEvent::ChatCleared => {
                self.messages.clear();
                self.scroll = 0;
                self.unread = 0;
            }
            ChatEvent::Connection { status, detail } => {
                self.connection = (status, detail);
            }
        }
    }

    fn apply_message(&mut self, msg: ChatMessage) {
        // Reconcile rule (both source apps): a message whose id is already on
        // screen replaces that copy in place. This is how a local echo is
        // upgraded to the authoritative platform copy without appearing
        // twice, and it also absorbs any transport-level duplicate delivery.
        if !msg.id.is_empty() {
            for existing in self.messages.iter_mut() {
                if existing.id == msg.id {
                    // A tombstone that raced the authoritative copy must win:
                    // once deleted, a message never comes back.
                    let was_deleted = existing.deleted;
                    *existing = msg;
                    existing.deleted = existing.deleted || was_deleted;
                    return;
                }
            }
        }

        let is_news = !msg.historical && !msg.local_echo;
        let evicted = self.messages.push(msg);

        if self.scroll > 0 {
            // The user is reading history. A new message appends below the
            // view, pushing every on-screen row one step further from the
            // bottom; growing the offset by one keeps the view still. When
            // the ring evicted the oldest row instead of growing, the top of
            // history moved down to meet the view, so the offset is capped at
            // the last reachable row.
            self.scroll += 1;
            if evicted {
                self.scroll = self.scroll.min(self.messages.len().saturating_sub(1));
            }
        }

        if is_news && !self.viewed {
            self.unread += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::{ChatAuthor, MessageKind};

    fn config(limit: usize) -> ChatConfig {
        ChatConfig {
            scrollback_limit: limit,
            ..ChatConfig::default()
        }
    }

    fn msg(id: &str, login: &str, text: &str) -> ChatMessage {
        ChatMessage {
            id: id.into(),
            timestamp: None,
            author: ChatAuthor {
                id: format!("id-{login}"),
                login: login.into(),
                display_name: login.into(),
                badges: vec![],
                color_hint: None,
            },
            text: text.into(),
            kind: MessageKind::Chat,
            deleted: false,
            historical: false,
            local_echo: false,
            meta: None,
        }
    }

    fn deliver(state: &mut ChatState, m: ChatMessage) {
        state.apply(ChatEvent::Message(Box::new(m)));
    }

    #[test]
    fn a_local_echo_and_its_authoritative_copy_are_one_message() {
        let mut state = ChatState::new(&config(100));
        let mut echo = msg("abc", "me", "hello there");
        echo.local_echo = true;
        deliver(&mut state, echo);
        assert_eq!(state.messages.len(), 1);

        // The platform echoes the authoritative copy back under the same id.
        deliver(&mut state, msg("abc", "me", "hello there"));
        assert_eq!(
            state.messages.len(),
            1,
            "the echo must be replaced, not doubled"
        );
        let stored = state.messages.get(0).unwrap();
        assert!(!stored.local_echo, "the authoritative copy wins");
    }

    #[test]
    fn a_deletion_that_raced_the_authoritative_copy_stays_deleted() {
        let mut state = ChatState::new(&config(100));
        let mut echo = msg("abc", "me", "oops");
        echo.local_echo = true;
        deliver(&mut state, echo);
        state.apply(ChatEvent::MessageDeleted {
            message_id: "abc".into(),
        });
        deliver(&mut state, msg("abc", "me", "oops"));
        assert!(state.messages.get(0).unwrap().deleted);
    }

    #[test]
    fn deleting_a_message_marks_it_without_removing_it() {
        let mut state = ChatState::new(&config(100));
        deliver(&mut state, msg("1", "alice", "one"));
        deliver(&mut state, msg("2", "bob", "two"));
        state.apply(ChatEvent::MessageDeleted {
            message_id: "1".into(),
        });
        assert_eq!(state.messages.len(), 2, "deletion must not remove the row");
        assert!(state.messages.get(0).unwrap().deleted);
        assert!(!state.messages.get(1).unwrap().deleted);
        // The original text is still stored (the renderer hides it), because
        // dropping it would also drop the evidence that moderation happened.
        assert_eq!(state.messages.get(0).unwrap().text, "one");
    }

    #[test]
    fn a_purge_marks_every_message_from_that_author() {
        let mut state = ChatState::new(&config(100));
        deliver(&mut state, msg("1", "troll", "bad"));
        deliver(&mut state, msg("2", "alice", "fine"));
        deliver(&mut state, msg("3", "troll", "worse"));
        state.apply(ChatEvent::UserPurged {
            author_login: "Troll".into(), // case-insensitive on login
            timeout_secs: Some(600),
        });
        let deleted: Vec<bool> = state.messages.iter().map(|m| m.deleted).collect();
        assert_eq!(deleted, [true, false, true]);
    }

    #[test]
    fn a_purge_also_matches_on_author_id() {
        let mut state = ChatState::new(&config(100));
        deliver(&mut state, msg("1", "someone", "hi"));
        state.apply(ChatEvent::UserPurged {
            author_login: "id-someone".into(),
            timeout_secs: None,
        });
        assert!(state.messages.get(0).unwrap().deleted);
    }

    #[test]
    fn clearing_the_chat_empties_it() {
        let mut state = ChatState::new(&config(100));
        deliver(&mut state, msg("1", "alice", "one"));
        deliver(&mut state, msg("2", "bob", "two"));
        state.scroll = 1;
        state.apply(ChatEvent::ChatCleared);
        assert!(state.messages.is_empty());
        assert_eq!(state.scroll, 0);
        assert_eq!(state.unread, 0);
    }

    #[test]
    fn unread_counts_skip_historical_backlog_and_own_echoes() {
        let mut state = ChatState::new(&config(100));
        let mut backlog = msg("h1", "alice", "old");
        backlog.historical = true;
        deliver(&mut state, backlog);
        let mut echo = msg("e1", "me", "mine");
        echo.local_echo = true;
        deliver(&mut state, echo);
        deliver(&mut state, msg("n1", "bob", "new"));
        assert_eq!(
            state.unread, 1,
            "only the live message from someone else counts"
        );

        state.mark_viewed();
        assert_eq!(state.unread, 0);
        deliver(&mut state, msg("n2", "bob", "while watching"));
        assert_eq!(
            state.unread, 0,
            "nothing is unread while the chat is on screen"
        );

        state.mark_hidden();
        deliver(&mut state, msg("n3", "bob", "while away"));
        assert_eq!(state.unread, 1);
    }

    #[test]
    fn scrolled_view_holds_still_as_messages_arrive() {
        let mut state = ChatState::new(&config(100));
        for i in 0..10 {
            deliver(&mut state, msg(&format!("m{i}"), "alice", "x"));
        }
        // The user scrolls up: the row at offset 3 from the bottom is "m6".
        state.scroll = 3;
        deliver(&mut state, msg("m10", "alice", "x"));
        // One more row below the view: the offset grows so the same rows stay
        // on screen.
        assert_eq!(state.scroll, 4);
        let anchored = state
            .messages
            .get(state.messages.len() - 1 - state.scroll)
            .unwrap();
        assert_eq!(anchored.id, "m6");
    }

    #[test]
    fn scroll_position_stays_stable_and_in_bounds_across_eviction() {
        let mut state = ChatState::new(&config(5));
        for i in 0..5 {
            deliver(&mut state, msg(&format!("m{i}"), "alice", "x"));
        }
        // Scrolled to the very top of a full ring.
        state.scroll = 4;
        deliver(&mut state, msg("m5", "alice", "x"));
        // The oldest row was evicted; the offset must stay reachable
        // (at most len - 1) instead of pointing past the top.
        assert_eq!(state.messages.len(), 5);
        assert!(state.scroll < state.messages.len());
        assert_eq!(state.scroll, 4);
        // And the anchored row is still the oldest surviving one.
        let anchored = state
            .messages
            .get(state.messages.len() - 1 - state.scroll)
            .unwrap();
        assert_eq!(anchored.id, "m1");
    }

    #[test]
    fn following_the_tail_never_scrolls() {
        let mut state = ChatState::new(&config(3));
        for i in 0..10 {
            deliver(&mut state, msg(&format!("m{i}"), "alice", "x"));
        }
        assert_eq!(state.scroll, 0, "offset 0 means follow the tail");
    }

    #[test]
    fn connection_events_update_the_header_state() {
        let mut state = ChatState::new(&config(10));
        state.apply(ChatEvent::Connection {
            status: ConnectionStatus::QuotaPaused,
            detail: "out of API quota until 09:00".into(),
        });
        assert_eq!(state.connection.0, ConnectionStatus::QuotaPaused);
        assert_eq!(state.connection.1, "out of API quota until 09:00");
    }
}
