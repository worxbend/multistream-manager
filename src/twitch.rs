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

use crate::backend::{Backend, BoxFuture};
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
}

impl TwitchBackend {
    pub fn new(http: reqwest::Client, client_id: String, access_token: String) -> Self {
        Self {
            http,
            client_id,
            access_token,
            broadcaster_id: None,
            login: None,
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
            "{HELIX}/search/categories?query={}&first=25",
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
        let url = format!("{HELIX}/streams/key?broadcaster_id={id}");
        let response = self.request(reqwest::Method::GET, &url).send().await.ok()?;
        if !response.status().is_success() {
            tracing::warn!(status = ?response.status(), "could not read the Twitch stream key");
            return None;
        }
        let body: DataList<StreamKey> = response.json().await.ok()?;
        body.data.into_iter().next().map(|k| k.stream_key)
    }

    /// Total follower count. Best-effort, same reasoning as the stream key.
    async fn follower_count(&self) -> Option<u64> {
        let id = self.broadcaster_id().ok()?;
        // `first=1` because we only want the `total` field, not the actual list.
        let url = format!("{HELIX}/channels/followers?broadcaster_id={id}&first=1");
        let response = self.request(reqwest::Method::GET, &url).send().await.ok()?;
        if !response.status().is_success() {
            return None;
        }
        let body: TotalList = response.json().await.ok()?;
        body.total
    }

    /// Total subscriber count. Non-affiliate channels get a 403 here, which is
    /// expected and simply means the row is omitted from the stats panel.
    async fn subscriber_count(&self) -> Option<u64> {
        let id = self.broadcaster_id().ok()?;
        let url = format!("{HELIX}/subscriptions?broadcaster_id={id}&first=1");
        let response = self.request(reqwest::Method::GET, &url).send().await.ok()?;
        if !response.status().is_success() {
            return None;
        }
        let body: TotalList = response.json().await.ok()?;
        body.total
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

        let url = format!("{HELIX}/channels?broadcaster_id={id}");
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
            let url = format!("{HELIX}/streams?user_id={id}");
            let response = self
                .request(reqwest::Method::GET, &url)
                .send()
                .await
                .context("asking Twitch whether you are live")?;
            let response = check(response, "reading your Twitch stream status").await?;

            let body: DataList<StreamInfo> = response
                .json()
                .await
                .context("parsing the Twitch stream status")?;

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

            if let Some(followers) = self.follower_count().await {
                stats.extra.push(Stat {
                    label: "Followers".into(),
                    value: format_count(followers),
                });
            }
            if let Some(subs) = self.subscriber_count().await {
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
}
