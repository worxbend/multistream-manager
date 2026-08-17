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
    /// Apply the plan to every connected platform. `generation` is echoed
    /// back in [`Event::WentLive`] so the UI can discard a result that
    /// belongs to an earlier, superseded submission.
    GoLive {
        plan: Box<StreamPlan>,
        generation: u64,
    },
    /// Finish the broadcast on every connected platform.
    ///
    /// The other end of `GoLive`. Sent only after the interface has asked
    /// twice, because it cannot be undone: a finished YouTube broadcast
    /// cannot be reopened, and viewers watching it are watching a recording
    /// from the moment it completes.
    EndLive,
    /// Refresh the statistics.
    PollStats,
    /// Run the browser login for these platforms, one after another, and save
    /// the resulting tokens. Sent by the login screen.
    Login(Vec<Platform>),
    /// Adopt a config the interface has just saved (API credentials entered on
    /// the setup screen), and forget any engine built from the old one.
    ReloadConfig(Box<Config>),
    /// Run a browser login and save the result as an *additional* chat
    /// account rather than replacing the platform's primary one.
    ///
    /// The primary account is the one streaming uses. An extra one only
    /// appears as a sub-tab on the Chat tab, which is how somebody reads and
    /// answers chat as a bot or a second identity.
    LoginAdd(Platform),
    /// Forget a platform's saved login.
    Logout(Platform),
    /// Find the YouTube broadcasts that were created and never went live, and
    /// with `delete`, remove them.
    Cleanup { delete: bool },
    /// Write every paid chat event to a CSV file beside the chat logs.
    ExportSuperchats,
    /// List the stream keys on the YouTube channel, so the id needed by
    /// `[youtube] stream_id` can be found without leaving the interface.
    ListStreams,
    /// Fetch this platform's stream key and put it on the system clipboard.
    ///
    /// The key never travels to the UI half at all: it goes from the API
    /// straight to the clipboard here, and the UI only learns whether that
    /// worked. Nothing that could end up on screen or in the log ever holds
    /// the value.
    CopyStreamKey(Platform),
    /// Hand a URL to the system browser.
    ///
    /// This goes through the worker rather than happening in the key handler
    /// because launching a browser touches the outside world, and the rule in
    /// the UI half is that it only ever mutates state and returns commands.
    OpenUrl(String),
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
    /// Going live finished, successfully or otherwise, per platform. Carries
    /// the generation of the submission it answers.
    WentLive {
        results: Vec<PlatformResult>,
        generation: u64,
    },
    /// One platform's browser login finished. `Ok` carries the token-store key
    /// it was saved under.
    LoggedIn {
        platform: Platform,
        result: Result<String, String>,
    },
    /// Finishing the broadcast finished, per platform.
    Ended {
        results: Vec<(Platform, Result<crate::model::EndOutcome, String>)>,
    },
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
    let mut config = config;
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
                    Ok((mut built, failures)) => {
                        // A platform that failed to build (missing credentials,
                        // unrenewable login) is reported alongside the ones
                        // whose connect check actually ran, each under its own
                        // name with its own explanation — never one platform's
                        // error smeared over all of them.
                        let mut results: Vec<(Platform, Result<String, String>)> = failures
                            .into_iter()
                            .map(|(platform, err)| (platform, Err(err)))
                            .collect();
                        results.extend(built.connect_all().await);
                        results.sort_by_key(|(platform, _)| *platform);
                        engine = Some(built);
                        let _ = events.send(Event::Connected(results));
                    }
                    Err(err) => {
                        // Only a genuinely global failure lands here (such as
                        // the HTTP client not building), so showing the same
                        // text for every platform is truthful.
                        //
                        // The previous engine is dropped rather than kept: it
                        // was built for an earlier platform selection, and
                        // letting it linger meant a later go-live could act on
                        // a platform the user has since deselected.
                        engine = None;
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
                    // Answer anyway, with nothing: an empty reply is how the UI
                    // learns that the API list is unavailable and that it should
                    // fall back to its built-in list. Staying silent here is
                    // what used to leave the YouTube category field apparently
                    // dead until the first successful login.
                    let _ = events.send(Event::Categories {
                        platform,
                        results: Vec::new(),
                        generation,
                    });
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
                        // Answer the request even though it failed, so the popup
                        // stops saying "searching…" and shows "no matches"
                        // instead of hanging on a spinner that never resolves.
                        let _ = events.send(Event::Categories {
                            platform,
                            results: Vec::new(),
                            generation,
                        });
                    }
                }
            }

            Command::GoLive { plan, generation } => {
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

                let _ = events.send(Event::WentLive {
                    results,
                    generation,
                });
            }

            Command::EndLive => {
                let Some(engine) = engine.as_mut() else {
                    let _ = events.send(Event::Log {
                        level: LogLevel::Error,
                        message: "Not connected to any platform yet.".into(),
                    });
                    continue;
                };

                let _ = events.send(Event::Log {
                    level: LogLevel::Info,
                    message: "Finishing the broadcast…".into(),
                });

                let results = engine.end_live().await;
                for (platform, outcome) in &results {
                    let (level, message) = match outcome {
                        // "Nothing to end" is reported as information, not as
                        // success and not as a fault. Twitch answers that way
                        // every single time, and a green tick saying a
                        // platform was closed when nothing happened would be
                        // a lie told twice a session.
                        Ok(outcome) if outcome.changed_anything() => (
                            LogLevel::Success,
                            format!("{}: {}", platform.label(), outcome.message()),
                        ),
                        Ok(outcome) => (
                            LogLevel::Info,
                            format!("{}: {}", platform.label(), outcome.message()),
                        ),
                        Err(err) => (
                            LogLevel::Error,
                            format!("{} could not be finished: {err}", platform.label()),
                        ),
                    };
                    let _ = events.send(Event::Log { level, message });
                }

                let _ = events.send(Event::Ended { results });
            }

            Command::PollStats => {
                let Some(engine) = engine.as_mut() else {
                    continue;
                };
                let stats = engine.poll_stats().await;
                let _ = events.send(Event::Stats(stats));
            }

            Command::ReloadConfig(fresh) => {
                config = *fresh;
                // Any engine was built from the previous credentials, so it can
                // no longer be trusted to belong to this configuration.
                engine = None;
            }

            Command::Login(platforms) => {
                for platform in platforms {
                    let _ = events.send(Event::Log {
                        level: LogLevel::Info,
                        message: format!("Starting the {} login…", platform.label()),
                    });

                    // The login prints nothing: the interface owns the screen,
                    // so every progress line (including the URL to paste if no
                    // browser opens) goes to the activity log instead.
                    let sink = events.clone();
                    let notice = move |message: String| {
                        let _ = sink.send(Event::Log {
                            level: LogLevel::Info,
                            message,
                        });
                    };

                    let result = crate::auth::login_with(&config, platform, false, &notice)
                        .await
                        .map_err(|err| format!("{err:#}"));
                    if result.is_ok() {
                        // The saved token has changed, so a previously built
                        // engine is holding a stale one.
                        engine = None;
                    }
                    let _ = events.send(Event::LoggedIn { platform, result });
                }
            }

            Command::LoginAdd(platform) => {
                let _ = events.send(Event::Log {
                    level: LogLevel::Info,
                    message: format!(
                        "Starting an additional {} login — sign in as the other account.",
                        platform.label()
                    ),
                });
                let sink = events.clone();
                let notice = move |message: String| {
                    let _ = sink.send(Event::Log {
                        level: LogLevel::Info,
                        message,
                    });
                };
                let result = crate::auth::login_with(&config, platform, true, &notice)
                    .await
                    .map_err(|err| format!("{err:#}"));
                let _ = events.send(match result {
                    Ok(name) => Event::Log {
                        level: LogLevel::Success,
                        message: format!("Added the {} chat account {name}.", platform.label()),
                    },
                    Err(reason) => Event::Log {
                        level: LogLevel::Error,
                        message: format!("Could not add that account: {reason}"),
                    },
                });
            }

            Command::Logout(platform) => {
                let outcome = (|| -> anyhow::Result<()> {
                    let mut store = crate::auth::store::TokenStore::load()?;
                    store.remove(platform);
                    store.save()
                })();
                let _ = events.send(match outcome {
                    Ok(()) => {
                        // The engine holds a backend authenticated with the
                        // token that has just been thrown away, so it has to
                        // go too rather than carrying on with a login that no
                        // longer exists on disk.
                        engine = None;
                        Event::Log {
                            level: LogLevel::Success,
                            message: format!("Logged out of {}.", platform.label()),
                        }
                    }
                    Err(err) => Event::Log {
                        level: LogLevel::Error,
                        message: format!("Could not log out of {}: {err:#}", platform.label()),
                    },
                });
            }

            Command::Cleanup { delete } => {
                let _ = events.send(Event::Log {
                    level: LogLevel::Info,
                    message: "Looking for abandoned YouTube broadcasts…".into(),
                });
                match crate::maintenance::find_stale_broadcasts(&config).await {
                    Ok(stale) if stale.is_empty() => {
                        let _ = events.send(Event::Log {
                            level: LogLevel::Success,
                            message: "No abandoned broadcasts to clean up.".into(),
                        });
                    }
                    Ok(stale) if !delete => {
                        // Listing before deleting, always. These are things
                        // somebody made, and a command that removed them
                        // without showing them first would be asking for
                        // trust it has no way to earn.
                        for broadcast in &stale {
                            let _ = events.send(Event::Log {
                                level: LogLevel::Info,
                                message: format!("  {} — {}", broadcast.id, broadcast.title),
                            });
                        }
                        let _ = events.send(Event::Log {
                            level: LogLevel::Warning,
                            message: format!(
                                "{} abandoned broadcast(s). Press D again to delete them.",
                                stale.len()
                            ),
                        });
                    }
                    Ok(stale) => match crate::maintenance::delete_broadcasts(&config, &stale).await
                    {
                        Ok(report) => {
                            for (title, reason) in &report.failed {
                                let _ = events.send(Event::Log {
                                    level: LogLevel::Warning,
                                    message: format!("Could not delete {title}: {reason}"),
                                });
                            }
                            let _ = events.send(Event::Log {
                                level: LogLevel::Success,
                                message: report.describe(),
                            });
                        }
                        Err(err) => {
                            let _ = events.send(Event::Log {
                                level: LogLevel::Error,
                                message: format!("Cleanup failed: {err:#}"),
                            });
                        }
                    },
                    Err(err) => {
                        let _ = events.send(Event::Log {
                            level: LogLevel::Error,
                            message: format!("Could not look for broadcasts: {err:#}"),
                        });
                    }
                }
            }

            Command::ExportSuperchats => {
                let _ = events.send(match crate::maintenance::export_superchats(&config) {
                    Ok((path, rows)) => Event::Log {
                        level: LogLevel::Success,
                        message: format!("Exported {rows} paid event(s) to {}", path.display()),
                    },
                    Err(err) => Event::Log {
                        level: LogLevel::Error,
                        message: format!("Export failed: {err:#}"),
                    },
                });
            }

            Command::ListStreams => {
                let Some(engine) = engine.as_mut() else {
                    let _ = events.send(Event::Log {
                        level: LogLevel::Error,
                        message: "Not connected yet, so there are no streams to list.".into(),
                    });
                    continue;
                };
                match engine.list_ingest_endpoints(Platform::YouTube).await {
                    Ok(endpoints) if endpoints.is_empty() => {
                        let _ = events.send(Event::Log {
                            level: LogLevel::Warning,
                            message: "This channel has no reusable stream keys yet.".into(),
                        });
                    }
                    Ok(endpoints) => {
                        for endpoint in endpoints {
                            // The id, never the key: this window is often part
                            // of the broadcast, and the id is the only half
                            // needed for `[youtube] stream_id`.
                            let _ = events.send(Event::Log {
                                level: LogLevel::Info,
                                message: format!("  {} — {}", endpoint.id, endpoint.title),
                            });
                        }
                    }
                    Err(err) => {
                        let _ = events.send(Event::Log {
                            level: LogLevel::Error,
                            message: format!("Could not list the streams: {err:#}"),
                        });
                    }
                }
            }

            Command::CopyStreamKey(platform) => {
                let Some(engine) = engine.as_mut() else {
                    let _ = events.send(Event::Log {
                        level: LogLevel::Error,
                        message: "Not connected yet, so there is no stream key to copy.".into(),
                    });
                    continue;
                };
                match engine.stream_key(platform).await {
                    Ok(Some(key)) => {
                        let outcome = crate::clipboard::copy(&key);
                        // Drop the key as soon as the copy is done rather than
                        // letting it live until the end of the match arm.
                        drop(key);
                        let _ = events.send(match outcome {
                            Ok(()) => Event::Log {
                                level: LogLevel::Success,
                                message: format!(
                                    "{} stream key copied to the clipboard — paste it into OBS.",
                                    platform.label()
                                ),
                            },
                            Err(err) => Event::Log {
                                level: LogLevel::Error,
                                message: format!("Could not copy the stream key: {err:#}"),
                            },
                        });
                    }
                    Ok(None) => {
                        let _ = events.send(Event::Log {
                            level: LogLevel::Warning,
                            message: format!(
                                "{} did not return a stream key. On Twitch this usually means the \
                                 saved login predates the `channel:read:stream_key` permission — \
                                 log in again under Config → Accounts.",
                                platform.label()
                            ),
                        });
                    }
                    Err(err) => {
                        let _ = events.send(Event::Log {
                            level: LogLevel::Error,
                            message: format!(
                                "Could not fetch the {} stream key: {err:#}",
                                platform.label()
                            ),
                        });
                    }
                }
            }

            Command::OpenUrl(url) => {
                // `that_detached` starts the browser and returns immediately.
                // The plain `that` would wait for the browser process to exit,
                // which on a fresh browser launch means the worker would stop
                // answering the dashboard for the rest of the session.
                match open::that_detached(&url) {
                    Ok(()) => {
                        let _ = events.send(Event::Log {
                            level: LogLevel::Info,
                            message: format!("Opened {url} in your browser."),
                        });
                    }
                    Err(err) => {
                        // Headless machines and bare window managers often have
                        // no browser to hand. Print the URL so it can be copied
                        // out of the log by hand.
                        let _ = events.send(Event::Log {
                            level: LogLevel::Warning,
                            message: format!(
                                "Could not open a browser ({err}). The page is at {url}"
                            ),
                        });
                    }
                }
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
            .send(Command::GoLive {
                plan: Box::default(),
                generation: 1,
            })
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
    async fn searching_before_connecting_answers_with_nothing_rather_than_staying_silent() {
        // The UI treats an empty reply as "the API list is unavailable" and
        // falls back to its built-in YouTube category list. That only works if
        // an unanswerable search still produces a reply — staying silent is what
        // used to leave the category field looking dead before the first login.
        let (command_tx, command_rx) = mpsc::channel(4);
        let (event_tx, mut event_rx) = mpsc::unbounded_channel();

        let handle = tokio::spawn(run(Config::default(), command_rx, event_tx));

        command_tx
            .send(Command::SearchCategories {
                platform: Platform::YouTube,
                query: "gam".into(),
                generation: 7,
            })
            .await
            .unwrap();

        match event_rx.recv().await.expect("a reply should arrive") {
            Event::Categories {
                platform,
                results,
                generation,
            } => {
                assert_eq!(platform, Platform::YouTube);
                assert!(results.is_empty());
                // The generation is echoed back, or the UI would discard the
                // reply as stale and go on waiting forever.
                assert_eq!(generation, 7);
            }
            other => panic!("expected a category reply, got {other:?}"),
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
