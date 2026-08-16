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
pub fn copy(text: &str) -> Result<()> {
    if let Some(helper) = copy_with_helper(text) {
        return helper;
    }
    copy_with_osc52(text).context(
        "no clipboard helper program was found (wl-copy, xclip, xsel, pbcopy or clip) \
         and the terminal did not accept the copy escape sequence",
    )
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

/// The first clipboard helper that is actually installed, if any.
///
/// Only used by `msm doctor`: the copy path itself finds this out by trying,
/// which is both cheaper and more honest than asking in advance. A separate
/// check exists because "why did nothing get copied?" is a question worth
/// being able to answer before it is asked.
pub fn available_helper() -> Option<&'static str> {
    helpers().into_iter().find_map(|(program, _)| {
        // `--help` rather than running it for real: this must not touch the
        // clipboard, and every one of these programs accepts it.
        let ran = Command::new(program)
            .arg("--help")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        // The exit status is not checked — `clip.exe` has no `--help` and
        // fails. The question is only whether the program exists at all,
        // which is what a successful spawn answers.
        ran.ok().map(|_| program)
    })
}

/// Try each helper in turn. Returns `None` when none of them is installed, so
/// the caller can fall back to OSC 52; returns `Some(Err(..))` when a helper
/// was found but failed, because that is a real problem worth reporting.
fn copy_with_helper(text: &str) -> Option<Result<()>> {
    for (program, args) in helpers() {
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
