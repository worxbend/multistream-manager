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
    /// Forget one extra chat account by its token-store key.
    ///
    /// Separate from `Logout`, which removes the account you stream as.
    /// `TokenStore::remove` only ever deleted the bare platform slug, so an
    /// extra account added by mistake could not be taken out at all and its
    /// refresh token stayed valid indefinitely.
    ForgetAccount { key: String, label: String },
    /// Find the YouTube broadcasts that were created and never went live.
    ///
    /// `approved` is empty for the listing press. On the confirming press it
    /// carries the ids the user was actually shown, and only those are
    /// deleted: the confirming press used to re-list and delete whatever came
    /// back, so a broadcast created between the two presses — the one you had
    /// just set up for tonight, say — was deleted without ever having
    /// appeared in the list you approved.
    Cleanup { approved: Vec<String> },
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
    /// A logout finished, with whether it actually worked.
    ///
    /// The interface used to clear the login flag the moment it *sent* the
    /// command, so a logout that failed to write left the screen saying you
    /// were logged out while the token was still on disk.
    LoggedOut {
        platform: Platform,
        result: Result<(), String>,
    },
    /// The abandoned broadcasts a listing found, so the confirming press can
    /// send back exactly what was shown.
    StaleBroadcasts(Vec<crate::model::StaleBroadcast>),
    /// The reusable YouTube stream ids on the channel, as `(id, title)`.
    ///
    /// Deliberately not the endpoints themselves: an `IngestEndpoint` carries
    /// the stream *key*, and this window is often part of the broadcast. The
    /// key never leaves the worker.
    Streams(Vec<(String, String)>),
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
    ledger: crate::quota::QuotaStore,
) {
    let mut config = config;
    let mut engine: Option<Engine> = None;

    while let Some(command) = commands.recv().await {
        match command {
            Command::Connect(platforms) => {
                handle_connect(&config, &ledger, &events, &mut engine, platforms).await
            }
            Command::SearchCategories {
                platform,
                query,
                generation,
            } => handle_search_categories(&mut engine, &events, platform, query, generation).await,
            Command::GoLive { plan, generation } => {
                handle_go_live(&mut engine, &events, plan, generation).await
            }
            Command::EndLive => handle_end_live(&mut engine, &events).await,
            Command::PollStats => handle_poll_stats(&mut engine, &events).await,
            Command::ReloadConfig(fresh) => {
                handle_reload_config(&mut config, &mut engine, fresh).await
            }
            Command::Login(platforms) => {
                handle_login(&config, &events, &mut engine, platforms).await
            }
            Command::LoginAdd(platform) => handle_login_add(&config, &events, platform).await,
            Command::Logout(platform) => handle_logout(&events, &mut engine, platform).await,
            Command::ForgetAccount { key, label } => {
                handle_forget_account(&events, key, label).await
            }
            Command::Cleanup { approved } => {
                handle_cleanup(&config, &ledger, &events, approved).await
            }
            Command::ExportSuperchats => handle_export_superchats(&config, &events).await,
            Command::ListStreams => handle_list_streams(&config, &ledger, &events).await,
            Command::CopyStreamKey(platform) => {
                handle_copy_stream_key(&mut engine, &events, platform).await
            }
            Command::OpenUrl(url) => handle_open_url(&events, url).await,
        }
    }
}

/// Build backends for `platforms` and verify the logins, replacing `engine`.
async fn handle_connect(
    config: &Config,
    ledger: &crate::quota::QuotaStore,
    events: &mpsc::UnboundedSender<Event>,
    engine: &mut Option<Engine>,
    platforms: Vec<Platform>,
) {
    if platforms.is_empty() {
        let _ = events.send(Event::Log {
            level: LogLevel::Warning,
            message: "Select at least one platform first.".into(),
        });
        return;
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

    match Engine::build(config, &platforms, ledger.clone()).await {
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
            *engine = Some(built);
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
            *engine = None;
            let _ = events.send(Event::Connected(
                platforms
                    .into_iter()
                    .map(|p| (p, Err(format!("{err:#}"))))
                    .collect(),
            ));
        }
    }
}

async fn handle_search_categories(
    engine: &mut Option<Engine>,
    events: &mpsc::UnboundedSender<Event>,
    platform: Platform,
    query: String,
    generation: u64,
) {
    let Some(engine) = engine.as_mut() else {
        // Not connected yet, so there is nothing to search against.
        // Answer anyway, with nothing: an empty reply is how the UI
        // learns that the API list is unavailable and that it should
        // fall back to its built-in list. Staying silent here is
        // what used to leave the YouTube category field apparently
        // dead until the first successful login.
        //
        // A real typed query landing here is worth a word in the log: this
        // and a search that genuinely found nothing both surface as the same
        // bare "no matches" popup, with no way to tell them apart otherwise —
        // which is exactly what made a search racing a reconnect look
        // indistinguishable from Twitch simply not having that category.
        if !query.trim().is_empty() {
            let _ = events.send(Event::Log {
                level: LogLevel::Warning,
                message: format!(
                    "{} isn't connected yet, so \"{query}\" couldn't be searched. Try again \
                     once it finishes connecting.",
                    platform.label()
                ),
            });
        }
        let _ = events.send(Event::Categories {
            platform,
            results: Vec::new(),
            generation,
        });
        return;
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

async fn handle_go_live(
    engine: &mut Option<Engine>,
    events: &mpsc::UnboundedSender<Event>,
    plan: Box<StreamPlan>,
    generation: u64,
) {
    let Some(engine) = engine.as_mut() else {
        let _ = events.send(Event::Log {
            level: LogLevel::Error,
            message: "Not connected to any platform yet.".into(),
        });
        return;
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

async fn handle_end_live(engine: &mut Option<Engine>, events: &mpsc::UnboundedSender<Event>) {
    let Some(engine) = engine.as_mut() else {
        let _ = events.send(Event::Log {
            level: LogLevel::Error,
            message: "Not connected to any platform yet.".into(),
        });
        return;
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

async fn handle_poll_stats(engine: &mut Option<Engine>, events: &mpsc::UnboundedSender<Event>) {
    let Some(engine) = engine.as_mut() else {
        return;
    };
    let stats = engine.poll_stats().await;
    let _ = events.send(Event::Stats(stats));
}

async fn handle_reload_config(
    config: &mut Config,
    engine: &mut Option<Engine>,
    fresh: Box<Config>,
) {
    // Only drop the engine when something it was built from
    // actually changed. This command is sent on every save — which
    // includes picking a theme, moving a layout panel and toggling
    // a notification — and discarding the engine there meant the
    // next statistics poll had to rebuild it and redo the token
    // work, so the dashboard would blank out and refill because
    // somebody changed a colour.
    if config.credentials_fingerprint() != fresh.credentials_fingerprint() {
        *engine = None;
    }
    *config = *fresh;
}

async fn handle_login(
    config: &Config,
    events: &mpsc::UnboundedSender<Event>,
    engine: &mut Option<Engine>,
    platforms: Vec<Platform>,
) {
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

        let result = crate::auth::login_with(config, platform, false, &notice)
            .await
            .map_err(|err| format!("{err:#}"));
        if result.is_ok() {
            // The saved token has changed, so a previously built
            // engine is holding a stale one.
            *engine = None;
        }
        let _ = events.send(Event::LoggedIn { platform, result });
    }
}

async fn handle_login_add(
    config: &Config,
    events: &mpsc::UnboundedSender<Event>,
    platform: Platform,
) {
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
    let result = crate::auth::login_with(config, platform, true, &notice)
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

async fn handle_logout(
    events: &mpsc::UnboundedSender<Event>,
    engine: &mut Option<Engine>,
    platform: Platform,
) {
    // Through `mutate_store`, so the whole read-change-write cycle
    // happens under the cross-process lock. Doing its own load and
    // save meant a token refresh running concurrently could write
    // its snapshot afterwards and quietly restore the login that
    // had just been forgotten.
    let outcome = crate::auth::mutate_store(move |store| {
        store.remove(platform);
    })
    .await;
    let _ = events.send(match &outcome {
        Ok(()) => {
            // The engine holds a backend authenticated with the
            // token that has just been thrown away, so it has to
            // go too rather than carrying on with a login that no
            // longer exists on disk.
            *engine = None;
            Event::Log {
                level: LogLevel::Success,
                // Say what was and was not done. This deletes the
                // token here; it does not revoke it at the
                // provider, and somebody logging out *because* a
                // token leaked needs to know the difference — a
                // refresh token stays valid for months.
                message: format!(
                    "Logged out of {}. The token is deleted from this machine; to \
                     revoke it at {} as well, remove this application from your \
                     account's connections page.",
                    platform.label(),
                    platform.label()
                ),
            }
        }
        Err(err) => Event::Log {
            level: LogLevel::Error,
            message: format!("Could not log out of {}: {err:#}", platform.label()),
        },
    });
    // …and the outcome itself, so the interface reports what
    // happened rather than what it hoped would happen.
    let _ = events.send(Event::LoggedOut {
        platform,
        result: outcome.map_err(|err| format!("{err:#}")),
    });
}

async fn handle_forget_account(events: &mpsc::UnboundedSender<Event>, key: String, label: String) {
    let removing = key.clone();
    let outcome = crate::auth::mutate_store(move |store| {
        store.remove_keyed(&removing);
    })
    .await;
    let _ = events.send(match outcome {
        Ok(()) => Event::Log {
            level: LogLevel::Success,
            message: format!("Forgot {label}. Its token is no longer on disk."),
        },
        Err(err) => Event::Log {
            level: LogLevel::Error,
            message: format!("Could not forget {label}: {err:#}"),
        },
    });
}

async fn handle_cleanup(
    config: &Config,
    ledger: &crate::quota::QuotaStore,
    events: &mpsc::UnboundedSender<Event>,
    approved: Vec<String>,
) {
    let confirming = !approved.is_empty();
    let _ = events.send(Event::Log {
        level: LogLevel::Info,
        message: "Looking for abandoned YouTube broadcasts…".into(),
    });

    // The list is always taken fresh, even on the confirming
    // press — but on that press it is used to *narrow* what was
    // approved, never to widen it.
    let listing = match crate::maintenance::find_stale_broadcasts(config, ledger.clone()).await {
        Ok(listing) => listing,
        Err(err) => {
            let _ = events.send(Event::Log {
                level: LogLevel::Error,
                message: format!("Cleanup failed: {err:#}"),
            });
            let _ = events.send(Event::StaleBroadcasts(Vec::new()));
            return;
        }
    };

    let found = listing.broadcasts;
    // The page cap is real and used to be silent. A channel with
    // more than five hundred broadcasts is precisely the one with
    // hundreds to clear, and it was told a number that was not
    // the whole number.
    if listing.truncated {
        let _ = events.send(Event::Log {
            level: LogLevel::Warning,
            message: "There are more broadcasts than one listing can reach — run \
                      this again after deleting these to catch the rest."
                .into(),
        });
    }

    if found.is_empty() {
        let _ = events.send(Event::Log {
            level: LogLevel::Success,
            message: "No abandoned broadcasts to clean up.".into(),
        });
        let _ = events.send(Event::StaleBroadcasts(Vec::new()));
        return;
    }

    if !confirming {
        // Listing before deleting, always. These are things
        // somebody made, and a command that removed them without
        // showing them first would be asking for trust it has no
        // way to earn.
        for broadcast in &found {
            // The status and the scheduled time are the whole
            // reason each one was picked out, and the line used to
            // show neither.
            let when = broadcast
                .scheduled_start
                .map(|at| {
                    format!(
                        ", scheduled {}",
                        at.with_timezone(&chrono::Local).format("%d %b %H:%M")
                    )
                })
                .unwrap_or_default();
            let _ = events.send(Event::Log {
                level: LogLevel::Info,
                message: format!(
                    "  {} — {}{when} (id {})",
                    broadcast.title, broadcast.status, broadcast.id
                ),
            });
        }
        // What it will cost. Fifty units per delete is one of the
        // few things in this program that can spend a real slice
        // of the day's YouTube allowance, and clearing a
        // long-neglected channel is exactly when it bites.
        let cost = found.len() as u64 * crate::quota::cost::DELETE_BROADCAST;
        let budget = match ledger.summary() {
            Some((used, limit, _)) => format!(
                " That will spend about {cost} units of the {} you have left today.",
                limit.saturating_sub(used)
            ),
            None => String::new(),
        };
        let _ = events.send(Event::Log {
            level: LogLevel::Warning,
            message: format!(
                "{} abandoned broadcast(s). Press enter again to delete them.{budget}",
                found.len()
            ),
        });
        let _ = events.send(Event::StaleBroadcasts(found));
        return;
    }

    // Only what was both approved and still abandoned.
    let (to_delete, appeared): (Vec<_>, Vec<_>) = found
        .into_iter()
        .partition(|broadcast| approved.contains(&broadcast.id));
    if !appeared.is_empty() {
        let _ = events.send(Event::Log {
            level: LogLevel::Warning,
            message: format!(
                "{} broadcast(s) appeared since the list and were left alone.",
                appeared.len()
            ),
        });
    }
    if to_delete.is_empty() {
        let _ = events.send(Event::Log {
            level: LogLevel::Success,
            message: "Nothing left to delete — the listed broadcasts are already gone.".into(),
        });
        let _ = events.send(Event::StaleBroadcasts(Vec::new()));
        return;
    }

    match crate::maintenance::delete_broadcasts(config, &to_delete, ledger.clone()).await {
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
    }
    let _ = events.send(Event::StaleBroadcasts(Vec::new()));
}

async fn handle_export_superchats(config: &Config, events: &mpsc::UnboundedSender<Event>) {
    // On the blocking pool, like the clipboard path below. This
    // line-reads and JSON-parses every rotated chat log — on a
    // long-running channel that is megabytes — and doing it in
    // the command loop stalled every other command behind it.
    let config_for_export = config.clone();
    let exported = tokio::task::spawn_blocking(move || {
        crate::maintenance::export_superchats(&config_for_export)
    })
    .await
    .unwrap_or_else(|err| Err(anyhow::anyhow!("the export task did not finish: {err}")));
    let _ = events.send(match exported {
        // Zero rows is almost always the same cause: the export
        // reads the chat logs, and the chat log is off by
        // default. Reporting a successful export of nothing was
        // technically true and completely useless — the
        // dependency was stated only in prose in the docs.
        Ok((path, 0)) if !config.chat.chat_logging => Event::Log {
            level: LogLevel::Warning,
            message: format!(
                "Nothing to export: chat logging is off, so no paid events have \
                 been recorded. Turn it on under Config → Chat and it will record \
                 from now on. (Wrote an empty {}.)",
                path.display()
            ),
        },
        Ok((path, 0)) => Event::Log {
            level: LogLevel::Warning,
            message: format!(
                "No paid events found in the chat logs — wrote an empty {}.",
                path.display()
            ),
        },
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

async fn handle_list_streams(
    config: &Config,
    ledger: &crate::quota::QuotaStore,
    events: &mpsc::UnboundedSender<Event>,
) {
    // Its own connection, like the other two housekeeping jobs.
    // Requiring the shared engine meant the one job most needed
    // during first-run setup — finding the id to pin, before you
    // have ever gone live — was the only one that refused then.
    match crate::maintenance::list_stream_ids(config, ledger.clone()).await {
        Ok(endpoints) if endpoints.is_empty() => {
            let _ = events.send(Event::Log {
                level: LogLevel::Warning,
                message: "This channel has no reusable stream keys yet.".into(),
            });
        }
        Ok(endpoints) => {
            // The id and the title, never the key: this window is
            // often part of the broadcast, and the id is the only
            // half `[youtube] stream_id` needs.
            let listed: Vec<(String, String)> = endpoints
                .into_iter()
                .map(|endpoint| (endpoint.id, endpoint.title))
                .collect();
            for (id, title) in &listed {
                let _ = events.send(Event::Log {
                    level: LogLevel::Info,
                    message: format!("  {id} — {title}"),
                });
            }
            let _ = events.send(Event::Streams(listed));
        }
        Err(err) => {
            let _ = events.send(Event::Log {
                level: LogLevel::Error,
                message: format!("Could not list the streams: {err:#}"),
            });
        }
    }
}

async fn handle_copy_stream_key(
    engine: &mut Option<Engine>,
    events: &mpsc::UnboundedSender<Event>,
    platform: Platform,
) {
    let Some(engine) = engine.as_mut() else {
        let _ = events.send(Event::Log {
            level: LogLevel::Error,
            message: "Not connected yet, so there is no stream key to copy.".into(),
        });
        return;
    };
    match engine.stream_key(platform).await {
        Ok(Some(key)) => {
            // On the blocking pool: copying spawns a helper
            // program and writes to its stdin, and a helper that
            // hangs (an xclip with no X display to answer it)
            // would otherwise block a tokio worker thread — one
            // shared with the chat tasks and the OBS connection.
            //
            // The key is moved in and dropped inside the closure,
            // so it stops existing as soon as the copy is done
            // rather than living to the end of the match arm.
            let outcome = tokio::task::spawn_blocking(move || {
                let outcome = crate::clipboard::copy(&key);
                drop(key);
                outcome
            })
            .await
            .unwrap_or_else(|err| Err(anyhow::anyhow!("the clipboard task did not finish: {err}")));
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

async fn handle_open_url(events: &mpsc::UnboundedSender<Event>, url: String) {
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
                message: format!("Could not open a browser ({err}). The page is at {url}"),
            });
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

        let handle = tokio::spawn(run(
            Config::default(),
            command_rx,
            event_tx,
            crate::quota::QuotaStore::new(0, None),
        ));

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

        let handle = tokio::spawn(run(
            Config::default(),
            command_rx,
            event_tx,
            crate::quota::QuotaStore::new(0, None),
        ));

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

    /// Every command that needs an engine has to say so when there is none.
    /// The rule matters because the worker is a loop that answers on a
    /// channel: a command it silently ignores leaves the interface waiting
    /// for a reply that will never come, with a spinner and no explanation.
    #[tokio::test]
    async fn ending_before_connecting_reports_an_error_rather_than_silence() {
        let (command_tx, command_rx) = mpsc::channel(4);
        let (event_tx, mut event_rx) = mpsc::unbounded_channel();

        let handle = tokio::spawn(run(
            Config::default(),
            command_rx,
            event_tx,
            crate::quota::QuotaStore::new(0, None),
        ));

        command_tx.send(Command::EndLive).await.unwrap();

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

    /// A category search before connecting used to answer with a bare empty
    /// list — indistinguishable, from the popup alone, from Twitch genuinely
    /// having no match for what was typed. A real typed query now gets a
    /// warning explaining which of those two it actually was.
    #[tokio::test]
    async fn searching_categories_before_connecting_explains_the_empty_reply() {
        let (command_tx, command_rx) = mpsc::channel(4);
        let (event_tx, mut event_rx) = mpsc::unbounded_channel();

        let handle = tokio::spawn(run(
            Config::default(),
            command_rx,
            event_tx,
            crate::quota::QuotaStore::new(0, None),
        ));

        command_tx
            .send(Command::SearchCategories {
                platform: Platform::Twitch,
                query: "Science & Technology".to_string(),
                generation: 1,
            })
            .await
            .unwrap();

        // The warning is sent before the reply, so it arrives first.
        match event_rx.recv().await.expect("a warning should arrive") {
            Event::Log { level, message } => {
                assert_eq!(level, LogLevel::Warning);
                assert!(message.contains("isn't connected"));
                assert!(message.contains("Science & Technology"));
            }
            other => panic!("expected a warning explaining the empty reply, got {other:?}"),
        }

        match event_rx
            .recv()
            .await
            .expect("the empty reply should follow")
        {
            Event::Categories {
                platform,
                results,
                generation,
            } => {
                assert_eq!(platform, Platform::Twitch);
                assert!(results.is_empty());
                assert_eq!(generation, 1);
            }
            other => panic!("expected the empty reply, got {other:?}"),
        }

        drop(command_tx);
        handle.await.unwrap();
    }

    /// An empty field has nothing to search for regardless of connection
    /// state, so racing a reconnect is not what is wrong here — it would be
    /// noise, not a diagnostic, to warn about it the way a real query does.
    /// The warning, when there is one, is always sent before the reply, so
    /// the reply arriving first is itself the proof nothing was said.
    #[tokio::test]
    async fn searching_an_empty_category_before_connecting_stays_quiet() {
        let (command_tx, command_rx) = mpsc::channel(4);
        let (event_tx, mut event_rx) = mpsc::unbounded_channel();

        let handle = tokio::spawn(run(
            Config::default(),
            command_rx,
            event_tx,
            crate::quota::QuotaStore::new(0, None),
        ));

        command_tx
            .send(Command::SearchCategories {
                platform: Platform::Twitch,
                query: String::new(),
                generation: 1,
            })
            .await
            .unwrap();

        match event_rx.recv().await.expect("an empty reply should arrive") {
            Event::Categories { results, .. } => assert!(results.is_empty()),
            other => panic!("expected the empty reply with no warning first, got {other:?}"),
        }

        drop(command_tx);
        handle.await.unwrap();
    }

    /// A statistics poll is the one command that is *allowed* to say nothing
    /// when there is no engine: it runs on a timer, and a log line every
    /// fifteen seconds saying "still not connected" would bury everything
    /// else in the log.
    #[tokio::test]
    async fn polling_before_connecting_stays_quiet() {
        let (command_tx, command_rx) = mpsc::channel(4);
        let (event_tx, mut event_rx) = mpsc::unbounded_channel();

        let handle = tokio::spawn(run(
            Config::default(),
            command_rx,
            event_tx,
            crate::quota::QuotaStore::new(0, None),
        ));

        command_tx.send(Command::PollStats).await.unwrap();
        // Followed by something that does answer, so the test can tell
        // "nothing yet" from "nothing ever" without sleeping.
        command_tx.send(Command::Connect(vec![])).await.unwrap();

        match event_rx.recv().await.expect("the second command answers") {
            Event::Log { message, .. } => assert!(
                message.contains("at least one platform"),
                "the poll must not have produced a line of its own: {message}"
            ),
            other => panic!("expected a log line, got {other:?}"),
        }

        drop(command_tx);
        handle.await.unwrap();
    }

    /// The worker keeps its own copy of the config. When the *credentials*
    /// change, adopting the new config has to drop the engine with it: an
    /// engine built from the old credentials would keep using them, which
    /// looks exactly like the setup screen having silently failed to save.
    #[tokio::test]
    async fn reloading_a_config_with_new_credentials_forgets_the_old_engine() {
        let (command_tx, command_rx) = mpsc::channel(4);
        let (event_tx, mut event_rx) = mpsc::unbounded_channel();

        let handle = tokio::spawn(run(
            Config::default(),
            command_rx,
            event_tx,
            crate::quota::QuotaStore::new(0, None),
        ));

        // Connecting with no credentials still builds an engine — one whose
        // platforms all failed — which is enough to tell whether it survived.
        command_tx
            .send(Command::Connect(vec![Platform::Twitch]))
            .await
            .unwrap();
        // Drain until the connection answers.
        loop {
            match event_rx.recv().await.expect("the connect answers") {
                Event::Connected(_) => break,
                _ => continue,
            }
        }

        let mut changed = Config::default();
        changed.twitch.client_id = "a-new-client-id".into();
        command_tx
            .send(Command::ReloadConfig(Box::new(changed)))
            .await
            .unwrap();
        command_tx.send(Command::EndLive).await.unwrap();

        match event_rx.recv().await.expect("an answer arrives") {
            Event::Log { level, message } => {
                assert_eq!(level, LogLevel::Error);
                assert!(
                    message.contains("Not connected"),
                    "the old engine must have been dropped: {message}"
                );
            }
            other => panic!("expected a log line, got {other:?}"),
        }

        drop(command_tx);
        handle.await.unwrap();
    }

    /// The counterpart. This command is sent on *every* save, which includes
    /// picking a theme, moving a layout panel and toggling a notification.
    /// Dropping the engine there cost a full round of token work and blanked
    /// the dashboard because somebody changed a colour.
    #[tokio::test]
    async fn reloading_a_config_that_changed_only_appearance_keeps_the_engine() {
        let (command_tx, command_rx) = mpsc::channel(4);
        let (event_tx, mut event_rx) = mpsc::unbounded_channel();

        let handle = tokio::spawn(run(
            Config::default(),
            command_rx,
            event_tx,
            crate::quota::QuotaStore::new(0, None),
        ));

        command_tx
            .send(Command::Connect(vec![Platform::Twitch]))
            .await
            .unwrap();
        loop {
            match event_rx.recv().await.expect("the connect answers") {
                Event::Connected(_) => break,
                _ => continue,
            }
        }

        let mut cosmetic = Config::default();
        cosmetic.appearance.theme = "some-other-theme".into();
        command_tx
            .send(Command::ReloadConfig(Box::new(cosmetic)))
            .await
            .unwrap();
        command_tx.send(Command::EndLive).await.unwrap();

        match event_rx.recv().await.expect("an answer arrives") {
            Event::Log { message, .. } => {
                assert!(
                    !message.contains("Not connected"),
                    "a cosmetic change must not throw the engine away: {message}"
                );
            }
            other => panic!("expected a log line, got {other:?}"),
        }

        drop(command_tx);
        handle.await.unwrap();
    }

    /// Dropping the command sender is the only shutdown signal there is, and
    /// the loop has to end on it — a worker that outlived the interface would
    /// hold the runtime open and the program would not exit.
    #[tokio::test]
    async fn dropping_the_commands_ends_the_worker() {
        let (command_tx, command_rx) = mpsc::channel(4);
        let (event_tx, _event_rx) = mpsc::unbounded_channel();

        let handle = tokio::spawn(run(
            Config::default(),
            command_rx,
            event_tx,
            crate::quota::QuotaStore::new(0, None),
        ));
        drop(command_tx);

        tokio::time::timeout(std::time::Duration::from_secs(5), handle)
            .await
            .expect("the worker must end when its commands do")
            .expect("and not by panicking");
    }

    #[tokio::test]
    async fn searching_before_connecting_answers_with_nothing_rather_than_staying_silent() {
        // The UI treats an empty reply as "the API list is unavailable" and
        // falls back to its built-in YouTube category list. That only works if
        // an unanswerable search still produces a reply — staying silent is what
        // used to leave the category field looking dead before the first login.
        // A real typed query also explains itself first, the same as it does
        // for Twitch (see the dedicated tests above).
        let (command_tx, command_rx) = mpsc::channel(4);
        let (event_tx, mut event_rx) = mpsc::unbounded_channel();

        let handle = tokio::spawn(run(
            Config::default(),
            command_rx,
            event_tx,
            crate::quota::QuotaStore::new(0, None),
        ));

        command_tx
            .send(Command::SearchCategories {
                platform: Platform::YouTube,
                query: "gam".into(),
                generation: 7,
            })
            .await
            .unwrap();

        match event_rx.recv().await.expect("a warning should arrive") {
            Event::Log { level, message } => {
                assert_eq!(level, LogLevel::Warning);
                assert!(message.contains("YouTube"));
                assert!(message.contains("gam"));
            }
            other => panic!("expected a warning explaining the empty reply, got {other:?}"),
        }

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

        let handle = tokio::spawn(run(
            Config::default(),
            command_rx,
            event_tx,
            crate::quota::QuotaStore::new(0, None),
        ));

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

        let handle = tokio::spawn(run(
            Config::default(),
            command_rx,
            event_tx,
            crate::quota::QuotaStore::new(0, None),
        ));
        drop(command_tx);

        // Completing at all proves the loop exits rather than hanging.
        handle.await.unwrap();
    }
}
