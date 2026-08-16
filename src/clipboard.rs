//! Putting text on the system clipboard from inside a terminal program.
//!
//! This exists so a stream key can be handed to OBS without ever being drawn
//! on screen. The window running this program is very often part of the
//! broadcast itself, and a stream key is enough for a stranger to take over
//! your channel — so it goes straight from the API to the clipboard, and
//! nothing in between prints it, logs it, or renders it.
//!
//! Two routes are tried, in this order:
//!
//! 1. **A clipboard helper program.** On Linux that is `wl-copy` (Wayland) or
//!    `xclip`/`xsel` (X11), on macOS `pbcopy`, on Windows `clip`. This is the
//!    reliable route when the program runs on the same machine as the desktop.
//! 2. **OSC 52.** An escape sequence that asks the *terminal emulator* to set
//!    its clipboard. This is the only route that works over SSH, because the
//!    terminal doing the pasting is the one on your own desk rather than the
//!    machine the program runs on. Many terminals support it; some disable it
//!    by default, which is why it is the fallback rather than the first try.
//!
//! No clipboard crate is used deliberately: the usual ones pull in X11 and
//! Wayland client libraries, which would make a program that is otherwise
//! pure Rust and headless-friendly depend on a desktop being installed.

use anyhow::{Context, Result};
use std::io::Write;
use std::process::{Command, Stdio};

/// Copy `text` to the system clipboard.
///
/// The text is never logged or echoed anywhere, including in error messages.
///
/// A helper program that fails is not the end of the attempt: the escape
/// sequence is tried afterwards regardless. This matters most over ssh, which
/// is where the two routes disagree — the remote machine may well have
/// `xclip` installed with no display for it to talk to, and giving up there
/// would fail in exactly the situation the escape sequence exists to handle.
/// Only when both routes have failed is anything reported, and then the
/// reason from each is kept, because "it did not work" without saying which
/// half broke is not a report anybody can act on.
pub fn copy(text: &str) -> Result<()> {
    let helper_failure = match copy_with_helper(text) {
        Some(Ok(())) => return Ok(()),
        Some(Err(err)) => Some(err),
        None => None,
    };

    let osc52 = copy_with_osc52(text);
    match (osc52, helper_failure) {
        (Ok(()), _) => Ok(()),
        (Err(osc_err), Some(helper_err)) => Err(osc_err.context(format!(
            "the clipboard helper failed first ({helper_err:#}), then the terminal did not \
             accept the copy escape sequence"
        ))),
        (Err(osc_err), None) => Err(osc_err.context(
            "no clipboard helper program was usable (wl-copy, xclip, xsel, pbcopy or clip) \
             and the terminal did not accept the copy escape sequence",
        )),
    }
}

/// The helper programs worth trying, each with the arguments that make it read
/// the clipboard content from standard input.
fn helpers() -> Vec<(&'static str, Vec<&'static str>)> {
    vec![
        ("wl-copy", vec![]),
        ("xclip", vec!["-selection", "clipboard"]),
        ("xsel", vec!["--clipboard", "--input"]),
        ("pbcopy", vec![]),
        ("clip.exe", vec![]),
        ("clip", vec![]),
    ]
}

/// Which clipboard helper is available, for `msm doctor`.
///
/// The copy path finds this out by trying rather than by asking in advance,
/// which is both cheaper and more honest. This exists because "why did
/// nothing get copied?" is a question worth being able to answer before it is
/// asked.
pub fn available_helper() -> HelperStatus {
    let installed: Vec<&'static str> = helpers()
        .into_iter()
        .map(|(program, _)| program)
        .filter(|program| is_installed(program))
        .collect();

    match installed
        .iter()
        .find(|program| has_the_display_it_needs(program))
    {
        Some(program) => HelperStatus::Ready(program),
        None => match installed.first() {
            Some(program) => HelperStatus::NeedsDisplay(program),
            None => HelperStatus::Missing,
        },
    }
}

/// What `msm doctor` found when it looked for a clipboard helper.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HelperStatus {
    /// Installed and able to run.
    Ready(&'static str),
    /// Installed, but the display server it talks to is not there — which is
    /// the normal situation over ssh. Worth distinguishing from missing,
    /// because "install xclip" is useless advice to someone who already has
    /// it.
    NeedsDisplay(&'static str),
    /// Nothing suitable is installed.
    Missing,
}

/// Whether a program exists and can be run.
fn is_installed(program: &str) -> bool {
    // `--help` rather than running it for real: this must not touch the
    // clipboard. The exit status is not checked — `clip.exe` has no `--help`
    // and fails — because the question is only whether the program exists at
    // all, which is what a successful spawn answers.
    Command::new(program)
        .arg("--help")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok()
}

/// Whether the display server a helper needs is actually there.
///
/// Being installed is not the same as being able to work. `xclip` is present
/// on plenty of machines that are reached over ssh with no X display, and
/// `wl-copy` on plenty with no Wayland session — in both cases it exits with
/// an error the moment it is run. Checking this is what stops `msm doctor`
/// reporting that the clipboard is fine on a machine where copying a stream
/// key will silently fall through to the escape-sequence route instead.
fn has_the_display_it_needs(program: &str) -> bool {
    let set = |name: &str| std::env::var_os(name).is_some_and(|value| !value.is_empty());
    match program {
        "wl-copy" => set("WAYLAND_DISPLAY"),
        "xclip" | "xsel" => set("DISPLAY"),
        // `pbcopy` talks to the macOS pasteboard and `clip` to Windows;
        // neither needs anything in the environment.
        _ => true,
    }
}

/// Try each helper in turn. Returns `None` when none of them is installed, so
/// the caller can fall back to OSC 52; returns `Some(Err(..))` when a helper
/// was found but failed, because that is a real problem worth reporting.
fn copy_with_helper(text: &str) -> Option<Result<()>> {
    for (program, args) in helpers() {
        // Skip a helper whose display server is not there. It would spawn
        // happily and then exit with an error, and treating that as "the
        // clipboard is broken" would hide the escape-sequence route that
        // actually works in this situation.
        if !has_the_display_it_needs(program) {
            continue;
        }
        let spawned = Command::new(program)
            .args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();

        let mut child = match spawned {
            Ok(child) => child,
            // "Not installed" is the expected outcome for most of this list, so
            // it moves on to the next candidate rather than reporting anything.
            Err(_) => continue,
        };

        let write = child
            .stdin
            .as_mut()
            .context("the clipboard helper did not accept input")
            .and_then(|stdin| {
                stdin
                    .write_all(text.as_bytes())
                    .context("writing to the clipboard helper")
            });
        if let Err(err) = write {
            return Some(Err(err));
        }
        // Dropping stdin closes the pipe, which is how the helper learns that
        // the content is complete; wl-copy in particular waits for it.
        drop(child.stdin.take());

        return Some(match child.wait() {
            Ok(status) if status.success() => Ok(()),
            Ok(status) => Err(anyhow::anyhow!("{program} exited with {status}")),
            Err(err) => Err(err).with_context(|| format!("waiting for {program}")),
        });
    }
    None
}

/// Ask the terminal emulator itself to take the text, via the OSC 52 sequence
/// `ESC ] 52 ; c ; <base64> BEL`.
///
/// It is written to `/dev/tty` rather than to standard output because standard
/// output belongs to the drawing code: mixing an escape sequence into the
/// middle of a frame would corrupt it.
fn copy_with_osc52(text: &str) -> Result<()> {
    let payload = base64(text.as_bytes());
    let sequence = format!("\x1b]52;c;{payload}\x07");

    // On Unix the sequence goes to the terminal device directly. Standard
    // output belongs to the drawing code, and an escape sequence injected into
    // the middle of a frame would corrupt it. Windows has no `/dev/tty`, so
    // there the sequence goes to standard output.
    #[cfg(unix)]
    let mut sink: Box<dyn Write> = Box::new(
        std::fs::OpenOptions::new()
            .write(true)
            .open("/dev/tty")
            .context("opening the terminal to send the copy sequence")?,
    );
    #[cfg(not(unix))]
    let mut sink: Box<dyn Write> = Box::new(std::io::stdout());

    sink.write_all(sequence.as_bytes())
        .context("sending the copy sequence to the terminal")?;
    sink.flush().context("flushing the copy sequence")?;
    Ok(())
}

/// Standard base64, written out here because it is a dozen lines and the
/// program has no other use for an encoding dependency.
fn base64(input: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);

    for chunk in input.chunks(3) {
        // Pack the (up to) three bytes into one 24-bit number, then read it
        // back out as four 6-bit groups.
        let mut packed = 0u32;
        for (index, byte) in chunk.iter().enumerate() {
            packed |= (*byte as u32) << (16 - 8 * index);
        }
        for group in 0..4 {
            if group <= chunk.len() {
                let value = (packed >> (18 - 6 * group)) & 0b11_1111;
                out.push(ALPHABET[value as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A helper that needs a display server has to be judged on whether that
    /// display server is there, not merely on whether the program exists —
    /// otherwise `msm doctor` reports a working clipboard on a machine where
    /// copying will quietly fall back to the escape sequence.
    /// Guards the two display variables, which are process-wide state that
    /// more than one test here reads and one of them changes.
    static DISPLAY_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn a_helper_that_needs_a_display_is_only_usable_when_one_is_set() {
        let _guard = DISPLAY_LOCK.lock().unwrap_or_else(|err| err.into_inner());
        // Whatever this machine has, the rule has to be consistent with it.
        let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some_and(|v| !v.is_empty());
        let x11 = std::env::var_os("DISPLAY").is_some_and(|v| !v.is_empty());
        assert_eq!(has_the_display_it_needs("wl-copy"), wayland);
        assert_eq!(has_the_display_it_needs("xclip"), x11);
        assert_eq!(has_the_display_it_needs("xsel"), x11);
        // These two answer to the operating system rather than to a display
        // server, so nothing in the environment can rule them out.
        assert!(has_the_display_it_needs("pbcopy"));
        assert!(has_the_display_it_needs("clip.exe"));
    }

    /// Over ssh the remote machine often has a helper installed with no
    /// display for it to talk to. Copying has to reach the escape sequence in
    /// that case rather than reporting the helper's failure, because the
    /// escape sequence is the route that works there — it is answered by the
    /// terminal on the user's own desk.
    #[test]
    fn a_helper_with_no_display_is_skipped_rather_than_failing_the_copy() {
        // With neither display variable set, none of the display-dependent
        // helpers may be attempted at all.
        temporarily_without_display(|| {
            for program in ["wl-copy", "xclip", "xsel"] {
                assert!(
                    !has_the_display_it_needs(program),
                    "{program} would be attempted with no display to talk to"
                );
            }
        });
    }

    /// Run `body` with both display variables unset, then put them back.
    ///
    /// The environment is process-wide and these tests can run in parallel,
    /// so this takes a lock rather than trusting that nothing else is looking
    /// at the same two variables at the same moment.
    fn temporarily_without_display(body: impl FnOnce()) {
        let _guard = DISPLAY_LOCK.lock().unwrap_or_else(|err| err.into_inner());

        let wayland = std::env::var_os("WAYLAND_DISPLAY");
        let x11 = std::env::var_os("DISPLAY");
        // SAFETY: the lock above makes this the only thread touching these
        // two variables, and both are restored before it is released.
        unsafe {
            std::env::remove_var("WAYLAND_DISPLAY");
            std::env::remove_var("DISPLAY");
        }

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(body));

        unsafe {
            if let Some(value) = wayland {
                std::env::set_var("WAYLAND_DISPLAY", value);
            }
            if let Some(value) = x11 {
                std::env::set_var("DISPLAY", value);
            }
        }
        if let Err(panic) = result {
            std::panic::resume_unwind(panic);
        }
    }

    #[test]
    fn base64_matches_the_standard_including_padding() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foob"), "Zm9vYg==");
        assert_eq!(base64(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    /// Stream keys contain characters that must survive the round trip, and a
    /// non-ASCII byte must not be mangled either.
    #[test]
    fn base64_handles_key_shaped_input() {
        assert_eq!(base64("live_123456_AbCdEf-gh".as_bytes()).len() % 4, 0);
        assert_eq!(base64("é".as_bytes()), "w6k=");
    }
}
