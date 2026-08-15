//! The YouTube backend, built on the YouTube Data API v3.
//!
//! ## Why this is four API calls and not one
//!
//! YouTube models a live stream as two separate objects that have to be joined:
//!
//! * A **broadcast** (`liveBroadcasts`) is the *event* — the thing with a title,
//!   a description, a scheduled start time and a watch page. It is also a video,
//!   and it gets a video id.
//! * A **stream** (`liveStreams`) is the *pipe* — the RTMP endpoint and stream
//!   key that OBS actually pushes bytes into.
//!
//! Going live therefore means:
//!
//! 1. Find (or create) a stream, so we have somewhere for OBS to push to.
//! 2. Create a broadcast with the title, description and privacy setting.
//! 3. **Bind** the two together, so the broadcast knows which pipe feeds it.
//! 4. Update the underlying video to set tags, category and language — because,
//!    annoyingly, `liveBroadcasts.insert` has no fields for any of those.
//!
//! ## The stream key trap, and why `reuse_stream` defaults to on
//!
//! Step 1 is the one that matters for your setup. Every time you call
//! `liveStreams.insert` YouTube mints a **brand new stream key**. If this
//! backend created a new stream for every broadcast, you would have to paste a
//! fresh key into OBS (or into the Aitum multistream plugin) before every single
//! session — which is precisely the manual work you are trying to get rid of.
//!
//! So by default this looks for an existing *reusable* stream on your channel
//! and binds to that instead. Your key never changes, and OBS keeps working
//! untouched forever.
//!
//! ## A warning about `videos.update`
//!
//! The `videos.update` endpoint replaces **every mutable field in the parts you
//! name**. Sending `part=snippet` with only `tags` set does not merge the tags
//! in — it wipes the title, description and category, because they were absent.
//! `apply_video_metadata` therefore always sends the complete snippet.

use anyhow::{anyhow, bail, Context, Result};
use chrono::{Duration, Utc};
use serde::Deserialize;
use std::time::{Duration as StdDuration, Instant};

use crate::backend::{Backend, BoxFuture, AUDIENCE_REFRESH};
use crate::model::{
    Category, GoLiveOutcome, IngestEndpoint, PlatformStats, StaleBroadcast, Stat, StreamPlan,
};

const API: &str = "https://www.googleapis.com/youtube/v3";

/// The YouTube API client.
pub struct YouTubeBackend {
    http: reqwest::Client,
    access_token: String,
    /// Whether to bind to an existing reusable stream rather than making one.
    reuse_stream: bool,
    /// Pin a particular reusable stream by id. Empty means "pick the first".
    preferred_stream_id: String,
    /// The broadcast created by `go_live`, remembered so stats can be polled.
    broadcast_id: Option<String>,
    /// The channel id, resolved during `connect`.
    channel_id: Option<String>,
    /// The subscriber count, cached so statistics polls do not re-fetch the
    /// whole channel every few seconds. Subscriber totals change on a scale of
    /// hours, but every `channels.list` call spends daily API quota, so at a
    /// 15-second poll interval the uncached version was the single biggest
    /// quota consumer in the program.
    ///
    /// The inner `Option` records "the channel was asked and had no visible
    /// subscriber count" (subscriber totals can be hidden in channel
    /// settings). Caching that absence matters: if only present counts were
    /// cached, a hidden-count channel would re-fetch on every single poll —
    /// exactly the quota burn the cache exists to remove.
    subscriber_cache: Option<(Instant, Option<String>)>,
    /// YouTube's video category list, fetched once and then filtered locally.
    ///
    /// The list is fixed for a region and it powers a type-ahead field, so
    /// fetching it per keystroke meant one `videoCategories.list` request per
    /// character typed — every one of them spending daily quota to return the
    /// exact same answer.
    category_cache: Option<Vec<Category>>,
    /// Stops statistics polling from hammering the API after it has already
    /// said the daily quota is gone. See [`QuotaBackoff`].
    quota: QuotaBackoff,
}

/// Backoff for statistics polling after the YouTube API reports its quota is
/// exhausted.
///
/// The quota is a *daily* budget that resets at midnight Pacific time, so once
/// it is gone, retrying every poll interval cannot succeed — it only burns
/// network traffic and fills the log with the same error. Instead the first
/// quota error pauses polling for five minutes, and each further quota error
/// doubles the pause up to an hour. One successful poll resets the whole
/// state, so recovery after the daily reset is automatic.
struct QuotaBackoff {
    /// While set and in the future, statistics polls return an explanatory
    /// error without touching the network.
    paused_until: Option<Instant>,
    /// The pause the *next* quota error will apply.
    next_pause: StdDuration,
}

const QUOTA_PAUSE_INITIAL: StdDuration = StdDuration::from_secs(5 * 60);
const QUOTA_PAUSE_MAX: StdDuration = StdDuration::from_secs(60 * 60);

impl Default for QuotaBackoff {
    fn default() -> Self {
        Self {
            paused_until: None,
            next_pause: QUOTA_PAUSE_INITIAL,
        }
    }
}

impl QuotaBackoff {
    /// How much longer polling should stay paused, or `None` if it may run.
    fn remaining(&self, now: Instant) -> Option<StdDuration> {
        let until = self.paused_until?;
        if now < until {
            Some(until - now)
        } else {
            None
        }
    }

    /// Record a quota error: pause polling and double the next pause.
    /// Returns the pause that was just applied, for the error message.
    fn on_quota_error(&mut self, now: Instant) -> StdDuration {
        let pause = self.next_pause;
        self.paused_until = Some(now + pause);
        self.next_pause = (pause * 2).min(QUOTA_PAUSE_MAX);
        pause
    }

    /// Record a successful call: the quota is evidently back, forget it all.
    fn on_success(&mut self) {
        *self = Self::default();
    }
}

/// Rounds a duration up to whole minutes for a human-facing message.
fn human_minutes(duration: StdDuration) -> String {
    format!("{} min", duration.as_secs().div_ceil(60).max(1))
}

/// Marker attached to errors caused by an exhausted API quota, so callers can
/// tell "out of quota" apart from every other failure without parsing message
/// text. Retrieved with `error.downcast_ref::<QuotaExceeded>()`, which walks
/// the whole anyhow context chain.
#[derive(Debug)]
struct QuotaExceeded;

impl std::fmt::Display for QuotaExceeded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the YouTube API quota is exhausted")
    }
}

impl std::error::Error for QuotaExceeded {}

impl YouTubeBackend {
    pub fn new(
        http: reqwest::Client,
        access_token: String,
        reuse_stream: bool,
        preferred_stream_id: String,
    ) -> Self {
        Self {
            http,
            access_token,
            reuse_stream,
            preferred_stream_id,
            broadcast_id: None,
            channel_id: None,
            subscriber_cache: None,
            category_cache: None,
            quota: QuotaBackoff::default(),
        }
    }

    fn request(&self, method: reqwest::Method, url: &str) -> reqwest::RequestBuilder {
        self.http
            .request(method, url)
            .bearer_auth(&self.access_token)
    }

    /// Fetch the authenticated user's own channel.
    async fn my_channel(&self) -> Result<ChannelResource> {
        let url = format!("{API}/channels?part=snippet,statistics&mine=true");
        let response = self
            .request(reqwest::Method::GET, &url)
            .send()
            .await
            .context("asking YouTube which channel your login belongs to")?;
        let response = check(response, "reading your YouTube channel").await?;

        let body: ListResponse<ChannelResource> = response
            .json()
            .await
            .context("parsing your YouTube channel information")?;

        body.items.into_iter().next().ok_or_else(|| {
            anyhow!(
                "your Google account has no YouTube channel. Open youtube.com, create a \
                 channel, then run `msm login youtube` again."
            )
        })
    }

    /// Find an existing stream key to reuse, or create one if there is none.
    ///
    /// Returns the stream plus a note explaining which branch was taken, because
    /// "your stream key has not changed" is genuinely useful reassurance and
    /// "here is your new key, put it in OBS" is genuinely important news.
    ///
    /// ## Why reusability is not checked here
    ///
    /// A stream's `contentDetails.isReusable` flag would be the obvious thing to
    /// filter on — but `liveStreams.list` does not support the `contentDetails`
    /// part at all. Its documented part values are only `id`, `snippet`, `cdn`
    /// and `status`, so that flag is simply not obtainable from a list call, and
    /// asking for it makes the request invalid.
    ///
    /// So the candidate is chosen without it and the *bind* is what validates
    /// the choice: binding a non-reusable stream fails, and `go_live` falls back
    /// to creating a fresh stream at that point. That is strictly better than
    /// guessing, because it is the API itself that decides.
    async fn obtain_stream(&self) -> Result<(LiveStreamResource, String)> {
        if self.reuse_stream {
            let existing = self.list_streams().await?;

            // Honour an explicitly pinned id first.
            if !self.preferred_stream_id.is_empty() {
                match existing
                    .iter()
                    .find(|s| s.id == self.preferred_stream_id)
                    .cloned()
                {
                    Some(stream) => {
                        let note = format!(
                            "Bound to your pinned stream key \"{}\" — your OBS settings are unchanged.",
                            stream.snippet.title
                        );
                        return Ok((stream, note));
                    }
                    None => bail!(
                        "the stream id {:?} set as `stream_id` in your config does not exist on \
                         your channel. Remove that setting to let the application pick one \
                         automatically.",
                        self.preferred_stream_id
                    ),
                }
            }

            if let Some(stream) = existing.into_iter().next() {
                let note = format!(
                    "Reused your existing stream key \"{}\" — OBS and Aitum need no changes.",
                    stream.snippet.title
                );
                return Ok((stream, note));
            }
        }

        // Nothing to reuse: make one.
        let created = self.create_stream().await?;
        let note = "No reusable stream key existed, so a new one was created. \
                    Copy the stream key below into OBS (or Aitum) once — it will be \
                    reused automatically from now on."
            .to_string();
        Ok((created, note))
    }

    /// List the streams already configured on the channel.
    ///
    /// `contentDetails` is deliberately absent from the requested parts: it is
    /// not a supported part value for this method, and including it makes the
    /// whole request invalid.
    async fn list_streams(&self) -> Result<Vec<LiveStreamResource>> {
        let url = format!("{API}/liveStreams?part=id,snippet,cdn,status&mine=true&maxResults=50");
        let response = self
            .request(reqwest::Method::GET, &url)
            .send()
            .await
            .context("listing your YouTube stream keys")?;
        let response = check(response, "listing your YouTube stream keys").await?;

        let body: ListResponse<LiveStreamResource> = response
            .json()
            .await
            .context("parsing your YouTube stream key list")?;
        Ok(body.items)
    }

    /// Create a new reusable RTMP stream.
    async fn create_stream(&self) -> Result<LiveStreamResource> {
        let body = serde_json::json!({
            "snippet": { "title": "multistream-manager (reusable)" },
            "cdn": {
                "ingestionType": "rtmp",
                // "variable" lets OBS send whatever it is configured for rather
                // than forcing a specific resolution and frame rate here.
                "resolution": "variable",
                "frameRate": "variable"
            },
            // The whole point: a reusable stream keeps the same key forever.
            "contentDetails": { "isReusable": true }
        });

        let url = format!("{API}/liveStreams?part=id,snippet,cdn,contentDetails");
        let response = self
            .request(reqwest::Method::POST, &url)
            .json(&body)
            .send()
            .await
            .context("creating a YouTube stream key")?;
        let response = check(response, "creating a YouTube stream key").await?;

        response
            .json()
            .await
            .context("parsing the newly created YouTube stream")
    }

    /// Create the broadcast — the event with a title and a watch page.
    async fn create_broadcast(&self, plan: &StreamPlan) -> Result<LiveBroadcastResource> {
        // YouTube requires a scheduled start time and rejects one in the past.
        // A minute into the future is far enough to survive clock skew between
        // this machine and Google's servers.
        let start = Utc::now() + Duration::minutes(1);

        let body = serde_json::json!({
            "snippet": {
                "title": plan.youtube_title(),
                "description": crate::model::truncate_chars(
                    &plan.description,
                    crate::model::limits::YOUTUBE_DESCRIPTION,
                ),
                "scheduledStartTime": start.to_rfc3339(),
            },
            "status": {
                "privacyStatus": plan.privacy.api_value(),
                // YouTube legally requires this declaration on every upload.
                "selfDeclaredMadeForKids": plan.made_for_kids,
            },
            "contentDetails": {
                // With auto-start on, YouTube flips the broadcast live the
                // moment it sees OBS's feed, so there is no button to press.
                "enableAutoStart": plan.youtube_auto_start,
                "enableAutoStop": plan.youtube_auto_stop,
                "enableDvr": true,
                "recordFromStart": true,
                // The monitor stream is YouTube Studio's preview pane. Turning
                // it off removes a few seconds of delay before viewers see you.
                "monitorStream": { "enableMonitorStream": false },
            }
        });

        let url = format!("{API}/liveBroadcasts?part=id,snippet,status,contentDetails");
        let response = self
            .request(reqwest::Method::POST, &url)
            .json(&body)
            .send()
            .await
            .context("creating the YouTube broadcast")?;
        let response = check(response, "creating the YouTube broadcast").await?;

        response
            .json()
            .await
            .context("parsing the newly created YouTube broadcast")
    }

    /// Join the broadcast to the stream, so the RTMP feed reaches the event.
    async fn bind(&self, broadcast_id: &str, stream_id: &str) -> Result<()> {
        let url = format!(
            "{API}/liveBroadcasts/bind?id={}&streamId={}&part=id,contentDetails",
            urlencoding::encode(broadcast_id),
            urlencoding::encode(stream_id)
        );
        let response = self
            .request(reqwest::Method::POST, &url)
            .send()
            .await
            .context("binding the YouTube broadcast to your stream key")?;
        check(response, "binding the YouTube broadcast to your stream key").await?;
        Ok(())
    }

    /// Set the tags, category and language on the broadcast's underlying video.
    ///
    /// A broadcast *is* a video, so this uses the ordinary `videos.update`
    /// endpoint with the broadcast id. Every snippet field is sent every time —
    /// see the warning in this module's documentation for why that is essential.
    async fn apply_video_metadata(&self, video_id: &str, plan: &StreamPlan) -> Result<()> {
        let mut snippet = serde_json::json!({
            "title": plan.youtube_title(),
            "categoryId": plan.youtube_category_id,
            "description": crate::model::truncate_chars(
                &plan.description,
                crate::model::limits::YOUTUBE_DESCRIPTION,
            ),
            "tags": plan.youtube_tags(),
        });

        // Only send a language if it looks like a valid code; an invalid one
        // makes YouTube reject the entire update.
        // Only `defaultLanguage` is writable here. `defaultAudioLanguage` is a
        // read-only property of the videos resource, so setting it would be
        // silently dropped rather than applied.
        if plan.language.chars().count() == 2 {
            snippet["defaultLanguage"] = serde_json::Value::String(plan.language.clone());
        }

        let body = serde_json::json!({ "id": video_id, "snippet": snippet });

        let url = format!("{API}/videos?part=snippet");
        let response = self
            .request(reqwest::Method::PUT, &url)
            .json(&body)
            .send()
            .await
            .context("setting the tags and category on your YouTube broadcast")?;
        check(
            response,
            "setting the tags and category on your YouTube broadcast",
        )
        .await?;
        Ok(())
    }

    /// List every broadcast on the channel, following YouTube's pagination.
    ///
    /// `liveBroadcasts.list` returns 50 at a time. A channel that has been
    /// resubmitted to over many sessions can hold more than one page of the
    /// leftovers `msm cleanup` is looking for, and a command that offered to
    /// tidy up but only ever saw the first fifty would be worse than useless.
    ///
    /// The page count is capped because each page costs API quota, and because
    /// an unexpected reply that kept handing back a page token would otherwise
    /// spin here forever.
    async fn list_broadcasts(&self) -> Result<Vec<LiveBroadcastResource>> {
        const MAX_PAGES: usize = 10;

        let mut out = Vec::new();
        let mut page_token: Option<String> = None;

        for _ in 0..MAX_PAGES {
            let mut url =
                format!("{API}/liveBroadcasts?part=id,snippet,status&mine=true&maxResults=50");
            if let Some(token) = &page_token {
                url.push_str(&format!("&pageToken={}", urlencoding::encode(token)));
            }

            let response = self
                .request(reqwest::Method::GET, &url)
                .send()
                .await
                .context("listing your YouTube broadcasts")?;
            let response = check(response, "listing your YouTube broadcasts").await?;

            let body: ListResponse<LiveBroadcastResource> = response
                .json()
                .await
                .context("parsing your YouTube broadcast list")?;

            out.extend(body.items);

            match body.next_page_token {
                Some(token) if !token.is_empty() => page_token = Some(token),
                _ => break,
            }
        }

        Ok(out)
    }

    /// Delete one broadcast from the channel.
    async fn delete_broadcast_by_id(&self, id: &str) -> Result<()> {
        let url = format!("{API}/liveBroadcasts?id={}", urlencoding::encode(id));
        let response = self
            .request(reqwest::Method::DELETE, &url)
            .send()
            .await
            .context("deleting a YouTube broadcast")?;
        check(response, "deleting a YouTube broadcast").await?;
        Ok(())
    }

    /// The video category list, fetched from the API only the first time.
    ///
    /// The autocomplete calls this on every keystroke. The list does not change
    /// during a session, so after the first call this is a borrow of memory and
    /// costs nothing — which is the difference between one API request per login
    /// and one per character typed.
    async fn cached_video_categories(&mut self, region: &str) -> Result<&[Category]> {
        if self.category_cache.is_none() {
            self.category_cache = Some(self.video_categories(region).await?);
        }
        Ok(self
            .category_cache
            .as_deref()
            .expect("just populated above if it was empty"))
    }

    /// List YouTube's video categories, used for the category autocomplete.
    ///
    /// Unlike Twitch there is no search endpoint — the list is short and fixed,
    /// so it is fetched whole and filtered locally.
    async fn video_categories(&self, region: &str) -> Result<Vec<Category>> {
        let url = format!("{API}/videoCategories?part=snippet&regionCode={region}");
        let response = self
            .request(reqwest::Method::GET, &url)
            .send()
            .await
            .context("listing YouTube video categories")?;
        let response = check(response, "listing YouTube video categories").await?;

        let body: ListResponse<VideoCategoryResource> = response
            .json()
            .await
            .context("parsing the YouTube category list")?;

        Ok(body
            .items
            .into_iter()
            // Some categories are read-only historical entries that cannot be
            // assigned to a new video; offering them would only cause failures.
            .filter(|c| c.snippet.assignable)
            .map(|c| Category {
                id: c.id,
                name: c.snippet.title,
            })
            .collect())
    }
}

impl Backend for YouTubeBackend {
    fn connect(&mut self) -> BoxFuture<'_, Result<String>> {
        Box::pin(async move {
            let channel = self.my_channel().await?;
            self.channel_id = Some(channel.id.clone());
            // Seed the subscriber cache from this fetch, so the first stats
            // poll does not need its own channel request. A missing count
            // (hidden in channel settings) is cached too, as "asked, none".
            let subs = channel
                .statistics
                .as_ref()
                .and_then(|s| s.subscriber_count.clone());
            self.subscriber_cache = Some((Instant::now(), subs));
            Ok(channel.snippet.title)
        })
    }

    fn go_live<'a>(&'a mut self, plan: &'a StreamPlan) -> BoxFuture<'a, Result<GoLiveOutcome>> {
        Box::pin(async move {
            let mut notes = Vec::new();

            // 1. Somewhere for OBS to push to.
            let (stream, stream_note) = self.obtain_stream().await?;
            notes.push(stream_note);

            // 2. The event itself.
            let broadcast = self.create_broadcast(plan).await?;

            // 3. Join them.
            //
            // If the reused stream cannot be bound — the usual reason is that it
            // belongs to a single past broadcast and is not reusable — fall back
            // to a fresh one rather than failing the go-live outright. The user
            // is told, because a new key is something they must act on.
            let stream = match self.bind(&broadcast.id, &stream.id).await {
                Ok(()) => stream,
                Err(err) => {
                    tracing::warn!(?err, "could not bind the reused stream; creating a new one");
                    notes.pop();
                    let fresh = self.create_stream().await?;
                    self.bind(&broadcast.id, &fresh.id).await?;
                    notes.push(
                        "Your existing stream key could not be reused, so a new one was \
                         created. Copy the stream key below into OBS (or Aitum) — it will \
                         be reused from now on."
                            .to_string(),
                    );
                    fresh
                }
            };

            // 4. Tags, category and language, which step 2 could not set.
            //
            // A failure here is deliberately not fatal: the broadcast exists and
            // is bound, so you can still stream. Losing the tags is a far better
            // outcome than aborting a go-live that has already half happened.
            match self.apply_video_metadata(&broadcast.id, plan).await {
                Ok(()) => notes.push("Tags, category and title language applied.".to_string()),
                Err(err) => notes.push(format!(
                    "Warning: the broadcast is ready, but its tags and category could not be \
                     set ({err}). You can still stream; fix them in YouTube Studio if it matters."
                )),
            }

            if plan.youtube_auto_start {
                notes.push(
                    "Auto-start is on: YouTube will go live by itself as soon as it sees the \
                     feed from OBS."
                        .to_string(),
                );
            } else {
                notes.push(
                    "Auto-start is off: after OBS connects you must press \"Go live\" in \
                     YouTube Studio."
                        .to_string(),
                );
            }

            self.broadcast_id = Some(broadcast.id.clone());

            let ingestion = stream.cdn.and_then(|cdn| cdn.ingestion_info);

            Ok(GoLiveOutcome {
                watch_url: Some(format!("https://youtube.com/watch?v={}", broadcast.id)),
                manage_url: Some(format!(
                    "https://studio.youtube.com/video/{}/livestreaming",
                    broadcast.id
                )),
                ingest_url: ingestion.as_ref().map(|i| i.ingestion_address.clone()),
                stream_key: ingestion.as_ref().map(|i| i.stream_name.clone()),
                notes,
            })
        })
    }

    fn fetch_stats(&mut self) -> BoxFuture<'_, Result<PlatformStats>> {
        Box::pin(async move {
            let Some(broadcast_id) = self.broadcast_id.clone() else {
                // Nothing has been created yet this session, so there is
                // genuinely nothing to report. Not an error.
                return Ok(PlatformStats::default());
            };

            // If a recent poll already learned the daily quota is gone, do not
            // spend network traffic confirming it again — it resets at
            // midnight Pacific time, not in the next fifteen seconds.
            if let Some(left) = self.quota.remaining(Instant::now()) {
                bail!(
                    "YouTube statistics are paused for another {} because the API said \
                     its daily quota is exhausted. Polling resumes automatically.",
                    human_minutes(left)
                );
            }

            let url = format!(
                "{API}/videos?part=liveStreamingDetails,statistics,status&id={}",
                urlencoding::encode(&broadcast_id)
            );
            let response = self
                .request(reqwest::Method::GET, &url)
                .send()
                .await
                .context("reading your YouTube broadcast statistics")?;
            let response = match check(response, "reading your YouTube broadcast statistics").await
            {
                Ok(response) => {
                    self.quota.on_success();
                    response
                }
                Err(err) => {
                    if err.downcast_ref::<QuotaExceeded>().is_some() {
                        let pause = self.quota.on_quota_error(Instant::now());
                        return Err(err.context(format!(
                            "pausing statistics polls for {} — retrying sooner cannot \
                             succeed, it only spends more of tomorrow's patience",
                            human_minutes(pause)
                        )));
                    }
                    return Err(err);
                }
            };

            let body: ListResponse<VideoResource> = response
                .json()
                .await
                .context("parsing your YouTube broadcast statistics")?;

            let mut stats = PlatformStats::default();

            let Some(video) = body.items.into_iter().next() else {
                return Ok(stats);
            };

            if let Some(details) = video.live_streaming_details {
                // `concurrentViewers` is only present while actually live, which
                // makes it a reliable liveness signal in itself.
                stats.viewers = details
                    .concurrent_viewers
                    .as_deref()
                    .and_then(|v| v.parse().ok());
                stats.started_at = details.actual_start_time;
                stats.live =
                    details.actual_start_time.is_some() && details.actual_end_time.is_none();
            }

            if let Some(statistics) = video.statistics {
                if let Some(likes) = statistics.like_count.as_deref() {
                    stats.extra.push(Stat {
                        label: "Likes".into(),
                        value: likes.to_string(),
                    });
                }
                if let Some(views) = statistics.view_count.as_deref() {
                    stats.extra.push(Stat {
                        label: "Total views".into(),
                        value: views.to_string(),
                    });
                }
            }

            // Subscriber count comes from the channel, not the video — and a
            // `channels.list` call costs quota, so the cached value is used
            // until it is AUDIENCE_REFRESH old. A stale-by-ten-minutes
            // subscriber total is invisible; the quota it saves is not.
            let now = Instant::now();
            let cache_is_fresh = self
                .subscriber_cache
                .as_ref()
                .is_some_and(|(fetched_at, _)| now.duration_since(*fetched_at) < AUDIENCE_REFRESH);
            if !cache_is_fresh {
                match self.my_channel().await {
                    // Stamp the cache even when the count is absent (hidden
                    // subscriber counts are a channel setting), otherwise a
                    // hidden count would mean one channels.list call per poll.
                    Ok(channel) => {
                        let subs = channel.statistics.and_then(|s| s.subscriber_count);
                        self.subscriber_cache = Some((now, subs));
                    }
                    // Not worth failing the whole stats row over; the viewer
                    // count above is the number people actually watch. But do
                    // say why the subscriber figure stopped updating.
                    Err(err) => {
                        tracing::warn!("could not refresh the subscriber count: {err:#}");
                        if err.downcast_ref::<QuotaExceeded>().is_some() {
                            self.quota.on_quota_error(now);
                        }
                    }
                }
            }
            if let Some((_, Some(subs))) = &self.subscriber_cache {
                stats.extra.push(Stat {
                    label: "Subscribers".into(),
                    value: subs.clone(),
                });
            }

            Ok(stats)
        })
    }

    fn set_access_token(&mut self, token: String) {
        self.access_token = token;
    }

    fn search_categories<'a>(&'a mut self, query: &'a str) -> BoxFuture<'a, Result<Vec<Category>>> {
        Box::pin(async move {
            let all = self.cached_video_categories("US").await?;
            let needle = query.trim().to_lowercase();
            if needle.is_empty() {
                return Ok(all.to_vec());
            }
            Ok(all
                .iter()
                .filter(|c| c.name.to_lowercase().contains(&needle))
                .cloned()
                .collect())
        })
    }

    fn list_ingest_endpoints(&mut self) -> BoxFuture<'_, Result<Vec<IngestEndpoint>>> {
        Box::pin(async move {
            Ok(self
                .list_streams()
                .await?
                .into_iter()
                .map(|stream| IngestEndpoint {
                    id: stream.id,
                    title: stream.snippet.title,
                    key: stream
                        .cdn
                        .and_then(|cdn| cdn.ingestion_info)
                        .map(|info| info.stream_name),
                })
                .collect())
        })
    }

    fn list_stale_broadcasts(&mut self) -> BoxFuture<'_, Result<Vec<StaleBroadcast>>> {
        Box::pin(async move {
            Ok(self
                .list_broadcasts()
                .await?
                .into_iter()
                .filter(never_went_live)
                .map(|broadcast| {
                    let snippet = broadcast.snippet.unwrap_or_default();
                    StaleBroadcast {
                        id: broadcast.id,
                        title: snippet.title,
                        scheduled_start: snippet.scheduled_start_time,
                        status: broadcast
                            .status
                            .map(|status| status.life_cycle_status)
                            .unwrap_or_default(),
                    }
                })
                .collect())
        })
    }

    fn delete_broadcast<'a>(&'a mut self, id: &'a str) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move { self.delete_broadcast_by_id(id).await })
    }
}

/// Whether a listed broadcast was created but never received a video feed.
///
/// This is the safety rule behind `msm cleanup`, so it is deliberately
/// pessimistic: deleting a broadcast that holds a recording somebody wanted
/// cannot be undone, whereas leaving an orphan behind costs nothing but clutter.
///
/// A broadcast qualifies only when **both** of these hold:
///
/// * YouTube reports neither an `actualStartTime` nor an `actualEndTime`. Either
///   of those means a feed reached it at some point, so it has been live.
/// * Its `lifeCycleStatus` is still one of the two pre-live values, `created`
///   (made but not yet bound and validated) or `ready` (bound and waiting).
///   Everything else — `live`, `testing`, `complete`, `revoked` and the
///   transitional `liveStarting` / `testStarting` — is left alone.
///
/// A broadcast whose `snippet` or `status` is missing from the reply is treated
/// as *not* stale, because there is then no evidence either way and the safe
/// answer is to keep it.
fn never_went_live(broadcast: &LiveBroadcastResource) -> bool {
    let Some(snippet) = broadcast.snippet.as_ref() else {
        return false;
    };
    if snippet.actual_start_time.is_some() || snippet.actual_end_time.is_some() {
        return false;
    }

    let status = broadcast
        .status
        .as_ref()
        .map(|status| status.life_cycle_status.as_str())
        .unwrap_or("");

    matches!(status, "created" | "ready")
}

/// Turn a non-2xx response into an error carrying Google's own explanation.
async fn check(response: reqwest::Response, action: &str) -> Result<reqwest::Response> {
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }

    let body = response.text().await.unwrap_or_default();
    let parsed = serde_json::from_str::<serde_json::Value>(&body).ok();

    let message = parsed
        .as_ref()
        .and_then(|json| json.pointer("/error/message").and_then(|m| m.as_str()))
        .unwrap_or("no detail given")
        .to_string();

    // Google puts a machine-readable reason inside the first error detail. It is
    // the difference between "403 Forbidden" and "you are out of quota".
    let reason = parsed
        .as_ref()
        .and_then(|json| {
            json.pointer("/error/errors/0/reason")
                .and_then(|r| r.as_str())
        })
        .unwrap_or("")
        .to_string();

    let hint = match reason.as_str() {
        "quotaExceeded" | "dailyLimitExceeded" => {
            "\n  You have used up today's YouTube API quota. It resets at midnight \
             Pacific time. Raising `poll_interval_secs` in your config makes the \
             quota last longer, since every statistics refresh spends some of it."
        }
        "liveStreamingNotEnabled" => {
            "\n  Live streaming is not enabled on this YouTube channel. Enable it at \
             youtube.com/features — note that first-time activation takes 24 hours."
        }
        "insufficientPermissions" => {
            "\n  The saved token lacks the permission this needs. Run `msm login youtube`."
        }
        "invalidVideoMetadata" | "invalidTags" => {
            "\n  YouTube rejected the metadata. An over-long description or a tag \
             containing a `<` or `>` character is the usual cause."
        }
        "errorStreamInactive" => {
            "\n  YouTube cannot see a video feed yet. Start streaming in OBS first."
        }
        _ => match status.as_u16() {
            401 => "\n  Your YouTube login has expired. Run `msm login youtube`.",
            403 => {
                "\n  YouTube refused this action. Check that live streaming is enabled \
                    on your channel and that you are not out of API quota."
            }
            _ => "",
        },
    };

    let full_message = format!("{action} failed (HTTP {status}): {message}{hint}");

    // Quota exhaustion gets a typed marker in the error chain so callers can
    // recognise it (and back off) without matching on message text.
    if matches!(reason.as_str(), "quotaExceeded" | "dailyLimitExceeded") {
        return Err(anyhow::Error::new(QuotaExceeded).context(full_message));
    }
    bail!("{full_message}")
}

// ---------------------------------------------------------------------------
// Wire types mirroring the YouTube Data API's JSON.
// ---------------------------------------------------------------------------

/// Every list endpoint returns its payload under `items`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListResponse<T> {
    #[serde(default = "Vec::new")]
    items: Vec<T>,
    /// Present when there are more results than fit in one reply. Absent on the
    /// last page, and on endpoints short enough never to need paging at all.
    #[serde(default)]
    next_page_token: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ChannelResource {
    id: String,
    snippet: ChannelSnippet,
    #[serde(default)]
    statistics: Option<ChannelStatistics>,
}

#[derive(Debug, Deserialize)]
struct ChannelSnippet {
    #[serde(default)]
    title: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChannelStatistics {
    /// A string in the JSON because the value can exceed 2^53.
    #[serde(default)]
    subscriber_count: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct LiveStreamResource {
    id: String,
    snippet: StreamSnippet,
    #[serde(default)]
    cdn: Option<Cdn>,
}

#[derive(Debug, Clone, Deserialize)]
struct StreamSnippet {
    #[serde(default)]
    title: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Cdn {
    #[serde(default)]
    ingestion_info: Option<IngestionInfo>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct IngestionInfo {
    /// The RTMP URL, e.g. `rtmp://a.rtmp.youtube.com/live2`.
    #[serde(default)]
    ingestion_address: String,
    /// Confusingly named: this is the stream *key*, not a name.
    #[serde(default)]
    stream_name: String,
}

#[derive(Debug, Deserialize)]
struct LiveBroadcastResource {
    id: String,
    /// Absent when only `part=id` was requested, which is why it is optional.
    #[serde(default)]
    snippet: Option<BroadcastSnippet>,
    #[serde(default)]
    status: Option<BroadcastStatus>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BroadcastSnippet {
    #[serde(default)]
    title: String,
    #[serde(default)]
    scheduled_start_time: Option<chrono::DateTime<chrono::Utc>>,
    /// Set the moment YouTube first sees a feed. Its presence is the definitive
    /// proof that a broadcast has been live, which is what `msm cleanup` uses to
    /// decide what it must never touch.
    #[serde(default)]
    actual_start_time: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default)]
    actual_end_time: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BroadcastStatus {
    /// One of `created`, `ready`, `testStarting`, `testing`, `liveStarting`,
    /// `live`, `complete` or `revoked`.
    #[serde(default)]
    life_cycle_status: String,
}

#[derive(Debug, Deserialize)]
struct VideoResource {
    #[serde(default, rename = "liveStreamingDetails")]
    live_streaming_details: Option<LiveStreamingDetails>,
    #[serde(default)]
    statistics: Option<VideoStatistics>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LiveStreamingDetails {
    #[serde(default)]
    concurrent_viewers: Option<String>,
    #[serde(default)]
    actual_start_time: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default)]
    actual_end_time: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct VideoStatistics {
    #[serde(default)]
    view_count: Option<String>,
    #[serde(default)]
    like_count: Option<String>,
}

#[derive(Debug, Deserialize)]
struct VideoCategoryResource {
    id: String,
    snippet: VideoCategorySnippet,
}

#[derive(Debug, Deserialize)]
struct VideoCategorySnippet {
    #[serde(default)]
    title: String,
    /// `false` for legacy categories that can no longer be set on a video.
    #[serde(default)]
    assignable: bool,
}

/// The handful of YouTube category ids worth offering without an API call.
///
/// Used to label the current selection in the form before the full list has
/// been fetched, so the field never shows a bare number like "20".
pub const COMMON_CATEGORIES: &[(&str, &str)] = &[
    ("20", "Gaming"),
    ("28", "Science & Technology"),
    ("27", "Education"),
    ("24", "Entertainment"),
    ("22", "People & Blogs"),
    ("10", "Music"),
    ("26", "Howto & Style"),
    ("23", "Comedy"),
    ("17", "Sports"),
    ("25", "News & Politics"),
];

/// Filter the built-in category list by name, without touching the network.
///
/// The form's YouTube category field is normally backed by `videoCategories`
/// fetched from the API, but that list is only reachable once you are logged in
/// and the daily quota still has room in it. Before then — which includes the
/// very first run, when nobody has authorised anything yet — typing in that
/// field used to do nothing at all, with no hint as to why.
///
/// So the field falls back to this, exactly the way the language field works:
/// a short list held in the binary, filtered locally, always available. The ten
/// entries here cover the categories a live stream realistically uses, and the
/// full list still replaces them as soon as the API answers.
pub fn search_common(query: &str) -> Vec<Category> {
    let needle = query.trim().to_lowercase();

    COMMON_CATEGORIES
        .iter()
        .filter(|(_, name)| needle.is_empty() || name.to_lowercase().contains(&needle))
        .map(|(id, name)| Category {
            id: (*id).to_string(),
            name: (*name).to_string(),
        })
        .collect()
}

/// Human-readable name for a category id, falling back to the raw id.
pub fn category_name(id: &str) -> String {
    COMMON_CATEGORIES
        .iter()
        .find(|(cid, _)| *cid == id)
        .map(|(_, name)| (*name).to_string())
        .unwrap_or_else(|| format!("category {id}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Privacy;

    #[test]
    fn known_category_ids_get_a_readable_name() {
        assert_eq!(category_name("20"), "Gaming");
        assert_eq!(category_name("28"), "Science & Technology");
    }

    #[test]
    fn an_unknown_category_id_falls_back_to_the_number() {
        assert_eq!(category_name("999"), "category 999");
    }

    #[test]
    fn a_stream_listing_yields_its_ingestion_details() {
        // Note the absence of `contentDetails`: `liveStreams.list` does not
        // support that part, so the reuse decision cannot depend on it.
        let json = r#"{
            "id": "abc",
            "snippet": {"title": "My key"},
            "cdn": {"ingestionInfo": {
                "ingestionAddress": "rtmp://a.rtmp.youtube.com/live2",
                "streamName": "xxxx-yyyy"
            }}
        }"#;
        let stream: LiveStreamResource = serde_json::from_str(json).unwrap();

        let ingestion = stream.cdn.unwrap().ingestion_info.unwrap();
        // `streamName` really is the stream key, despite the name.
        assert_eq!(ingestion.stream_name, "xxxx-yyyy");
        assert_eq!(
            ingestion.ingestion_address,
            "rtmp://a.rtmp.youtube.com/live2"
        );
    }

    #[test]
    fn a_stream_with_no_cdn_block_still_parses() {
        let json = r#"{"id":"a","snippet":{"title":"t"}}"#;
        let stream: LiveStreamResource = serde_json::from_str(json).unwrap();
        assert!(stream.cdn.is_none());
    }

    #[test]
    fn the_stream_listing_url_does_not_request_an_unsupported_part() {
        // `liveStreams.list` supports only id, snippet, cdn and status. Asking
        // for contentDetails makes the whole request invalid, which would send
        // every go-live down the "create a new key" path and hand the user a
        // different stream key on every single broadcast.
        let url = format!("{API}/liveStreams?part=id,snippet,cdn,status&mine=true&maxResults=50");
        assert!(!url.contains("contentDetails"));
        assert!(url.contains("part=id,snippet,cdn,status"));
    }

    #[test]
    fn live_streaming_details_distinguish_running_from_finished() {
        let running = r#"{"liveStreamingDetails":{
            "concurrentViewers":"137",
            "actualStartTime":"2026-08-08T10:00:00Z"
        }}"#;
        let video: VideoResource = serde_json::from_str(running).unwrap();
        let details = video.live_streaming_details.unwrap();
        assert_eq!(details.concurrent_viewers.as_deref(), Some("137"));
        assert!(details.actual_start_time.is_some());
        assert!(details.actual_end_time.is_none());

        let finished = r#"{"liveStreamingDetails":{
            "actualStartTime":"2026-08-08T10:00:00Z",
            "actualEndTime":"2026-08-08T12:00:00Z"
        }}"#;
        let video: VideoResource = serde_json::from_str(finished).unwrap();
        assert!(video
            .live_streaming_details
            .unwrap()
            .actual_end_time
            .is_some());
    }

    #[test]
    fn an_empty_list_response_parses() {
        let parsed: ListResponse<VideoResource> = serde_json::from_str(r#"{"items":[]}"#).unwrap();
        assert!(parsed.items.is_empty());
    }

    #[test]
    fn non_assignable_categories_would_be_filtered_out() {
        let json = r#"{"items":[
            {"id":"20","snippet":{"title":"Gaming","assignable":true}},
            {"id":"18","snippet":{"title":"Short Movies","assignable":false}}
        ]}"#;
        let parsed: ListResponse<VideoCategoryResource> = serde_json::from_str(json).unwrap();
        let assignable: Vec<_> = parsed
            .items
            .into_iter()
            .filter(|c| c.snippet.assignable)
            .collect();
        assert_eq!(assignable.len(), 1);
        assert_eq!(assignable[0].snippet.title, "Gaming");
    }

    #[test]
    fn privacy_maps_to_the_exact_strings_the_api_wants() {
        assert_eq!(Privacy::Public.api_value(), "public");
        assert_eq!(Privacy::Unlisted.api_value(), "unlisted");
        assert_eq!(Privacy::Private.api_value(), "private");
    }

    /// Build a broadcast reply the way YouTube sends one, so the tests below
    /// exercise the real deserialisation rather than a hand-built struct.
    fn broadcast(
        status: &str,
        actual_start: Option<&str>,
        actual_end: Option<&str>,
    ) -> LiveBroadcastResource {
        let start = actual_start
            .map(|at| format!(r#","actualStartTime":"{at}""#))
            .unwrap_or_default();
        let end = actual_end
            .map(|at| format!(r#","actualEndTime":"{at}""#))
            .unwrap_or_default();

        let json = format!(
            r#"{{
                "id": "bid",
                "snippet": {{
                    "title": "A stream",
                    "scheduledStartTime": "2026-08-08T10:00:00Z"{start}{end}
                }},
                "status": {{"lifeCycleStatus": "{status}"}}
            }}"#
        );
        serde_json::from_str(&json).expect("the fixture must be valid JSON")
    }

    #[test]
    fn a_broadcast_that_was_only_created_counts_as_stale() {
        // This is the exact leftover `msm cleanup` exists for: submitting the
        // form again makes a fresh broadcast and abandons the previous one in
        // the "created" state with no feed ever attached to it.
        assert!(never_went_live(&broadcast("created", None, None)));
    }

    #[test]
    fn a_bound_but_unused_broadcast_counts_as_stale() {
        // "ready" means it was bound to a stream key and was waiting for OBS,
        // but OBS never arrived. Nothing was recorded, so it is safe to remove.
        assert!(never_went_live(&broadcast("ready", None, None)));
    }

    #[test]
    fn a_broadcast_that_has_ever_been_live_is_never_stale() {
        // The single most important rule in the whole command: an actual start
        // time means a feed reached it, so there may be a recording behind it
        // and deleting it cannot be undone.
        assert!(!never_went_live(&broadcast(
            "complete",
            Some("2026-08-08T10:00:00Z"),
            Some("2026-08-08T12:00:00Z")
        )));
        assert!(!never_went_live(&broadcast(
            "live",
            Some("2026-08-08T10:00:00Z"),
            None
        )));
    }

    #[test]
    fn a_created_broadcast_that_somehow_has_a_start_time_is_still_spared() {
        // Belt and braces: the status alone is not trusted. If the two signals
        // ever disagree, the one that argues for keeping the broadcast wins.
        assert!(!never_went_live(&broadcast(
            "created",
            Some("2026-08-08T10:00:00Z"),
            None
        )));
    }

    #[test]
    fn transitional_and_finished_statuses_are_left_alone() {
        for status in [
            "testing",
            "testStarting",
            "liveStarting",
            "complete",
            "revoked",
        ] {
            assert!(
                !never_went_live(&broadcast(status, None, None)),
                "{status:?} must not be offered for deletion"
            );
        }
    }

    #[test]
    fn a_broadcast_with_no_snippet_or_status_is_kept() {
        // A reply missing the parts we asked for gives no evidence either way,
        // and "no evidence" must never mean "delete it".
        let bare: LiveBroadcastResource = serde_json::from_str(r#"{"id":"x"}"#).unwrap();
        assert!(!never_went_live(&bare));

        let no_status: LiveBroadcastResource =
            serde_json::from_str(r#"{"id":"x","snippet":{"title":"t"}}"#).unwrap();
        assert!(!never_went_live(&no_status));
    }

    #[test]
    fn a_broadcast_listing_page_reports_whether_more_pages_follow() {
        // Without reading the page token the cleanup command would only ever see
        // the first fifty broadcasts and quietly leave the rest behind.
        let one_page: ListResponse<LiveBroadcastResource> =
            serde_json::from_str(r#"{"items":[]}"#).unwrap();
        assert!(one_page.next_page_token.is_none());

        let more: ListResponse<LiveBroadcastResource> =
            serde_json::from_str(r#"{"items":[],"nextPageToken":"CDIQAA"}"#).unwrap();
        assert_eq!(more.next_page_token.as_deref(), Some("CDIQAA"));
    }

    #[test]
    fn the_broadcast_listing_url_asks_for_the_parts_the_cleanup_rule_needs() {
        // `snippet` carries the actual start time and `status` the lifecycle
        // word. Dropping either would leave `never_went_live` with no evidence,
        // and it would then correctly but uselessly spare everything.
        let url = format!("{API}/liveBroadcasts?part=id,snippet,status&mine=true&maxResults=50");
        assert!(url.contains("part=id,snippet,status"));
        assert!(url.contains("mine=true"));
    }

    #[test]
    fn the_builtin_category_list_can_be_searched_without_the_api() {
        // This is what makes the YouTube category field usable before the first
        // login, when there is no token with which to fetch the real list.
        let matches = search_common("gam");
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].id, "20");
        assert_eq!(matches[0].name, "Gaming");
    }

    #[test]
    fn searching_the_builtin_category_list_ignores_case_and_surrounding_spaces() {
        // People type what they see, capitals and stray spaces included.
        let matches = search_common("  MUSIC ");
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].id, "10");
    }

    #[test]
    fn an_empty_category_query_offers_every_builtin_category() {
        // Pressing Enter on an empty field should show the whole list to pick
        // from, not nothing at all.
        assert_eq!(search_common("").len(), COMMON_CATEGORIES.len());
    }

    #[test]
    fn an_unknown_category_query_matches_nothing() {
        assert!(search_common("underwater basket weaving").is_empty());
    }

    #[test]
    fn every_builtin_category_id_is_numeric_and_unique() {
        // The API rejects a non-numeric category id outright, and a duplicate
        // would make the autocomplete offer the same choice twice.
        let mut ids: Vec<&str> = COMMON_CATEGORIES.iter().map(|(id, _)| *id).collect();
        let count = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(
            ids.len(),
            count,
            "duplicate category id in the built-in list"
        );
        assert!(COMMON_CATEGORIES
            .iter()
            .all(|(id, _)| id.chars().all(|c| c.is_ascii_digit())));
    }

    #[test]
    fn a_stream_listing_maps_onto_the_ingest_endpoint_type() {
        // `msm streams` shows the id (so it can be pinned), the title (so a
        // human recognises it) and optionally the key.
        let json = r#"{
            "id": "sid",
            "snippet": {"title": "Default stream key"},
            "cdn": {"ingestionInfo": {
                "ingestionAddress": "rtmp://a.rtmp.youtube.com/live2",
                "streamName": "abcd-efgh"
            }}
        }"#;
        let stream: LiveStreamResource = serde_json::from_str(json).unwrap();

        let endpoint = IngestEndpoint {
            id: stream.id,
            title: stream.snippet.title,
            key: stream
                .cdn
                .and_then(|cdn| cdn.ingestion_info)
                .map(|info| info.stream_name),
        };

        assert_eq!(endpoint.id, "sid");
        assert_eq!(endpoint.title, "Default stream key");
        assert_eq!(endpoint.key.as_deref(), Some("abcd-efgh"));
    }

    #[test]
    fn quota_backoff_starts_unpaused() {
        let backoff = QuotaBackoff::default();
        assert!(backoff.remaining(Instant::now()).is_none());
    }

    #[test]
    fn quota_backoff_pauses_and_doubles() {
        let mut backoff = QuotaBackoff::default();
        let now = Instant::now();

        let first = backoff.on_quota_error(now);
        assert_eq!(first, QUOTA_PAUSE_INITIAL);
        assert!(backoff.remaining(now).is_some());
        // Just before the pause ends it is still paused; at the end it is not.
        assert!(backoff
            .remaining(now + first - StdDuration::from_secs(1))
            .is_some());
        assert!(backoff.remaining(now + first).is_none());

        let second = backoff.on_quota_error(now + first);
        assert_eq!(second, QUOTA_PAUSE_INITIAL * 2);
    }

    #[test]
    fn quota_backoff_is_capped_and_reset_by_success() {
        let mut backoff = QuotaBackoff::default();
        let now = Instant::now();
        for _ in 0..10 {
            backoff.on_quota_error(now);
        }
        assert_eq!(backoff.on_quota_error(now), QUOTA_PAUSE_MAX);

        backoff.on_success();
        assert!(backoff.remaining(now).is_none());
        assert_eq!(backoff.on_quota_error(now), QUOTA_PAUSE_INITIAL);
    }

    #[test]
    fn quota_marker_survives_the_anyhow_context_chain() {
        let err = anyhow::Error::new(QuotaExceeded)
            .context("reading stats failed")
            .context("outer");
        assert!(err.downcast_ref::<QuotaExceeded>().is_some());

        let plain = anyhow::anyhow!("some other failure").context("outer");
        assert!(plain.downcast_ref::<QuotaExceeded>().is_none());
    }

    #[test]
    fn human_minutes_rounds_up_and_never_says_zero() {
        assert_eq!(human_minutes(StdDuration::from_secs(0)), "1 min");
        assert_eq!(human_minutes(StdDuration::from_secs(59)), "1 min");
        assert_eq!(human_minutes(StdDuration::from_secs(60)), "1 min");
        assert_eq!(human_minutes(StdDuration::from_secs(61)), "2 min");
        assert_eq!(human_minutes(StdDuration::from_secs(600)), "10 min");
    }

    /// The autocomplete calls `search_categories` on every keystroke. It used to
    /// fetch the whole `videoCategories.list` each time — twenty requests to
    /// type "Science & Technology", nineteen of them spending daily quota to
    /// return an answer already in hand.
    ///
    /// The backend here is built with an HTTP client that cannot reach anything,
    /// so any attempt to go to the network fails the test rather than merely
    /// being slow.
    #[tokio::test]
    async fn typing_a_category_name_does_not_go_to_the_network_twice() {
        let offline = reqwest::Client::builder()
            .timeout(StdDuration::from_millis(1))
            .build()
            .unwrap();
        let mut backend = YouTubeBackend::new(offline, "token".into(), false, String::new());

        // Stand in for the one fetch a real session performs on first use.
        backend.category_cache = Some(vec![
            Category {
                id: "20".into(),
                name: "Gaming".into(),
            },
            Category {
                id: "28".into(),
                name: "Science & Technology".into(),
            },
        ]);

        let typed = "Science & Technology";
        for end in 1..=typed.len() {
            let matches = backend
                .search_categories(&typed[..end])
                .await
                .expect("filtering the cached list must not touch the network");
            assert!(
                matches.iter().any(|c| c.id == "28"),
                "expected the science category while typing {:?}",
                &typed[..end]
            );
        }

        // An empty query still offers the whole list.
        assert_eq!(backend.search_categories("").await.unwrap().len(), 2);
    }
}
