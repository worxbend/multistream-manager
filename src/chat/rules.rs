//! Highlight rules: the messages you asked to be shown, not hidden.
//!
//! The digit filters already in the chat pane *exclude* — pressing `3` hides
//! everything that is not a paid event. That is the right tool for reading a
//! busy chat backwards, and the wrong one for the live case, where you want
//! everything on screen and one line to catch your eye.
//!
//! Chatterino draws that distinction and gets it right: highlights promote,
//! filters exclude, and the two are separate systems. This is the same idea
//! at the size a config file can express — a list of rules in
//! `[[chat.highlight]]`, each saying what to match and what to do about it,
//! with an ignore list evaluated first so your own bots never trip an alert.
//!
//! Deliberately no regular expressions. Adding a regex engine to a project
//! that vets its dependencies for licences and advisories is a real cost, and
//! phrase, user, badge and event matching covers what people actually write.
//! When a phrase is not enough the answer is a second rule, not a language.

use serde::{Deserialize, Serialize};

use super::ChatMessage;

/// What a rule looks at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Match {
    /// A phrase anywhere in the message text, case-insensitively.
    #[default]
    Phrase,
    /// A whole word in the message text — `"cat"` will not match "category".
    Word,
    /// The author's login or display name, case-insensitively.
    User,
    /// A badge the author carries: `moderator`, `vip`, `subscriber`,
    /// `member`, `broadcaster`.
    Badge,
    /// The kind of row: `paid`, `membership`, `notice`, `action`.
    Event,
}

/// One highlight rule.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Rule {
    /// What to look at.
    #[serde(rename = "match")]
    pub match_on: Match,
    /// What to look for. Empty never matches — a rule with no pattern would
    /// otherwise highlight the entire chat.
    pub pattern: String,
    /// Raise a desktop notification when this rule matches.
    ///
    /// Off by default: a rule is usually written to make something *visible*,
    /// and a notification for every match of a common word is how somebody
    /// learns to ignore their notifications.
    pub notify: bool,
}

impl Default for Rule {
    fn default() -> Self {
        Self {
            match_on: Match::Phrase,
            pattern: String::new(),
            notify: false,
        }
    }
}

impl Rule {
    /// Whether this rule picks out `msg`.
    pub fn matches(&self, msg: &ChatMessage) -> bool {
        let needle = self.pattern.trim().to_lowercase();
        if needle.is_empty() {
            return false;
        }

        match self.match_on {
            Match::Phrase => msg.text.to_lowercase().contains(&needle),
            Match::Word => contains_word(&msg.text.to_lowercase(), &needle),
            Match::User => {
                msg.author.login.eq_ignore_ascii_case(&needle)
                    || msg.author.display_name.eq_ignore_ascii_case(&needle)
            }
            Match::Badge => msg
                .author
                .badges
                .iter()
                .any(|badge| badge.set.eq_ignore_ascii_case(&needle)),
            Match::Event => msg.kind.as_str() == needle,
        }
    }
}

/// The rules in force, plus the people never to highlight.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Highlights {
    /// Logins never highlighted, whatever the rules say.
    ///
    /// Evaluated first and short-circuiting. This is what stops your own
    /// bot's every announcement lighting up the pane — which is the first
    /// thing that happens to anybody who writes a rule for their own channel
    /// name.
    pub ignore: Vec<String>,
    /// The rules themselves, in order. The first match wins, so a more
    /// specific rule belongs above a broader one.
    #[serde(rename = "highlight")]
    pub rules: Vec<Rule>,
}

/// What a message matched, if anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hit {
    /// Which rule, so the caller can say which one and act on its settings.
    pub index: usize,
    pub notify: bool,
}

impl Highlights {
    /// Whether anything is configured at all, so the common case costs
    /// nothing.
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// The first rule that picks out `msg`.
    pub fn check(&self, msg: &ChatMessage) -> Option<Hit> {
        if self.rules.is_empty() {
            return None;
        }
        // Never highlight your own echo: a rule on your own name would fire
        // on everything you send.
        if msg.local_echo {
            return None;
        }
        if self
            .ignore
            .iter()
            .any(|login| !login.trim().is_empty() && login.eq_ignore_ascii_case(&msg.author.login))
        {
            return None;
        }

        self.rules
            .iter()
            .position(|rule| rule.matches(msg))
            .map(|index| Hit {
                index,
                notify: self.rules[index].notify,
            })
    }
}

/// Whether `needle` appears in `hay` as a whole word.
///
/// Both are expected lowercase. Shared with `state::mentions`, which asks
/// the same question about an `@mention`.
fn contains_word(hay: &str, needle: &str) -> bool {
    super::state::contains_word_boundary(hay, needle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::{Badge, ChatAuthor, MessageKind};

    fn message(text: &str) -> ChatMessage {
        ChatMessage {
            id: "m1".into(),
            timestamp: None,
            author: ChatAuthor {
                id: "u1".into(),
                login: "someone".into(),
                display_name: "Someone".into(),
                badges: vec![],
                color_hint: None,
            },
            text: text.into(),
            kind: MessageKind::Chat,
            deleted: false,
            historical: false,
            local_echo: false,
            meta: None,
        }
    }

    fn rule(match_on: Match, pattern: &str) -> Rule {
        Rule {
            match_on,
            pattern: pattern.into(),
            notify: false,
        }
    }

    #[test]
    fn a_phrase_matches_anywhere_and_ignores_case() {
        let highlights = Highlights {
            ignore: vec![],
            rules: vec![rule(Match::Phrase, "giveaway")],
        };
        assert!(highlights.check(&message("is there a GIVEAWAY?")).is_some());
        assert!(highlights.check(&message("nothing here")).is_none());
    }

    /// The difference people actually want: `cat` should not fire on
    /// "category", which is the first thing that goes wrong with substring
    /// matching.
    #[test]
    fn a_word_rule_does_not_match_inside_a_longer_word() {
        let highlights = Highlights {
            ignore: vec![],
            rules: vec![rule(Match::Word, "cat")],
        };
        assert!(highlights.check(&message("look at my cat")).is_some());
        assert!(highlights
            .check(&message("what category is this"))
            .is_none());
        // Punctuation is a boundary.
        assert!(highlights.check(&message("a cat, obviously")).is_some());
    }

    #[test]
    fn a_user_rule_matches_the_login_or_the_display_name() {
        let highlights = Highlights {
            ignore: vec![],
            rules: vec![rule(Match::User, "SOMEONE")],
        };
        assert!(highlights.check(&message("anything")).is_some());
    }

    #[test]
    fn a_badge_rule_matches_what_the_author_carries() {
        let highlights = Highlights {
            ignore: vec![],
            rules: vec![rule(Match::Badge, "moderator")],
        };
        let mut msg = message("hello");
        assert!(highlights.check(&msg).is_none());

        msg.author.badges = vec![Badge {
            set: "moderator".into(),
            id: "1".into(),
            info: String::new(),
        }];
        assert!(highlights.check(&msg).is_some());
    }

    #[test]
    fn an_event_rule_matches_the_kind_of_row() {
        let highlights = Highlights {
            ignore: vec![],
            rules: vec![rule(Match::Event, "paid")],
        };
        let mut msg = message("thanks!");
        assert!(highlights.check(&msg).is_none());
        msg.kind = MessageKind::Paid;
        assert!(highlights.check(&msg).is_some());
    }

    /// The first thing that happens to anybody who writes a rule for their
    /// own channel name is that their own bot lights up the pane.
    #[test]
    fn the_ignore_list_wins_over_every_rule() {
        let highlights = Highlights {
            ignore: vec!["mybot".into()],
            rules: vec![rule(Match::Phrase, "giveaway")],
        };
        let mut msg = message("the giveaway ends in 5 minutes");
        assert!(highlights.check(&msg).is_some());

        msg.author.login = "MyBot".into();
        assert!(
            highlights.check(&msg).is_none(),
            "the ignore list is evaluated first and short-circuits"
        );
    }

    /// A rule on your own name would otherwise fire on everything you send.
    #[test]
    fn your_own_messages_are_never_highlighted() {
        let highlights = Highlights {
            ignore: vec![],
            rules: vec![rule(Match::Phrase, "hello")],
        };
        let mut msg = message("hello everyone");
        assert!(highlights.check(&msg).is_some());
        msg.local_echo = true;
        assert!(highlights.check(&msg).is_none());
    }

    /// An empty pattern would highlight the entire chat.
    #[test]
    fn a_rule_with_no_pattern_never_matches() {
        let highlights = Highlights {
            ignore: vec![],
            rules: vec![rule(Match::Phrase, "   ")],
        };
        assert!(highlights.check(&message("anything at all")).is_none());
    }

    /// The first match wins, so a specific rule can sit above a broad one and
    /// its own `notify` setting is the one that applies.
    #[test]
    fn the_first_matching_rule_wins() {
        let highlights = Highlights {
            ignore: vec![],
            rules: vec![
                Rule {
                    match_on: Match::Word,
                    pattern: "raid".into(),
                    notify: true,
                },
                rule(Match::Phrase, "a"),
            ],
        };
        let hit = highlights
            .check(&message("a raid is coming"))
            .expect("a hit");
        assert_eq!(hit.index, 0);
        assert!(hit.notify);
    }
}
