//! Persisting OAuth tokens between runs.
//!
//! Tokens live in `tokens.json` next to the config, written with owner-only
//! permissions. A refresh token grants ongoing access to your channel, so treat
//! that file exactly as you would treat a password.

use anyhow::{Context, Result};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::model::Platform;
use crate::paths;

/// One platform's tokens.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenSet {
    /// Sent on every API request. Typically valid for an hour or so.
    pub access_token: String,
    /// Used to obtain a new access token without user interaction. Long-lived.
    pub refresh_token: Option<String>,
    /// Absolute time the access token stops working. Absolute rather than
    /// "seconds remaining" so it survives the program being closed and reopened.
    pub expires_at: Option<DateTime<Utc>>,
    /// The permissions the provider actually granted. Kept so the application
    /// can notice when a token predates a newly added feature's scope.
    #[serde(default)]
    pub scopes: Vec<String>,
}

impl TokenSet {
    /// Build a token set from a fresh token endpoint response.
    ///
    /// `expires_in_secs` comes straight off the wire, so it is treated as
    /// untrusted. `Duration::seconds` panics on values chrono cannot represent,
    /// and adding a huge duration to "now" can overflow the date type as well,
    /// so both steps are done with their non-panicking `try_`/`checked_`
    /// equivalents. A value that does not survive them is recorded as "no expiry
    /// information", which the rest of this type already handles: the token is
    /// used until the API rejects it with a 401.
    pub fn new(
        access_token: String,
        refresh_token: Option<String>,
        expires_in_secs: Option<i64>,
        scopes: Vec<String>,
    ) -> Self {
        Self {
            access_token,
            refresh_token,
            expires_at: expires_in_secs.and_then(expiry_from_now),
            scopes,
        }
    }

    /// Whether the access token needs renewing.
    ///
    /// A 60 second safety margin is applied so a token does not expire in flight
    /// between us checking it and the API receiving the request.
    pub fn needs_refresh(&self) -> bool {
        match self.expires_at {
            // No expiry information: assume it is fine and let a 401 tell us
            // otherwise. Twitch tokens always carry an expiry, so this is rare.
            None => false,
            Some(at) => Utc::now() + Duration::seconds(60) >= at,
        }
    }

    /// Human-readable time remaining, for the status display.
    pub fn expires_in_human(&self) -> String {
        let Some(at) = self.expires_at else {
            return "unknown".to_string();
        };
        let remaining = at - Utc::now();
        if remaining.num_seconds() <= 0 {
            return "expired".to_string();
        }
        let hours = remaining.num_hours();
        let minutes = remaining.num_minutes() % 60;
        if hours > 0 {
            format!("{hours}h {minutes}m")
        } else {
            format!("{minutes}m")
        }
    }
}

/// Turn "valid for this many seconds" into an absolute instant, or `None` if the
/// number is out of range.
///
/// Providers are supposed to send something like 3600. A broken provider, a
/// misbehaving proxy, or a hostile response can send anything at all, and the
/// naive `Utc::now() + Duration::seconds(n)` crashes the whole program on a
/// number chrono cannot hold — including in the background token refresh, which
/// would take the dashboard down mid-stream.
fn expiry_from_now(secs: i64) -> Option<DateTime<Utc>> {
    let delta = Duration::try_seconds(secs)?;
    Utc::now().checked_add_signed(delta)
}

/// The whole `tokens.json`: a map from platform slug to that platform's tokens.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TokenStore {
    tokens: BTreeMap<String, TokenSet>,
}

impl TokenStore {
    /// Read the token file, treating "not there yet" as "no tokens".
    pub fn load() -> Result<Self> {
        let path = paths::token_file()?;
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        if text.trim().is_empty() {
            return Ok(Self::default());
        }
        serde_json::from_str(&text).with_context(|| {
            format!(
                "parsing {}. If it has been corrupted, delete it and run `msm login` again.",
                path.display()
            )
        })
    }

    /// Write the token file back out with owner-only permissions.
    pub fn save(&self) -> Result<()> {
        let path = paths::token_file()?;
        let json = serde_json::to_string_pretty(self).context("serialising tokens")?;
        paths::write_secret_file(&path, &json)
    }

    pub fn get(&self, platform: Platform) -> Option<&TokenSet> {
        self.tokens.get(platform.slug())
    }

    pub fn set(&mut self, platform: Platform, tokens: TokenSet) {
        self.tokens.insert(platform.slug().to_string(), tokens);
    }

    /// Forget one platform's tokens. Used by `msm logout`.
    pub fn remove(&mut self, platform: Platform) -> bool {
        self.tokens.remove(platform.slug()).is_some()
    }

    /// Which platforms currently have any stored credentials.
    pub fn authorised_platforms(&self) -> Vec<Platform> {
        Platform::ALL
            .into_iter()
            .filter(|p| self.tokens.contains_key(p.slug()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_token_expiring_within_the_safety_margin_is_due_for_refresh() {
        let soon = TokenSet {
            access_token: "a".into(),
            refresh_token: Some("r".into()),
            // 30 seconds left is inside the 60 second margin.
            expires_at: Some(Utc::now() + Duration::seconds(30)),
            scopes: vec![],
        };
        assert!(soon.needs_refresh());
    }

    #[test]
    fn a_token_with_plenty_of_life_left_is_not_refreshed() {
        let fresh = TokenSet::new("a".into(), Some("r".into()), Some(3600), vec![]);
        assert!(!fresh.needs_refresh());
    }

    #[test]
    fn a_token_with_no_stated_expiry_is_left_alone() {
        let unknown = TokenSet {
            access_token: "a".into(),
            refresh_token: None,
            expires_at: None,
            scopes: vec![],
        };
        assert!(!unknown.needs_refresh());
    }

    #[test]
    fn expiry_is_rendered_in_hours_and_minutes() {
        let tokens = TokenSet::new("a".into(), None, Some(3 * 3600 + 25 * 60), vec![]);
        let text = tokens.expires_in_human();
        assert!(text.starts_with("3h"), "got {text:?}");
    }

    #[test]
    fn an_already_expired_token_says_so() {
        let tokens = TokenSet {
            access_token: "a".into(),
            refresh_token: None,
            expires_at: Some(Utc::now() - Duration::hours(1)),
            scopes: vec![],
        };
        assert_eq!(tokens.expires_in_human(), "expired");
    }

    #[test]
    fn the_store_round_trips_through_json() {
        let mut store = TokenStore::default();
        store.set(
            Platform::Twitch,
            TokenSet::new("access".into(), Some("refresh".into()), Some(3600), vec![]),
        );

        let json = serde_json::to_string(&store).unwrap();
        let back: TokenStore = serde_json::from_str(&json).unwrap();

        assert_eq!(back.get(Platform::Twitch).unwrap().access_token, "access");
        assert!(back.get(Platform::YouTube).is_none());
        assert_eq!(back.authorised_platforms(), vec![Platform::Twitch]);
    }

    /// `expires_in` is a number chosen by the remote server. An absurd one used
    /// to panic inside chrono and abort the process — during `msm login`, and
    /// worse, during the silent refresh that runs while the dashboard is up.
    #[test]
    fn an_out_of_range_expiry_is_ignored_rather_than_crashing() {
        for absurd in [i64::MAX, i64::MIN, 9_300_000_000_000_000] {
            let tokens = TokenSet::new("a".into(), None, Some(absurd), vec![]);
            assert!(
                tokens.expires_at.is_none(),
                "{absurd} should be discarded, not stored"
            );
            // No expiry information means "use it until the API says otherwise".
            assert!(!tokens.needs_refresh());
            assert_eq!(tokens.expires_in_human(), "unknown");
        }
    }

    #[test]
    fn a_normal_expiry_is_still_recorded() {
        let tokens = TokenSet::new("a".into(), None, Some(3600), vec![]);
        let expires_at = tokens.expires_at.expect("an hour is perfectly valid");
        let remaining = (expires_at - Utc::now()).num_seconds();
        assert!((3500..=3600).contains(&remaining), "{remaining}s");
        assert!(!tokens.needs_refresh());
    }
}
