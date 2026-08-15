//! The terminal UI: setup, teardown and the main event loop.

pub mod app;
pub mod draw;
pub mod input;
pub mod worker;

use anyhow::{Context, Result};
use crossterm::event::{Event as TermEvent, EventStream, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use std::io::Stdout;
use tokio::sync::mpsc;
use tokio_stream::StreamExt as _;

use crate::config::Config;
use app::{App, Screen};

/// A terminal that restores itself when dropped.
///
/// Without this, a panic anywhere in the drawing code would leave the terminal
/// in raw mode with the alternate screen still active — no echo, no line
/// editing, nothing visible. The user would have to blindly type `reset`.
/// Tying restoration to `Drop` means it happens on every exit path, including
/// panics and `?` propagation.
struct TerminalGuard {
    terminal: Terminal<CrosstermBackend<Stdout>>,
}

impl TerminalGuard {
    fn new() -> Result<Self> {
        enable_raw_mode().context("switching the terminal into raw mode")?;
        let mut stdout = std::io::stdout();
        execute!(stdout, EnterAlternateScreen).context("opening the alternate screen")?;
        let terminal = Terminal::new(CrosstermBackend::new(stdout))
            .context("initialising the terminal backend")?;
        Ok(Self { terminal })
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        // Every step is best-effort: if restoring fails there is nothing useful
        // left to do about it, and returning an error from `drop` is impossible.
        let _ = disable_raw_mode();
        let _ = execute!(self.terminal.backend_mut(), LeaveAlternateScreen);
        let _ = self.terminal.show_cursor();
    }
}

/// Run the interactive application until the user quits.
pub async fn run(config: Config) -> Result<()> {
    // Channels between the UI and the worker. Commands are bounded because a
    // backlog of them would mean the user is queueing work faster than the APIs
    // can absorb it; events are unbounded so the worker never blocks on the UI.
    let (command_tx, command_rx) = mpsc::channel(32);
    let (event_tx, mut event_rx) = mpsc::unbounded_channel();

    let worker = tokio::spawn(worker::run(config.clone(), command_rx, event_tx));

    let mut guard = TerminalGuard::new()?;
    let mut app = App::new(config);
    let mut keys = EventStream::new();

    // Drives the periodic statistics refresh on the dashboard.
    let mut ticker = tokio::time::interval(app.config.poll_interval());
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    // A slower tick that only redraws, so the uptime counter advances smoothly
    // between the (much more expensive) statistics polls.
    let mut redraw = tokio::time::interval(std::time::Duration::from_millis(500));

    loop {
        guard
            .terminal
            .draw(|frame| draw::draw(frame, &app))
            .context("drawing the interface")?;

        if app.should_quit {
            break;
        }

        tokio::select! {
            // Keyboard input.
            Some(event) = keys.next() => {
                match event {
                    Ok(TermEvent::Key(key)) => {
                        // Windows reports both press and release; acting on both
                        // would make every keystroke register twice.
                        if key.kind != KeyEventKind::Release {
                            for command in app.handle_key(key) {
                                // `try_send`, never `send().await`: this loop is
                                // the only thing that draws the screen and reads
                                // the keyboard. An awaited send on a full channel
                                // would park it — no redraw, no Ctrl+C — until
                                // the single-threaded worker frees a slot, which
                                // behind a stalled network call can take minutes.
                                // A full queue already means the worker is far
                                // behind, so dropping the command and saying so
                                // beats freezing the whole interface.
                                use tokio::sync::mpsc::error::TrySendError;
                                match command_tx.try_send(command) {
                                    Ok(()) => {}
                                    Err(TrySendError::Full(_)) => {
                                        app.push_log(
                                            worker::LogLevel::Error,
                                            "Still busy with earlier requests — that key press was ignored, try again in a moment.",
                                        );
                                    }
                                    Err(TrySendError::Closed(_)) => {
                                        // The worker has gone; nothing more can happen.
                                        app.should_quit = true;
                                    }
                                }
                            }
                        }
                    }
                    Ok(TermEvent::Resize(_, _)) => {
                        // The loop redraws at the top of every iteration, so a
                        // resize needs no special handling beyond waking up.
                    }
                    Ok(_) => {}
                    Err(err) => {
                        tracing::error!(?err, "terminal input failed");
                        app.should_quit = true;
                    }
                }
            }

            // Results coming back from the worker.
            Some(event) = event_rx.recv() => {
                app.handle_event(event);
            }

            // Periodic statistics refresh, only once there is something to poll.
            _ = ticker.tick() => {
                if app.screen == Screen::Dashboard && !app.results.is_empty() {
                    // Dropping a poll is harmless — the next tick simply asks
                    // again — but leave a trace so a stretch of missing stats
                    // can be explained from the log.
                    if command_tx.try_send(worker::Command::PollStats).is_err() {
                        tracing::debug!("statistics poll skipped: the worker is still busy");
                    }
                }
            }

            // Plain redraw tick, so counters stay current.
            _ = redraw.tick() => {}
        }
    }

    // Dropping the sender tells the worker to finish, and dropping the guard
    // restores the terminal. Order matters only in that both must happen.
    drop(command_tx);
    drop(guard);

    // Give the worker a moment to wind down, but do not hang on it: a request
    // already in flight to YouTube could take several seconds, and the user has
    // asked to quit.
    let _ = tokio::time::timeout(std::time::Duration::from_millis(500), worker).await;

    Ok(())
}
