//! The seam between chat transports and the UI.
//!
//! Each open chat runs as one tokio task, spawned by its platform adapter
//! ([`crate::chat::twitch`] / [`crate::chat::youtube`]). The task owns the
//! network connection and emits normalized [`ChatEvent`]s over a shared
//! unbounded channel, tagged with the [`ChatKey`] they belong to; the UI
//! drains that channel non-blockingly each frame and never touches a socket.
//!
//! Dispatch is a closed enum rather than `Box<dyn ChatSource>`: there are
//! exactly two platforms (the same closed set as `backend.rs`), and enum
//! dispatch sidesteps object-safety friction around async methods.
//!
//! Shutdown is deterministic: dropping a [`ChatHandle`] closes its command
//! channel, which ends the task's `select!` loop — the same convention the
//! main worker uses. No task outlives its handle.

use std::sync::Arc;

use tokio::sync::mpsc;

use crate::backend::BoxFuture;
use crate::chat::{ChatEvent, ChatKey};
use crate::config::Config;

/// What the UI can ask a running chat task to do.
#[derive(Debug, Clone)]
pub enum ChatCommand {
    /// Send a message. `reply_to` is a platform message id (Twitch threads
    /// via `reply-parent-msg-id`; the YouTube adapter ignores it — YouTube's
    /// reply convention is an `@Name ` prefix the UI adds to `text`).
    Send {
        text: String,
        reply_to: Option<String>,
    },
    /// Tear the connection down and reconnect now (the user pressed the
    /// reconnect key, or wants to override a quota pause).
    Reconnect,
    /// Remove one message (YouTube `liveChatMessages.delete`). Twitch chats
    /// answer with a notice: the reference implementation performs no
    /// client-side moderation, because Twitch removed moderation commands
    /// from IRC in 2023.
    Delete { message_id: String },
    /// Ban an author permanently (`timeout_secs: None`) or time them out
    /// (YouTube `liveChatBans.insert`). Same Twitch caveat as `Delete`.
    Ban {
        channel_id: String,
        timeout_secs: Option<u64>,
    },
    /// Create a clip of the account's own live stream (Twitch Helix Create
    /// Clip, twi's `/clip`). YouTube chats answer with a notice — clips are
    /// a Twitch feature.
    Clip,
}

/// One running chat: its identity, its command channel, and its task.
///
/// Dropping the handle closes the command channel and thereby stops the task.
pub struct ChatHandle {
    pub key: ChatKey,
    pub commands: mpsc::Sender<ChatCommand>,
    /// Kept so shutdown can be awaited (with a timeout) when quitting.
    pub task: tokio::task::JoinHandle<()>,
}

/// Events from every chat task, multiplexed onto one channel and tagged with
/// the chat they belong to.
pub type EventSender = mpsc::UnboundedSender<(ChatKey, ChatEvent)>;

/// An async "give me a currently-valid access token" the adapters call before
/// connecting and around every reauthenticating reconnect. Wraps the token
/// store's keyed lookup + refresh, so a token renewed mid-session reaches
/// every chat task the same way it reaches the streaming engine.
pub type TokenProvider = Arc<dyn Fn() -> BoxFuture<'static, anyhow::Result<String>> + Send + Sync>;

/// A [`TokenProvider`] for one stored account key.
pub fn token_provider(config: &Config, account_key: &str) -> TokenProvider {
    let config = config.clone();
    let key = account_key.to_string();
    Arc::new(move || {
        let config = config.clone();
        let key = key.clone();
        Box::pin(async move { crate::auth::access_token_for_key(&config, &key).await })
    })
}

/// How many commands may queue per chat before sends are refused. Small on
/// purpose: a backlog of unsent messages is a promise the UI cannot keep.
pub const COMMAND_QUEUE: usize = 8;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Platform;

    /// Dropping a handle must end its task — the no-leaked-tasks rule.
    #[tokio::test]
    async fn dropping_a_handle_stops_its_task() {
        let (commands, mut command_rx) = mpsc::channel::<ChatCommand>(COMMAND_QUEUE);
        let task = tokio::spawn(async move { while command_rx.recv().await.is_some() {} });
        let handle = ChatHandle {
            key: ChatKey {
                platform: Platform::Twitch,
                account: "twitch".into(),
                target: "someone".into(),
            },
            commands,
            task,
        };

        let task = handle.task;
        drop(handle.commands);
        tokio::time::timeout(std::time::Duration::from_secs(1), task)
            .await
            .expect("the task must end when the command channel closes")
            .expect("and end cleanly");
    }
}
