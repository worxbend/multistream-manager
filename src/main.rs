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
use model::Platform;

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

    let config = match &cli.config {
        Some(path) => Config::load_from(path)?,
        None => Config::load()?,
    };

    match cli.command {
        None | Some(Commands::Tui) => ui::run(config).await,
        Some(Commands::Login { platform }) => cmd_login(&config, &platform).await,
        Some(Commands::Logout { platform }) => cmd_logout(&platform),
        Some(Commands::Status) => cmd_status(&config),
        Some(Commands::Go { platforms, yes }) => cmd_go(&config, platforms, yes).await,
        Some(Commands::Key { platform }) => cmd_key(&config, &platform).await,
        Some(Commands::Categories { query }) => cmd_categories(&config, &query).await,
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

async fn cmd_go(config: &Config, platforms: Option<String>, skip_prompt: bool) -> Result<()> {
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
            Ok(name) => println!("{:<8} connected as {name}", platform.label()),
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
        println!("{prefix}: {} — {}", issue.field.label(), issue.message);
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
    print!("{}", engine::render_results(&results));

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
