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

    let file = open_log_file(&path)?;

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

/// Open the log file for appending, with owner-only permissions.
///
/// Split out of [`init`] so the permission handling can be tested without
/// installing a global tracing subscriber.
fn open_log_file(path: &std::path::Path) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.create(true).append(true);

    // The log is owner-only, like `tokens.json`, because it can end up holding
    // secrets. When a token endpoint answers with something that is not valid
    // JSON, the parse error carries the whole response body as context, and that
    // error is logged by the background token refresh — so on a bad day the
    // access and refresh tokens themselves are in here. A world-readable file
    // would hand them to every other account on the machine.
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }

    let file = options.open(path)?;

    // `mode` above only applies when the file is created. A log left behind by
    // an older version is already world-readable, so tighten it as well.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }

    Ok(file)
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    fn mode_of(path: &std::path::Path) -> u32 {
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    /// `init` itself installs a global subscriber and so cannot be called twice
    /// in one test binary. What is tested here is the file-opening behaviour it
    /// performs, on a scratch path.
    #[test]
    fn the_log_file_is_owner_only_whether_it_is_new_or_left_over() {
        let dir = std::env::temp_dir().join(format!("msm-log-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("msm.log");
        let _ = std::fs::remove_file(&path);

        super::open_log_file(&path).unwrap();
        assert_eq!(
            mode_of(&path),
            0o600,
            "a newly created log must be owner-only"
        );

        // A log written by an older version is world-readable; reopening must
        // tighten it rather than leaving the old permissions in place.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        super::open_log_file(&path).unwrap();
        assert_eq!(mode_of(&path), 0o600, "an existing log must be tightened");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
