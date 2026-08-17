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

/// What the program was asked to do before the interface opens.
///
/// There are no options that *change* how msm runs — everything it can be
/// told is inside the interface. But two arguments have to work anyway,
/// because the world assumes them: package builders, installers and CI
/// pipelines all call `--version` to find out what they just installed, and
/// somebody who types `--help` deserves an answer rather than a shrug.
#[derive(Debug, PartialEq, Eq)]
enum Invocation {
    /// No arguments: open the interface.
    Interface,
    /// `--version` / `-V`: print the version and stop.
    Version,
    /// `--help` / `-h`: print where everything lives and stop.
    Help,
    /// Something else, which is a mistake worth naming.
    Unknown(String),
}

/// Read the command line, such as it is.
///
/// Only the first argument is looked at: with no options to combine there is
/// nothing a second one could mean, and reporting the first unrecognised
/// thing is more useful than reporting the last.
fn parse_arguments(arguments: &[String]) -> Invocation {
    match arguments.first().map(String::as_str) {
        None => Invocation::Interface,
        Some("--version" | "-V") => Invocation::Version,
        Some("--help" | "-h") => Invocation::Help,
        Some(other) => Invocation::Unknown(other.to_string()),
    }
}

/// `msm --version`, in the one-line `name version` form every other program
/// uses — so `msm --version | cut -d" " -f2` gets a version and nothing else.
fn version_line() -> String {
    format!("msm {}", env!("CARGO_PKG_VERSION"))
}

/// Where everything lives, for `--help` and for anyone who typed something
/// that does not exist.
fn help_text() -> String {
    let mut text = String::new();
    text.push_str(&version_line());
    text.push_str("\n\n");
    text.push_str("Usage: msm\n\n");
    text.push_str(
        "This program has no command-line options beyond --version and --help.\n\
         Everything it can do is inside the interface:\n",
    );
    text.push_str("  alt+1  stream title, category and going live\n");
    text.push_str("  alt+2  both chats\n");
    text.push_str("  alt+3  the combined view\n");
    text.push_str("  alt+4  OBS scenes, audio, streaming and recording\n");
    text.push_str("  alt+5  configuration — layout, appearance, accounts, files\n\n");
    text.push_str("Press space inside it to see every key, or ctrl+p to search them.");
    text
}

#[tokio::main]
async fn main() -> Result<()> {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    match parse_arguments(&arguments) {
        Invocation::Interface => {}
        Invocation::Version => {
            println!("{}", version_line());
            return Ok(());
        }
        Invocation::Help => {
            println!("{}", help_text());
            return Ok(());
        }
        Invocation::Unknown(argument) => {
            // To stderr and with a non-zero status, because this *is* a
            // failure: a script that passed an option msm does not have did
            // not get what it asked for, and exiting 0 would tell it that it
            // did. 2 is the conventional status for a usage error.
            eprintln!("msm: unrecognised argument {argument:?}");
            eprintln!();
            eprintln!("{}", help_text());
            std::process::exit(2);
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn no_arguments_opens_the_interface() {
        assert_eq!(parse_arguments(&args(&[])), Invocation::Interface);
    }

    /// Package builders, installers and the release workflow's own smoke test
    /// all call this. It used to print the version followed by a paragraph
    /// saying the argument had been ignored, which is not an answer.
    #[test]
    fn version_is_a_real_option_in_both_spellings() {
        assert_eq!(parse_arguments(&args(&["--version"])), Invocation::Version);
        assert_eq!(parse_arguments(&args(&["-V"])), Invocation::Version);
    }

    /// One line, `name version`, so the usual shell one-liners work.
    #[test]
    fn the_version_line_is_parseable() {
        let line = version_line();
        let mut parts = line.split(' ');
        assert_eq!(parts.next(), Some("msm"));
        let version = parts.next().expect("a version");
        assert_eq!(version, env!("CARGO_PKG_VERSION"));
        assert_eq!(parts.next(), None, "nothing else on the line");
    }

    #[test]
    fn help_is_a_real_option_in_both_spellings() {
        assert_eq!(parse_arguments(&args(&["--help"])), Invocation::Help);
        assert_eq!(parse_arguments(&args(&["-h"])), Invocation::Help);
    }

    #[test]
    fn anything_else_is_named_rather_than_ignored() {
        assert_eq!(
            parse_arguments(&args(&["--config", "other.toml"])),
            Invocation::Unknown("--config".to_string())
        );
        assert_eq!(
            parse_arguments(&args(&["go"])),
            Invocation::Unknown("go".to_string())
        );
    }

    /// The help has to name the interface, since that is the whole answer to
    /// "how do I use this".
    #[test]
    fn the_help_says_where_everything_is() {
        let help = help_text();
        assert!(help.contains("Usage: msm"));
        assert!(help.contains("alt+1"));
        assert!(help.contains("--version"));
    }
}
