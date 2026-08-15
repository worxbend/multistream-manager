//! The OAuth 2.0 "authorization code" flow, done locally.
//!
//! ## What this actually does, step by step
//!
//! Neither Twitch nor Google will hand a program your account credentials. What
//! they do instead is:
//!
//! 1. We open your web browser at *their* login page, with a note saying which
//!    permissions we want and where to send you back to.
//! 2. You log in and click "Authorise" on their site. We never see your password.
//! 3. Their site redirects your browser to `http://localhost:8017/callback?code=…`.
//! 4. This program is listening on that port, so it receives that request and
//!    reads the one-time `code` out of the URL.
//! 5. We swap that code — server to server, no browser involved — for an
//!    **access token** (short-lived, used on every API call) and a **refresh
//!    token** (long-lived, used to silently get new access tokens later).
//!
//! ## Why the loopback listener and not a "paste this code" prompt
//!
//! Google's device flow does not grant the YouTube scopes we need for anything
//! other than a TV-style device, so a local redirect is the supported route for
//! a desktop application. Twitch works the same way. Listening on `localhost`
//! means the code never has to be copied and pasted, and never leaves the
//! machine.
//!
//! ## Two protections against tampering
//!
//! * **`state`** — a random string we send out and require back. If a page tries
//!   to trick your browser into hitting our callback with someone else's code,
//!   the state will not match and we reject it.
//! ## Why the listener binds two addresses
//!
//! The redirect URI names the host `localhost`, but that name resolves to
//! `127.0.0.1` on some machines and to `::1` on others — and on most modern
//! Linux systems it resolves to `::1` first. Binding only IPv4 means the
//! browser's first connection attempt is refused and the login limps along on a
//! retry, or fails outright. So [`Loopback`] binds both families and accepts
//! from whichever the browser actually reaches.
//!
//! * **PKCE** — we invent a random `code_verifier`, send only its SHA-256 hash
//!   up front, then reveal the original when redeeming the code. Anyone who
//!   intercepts the code cannot use it without the verifier. Google effectively
//!   requires this; Twitch ignores the extra parameters, which is harmless.

use anyhow::{anyhow, bail, Context, Result};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use rand::RngCore as _;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use super::store::TokenSet;

/// Everything that differs between one OAuth provider and another.
#[derive(Debug, Clone)]
pub struct ProviderSpec {
    /// Display name, used in log lines and error messages.
    pub name: &'static str,
    /// Where the browser gets sent to log in.
    pub auth_url: &'static str,
    /// Where codes and refresh tokens get exchanged for access tokens.
    pub token_url: &'static str,
    /// The permissions being requested, space-separated at the wire level.
    pub scopes: Vec<&'static str>,
    /// Extra query parameters the provider needs on the authorise URL.
    ///
    /// Google needs `access_type=offline` (otherwise it never issues a refresh
    /// token) and `prompt=consent` (otherwise a *second* authorisation for an
    /// account that already approved returns no refresh token at all, which is
    /// a genuinely nasty trap).
    pub extra_auth_params: Vec<(&'static str, &'static str)>,
    /// Whether to send PKCE parameters.
    pub use_pkce: bool,
}

impl ProviderSpec {
    /// Twitch. The scopes cover: editing the channel's title/category/tags,
    /// reading the stream key, reading follower and subscriber counts.
    pub fn twitch() -> Self {
        Self {
            name: "Twitch",
            auth_url: "https://id.twitch.tv/oauth2/authorize",
            token_url: "https://id.twitch.tv/oauth2/token",
            scopes: vec![
                // Required to change title, category, language and tags.
                "channel:manage:broadcast",
                // Lets us show you the RTMP stream key in the dashboard.
                "channel:read:stream_key",
                // Follower total on the stats panel.
                "moderator:read:followers",
                // Subscriber total on the stats panel.
                "channel:read:subscriptions",
            ],
            extra_auth_params: vec![],
            use_pkce: true,
        }
    }

    /// YouTube. A single broad scope, because creating a broadcast, binding a
    /// stream and updating the resulting video all need write access.
    pub fn youtube() -> Self {
        Self {
            name: "YouTube",
            auth_url: "https://accounts.google.com/o/oauth2/v2/auth",
            token_url: "https://oauth2.googleapis.com/token",
            scopes: vec![
                // Full read/write on the account's YouTube data. The live
                // streaming endpoints are not available under a narrower scope.
                "https://www.googleapis.com/auth/youtube",
            ],
            extra_auth_params: vec![
                // Without this Google issues an access token but no refresh
                // token, and you would have to log in again every hour.
                ("access_type", "offline"),
                // Forces the consent screen even for an already-approved
                // account. Google only returns a refresh token on a *fresh*
                // consent, so skipping this makes re-authorising useless.
                ("prompt", "consent"),
            ],
            use_pkce: true,
        }
    }
}

/// A callback listener bound to every loopback address available.
///
/// See the module documentation for why one address is not enough.
struct Loopback {
    v4: TcpListener,
    /// `None` when the host has no IPv6 loopback, which is unusual but legal.
    v6: Option<TcpListener>,
}

impl Loopback {
    /// Bind both loopback families on `port`.
    ///
    /// IPv4 is required — if that fails the port is genuinely unusable. IPv6 is
    /// best-effort, since a host without it is perfectly functional.
    async fn bind(port: u16) -> std::io::Result<Self> {
        let v4 = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port)).await?;
        let v6 = TcpListener::bind((std::net::Ipv6Addr::LOCALHOST, port))
            .await
            .map_err(|err| {
                tracing::debug!(?err, "no IPv6 loopback available; IPv4 only");
                err
            })
            .ok();
        Ok(Self { v4, v6 })
    }

    /// Accept the next connection on whichever family it arrives over.
    async fn accept(&self) -> std::io::Result<tokio::net::TcpStream> {
        match &self.v6 {
            None => self.v4.accept().await.map(|(socket, _)| socket),
            Some(v6) => tokio::select! {
                result = self.v4.accept() => result.map(|(socket, _)| socket),
                result = v6.accept() => result.map(|(socket, _)| socket),
            },
        }
    }
}

/// A PKCE verifier/challenge pair.
struct Pkce {
    /// The secret, revealed only when redeeming the code.
    verifier: String,
    /// The SHA-256 hash of the verifier, sent up front.
    challenge: String,
}

impl Pkce {
    fn generate() -> Self {
        // The spec allows 43–128 characters. 64 random bytes base64url-encoded
        // gives 86, comfortably inside the range.
        let mut bytes = [0u8; 64];
        rand::thread_rng().fill_bytes(&mut bytes);
        let verifier = URL_SAFE_NO_PAD.encode(bytes);
        let digest = Sha256::digest(verifier.as_bytes());
        let challenge = URL_SAFE_NO_PAD.encode(digest);
        Self {
            verifier,
            challenge,
        }
    }
}

/// Generate the random `state` value used to tie the redirect back to us.
fn random_state() -> String {
    let mut bytes = [0u8; 24];
    rand::thread_rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

/// What the provider's token endpoint sends back.
#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    /// Absent when refreshing against Google, which reuses the existing one.
    #[serde(default)]
    refresh_token: Option<String>,
    /// Seconds until `access_token` stops working.
    #[serde(default)]
    expires_in: Option<i64>,
    /// Twitch returns a JSON array here; Google returns a space-separated
    /// string. Neither is load-bearing for us, so it is captured loosely and
    /// only used for diagnostics.
    #[serde(default)]
    scope: Option<serde_json::Value>,
}

/// Run the whole interactive login and return the resulting tokens.
///
/// This blocks until you finish (or abandon) the login in your browser, with a
/// five minute ceiling so a forgotten tab cannot wedge the program forever.
pub async fn interactive_login(
    spec: &ProviderSpec,
    client_id: &str,
    client_secret: &str,
    redirect_uri: &str,
    port: u16,
) -> Result<TokenSet> {
    let state = random_state();
    let pkce = spec.use_pkce.then(Pkce::generate);

    // Bind the listener *before* opening the browser. Doing it the other way
    // round is a race: a fast browser can hit the callback before we are ready.
    let listener = Loopback::bind(port).await.with_context(|| {
        format!(
            "could not listen on 127.0.0.1:{port} for the login redirect.\n\
                 Something else is probably using that port. Change `oauth_port`\n\
                 in your config (and update the redirect URI in the {} developer\n\
                 console to match) or stop the other program.",
            spec.name
        )
    })?;

    let auth_url = build_auth_url(spec, client_id, redirect_uri, &state, pkce.as_ref());

    println!("Opening your browser to authorise {}…", spec.name);
    println!();
    println!("If nothing opens, paste this URL in yourself:");
    println!("{auth_url}");
    println!();

    // A failure to launch a browser is not fatal — the URL was just printed.
    if let Err(err) = open::that_detached(&auth_url) {
        tracing::warn!(?err, "could not launch a browser automatically");
    }

    let code = tokio::time::timeout(
        Duration::from_secs(300),
        wait_for_callback(&listener, &state),
    )
    .await
    .map_err(|_| {
        anyhow!(
            "timed out after 5 minutes waiting for the {} login to finish",
            spec.name
        )
    })??;

    let tokens = exchange_code(
        spec,
        client_id,
        client_secret,
        redirect_uri,
        &code,
        pkce.as_ref().map(|p| p.verifier.as_str()),
    )
    .await?;

    println!("{} authorised successfully.", spec.name);
    Ok(tokens)
}

/// Assemble the provider's authorise URL with every parameter percent-encoded.
fn build_auth_url(
    spec: &ProviderSpec,
    client_id: &str,
    redirect_uri: &str,
    state: &str,
    pkce: Option<&Pkce>,
) -> String {
    let scope = spec.scopes.join(" ");
    let mut url = format!(
        "{}?client_id={}&redirect_uri={}&response_type=code&scope={}&state={}",
        spec.auth_url,
        urlencoding::encode(client_id),
        urlencoding::encode(redirect_uri),
        urlencoding::encode(&scope),
        urlencoding::encode(state),
    );

    for (key, value) in &spec.extra_auth_params {
        url.push_str(&format!(
            "&{}={}",
            urlencoding::encode(key),
            urlencoding::encode(value)
        ));
    }

    if let Some(pkce) = pkce {
        url.push_str(&format!(
            "&code_challenge={}&code_challenge_method=S256",
            urlencoding::encode(&pkce.challenge)
        ));
    }

    url
}

/// Accept connections until one of them is the redirect we are waiting for.
///
/// Browsers open speculative connections and request `/favicon.ico`, so this
/// cannot simply take the first connection and assume it is the callback.
async fn wait_for_callback(listener: &Loopback, expected_state: &str) -> Result<String> {
    loop {
        let mut socket = listener
            .accept()
            .await
            .context("accepting the OAuth redirect connection")?;

        let Some(request_line) = read_request_line(&mut socket).await else {
            continue;
        };

        // The line looks like: GET /callback?code=abc&state=xyz HTTP/1.1
        let Some(path) = request_line.split_whitespace().nth(1) else {
            continue;
        };

        if !path.starts_with("/callback") {
            // Almost certainly /favicon.ico. Answer politely and keep waiting.
            let _ = socket
                .write_all(
                    b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .await;
            continue;
        }

        let params = parse_query(path);

        // The provider reports a refusal by redirecting with ?error=… instead
        // of ?code=…. Surfacing their description is far more useful than a
        // generic "login failed".
        if let Some(error) = params.get("error") {
            let description = params
                .get("error_description")
                .map(|s| s.as_str())
                .unwrap_or("no further detail given");
            respond(
                &mut socket,
                "Authorisation refused",
                &format!("{error}: {description}"),
                false,
            )
            .await;
            bail!("the provider refused authorisation: {error} ({description})");
        }

        // Reject anything whose state does not match what we sent out.
        match params.get("state") {
            Some(state) if state == expected_state => {}
            _ => {
                respond(
                    &mut socket,
                    "Rejected",
                    "The security token in that request did not match. Nothing has been changed. Please start the login again.",
                    false,
                )
                .await;
                bail!(
                    "the redirect carried the wrong `state` value, so it was rejected. \
                     This usually means a stale browser tab from an earlier login attempt \
                     completed late — try logging in again."
                );
            }
        }

        let Some(code) = params.get("code").cloned() else {
            respond(
                &mut socket,
                "Something went wrong",
                "The provider redirected back without an authorisation code.",
                false,
            )
            .await;
            bail!("the redirect contained no authorisation code");
        };

        respond(
            &mut socket,
            "You're all set",
            "Authorisation complete. You can close this tab and go back to the terminal.",
            true,
        )
        .await;

        return Ok(code);
    }
}

/// How long to wait for one connection to send its request line before giving up
/// on it and going back to waiting for the real redirect.
const REQUEST_LINE_TIMEOUT: Duration = Duration::from_secs(10);

/// The longest request line accepted, in bytes. A redirect URL is a few hundred
/// bytes; anything past this is not a browser sending us a callback, and the cap
/// stops a connection that never sends a newline from growing the buffer without
/// limit.
const MAX_REQUEST_LINE: usize = 8192;

/// Read the first line of an HTTP request, however many packets it arrives in.
///
/// TCP is a stream of bytes, not a stream of messages: one `read` returns
/// whatever has arrived so far, which may be only part of the line. Assuming a
/// single read holds the whole request line meant that a redirect split across
/// two packets — `GET /callb` in one, the rest a moment later — was seen as a
/// request for the path `/callb`, answered with 404 and dropped. The user had
/// authorised correctly but saw a 404 page while the login sat there until it
/// timed out. So this keeps reading until the terminating newline arrives.
///
/// Returns `None` if the connection closes, stalls, or sends an implausibly long
/// line; the caller then goes back to waiting for another connection.
async fn read_request_line<R: tokio::io::AsyncRead + Unpin>(reader: &mut R) -> Option<String> {
    let mut collected: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 1024];

    loop {
        let read = match tokio::time::timeout(REQUEST_LINE_TIMEOUT, reader.read(&mut chunk)).await {
            // Timed out: the peer opened a connection and said nothing useful.
            Err(_) => return None,
            // Clean close before a complete line arrived.
            Ok(Ok(0)) => return None,
            Ok(Ok(n)) => n,
            Ok(Err(err)) => {
                tracing::warn!(?err, "failed reading a connection on the callback port");
                return None;
            }
        };
        collected.extend_from_slice(&chunk[..read]);

        if let Some(end) = collected.iter().position(|&b| b == b'\n') {
            let line = String::from_utf8_lossy(&collected[..end]);
            return Some(line.trim_end_matches('\r').to_string());
        }

        if collected.len() > MAX_REQUEST_LINE {
            tracing::warn!("a connection on the callback port sent no request line");
            return None;
        }
    }
}

/// Pull the query string out of a request path and decode it into a map.
fn parse_query(path: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let Some((_, query)) = path.split_once('?') else {
        return map;
    };
    for pair in query.split('&') {
        let Some((key, value)) = pair.split_once('=') else {
            continue;
        };
        // A redirect query string is form-urlencoded, where `+` means a space.
        // Twitch plus-encodes its error descriptions, so decoding percent
        // escapes alone would surface "The+user+denied+you+access".
        let key = decode_form_component(key);
        let value = decode_form_component(value);
        map.insert(key, value);
    }
    map
}

/// Percent-decode one form-urlencoded component, treating `+` as a space.
fn decode_form_component(raw: &str) -> String {
    urlencoding::decode(&raw.replace('+', "%20"))
        .unwrap_or_default()
        .into_owned()
}

/// Escape the five characters that would otherwise be read as HTML markup.
///
/// The text shown on the callback page can come from the provider's redirect —
/// `error` and `error_description` are simply query parameters, so their
/// contents are whatever the browser was pointed at. Interpolating them raw
/// would mean that a URL containing `<script>…</script>` executes that script in
/// the `http://localhost:8017` origin, i.e. inside the loopback listener this
/// program runs during a login. Escaping turns the markup into visible text.
fn escape_html(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

/// Write a small styled HTML page back to the browser so the user gets a clear
/// visual confirmation rather than a blank page or a connection error.
async fn respond(socket: &mut tokio::net::TcpStream, heading: &str, message: &str, success: bool) {
    let body = callback_page(heading, message, success);

    let status = if success { "200 OK" } else { "400 Bad Request" };
    let response = format!(
        "HTTP/1.1 {status}\r\n\
         Content-Type: text/html; charset=utf-8\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\r\n{body}",
        body.len()
    );

    let _ = socket.write_all(response.as_bytes()).await;
    let _ = socket.flush().await;
}

/// Build the HTML of that page. Separate from sending it so the escaping can be
/// tested without opening a socket.
fn callback_page(heading: &str, message: &str, success: bool) -> String {
    let accent = if success { "#3fb950" } else { "#f85149" };
    let heading = escape_html(heading);
    let message = escape_html(message);
    format!(
        r#"<!doctype html>
<html lang="en"><head><meta charset="utf-8"><title>{heading}</title></head>
<body style="margin:0;display:grid;place-items:center;min-height:100vh;background:#0d1117;color:#e6edf3;font:16px/1.6 system-ui,sans-serif">
  <main style="max-width:32rem;padding:2rem;text-align:center">
    <div style="width:3rem;height:3rem;margin:0 auto 1.5rem;border-radius:50%;background:{accent}"></div>
    <h1 style="margin:0 0 .75rem;font-size:1.5rem">{heading}</h1>
    <p style="margin:0;color:#8b949e">{message}</p>
    <p style="margin-top:2rem;color:#484f58;font-size:.85rem">multistream-manager</p>
  </main>
</body></html>"#
    )
}

/// Trade the one-time authorisation code for real tokens.
async fn exchange_code(
    spec: &ProviderSpec,
    client_id: &str,
    client_secret: &str,
    redirect_uri: &str,
    code: &str,
    verifier: Option<&str>,
) -> Result<TokenSet> {
    let mut form = vec![
        ("client_id", client_id.to_string()),
        ("client_secret", client_secret.to_string()),
        ("code", code.to_string()),
        ("grant_type", "authorization_code".to_string()),
        ("redirect_uri", redirect_uri.to_string()),
    ];
    if let Some(verifier) = verifier {
        form.push(("code_verifier", verifier.to_string()));
    }

    let client = reqwest::Client::new();
    let response = client
        .post(spec.token_url)
        .form(&form)
        .send()
        .await
        .with_context(|| format!("contacting the {} token endpoint", spec.name))?;

    parse_token_response(spec, response, None).await
}

/// Use a refresh token to get a new access token, without any user interaction.
///
/// This is what makes the application usable day to day: you authorise once, and
/// every later run quietly renews itself.
pub async fn refresh(
    spec: &ProviderSpec,
    client_id: &str,
    client_secret: &str,
    refresh_token: &str,
) -> Result<TokenSet> {
    let form = vec![
        ("client_id", client_id.to_string()),
        ("client_secret", client_secret.to_string()),
        ("grant_type", "refresh_token".to_string()),
        ("refresh_token", refresh_token.to_string()),
    ];

    let client = reqwest::Client::new();
    let response = client
        .post(spec.token_url)
        .form(&form)
        .send()
        .await
        .with_context(|| format!("contacting the {} token endpoint to refresh", spec.name))?;

    // Google omits `refresh_token` from a refresh response because the old one
    // stays valid, so carry it forward rather than losing it.
    parse_token_response(spec, response, Some(refresh_token)).await
}

/// Turn an HTTP response from a token endpoint into a [`TokenSet`], or into an
/// error that says what actually went wrong.
async fn parse_token_response(
    spec: &ProviderSpec,
    response: reqwest::Response,
    fallback_refresh: Option<&str>,
) -> Result<TokenSet> {
    let status = response.status();
    let text = response
        .text()
        .await
        .context("reading the token endpoint response body")?;

    if !status.is_success() {
        // Both providers return a JSON body explaining the failure. Show it,
        // because "invalid_grant" plus its description is the difference between
        // a five second fix and an hour of guessing.
        bail!(
            "{} rejected the token request (HTTP {status}): {}",
            spec.name,
            summarise_oauth_error(&text)
        );
    }

    let parsed: TokenResponse = serde_json::from_str(&text)
        .with_context(|| format!("parsing the {} token response: {text}", spec.name))?;

    let refresh_token = parsed
        .refresh_token
        .or_else(|| fallback_refresh.map(str::to_string));

    if refresh_token.is_none() {
        tracing::warn!(
            provider = spec.name,
            "no refresh token was issued; you will have to log in again when the access token expires"
        );
    }

    let scopes = match parsed.scope {
        Some(serde_json::Value::Array(items)) => items
            .into_iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect(),
        Some(serde_json::Value::String(s)) => {
            s.split_whitespace().map(str::to_string).collect::<Vec<_>>()
        }
        _ => Vec::new(),
    };

    Ok(TokenSet::new(
        parsed.access_token,
        refresh_token,
        parsed.expires_in,
        scopes,
    ))
}

/// Extract a readable message from an OAuth error body.
fn summarise_oauth_error(body: &str) -> String {
    let Ok(json) = serde_json::from_str::<serde_json::Value>(body) else {
        return body.chars().take(300).collect();
    };

    let code = json
        .get("error")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown_error");
    let description = json
        .get("error_description")
        .or_else(|| json.get("message"))
        .and_then(|v| v.as_str())
        .unwrap_or("");

    // Translate the two failures people actually hit into plain English.
    let hint = match code {
        "invalid_grant" => {
            "\n  This usually means the saved refresh token has been revoked or has expired. \
             Run `msm login <platform>` to authorise again."
        }
        "invalid_client" => {
            "\n  The client id or client secret in your config does not match what the \
             provider has on file. Check both for stray whitespace."
        }
        "redirect_mismatch" | "invalid_request" => {
            "\n  The redirect URI must match the one registered in the developer console \
             exactly, including the scheme, the port and the /callback path."
        }
        _ => "",
    };

    format!("{code} {description}{hint}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_challenge_is_the_base64url_sha256_of_the_verifier() {
        let pkce = Pkce::generate();
        let expected = URL_SAFE_NO_PAD.encode(Sha256::digest(pkce.verifier.as_bytes()));
        assert_eq!(pkce.challenge, expected);
        // The spec requires the verifier to be between 43 and 128 characters.
        assert!((43..=128).contains(&pkce.verifier.len()));
    }

    #[test]
    fn two_generated_states_differ() {
        assert_ne!(random_state(), random_state());
    }

    #[test]
    fn query_parsing_decodes_percent_escapes() {
        let params = parse_query("/callback?code=a%20b&state=x%2By");
        assert_eq!(params.get("code").unwrap(), "a b");
        assert_eq!(params.get("state").unwrap(), "x+y");
    }

    #[test]
    fn query_parsing_treats_plus_as_a_space() {
        // Twitch sends `error_description=The+user+denied+you+access`; decoding
        // percent escapes alone would leave the plus signs in the message.
        let params = parse_query(
            "/callback?error=access_denied&error_description=The+user+denied+you+access",
        );
        assert_eq!(
            params.get("error_description").unwrap(),
            "The user denied you access"
        );
    }

    #[test]
    fn query_parsing_survives_a_path_with_no_query_string() {
        assert!(parse_query("/callback").is_empty());
    }

    #[test]
    fn auth_url_includes_pkce_and_provider_specific_parameters() {
        let spec = ProviderSpec::youtube();
        let pkce = Pkce::generate();
        let url = build_auth_url(
            &spec,
            "my-client",
            "http://localhost:8017/callback",
            "the-state",
            Some(&pkce),
        );

        assert!(url.contains("client_id=my-client"));
        assert!(url.contains("code_challenge_method=S256"));
        // Without these two Google never issues a refresh token.
        assert!(url.contains("access_type=offline"));
        assert!(url.contains("prompt=consent"));
        // The redirect URI has to be escaped or the query string breaks apart.
        assert!(url.contains("redirect_uri=http%3A%2F%2Flocalhost%3A8017%2Fcallback"));
    }

    #[test]
    fn twitch_auth_url_requests_the_broadcast_management_scope() {
        let spec = ProviderSpec::twitch();
        let url = build_auth_url(&spec, "id", "http://localhost:1/callback", "s", None);
        assert!(url.contains("channel%3Amanage%3Abroadcast"));
        assert!(!url.contains("code_challenge"));
    }

    #[test]
    fn oauth_errors_are_translated_into_advice() {
        let body =
            r#"{"error":"invalid_grant","error_description":"Token has been expired or revoked."}"#;
        let summary = summarise_oauth_error(body);
        assert!(summary.contains("invalid_grant"));
        assert!(summary.contains("msm login"));
    }

    #[test]
    fn a_non_json_error_body_is_still_reported() {
        let summary = summarise_oauth_error("<html>502 Bad Gateway</html>");
        assert!(summary.contains("502"));
    }

    /// The provider's `error_description` is just a query parameter, so it is
    /// attacker-controlled text. Before this was escaped, pointing the browser
    /// at the callback with a `<script>` tag in that parameter ran the script in
    /// the http://localhost:8017 origin — the listener used for logging in.
    #[test]
    fn provider_supplied_text_cannot_inject_markup_into_the_callback_page() {
        let hostile = "<script>alert(document.domain)</script>";
        let page = callback_page("Authorisation refused", &format!("access_denied: {hostile}"), false);

        assert!(!page.contains("<script>"), "raw script tag in:\n{page}");
        assert!(page.contains("&lt;script&gt;alert(document.domain)&lt;/script&gt;"));
        // The wording around it is still readable.
        assert!(page.contains("access_denied:"));
    }

    #[test]
    fn ordinary_text_is_left_alone_apart_from_markup_characters() {
        assert_eq!(escape_html("The user denied you access"), "The user denied you access");
        assert_eq!(escape_html("a & b"), "a &amp; b");
        assert_eq!(escape_html("\"quoted\""), "&quot;quoted&quot;");
    }

    /// A request line arriving in two pieces used to be misread as the path of
    /// the first piece alone, so a genuine redirect was answered with 404 and
    /// the login hung until it timed out.
    #[tokio::test]
    async fn a_request_line_split_across_packets_is_read_whole() {
        let (mut client, mut server) = tokio::io::duplex(64);

        let reader = tokio::spawn(async move { read_request_line(&mut server).await });

        client.write_all(b"GET /callb").await.unwrap();
        tokio::task::yield_now().await;
        client
            .write_all(b"ack?code=THECODE&state=st HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .await
            .unwrap();

        let line = reader.await.unwrap().expect("the line should be read");
        assert_eq!(line, "GET /callback?code=THECODE&state=st HTTP/1.1");
        let path = line.split_whitespace().nth(1).unwrap();
        assert_eq!(parse_query(path).get("code").map(String::as_str), Some("THECODE"));
    }

    #[tokio::test]
    async fn a_connection_that_closes_without_a_request_line_is_skipped() {
        let (client, mut server) = tokio::io::duplex(64);
        drop(client);
        assert!(read_request_line(&mut server).await.is_none());
    }
}
