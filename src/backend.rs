//! The interface every streaming platform implements.
//!
//! The point of this trait is that the rest of the application — the TUI, the
//! orchestration engine, the CLI — never mentions Twitch or YouTube by name. It
//! holds a list of `Box<dyn Backend>` and treats them identically. Adding Kick
//! or Trovo later means writing one new file, not touching the UI.

use anyhow::Result;
use std::future::Future;
use std::pin::Pin;

use crate::model::{
    Category, EndOutcome, GoLiveOutcome, IngestEndpoint, Platform, PlatformStats, StaleBroadcast,
    StreamPlan,
};

/// How long an audience total — followers, subscribers — is trusted before the
/// platform is asked again.
///
/// These numbers move on a scale of hours, while statistics are polled every
/// few seconds, so re-fetching them each time spends API budget (and on YouTube,
/// scarce daily quota) to learn nothing. It lives here rather than in one
/// backend because every platform has the same problem and should answer it the
/// same way.
pub const AUDIENCE_REFRESH: std::time::Duration = std::time::Duration::from_secs(10 * 60);

/// A boxed future, which is how a trait object can have async methods without
/// pulling in the `async-trait` crate.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// One streaming platform.
pub trait Backend: Send {
    /// Verify the credentials work and cache whatever identity information the
    /// platform needs for later calls (the Twitch broadcaster id, for example).
    ///
    /// Called once before `go_live` so that an authentication problem surfaces
    /// as "your Twitch login expired" rather than as a confusing failure halfway
    /// through creating a broadcast.
    fn connect(&mut self) -> BoxFuture<'_, Result<String>>;

    /// Apply the plan: set the title, category, tags and everything else, and
    /// create whatever objects the platform needs in order to accept a feed.
    ///
    /// After this returns successfully the platform is ready — you can hit "Start
    /// Streaming" in OBS and the broadcast will go out.
    fn go_live<'a>(&'a mut self, plan: &'a StreamPlan) -> BoxFuture<'a, Result<GoLiveOutcome>>;

    /// Finish the broadcast this session started.
    ///
    /// The other half of [`Backend::go_live`], and for a long time it was
    /// missing: you could create a YouTube broadcast from here but only
    /// YouTube Studio could close one.
    ///
    /// The default is the honest answer for a platform that has no broadcast
    /// object at all. On Twitch the channel is permanently there; going live
    /// means pointing OBS at it and going offline means stopping OBS, so
    /// there is nothing here to end and saying so is not a failure.
    fn end_stream(&mut self) -> BoxFuture<'_, Result<EndOutcome>> {
        Box::pin(async {
            Ok(EndOutcome::NothingToEnd {
                reason: "this platform has no broadcast to close — the stream ends when OBS \
                         stops sending"
                    .to_string(),
            })
        })
    }

    /// Fetch current statistics. Called on a timer by the dashboard.
    fn fetch_stats(&mut self) -> BoxFuture<'_, Result<PlatformStats>>;

    /// Search the platform's category list. Used to power the autocomplete in
    /// the form. Platforms without a searchable category list return an empty
    /// vector rather than an error.
    fn search_categories<'a>(
        &'a mut self,
        _query: &'a str,
    ) -> BoxFuture<'a, Result<Vec<Category>>> {
        Box::pin(async { Ok(Vec::new()) })
    }

    /// Replace the access token this backend sends on every request.
    ///
    /// Access tokens are short-lived — Google's last about an hour, Twitch's a
    /// few hours — but a streaming session routinely runs longer than that. The
    /// engine therefore renews the token before each batch of work and pushes
    /// the fresh one in here, rather than each backend holding whatever token
    /// happened to be valid when it was built.
    fn set_access_token(&mut self, token: String);

    /// Read the RTMP stream key without changing anything about the channel.
    ///
    /// This backs `msm key`, which exists so that a key can be fetched for
    /// pasting into OBS without the side effect of a full go-live. Platforms
    /// where a key only exists in the context of a specific broadcast — YouTube —
    /// return `None`.
    fn stream_key(&mut self) -> BoxFuture<'_, Result<Option<String>>> {
        Box::pin(async { Ok(None) })
    }

    /// List every RTMP ingest endpoint configured on the account.
    ///
    /// Shown by Config → Housekeeping. On YouTube these are the reusable stream objects,
    /// and the whole reason for showing them is that one of their ids can be
    /// pinned as `stream_id` so the same key is bound every single time.
    ///
    /// Platforms whose key belongs to the channel rather than to a separate
    /// object — Twitch — have nothing to list and return an empty vector, which
    /// is a truthful answer rather than an error.
    fn list_ingest_endpoints(&mut self) -> BoxFuture<'_, Result<Vec<IngestEndpoint>>> {
        Box::pin(async { Ok(Vec::new()) })
    }

    /// List broadcasts that were created but never received a video feed.
    ///
    /// Backs the cleanup job in Config → Housekeeping. Platforms with no broadcast objects —
    /// Twitch, where the channel is permanently there and going live means no
    /// more than pointing OBS at it — return an empty vector, so there is never
    /// anything for the command to offer to delete.
    fn list_stale_broadcasts(&mut self) -> BoxFuture<'_, Result<Vec<StaleBroadcast>>> {
        Box::pin(async { Ok(Vec::new()) })
    }

    /// Delete one broadcast by its platform id.
    ///
    /// Only ever called with an id that [`Backend::list_stale_broadcasts`]
    /// returned a moment earlier, so a platform that lists nothing can never
    /// reach this. The default therefore refuses rather than pretending to have
    /// done something.
    fn delete_broadcast<'a>(&'a mut self, _id: &'a str) -> BoxFuture<'a, Result<()>> {
        Box::pin(async {
            anyhow::bail!(
                "this platform has no broadcasts of its own, so there is nothing to delete"
            )
        })
    }
}

/// How a single platform's go-live attempt turned out.
///
/// Deliberately not a plain `Result`: when you are streaming to two platforms
/// and one fails, you want the other one's URLs and stream key regardless. The
/// engine collects one of these per platform and shows them side by side.
#[derive(Debug, Clone)]
pub struct PlatformResult {
    pub platform: Platform,
    pub outcome: Result<GoLiveOutcome, String>,
}

impl PlatformResult {
    pub fn succeeded(&self) -> bool {
        self.outcome.is_ok()
    }
}
