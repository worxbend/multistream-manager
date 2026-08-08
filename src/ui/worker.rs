//! The background half of the terminal UI.
//!
//! ## Why this exists
//!
//! A terminal UI has to redraw and respond to keystrokes constantly. An HTTP
//! call to YouTube can easily take a second or two. If the UI called the API
//! directly it would freeze — no cursor movement, no scrolling, no way to
//! cancel — every time it talked to a platform.
//!
//! So all the slow work happens here, on its own task. The UI sends a
//! [`Command`] and carries on redrawing; when the work finishes an [`Event`]
//! arrives and the UI updates. The two halves only ever speak through channels,
//! which also means the UI code contains no `await` at all.

use tokio::sync::mpsc;

use crate::backend::PlatformResult;
use crate::config::Config;
use crate::engine::Engine;
use crate::model::{Category, Platform, PlatformStats, StreamPlan};

/// A request from the UI to the worker.
#[derive(Debug, Clone)]
pub enum Command {
    /// Build backends for these platforms and verify the logins.
    Connect(Vec<Platform>),
    /// Autocomplete a category name. `generation` is echoed back in the reply so
    /// that a slow response to an old keystroke can be discarded.
    SearchCategories {
        platform: Platform,
        query: String,
        generation: u64,
    },
    /// Apply the plan to every connected platform.
    GoLive(Box<StreamPlan>),
    /// Refresh the statistics.
    PollStats,
}

/// A message from the worker back to the UI.
#[derive(Debug, Clone)]
pub enum Event {
    /// Connection finished; carries the account name resolved for each platform.
    Connected(Vec<(Platform, Result<String, String>)>),
    /// Autocomplete results for the keystroke identified by `generation`.
    Categories {
        platform: Platform,
        results: Vec<Category>,
        generation: u64,
    },
    /// Going live finished, successfully or otherwise, per platform.
    WentLive(Vec<PlatformResult>),
    /// A fresh statistics snapshot.
    Stats(Vec<(Platform, PlatformStats)>),
    /// Something to append to the on-screen activity log.
    Log { level: LogLevel, message: String },
}

/// How prominently a log line should be rendered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    Info,
    Success,
    Warning,
    Error,
}

/// Run the worker until the UI drops its command sender.
///
/// Owning the [`Engine`] here rather than in the UI is deliberate: it means the
/// engine is only ever touched from one task, so no locking is needed anywhere.
pub async fn run(
    config: Config,
    mut commands: mpsc::Receiver<Command>,
    events: mpsc::UnboundedSender<Event>,
) {
    let mut engine: Option<Engine> = None;

    while let Some(command) = commands.recv().await {
        match command {
            Command::Connect(platforms) => {
                if platforms.is_empty() {
                    let _ = events.send(Event::Log {
                        level: LogLevel::Warning,
                        message: "Select at least one platform first.".into(),
                    });
                    continue;
                }

                let _ = events.send(Event::Log {
                    level: LogLevel::Info,
                    message: format!(
                        "Connecting to {}…",
                        platforms
                            .iter()
                            .map(|p| p.label())
                            .collect::<Vec<_>>()
                            .join(" and ")
                    ),
                });

                match Engine::build(&config, &platforms).await {
                    Ok(mut built) => {
                        let results = built.connect_all().await;
                        engine = Some(built);
                        let _ = events.send(Event::Connected(results));
                    }
                    Err(err) => {
                        // A build failure is almost always missing credentials
                        // or an expired login, and the error text explains how
                        // to fix it — so pass it through in full.
                        let _ = events.send(Event::Connected(
                            platforms
                                .into_iter()
                                .map(|p| (p, Err(format!("{err:#}"))))
                                .collect(),
                        ));
                    }
                }
            }

            Command::SearchCategories {
                platform,
                query,
                generation,
            } => {
                let Some(engine) = engine.as_mut() else {
                    // Not connected yet, so there is nothing to search against.
                    // Silently ignoring this is right: the user is just typing.
                    continue;
                };

                match engine.search_categories(platform, &query).await {
                    Ok(results) => {
                        let _ = events.send(Event::Categories {
                            platform,
                            results,
                            generation,
                        });
                    }
                    Err(err) => {
                        let _ = events.send(Event::Log {
                            level: LogLevel::Warning,
                            message: format!("Category search failed: {err:#}"),
                        });
                    }
                }
            }

            Command::GoLive(plan) => {
                let Some(engine) = engine.as_mut() else {
                    let _ = events.send(Event::Log {
                        level: LogLevel::Error,
                        message: "Not connected to any platform yet.".into(),
                    });
                    continue;
                };

                let _ = events.send(Event::Log {
                    level: LogLevel::Info,
                    message: "Submitting to all selected platforms…".into(),
                });

                let results = engine.go_live(&plan).await;

                for result in &results {
                    match &result.outcome {
                        Ok(outcome) => {
                            let _ = events.send(Event::Log {
                                level: LogLevel::Success,
                                message: format!("{} is ready.", result.platform.label()),
                            });
                            for note in &outcome.notes {
                                let level = if note.starts_with("Warning") {
                                    LogLevel::Warning
                                } else {
                                    LogLevel::Info
                                };
                                let _ = events.send(Event::Log {
                                    level,
                                    message: format!("  {} — {note}", result.platform.label()),
                                });
                            }
                        }
                        Err(err) => {
                            let _ = events.send(Event::Log {
                                level: LogLevel::Error,
                                message: format!("{} failed: {err}", result.platform.label()),
                            });
                        }
                    }
                }

                let _ = events.send(Event::WentLive(results));
            }

            Command::PollStats => {
                let Some(engine) = engine.as_mut() else {
                    continue;
                };
                let stats = engine.poll_stats().await;
                let _ = events.send(Event::Stats(stats));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn connecting_with_no_platforms_warns_instead_of_failing() {
        let (command_tx, command_rx) = mpsc::channel(4);
        let (event_tx, mut event_rx) = mpsc::unbounded_channel();

        let handle = tokio::spawn(run(Config::default(), command_rx, event_tx));

        command_tx.send(Command::Connect(vec![])).await.unwrap();

        let event = event_rx.recv().await.expect("a warning should arrive");
        match event {
            Event::Log { level, message } => {
                assert_eq!(level, LogLevel::Warning);
                assert!(message.contains("at least one platform"));
            }
            other => panic!("expected a log line, got {other:?}"),
        }

        drop(command_tx);
        handle.await.unwrap();
    }

    #[tokio::test]
    async fn going_live_before_connecting_reports_an_error() {
        let (command_tx, command_rx) = mpsc::channel(4);
        let (event_tx, mut event_rx) = mpsc::unbounded_channel();

        let handle = tokio::spawn(run(Config::default(), command_rx, event_tx));

        command_tx
            .send(Command::GoLive(Box::default()))
            .await
            .unwrap();

        match event_rx.recv().await.expect("an error should arrive") {
            Event::Log { level, message } => {
                assert_eq!(level, LogLevel::Error);
                assert!(message.contains("Not connected"));
            }
            other => panic!("expected a log line, got {other:?}"),
        }

        drop(command_tx);
        handle.await.unwrap();
    }

    #[tokio::test]
    async fn polling_before_connecting_is_silently_ignored() {
        let (command_tx, command_rx) = mpsc::channel(4);
        let (event_tx, mut event_rx) = mpsc::unbounded_channel();

        let handle = tokio::spawn(run(Config::default(), command_rx, event_tx));

        command_tx.send(Command::PollStats).await.unwrap();
        drop(command_tx);
        handle.await.unwrap();

        // The worker has exited, so the channel is closed and empty: the poll
        // produced no event at all, which is the intended behaviour.
        assert!(event_rx.recv().await.is_none());
    }

    #[tokio::test]
    async fn the_worker_shuts_down_when_the_ui_drops_its_sender() {
        let (command_tx, command_rx) = mpsc::channel(1);
        let (event_tx, _event_rx) = mpsc::unbounded_channel();

        let handle = tokio::spawn(run(Config::default(), command_rx, event_tx));
        drop(command_tx);

        // Completing at all proves the loop exits rather than hanging.
        handle.await.unwrap();
    }
}
