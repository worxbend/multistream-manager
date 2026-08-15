//! Authentication: obtaining, storing and silently renewing OAuth tokens.

pub mod oauth;
pub mod store;

use anyhow::{bail, Context, Result};

use crate::config::Config;
use crate::model::Platform;
use oauth::ProviderSpec;
use store::{StoreLock, TokenSet, TokenStore};

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

    // The lock covers the whole load-set-save cycle, so a token refresh
    // running in another msm process cannot save a stale snapshot over this
    // brand-new login (or vice versa).
    let _lock = lock_store().await?;
    let mut store = load_store().await?;
    store.set(platform, tokens);
    save_store(store).await?;
    Ok(())
}

/// Take the cross-process token-store lock without blocking the async runtime.
///
/// Acquiring can wait on another process that is mid-refresh, so like the
/// load/save helpers below it runs on the blocking pool.
async fn lock_store() -> Result<StoreLock> {
    tokio::task::spawn_blocking(StoreLock::acquire)
        .await
        .context("locking the token store")?
}

/// Read the token file without blocking the async runtime.
///
/// `TokenStore::load` and `save` are ordinary blocking filesystem calls, and
/// `save` includes an `fsync` that can stall for tens of milliseconds on a busy
/// or networked disk. Called directly from an async task they block a tokio
/// worker thread — a thread shared with everything else the program is doing —
/// so they are handed to the blocking pool instead.
async fn load_store() -> Result<TokenStore> {
    tokio::task::spawn_blocking(TokenStore::load)
        .await
        .context("reading the saved tokens")?
}

/// Write the token file without blocking the async runtime. See [`load_store`].
async fn save_store(store: TokenStore) -> Result<()> {
    tokio::task::spawn_blocking(move || store.save())
        .await
        .context("saving the renewed tokens")?
}

/// Tokens for several platforms at once, reading and writing the file once.
///
/// This is the function the engine calls before every batch of API work; it
/// hides the whole expiry/refresh dance from the rest of the program. Asking
/// per platform would mean re-reading `tokens.json` once per platform, on
/// every statistics poll, just to discover that nothing had expired. This
/// reads the file once and writes it back only if something was actually
/// renewed.
///
/// A platform whose token cannot be produced gets its own error rather than
/// spoiling the others: a expired YouTube login should not stop Twitch working.
pub async fn access_tokens(
    config: &Config,
    platforms: &[Platform],
) -> Vec<(Platform, Result<String>)> {
    // Held for the whole load-refresh-save cycle. Without it, this process
    // could spend seconds in a network refresh and then save a snapshot that
    // erases whatever another msm process wrote in the meantime — such as the
    // fresh tokens of an `msm login` running in a second terminal.
    let lock = match lock_store().await {
        Ok(lock) => lock,
        Err(err) => {
            let message = format!("{err:#}");
            return platforms
                .iter()
                .map(|&platform| (platform, Err(anyhow::anyhow!(message.clone()))))
                .collect();
        }
    };

    let mut store = match load_store().await {
        Ok(store) => store,
        // Nothing can be renewed without the file, so every platform gets the
        // same explanation.
        Err(err) => {
            let message = format!("{err:#}");
            return platforms
                .iter()
                .map(|&platform| (platform, Err(anyhow::anyhow!(message.clone()))))
                .collect();
        }
    };

    let mut results = Vec::new();
    let mut renewed_any = false;
    for &platform in platforms {
        match token_from(config, platform, &mut store).await {
            Ok((token, renewed)) => {
                renewed_any |= renewed;
                results.push((platform, Ok(token)));
            }
            Err(err) => results.push((platform, Err(err))),
        }
    }

    if renewed_any {
        if let Err(err) = save_store(store).await {
            tracing::warn!(error = %format!("{err:#}"), "could not save the renewed tokens");
        }
    }
    drop(lock);

    results
}

/// The token for one platform, renewing it in `store` if it has aged out.
///
/// Returns the token and whether `store` was changed and so needs writing back.
async fn token_from(
    config: &Config,
    platform: Platform,
    store: &mut TokenStore,
) -> Result<(String, bool)> {
    let Some(tokens) = store.get(platform).cloned() else {
        bail!(
            "not logged in to {}. Run `msm login {}` first.",
            platform.label(),
            platform.slug()
        );
    };

    if !tokens.needs_refresh() {
        return Ok((tokens.access_token, false));
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
    Ok((access, true))
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

    /// The engine asks for every platform's token before each batch of work.
    /// This must read `tokens.json` once, not once per platform, and one
    /// platform that is not logged in must not spoil the other.
    #[tokio::test]
    async fn tokens_for_several_platforms_come_from_one_read() {
        let scratch = crate::paths::test_support::ScratchConfigDir::new("auth-access-tokens");

        // Only Twitch is logged in, with a token that is nowhere near expiring.
        let mut store = TokenStore::default();
        store.set(
            Platform::Twitch,
            TokenSet::new("twitch-token".into(), Some("r".into()), Some(3600), vec![]),
        );
        store.save().expect("writing the scratch token file");

        let config = Config::default();
        let results = access_tokens(&config, &Platform::ALL).await;

        assert_eq!(results.len(), Platform::ALL.len());
        let twitch = results
            .iter()
            .find(|(p, _)| *p == Platform::Twitch)
            .unwrap();
        assert_eq!(twitch.1.as_deref().unwrap(), "twitch-token");

        let youtube = results
            .iter()
            .find(|(p, _)| *p == Platform::YouTube)
            .unwrap();
        let err = youtube.1.as_ref().unwrap_err().to_string();
        assert!(err.contains("msm login youtube"), "unhelpful error: {err}");

        // Nothing needed renewing, so the file must not have been rewritten.
        let written = std::fs::read_to_string(scratch.path().join("tokens.json")).unwrap();
        assert!(written.contains("twitch-token"));
        assert!(!written.contains("youtube"));
    }
}
