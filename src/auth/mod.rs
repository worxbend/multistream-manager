//! Authentication: obtaining, storing and silently renewing OAuth tokens.

pub mod oauth;
pub mod store;

use anyhow::{bail, Context, Result};

use crate::config::Config;
use crate::model::Platform;
use oauth::ProviderSpec;
use store::{TokenSet, TokenStore};

/// The OAuth details for a platform.
pub fn spec_for(platform: Platform) -> ProviderSpec {
    match platform {
        Platform::Twitch => ProviderSpec::twitch(),
        Platform::YouTube => ProviderSpec::youtube(),
    }
}

/// The client id and secret for a platform, pulled out of the config.
fn credentials(config: &Config, platform: Platform) -> (String, String) {
    match platform {
        Platform::Twitch => (
            config.twitch.client_id.clone(),
            config.twitch.client_secret.clone(),
        ),
        Platform::YouTube => (
            config.youtube.client_id.clone(),
            config.youtube.client_secret.clone(),
        ),
    }
}

/// Run the interactive browser login for one platform and save the result.
pub async fn login(config: &Config, platform: Platform) -> Result<()> {
    config.check_credentials(&[platform])?;
    let (client_id, client_secret) = credentials(config, platform);
    let spec = spec_for(platform);

    let tokens = oauth::interactive_login(
        &spec,
        &client_id,
        &client_secret,
        &config.redirect_uri(),
        config.general.oauth_port,
    )
    .await?;

    let mut store = TokenStore::load()?;
    store.set(platform, tokens);
    store.save()?;
    Ok(())
}

/// Return a usable access token for `platform`, renewing it first if needed.
///
/// This is the function every API client calls before making a request. It
/// hides the whole expiry/refresh dance from the rest of the program: callers
/// just ask for a token and get one that works.
pub async fn access_token(config: &Config, platform: Platform) -> Result<String> {
    let mut store = TokenStore::load()?;

    let Some(tokens) = store.get(platform).cloned() else {
        bail!(
            "not logged in to {}. Run `msm login {}` first.",
            platform.label(),
            platform.slug()
        );
    };

    if !tokens.needs_refresh() {
        return Ok(tokens.access_token);
    }

    let Some(refresh_token) = tokens.refresh_token.clone() else {
        bail!(
            "your {} access token has expired and there is no refresh token saved, \
             so it cannot be renewed automatically. Run `msm login {}` again.",
            platform.label(),
            platform.slug()
        );
    };

    tracing::info!(
        platform = platform.slug(),
        "refreshing expired access token"
    );

    let (client_id, client_secret) = credentials(config, platform);
    let spec = spec_for(platform);

    let refreshed = oauth::refresh(&spec, &client_id, &client_secret, &refresh_token)
        .await
        .with_context(|| {
            format!(
                "could not renew your {} access token. Run `msm login {}` to authorise again.",
                platform.label(),
                platform.slug()
            )
        })?;

    let access = refreshed.access_token.clone();
    store.set(platform, refreshed);
    store.save()?;

    Ok(access)
}

/// A one-line summary of a platform's login state, for `msm status`.
pub fn describe(platform: Platform, tokens: Option<&TokenSet>) -> String {
    match tokens {
        None => format!(
            "{:<8} not logged in — run `msm login {}`",
            platform.label(),
            platform.slug()
        ),
        Some(tokens) => {
            let renewable = if tokens.refresh_token.is_some() {
                "renews automatically"
            } else {
                "no refresh token — will need a new login when it expires"
            };
            format!(
                "{:<8} logged in, token valid for {} ({renewable})",
                platform.label(),
                tokens.expires_in_human()
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describe_tells_you_what_to_run_when_logged_out() {
        let text = describe(Platform::Twitch, None);
        assert!(text.contains("msm login twitch"));
    }

    #[test]
    fn describe_warns_when_there_is_no_refresh_token() {
        let tokens = TokenSet::new("a".into(), None, Some(3600), vec![]);
        let text = describe(Platform::YouTube, Some(&tokens));
        assert!(text.contains("no refresh token"));
    }

    #[test]
    fn credentials_are_read_from_the_matching_config_section() {
        let mut config = Config::default();
        config.twitch.client_id = "tw".into();
        config.youtube.client_id = "yt".into();

        assert_eq!(credentials(&config, Platform::Twitch).0, "tw");
        assert_eq!(credentials(&config, Platform::YouTube).0, "yt");
    }
}
