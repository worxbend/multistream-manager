//! Housekeeping: the jobs that are neither streaming nor chatting.
//!
//! Two things accumulate while this program is used. YouTube creates a
//! broadcast every time a stream is set up, and one that never went live sits
//! in YouTube Studio forever as clutter nobody asked for. And with chat
//! logging on, a session leaves a pile of JSON Lines files that hold every
//! paid event of the stream, in a shape a person cannot read.
//!
//! Both of these used to be command-line subcommands. The program has no
//! command line now, so they live here as plain functions the interface can
//! run — which is the better place for them anyway: the moment somebody wants
//! to tidy up abandoned broadcasts is the moment they are looking at a list
//! of them, not a moment they would think to open a terminal for.

use anyhow::{Context, Result};

use crate::config::Config;
use crate::model::{Platform, StaleBroadcast};

/// What a cleanup did.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CleanupReport {
    pub deleted: Vec<String>,
    /// Each failure, as `(broadcast, why)`. One broadcast refusing to be
    /// deleted must not leave the rest of the clutter behind, so these are
    /// collected rather than returned as an error.
    pub failed: Vec<(String, String)>,
}

impl CleanupReport {
    /// A sentence for the activity log.
    pub fn describe(&self) -> String {
        match (self.deleted.len(), self.failed.len()) {
            (0, 0) => "Nothing to clean up: no abandoned broadcasts.".to_string(),
            (deleted, 0) => format!("Deleted {deleted} abandoned broadcast{}.", plural(deleted)),
            (0, failed) => format!(
                "None of the {failed} abandoned broadcast{} could be deleted.",
                plural(failed)
            ),
            (deleted, failed) => format!(
                "Deleted {deleted} abandoned broadcast{}; {failed} refused.",
                plural(deleted)
            ),
        }
    }
}

fn plural(count: usize) -> &'static str {
    if count == 1 {
        ""
    } else {
        "s"
    }
}

/// Find the YouTube broadcasts that were created and never used.
///
/// Anything that has ever received a feed is neither listed nor touched —
/// deleting a stream somebody might still want the recording of would be an
/// unrecoverable mistake made on their behalf.
pub async fn find_stale_broadcasts(config: &Config) -> Result<Vec<StaleBroadcast>> {
    let mut engine = youtube_engine(config).await?;
    engine.list_stale_broadcasts(Platform::YouTube).await
}

/// A YouTube backend, connected and ready.
///
/// Built for one platform on purpose: none of this touches Twitch, and
/// authenticating a platform that has no part in the job would turn a
/// YouTube problem into a Twitch one.
async fn youtube_engine(config: &Config) -> Result<crate::engine::Engine> {
    let (mut engine, mut failures) = crate::engine::Engine::build(config, &[Platform::YouTube])
        .await
        .context("preparing the YouTube connection")?;
    if let Some((_, reason)) = failures.pop() {
        anyhow::bail!("{reason}");
    }
    for (platform, outcome) in engine.connect_all().await {
        if let Err(reason) = outcome {
            anyhow::bail!("{} could not connect: {reason}", platform.label());
        }
    }
    Ok(engine)
}

/// Delete the broadcasts listed, reporting each outcome.
pub async fn delete_broadcasts(config: &Config, ids: &[StaleBroadcast]) -> Result<CleanupReport> {
    let mut engine = youtube_engine(config).await?;

    let mut report = CleanupReport::default();
    for broadcast in ids {
        match engine
            .delete_broadcast(Platform::YouTube, &broadcast.id)
            .await
        {
            Ok(()) => report.deleted.push(broadcast.title.clone()),
            Err(err) => report
                .failed
                .push((broadcast.title.clone(), format!("{err:#}"))),
        }
    }
    Ok(report)
}

/// Write every paid event from the chat logs to a CSV file.
///
/// Returns the path written to and how many rows it holds. The file goes
/// beside the logs it was made from, because that is where somebody will look
/// for it and because it needs no question asked to decide.
pub fn export_superchats(config: &Config) -> Result<(std::path::PathBuf, usize)> {
    let dir = if config.chat.chat_log_dir.is_empty() {
        crate::paths::chat_log_dir()?
    } else {
        std::path::PathBuf::from(&config.chat.chat_log_dir)
    };

    let out = dir.join("superchats.csv");
    let mut file =
        std::fs::File::create(&out).with_context(|| format!("creating {}", out.display()))?;
    let rows = crate::chat::chatlog::export_superchats(&dir, &mut file)?;

    // A full disk that only surfaced when the file closed would truncate the
    // export silently, which is the one failure that would not be noticed.
    use std::io::Write as _;
    file.flush().context("flushing the export file")?;

    Ok((out, rows))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn broadcast(title: &str) -> String {
        title.to_string()
    }

    #[test]
    fn a_cleanup_that_found_nothing_says_so_plainly() {
        assert_eq!(
            CleanupReport::default().describe(),
            "Nothing to clean up: no abandoned broadcasts."
        );
    }

    #[test]
    fn the_report_counts_what_it_deleted() {
        let report = CleanupReport {
            deleted: vec![broadcast("one")],
            failed: Vec::new(),
        };
        assert_eq!(report.describe(), "Deleted 1 abandoned broadcast.");

        let report = CleanupReport {
            deleted: vec![broadcast("one"), broadcast("two")],
            failed: Vec::new(),
        };
        assert_eq!(report.describe(), "Deleted 2 abandoned broadcasts.");
    }

    /// One broadcast refusing to be deleted must not read as total failure,
    /// nor as total success.
    #[test]
    fn a_partial_cleanup_reports_both_halves() {
        let report = CleanupReport {
            deleted: vec![broadcast("one")],
            failed: vec![(broadcast("two"), "no".to_string())],
        };
        let described = report.describe();
        assert!(described.contains('1'), "got {described}");
        assert!(described.contains("refused"), "got {described}");
    }

    #[test]
    fn a_cleanup_that_deleted_nothing_at_all_says_that_rather_than_nothing() {
        let report = CleanupReport {
            deleted: Vec::new(),
            failed: vec![(broadcast("one"), "no".to_string())],
        };
        assert!(report.describe().contains("None of the 1"));
    }
}
