//! Where the application keeps its files on disk.
//!
//! Everything lives under the standard per-user config directory, which on Linux
//! is `~/.config/multistream-manager` (or wherever `$XDG_CONFIG_HOME` points),
//! on macOS `~/Library/Application Support/…`, and on Windows `%APPDATA%\…`.

use anyhow::{Context, Result};
use directories::ProjectDirs;
use std::path::PathBuf;

/// The reverse-DNS-ish triple the `directories` crate uses to build the path.
const QUALIFIER: &str = "";
const ORGANISATION: &str = "";
const APPLICATION: &str = "multistream-manager";

/// The directory holding config, tokens, cache and logs. Created if missing.
pub fn config_dir() -> Result<PathBuf> {
    // Honour an explicit override first — useful for tests and for anyone who
    // wants to keep the whole thing inside a dotfiles repo.
    if let Some(custom) = std::env::var_os("MSM_CONFIG_DIR") {
        let dir = PathBuf::from(custom);
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("creating config directory {}", dir.display()))?;
        return Ok(dir);
    }

    let dirs = ProjectDirs::from(QUALIFIER, ORGANISATION, APPLICATION).context(
        "could not work out where your home directory is, so there is nowhere to store config",
    )?;
    let dir = dirs.config_dir().to_path_buf();
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("creating config directory {}", dir.display()))?;
    Ok(dir)
}

/// `config.toml` — API credentials and your saved default stream settings.
pub fn config_file() -> Result<PathBuf> {
    Ok(config_dir()?.join("config.toml"))
}

/// `tokens.json` — OAuth access and refresh tokens. Written with owner-only
/// permissions because a refresh token is as good as a password.
pub fn token_file() -> Result<PathBuf> {
    Ok(config_dir()?.join("tokens.json"))
}

/// `msm.log` — the log file. The terminal UI owns stdout, so any diagnostics
/// have to go to a file instead of being printed.
pub fn log_file() -> Result<PathBuf> {
    Ok(config_dir()?.join("msm.log"))
}

/// Write a file that only the current user can read, atomically.
///
/// Two properties matter here and this function provides both:
///
/// 1. **Owner-only permissions from the first instant.** On Unix the file is
///    created with mode `0600` (read/write for the owner, nothing for anyone
///    else) *before* the secret content is written, so there is no window
///    during which the content exists with looser permissions. On other
///    platforms it falls back to default permissions, since Windows ACL
///    handling is a different problem.
///
/// 2. **All-or-nothing replacement.** The content is written to a temporary
///    file in the same directory and then renamed over the target. A rename
///    within one filesystem is atomic, so a crash or power cut mid-write
///    leaves either the complete old file or the complete new file — never a
///    truncated half of one, which for `tokens.json` would mean being logged
///    out of everything.
pub fn write_secret_file(path: &std::path::Path, contents: &str) -> Result<()> {
    use std::io::Write;

    // The temporary file must live in the same directory as the target,
    // because rename is only atomic within a single filesystem.
    //
    // Its name has to be unique per *writer*, not per process: two threads in
    // one process sharing a temp path is the same collision as two processes
    // sharing one. One writer's rename moves the file out from under the other,
    // which then fails mid-write, and the failure path's cleanup can delete a
    // temp file belonging to someone else. So the name carries the process id
    // and a counter that no two calls anywhere in this program can share.
    static WRITE_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let ticket = WRITE_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .with_context(|| format!("{} has no file name to write to", path.display()))?;
    let tmp_path = path.with_file_name(format!(".{file_name}.tmp-{}-{ticket}", std::process::id()));

    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }

    let result = (|| {
        let mut file = options
            .open(&tmp_path)
            .with_context(|| format!("opening {} for writing", tmp_path.display()))?;
        file.write_all(contents.as_bytes())
            .with_context(|| format!("writing {}", tmp_path.display()))?;
        // Force the bytes to disk before the rename makes them the real file,
        // otherwise the rename could survive a crash that the content did not.
        file.sync_all()
            .with_context(|| format!("flushing {} to disk", tmp_path.display()))?;
        drop(file);
        std::fs::rename(&tmp_path, path).with_context(|| {
            format!(
                "moving {} into place at {}",
                tmp_path.display(),
                path.display()
            )
        })
    })();

    if result.is_err() {
        // Best-effort cleanup so a failed write does not leave a stray secret
        // file behind; the temp file already has owner-only permissions.
        let _ = std::fs::remove_file(&tmp_path);
    }
    result
}

/// Test helpers for pointing the whole program at a scratch config directory.
#[cfg(test)]
pub mod test_support {
    use std::path::PathBuf;
    use std::sync::{Mutex, MutexGuard, OnceLock};

    /// `MSM_CONFIG_DIR` is process-wide state, and tests run in parallel inside
    /// one process, so tests that redirect it take this lock for their duration.
    fn lock() -> MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// An empty config directory that the program will use until this guard is
    /// dropped, at which point the directory is removed.
    pub struct ScratchConfigDir {
        path: PathBuf,
        _guard: MutexGuard<'static, ()>,
    }

    impl ScratchConfigDir {
        pub fn new(test_name: &str) -> Self {
            let guard = lock();
            let path = std::env::temp_dir().join(format!("msm-scratch-{test_name}"));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).expect("creating a scratch config directory");
            std::env::set_var("MSM_CONFIG_DIR", &path);
            Self {
                path,
                _guard: guard,
            }
        }

        pub fn path(&self) -> &std::path::Path {
            &self.path
        }
    }

    impl Drop for ScratchConfigDir {
        fn drop(&mut self) {
            std::env::remove_var("MSM_CONFIG_DIR");
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch directory that removes itself when the test ends.
    struct ScratchDir(PathBuf);

    impl ScratchDir {
        fn new(test_name: &str) -> Self {
            let dir = std::env::temp_dir()
                .join(format!("msm-paths-test-{}-{test_name}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for ScratchDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn write_secret_file_writes_the_content_and_cleans_up_its_temp_file() {
        let scratch = ScratchDir::new("content");
        let target = scratch.0.join("tokens.json");

        write_secret_file(&target, "first").unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "first");

        // Replacing an existing file must also work (the common case: every
        // token refresh rewrites the store).
        write_secret_file(&target, "second").unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "second");

        // The temporary file used for the atomic rename must not be left
        // behind — the target should be the only file in the directory.
        let entries: Vec<_> = std::fs::read_dir(&scratch.0).unwrap().collect();
        assert_eq!(entries.len(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn write_secret_file_is_owner_only_on_unix() {
        use std::os::unix::fs::PermissionsExt;

        let scratch = ScratchDir::new("mode");
        let target = scratch.0.join("tokens.json");
        write_secret_file(&target, "secret").unwrap();

        let mode = std::fs::metadata(&target).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn write_secret_file_rejects_a_path_with_no_file_name() {
        let err = write_secret_file(std::path::Path::new("/"), "x").unwrap_err();
        assert!(err.to_string().contains("no file name"));
    }

    /// The temp file used to be named after the process id alone, so every
    /// thread in one process shared it: one thread's rename pulled the file out
    /// from under another, and roughly half of all writes failed. Sequential
    /// token refreshes hide this today, but a second concurrent saver would
    /// surface it as a spurious "could not renew your access token".
    #[test]
    fn concurrent_writers_in_one_process_do_not_clobber_each_other() {
        let dir = ScratchDir::new("concurrent-writers");
        let target = dir.0.join("tokens.json");

        std::thread::scope(|scope| {
            for writer in 0..4 {
                let target = target.clone();
                scope.spawn(move || {
                    for round in 0..100 {
                        write_secret_file(&target, &format!("writer {writer} round {round}"))
                            .expect("every write must succeed");
                    }
                });
            }
        });

        // Whichever write landed last, the file is one complete value — never a
        // torn mixture — and no temp files were left behind.
        let content = std::fs::read_to_string(&target).unwrap();
        assert!(content.starts_with("writer "), "torn content: {content:?}");

        let leftovers: Vec<_> = std::fs::read_dir(&dir.0)
            .unwrap()
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.contains(".tmp-"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "temp files left behind: {leftovers:?}"
        );
    }
}
