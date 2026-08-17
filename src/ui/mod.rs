//! The terminal UI: setup, teardown and the main event loop.

pub mod app;
pub mod chat_tab;
pub mod command_palette;
pub mod config_tab;
pub mod draw;
pub mod input;
pub mod mouse;
pub mod obs_tab;
pub mod splash;
pub mod theme_picker;
pub mod toast;
pub mod which_key;
pub mod worker;

use anyhow::{Context, Result};
use crossterm::event::{
    DisableMouseCapture, EnableMouseCapture, Event as TermEvent, EventStream, KeyEventKind,
};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use std::io::{IsTerminal as _, Stdout};
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
    /// Whether the terminal's own background colour was overridden, so `drop`
    /// knows whether it has to be given back.
    background_overridden: bool,
    /// Whether mouse reporting was switched on, so `drop` knows whether it
    /// has to be switched off again.
    mouse: bool,
}

impl TerminalGuard {
    fn new(mouse: bool) -> Result<Self> {
        // Checked before anything is switched on, because the error you get
        // by simply trying is "No such device or address (os error 6)",
        // which is true, unhelpful, and the first thing anybody sees when
        // they pipe msm somewhere or run it from cron. This program is an
        // interface and nothing else, so there is no meaningful non-terminal
        // mode to fall back to — only a better sentence.
        if !std::io::stdout().is_terminal() || !std::io::stdin().is_terminal() {
            anyhow::bail!(
                "msm is an interactive program and needs a terminal.\n\
                 It looks like its input or output is a pipe, a file or a service \
                 manager's log rather than a terminal window.\n\
                 There is nothing to run non-interactively: msm has no subcommands \
                 and no batch mode. Run it in a terminal, or under a multiplexer \
                 such as tmux or screen if you want it to outlive the session."
            );
        }
        enable_raw_mode().context("switching the terminal into raw mode")?;
        let mut stdout = std::io::stdout();
        execute!(stdout, EnterAlternateScreen).context("opening the alternate screen")?;
        if mouse {
            // Cell-motion reporting: the terminal sends clicks and wheel
            // events. It also stops the terminal handling drag-selection
            // itself, which is why this is a setting rather than always on.
            execute!(stdout, EnableMouseCapture).context("enabling mouse reporting")?;
        }
        let terminal = Terminal::new(CrosstermBackend::new(stdout))
            .context("initialising the terminal backend")?;
        Ok(Self {
            terminal,
            mouse,
            background_overridden: false,
        })
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        // Every step is best-effort: if restoring fails there is nothing useful
        // left to do about it, and returning an error from `drop` is impossible.
        let _ = disable_raw_mode();
        if self.mouse {
            let _ = execute!(self.terminal.backend_mut(), DisableMouseCapture);
        }
        if self.background_overridden {
            // Give the terminal its own background back. Without this the
            // theme's colour would be left behind in the shell this was
            // started from, which is not this program's window to repaint.
            use std::io::Write as _;
            let mut stdout = std::io::stdout();
            let _ = stdout.write_all(crate::theme::RESET_BACKGROUND_SEQUENCE.as_bytes());
            let _ = stdout.flush();
        }
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

    let mut guard = TerminalGuard::new(config.appearance.mouse)?;
    let mut app = App::new(config);
    // Start the OBS connection from inside the runtime, which is where
    // spawning a task is possible.
    app.connect_obs();
    let mut obs_updates = app.obs_updates.take();
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

    // The animation clock. Every moving thing on screen is a function of how
    // long the run has been going (see `crate::anim`), so making them move is
    // simply a matter of redrawing often enough — one clock for all of them
    // rather than a timer per effect.
    //
    // It only runs when there is actually something animating. With
    // `animations = "off"`, or once the splash has gone and nothing else is
    // moving, this stays parked and the interface goes back to redrawing
    // twice a second.
    let mut frames = tokio::time::interval(crate::anim::FRAME_INTERVAL);
    frames.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    // How often the process telemetry is re-read. Once a second: these
    // numbers are for noticing a trend, and a figure that jumps ten times a
    // second is harder to read than one that does not.
    let mut telemetry = tokio::time::interval(std::time::Duration::from_secs(1));
    telemetry.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    // The terminal background currently in effect, so it is only rewritten
    // when it actually changes — which is at start-up, and again each time
    // the selection moves in the theme picker.
    let mut applied_background: Option<String> = None;

    loop {
        if app.config.appearance.terminal_background {
            let wanted = app.palette.canvas();
            if applied_background.as_deref() != Some(wanted.as_str()) {
                use std::io::Write as _;
                let mut stdout = std::io::stdout();
                let _ = stdout.write_all(crate::theme::background_sequence(&wanted).as_bytes());
                let _ = stdout.flush();
                guard.background_overridden = true;
                applied_background = Some(wanted);
            }
        }

        guard
            .terminal
            .draw(|frame| draw::draw(frame, &app))
            .context("drawing the interface")?;
        // Counted after the frame has actually been drawn, so the frame rate
        // reports what the terminal received rather than how often the loop
        // went round.
        app.telemetry.record_frame(std::time::Instant::now());

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
                    Ok(TermEvent::Mouse(event)) => {
                        let area = guard.terminal.size().map(|size| ratatui::layout::Rect {
                            x: 0,
                            y: 0,
                            width: size.width,
                            height: size.height,
                        });
                        if let Ok(area) = area {
                            let commands = app.handle_mouse(event, area);
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

            // Updates from the OBS connection. `obs_updates` is `None` when
            // OBS control is turned off, and a `select!` branch whose future
            // is never ready simply never fires — so this costs nothing at
            // all in that case.
            Some(update) = async {
                match obs_updates.as_mut() {
                    Some(updates) => updates.recv().await,
                    None => std::future::pending().await,
                }
            } => {
                app.handle_obs_update(update);
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

            // The animation clock, ticking only while something is moving.
            // It also drives notification expiry: a pop-up whose time is up
            // has to come off the screen whether or not a key is pressed.
            _ = frames.tick(), if app.is_animating() => {
                app.toasts.expire(std::time::Instant::now());
            }

            // Re-read the process telemetry, but only while it is on screen:
            // reading `/proc` once a second for a figure nobody is looking at
            // is exactly the sort of cost this feature exists to expose.
            _ = telemetry.tick(), if app.config.appearance.telemetry => {
                app.telemetry.sample(std::time::Instant::now());
            }

            // Plain redraw tick, so counters stay current. It also paces
            // desktop notifications: a burst of stream events is queued
            // rather than dropped, and this is what releases the queue one
            // pop-up at a time.
            _ = redraw.tick() => {
                app.desktop.flush();
            }
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
    // The OBS connection winds down the same way: dropping its command
    // sender ends its loop. It is awaited alongside the chat tasks so a
    // command already in flight — a scene change, a mute — is not killed
    // half-sent.
    let obs_task = app.obs_handle.take().map(|handle| {
        drop(handle.commands);
        handle.task
    });

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
        if let Some(task) = obs_task {
            let _ = task.await;
        }
        for task in chat_tasks {
            let _ = task.await;
        }
    };
    let _ = tokio::time::timeout(std::time::Duration::from_millis(500), shutdown).await;

    Ok(())
}
