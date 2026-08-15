//! `msm` — configure and go live on Twitch and YouTube from one place.
//!
//! Running `msm` with no arguments opens the terminal interface, which is the
//! way it is meant to be used. The subcommands exist for the things a TUI is bad
//! at: one-off logins, scripted go-lives, and printing a stream key.

mod auth;
mod backend;
mod config;
mod engine;
mod lang;
mod logging;
mod model;
mod paths;
mod twitch;
mod ui;
mod youtube;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};

use config::Config;
use model::{IngestEndpoint, Platform, StaleBroadcast};

#[derive(Parser)]
#[command(
    name = "msm",
    version,
    about = "Configure and go live on Twitch and YouTube from a single terminal interface",
    long_about = "Set your stream title, category, tags and language once, push them to \
                  Twitch and YouTube together, then watch the live statistics — without \
                  opening either website.\n\n\
                  Run `msm` with no arguments to open the interface."
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// Use a specific config file instead of the default one.
    ///
    /// Handy for keeping one preset per kind of stream, for example
    /// `msm --config ~/streams/coding.toml go`.
    #[arg(long, short, global = true, value_name = "FILE")]
    config: Option<std::path::PathBuf>,
}

#[derive(Subcommand)]
enum Commands {
    /// Open the terminal interface. This is the default.
    Tui,

    /// Authorise a platform in your browser.
    Login {
        /// `twitch`, `youtube`, or `all`.
        platform: String,
    },

    /// Forget a platform's saved login.
    Logout {
        /// `twitch`, `youtube`, or `all`.
        platform: String,
    },

    /// Show which platforms are logged in and where the config lives.
    Status,

    /// Apply the preset from your config file without opening the interface.
    ///
    /// This is the scriptable path: edit `[preset]` in config.toml, run
    /// `msm go`, and both platforms are configured.
    Go {
        /// Override which platforms to use, e.g. `--platforms twitch,youtube`.
        #[arg(long, value_name = "LIST")]
        platforms: Option<String>,

        /// Skip the confirmation prompt.
        #[arg(long, short)]
        yes: bool,

        /// Print the result as JSON on stdout instead of the human report.
        ///
        /// One object per platform, with `platform`, `ok`, `watch_url`,
        /// `manage_url`, `ingest_url`, `error` and `notes`. The stream key is
        /// never included — use `msm key` for that. Implies --yes, since there
        /// is nobody at a keyboard to answer a prompt, and every progress
        /// message goes to stderr so stdout holds nothing but the JSON.
        #[arg(long)]
        json: bool,
    },

    /// Print a platform's stream key, for pasting into OBS or Aitum.
    ///
    /// Deliberately a separate command so a key is never printed by accident.
    Key {
        /// `twitch` or `youtube`.
        platform: String,
    },

    /// Search Twitch's category list from the command line.
    Categories {
        /// What to search for, e.g. `msm categories chess`.
        query: String,
    },

    /// List the stream keys on your YouTube channel.
    ///
    /// Use this to find the id to put in `stream_id` under `[youtube]` in your
    /// config, which pins one key so every broadcast binds to the same one and
    /// your OBS or Aitum settings never need changing.
    Streams {
        /// Also print the stream key itself.
        ///
        /// Off by default because a key is enough on its own to broadcast to
        /// your channel, and this output is easy to leave in scrollback.
        #[arg(long)]
        show_keys: bool,
    },

    /// Find YouTube broadcasts that were created but never went live.
    ///
    /// Submitting again creates a *new* YouTube broadcast rather than editing
    /// the previous one, so abandoned attempts accumulate in YouTube Studio.
    /// This lists them; `--yes` deletes them. Anything that has ever received a
    /// feed is never listed and never deleted.
    Cleanup {
        /// Delete the broadcasts listed instead of only showing them.
        #[arg(long, short)]
        yes: bool,
    },

    /// Write a commented starter config file.
    Init,

    /// Show where the config, token and log files live.
    Paths,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // Keep the guard alive for the whole run so buffered log lines get flushed.
    let _log_guard = logging::init().ok();

    // Commands that never read a config value are dispatched before the
    // config file is parsed. Otherwise a corrupted `config.toml` would lock
    // the user out of exactly the commands needed to recover from it:
    // `msm paths` (which tells them where the broken file lives) and
    // `msm logout` / `msm init` (which only touch other files).
    match &cli.command {
        Some(Commands::Logout { platform }) => return cmd_logout(platform),
        Some(Commands::Init) => return cmd_init(),
        Some(Commands::Paths) => return cmd_paths(),
        _ => {}
    }

    let config = match &cli.config {
        Some(path) => Config::load_from(path)?,
        None => Config::load()?,
    };

    match cli.command {
        None | Some(Commands::Tui) => ui::run(config).await,
        Some(Commands::Login { platform }) => cmd_login(&config, &platform).await,
        Some(Commands::Logout { platform }) => cmd_logout(&platform),
        Some(Commands::Status) => cmd_status(&config),
        Some(Commands::Go {
            platforms,
            yes,
            json,
        }) => cmd_go(&config, platforms, yes, json).await,
        Some(Commands::Key { platform }) => cmd_key(&config, &platform).await,
        Some(Commands::Categories { query }) => cmd_categories(&config, &query).await,
        Some(Commands::Streams { show_keys }) => cmd_streams(&config, show_keys).await,
        Some(Commands::Cleanup { yes }) => cmd_cleanup(&config, yes).await,
        Some(Commands::Init) => cmd_init(),
        Some(Commands::Paths) => cmd_paths(),
    }
}

/// Parse `twitch`, `youtube` or `all` into a platform list.
fn parse_platform_arg(value: &str) -> Result<Vec<Platform>> {
    if value.eq_ignore_ascii_case("all") {
        return Ok(Platform::ALL.to_vec());
    }

    value
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.parse::<Platform>().map_err(|e| anyhow::anyhow!(e)))
        .collect()
}

async fn cmd_login(config: &Config, platform_arg: &str) -> Result<()> {
    for platform in parse_platform_arg(platform_arg)? {
        auth::login(config, platform).await?;
    }
    println!("\nDone. Run `msm` to open the interface.");
    Ok(())
}

fn cmd_logout(platform_arg: &str) -> Result<()> {
    let mut store = auth::store::TokenStore::load()?;
    for platform in parse_platform_arg(platform_arg)? {
        if store.remove(platform) {
            println!("Forgot the saved {} login.", platform.label());
        } else {
            println!("There was no saved {} login.", platform.label());
        }
    }
    store.save()
}

fn cmd_status(config: &Config) -> Result<()> {
    let store = auth::store::TokenStore::load()?;

    println!("Logins");
    for platform in Platform::ALL {
        println!("  {}", auth::describe(platform, store.get(platform)));
    }

    println!("\nCredentials configured");
    for platform in Platform::ALL {
        let configured = config.check_credentials(&[platform]).is_ok();
        println!(
            "  {:<8} {}",
            platform.label(),
            if configured {
                "yes"
            } else {
                "no — see `msm init`"
            }
        );
    }

    let authorised = store.authorised_platforms();
    if authorised.is_empty() {
        println!("\nNo platform is authorised yet. Run `msm login all` to fix that.");
    } else {
        println!(
            "\nReady to stream to: {}",
            authorised
                .iter()
                .map(|p| p.label())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    println!("\nConfig file: {}", paths::config_file()?.display());
    Ok(())
}

/// Print a progress line for `msm go`.
///
/// In JSON mode stdout has to hold the JSON document and nothing else, or the
/// first thing a script does with the output is fail to parse it. The messages
/// are still worth having, so they go to stderr instead of being thrown away.
fn progress(json: bool, line: &str) {
    if json {
        eprintln!("{line}");
    } else {
        println!("{line}");
    }
}

async fn cmd_go(
    config: &Config,
    platforms: Option<String>,
    skip_prompt: bool,
    json: bool,
) -> Result<()> {
    // JSON output implies --yes. A machine-readable run is by definition one
    // where nobody is sitting at the keyboard to type "y", and the prompt would
    // land in the middle of the document anyway.
    let skip_prompt = skip_prompt || json;

    let platforms = match platforms {
        Some(list) => parse_platform_arg(&list)?,
        None => {
            if config.preset.platforms.is_empty() {
                bail!(
                    "no platforms are set. Either add `platforms = [\"twitch\", \"youtube\"]` \
                     to the [preset] section of your config, or pass --platforms."
                );
            }
            config.preset.platforms.clone()
        }
    };

    let mut plan = config.preset.to_plan();

    if plan.title.trim().is_empty() {
        bail!(
            "the preset has no title. Set `title` in the [preset] section of {}.",
            paths::config_file()?.display()
        );
    }

    let mut engine = engine::Engine::build(config, &platforms).await?;

    // Report who we are before changing anything, so a wrong account is obvious.
    for (platform, outcome) in engine.connect_all().await {
        match outcome {
            Ok(name) => progress(
                json,
                &format!("{:<8} connected as {name}", platform.label()),
            ),
            Err(err) => bail!("{} could not connect: {err}", platform.label()),
        }
    }

    // A hand-edited config names the Twitch category but has no id for it.
    engine
        .resolve_plan(&mut plan, &config.preset.twitch_category)
        .await?;

    let issues = plan.validate(&platforms);
    for issue in &issues {
        let prefix = if issue.blocking { "error" } else { "note" };
        progress(
            json,
            &format!("{prefix}: {} — {}", issue.field.label(), issue.message),
        );
    }
    if issues.iter().any(|i| i.blocking) {
        bail!("the preset has problems that must be fixed before going live");
    }

    if !skip_prompt {
        println!("\nAbout to apply:");
        println!("  Title:    {}", plan.title);
        if let Some(category) = &plan.twitch_category {
            println!("  Category: {} (Twitch)", category.name);
        }
        println!("  Language: {}", plan.language);
        println!("  Tags:     {}", plan.tags.join(", "));
        println!(
            "  To:       {}",
            platforms
                .iter()
                .map(|p| p.label())
                .collect::<Vec<_>>()
                .join(", ")
        );
        print!("\nGo ahead? [y/N] ");
        use std::io::Write as _;
        std::io::stdout().flush().ok();

        let mut answer = String::new();
        std::io::stdin()
            .read_line(&mut answer)
            .context("reading your answer")?;
        if !answer.trim().eq_ignore_ascii_case("y") {
            println!("Cancelled. Nothing was changed.");
            return Ok(());
        }
    }

    let results = engine.go_live(&plan).await;

    if json {
        println!("{}", engine::render_results_json(&results));
    } else {
        print!("{}", engine::render_results(&results));
    }

    // A non-zero exit status matters for scripting: a wrapper script needs to
    // know that something failed without parsing this output.
    if results.iter().all(|r| !r.succeeded()) {
        bail!("every platform failed");
    }

    Ok(())
}

async fn cmd_key(config: &Config, platform_arg: &str) -> Result<()> {
    let platforms = parse_platform_arg(platform_arg)?;
    let Some(&platform) = platforms.first() else {
        bail!("name a platform, e.g. `msm key twitch`");
    };

    if platform == Platform::YouTube {
        // YouTube's key belongs to a stream object, not the channel, and this
        // command has no broadcast context. Point at the two places it exists.
        println!(
            "YouTube's stream key is shown on the dashboard after `msm go`, or at\n\
             https://studio.youtube.com > Go live > Stream settings."
        );
        return Ok(());
    }

    let mut engine = engine::Engine::build(config, &[platform]).await?;
    for (_, outcome) in engine.connect_all().await {
        outcome.map_err(|err| anyhow::anyhow!(err))?;
    }

    match engine.stream_key(platform).await? {
        Some(key) => println!("{key}"),
        None => bail!(
            "could not read your Twitch stream key. The saved login may predate the \
             `channel:read:stream_key` permission — run `msm login twitch` again."
        ),
    }
    Ok(())
}

async fn cmd_categories(config: &Config, query: &str) -> Result<()> {
    let mut engine = engine::Engine::build(config, &[Platform::Twitch]).await?;
    for (_, outcome) in engine.connect_all().await {
        outcome.map_err(|err| anyhow::anyhow!(err))?;
    }

    let matches = engine.search_categories(Platform::Twitch, query).await?;
    if matches.is_empty() {
        println!("No Twitch category matches {query:?}.");
        return Ok(());
    }

    println!("{:<14} NAME", "ID");
    for category in matches {
        println!("{:<14} {}", category.id, category.name);
    }
    println!(
        "\nPut the name into `twitch_category` in your config; the id is filled in \
         automatically the first time it is used."
    );
    Ok(())
}

/// Connect to YouTube alone, returning an engine ready to use.
///
/// Both `msm streams` and `msm cleanup` are YouTube-only — Twitch has no stream
/// objects and no broadcast objects to list — and both want the same "who am I
/// talking to" line printed before they do anything.
async fn connect_youtube(config: &Config) -> Result<engine::Engine> {
    let mut engine = engine::Engine::build(config, &[Platform::YouTube]).await?;

    for (platform, outcome) in engine.connect_all().await {
        match outcome {
            Ok(name) => println!("{} channel: {name}\n", platform.label()),
            Err(err) => bail!("{} could not connect: {err}", platform.label()),
        }
    }

    Ok(engine)
}

/// Format the stream key listing for `msm streams`.
///
/// Split out from the command so that the rule the `--show-keys` flag exists to
/// enforce — a key is never printed unless it was explicitly asked for — can be
/// asserted in a test without going anywhere near the network.
fn render_stream_list(streams: &[IngestEndpoint], pinned_id: &str, show_keys: bool) -> String {
    if streams.is_empty() {
        return "This channel has no stream keys yet. One is created the first time you run \
                `msm go` with YouTube selected.\n"
            .to_string();
    }

    let mut out = String::new();

    if show_keys {
        out.push_str(&format!(
            "{:<26} {:<7} {:<34} {}\n",
            "ID", "PINNED", "TITLE", "KEY"
        ));
    } else {
        out.push_str(&format!("{:<26} {:<7} {}\n", "ID", "PINNED", "TITLE"));
    }

    for stream in streams {
        let pinned = if !pinned_id.is_empty() && stream.id == pinned_id {
            "yes"
        } else {
            ""
        };

        if show_keys {
            // A stream whose `cdn` block was absent from the reply has no key to
            // show, which is worth saying rather than printing a blank column.
            let key = stream.key.as_deref().unwrap_or("(not reported)");
            out.push_str(&format!(
                "{:<26} {:<7} {:<34} {key}\n",
                stream.id, pinned, stream.title
            ));
        } else {
            out.push_str(&format!(
                "{:<26} {:<7} {}\n",
                stream.id, pinned, stream.title
            ));
        }
    }

    out.push('\n');

    if pinned_id.is_empty() {
        if streams.len() > 1 {
            out.push_str(
                "No stream is pinned, so whichever one YouTube lists first is the one that \
                 gets bound. With more than one key on the channel that ordering is not \
                 yours to control, so copy the id you want into `stream_id` under [youtube] \
                 in your config.\n",
            );
        } else {
            out.push_str(
                "No stream is pinned. With one key on the channel that is fine — it is the \
                 one that will be bound. Set `stream_id` under [youtube] in your config if \
                 you ever add another.\n",
            );
        }
    } else if !streams.iter().any(|stream| stream.id == pinned_id) {
        out.push_str(&format!(
            "Warning: `stream_id` in your config is {pinned_id:?}, which is not on this \
             channel. Going live will fail until you correct that setting or clear it.\n"
        ));
    }

    if !show_keys {
        out.push_str(
            "Stream keys are hidden. Pass --show-keys to print them — anyone holding a key \
             can broadcast to your channel, so think twice on a shared screen.\n",
        );
    }

    out
}

async fn cmd_streams(config: &Config, show_keys: bool) -> Result<()> {
    let mut engine = connect_youtube(config).await?;
    let streams = engine.list_ingest_endpoints(Platform::YouTube).await?;

    print!(
        "{}",
        render_stream_list(&streams, &config.youtube.stream_id, show_keys)
    );
    Ok(())
}

/// Format the list of never-live broadcasts for `msm cleanup`.
///
/// Scheduled times are shown in your own time zone, because that is the one you
/// would have been streaming in and so the one that makes a broadcast
/// recognisable.
fn render_stale_broadcasts(broadcasts: &[StaleBroadcast]) -> String {
    if broadcasts.is_empty() {
        return "Nothing to clean up: every broadcast on this channel has been live at some \
                point.\n"
            .to_string();
    }

    let mut out = if broadcasts.len() == 1 {
        "1 broadcast was created but never went live:\n\n".to_string()
    } else {
        format!(
            "{} broadcasts were created but never went live:\n\n",
            broadcasts.len()
        )
    };

    out.push_str(&format!(
        "{:<26} {:<9} {:<18} {}\n",
        "ID", "STATUS", "SCHEDULED", "TITLE"
    ));

    for broadcast in broadcasts {
        let when = match broadcast.scheduled_start {
            Some(at) => at
                .with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M")
                .to_string(),
            None => "(none given)".to_string(),
        };
        out.push_str(&format!(
            "{:<26} {:<9} {:<18} {}\n",
            broadcast.id, broadcast.status, when, broadcast.title
        ));
    }

    out
}

async fn cmd_cleanup(config: &Config, delete: bool) -> Result<()> {
    let mut engine = connect_youtube(config).await?;
    let stale = engine.list_stale_broadcasts(Platform::YouTube).await?;

    print!("{}", render_stale_broadcasts(&stale));

    if stale.is_empty() {
        return Ok(());
    }

    if !delete {
        println!(
            "\nNothing was deleted. Run `msm cleanup --yes` to delete the broadcasts above. \
             Anything that has ever been live is neither listed here nor deleted."
        );
        return Ok(());
    }

    println!();
    let mut failed = 0;
    for broadcast in &stale {
        match engine
            .delete_broadcast(Platform::YouTube, &broadcast.id)
            .await
        {
            Ok(()) => println!("Deleted {} ({})", broadcast.id, broadcast.title),
            Err(err) => {
                // Keep going: one broadcast that refuses to be deleted should
                // not leave the rest of the clutter behind.
                failed += 1;
                println!(
                    "Could not delete {} ({}): {err:#}",
                    broadcast.id, broadcast.title
                );
            }
        }
    }

    if failed == stale.len() {
        bail!("none of the broadcasts could be deleted");
    }
    if failed > 0 {
        println!("\n{failed} could not be deleted; the rest are gone.");
    }
    Ok(())
}

fn cmd_init() -> Result<()> {
    let path = paths::config_file()?;

    if path.exists() {
        println!(
            "{} already exists — leaving it alone.\n\nEdit it directly, or delete it and \
             run `msm init` again for a fresh one.",
            path.display()
        );
        return Ok(());
    }

    paths::write_secret_file(&path, STARTER_CONFIG)?;

    println!("Wrote a starter config to {}\n", path.display());
    println!("Next steps:");
    println!("  1. Open that file and fill in the client id and secret for each platform.");
    println!("     The comments in the file explain where to get them.");
    println!("  2. Run `msm login all` to authorise.");
    println!("  3. Run `msm` to open the interface.");
    Ok(())
}

fn cmd_paths() -> Result<()> {
    println!("Config: {}", paths::config_file()?.display());
    println!("Tokens: {}", paths::token_file()?.display());
    println!("Log:    {}", paths::log_file()?.display());
    Ok(())
}

/// The file written by `msm init`.
const STARTER_CONFIG: &str = r#"# multistream-manager configuration
#
# Fill in the credentials below, then run `msm login all`.

[twitch]
# From https://dev.twitch.tv/console/apps
#   1. "Register Your Application"
#   2. OAuth Redirect URL must be exactly:  http://localhost:8017/callback
#   3. Category: "Application Integration"
#   4. Copy the Client ID, then press "New Secret" and copy that too.
client_id = ""
client_secret = ""

[youtube]
# From https://console.cloud.google.com/
#   1. Create a project.
#   2. APIs & Services > Library > enable "YouTube Data API v3".
#   3. APIs & Services > Credentials > Create credentials > OAuth client ID.
#      Application type: "Desktop app".
#   4. Add http://localhost:8017/callback as an authorised redirect URI.
#   5. While the app is in "Testing" mode, add your own Google account under
#      "OAuth consent screen" > "Test users" or Google will refuse the login.
client_id = ""
client_secret = ""

# Reuse your existing YouTube stream key instead of creating a new one for every
# broadcast. Leave this as true: creating a new one would change the key, and you
# would have to re-paste it into OBS (or the Aitum multistream plugin) every time.
reuse_stream = true

# Pin one specific reusable stream by id. Leave empty to pick automatically,
# which is right unless you have several stream keys on the channel.
stream_id = ""

[general]
# How often the dashboard refreshes its statistics, in seconds. Every refresh
# spends a little of YouTube's daily API quota, so do not set this very low.
poll_interval_secs = 15

# The local port used to catch the browser redirect during login. If you change
# this you must also change the redirect URI in both developer consoles.
oauth_port = 8017

[preset]
# These are the defaults the form starts from. You can edit them here and run
# `msm go` to skip the interface entirely, or press Ctrl+S in the form to save
# whatever you have typed back into this section.
title = "My stream"
description = """
Multi-line descriptions work like this.

Only YouTube has a description field; Twitch ignores it.
"""
tags = ["rust", "programming"]

# The Twitch category, spelled exactly as Twitch spells it. Run
# `msm categories <search>` to find the right name.
twitch_category = "Software and Game Development"
twitch_category_id = ""

# YouTube's numeric category. 20 = Gaming, 28 = Science & Technology,
# 27 = Education, 24 = Entertainment, 22 = People & Blogs.
youtube_category_id = "20"

# ISO 639-1 two-letter code: en, pl, de, fr, es, uk, ...
language = "en"

# YouTube visibility: "public", "unlisted" or "private".
privacy = "public"

# YouTube requires this declaration on every broadcast.
made_for_kids = false

# Let YouTube go live by itself as soon as it sees the feed from OBS.
youtube_auto_start = true

# Let YouTube end the broadcast when the feed stops. Off is safer — a brief OBS
# crash will not end your stream.
youtube_auto_stop = false

# Which platforms are ticked when the interface opens.
platforms = ["twitch", "youtube"]
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn the_cli_definition_is_valid() {
        // clap can catch conflicting flags and bad argument definitions, but
        // only if something asks it to check.
        Cli::command().debug_assert();
    }

    #[test]
    fn platform_arguments_accept_names_lists_and_all() {
        assert_eq!(
            parse_platform_arg("twitch").unwrap(),
            vec![Platform::Twitch]
        );
        assert_eq!(
            parse_platform_arg("twitch,youtube").unwrap(),
            vec![Platform::Twitch, Platform::YouTube]
        );
        assert_eq!(parse_platform_arg("all").unwrap(), Platform::ALL.to_vec());
        // Whitespace around a comma is a natural thing to type.
        assert_eq!(
            parse_platform_arg("twitch , youtube").unwrap(),
            vec![Platform::Twitch, Platform::YouTube]
        );
    }

    #[test]
    fn an_unknown_platform_name_is_rejected_with_a_helpful_message() {
        let err = parse_platform_arg("kick").unwrap_err().to_string();
        assert!(err.contains("kick"));
        assert!(err.contains("twitch"));
    }

    #[test]
    fn running_with_no_arguments_opens_the_interface() {
        let cli = Cli::parse_from(["msm"]);
        assert!(cli.command.is_none());
    }

    #[test]
    fn the_config_flag_is_accepted_before_a_subcommand() {
        let cli = Cli::parse_from(["msm", "--config", "/tmp/x.toml", "status"]);
        assert_eq!(cli.config.unwrap().to_str().unwrap(), "/tmp/x.toml");
        assert!(matches!(cli.command, Some(Commands::Status)));
    }

    #[test]
    fn the_starter_config_parses_as_valid_toml() {
        // A starter file that does not parse would be a terrible first
        // impression, so this is worth asserting.
        let parsed: Config =
            toml::from_str(STARTER_CONFIG).expect("the starter config must be valid TOML");
        assert!(parsed.youtube.reuse_stream);
        assert_eq!(parsed.general.oauth_port, 8017);
        assert_eq!(parsed.preset.platforms.len(), 2);
    }

    fn endpoint(id: &str, title: &str, key: &str) -> IngestEndpoint {
        IngestEndpoint {
            id: id.to_string(),
            title: title.to_string(),
            key: Some(key.to_string()),
        }
    }

    #[test]
    fn the_stream_listing_shows_the_id_and_title_of_every_key() {
        // The id is the whole reason the command exists: it is what goes into
        // `stream_id`, and there is nowhere else convenient to read it from.
        let text = render_stream_list(
            &[
                endpoint("abc123", "Default stream key", "secret-one"),
                endpoint("def456", "Backup key", "secret-two"),
            ],
            "",
            false,
        );

        assert!(text.contains("abc123"));
        assert!(text.contains("Default stream key"));
        assert!(text.contains("def456"));
        assert!(text.contains("Backup key"));
    }

    #[test]
    fn the_stream_listing_never_prints_a_key_by_default() {
        // A stream key is enough on its own to broadcast to the channel, and
        // this output lands in terminal scrollback where it stays.
        let text = render_stream_list(&[endpoint("abc123", "Default", "secret-one")], "", false);

        assert!(
            !text.contains("secret-one"),
            "a stream key was printed without --show-keys"
        );
        assert!(
            text.contains("--show-keys"),
            "the listing should say how to see a key when one is genuinely needed"
        );
    }

    #[test]
    fn the_stream_listing_prints_keys_only_when_they_were_asked_for() {
        let text = render_stream_list(&[endpoint("abc123", "Default", "secret-one")], "", true);
        assert!(text.contains("secret-one"));
    }

    #[test]
    fn a_stream_with_no_reported_key_says_so_rather_than_showing_a_blank() {
        let mut without_key = endpoint("abc123", "Default", "unused");
        without_key.key = None;

        let text = render_stream_list(&[without_key], "", true);
        assert!(text.contains("(not reported)"));
    }

    #[test]
    fn the_pinned_stream_is_marked_and_the_others_are_not() {
        let text = render_stream_list(
            &[
                endpoint("abc123", "Default", "k1"),
                endpoint("def456", "Backup", "k2"),
            ],
            "def456",
            false,
        );

        let pinned_line = text
            .lines()
            .find(|line| line.contains("def456"))
            .expect("the pinned stream should be listed");
        assert!(pinned_line.contains("yes"));

        let other_line = text.lines().find(|line| line.contains("abc123")).unwrap();
        assert!(!other_line.contains("yes"));
    }

    #[test]
    fn a_pinned_id_that_is_not_on_the_channel_is_flagged() {
        // Left uncorrected this makes every go-live fail, and the error at that
        // point is far less obvious than saying it here.
        let text = render_stream_list(&[endpoint("abc123", "Default", "k1")], "gone999", false);
        assert!(text.contains("Warning"));
        assert!(text.contains("gone999"));
    }

    #[test]
    fn several_unpinned_streams_prompt_you_to_pin_one() {
        // With more than one key, which gets bound is YouTube's choice rather
        // than yours, and that is worth knowing before it surprises you.
        let text = render_stream_list(
            &[
                endpoint("abc123", "Default", "k1"),
                endpoint("def456", "Backup", "k2"),
            ],
            "",
            false,
        );
        assert!(text.contains("stream_id"));
    }

    #[test]
    fn an_empty_stream_listing_explains_where_the_first_key_comes_from() {
        let text = render_stream_list(&[], "", false);
        assert!(text.contains("msm go"));
    }

    fn stale(id: &str, title: &str, status: &str, scheduled: bool) -> StaleBroadcast {
        StaleBroadcast {
            id: id.to_string(),
            title: title.to_string(),
            scheduled_start: scheduled.then(|| {
                "2026-08-08T10:00:00Z"
                    .parse::<chrono::DateTime<chrono::Utc>>()
                    .unwrap()
            }),
            status: status.to_string(),
        }
    }

    #[test]
    fn the_cleanup_listing_shows_the_title_and_status_of_each_broadcast() {
        // You have to be able to recognise what is about to be deleted, so the
        // title and the reason it was picked out both have to be on screen.
        let text = render_stale_broadcasts(&[
            stale("bid1", "Friday coding", "created", true),
            stale("bid2", "Test run", "ready", true),
        ]);

        assert!(text.contains("2 broadcasts"));
        assert!(text.contains("bid1"));
        assert!(text.contains("Friday coding"));
        assert!(text.contains("created"));
        assert!(text.contains("Test run"));
        assert!(text.contains("ready"));
    }

    #[test]
    fn a_single_stale_broadcast_is_described_in_the_singular() {
        let text = render_stale_broadcasts(&[stale("bid1", "Only one", "created", true)]);
        assert!(text.contains("1 broadcast was created"));
    }

    #[test]
    fn a_broadcast_with_no_scheduled_time_says_so() {
        // YouTube normally sets one, but the column has to hold something
        // sensible rather than an empty gap if it does not.
        let text = render_stale_broadcasts(&[stale("bid1", "Untimed", "created", false)]);
        assert!(text.contains("(none given)"));
    }

    #[test]
    fn nothing_to_clean_up_says_so_plainly() {
        let text = render_stale_broadcasts(&[]);
        assert!(text.contains("Nothing to clean up"));
    }

    #[test]
    fn the_streams_command_hides_keys_unless_the_flag_is_given() {
        let plain = Cli::parse_from(["msm", "streams"]);
        assert!(matches!(
            plain.command,
            Some(Commands::Streams { show_keys: false })
        ));

        let revealing = Cli::parse_from(["msm", "streams", "--show-keys"]);
        assert!(matches!(
            revealing.command,
            Some(Commands::Streams { show_keys: true })
        ));
    }

    #[test]
    fn the_cleanup_command_only_deletes_when_yes_is_passed() {
        // Deleting broadcasts cannot be undone, so listing has to be what
        // happens when you type the command without thinking about it.
        let listing = Cli::parse_from(["msm", "cleanup"]);
        assert!(matches!(
            listing.command,
            Some(Commands::Cleanup { yes: false })
        ));

        let deleting = Cli::parse_from(["msm", "cleanup", "--yes"]);
        assert!(matches!(
            deleting.command,
            Some(Commands::Cleanup { yes: true })
        ));
    }

    #[test]
    fn go_accepts_the_json_flag_alongside_the_existing_ones() {
        let cli = Cli::parse_from(["msm", "go", "--json", "--platforms", "twitch"]);
        match cli.command {
            Some(Commands::Go {
                json,
                platforms,
                yes,
            }) => {
                assert!(json);
                assert_eq!(platforms.as_deref(), Some("twitch"));
                // --json implies --yes at the point of use, not at parse time,
                // so the flag itself is still false here.
                assert!(!yes);
            }
            _ => panic!("expected the go subcommand"),
        }
    }

    #[test]
    fn the_starter_config_documents_the_redirect_uri_consistently() {
        // The port in the prose and the port in the setting have to agree, or
        // the instructions send people to register the wrong redirect URI.
        let parsed: Config = toml::from_str(STARTER_CONFIG).unwrap();
        let expected = format!("localhost:{}/callback", parsed.general.oauth_port);
        assert!(
            STARTER_CONFIG.matches(&expected).count() >= 2,
            "the documented redirect URI must match `oauth_port`"
        );
    }
}
