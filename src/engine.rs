//! Orchestration: turning one plan into calls against every selected platform.
//!
//! This is the layer that owns the backends and drives them. It exists so that
//! the terminal UI never has to know that Twitch takes one API call and YouTube
//! takes four, or that one of them might fail while the other succeeds.
//!
//! The important behaviour here is **partial success**. If you are going live on
//! both platforms and YouTube is out of quota, Twitch should still be configured
//! and you should still get its URL and stream key. Nothing is rolled back, and
//! nothing is hidden — you get one result per platform and the UI shows both.

use anyhow::{Context, Result};
use std::collections::HashMap;

use crate::auth;
use crate::backend::{Backend, PlatformResult};
use crate::config::Config;
use crate::model::{Category, Platform, PlatformStats, StreamPlan};
use crate::twitch::TwitchBackend;
use crate::youtube::YouTubeBackend;

/// Holds one live backend per selected platform.
pub struct Engine {
    /// Kept so tokens can be renewed later. A streaming session outlives an
    /// access token, so the engine cannot rely on the ones it started with.
    config: Config,
    backends: HashMap<Platform, Box<dyn Backend>>,
}

impl Engine {
    /// Build backends for the given platforms, authenticating each one.
    ///
    /// Tokens are refreshed here if they have expired, so by the time this
    /// returns every backend holds a working access token.
    pub async fn build(config: &Config, platforms: &[Platform]) -> Result<Self> {
        config.check_credentials(platforms)?;

        // One HTTP client shared by every backend, so connections are pooled
        // rather than re-established for each call.
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .user_agent(concat!("multistream-manager/", env!("CARGO_PKG_VERSION")))
            .build()
            .context("building the HTTP client")?;

        let mut backends: HashMap<Platform, Box<dyn Backend>> = HashMap::new();

        for &platform in platforms {
            let token = auth::access_token(config, platform).await?;
            let backend: Box<dyn Backend> = match platform {
                Platform::Twitch => Box::new(TwitchBackend::new(
                    http.clone(),
                    config.twitch.client_id.clone(),
                    token,
                )),
                Platform::YouTube => Box::new(YouTubeBackend::new(
                    http.clone(),
                    token,
                    config.youtube.reuse_stream,
                    config.youtube.stream_id.clone(),
                )),
            };
            backends.insert(platform, backend);
        }

        Ok(Self {
            config: config.clone(),
            backends,
        })
    }

    /// Renew every backend's access token if it is close to expiring.
    ///
    /// Called before each batch of API work. `auth::access_token` is a no-op
    /// when the current token still has life left in it, so this costs nothing
    /// in the common case; when a token has aged out it silently exchanges the
    /// refresh token for a new one.
    ///
    /// Without this, a session longer than an hour would see every statistics
    /// poll fail with a 401 and the dashboard would freeze on stale numbers —
    /// which is precisely the long-running case this application exists for.
    async fn refresh_tokens(&mut self) {
        for platform in self.platforms() {
            match auth::access_token(&self.config, platform).await {
                Ok(token) => {
                    if let Some(backend) = self.backends.get_mut(&platform) {
                        backend.set_access_token(token);
                    }
                }
                Err(err) => {
                    // Not fatal here: the existing token may still work, and the
                    // request that follows will report a far more specific error
                    // than this would.
                    tracing::warn!(
                        platform = platform.slug(),
                        error = %format!("{err:#}"),
                        "could not renew the access token"
                    );
                }
            }
        }
    }

    /// Which platforms this engine is managing, in a stable display order.
    pub fn platforms(&self) -> Vec<Platform> {
        Platform::ALL
            .into_iter()
            .filter(|p| self.backends.contains_key(p))
            .collect()
    }

    /// Verify every backend's credentials and return the account name each one
    /// resolved to, so the UI can show "logged in as …" before anything is sent.
    pub async fn connect_all(&mut self) -> Vec<(Platform, Result<String, String>)> {
        let mut results = Vec::new();
        for platform in self.platforms() {
            let Some(backend) = self.backends.get_mut(&platform) else {
                continue;
            };
            let outcome = backend.connect().await.map_err(|err| format!("{err:#}"));
            results.push((platform, outcome));
        }
        results
    }

    /// Resolve a category that the config named but did not give an id for.
    ///
    /// A hand-edited preset says `twitch_category = "Just Chatting"` with no id,
    /// so the name has to be turned into one before the plan can be submitted.
    pub async fn resolve_plan(&mut self, plan: &mut StreamPlan, name_hint: &str) -> Result<()> {
        if plan.twitch_category.is_some() || name_hint.trim().is_empty() {
            return Ok(());
        }
        if !self.backends.contains_key(&Platform::Twitch) {
            return Ok(());
        }

        let matches = self.search_categories(Platform::Twitch, name_hint).await?;

        // Prefer an exact match; fall back to the best fuzzy hit rather than
        // failing, since the config was written by a human typing from memory.
        let chosen = matches
            .iter()
            .find(|c| c.name.eq_ignore_ascii_case(name_hint))
            .or_else(|| matches.first())
            .cloned();

        match chosen {
            Some(category) => {
                plan.twitch_category = Some(category);
                Ok(())
            }
            None => anyhow::bail!(
                "could not find a Twitch category matching {name_hint:?} from your config. \
                 Check the spelling against Twitch's own category list."
            ),
        }
    }

    /// Apply the plan to every platform, all at once.
    ///
    /// The platforms are driven concurrently rather than one after another, so
    /// going live on both takes as long as the slower one rather than the sum of
    /// the two. Each backend is moved into its own task and moved back
    /// afterwards, which is what keeps stats polling working later.
    pub async fn go_live(&mut self, plan: &StreamPlan) -> Vec<PlatformResult> {
        self.refresh_tokens().await;

        let mut tasks = tokio::task::JoinSet::new();

        // Take every backend out of the map so each can be moved into a task.
        for (platform, mut backend) in self.backends.drain() {
            let plan = plan.clone();
            tasks.spawn(async move {
                let outcome = backend
                    .go_live(&plan)
                    .await
                    .map_err(|err| format!("{err:#}"));
                (platform, backend, outcome)
            });
        }

        let mut results = Vec::new();
        while let Some(joined) = tasks.join_next().await {
            match joined {
                Ok((platform, backend, outcome)) => {
                    // Put the backend back so `poll_stats` can use it.
                    self.backends.insert(platform, backend);
                    results.push(PlatformResult { platform, outcome });
                }
                Err(err) => {
                    // A panic inside a backend. Report it rather than losing the
                    // platform silently — but the backend is gone, so that
                    // platform will simply be absent from the stats panel.
                    tracing::error!(?err, "a platform task panicked while going live");
                    results.push(PlatformResult {
                        platform: Platform::Twitch,
                        outcome: Err(format!(
                            "internal error: the platform task stopped unexpectedly ({err})"
                        )),
                    });
                }
            }
        }

        // Sort into the canonical platform order so the UI does not reshuffle
        // its panels depending on which request happened to finish first.
        results.sort_by_key(|r| r.platform);
        results
    }

    /// Collect one statistics snapshot from every platform, concurrently.
    pub async fn poll_stats(&mut self) -> Vec<(Platform, PlatformStats)> {
        self.refresh_tokens().await;

        let mut tasks = tokio::task::JoinSet::new();

        for (platform, mut backend) in self.backends.drain() {
            tasks.spawn(async move {
                let stats = match backend.fetch_stats().await {
                    Ok(stats) => stats,
                    // A failed poll must not kill the dashboard. Record the
                    // error in the snapshot and let the UI show it as stale.
                    Err(err) => PlatformStats {
                        error: Some(format!("{err:#}")),
                        ..Default::default()
                    },
                };
                (platform, backend, stats)
            });
        }

        let mut results = Vec::new();
        while let Some(joined) = tasks.join_next().await {
            if let Ok((platform, backend, stats)) = joined {
                self.backends.insert(platform, backend);
                results.push((platform, stats));
            }
        }

        results.sort_by_key(|(platform, _)| *platform);
        results
    }

    /// Search one platform's category list, for the form's autocomplete.
    pub async fn search_categories(
        &mut self,
        platform: Platform,
        query: &str,
    ) -> Result<Vec<Category>> {
        let backend = self
            .backends
            .get_mut(&platform)
            .ok_or_else(|| anyhow::anyhow!("{} is not connected", platform.label()))?;
        backend.search_categories(query).await
    }

    /// Read a platform's stream key without changing anything else.
    ///
    /// Backs `msm key`, which exists so a key can be fetched for pasting into
    /// OBS without the side effect of applying a whole plan.
    pub async fn stream_key(&mut self, platform: Platform) -> Result<Option<String>> {
        let backend = self
            .backends
            .get_mut(&platform)
            .ok_or_else(|| anyhow::anyhow!("{} is not connected", platform.label()))?;
        backend.stream_key().await
    }
}

/// Format a go-live result set as plain text, for the non-interactive CLI path.
pub fn render_results(results: &[PlatformResult]) -> String {
    let mut out = String::new();

    for result in results {
        out.push_str(&format!("\n=== {} ===\n", result.platform.label()));
        match &result.outcome {
            Err(err) => {
                out.push_str(&format!("FAILED: {err}\n"));
            }
            Ok(outcome) => {
                out.push_str("Ready.\n");
                if let Some(url) = &outcome.watch_url {
                    out.push_str(&format!("  Watch:   {url}\n"));
                }
                if let Some(url) = &outcome.manage_url {
                    out.push_str(&format!("  Manage:  {url}\n"));
                }
                if let Some(url) = &outcome.ingest_url {
                    out.push_str(&format!("  Ingest:  {url}\n"));
                }
                if outcome.stream_key.is_some() {
                    // Never printed in full: terminal scrollback and screen
                    // shares leak it. `msm key <platform>` prints it on demand.
                    out.push_str("  Key:     (hidden — run `msm key` to print it)\n");
                }
                for note in &outcome.notes {
                    out.push_str(&format!("  - {note}\n"));
                }
            }
        }
    }

    let failed = results.iter().filter(|r| !r.succeeded()).count();
    if failed > 0 && failed < results.len() {
        out.push_str(
            "\nSome platforms are ready and some are not. The ones marked Ready will \
             work if you start streaming now.\n",
        );
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::GoLiveOutcome;

    fn ok_result(platform: Platform) -> PlatformResult {
        PlatformResult {
            platform,
            outcome: Ok(GoLiveOutcome {
                watch_url: Some("https://example.com/watch".into()),
                stream_key: Some("super-secret-key".into()),
                notes: vec!["a note".into()],
                ..Default::default()
            }),
        }
    }

    fn err_result(platform: Platform) -> PlatformResult {
        PlatformResult {
            platform,
            outcome: Err("out of quota".into()),
        }
    }

    #[test]
    fn rendering_shows_urls_and_notes_for_a_success() {
        let text = render_results(&[ok_result(Platform::Twitch)]);
        assert!(text.contains("Twitch"));
        assert!(text.contains("https://example.com/watch"));
        assert!(text.contains("a note"));
    }

    #[test]
    fn the_stream_key_is_never_printed() {
        let text = render_results(&[ok_result(Platform::Twitch)]);
        assert!(
            !text.contains("super-secret-key"),
            "the stream key must not appear in ordinary output"
        );
        assert!(text.contains("msm key"));
    }

    #[test]
    fn a_failure_is_reported_with_its_reason() {
        let text = render_results(&[err_result(Platform::YouTube)]);
        assert!(text.contains("FAILED"));
        assert!(text.contains("out of quota"));
    }

    #[test]
    fn partial_success_gets_an_explicit_explanation() {
        let text = render_results(&[ok_result(Platform::Twitch), err_result(Platform::YouTube)]);
        assert!(text.contains("Some platforms are ready and some are not"));
    }

    #[test]
    fn total_failure_does_not_claim_partial_success() {
        let text = render_results(&[err_result(Platform::Twitch), err_result(Platform::YouTube)]);
        assert!(!text.contains("Some platforms are ready"));
    }

    #[test]
    fn results_are_reported_in_a_stable_platform_order() {
        let mut results = [err_result(Platform::YouTube), ok_result(Platform::Twitch)];
        results.sort_by_key(|r| r.platform);
        assert_eq!(results[0].platform, Platform::Twitch);
    }
}
