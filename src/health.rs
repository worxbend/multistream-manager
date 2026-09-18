//! The one line that says whether the broadcast is actually working.
//!
//! The numbers behind this were already on screen — somewhere. Viewer counts
//! live on the dashboard, dropped frames on the OBS tab, and a platform that
//! has stopped receiving your feed shows up as a `live` flag changing in a
//! panel you are not looking at. Nothing put them together, and nothing said
//! them on the Chat tab, which is where a streamer actually spends the hour.
//!
//! So this reduces all of it to one strip that fits in the header and is the
//! same everywhere:
//!
//! ```text
//! TW ● live 142  ·  YT ● live 38  ·  OBS ● 6100 kb/s drop 0.2%  ·  1:23:04
//! ```
//!
//! The colour is computed rather than decorative, and that is the point. Green
//! means what you think it means. Amber means something is worth a look but
//! the broadcast is going out. Red means the thing every streamer fears and
//! nothing else on screen will tell you: OBS is still sending, and the
//! platform has stopped receiving.
//!
//! Everything here is a pure function over state the interface already holds,
//! so it can be recomputed every frame and tested without a network.

use std::collections::BTreeMap;

use unicode_segmentation::UnicodeSegmentation;

use crate::model::{Platform, PlatformStats};
use crate::obs::state::ObsState;

/// How a segment is doing, which decides its colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Health {
    /// Working.
    Good,
    /// Working, but worth a look.
    Warn,
    /// Broken, or about to be.
    Bad,
    /// Nothing to say — not selected, not connected, not started.
    Idle,
}

/// One piece of the strip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    /// The short label at the front: `TW`, `YT`, `OBS`, or empty for the
    /// uptime at the end.
    pub label: &'static str,
    /// Everything after the label.
    pub detail: String,
    pub health: Health,
}

/// The share of outgoing frames that may be dropped before it is worth
/// saying so.
///
/// Dropped *output* frames are the upload not keeping up, which is the one
/// that viewers see as stuttering. A little is normal on any real connection;
/// this is the point at which it stops being noise.
const DROPPED_WARN_PERCENT: f64 = 1.0;

/// …and the point at which it is a problem rather than a blemish.
const DROPPED_BAD_PERCENT: f64 = 5.0;

/// Build the strip.
///
/// `obs` is `None` when OBS control is switched off, in which case there is
/// no OBS segment — an absent feature is not an unhealthy one.
pub fn strip(
    selected: &[Platform],
    stats: &BTreeMap<Platform, PlatformStats>,
    obs: Option<&ObsState>,
) -> Vec<Segment> {
    let mut segments = Vec::new();

    // Whether OBS believes it is sending. This is what turns "not live" from
    // a normal state into an alarm: nobody minds a platform being offline
    // before the stream starts.
    let sending = obs.is_some_and(|obs| obs.is_connected() && obs.streaming);

    for &platform in selected {
        segments.push(platform_segment(platform, stats.get(&platform), sending));
    }

    if let Some(obs) = obs {
        segments.push(obs_segment(obs));
    }

    if let Some(uptime) = uptime(stats) {
        segments.push(Segment {
            label: "",
            detail: uptime,
            health: Health::Good,
        });
    }

    segments
}

fn platform_segment(
    platform: Platform,
    stats: Option<&PlatformStats>,
    obs_is_sending: bool,
) -> Segment {
    let label = match platform {
        Platform::Twitch => "TW",
        Platform::YouTube => "YT",
    };

    let Some(stats) = stats else {
        return Segment {
            label,
            detail: "—".into(),
            health: Health::Idle,
        };
    };

    // A failed poll is not a dead stream. The numbers on screen are simply
    // older than they look, and saying so is more honest than either hiding
    // it or announcing a failure that may not have happened.
    if let Some(error) = &stats.error {
        return Segment {
            label,
            detail: format!("stale ({})", first_sentence(error)),
            health: Health::Warn,
        };
    }

    if stats.live {
        let viewers = stats
            .viewers
            .map(|count| format!("live {count}"))
            .unwrap_or_else(|| "live".to_string());
        return Segment {
            label,
            detail: viewers,
            health: Health::Good,
        };
    }

    // Not live. Before the stream starts that is simply the truth; while OBS
    // is sending, it is the encoder-died-forty-minutes-ago case, and it is
    // the reason this strip exists.
    if obs_is_sending {
        Segment {
            label,
            detail: "NOT RECEIVING".into(),
            health: Health::Bad,
        }
    } else {
        Segment {
            label,
            detail: "offline".into(),
            health: Health::Idle,
        }
    }
}

fn obs_segment(obs: &ObsState) -> Segment {
    if !obs.is_connected() {
        return Segment {
            label: "OBS",
            detail: "not connected".into(),
            health: Health::Idle,
        };
    }

    if !obs.streaming {
        let detail = if obs.recording {
            "recording".to_string()
        } else {
            "idle".to_string()
        };
        return Segment {
            label: "OBS",
            detail,
            health: Health::Idle,
        };
    }

    let mut detail = match obs.stream_bitrate_kbps {
        Some(kbps) => format!("{kbps:.0} kb/s"),
        None => "streaming".to_string(),
    };

    let mut health = Health::Good;
    if let Some(stats) = &obs.stats {
        let dropped = stats.output_skipped_percent();
        if dropped >= DROPPED_WARN_PERCENT {
            detail.push_str(&format!(" drop {dropped:.1}%"));
            health = if dropped >= DROPPED_BAD_PERCENT {
                Health::Bad
            } else {
                Health::Warn
            };
        }
    }

    Segment {
        label: "OBS",
        detail,
        health,
    }
}

/// How long the longest-running broadcast has been up, as `1:23:04`.
///
/// The earliest start across the platforms, because going live on two
/// platforms a few seconds apart should show one uptime rather than two that
/// disagree.
fn uptime(stats: &BTreeMap<Platform, PlatformStats>) -> Option<String> {
    let earliest = stats
        .values()
        .filter(|stats| stats.live)
        .filter_map(|stats| stats.started_at)
        .min()?;

    let elapsed = chrono::Utc::now() - earliest;
    // A clock skew between here and the platform can put the start slightly
    // in the future; showing a negative uptime would be worse than showing
    // none.
    let seconds = elapsed.num_seconds().max(0);
    Some(format!(
        "{}:{:02}:{:02}",
        seconds / 3600,
        (seconds % 3600) / 60,
        seconds % 60
    ))
}

/// The first sentence of an error, so a paragraph-long API failure still fits
/// on one line beside everything else.
fn first_sentence(text: &str) -> String {
    let trimmed = text.trim();
    let end = trimmed.find(". ").map(|at| at + 1).unwrap_or(trimmed.len());
    let mut sentence = trimmed[..end].trim_end_matches('.').to_string();
    const LIMIT: usize = 40;
    if sentence.graphemes(true).count() > LIMIT {
        sentence = sentence.graphemes(true).take(LIMIT - 1).collect::<String>() + "…";
    }
    sentence
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::obs::state::{Connection, Stats};

    fn live(viewers: u64, minutes: i64) -> PlatformStats {
        PlatformStats {
            live: true,
            viewers: Some(viewers),
            started_at: Some(chrono::Utc::now() - chrono::Duration::minutes(minutes)),
            extra: Vec::new(),
            error: None,
        }
    }

    fn offline() -> PlatformStats {
        PlatformStats {
            live: false,
            viewers: None,
            started_at: None,
            extra: Vec::new(),
            error: None,
        }
    }

    fn streaming_obs(output_skipped: u64, total: u64) -> ObsState {
        ObsState {
            connection: Connection::Connected,
            streaming: true,
            stream_bitrate_kbps: Some(6100.0),
            stats: Some(Stats {
                cpu_usage_percent: 10.0,
                memory_usage_mb: 500.0,
                available_disk_space_mb: 100_000.0,
                active_fps: 60.0,
                average_frame_render_time_ms: 2.0,
                render_skipped_frames: 0,
                render_total_frames: total,
                output_skipped_frames: output_skipped,
                output_total_frames: total,
            }),
            ..ObsState::default()
        }
    }

    fn stats_of(entries: Vec<(Platform, PlatformStats)>) -> BTreeMap<Platform, PlatformStats> {
        entries.into_iter().collect()
    }

    #[test]
    fn a_healthy_stream_reads_green_with_its_viewer_count() {
        let stats = stats_of(vec![(Platform::Twitch, live(142, 83))]);
        let obs = streaming_obs(0, 10_000);

        let segments = strip(&[Platform::Twitch], &stats, Some(&obs));

        assert_eq!(segments[0].label, "TW");
        assert_eq!(segments[0].detail, "live 142");
        assert_eq!(segments[0].health, Health::Good);
        assert_eq!(segments[1].detail, "6100 kb/s");
    }

    /// The case the strip exists for. OBS is still sending and the platform
    /// has stopped receiving — the encoder-died-forty-minutes-ago failure
    /// that nothing else on screen reports.
    #[test]
    fn a_platform_that_stops_receiving_while_obs_sends_is_red() {
        let stats = stats_of(vec![(Platform::Twitch, offline())]);
        let obs = streaming_obs(0, 10_000);

        let segments = strip(&[Platform::Twitch], &stats, Some(&obs));

        assert_eq!(segments[0].health, Health::Bad);
        assert_eq!(segments[0].detail, "NOT RECEIVING");
    }

    /// …but the same "not live" before the stream starts is just the truth,
    /// and an interface that shouts about it would be one nobody reads.
    #[test]
    fn not_being_live_before_the_stream_starts_is_not_an_alarm() {
        let stats = stats_of(vec![(Platform::Twitch, offline())]);
        let obs = ObsState {
            connection: Connection::Connected,
            ..ObsState::default()
        };

        let segments = strip(&[Platform::Twitch], &stats, Some(&obs));

        assert_eq!(segments[0].health, Health::Idle);
        assert_eq!(segments[0].detail, "offline");
    }

    /// A failed poll means the numbers are older than they look. That is not
    /// the same as a dead stream and must not be reported as one.
    #[test]
    fn a_failed_poll_is_amber_and_says_the_data_is_stale() {
        let stats = stats_of(vec![(
            Platform::Twitch,
            PlatformStats {
                error: Some("the request timed out. Try again shortly.".into()),
                ..offline()
            },
        )]);
        let obs = streaming_obs(0, 10_000);

        let segments = strip(&[Platform::Twitch], &stats, Some(&obs));

        assert_eq!(segments[0].health, Health::Warn);
        assert!(segments[0].detail.starts_with("stale"));
        assert!(
            !segments[0].detail.contains("Try again"),
            "only the first sentence fits on this line: {}",
            segments[0].detail
        );
    }

    /// A char-based cap can split a multi-codepoint grapheme cluster in half,
    /// leaving a dangling joiner on a line drawn straight into the header.
    #[test]
    fn first_sentence_truncates_at_grapheme_boundaries() {
        // One grapheme cluster, seven chars: a char-based cap would cut it
        // apart well before 40 chars in.
        let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}\u{200D}\u{1F466}";
        let error = family.repeat(45);

        let sentence = first_sentence(&error);

        assert!(
            sentence.ends_with('…'),
            "expected the long error to be truncated: {sentence}"
        );
        assert!(
            sentence.graphemes(true).all(|g| g == family || g == "…"),
            "a grapheme cluster was split apart: {sentence}"
        );
    }

    #[test]
    fn dropped_frames_climb_from_green_through_amber_to_red() {
        let stats = stats_of(vec![(Platform::Twitch, live(1, 1))]);

        let fine = strip(
            &[Platform::Twitch],
            &stats,
            Some(&streaming_obs(50, 10_000)), // 0.5%
        );
        assert_eq!(fine[1].health, Health::Good);
        assert!(!fine[1].detail.contains("drop"), "{}", fine[1].detail);

        let warn = strip(
            &[Platform::Twitch],
            &stats,
            Some(&streaming_obs(200, 10_000)), // 2%
        );
        assert_eq!(warn[1].health, Health::Warn);
        assert!(warn[1].detail.contains("drop 2.0%"), "{}", warn[1].detail);

        let bad = strip(
            &[Platform::Twitch],
            &stats,
            Some(&streaming_obs(800, 10_000)), // 8%
        );
        assert_eq!(bad[1].health, Health::Bad);
    }

    /// OBS switched off is not an unhealthy OBS. Somebody streaming from a
    /// different encoder should see no OBS segment at all.
    #[test]
    fn obs_being_switched_off_leaves_no_obs_segment() {
        let stats = stats_of(vec![(Platform::Twitch, live(5, 2))]);

        let segments = strip(&[Platform::Twitch], &stats, None);

        assert!(!segments.iter().any(|segment| segment.label == "OBS"));
    }

    /// Going live on two platforms a few seconds apart must show one uptime,
    /// not two that disagree.
    #[test]
    fn the_uptime_follows_the_broadcast_that_started_first() {
        let stats = stats_of(vec![
            (Platform::Twitch, live(10, 90)),
            (Platform::YouTube, live(4, 88)),
        ]);

        let segments = strip(&[Platform::Twitch, Platform::YouTube], &stats, None);

        let uptime = segments.last().expect("an uptime segment");
        assert_eq!(uptime.label, "");
        assert!(
            uptime.detail.starts_with("1:30"),
            "the earlier start wins: {}",
            uptime.detail
        );
    }

    #[test]
    fn nothing_live_means_no_uptime_at_all() {
        let stats = stats_of(vec![(Platform::Twitch, offline())]);
        let segments = strip(&[Platform::Twitch], &stats, None);
        assert_eq!(segments.len(), 1, "{segments:?}");
    }
}
