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
    /// returns every working backend holds a usable access token.
    ///
    /// One platform's problem must not take the others down — that is the
    /// partial-success rule this whole module is built around, and it applies
    /// from the very first step. A platform whose credentials are missing or
    /// whose login cannot be renewed is returned in the second half of the
    /// tuple with the explanation, while every healthy platform still gets its
    /// backend. (This used to be all-or-nothing: an expired YouTube login
    /// stopped Twitch from being configured at all, and the single error was
    /// then shown against every platform.) The `Err` case is reserved for
    /// problems that genuinely affect everything, like failing to construct
    /// the HTTP client.
    pub async fn build(
        config: &Config,
        platforms: &[Platform],
    ) -> Result<(Self, Vec<(Platform, String)>)> {
        // One HTTP client shared by every backend, so connections are pooled
        // rather than re-established for each call.
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .user_agent(concat!("multistream-manager/", env!("CARGO_PKG_VERSION")))
            .build()
            .context("building the HTTP client")?;

        let mut failures: Vec<(Platform, String)> = Vec::new();

        // Credentials are checked per platform, so the error a platform gets
        // is its own rather than the first problem found anywhere.
        let mut with_credentials = Vec::new();
        for &platform in platforms {
            match config.check_credentials(&[platform]) {
                Ok(()) => with_credentials.push(platform),
                Err(err) => failures.push((platform, format!("{err:#}"))),
            }
        }

        let mut backends: HashMap<Platform, Box<dyn Backend>> = HashMap::new();
        for (platform, token) in auth::access_tokens(config, &with_credentials).await {
            let token = match token {
                Ok(token) => token,
                Err(err) => {
                    failures.push((platform, format!("{err:#}")));
                    continue;
                }
            };
            let backend: Box<dyn Backend> = match platform {
                Platform::Twitch => Box::new(TwitchBackend::new(
                    http.clone(),
                    config.twitch.client_id(),
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

        failures.sort_by_key(|(platform, _)| *platform);
        Ok((
            Self {
                config: config.clone(),
                backends,
            },
            failures,
        ))
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

        // Same panic bookkeeping as `go_live`: if a task dies, its id is the
        // only thing that comes back, and this map turns it into a platform.
        let mut task_platforms = HashMap::new();

        for (platform, mut backend) in self.backends.drain() {
            let handle = tasks.spawn(async move {
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
            task_platforms.insert(handle.id(), platform);
        }

        let mut results = Vec::new();
        while let Some(joined) = tasks.join_next().await {
            match joined {
                Ok((platform, backend, stats)) => {
                    self.backends.insert(platform, backend);
                    results.push((platform, stats));
                }
                Err(err) => {
                    // A panic inside fetch_stats. The backend was moved into
                    // the task and died with it, so this platform is gone for
                    // the rest of the session — but that must be *said*, as an
                    // error snapshot under the right platform's name, not
                    // spelled by the panel silently freezing forever.
                    let Some(platform) = task_platforms.get(&err.id()).copied() else {
                        tracing::error!(?err, "a statistics task failed with an unknown id");
                        continue;
                    };
                    tracing::error!(
                        platform = platform.slug(),
                        ?err,
                        "a platform task panicked while fetching statistics"
                    );
                    results.push((
                        platform,
                        PlatformStats {
                            error: Some(format!(
                                "internal error: the statistics task stopped unexpectedly ({err})"
                            )),
                            ..Default::default()
                        },
                    ));
                }
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
    /// the stream listing in Config → Housekeeping.
    pub async fn list_ingest_endpoints(
        &mut self,
        platform: Platform,
    ) -> Result<Vec<IngestEndpoint>> {
        self.backend(platform)?.list_ingest_endpoints().await
    }

    /// List broadcasts that were created but never received a feed. Backs the
    /// listing half of the cleanup job in Config → Housekeeping.
    pub async fn list_stale_broadcasts(
        &mut self,
        platform: Platform,
    ) -> Result<Vec<StaleBroadcast>> {
        self.backend(platform)?.list_stale_broadcasts().await
    }

    /// Delete one broadcast. Backs the second press of the cleanup job, which is
    /// the one that actually removes anything.
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
    fn results_are_reported_in_a_stable_platform_order() {
        let mut results = [err_result(Platform::YouTube), ok_result(Platform::Twitch)];
        results.sort_by_key(|r| r.platform);
        assert_eq!(results[0].platform, Platform::Twitch);
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
            let panics = self.panics;
            Box::pin(async move {
                assert!(!panics, "deliberate test panic");
                Ok(PlatformStats::default())
            })
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

    /// A panic during a statistics poll used to be dropped without a trace:
    /// no snapshot, no error, and the platform's dashboard panel simply froze
    /// on stale numbers forever. The platform cannot keep polling (its backend
    /// died with the task), but the failure must be reported as an error
    /// snapshot under the right platform's name.
    #[tokio::test]
    async fn a_panicking_stats_poll_reports_an_error_instead_of_vanishing() {
        let _scratch = crate::paths::test_support::ScratchConfigDir::new("engine-poll-stats");

        let mut engine = engine_with(vec![(Platform::Twitch, false), (Platform::YouTube, true)]);

        let results = engine.poll_stats().await;

        assert_eq!(results.len(), 2, "both platforms must be reported");
        let (_, twitch) = &results[0];
        let (platform, youtube) = &results[1];
        assert_eq!(*platform, Platform::YouTube);
        assert!(twitch.error.is_none());
        assert!(
            youtube
                .error
                .as_deref()
                .unwrap_or("")
                .contains("unexpectedly"),
            "the panicking platform must carry an error snapshot: {youtube:?}"
        );
    }

    /// Building must follow the partial-success rule too: each platform gets
    /// its own explanation, and no platform's problem is fatal to the others.
    /// (With no credentials configured at all, both fail — but separately.)
    #[tokio::test]
    async fn building_reports_each_platform_failure_separately() {
        let _scratch = crate::paths::test_support::ScratchConfigDir::new("engine-build");

        let (engine, failures) = Engine::build(&Config::default(), &Platform::ALL)
            .await
            .expect("only a global failure may abort the build");

        assert!(engine.platforms().is_empty());
        assert_eq!(failures.len(), 2, "one failure per platform: {failures:?}");
        assert_eq!(failures[0].0, Platform::Twitch);
        assert_eq!(failures[1].0, Platform::YouTube);
        // Each message must be about its own platform, not a shared blur.
        assert!(failures[0].1.to_lowercase().contains("twitch"));
        assert!(failures[1].1.to_lowercase().contains("youtube"));
    }

    /// A backend that panics must be reported against *its own* platform. The
    /// bug this guards against tagged every panic as Twitch, so a broken
    /// YouTube produced two contradictory Twitch rows and no YouTube row at
    /// all — the dashboard blamed a healthy platform that had worked.
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
