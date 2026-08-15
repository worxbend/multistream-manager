//! The Twitch backend, built on the Helix API.
//!
//! Twitch is the simpler of the two platforms. There is no "create a broadcast"
//! step at all — your channel is permanently there, and going live just means
//! pointing OBS at it. All this backend has to do is set the channel's title,
//! category, language and tags before you start.
//!
//! The one wrinkle is that Twitch will not accept a category *name*. It only
//! accepts a numeric `game_id`, so a name like "Software and Game Development"
//! has to be looked up first. That lookup is also what powers the autocomplete
//! in the form.

use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;
use std::time::Instant;

use crate::backend::{Backend, BoxFuture, AUDIENCE_REFRESH};
use crate::model::{Category, GoLiveOutcome, PlatformStats, Stat, StreamPlan};

const HELIX: &str = "https://api.twitch.tv/helix";
const VALIDATE_URL: &str = "https://id.twitch.tv/oauth2/validate";

/// The Twitch API client.
pub struct TwitchBackend {
    http: reqwest::Client,
    client_id: String,
    access_token: String,
    /// Filled in by `connect`. Every channel endpoint needs it.
    broadcaster_id: Option<String>,
    /// The channel's login name, used to build the public channel URL.
    login: Option<String>,
    /// Where the Helix API lives. Always [`HELIX`] in the running program; the
    /// tests point it at a local server so real request behaviour — how many
    /// calls are made, and whether they overlap — can be observed.
    base: String,
    /// The follower and subscriber totals, with the time they were fetched.
    ///
    /// Both are re-read only every [`AUDIENCE_REFRESH`]; see that constant for
    /// why. `None` inside the tuple means the platform declined to answer — a
    /// non-affiliate channel gets a 403 for subscriptions — and that is cached
    /// too, so polls stop re-asking a question already refused. A request that
    /// merely failed (timeout, 429, expired token) is never written here, so
    /// the next poll tries again instead of showing a blank row for the whole
    /// refresh window.
    audience_cache: Option<(Instant, Option<u64>, Option<u64>)>,
}

impl TwitchBackend {
    pub fn new(http: reqwest::Client, client_id: String, access_token: String) -> Self {
        Self {
            http,
            client_id,
            access_token,
            broadcaster_id: None,
            login: None,
            base: HELIX.to_string(),
            audience_cache: None,
        }
    }

    /// Attach the two headers every Helix request needs.
    fn request(&self, method: reqwest::Method, url: &str) -> reqwest::RequestBuilder {
        self.http
            .request(method, url)
            .header("Client-Id", &self.client_id)
            .bearer_auth(&self.access_token)
    }

    /// The broadcaster id, or a clear error if `connect` was never called.
    fn broadcaster_id(&self) -> Result<&str> {
        self.broadcaster_id
            .as_deref()
            .ok_or_else(|| anyhow!("internal error: Twitch backend used before connect()"))
    }

    /// Ask Twitch who this token belongs to.
    ///
    /// The `/validate` endpoint is special: it takes an `OAuth <token>` header
    /// rather than the usual `Bearer <token>`, and it needs no client id. It
    /// returns the user id, the login name, and the granted scopes — which is
    /// exactly the set of things we need before doing anything else.
    async fn validate(&self) -> Result<ValidateResponse> {
        let response = self
            .http
            .get(VALIDATE_URL)
            .header("Authorization", format!("OAuth {}", self.access_token))
            .send()
            .await
            .context("contacting Twitch to validate your access token")?;

        if response.status() == reqwest::StatusCode::UNAUTHORIZED {
            bail!(
                "your Twitch access token is not valid. Run `msm login twitch` to authorise again."
            );
        }

        let response = check(response, "validating the Twitch token").await?;
        response
            .json::<ValidateResponse>()
            .await
            .context("parsing Twitch's token validation response")
    }

    /// Look up categories by name.
    ///
    /// Twitch's search is fuzzy, so "software" finds "Software and Game
    /// Development". Results come back in Twitch's own relevance order, which is
    /// good enough to show directly in the autocomplete list.
    pub async fn search_categories_impl(&self, query: &str) -> Result<Vec<Category>> {
        if query.trim().is_empty() {
            return Ok(Vec::new());
        }

        let url = format!(
            "{}/search/categories?query={}&first=25",
            self.base,
            urlencoding::encode(query.trim())
        );
        let response = self
            .request(reqwest::Method::GET, &url)
            .send()
            .await
            .context("searching Twitch categories")?;
        let response = check(response, "searching Twitch categories").await?;

        let body: DataList<TwitchCategory> = response
            .json()
            .await
            .context("parsing the Twitch category search response")?;

        Ok(body
            .data
            .into_iter()
            .map(|c| Category {
                id: c.id,
                name: c.name,
            })
            .collect())
    }

    /// Fetch the RTMP stream key so the dashboard can display it.
    ///
    /// This is best-effort: if the token lacks `channel:read:stream_key` we would
    /// rather show the dashboard without a key than fail the whole go-live.
    async fn fetch_stream_key(&self) -> Option<String> {
        let id = self.broadcaster_id().ok()?;
        let base = &self.base;
        let url = format!("{base}/streams/key?broadcaster_id={id}");
        let response = self.request(reqwest::Method::GET, &url).send().await.ok()?;
        if !response.status().is_success() {
            tracing::warn!(status = ?response.status(), "could not read the Twitch stream key");
            return None;
        }
        let body: DataList<StreamKey> = response.json().await.ok()?;
        body.data.into_iter().next().map(|k| k.stream_key)
    }

    /// Total follower count. Best-effort, same reasoning as the stream key.
    async fn follower_count(&self) -> AudienceProbe {
        let Ok(id) = self.broadcaster_id() else {
            return AudienceProbe::Refused;
        };
        // `first=1` because we only want the `total` field, not the actual list.
        let base = &self.base;
        let url = format!("{base}/channels/followers?broadcaster_id={id}&first=1");
        probe(self.request(reqwest::Method::GET, &url).send().await).await
    }

    /// Total subscriber count. Non-affiliate channels get a 403 here, which is
    /// expected and simply means the row is omitted from the stats panel.
    async fn subscriber_count(&self) -> AudienceProbe {
        let Ok(id) = self.broadcaster_id() else {
            return AudienceProbe::Refused;
        };
        let base = &self.base;
        let url = format!("{base}/subscriptions?broadcaster_id={id}&first=1");
        probe(self.request(reqwest::Method::GET, &url).send().await).await
    }

    /// Apply the plan to the channel.
    async fn update_channel(&self, plan: &StreamPlan) -> Result<()> {
        let id = self.broadcaster_id()?;
        let category = plan.twitch_category.as_ref().ok_or_else(|| {
            anyhow!("no Twitch category was selected, and Twitch requires one to update a channel")
        })?;

        let tags = plan.twitch_tags();

        let body = UpdateChannelRequest {
            title: plan.twitch_title(),
            game_id: category.id.clone(),
            broadcaster_language: plan.language.clone(),
            // Twitch documents an empty array as "remove all tags". Sending one
            // when the user simply did not set any tags would silently wipe the
            // tags already on their channel, so the field is omitted instead —
            // every field on this endpoint is optional.
            tags: if tags.is_empty() { None } else { Some(tags) },
        };

        let base = &self.base;
        let url = format!("{base}/channels?broadcaster_id={id}");
        let response = self
            .request(reqwest::Method::PATCH, &url)
            .json(&body)
            .send()
            .await
            .context("sending your channel update to Twitch")?;

        // A successful update returns 204 No Content — no body to parse.
        check(response, "updating your Twitch channel").await?;
        Ok(())
    }
}

impl Backend for TwitchBackend {
    fn connect(&mut self) -> BoxFuture<'_, Result<String>> {
        Box::pin(async move {
            let validated = self.validate().await?;

            // Fail early and specifically if the saved token predates a scope we
            // now need. Otherwise the user gets a bare 401 from a later call.
            let required = "channel:manage:broadcast";
            if !validated.scopes.is_empty() && !validated.scopes.iter().any(|s| s == required) {
                bail!(
                    "your saved Twitch token does not include the {required} permission, \
                     so it cannot change your stream title or category. \
                     Run `msm login twitch` to re-authorise with the current permissions."
                );
            }

            self.broadcaster_id = Some(validated.user_id.clone());
            self.login = Some(validated.login.clone());
            Ok(validated.login)
        })
    }

    fn go_live<'a>(&'a mut self, plan: &'a StreamPlan) -> BoxFuture<'a, Result<GoLiveOutcome>> {
        Box::pin(async move {
            self.update_channel(plan).await?;

            let login = self
                .login
                .clone()
                .unwrap_or_else(|| "your-channel".to_string());

            let mut notes = vec![
                "Channel updated. Twitch has no separate \"create broadcast\" step — \
                 start streaming in OBS whenever you are ready."
                    .to_string(),
            ];

            let dropped = plan.tags.len().saturating_sub(plan.twitch_tags().len());
            if dropped > 0 {
                notes.push(format!(
                    "{dropped} tag(s) were not sent to Twitch because Twitch allows at most 10."
                ));
            }

            Ok(GoLiveOutcome {
                watch_url: Some(format!("https://twitch.tv/{login}")),
                manage_url: Some(format!(
                    "https://dashboard.twitch.tv/u/{login}/stream-manager"
                )),
                ingest_url: Some("rtmp://live.twitch.tv/app".to_string()),
                stream_key: self.fetch_stream_key().await,
                notes,
            })
        })
    }

    fn fetch_stats(&mut self) -> BoxFuture<'_, Result<PlatformStats>> {
        Box::pin(async move {
            let id = self.broadcaster_id()?.to_string();

            // The three calls do not depend on each other, so they are started
            // together and awaited together. Run one after another they cost
            // three round trips on every poll — at the default 15-second
            // interval, on a slow connection, that was a noticeable part of the
            // interval spent waiting.
            let base = &self.base;
            let url = format!("{base}/streams?user_id={id}");
            let live = async {
                let response = self
                    .request(reqwest::Method::GET, &url)
                    .send()
                    .await
                    .context("asking Twitch whether you are live")?;
                let response = check(response, "reading your Twitch stream status").await?;
                response
                    .json::<DataList<StreamInfo>>()
                    .await
                    .context("parsing the Twitch stream status")
            };

            // Follower and subscriber totals are re-read only occasionally, so
            // most polls make one request rather than three.
            let cached = match &self.audience_cache {
                Some((at, followers, subs)) if at.elapsed() < AUDIENCE_REFRESH => {
                    Some((*followers, *subs))
                }
                _ => None,
            };

            let (body, (followers, subs)) = match cached {
                Some(audience) => (live.await, audience),
                None => {
                    let (body, followers, subs) =
                        tokio::join!(live, self.follower_count(), self.subscriber_count());
                    let previous = self
                        .audience_cache
                        .as_ref()
                        .map(|(_, followers, subs)| (*followers, *subs));
                    let (followers, subs, settled) = resolve_audience(previous, followers, subs);
                    if settled {
                        self.audience_cache = Some((Instant::now(), followers, subs));
                    }
                    (body, (followers, subs))
                }
            };
            let body = body?;

            let mut stats = PlatformStats::default();

            if let Some(stream) = body.data.into_iter().next() {
                stats.live = stream.stream_type == "live";
                stats.viewers = Some(stream.viewer_count);
                stats.started_at = stream.started_at;
                // `game_name` comes back as an empty string when the channel
                // has no category set, which would render a labelled blank row.
                if !stream.game_name.is_empty() {
                    stats.extra.push(Stat {
                        label: "Category".into(),
                        value: stream.game_name,
                    });
                }
            }

            if let Some(followers) = followers {
                stats.extra.push(Stat {
                    label: "Followers".into(),
                    value: format_count(followers),
                });
            }
            if let Some(subs) = subs {
                stats.extra.push(Stat {
                    label: "Subscribers".into(),
                    value: format_count(subs),
                });
            }

            Ok(stats)
        })
    }

    fn search_categories<'a>(&'a mut self, query: &'a str) -> BoxFuture<'a, Result<Vec<Category>>> {
        Box::pin(async move { self.search_categories_impl(query).await })
    }

    fn set_access_token(&mut self, token: String) {
        self.access_token = token;
    }

    fn stream_key(&mut self) -> BoxFuture<'_, Result<Option<String>>> {
        Box::pin(async move { Ok(self.fetch_stream_key().await) })
    }
}

/// What one follower/subscriber lookup came back with.
///
/// The distinction matters because the answer is cached for a while: a refusal
/// is worth remembering ("stop asking"), a temporary failure is not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AudienceProbe {
    /// Twitch answered with a number.
    Count(u64),
    /// Twitch answered, and the answer is "you may not have this" — a
    /// non-affiliate channel asking for its subscriber total gets a 403. That
    /// will not change between two polls, so it is safe to cache as "no row".
    Refused,
    /// The question never got a usable answer: a timeout, a dropped
    /// connection, a 429 rate limit, an expired token or a Twitch 5xx. Nothing
    /// was learned, so nothing should be written to the cache.
    Unavailable,
}

/// Classify one lookup's outcome, reading the body only when it is worth it.
async fn probe(sent: reqwest::Result<reqwest::Response>) -> AudienceProbe {
    let Ok(response) = sent else {
        return AudienceProbe::Unavailable;
    };
    let status = response.status();
    if !status.is_success() {
        // 401 means the token needs refreshing, 429 means slow down, 5xx means
        // Twitch is having a moment — all of them succeed again later. Only a
        // deliberate "no" (403, and 404 for a channel Twitch will not describe)
        // is a settled answer.
        return match status.as_u16() {
            403 | 404 => AudienceProbe::Refused,
            _ => AudienceProbe::Unavailable,
        };
    }
    match response.json::<TotalList>().await {
        Ok(body) => match body.total {
            Some(total) => AudienceProbe::Count(total),
            // A 200 with no `total` is Twitch saying there is nothing to show.
            None => AudienceProbe::Refused,
        },
        Err(_) => AudienceProbe::Unavailable,
    }
}

/// Work out what to display and whether the result may be cached.
///
/// `previous` is whatever the cache already held (the values only — its age no
/// longer matters, since we are past the refresh window). The returned flag is
/// true only when both lookups produced a settled answer; if either was merely
/// unavailable the caller must leave the cache alone so the next poll retries
/// instead of showing a blank row for the whole refresh window.
fn resolve_audience(
    previous: Option<(Option<u64>, Option<u64>)>,
    followers: AudienceProbe,
    subs: AudienceProbe,
) -> (Option<u64>, Option<u64>, bool) {
    fn value(probe: AudienceProbe, previous: Option<u64>) -> Option<u64> {
        match probe {
            AudienceProbe::Count(n) => Some(n),
            AudienceProbe::Refused => None,
            // Keep showing the last known number rather than blanking the row
            // over one failed request.
            AudienceProbe::Unavailable => previous,
        }
    }

    let settled = !matches!(followers, AudienceProbe::Unavailable)
        && !matches!(subs, AudienceProbe::Unavailable);
    (
        value(followers, previous.and_then(|(f, _)| f)),
        value(subs, previous.and_then(|(_, s)| s)),
        settled,
    )
}

/// Turn a non-2xx response into an error carrying Twitch's own explanation.
async fn check(response: reqwest::Response, action: &str) -> Result<reqwest::Response> {
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }

    let body = response.text().await.unwrap_or_default();
    let detail = serde_json::from_str::<serde_json::Value>(&body)
        .ok()
        .and_then(|json| {
            json.get("message")
                .and_then(|m| m.as_str())
                .map(str::to_string)
        })
        .unwrap_or_else(|| body.chars().take(200).collect());

    // Translate the status codes that have a specific, actionable cause.
    let hint = match status.as_u16() {
        401 => "\n  Your Twitch login has expired. Run `msm login twitch`.",
        403 => {
            "\n  Twitch refused this action. If you were changing the category, some \
                categories are age-restricted or unavailable in your region."
        }
        429 => "\n  You have hit Twitch's rate limit. Wait a minute and try again.",
        400 => {
            "\n  Twitch rejected the request as malformed. A too-long title or an \
                invalid tag is the usual cause."
        }
        _ => "",
    };

    bail!("{action} failed (HTTP {status}): {detail}{hint}")
}

/// Render a large number compactly: 12345 becomes "12.3K".
fn format_count(n: u64) -> String {
    match n {
        0..=999 => n.to_string(),
        1_000..=999_999 => format!("{:.1}K", n as f64 / 1_000.0),
        _ => format!("{:.1}M", n as f64 / 1_000_000.0),
    }
}

// ---------------------------------------------------------------------------
// Wire types. These mirror Twitch's JSON exactly; nothing else uses them.
// ---------------------------------------------------------------------------

/// Almost every Helix response wraps its payload in a `data` array.
#[derive(Debug, Deserialize)]
struct DataList<T> {
    #[serde(default = "Vec::new")]
    data: Vec<T>,
}

/// Responses that carry a `total` alongside a (here unused) `data` array.
#[derive(Debug, Deserialize)]
struct TotalList {
    #[serde(default)]
    total: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct ValidateResponse {
    user_id: String,
    login: String,
    #[serde(default)]
    scopes: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct TwitchCategory {
    id: String,
    name: String,
}

#[derive(Debug, Deserialize)]
struct StreamKey {
    stream_key: String,
}

#[derive(Debug, Deserialize)]
struct StreamInfo {
    #[serde(default)]
    viewer_count: u64,
    #[serde(default)]
    started_at: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default)]
    game_name: String,
    /// Named `type` in the JSON, which is a reserved word in Rust.
    #[serde(rename = "type", default)]
    stream_type: String,
}

#[derive(Debug, serde::Serialize)]
struct UpdateChannelRequest {
    title: String,
    game_id: String,
    broadcaster_language: String,
    /// Omitted entirely when there are no tags. See `update_channel` for why
    /// sending `[]` here would be destructive.
    #[serde(skip_serializing_if = "Option::is_none")]
    tags: Option<Vec<String>>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration as StdDuration;

    #[test]
    fn counts_are_abbreviated_for_display() {
        assert_eq!(format_count(0), "0");
        assert_eq!(format_count(999), "999");
        assert_eq!(format_count(12_345), "12.3K");
        assert_eq!(format_count(2_500_000), "2.5M");
    }

    #[test]
    fn a_helix_list_response_deserialises() {
        let json = r#"{"data":[{"id":"509658","name":"Just Chatting"}]}"#;
        let parsed: DataList<TwitchCategory> = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.data[0].name, "Just Chatting");
    }

    #[test]
    fn an_empty_helix_response_deserialises_to_an_empty_list() {
        // Twitch returns `{"data":[]}` when you are offline; it must not error.
        let parsed: DataList<StreamInfo> = serde_json::from_str(r#"{"data":[]}"#).unwrap();
        assert!(parsed.data.is_empty());
    }

    #[test]
    fn stream_info_maps_the_reserved_type_field() {
        let json = r#"{"viewer_count":42,"type":"live","game_name":"Chess",
                       "started_at":"2026-08-08T10:00:00Z"}"#;
        let parsed: StreamInfo = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.stream_type, "live");
        assert_eq!(parsed.viewer_count, 42);
        assert!(parsed.started_at.is_some());
    }

    #[test]
    fn the_update_request_serialises_to_the_field_names_twitch_expects() {
        let request = UpdateChannelRequest {
            title: "Hello".into(),
            game_id: "1469308723".into(),
            broadcaster_language: "en".into(),
            tags: Some(vec!["rust".into()]),
        };
        let json = serde_json::to_value(&request).unwrap();
        assert_eq!(json["game_id"], "1469308723");
        assert_eq!(json["broadcaster_language"], "en");
        assert_eq!(json["tags"][0], "rust");
    }

    #[test]
    fn no_tags_omits_the_field_rather_than_sending_an_empty_array() {
        // Twitch documents `"tags": []` as "remove every tag from the channel",
        // so sending it for a preset that simply has no tags would destroy tags
        // the user set elsewhere.
        let request = UpdateChannelRequest {
            title: "Hello".into(),
            game_id: "1".into(),
            broadcaster_language: "en".into(),
            tags: None,
        };
        let json = serde_json::to_value(&request).unwrap();
        assert!(
            json.get("tags").is_none(),
            "an empty tag list must be omitted, not sent as []: {json}"
        );
    }

    #[test]
    fn using_the_backend_before_connecting_is_a_clear_internal_error() {
        let backend = TwitchBackend::new(reqwest::Client::new(), "id".into(), "token".into());
        let err = backend.broadcaster_id().unwrap_err().to_string();
        assert!(err.contains("before connect()"));
    }

    /// A minimal Helix stand-in: every endpoint takes `delay` to answer, so the
    /// wall-clock time of one `fetch_stats` says whether the calls overlapped.
    ///
    /// Returns the base URL to point a backend at, and a counter of how many
    /// requests each path received.
    async fn fake_helix(
        delay: StdDuration,
    ) -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorded = seen.clone();

        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let recorded = recorded.clone();
                tokio::spawn(async move {
                    let mut buffer = [0u8; 2048];
                    let read = socket.read(&mut buffer).await.unwrap_or(0);
                    let request = String::from_utf8_lossy(&buffer[..read]).to_string();
                    let path = request
                        .split_whitespace()
                        .nth(1)
                        .unwrap_or("/")
                        .split('?')
                        .next()
                        .unwrap_or("/")
                        .to_string();
                    recorded.lock().unwrap().push(path);

                    tokio::time::sleep(delay).await;

                    let body = r#"{"data":[],"total":7}"#;
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = socket.write_all(response.as_bytes()).await;
                    let _ = socket.flush().await;
                });
            }
        });

        (base, seen)
    }

    fn backend_for(base: String) -> TwitchBackend {
        let mut backend =
            TwitchBackend::new(reqwest::Client::new(), "client".into(), "token".into());
        backend.base = base;
        backend.broadcaster_id = Some("123".into());
        backend.login = Some("someone".into());
        backend
    }

    /// The stream status, the follower total and the subscriber total are three
    /// independent requests. Awaited one after another they cost three round
    /// trips on every single poll; started together they cost one.
    #[tokio::test]
    async fn a_stats_poll_makes_its_three_requests_at_the_same_time() {
        let delay = StdDuration::from_millis(200);
        let (base, seen) = fake_helix(delay).await;
        let mut backend = backend_for(base);

        let started = std::time::Instant::now();
        backend
            .fetch_stats()
            .await
            .expect("the fake server always answers");
        let elapsed = started.elapsed();

        let paths = seen.lock().unwrap().clone();
        assert_eq!(paths.len(), 3, "expected three requests, got {paths:?}");
        assert!(
            elapsed < delay * 2,
            "the three requests took {elapsed:?}, which means they were still serialised"
        );
    }

    /// Follower and subscriber totals change on a scale of hours, but statistics
    /// are polled every 15 seconds. Re-fetching them each time spent two
    /// requests per poll to learn nothing.
    #[tokio::test]
    async fn repeat_polls_reuse_the_follower_and_subscriber_totals() {
        let (base, seen) = fake_helix(StdDuration::from_millis(0)).await;
        let mut backend = backend_for(base);

        for _ in 0..5 {
            let stats = backend
                .fetch_stats()
                .await
                .expect("the fake server always answers");
            // The cached values are still reported, not dropped from the panel.
            assert!(stats.extra.iter().any(|s| s.label == "Followers"));
            assert!(stats.extra.iter().any(|s| s.label == "Subscribers"));
        }

        let paths = seen.lock().unwrap().clone();
        let audience = paths
            .iter()
            .filter(|p| p.contains("followers") || p.contains("subscriptions"))
            .count();
        assert_eq!(
            audience, 2,
            "five polls should fetch the totals once: {paths:?}"
        );
        // The live/viewer status is genuinely time-sensitive and is still asked
        // for on every poll.
        assert_eq!(paths.iter().filter(|p| p.ends_with("/streams")).count(), 5);
    }

    /// Only a deliberate refusal deserves to be remembered. A timeout, a 429 or
    /// an expired token used to be cached as "no followers, no subscribers" for
    /// the entire refresh window, so one unlucky request blanked both rows for
    /// minutes even though the very next call would have worked.
    #[test]
    fn a_transient_lookup_failure_is_not_cached() {
        let (followers, subs, settled) = resolve_audience(
            Some((Some(500), Some(20))),
            AudienceProbe::Unavailable,
            AudienceProbe::Count(21),
        );
        assert_eq!(followers, Some(500), "the last known total must survive");
        assert_eq!(subs, Some(21));
        assert!(!settled, "an unavailable lookup must not refresh the cache");
    }

    /// With nothing cached yet there is no previous value to fall back on, but
    /// the failure still must not be written down as an answer.
    #[test]
    fn a_transient_failure_with_no_previous_value_leaves_the_cache_empty() {
        let (followers, subs, settled) =
            resolve_audience(None, AudienceProbe::Unavailable, AudienceProbe::Unavailable);
        assert_eq!((followers, subs), (None, None));
        assert!(!settled);
    }

    /// The case the negative cache exists for: a non-affiliate channel is told
    /// "no" for subscriptions, and asking again in 15 seconds cannot change it.
    #[test]
    fn a_deliberate_refusal_is_cached_as_no_answer() {
        let (followers, subs, settled) = resolve_audience(
            Some((Some(500), Some(20))),
            AudienceProbe::Count(501),
            AudienceProbe::Refused,
        );
        assert_eq!(followers, Some(501));
        assert_eq!(subs, None, "a refusal must clear the stale subscriber total");
        assert!(settled);
    }

    /// The status code decides which of the two kinds of failure this is.
    #[tokio::test]
    async fn status_codes_are_split_into_refusals_and_temporary_failures() {
        for status in [403u16, 404] {
            let backend = backend_for(helix_answering(status, "{}").await);
            assert_eq!(
                backend.follower_count().await,
                AudienceProbe::Refused,
                "HTTP {status} is a settled no"
            );
        }
        for status in [401u16, 429, 500, 503] {
            let backend = backend_for(helix_answering(status, "{}").await);
            assert_eq!(
                backend.follower_count().await,
                AudienceProbe::Unavailable,
                "HTTP {status} can succeed on the next poll"
            );
        }
        let backend = backend_for(helix_answering(200, r#"{"data":[],"total":9}"#).await);
        assert_eq!(backend.follower_count().await, AudienceProbe::Count(9));
    }

    /// A Helix stand-in that answers every request with one fixed status and
    /// body, for exercising the failure classification.
    async fn helix_answering(status: u16, body: &'static str) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let mut buffer = [0u8; 2048];
                    let _ = socket.read(&mut buffer).await;
                    let response = format!(
                        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = socket.write_all(response.as_bytes()).await;
                    let _ = socket.flush().await;
                });
            }
        });
        base
    }
}
