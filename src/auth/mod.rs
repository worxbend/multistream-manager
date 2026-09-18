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

/// A platform's OAuth client id and secret.
struct ClientCredentials {
    client_id: String,
    client_secret: String,
}

/// The client id and secret for a platform, pulled out of the config.
fn credentials(config: &Config, platform: Platform) -> ClientCredentials {
    match platform {
        Platform::Twitch => ClientCredentials {
            client_id: config.twitch.client_id(),
            client_secret: config.twitch.client_secret(),
        },
        Platform::YouTube => ClientCredentials {
            client_id: config.youtube.client_id(),
            client_secret: config.youtube.client_secret(),
        },
    }
}

/// The same login, with the progress messages routed somewhere other than
/// standard output — which is what the terminal interface needs, since it owns
/// the screen and cannot have anything printed underneath it.
pub async fn login_with(
    config: &Config,
    platform: Platform,
    add: bool,
    notice: oauth::Notice<'_>,
) -> Result<String> {
    config.check_credentials(&[platform])?;
    let credentials = credentials(config, platform);
    let spec = spec_for(platform);

    let mut tokens = oauth::interactive_login_with(
        &spec,
        &credentials.client_id,
        &credentials.client_secret,
        &config.redirect_uri(),
        config.general.oauth_port,
        notice,
    )
    .await?;

    // Ask the platform who this token belongs to, so account sub-tabs can be
    // labelled and extra accounts keyed by identity. Best-effort for a
    // primary login (the app worked for years without it); required for
    // --add, because without an identity there is nothing to key the extra
    // account by.
    let identity = resolve_identity(platform, &tokens.access_token).await;
    let key = login_key(platform, add, &identity)?;
    if let Ok(identity) = identity {
        tokens.identity = Some(identity);
    }

    // The lock covers the whole load-set-save cycle, so a token refresh
    // running in another msm process cannot save a stale snapshot over this
    // brand-new login (or vice versa).
    let lock = lock_store().await?;
    let mut store = load_store().await?;

    // Logging in with --add using the account that is already the primary
    // would create a confusing duplicate; update the primary instead.
    let final_key = match (add, store.get(platform).and_then(|t| t.identity.as_ref())) {
        (true, Some(primary)) if tokens.identity.as_ref() == Some(primary) => {
            platform.slug().to_string()
        }
        _ => key,
    };
    if final_key == platform.slug() {
        store.set(platform, tokens);
    } else {
        store.set_keyed(final_key.clone(), tokens);
    }
    let (_lock, result) = save_store(store, lock).await?;
    result?;
    Ok(final_key)
}

/// The account key to store a fresh login under.
///
/// Errors only when `add` is set and the identity lookup failed: without an
/// identity there is nothing to key an additional account by.
fn login_key(
    platform: Platform,
    add: bool,
    identity: &Result<store::AccountIdentity>,
) -> Result<String> {
    match (identity, add) {
        (Ok(identity), true) => {
            let suffix = match platform {
                Platform::Twitch => identity.login.clone(),
                Platform::YouTube => identity.id.clone(),
            };
            Ok(format!("{}:{}", platform.slug(), suffix.to_lowercase()))
        }
        (Err(err), true) => Err(anyhow::anyhow!("{err:#}")).context(
            "could not find out which account this login belongs to, and an \
             additional account cannot be stored without knowing that. The \
             login itself succeeded — try again.",
        ),
        (_, false) => Ok(platform.slug().to_string()),
    }
}

/// Ask the platform which account an access token belongs to.
///
/// Twitch: `GET id.twitch.tv/oauth2/validate` (the same endpoint the token
/// preflight uses; note its non-standard `OAuth` authorization scheme).
/// YouTube: `GET youtube/v3/channels?mine=true` (1 quota unit, spent once per
/// login).
async fn resolve_identity(
    platform: Platform,
    access_token: &str,
) -> Result<store::AccountIdentity> {
    let http = crate::backend::http_client().context("for the identity lookup")?;

    match platform {
        Platform::Twitch => {
            #[derive(serde::Deserialize)]
            struct Validate {
                #[serde(default)]
                user_id: String,
                #[serde(default)]
                login: String,
            }
            let response = http
                .get("https://id.twitch.tv/oauth2/validate")
                .header("Authorization", format!("OAuth {access_token}"))
                .send()
                .await
                .context("asking Twitch whose login this is")?;
            if !response.status().is_success() {
                bail!(
                    "Twitch rejected the token validation (HTTP {})",
                    response.status()
                );
            }
            let v: Validate = response
                .json()
                .await
                .context("parsing the Twitch validation response")?;
            Ok(store::AccountIdentity {
                id: v.user_id,
                login: v.login.clone(),
                display_name: v.login,
            })
        }
        Platform::YouTube => {
            #[derive(serde::Deserialize)]
            struct List {
                #[serde(default)]
                items: Vec<Channel>,
            }
            #[derive(serde::Deserialize)]
            struct Channel {
                id: String,
                snippet: Snippet,
            }
            #[derive(serde::Deserialize)]
            struct Snippet {
                #[serde(default)]
                title: String,
            }
            let response = http
                .get("https://www.googleapis.com/youtube/v3/channels?part=snippet&mine=true")
                .bearer_auth(access_token)
                .send()
                .await
                .context("asking YouTube which channel this login belongs to")?;
            if !response.status().is_success() {
                bail!(
                    "YouTube rejected the channel lookup (HTTP {})",
                    response.status()
                );
            }
            let list: List = response
                .json()
                .await
                .context("parsing the YouTube channel response")?;
            let channel = list
                .items
                .into_iter()
                .next()
                .context("this Google account has no YouTube channel")?;
            Ok(store::AccountIdentity {
                id: channel.id,
                login: String::new(),
                display_name: channel.snippet.title,
            })
        }
    }
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
///
/// The lock is moved in and handed back out so that it stays held across the
/// write — releasing it before the file is on disk would reopen exactly the
/// race it exists to prevent.
async fn save_store(store: TokenStore, lock: StoreLock) -> Result<(StoreLock, Result<()>)> {
    tokio::task::spawn_blocking(move || {
        let result = store.save(&lock);
        (lock, result)
    })
    .await
    .context("saving the renewed tokens")
}

/// Read, change and write the token store as one locked operation.
///
/// The whole cycle happens under the cross-process lock, so a change made
/// here cannot be lost to a token refresh running in another copy of the
/// program (or in another task of this one). Anything that edits stored
/// tokens outside a refresh — logging out, forgetting an extra chat account —
/// should go through this rather than doing its own load/save pair.
pub async fn mutate_store<F>(change: F) -> Result<()>
where
    F: FnOnce(&mut TokenStore) + Send + 'static,
{
    let lock = lock_store().await?;
    let mut store = load_store().await?;
    change(&mut store);
    let (_lock, result) = save_store(store, lock).await?;
    result
}

/// Map one error onto every platform, for a failure — like the store lock or
/// the file read — that stops all of them at once rather than just one.
fn same_error_for_all(
    platforms: &[Platform],
    err: anyhow::Error,
) -> Vec<(Platform, Result<String>)> {
    let message = format!("{err:#}");
    platforms
        .iter()
        .map(|&platform| (platform, Err(anyhow::anyhow!(message.clone()))))
        .collect()
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
    // fresh tokens of a login running in a second copy of this program.
    let lock = match lock_store().await {
        Ok(lock) => lock,
        Err(err) => return same_error_for_all(platforms, err),
    };

    let mut store = match load_store().await {
        Ok(store) => store,
        // Nothing can be renewed without the file, so every platform gets the
        // same explanation.
        Err(err) => return same_error_for_all(platforms, err),
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
        let saved = match save_store(store, lock).await {
            Ok((_lock, result)) => result,
            Err(err) => Err(err),
        };
        if let Err(err) = saved {
            // A renewed token that was not written down is a token this
            // process will use once and then lose: the next run reads the old
            // one, which the provider has already rotated away. Reporting it
            // as an error per renewed platform matches what
            // `access_token_for_key` already does, instead of leaving the
            // problem in a log file nobody is looking at mid-stream.
            let message = format!("{err:#}");
            tracing::warn!(error = %message, "could not save the renewed tokens");
            for (_, result) in results.iter_mut() {
                if result.is_ok() {
                    *result = Err(anyhow::anyhow!(
                        "the renewed login could not be saved: {message}"
                    ));
                }
            }
        }
    }

    results
}

/// A usable access token for one stored account key, renewing it if needed.
///
/// Account keys are `twitch` / `youtube` for the primary logins and
/// `twitch:<login>` / `youtube:<channel-id>` for extra chat accounts. This is
/// what the chat adapters call; the engine's batch path stays on
/// [`access_tokens`].
pub async fn access_token_for_key(config: &Config, key: &str) -> Result<String> {
    let slug = key.split(':').next().unwrap_or(key);
    let platform: Platform = slug
        .parse()
        .map_err(|err: String| anyhow::anyhow!(err))
        .with_context(|| format!("account key {key:?} does not name a platform"))?;

    let lock = lock_store().await?;
    let mut store = load_store().await?;
    let (token, renewed) = keyed_token_from(config, platform, key, &mut store).await?;
    if renewed {
        let (_lock, result) = save_store(store, lock).await?;
        result?;
    }
    Ok(token)
}

/// The token for one platform, renewing it in `store` if it has aged out.
///
/// Returns the token and whether `store` was changed and so needs writing back.
async fn token_from(
    config: &Config,
    platform: Platform,
    store: &mut TokenStore,
) -> Result<(String, bool)> {
    keyed_token_from(config, platform, platform.slug(), store).await
}

/// [`token_from`] generalised over the token-store key, so extra chat
/// accounts refresh through the same door as the primaries.
async fn keyed_token_from(
    config: &Config,
    platform: Platform,
    key: &str,
    store: &mut TokenStore,
) -> Result<(String, bool)> {
    let Some(tokens) = store.get_keyed(key).cloned() else {
        if key == platform.slug() {
            bail!(
                "not logged in to {}. Log in under Config → Accounts (alt+5), or on the \
                 Authorise your accounts screen.",
                platform.label()
            );
        }
        bail!(
            "no saved login for the chat account `{key}`. Add it under Config → Accounts \
             and authorise that account in the browser."
        );
    };

    if !tokens.needs_refresh() {
        return Ok((tokens.access_token, false));
    }

    let Some(refresh_token) = tokens.refresh_token.clone() else {
        bail!(
            "your {} access token has expired and there is no refresh token saved, \
             so it cannot be renewed automatically. Log in again under Config → Accounts.",
            platform.label()
        );
    };

    tracing::info!(
        platform = platform.slug(),
        "refreshing expired access token"
    );

    let credentials = credentials(config, platform);
    let spec = spec_for(platform);

    let refreshed = oauth::refresh(
        &spec,
        &credentials.client_id,
        &credentials.client_secret,
        &refresh_token,
    )
    .await
    .with_context(|| {
        format!(
            "could not renew your {} access token. Log in again under Config → Accounts.",
            platform.label()
        )
    })?;

    let access = refreshed.access_token.clone();
    // The refresh drops the cached identity if we are not careful — carry it
    // over, since who the account is does not change when its token does.
    let mut refreshed = refreshed;
    refreshed.identity = tokens.identity.clone();
    store.set_keyed(key.to_string(), refreshed);
    Ok((access, true))
}

/// A one-line summary of a platform's login state, for `msm status`.
pub fn describe(platform: Platform, tokens: Option<&TokenSet>) -> String {
    match tokens {
        None => format!("{:<8} not logged in — Config → Accounts", platform.label()),
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
    fn describe_tells_you_where_to_log_in_when_logged_out() {
        let text = describe(Platform::Twitch, None);
        assert!(
            text.contains("Config → Accounts"),
            "a logged-out platform should say where to log in: {text}"
        );
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

        assert_eq!(credentials(&config, Platform::Twitch).client_id, "tw");
        assert_eq!(credentials(&config, Platform::YouTube).client_id, "yt");
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
        let lock = StoreLock::acquire().expect("locking the scratch store");
        store.save(&lock).expect("writing the scratch token file");
        drop(lock);

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
        assert!(err.contains("Config → Accounts"), "unhelpful error: {err}");

        // Nothing needed renewing, so the file must not have been rewritten.
        let written = std::fs::read_to_string(scratch.path().join("tokens.json")).unwrap();
        assert!(written.contains("twitch-token"));
        assert!(!written.contains("youtube"));
    }
}
