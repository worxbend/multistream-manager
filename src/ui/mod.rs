//! The terminal UI: setup, teardown and the main event loop.

pub mod app;
pub mod chat_tab;
pub mod draw;
pub mod input;
pub mod theme_picker;
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

/// Hand commands to the worker without ever blocking the interface.
///
/// `try_send`, never `send().await`: this loop is the only thing that draws
/// the screen and reads the keyboard. An awaited send on a full channel would
/// park it — no redraw, no Ctrl+C — until the single-threaded worker frees a
/// slot, which behind a stalled network call can take minutes. A full queue
/// already means the worker is far behind, so dropping the command and saying
/// so beats freezing the whole interface.
fn dispatch(
    app: &mut App,
    command_tx: &mpsc::Sender<worker::Command>,
    commands: Vec<worker::Command>,
) {
    use tokio::sync::mpsc::error::TrySendError;
    for command in commands {
        match command_tx.try_send(command) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                app.push_log(
                    worker::LogLevel::Error,
                    "Still busy with earlier requests — that request was ignored, try again in a moment.",
                );
            }
            Err(TrySendError::Closed(_)) => {
                // The worker has gone; nothing more can happen.
                app.should_quit = true;
            }
        }
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
    // The chat tasks' shared event stream. Taken out of the tab state here
    // because only this loop may await on it.
    let mut chat_events = app
        .chat
        .events_rx
        .take()
        .expect("the chat event receiver is taken exactly once, here");
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
            // `next()` yielding `None` means the terminal input stream has
            // ended — the emulator closed, or the program was detached from its
            // tty. Without handling it the `Some(...)` pattern simply stops
            // matching, tokio::select! disables the arm, and the loop spins on
            // its redraw tick forever with no way to type a quit key. Treating
            // end-of-stream like an error and quitting is the only sane exit.
            event = keys.next() => {
                let Some(event) = event else {
                    tracing::info!("terminal input ended; shutting down");
                    app.should_quit = true;
                    continue;
                };
                match event {
                    Ok(TermEvent::Key(key)) => {
                        // Windows reports both press and release; acting on both
                        // would make every keystroke register twice.
                        if key.kind != KeyEventKind::Release {
                            let commands = app.handle_key(key);
                            dispatch(&mut app, &command_tx, commands);
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
                let commands = app.handle_event(event);
                dispatch(&mut app, &command_tx, commands);
            }

            // Messages and state changes from the chat tasks. After the
            // awaited event, drain whatever else is already queued so a busy
            // chat becomes one redraw, not one redraw per message.
            Some((key, event)) = chat_events.recv() => {
                app.handle_chat_event(key, event);
                while let Ok((key, event)) = chat_events.try_recv() {
                    app.handle_chat_event(key, event);
                }
            }

            // Periodic statistics refresh, wherever the numbers are on screen.
            //
            // This used to require a completed go-live. The dashboard now
            // opens as soon as a platform connects — showing whether the
            // channel is already live and who is watching — and the combined
            // tab shows the same numbers beside chat, so both need polling.
            _ = ticker.tick() => {
                let showing_stats = app.screen == Screen::Dashboard
                    || app.tab == app::Tab::Combined;
                if showing_stats && !app.accounts.is_empty() {
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

    // The chat tasks end the same way: dropping each handle's command sender
    // closes the channel their loop selects on. They must then be *awaited* —
    // an accepted send or a confirmed moderation call may still be in flight,
    // and returning from main would kill it mid-request after the composer
    // already reported it sent.
    let chat_tasks: Vec<_> = app
        .chat
        .open
        .drain()
        .map(|(_, chat)| {
            drop(chat.handle.commands);
            chat.handle.task
        })
        .collect();

    drop(guard);

    // Give everything a shared moment to wind down, but do not hang on it: a
    // request already in flight to YouTube could take several seconds, and
    // the user has asked to quit.
    let shutdown = async move {
        let _ = worker.await;
        for task in chat_tasks {
            let _ = task.await;
        }
    };
    let _ = tokio::time::timeout(std::time::Duration::from_millis(500), shutdown).await;

    Ok(())
}
