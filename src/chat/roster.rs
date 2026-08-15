//! Chatter roster and @mention autocomplete helpers.
//!
//! Ported behavior from `twi` (repo ../twi, commit
//! 7c6ad6bbbc3dec1b6af2ddbd03b55f96c0c162cf, files
//! internal/app/roster.go and internal/app/mention_autocomplete.go).
//!
//! One deliberate deviation from twi: **this roster is speakers-only.** twi
//! feeds its roster from two signals — JOIN/PART membership events and chat
//! messages. Our Twitch transport is the `twitch-irc` crate (v6), which
//! cannot request the `twitch.tv/membership` capability, so JOIN/PART never
//! arrives here; the only signal is people actually speaking. That is also
//! the *better* signal for what the roster backs (@mention completion and
//! identity metadata), and it is all YouTube ever offers anyway. twi's
//! `Present` flag, `membershipSeen`, and `activeCount` therefore have no
//! Rust counterpart.
//!
//! A second adaptation: entries are ordered by a caller-supplied
//! monotonically increasing sequence number instead of wall-clock
//! timestamps (twi sorts on `LastSeen` `time.Time`). The chat state applies
//! events in arrival order already, so a sequence carries the same "most
//! recently seen" meaning while keeping eviction and ranking fully
//! deterministic under test — no clock, no ties.

use std::collections::HashMap;

use crate::chat::ChatAuthor;

/// twi's `rosterMaxEntries`: bounds per-chat memory. Busy channels cycle
/// through far more chatters than anyone will ever mention, so the least
/// recently seen entries are evicted past this point.
pub const ROSTER_MAX_ENTRIES: usize = 4096;

/// Everything the roster knows about one chatter in one chat.
///
/// Role flags are **sticky for the session** (twi's `applyBadgeRoles` rule):
/// Twitch omits badges in some contexts, and a message arriving without them
/// must not silently demote a known moderator.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RosterEntry {
    /// The platform's stable author id — the map key, because it is the one
    /// thing a chatter cannot change.
    pub id: String,
    pub login: String,
    pub display_name: String,
    /// The sequence number of the last message observed from this author.
    /// Higher = more recent; drives both eviction and completion ranking.
    pub last_seen: u64,
    /// Broadcaster (Twitch `broadcaster` badge) or channel owner (YouTube
    /// synthesized `owner` badge).
    pub owner: bool,
    pub moderator: bool,
    /// Twitch subscriber/founder or YouTube member.
    pub member: bool,
    pub vip: bool,
    pub message_count: u64,
}

impl RosterEntry {
    /// The best display form: the display name when known, otherwise the
    /// login (twi's `chatterEntry.name`).
    pub fn name(&self) -> &str {
        if self.display_name.trim().is_empty() {
            &self.login
        } else {
            &self.display_name
        }
    }
}

/// A per-chat map of authors observed speaking, capped with LRU eviction.
#[derive(Debug, Default)]
pub struct Roster {
    entries: HashMap<String, RosterEntry>,
}

impl Roster {
    pub fn new() -> Self {
        Self::default()
    }

    /// Records (or updates) an author from a message they sent. `seq` must be
    /// monotonically increasing across calls — the chat state's per-message
    /// counter is the intended source.
    pub fn observe(&mut self, author: &ChatAuthor, seq: u64) {
        // twi keys on lowercase login; we key on the stable id (the model's
        // documented identity key) and fall back to login for the rare
        // author delivered without one, so nobody is dropped.
        let key = if author.id.is_empty() {
            let login = author.login.trim().to_lowercase();
            if login.is_empty() {
                return;
            }
            login
        } else {
            author.id.clone()
        };

        let entry = self.entries.entry(key).or_default();
        entry.id = author.id.clone();
        // Identity fields refresh on every message (messages are the only
        // source of display names), but never regress to empty.
        if !author.login.trim().is_empty() {
            entry.login = author.login.trim().to_lowercase();
        }
        if !author.display_name.trim().is_empty() {
            entry.display_name = author.display_name.trim().to_string();
        }
        entry.last_seen = entry.last_seen.max(seq);
        entry.message_count += 1;

        // twi's applyBadgeRoles: badge sets map onto sticky role flags.
        for badge in &author.badges {
            match badge.set.trim().to_lowercase().as_str() {
                // Twitch's broadcaster and YouTube's synthesized owner are
                // the same role: the person whose chat this is.
                "broadcaster" | "owner" => entry.owner = true,
                "moderator" => entry.moderator = true,
                "vip" => entry.vip = true,
                // Founders are subscribers with a different badge; YouTube
                // members are the same idea on the other platform.
                "subscriber" | "founder" | "member" => entry.member = true,
                _ => {}
            }
        }

        self.evict_oldest();
    }

    /// twi's `evictOldest`: drops least-recently-seen entries past the cap.
    fn evict_oldest(&mut self) {
        while self.entries.len() > ROSTER_MAX_ENTRIES {
            // Ties on last_seen cannot happen when seq is monotonic, but a
            // deterministic id tiebreak keeps this total anyway.
            let oldest = self
                .entries
                .iter()
                .min_by_key(|&(key, entry)| (entry.last_seen, key.clone()))
                .map(|(key, _)| key.clone());
            match oldest {
                Some(key) => {
                    self.entries.remove(&key);
                }
                // Unreachable: the loop condition guarantees the map is
                // non-empty, so min_by_key returned Some.
                None => break,
            }
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Completion candidates for `prefix`, at most `max`.
    ///
    /// Ranking (twi's `completions`, extended by its documented rule that
    /// prefix matches beat substring matches): login-or-display-name
    /// *prefix* matches first, then substring matches; within each class,
    /// most recently seen first. Matching is case-insensitive. An empty
    /// prefix lists everyone, most recent first.
    pub fn complete(&self, prefix: &str, max: usize) -> Vec<&RosterEntry> {
        if max == 0 {
            return Vec::new();
        }
        let needle = prefix.trim().trim_start_matches('@').to_lowercase();
        let mut matches: Vec<(bool, &RosterEntry)> = Vec::new();
        for entry in self.entries.values() {
            let login = entry.login.to_lowercase();
            let display = entry.display_name.to_lowercase();
            let (is_prefix, is_sub) = if needle.is_empty() {
                (true, true)
            } else {
                (
                    login.starts_with(&needle) || display.starts_with(&needle),
                    login.contains(&needle) || display.contains(&needle),
                )
            };
            if is_sub || is_prefix {
                matches.push((is_prefix, entry));
            }
        }
        // Prefix class first, then recency descending, then login for a
        // deterministic order on (impossible-with-monotonic-seq) ties.
        matches.sort_by(|a, b| {
            b.0.cmp(&a.0)
                .then(b.1.last_seen.cmp(&a.1.last_seen))
                .then_with(|| a.1.login.cmp(&b.1.login))
        });
        matches.truncate(max);
        matches.into_iter().map(|(_, entry)| entry).collect()
    }
}

/// The trailing @mention word of a composer draft, without its leading `@`,
/// or `None` when the caret is not in one.
///
/// Port of twi's `composerMentionPrefix`: the composer is append-only (the
/// caret is always at the end), so only the trailing word matters. The `@`
/// must start a word — the character before it, if any, must be whitespace —
/// so an email-like `a@b` never triggers completion. Everything after the
/// `@` must be a mention character (letter, digit, or `_`).
pub fn mention_prefix(composer: &str) -> Option<&str> {
    if composer.is_empty() {
        return None;
    }
    let at = composer.rfind('@')?;
    if at > 0 {
        let before = composer[..at].chars().next_back();
        match before {
            Some(c) if !c.is_whitespace() => return None,
            _ => {}
        }
    }
    let prefix = &composer[at + 1..];
    if prefix.chars().all(is_mention_char) {
        Some(prefix)
    } else {
        None
    }
}

/// twi's `isMentionRune`.
fn is_mention_char(c: char) -> bool {
    c == '_' || c.is_alphanumeric()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::Badge;

    fn author(id: &str, login: &str, display: &str, badge_sets: &[&str]) -> ChatAuthor {
        ChatAuthor {
            id: id.to_string(),
            login: login.to_string(),
            display_name: display.to_string(),
            badges: badge_sets
                .iter()
                .map(|set| Badge {
                    set: set.to_string(),
                    ..Badge::default()
                })
                .collect(),
            color_hint: None,
        }
    }

    #[test]
    fn observe_records_identity_and_counts() {
        let mut roster = Roster::new();
        let a = author("1", "Alice", "AliceDisplay", &[]);
        roster.observe(&a, 1);
        roster.observe(&a, 2);
        let got = roster.complete("", 10);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].login, "alice");
        assert_eq!(got[0].display_name, "AliceDisplay");
        assert_eq!(got[0].message_count, 2);
        assert_eq!(got[0].last_seen, 2);
    }

    #[test]
    fn lru_eviction_at_cap_drops_least_recently_seen() {
        let mut roster = Roster::new();
        for i in 0..ROSTER_MAX_ENTRIES as u64 {
            roster.observe(&author(&format!("id{i}"), &format!("u{i}"), "", &[]), i);
        }
        assert_eq!(roster.len(), ROSTER_MAX_ENTRIES);
        // id0 is the least recently seen; one more entry evicts exactly it.
        roster.observe(
            &author("new", "newcomer", "", &[]),
            ROSTER_MAX_ENTRIES as u64,
        );
        assert_eq!(roster.len(), ROSTER_MAX_ENTRIES);
        assert!(roster.complete("u0", 10).iter().all(|e| e.login != "u0"));
        assert_eq!(roster.complete("newcomer", 1)[0].login, "newcomer");
        // But an evictee who speaks again comes right back.
        roster.observe(&author("id1", "u1", "", &[]), ROSTER_MAX_ENTRIES as u64 + 1);
        assert_eq!(roster.complete("u1", 1)[0].login, "u1");
    }

    #[test]
    fn roles_derive_from_badges_and_are_sticky() {
        let mut roster = Roster::new();
        roster.observe(&author("1", "mod", "", &["moderator", "subscriber"]), 1);
        // A later badge-less message must not demote them.
        roster.observe(&author("1", "mod", "", &[]), 2);
        let entry = roster.complete("mod", 1)[0];
        assert!(entry.moderator);
        assert!(entry.member);
        assert!(!entry.owner);
        assert!(!entry.vip);

        roster.observe(&author("2", "boss", "", &["broadcaster"]), 3);
        assert!(roster.complete("boss", 1)[0].owner);
        roster.observe(&author("3", "ytowner", "", &["owner"]), 4);
        assert!(roster.complete("ytowner", 1)[0].owner);
        roster.observe(&author("4", "fan", "", &["vip", "founder"]), 5);
        let fan = roster.complete("fan", 1)[0];
        assert!(fan.vip && fan.member);
        roster.observe(&author("5", "ytm", "", &["member"]), 6);
        assert!(roster.complete("ytm", 1)[0].member);
    }

    #[test]
    fn prefix_matches_rank_before_substring_matches() {
        let mut roster = Roster::new();
        // "bo" is a substring of "turbo" but a prefix of "bob"; bob must win
        // even though turbo spoke more recently.
        roster.observe(&author("1", "bob", "", &[]), 1);
        roster.observe(&author("2", "turbo", "", &[]), 2);
        let got = roster.complete("bo", 10);
        assert_eq!(
            got.iter().map(|e| e.login.as_str()).collect::<Vec<_>>(),
            vec!["bob", "turbo"]
        );
    }

    #[test]
    fn within_a_class_most_recently_seen_first() {
        let mut roster = Roster::new();
        roster.observe(&author("1", "anna", "", &[]), 1);
        roster.observe(&author("2", "andy", "", &[]), 2);
        roster.observe(&author("3", "anton", "", &[]), 3);
        let got = roster.complete("an", 10);
        assert_eq!(
            got.iter().map(|e| e.login.as_str()).collect::<Vec<_>>(),
            vec!["anton", "andy", "anna"]
        );
        // max caps the list.
        assert_eq!(roster.complete("an", 2).len(), 2);
    }

    #[test]
    fn display_name_matches_too_and_matching_is_case_insensitive() {
        let mut roster = Roster::new();
        roster.observe(&author("1", "xx_1", "NightBot", &[]), 1);
        let got = roster.complete("night", 10);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].display_name, "NightBot");
        assert_eq!(roster.complete("NIGHT", 10).len(), 1);
    }

    #[test]
    fn empty_prefix_lists_everyone_most_recent_first() {
        let mut roster = Roster::new();
        roster.observe(&author("1", "a", "", &[]), 1);
        roster.observe(&author("2", "b", "", &[]), 2);
        let got = roster.complete("", 10);
        assert_eq!(
            got.iter().map(|e| e.login.as_str()).collect::<Vec<_>>(),
            vec!["b", "a"]
        );
    }

    #[test]
    fn mention_prefix_table() {
        // (composer, expected)
        let cases: &[(&str, Option<&str>)] = &[
            ("", None),
            ("hello", None),
            ("@", Some("")),         // empty prefix right after @
            ("@bo", Some("bo")),     // start of string
            ("hi @bo", Some("bo")),  // after a space
            ("hi\t@bo", Some("bo")), // after other whitespace
            ("a@b", None),           // mid-word @ never triggers (email rule)
            ("hi a@b", None),        // still mid-word
            ("@bob smith", None),    // space after the word ends the mention
            ("hey @user_1", Some("user_1")),
            ("@Ünïcode", Some("Ünïcode")), // letters beyond ASCII count
            ("@bo!", None),                // punctuation is not a mention char
        ];
        for (input, want) in cases {
            assert_eq!(mention_prefix(input), *want, "input: {input:?}");
        }
    }
}
