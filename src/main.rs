//! `msm` — configure and go live on Twitch and YouTube from one terminal.
//!
//! There is deliberately no command line here: no subcommands, no flags, no
//! separate configuration step. Running `msm` opens the interface, and
//! everything the program can do is in it — setting up API credentials,
//! logging in, writing the stream title, going live, reading and answering
//! both chats, switching scenes in OBS, and arranging the screen.
//!
//! That is a decision rather than an omission. A streaming setup is used with
//! one hand while the other is doing something else, and the moment somebody
//! wants to mute a microphone or switch a scene is never a moment they would
//! choose to leave what they are looking at, find a terminal, and remember a
//! subcommand. Everything that was once a subcommand now has a place in the
//! interface, which is also where somebody would look for it.

mod anim;
mod auth;
mod backend;
mod chat;
mod clipboard;
mod config;
mod diagnostics;
mod engine;
mod keys;
mod lang;
mod layout;
mod logging;
mod maintenance;
mod model;
mod notify;
mod obs;
mod paths;
mod telemetry;
mod theme;
mod twitch;
mod ui;
mod youtube;

use anyhow::Result;

use config::Config;

/// What to say to somebody who typed an option.
///
/// There are none, and there is no `--help` to send them to either. Rather
/// than start the interface as though nothing was typed — which would look
/// like the argument had been understood — this says plainly that everything
/// lives inside the interface, and where.
fn explain_no_arguments(arguments: &[String]) {
    println!("msm {}", env!("CARGO_PKG_VERSION"));
    println!();
    println!(
        "This program has no command-line options. {:?} was ignored.",
        arguments[0]
    );
    println!();
    println!("Run `msm` on its own. Everything is inside the interface:");
    println!("  alt+1  stream title, category and going live");
    println!("  alt+2  both chats");
    println!("  alt+3  the combined view");
    println!("  alt+4  OBS scenes, audio, streaming and recording");
    println!("  alt+5  configuration — layout, appearance, accounts, files");
    println!();
    println!("Press space inside it to see every key, or ctrl+p to search them.");
}

#[tokio::main]
async fn main() -> Result<()> {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    if !arguments.is_empty() {
        explain_no_arguments(&arguments);
        return Ok(());
    }

    // Keep the guard alive for the whole run so buffered log lines get
    // flushed. The interface owns the terminal, so nothing can be printed to
    // the screen while it runs and every diagnostic goes to the log file
    // instead; the Files section of the configuration tab says where that is.
    let _log_guard = logging::init().ok();

    // A config file that cannot be read is not a reason to refuse to start.
    // The interface can ask for everything it needs — that is what the setup
    // screen is for — and starting with the defaults and a note in the log
    // beats an error message in a terminal somebody has to go and fix by
    // hand before they can see anything at all.
    let config = match Config::load() {
        Ok(config) => config,
        Err(err) => {
            tracing::warn!(error = %format!("{err:#}"), "could not read the config file");
            Config::default()
        }
    };

    ui::run(config).await
}
