//! Core domain types shared by every part of the application.
//!
//! The idea here is that the user fills in **one** [`StreamPlan`] describing the
//! broadcast they want, and each backend (Twitch, YouTube) translates that single
//! plan into whatever sequence of API calls its platform happens to require.
//! Nothing in this file knows about HTTP, OAuth, or the terminal UI.

use serde::{Deserialize, Serialize};
use std::fmt;

/// Which platform(s) a broadcast should go out on.
///
/// This is a set rather than an enum-with-`Both` so that adding a third platform
/// later (Kick, Trovo, ...) does not require changing the shape of the type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Platform {
    Twitch,
    YouTube,
}

impl Platform {
    /// Every platform the application knows about, in display order.
    pub const ALL: [Platform; 2] = [Platform::Twitch, Platform::YouTube];

    /// Lower-case machine-readable name, used for config keys and token files.
    pub fn slug(self) -> &'static str {
        match self {
            Platform::Twitch => "twitch",
            Platform::YouTube => "youtube",
        }
    }

    /// Human-readable name for the UI.
    pub fn label(self) -> &'static str {
        match self {
            Platform::Twitch => "Twitch",
            Platform::YouTube => "YouTube",
        }
    }
}

impl fmt::Display for Platform {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

impl std::str::FromStr for Platform {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "twitch" | "ttv" => Ok(Platform::Twitch),
            "youtube" | "yt" => Ok(Platform::YouTube),
            other => Err(format!(
                "unknown platform {other:?} (expected \"twitch\" or \"youtube\")"
            )),
        }
    }
}

/// How visible a YouTube broadcast should be. Twitch has no equivalent setting,
/// so this field is simply ignored by the Twitch backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Privacy {
    /// Anyone can find and watch it.
    #[default]
    Public,
    /// Only people with the link can watch it; it does not appear in search.
    Unlisted,
    /// Only the broadcaster can watch it. Useful for test streams.
    Private,
}

impl Privacy {
    pub const ALL: [Privacy; 3] = [Privacy::Public, Privacy::Unlisted, Privacy::Private];

    /// The exact string the YouTube API expects in `status.privacyStatus`.
    pub fn api_value(self) -> &'static str {
        match self {
            Privacy::Public => "public",
            Privacy::Unlisted => "unlisted",
            Privacy::Private => "private",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Privacy::Public => "Public",
            Privacy::Unlisted => "Unlisted",
            Privacy::Private => "Private",
        }
    }
}

impl std::str::FromStr for Privacy {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "public" => Ok(Privacy::Public),
            "unlisted" => Ok(Privacy::Unlisted),
            "private" => Ok(Privacy::Private),
            other => Err(format!(
                "unknown privacy {other:?} (expected public, unlisted or private)"
            )),
        }
    }
}

/// A Twitch category (what Twitch calls a "game"), resolved against their API.
///
/// Twitch's update endpoint does not accept a category *name* — it only accepts
/// a numeric `game_id`. So the UI lets the user type a name, searches for it,
/// and stores the resolved pair here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Category {
    /// Twitch's numeric identifier, e.g. `"1469308723"` for Software & Game Dev.
    pub id: String,
    /// The display name, e.g. `"Software and Game Development"`.
    pub name: String,
}

/// Everything the user wants to say about the broadcast they are about to start.
///
/// This is the single source of truth that gets fanned out to every selected
/// platform. It is also exactly what gets written to / read from the config file
/// when the user prefers editing a file over using the form.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct StreamPlan {
    /// Stream title. Twitch allows 140 characters, YouTube only 100, so the
    /// YouTube backend truncates if necessary rather than failing the request.
    pub title: String,

    /// Long-form description. Twitch has no description field at all, so this is
    /// YouTube-only. Maximum 5000 characters.
    pub description: String,

    /// Free-form tags. On Twitch these become the channel's tags (max 10, each
    /// at most 25 characters and containing no spaces). On YouTube they become
    /// the video's `snippet.tags`, and — because you asked for it — they are also
    /// appended to the YouTube title as `#hashtags` while they still fit.
    pub tags: Vec<String>,

    /// The Twitch category, already resolved to an id by searching their API.
    pub twitch_category: Option<Category>,

    /// The YouTube video category id. 20 is "Gaming", 28 is "Science &
    /// Technology", 24 is "Entertainment". Stored as a string because that is
    /// what the API returns and expects.
    pub youtube_category_id: String,

    /// ISO 639-1 two-letter language code, e.g. `"en"`, `"pl"`, `"de"`.
    /// Twitch calls this `broadcaster_language`; YouTube calls it
    /// `snippet.defaultLanguage`.
    pub language: String,

    /// YouTube visibility. Ignored by Twitch.
    pub privacy: Privacy,

    /// YouTube legally requires every upload to declare whether it is aimed at
    /// children. Almost always `false` for a gaming or coding stream.
    pub made_for_kids: bool,

    /// Whether YouTube should start and stop the broadcast automatically as soon
    /// as it detects the incoming video feed from OBS. Leaving this on means you
    /// never have to click "Go live" in YouTube Studio.
    pub youtube_auto_start: bool,

    /// Whether YouTube should end the broadcast automatically when the feed
    /// stops. Handy, but it means a brief OBS crash can end your stream, so it
    /// is exposed as a setting rather than hard-coded.
    pub youtube_auto_stop: bool,
}

impl Default for StreamPlan {
    fn default() -> Self {
        Self {
            title: String::new(),
            description: String::new(),
            tags: Vec::new(),
            twitch_category: None,
            // 20 = "Gaming", the most common choice for a live stream.
            youtube_category_id: "20".to_string(),
            language: "en".to_string(),
            privacy: Privacy::Public,
            made_for_kids: false,
            youtube_auto_start: true,
            youtube_auto_stop: false,
        }
    }
}

/// The hard limits each platform imposes. Kept as constants so the UI can show
/// a live character counter that turns red before the API rejects the request.
pub mod limits {
    /// Twitch rejects titles longer than this.
    pub const TWITCH_TITLE: usize = 140;
    /// YouTube rejects titles longer than this. This is the tighter of the two,
    /// so it is what the UI warns against first.
    pub const YOUTUBE_TITLE: usize = 100;
    /// YouTube description ceiling.
    pub const YOUTUBE_DESCRIPTION: usize = 5000;
    /// Twitch accepts at most this many tags.
    pub const TWITCH_MAX_TAGS: usize = 10;
    /// Each individual Twitch tag is capped at this many characters.
    pub const TWITCH_TAG_LEN: usize = 25;
    /// YouTube counts the *combined* length of all tags against this budget.
    pub const YOUTUBE_TAGS_TOTAL: usize = 500;
}

/// A problem with the plan that we can detect locally, before spending an API
/// call to be told the same thing by Twitch or Google.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationIssue {
    /// Which form field the problem belongs to, so the UI can jump the cursor there.
    pub field: Field,
    /// `true` if this will definitely fail the request; `false` if it is merely
    /// something the user probably wants to know (e.g. "your title will be
    /// truncated for YouTube").
    pub blocking: bool,
    /// Plain-language explanation of what is wrong and how to fix it.
    pub message: String,
}

/// The editable fields of the form, in tab order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Field {
    Title,
    Description,
    Tags,
    TwitchCategory,
    YouTubeCategory,
    Language,
    Privacy,
    MadeForKids,
    AutoStart,
    AutoStop,
}

impl Field {
    /// Tab order for the form.
    pub const ORDER: [Field; 10] = [
        Field::Title,
        Field::Description,
        Field::Tags,
        Field::TwitchCategory,
        Field::YouTubeCategory,
        Field::Language,
        Field::Privacy,
        Field::MadeForKids,
        Field::AutoStart,
        Field::AutoStop,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Field::Title => "Title",
            Field::Description => "Description",
            Field::Tags => "Tags",
            Field::TwitchCategory => "Twitch category",
            Field::YouTubeCategory => "YouTube category",
            Field::Language => "Language",
            Field::Privacy => "Privacy (YouTube)",
            Field::MadeForKids => "Made for kids (YouTube)",
            Field::AutoStart => "Auto-start (YouTube)",
            Field::AutoStop => "Auto-stop (YouTube)",
        }
    }

    /// A one-line hint shown under the form explaining what this field does.
    pub fn help(self) -> &'static str {
        match self {
            Field::Title => "Shown on both platforms. Twitch allows 140 chars, YouTube only 100.",
            Field::Description => "YouTube only — Twitch has no description field.",
            Field::Tags => "Comma-separated. Twitch: max 10, 25 chars each, no spaces. Also appended to the YouTube title as #hashtags.",
            Field::TwitchCategory => "Type to search Twitch's category list; press Enter to pick a match.",
            Field::YouTubeCategory => "Type to search YouTube's category list; press Enter to pick a match.",
            Field::Language => "ISO 639-1 code, e.g. en, pl, de. Type to search by language name.",
            Field::Privacy => "Left/Right to change. Use Private for a test broadcast.",
            Field::MadeForKids => "Space to toggle. YouTube legally requires this declaration.",
            Field::AutoStart => "Space to toggle. YouTube starts the broadcast when it sees the OBS feed.",
            Field::AutoStop => "Space to toggle. Off is safer: a brief OBS crash will not end the stream.",
        }
    }

    /// Whether this field is a free-text input as opposed to a toggle or a
    /// left/right selector. Used by the key handler to decide what typing means.
    pub fn is_text_input(self) -> bool {
        matches!(
            self,
            Field::Title
                | Field::Description
                | Field::Tags
                | Field::TwitchCategory
                | Field::YouTubeCategory
                | Field::Language
        )
    }
}

impl StreamPlan {
    /// Parse the comma-separated tag input box into a clean tag list.
    ///
    /// Splits on commas, trims surrounding whitespace, drops empties, and
    /// removes duplicates while preserving the order the user typed them in.
    pub fn parse_tags(raw: &str) -> Vec<String> {
        let mut seen = Vec::new();
        for tag in raw.split(',') {
            let tag = tag.trim();
            if tag.is_empty() {
                continue;
            }
            // Case-insensitive duplicate check: "Rust" and "rust" are the same tag.
            if !seen
                .iter()
                .any(|existing: &String| existing.eq_ignore_ascii_case(tag))
            {
                seen.push(tag.to_string());
            }
        }
        seen
    }

    /// Turn the tag list back into the comma-separated form shown in the input box.
    pub fn tags_input(&self) -> String {
        self.tags.join(", ")
    }

    /// Twitch refuses tags containing spaces or punctuation, so a tag like
    /// `"live coding"` has to become `"livecoding"` before it is sent.
    ///
    /// Returns only tags that survive the transformation, capped at Twitch's
    /// limit of 10 tags of 25 characters each.
    pub fn twitch_tags(&self) -> Vec<String> {
        self.tags
            .iter()
            .map(|tag| {
                tag.chars()
                    .filter(|c| c.is_alphanumeric())
                    .take(limits::TWITCH_TAG_LEN)
                    .collect::<String>()
            })
            .filter(|tag| !tag.is_empty())
            .take(limits::TWITCH_MAX_TAGS)
            .collect()
    }

    /// Build the YouTube title: the plain title with as many `#hashtags` appended
    /// as will fit inside YouTube's 100-character limit.
    ///
    /// You asked for the tags to be carried into the YouTube title as hashtags.
    /// YouTube itself only honours the first three hashtags in a title (it shows
    /// them as links above the video), so adding more has no benefit — but we add
    /// whatever fits and stop cleanly at the limit rather than letting the API
    /// reject an over-long title.
    pub fn youtube_title(&self) -> String {
        let base: String = truncate_chars(&self.title, limits::YOUTUBE_TITLE);
        let mut out = base;

        for tag in &self.tags {
            // A YouTube hashtag cannot contain spaces or punctuation either.
            let cleaned: String = tag.chars().filter(|c| c.is_alphanumeric()).collect();
            if cleaned.is_empty() {
                continue;
            }
            let addition = format!(" #{cleaned}");
            if out.chars().count() + addition.chars().count() <= limits::YOUTUBE_TITLE {
                out.push_str(&addition);
            }
        }
        out
    }

    /// The Twitch title, truncated to Twitch's own (more generous) limit.
    pub fn twitch_title(&self) -> String {
        truncate_chars(&self.title, limits::TWITCH_TITLE)
    }

    /// YouTube counts the combined length of all tags against a 500-character
    /// budget, so return the longest prefix of the tag list that fits.
    pub fn youtube_tags(&self) -> Vec<String> {
        let mut out = Vec::new();
        let mut budget = limits::YOUTUBE_TAGS_TOTAL;
        for tag in &self.tags {
            let cost = tag.chars().count() + 1; // +1 for the separating comma
            if cost > budget {
                break;
            }
            budget -= cost;
            out.push(tag.clone());
        }
        out
    }

    /// Check the plan against everything we can verify without calling an API.
    ///
    /// `platforms` matters because most rules are platform-specific: an empty
    /// Twitch category is fatal if Twitch is selected and irrelevant otherwise.
    pub fn validate(&self, platforms: &[Platform]) -> Vec<ValidationIssue> {
        let mut issues = Vec::new();
        let twitch = platforms.contains(&Platform::Twitch);
        let youtube = platforms.contains(&Platform::YouTube);

        if self.title.trim().is_empty() {
            issues.push(ValidationIssue {
                field: Field::Title,
                blocking: true,
                message: "Title cannot be empty — both platforms reject a blank title.".into(),
            });
        }

        let title_len = self.title.chars().count();
        if twitch && title_len > limits::TWITCH_TITLE {
            issues.push(ValidationIssue {
                field: Field::Title,
                blocking: true,
                message: format!(
                    "Title is {title_len} characters; Twitch allows at most {}.",
                    limits::TWITCH_TITLE
                ),
            });
        }
        if youtube && title_len > limits::YOUTUBE_TITLE {
            issues.push(ValidationIssue {
                field: Field::Title,
                blocking: false,
                message: format!(
                    "Title is {title_len} characters; YouTube allows {}. It will be shortened for YouTube only.",
                    limits::YOUTUBE_TITLE
                ),
            });
        }

        if youtube && self.description.chars().count() > limits::YOUTUBE_DESCRIPTION {
            issues.push(ValidationIssue {
                field: Field::Description,
                blocking: true,
                message: format!(
                    "Description is {} characters; YouTube allows at most {}.",
                    self.description.chars().count(),
                    limits::YOUTUBE_DESCRIPTION
                ),
            });
        }

        if twitch {
            if self.twitch_category.is_none() {
                issues.push(ValidationIssue {
                    field: Field::TwitchCategory,
                    blocking: true,
                    message:
                        "Pick a Twitch category — type part of its name and press Enter to select a match."
                            .into(),
                });
            }
            if self.tags.len() > limits::TWITCH_MAX_TAGS {
                issues.push(ValidationIssue {
                    field: Field::Tags,
                    blocking: false,
                    message: format!(
                        "You have {} tags; Twitch accepts {}. The extra ones go to YouTube only.",
                        self.tags.len(),
                        limits::TWITCH_MAX_TAGS
                    ),
                });
            }
            for tag in &self.tags {
                if tag.chars().any(|c| !c.is_alphanumeric()) {
                    issues.push(ValidationIssue {
                        field: Field::Tags,
                        blocking: false,
                        message: format!(
                            "Twitch tags cannot contain spaces or punctuation, so {:?} will be sent as {:?}.",
                            tag,
                            tag.chars().filter(|c| c.is_alphanumeric()).collect::<String>()
                        ),
                    });
                    break;
                }
            }
        }

        if youtube && self.youtube_category_id.trim().is_empty() {
            issues.push(ValidationIssue {
                field: Field::YouTubeCategory,
                blocking: true,
                message: "Pick a YouTube category — the API rejects an update without one.".into(),
            });
        }

        if self.language.chars().count() != 2 && self.language != "other" {
            issues.push(ValidationIssue {
                field: Field::Language,
                blocking: twitch,
                message: format!(
                    "Language must be a two-letter ISO 639-1 code such as \"en\" or \"pl\", not {:?}.",
                    self.language
                ),
            });
        }

        issues
    }

    /// `true` when nothing blocking is wrong and the plan can be submitted.
    pub fn is_submittable(&self, platforms: &[Platform]) -> bool {
        !platforms.is_empty() && !self.validate(platforms).iter().any(|issue| issue.blocking)
    }
}

/// Cut a string to at most `max` **characters** (not bytes).
///
/// Using bytes here would panic on any multi-byte character — an emoji in a
/// stream title is enough to do it — so this counts real characters.
pub fn truncate_chars(input: &str, max: usize) -> String {
    if input.chars().count() <= max {
        return input.to_string();
    }
    input.chars().take(max).collect()
}

/// What a backend gives back once the broadcast has been set up successfully.
#[derive(Debug, Clone, Default)]
pub struct GoLiveOutcome {
    /// Where viewers watch. For Twitch this is the channel page; for YouTube it
    /// is the specific broadcast's watch URL.
    pub watch_url: Option<String>,
    /// The broadcaster's own management page (Twitch Stream Manager / YT Studio).
    pub manage_url: Option<String>,
    /// RTMP ingest endpoint, if the platform exposes one.
    pub ingest_url: Option<String>,
    /// The stream key. Held in memory only, never written to disk, and masked in
    /// the UI until explicitly revealed.
    pub stream_key: Option<String>,
    /// Human-readable notes worth showing the user, e.g. "reused existing stream
    /// key, your OBS settings still work".
    pub notes: Vec<String>,
}

/// One of the RTMP ingest endpoints a platform holds for your account — what
/// YouTube calls a "stream" and everybody else calls a stream key.
///
/// Listing these in Config → Housekeeping is how the id to pin as `stream_id`
/// in the config, so that every broadcast binds to the same key and your OBS (or
/// Aitum) settings never have to change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IngestEndpoint {
    /// The platform's own identifier. This is the value that goes into
    /// `stream_id` under `[youtube]` in the config file.
    pub id: String,
    /// The name the platform shows for it, e.g. "Default stream key".
    pub title: String,
    /// The key itself, when the platform reported one.
    ///
    /// It is carried here so the copy-to-clipboard path can reach it, and
    /// asked, and for no other reason: a key is enough on its own to broadcast
    /// to the channel, so nothing prints it unless the flag was passed.
    pub key: Option<String>,
}

/// A broadcast that a platform created but that never received a video feed.
///
/// Submitting a plan a second time creates a *new* YouTube broadcast rather than
/// editing the previous one, so a session where you fixed a typo and resubmitted
/// leaves the earlier attempt behind. Nothing removes them by itself, and they
/// pile up in YouTube Studio's list of upcoming streams. Config → Housekeeping finds
/// them; see there for why "never received a feed" is defined so cautiously.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StaleBroadcast {
    pub id: String,
    pub title: String,
    /// When it was scheduled to start, if the platform reported a time.
    pub scheduled_start: Option<chrono::DateTime<chrono::Utc>>,
    /// The platform's own status word, e.g. "created" or "ready". Shown to the
    /// user so it is visible *why* each broadcast was picked out.
    pub status: String,
}

/// A single live statistic, rendered as one row in the dashboard table.
#[derive(Debug, Clone)]
pub struct Stat {
    pub label: String,
    pub value: String,
}

/// The stats snapshot for one platform at one point in time.
#[derive(Debug, Clone, Default)]
pub struct PlatformStats {
    /// `true` if the platform reports an active incoming broadcast right now.
    pub live: bool,
    /// Current concurrent viewers, when the platform reports it.
    pub viewers: Option<u64>,
    /// When the broadcast started, used to render an uptime counter.
    pub started_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Everything else worth displaying — followers, likes, subscriber count.
    pub extra: Vec<Stat>,
    /// Set when the most recent poll failed, so the UI can show a stale-data
    /// warning instead of silently displaying old numbers.
    pub error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan_with(title: &str, tags: &[&str]) -> StreamPlan {
        StreamPlan {
            title: title.to_string(),
            tags: tags.iter().map(|t| t.to_string()).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn parse_tags_trims_and_deduplicates_case_insensitively() {
        let tags = StreamPlan::parse_tags("  rust , RUST,  tui ,, gamedev,");
        assert_eq!(tags, vec!["rust", "tui", "gamedev"]);
    }

    #[test]
    fn twitch_tags_strip_spaces_and_punctuation() {
        let plan = plan_with("t", &["live coding", "rust!", "c++"]);
        assert_eq!(plan.twitch_tags(), vec!["livecoding", "rust", "c"]);
    }

    #[test]
    fn twitch_tags_are_capped_at_ten() {
        let many: Vec<String> = (0..15).map(|i| format!("tag{i}")).collect();
        let plan = StreamPlan {
            tags: many,
            ..Default::default()
        };
        assert_eq!(plan.twitch_tags().len(), limits::TWITCH_MAX_TAGS);
    }

    #[test]
    fn youtube_title_appends_hashtags_until_the_limit_is_reached() {
        let plan = plan_with("Hello", &["rust", "tui"]);
        assert_eq!(plan.youtube_title(), "Hello #rust #tui");
    }

    #[test]
    fn youtube_title_never_exceeds_one_hundred_characters() {
        let long = "x".repeat(95);
        let plan = plan_with(&long, &["averylonghashtagindeed", "b"]);
        let title = plan.youtube_title();
        assert!(
            title.chars().count() <= limits::YOUTUBE_TITLE,
            "title was {} chars: {title:?}",
            title.chars().count()
        );
        // The short tag still fits in the leftover space even though the long one did not.
        assert!(title.ends_with("#b"));
    }

    #[test]
    fn truncation_counts_characters_not_bytes() {
        // Each of these emoji is four bytes; truncating by bytes would panic.
        let emoji = "🎮🎮🎮🎮🎮";
        assert_eq!(truncate_chars(emoji, 2).chars().count(), 2);
    }

    #[test]
    fn empty_title_is_a_blocking_error() {
        let plan = plan_with("", &[]);
        let issues = plan.validate(&[Platform::YouTube]);
        assert!(issues.iter().any(|i| i.blocking && i.field == Field::Title));
    }

    #[test]
    fn missing_twitch_category_only_blocks_when_twitch_is_selected() {
        let plan = plan_with("A title", &[]);
        assert!(!plan.is_submittable(&[Platform::Twitch]));
        assert!(plan.is_submittable(&[Platform::YouTube]));
    }

    #[test]
    fn long_title_blocks_twitch_but_only_warns_for_youtube() {
        let plan = plan_with(&"x".repeat(120), &[]);

        let yt = plan.validate(&[Platform::YouTube]);
        assert!(yt.iter().any(|i| i.field == Field::Title && !i.blocking));

        let tw = plan.validate(&[Platform::Twitch]);
        // 120 is under Twitch's 140 limit, so Twitch raises no title-length issue.
        assert!(!tw
            .iter()
            .any(|i| i.field == Field::Title && i.message.contains("Twitch allows")));
    }

    #[test]
    fn youtube_tags_respect_the_combined_length_budget() {
        let tags: Vec<String> = (0..100).map(|i| format!("tag-number-{i}")).collect();
        let plan = StreamPlan {
            tags,
            ..Default::default()
        };
        let total: usize = plan
            .youtube_tags()
            .iter()
            .map(|t| t.chars().count() + 1)
            .sum();
        assert!(total <= limits::YOUTUBE_TAGS_TOTAL);
    }

    #[test]
    fn platform_parses_from_common_spellings() {
        assert_eq!("TWITCH".parse::<Platform>().unwrap(), Platform::Twitch);
        assert_eq!("yt".parse::<Platform>().unwrap(), Platform::YouTube);
        assert!("kick".parse::<Platform>().is_err());
    }
}
