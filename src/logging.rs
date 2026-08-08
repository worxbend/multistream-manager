//! Logging setup.
//!
//! The terminal UI owns the screen, so nothing can be printed to stdout while it
//! is running — a stray `println!` would punch a hole in the interface. All
//! diagnostics therefore go to a file, which you can watch from another terminal
//! with `tail -f`.

use anyhow::Result;
use tracing_subscriber::EnvFilter;

/// Start logging to `msm.log` in the config directory.
///
/// The verbosity is controlled by the `MSM_LOG` environment variable using the
/// usual `tracing` syntax, for example `MSM_LOG=debug` or
/// `MSM_LOG=multistream_manager::youtube=trace`.
///
/// Returns a guard that must be kept alive for the lifetime of the program;
/// dropping it flushes anything still buffered.
pub fn init() -> Result<tracing_appender::non_blocking::WorkerGuard> {
    let path = crate::paths::log_file()?;

    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;

    let (writer, guard) = tracing_appender::non_blocking(file);

    let filter = EnvFilter::try_from_env("MSM_LOG").unwrap_or_else(|_| EnvFilter::new("info"));

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(writer)
        // ANSI colour codes would be written into the file as escape sequences.
        .with_ansi(false)
        .with_target(true)
        .init();

    tracing::info!(version = env!("CARGO_PKG_VERSION"), "starting");
    Ok(guard)
}
