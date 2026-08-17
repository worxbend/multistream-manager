//! The self-check: everything that has to be right before a stream can start.
//!
//! This used to be a command (`msm doctor`) that printed its findings and set
//! an exit status. The program is becoming a single terminal interface with no
//! subcommands, so the same knowledge now lives here as a module: [`run`]
//! gathers the findings and hands them back as plain data, and whoever asked —
//! today a pane in the interface — decides how to draw them. Keeping the
//! checks separate from the drawing also makes them testable, which a function
//! built around `println!` never was.
//!
//! Nothing in here talks to the network, so it is safe to call while the
//! interface is redrawing. The clipboard check does start a local program to
//! see whether it exists, which is quick and is what the old command did too.

// Nothing calls into this module yet: the pane that will display the findings
// is still being built. Without this the compiler reports every item here as
// unused, which under `-D warnings` fails the build for a file that is
// finished and tested. Remove this line once the interface calls `run`.
#![allow(dead_code)]

use crate::config::Config;
use crate::model::Platform;
use crate::{auth, clipboard, paths};

/// How a single check turned out.
///
/// The gap between [`Status::Warning`] and [`Status::Failed`] is the point of
/// having three states rather than two. A fresh install with no logins yet is
/// unfinished, not broken, so it warns. A tool that paints an unfinished setup
/// in the same alarming colour as a real fault teaches people to ignore the
/// alarming colour, and then it cannot tell them anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// Working.
    Ok,
    /// Working, but something is worth knowing.
    Warning,
    /// Genuinely broken. Streaming will not work until this is dealt with.
    Failed,
}

impl Status {
    /// Whether this counts towards "something is broken".
    ///
    /// Only [`Status::Failed`] does, for the reason given on [`Status`].
    pub fn is_failure(self) -> bool {
        matches!(self, Status::Failed)
    }
}

/// One finding: what was looked at, and what to do about it.
#[derive(Debug, Clone)]
pub struct Check {
    pub status: Status,
    /// What was checked, in plain words.
    pub summary: String,
    /// What to do about it. Empty when nothing needs doing.
    pub advice: String,
}

impl Check {
    fn ok(summary: impl Into<String>) -> Self {
        Check {
            status: Status::Ok,
            summary: summary.into(),
            advice: String::new(),
        }
    }

    fn warning(summary: impl Into<String>, advice: impl Into<String>) -> Self {
        Check {
            status: Status::Warning,
            summary: summary.into(),
            advice: advice.into(),
        }
    }

    fn failed(summary: impl Into<String>, advice: impl Into<String>) -> Self {
        Check {
            status: Status::Failed,
            summary: summary.into(),
            advice: advice.into(),
        }
    }
}

/// Run every check and return them in the order they matter.
///
/// The order is the order things matter in: a missing client id makes the
/// login question moot, and a login you do not have makes the stream key
/// question moot. Reporting them in that order means the first failure is
/// usually the only one worth acting on, so a reader can stop there.
///
/// The config is passed in rather than loaded here. When this was a command it
/// read the file itself, because a file with a syntax error in it was one of
/// the things most worth reporting. Inside the interface that cannot happen:
/// the program is already running, so the file has already been read
/// successfully. What is still useful is saying where the file lives and
/// whether it exists at all, which is what the first check does.
pub fn run(config: &Config) -> Vec<Check> {
    let mut checks = Vec::new();

    // Where things live. Someone who wants to edit the file by hand needs the
    // path, and someone who has never run the program needs to be told that
    // there is no file yet rather than left guessing.
    match paths::config_file() {
        Ok(path) => {
            if path.exists() {
                checks.push(Check::ok(format!("config file found: {}", path.display())));
            } else {
                checks.push(Check::warning(
                    format!("no config file at {}", path.display()),
                    "Nothing is wrong yet — the settings you are looking at are the defaults. \
                     Saving anything on the setup screen writes the file.",
                ));
            }
        }
        Err(err) => checks.push(Check::failed(
            "cannot work out where the config file should live",
            format!("{err:#}"),
        )),
    }

    // Credentials, per platform. Missing ones only warn: plenty of people
    // stream to one platform and have no reason to fill in the other.
    for platform in Platform::ALL {
        match config.check_credentials(&[platform]) {
            Ok(()) => checks.push(Check::ok(format!(
                "{} credentials configured",
                platform.label()
            ))),
            Err(_) => checks.push(Check::warning(
                format!("{} has no client id and secret", platform.label()),
                format!(
                    "Fill them in on the setup screen. Skip this if you do not stream to {}.",
                    platform.label()
                ),
            )),
        }
    }

    // Logins. Having none is the normal state of a fresh install, so it warns;
    // a saved-login file that cannot be read is real breakage, so it fails.
    match auth::store::TokenStore::load() {
        Ok(store) => {
            let authorised = store.authorised_platforms();
            if authorised.is_empty() {
                checks.push(Check::warning(
                    "no platform is authorised",
                    "Log in from the login screen — that opens your browser and brings the \
                     authorisation back here.",
                ));
            } else {
                for platform in authorised {
                    checks.push(Check::ok(auth::describe(platform, store.get(platform))));
                }
            }
        }
        Err(err) => checks.push(Check::failed(
            "the saved logins could not be read",
            format!("{err:#}  —  logging out of everything clears them so you can log in again."),
        )),
    }

    // The clipboard, which is how a stream key gets to OBS. This is worth
    // checking because it is the one part of the flow that depends on
    // something outside this program being installed.
    const OSC52_NOTE: &str = "Stream keys will be copied with a terminal escape sequence \
                              instead. That works over ssh, which a helper program cannot, but \
                              some terminals refuse it — if a copy reports success and nothing \
                              pastes, this is why.";
    match clipboard::available_helper() {
        clipboard::HelperStatus::Ready(program) => checks.push(Check::ok(format!(
            "clipboard: {program} will copy the stream key"
        ))),
        clipboard::HelperStatus::NeedsDisplay(program) => checks.push(Check::warning(
            format!("{program} is installed but has no display to talk to"),
            format!(
                "That is normal over ssh. {OSC52_NOTE} Nothing needs installing — the helper is \
                 already there and will work once you are at the machine itself."
            ),
        )),
        clipboard::HelperStatus::Missing => checks.push(Check::warning(
            "no clipboard helper is installed",
            format!("{OSC52_NOTE} Installing wl-copy (Wayland), xclip or xsel (X11) avoids it."),
        )),
    }

    // Desktop notifications, which are how a raid reaches somebody who is
    // looking at OBS rather than at this window. Best-effort by design, which
    // means a machine with no notification tooling is quiet rather than
    // broken — and silently quiet is exactly what needs saying out loud.
    if config.notifications.enabled {
        match crate::notify::available_backend() {
            Some(program) => checks.push(Check::ok(format!(
                "desktop notifications: {program} will show them"
            ))),
            None => checks.push(Check::warning(
                "no desktop notification program was found",
                "Raids, subscriptions and a stopped stream will fall back to the terminal bell. \
                 Installing libnotify (which provides notify-send) fixes it on every desktop; \
                 GLib's gdbus and KDE's kdialog are also used if present.",
            )),
        }
    } else {
        checks.push(Check::ok(
            "desktop notifications are turned off in config.toml",
        ));
    }

    // OBS, which is optional and therefore only worth a note either way. The
    // password is reported as present or absent and never shown: this list is
    // meant to be readable over someone's shoulder or in a screenshot.
    if config.obs.enabled {
        checks.push(Check::ok(format!(
            "OBS: will connect to {} ({})",
            config.obs.url(),
            if config.obs.password().is_some() {
                "with a password"
            } else {
                "no password"
            }
        )));
    } else {
        checks.push(Check::ok("OBS control is turned off in config.toml"));
    }

    // The theme, since a name that does not exist falls back to the default
    // without saying so — which looks like the theme setting being ignored.
    let (_, recognised) = config.appearance.palette();
    if recognised {
        checks.push(Check::ok(format!("theme: {}", config.appearance.theme)));
    } else {
        checks.push(Check::warning(
            format!("unknown theme name {:?}", config.appearance.theme),
            "The default palette is being used instead. The theme picker lists every name.",
        ));
    }

    // Colour support, because the palettes are not much use in eight colours.
    match std::env::var("COLORTERM").ok().as_deref() {
        Some("truecolor") | Some("24bit") => checks.push(Check::ok("terminal: 24-bit colour")),
        _ => checks.push(Check::warning(
            "the terminal does not advertise 24-bit colour",
            "Themes are written as exact colours, so they will be approximated. If your \
             terminal does support it, setting COLORTERM=truecolor tells programs so.",
        )),
    }

    // The log, which is where the answer lives when something fails later.
    match paths::log_file() {
        Ok(path) => checks.push(Check::ok(format!("log file: {}", path.display()))),
        Err(err) => checks.push(Check::warning(
            "the log file location could not be worked out",
            format!("{err:#}"),
        )),
    }

    checks
}

/// A one-line verdict, e.g. "Nothing is broken." or "2 checks failed."
///
/// Warnings are deliberately left out of the count, for the reason given on
/// [`Status`]: an unfinished setup should not read as a broken one.
pub fn verdict(checks: &[Check]) -> String {
    let failures = checks
        .iter()
        .filter(|check| check.status.is_failure())
        .count();

    match failures {
        0 => "Nothing is broken.".to_string(),
        1 => "1 check failed.".to_string(),
        n => format!("{n} checks failed."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(status: Status) -> Check {
        Check {
            status,
            summary: "something".to_string(),
            advice: String::new(),
        }
    }

    #[test]
    fn a_warning_is_not_counted_as_a_failure() {
        assert!(!Status::Ok.is_failure());
        assert!(!Status::Warning.is_failure());
        assert!(Status::Failed.is_failure());
    }

    #[test]
    fn a_list_of_warnings_still_reports_that_nothing_is_broken() {
        let checks = [check(Status::Warning), check(Status::Ok)];
        assert_eq!(verdict(&checks), "Nothing is broken.");
    }

    #[test]
    fn the_verdict_says_nothing_is_broken_when_there_are_no_checks_at_all() {
        assert_eq!(verdict(&[]), "Nothing is broken.");
    }

    #[test]
    fn the_verdict_uses_the_singular_for_exactly_one_failure() {
        let checks = [check(Status::Failed), check(Status::Warning)];
        assert_eq!(verdict(&checks), "1 check failed.");
    }

    #[test]
    fn the_verdict_uses_the_plural_for_more_than_one_failure() {
        let checks = [
            check(Status::Failed),
            check(Status::Failed),
            check(Status::Ok),
        ];
        assert_eq!(verdict(&checks), "2 checks failed.");
    }

    #[test]
    fn running_the_checks_against_a_default_config_produces_findings_without_panicking() {
        let checks = run(&Config::default());
        assert!(!checks.is_empty());
        // Every finding has to be readable on its own, and anything that is
        // not simply fine has to say what to do about it — a warning with no
        // advice leaves the reader stuck.
        for check in &checks {
            assert!(!check.summary.is_empty());
            if check.status != Status::Ok {
                assert!(
                    !check.advice.is_empty(),
                    "no advice for {:?}",
                    check.summary
                );
            }
        }
    }
}
