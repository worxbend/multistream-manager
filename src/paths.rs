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

/// Write a file that only the current user can read.
///
/// On Unix this sets mode `0600` (read/write for the owner, nothing for anyone
/// else) *before* the secret content is written, so there is no window during
/// which the file exists with looser permissions. On other platforms it falls
/// back to a plain write, since Windows ACL handling is a different problem.
pub fn write_secret_file(path: &std::path::Path, contents: &str) -> Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;

        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)
            .with_context(|| format!("opening {} for writing", path.display()))?;
        file.write_all(contents.as_bytes())
            .with_context(|| format!("writing {}", path.display()))?;
        file.sync_all().ok();
    }

    #[cfg(not(unix))]
    {
        std::fs::write(path, contents).with_context(|| format!("writing {}", path.display()))?;
    }

    Ok(())
}
