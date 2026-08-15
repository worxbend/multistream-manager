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
use crate::model::{Category, IngestEndpoint, Platform, PlatformStats, StaleBroadcast, StreamPlan};
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
    ///
    /// All the platforms are done in one call so that `tokens.json` is read once
    /// per poll rather than once per platform, and written only when a token was
    /// genuinely renewed.
    async fn refresh_tokens(&mut self) {
        let platforms = self.platforms();
        for (platform, result) in auth::access_tokens(&self.config, &platforms).await {
            match result {
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

        // Remember which task belongs to which platform. If a task panics we get
        // back only its id, not the tuple it would have returned, so without this
        // map there is no way to say which platform failed.
        let mut task_platforms = HashMap::new();

        // Take every backend out of the map so each can be moved into a task.
        for (platform, mut backend) in self.backends.drain() {
            let plan = plan.clone();
            let handle = tasks.spawn(async move {
                let outcome = backend
                    .go_live(&plan)
                    .await
                    .map_err(|err| format!("{err:#}"));
                (platform, backend, outcome)
            });
            task_platforms.insert(handle.id(), platform);
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
                    //
                    // The id lookup is what names the platform that actually
                    // failed. Guessing one instead would blame a healthy platform
                    // for the error and hide the broken one entirely.
                    let Some(platform) = task_platforms.get(&err.id()).copied() else {
                        tracing::error!(?err, "a platform task failed with an unknown id");
                        continue;
                    };
                    tracing::error!(
                        platform = platform.slug(),
                        ?err,
                        "a platform task panicked while going live"
                    );
                    results.push(PlatformResult {
                        platform,
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
        self.backend(platform)?.search_categories(query).await
    }

    /// Read a platform's stream key without changing anything else.
    ///
    /// Backs `msm key`, which exists so a key can be fetched for pasting into
    /// OBS without the side effect of applying a whole plan.
    pub async fn stream_key(&mut self, platform: Platform) -> Result<Option<String>> {
        self.backend(platform)?.stream_key().await
    }

    /// List the RTMP ingest endpoints on a platform's account. Backs
    /// `msm streams`.
    pub async fn list_ingest_endpoints(
        &mut self,
        platform: Platform,
    ) -> Result<Vec<IngestEndpoint>> {
        self.backend(platform)?.list_ingest_endpoints().await
    }

    /// List broadcasts that were created but never received a feed. Backs the
    /// listing half of `msm cleanup`.
    pub async fn list_stale_broadcasts(
        &mut self,
        platform: Platform,
    ) -> Result<Vec<StaleBroadcast>> {
        self.backend(platform)?.list_stale_broadcasts().await
    }

    /// Delete one broadcast. Backs the `--yes` half of `msm cleanup`.
    pub async fn delete_broadcast(&mut self, platform: Platform, id: &str) -> Result<()> {
        self.backend(platform)?.delete_broadcast(id).await
    }

    /// The backend for a platform, or an error naming the platform that is not
    /// connected — which is far more useful than a panic on a missing key.
    // The `+ 'static` is load-bearing rather than decorative: the map holds
    // `Box<dyn Backend>`, which means `dyn Backend + 'static`, and a `&mut` to a
    // trait object is invariant in that bound — so shortening it here would be
    // rejected by the borrow checker.
    fn backend(&mut self, platform: Platform) -> Result<&mut (dyn Backend + 'static)> {
        self.backends
            .get_mut(&platform)
            .map(|backend| backend.as_mut())
            .ok_or_else(|| anyhow::anyhow!("{} is not connected", platform.label()))
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

/// Format a go-live result set as JSON, for `msm go --json`.
///
/// One object per platform, in the same order as the human report, so a wrapper
/// script can loop over the array and act on each platform in turn. Every field
/// is present on every object — `null` where the platform did not supply one —
/// so a consumer can read `.watch_url` without first checking that it exists.
///
/// ## The stream key is deliberately absent
///
/// This output exists to be piped into another program, redirected into a file,
/// or pasted into a bug report. A stream key that lands in any of those places
/// lets whoever reads it broadcast to your channel, and unlike a password there
/// is no prompt in front of it. `msm key` prints it when you actually want it,
/// as a separate and deliberate act.
pub fn render_results_json(results: &[PlatformResult]) -> String {
    let items: Vec<serde_json::Value> = results
        .iter()
        .map(|result| {
            let (ok, watch, manage, ingest, error, notes) = match &result.outcome {
                Ok(outcome) => (
                    true,
                    outcome.watch_url.clone(),
                    outcome.manage_url.clone(),
                    outcome.ingest_url.clone(),
                    None,
                    outcome.notes.clone(),
                ),
                Err(err) => (false, None, None, None, Some(err.clone()), Vec::new()),
            };

            serde_json::json!({
                "platform": result.platform.slug(),
                "ok": ok,
                "watch_url": watch,
                "manage_url": manage,
                "ingest_url": ingest,
                "error": error,
                "notes": notes,
            })
        })
        .collect();

    // Serialising an array of plain JSON values cannot fail, but returning an
    // empty array rather than panicking keeps a scripted caller's parser happy
    // even in the impossible case.
    serde_json::to_string_pretty(&items).unwrap_or_else(|_| "[]".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::BoxFuture;
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

    /// Parse the JSON report back out, so the tests assert on the structure a
    /// script would actually see rather than on the exact text.
    fn parsed_json(results: &[PlatformResult]) -> Vec<serde_json::Value> {
        serde_json::from_str(&render_results_json(results))
            .expect("the JSON report must be valid JSON")
    }

    #[test]
    fn the_json_report_is_an_array_with_one_object_per_platform() {
        let items = parsed_json(&[ok_result(Platform::Twitch), err_result(Platform::YouTube)]);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0]["platform"], "twitch");
        assert_eq!(items[1]["platform"], "youtube");
    }

    #[test]
    fn a_successful_platform_reports_ok_with_its_urls_and_notes() {
        let items = parsed_json(&[ok_result(Platform::Twitch)]);
        assert_eq!(items[0]["ok"], true);
        assert_eq!(items[0]["watch_url"], "https://example.com/watch");
        assert_eq!(items[0]["error"], serde_json::Value::Null);
        assert_eq!(items[0]["notes"][0], "a note");
    }

    #[test]
    fn a_failed_platform_reports_its_reason_rather_than_disappearing() {
        // Partial success is the point: a script has to be able to see that one
        // platform worked and why the other did not.
        let items = parsed_json(&[err_result(Platform::YouTube)]);
        assert_eq!(items[0]["ok"], false);
        assert_eq!(items[0]["error"], "out of quota");
        assert_eq!(items[0]["watch_url"], serde_json::Value::Null);
    }

    #[test]
    fn the_json_report_never_contains_the_stream_key() {
        // The whole reason this output is safe to redirect into a file is that
        // the key is not in it. A key is enough on its own to broadcast to the
        // channel, and JSON output is meant to be stored and passed around.
        let text = render_results_json(&[ok_result(Platform::Twitch)]);
        assert!(
            !text.contains("super-secret-key"),
            "the stream key leaked into the machine-readable output"
        );
        assert!(!text.contains("stream_key"));
    }

    #[test]
    fn every_documented_field_is_present_even_when_it_has_no_value() {
        // A consumer should be able to read a field without checking for it
        // first, so absence is spelled `null` rather than by omitting the key.
        let items = parsed_json(&[err_result(Platform::Twitch)]);
        for field in [
            "platform",
            "ok",
            "watch_url",
            "manage_url",
            "ingest_url",
            "error",
            "notes",
        ] {
            assert!(
                items[0].get(field).is_some(),
                "the {field:?} field is missing from the JSON report"
            );
        }
    }

    #[test]
    fn an_empty_result_set_still_produces_a_parseable_array() {
        // `msm go` can in principle end with nothing to report; a script must
        // not have to special-case an empty stdout.
        assert!(parsed_json(&[]).is_empty());
    }

    /// A backend that does nothing except succeed, or panic on `go_live`.
    struct FakeBackend {
        panics: bool,
    }

    impl Backend for FakeBackend {
        fn connect(&mut self) -> BoxFuture<'_, Result<String>> {
            Box::pin(async { Ok("fake".to_string()) })
        }

        fn go_live<'a>(
            &'a mut self,
            _plan: &'a StreamPlan,
        ) -> BoxFuture<'a, Result<GoLiveOutcome>> {
            let panics = self.panics;
            Box::pin(async move {
                assert!(!panics, "deliberate test panic");
                Ok(GoLiveOutcome::default())
            })
        }

        fn fetch_stats(&mut self) -> BoxFuture<'_, Result<PlatformStats>> {
            Box::pin(async { Ok(PlatformStats::default()) })
        }

        fn set_access_token(&mut self, _token: String) {}
    }

    fn engine_with(backends: Vec<(Platform, bool)>) -> Engine {
        Engine {
            config: Config::default(),
            backends: backends
                .into_iter()
                .map(|(platform, panics)| {
                    let backend: Box<dyn Backend> = Box::new(FakeBackend { panics });
                    (platform, backend)
                })
                .collect(),
        }
    }

    /// A backend that panics must be reported against *its own* platform. The
    /// bug this guards against tagged every panic as Twitch, so a broken
    /// YouTube produced two contradictory Twitch rows and no YouTube row at
    /// all — the dashboard blamed a healthy platform and `msm go` exited 0.
    #[tokio::test]
    async fn a_panicking_backend_is_reported_against_its_own_platform() {
        // Token lookups must not touch the real user's files.
        let _scratch = crate::paths::test_support::ScratchConfigDir::new("engine-go-live");

        let mut engine = engine_with(vec![(Platform::Twitch, false), (Platform::YouTube, true)]);

        let results = engine.go_live(&StreamPlan::default()).await;

        assert_eq!(results.len(), 2);
        let twitch = results
            .iter()
            .find(|r| r.platform == Platform::Twitch)
            .expect("twitch must be reported");
        let youtube = results
            .iter()
            .find(|r| r.platform == Platform::YouTube)
            .expect("the panicking platform must still be reported, under its own name");
        assert!(twitch.succeeded());
        assert!(!youtube.succeeded());
    }
}
