# Running msm

Everything you need to go from a fresh install to a live stream, in the order
you need it. If you would rather be walked through the developer consoles
click by click, read
**[getting-started.md](getting-started.md)** instead — this page is the
condensed version and the reference you come back to.

**Contents**

* [Running it](#running-it)
* [What credentials you need, and why](#what-credentials-you-need-and-why)
* [Creating the Twitch application](#creating-the-twitch-application)
* [Creating the Google application](#creating-the-google-application)
* [Where credentials go](#where-credentials-go)
* [Environment variables](#environment-variables)
* [The config file](#the-config-file)
* [Checking it worked](#checking-it-worked)

---

## Running it

```bash
msm
```

That is the whole interface. There are no subcommands and no flags — if you
type one, it prints a short note saying so and where each tab is, rather than
starting as though the argument had been understood.

Five tabs, switched with <kbd>Alt</kbd> and a number:

| | Tab | What is on it |
|---|---|---|
| <kbd>Alt</kbd>+<kbd>1</kbd> | Stream Info | Title, description, tags, category, going live |
| <kbd>Alt</kbd>+<kbd>2</kbd> | Chat | Twitch and YouTube chat, side by side |
| <kbd>Alt</kbd>+<kbd>3</kbd> | Combined | Both of the above at once, arranged how you like |
| <kbd>Alt</kbd>+<kbd>4</kbd> | OBS | Scenes, microphones, streaming and recording |
| <kbd>Alt</kbd>+<kbd>5</kbd> | Config | Everything else, including where things are kept |

Press <kbd>Space</kbd> and wait: every key available from where you are
appears, grouped by subject. That is the fastest way to learn this program and
it is always there, so nothing below has to be memorised.

On a **first run with nothing configured**, the interface opens on a setup
screen asking for the credentials described next, and then on a login screen.
You do not have to prepare anything before running it for the first time.

---

## What credentials you need, and why

Twitch and Google will not let a program change your stream title, read your
chat or create a broadcast unless that program identifies itself. Both do this
the same way: you register an "application" once, they give you two values, and
`msm` presents those values when it asks you to log in.

| Value | What it is | Secret? |
|---|---|---|
| **Client ID** | Names the application. Sent with every request. | No |
| **Client secret** | Proves the request comes from *your* copy of that application. | **Yes** |

You need a pair for each platform you stream to, and only for the platforms you
actually use — Twitch-only and YouTube-only both work, and the interface never
asks for the other one.

These are **not** your Twitch or Google password, and they are not your stream
key. They identify the program, not you; logging in afterwards is what
identifies you, and that happens in your browser where `msm` never sees your
password.

> [!NOTE]
> Registering an application is free on both platforms and takes about five
> minutes each. You do it once, ever.

---

## Creating the Twitch application

1. Go to **<https://dev.twitch.tv/console/apps>** and sign in.
2. Click **Register Your Application**.
3. **Name**: anything not already taken — `msm on my machine` is fine.
4. **OAuth Redirect URLs**: exactly this, including the port —

   ```
   http://localhost:8017/callback
   ```

   This is where Twitch sends your browser back to after you approve the login,
   and `msm` listens on that port for the moment it takes to catch it. If you
   change `oauth_port` in the config, change it here to match.
5. **Category**: *Application Integration*.
6. **Client Type**: *Confidential*.
7. Click **Create**, then open the application you just made.
8. Copy the **Client ID**. Press **New Secret**, confirm, and copy that too.

> [!WARNING]
> Twitch shows a new secret **once**. If you navigate away without copying it,
> generate another — the old one stops working the moment you do, so do not
> generate one while a stream is running.

---

## Creating the Google application

YouTube's API is part of Google Cloud, so this has more steps. All of them are
free.

1. Go to **<https://console.cloud.google.com/>** and sign in with the account
   that owns the YouTube channel.
2. Create a project (top bar, project selector, **New Project**). Any name.
3. **APIs & Services → Library**, search for **YouTube Data API v3**, and
   press **Enable**.
4. **APIs & Services → OAuth consent screen**:
   * User type **External**, then **Create**.
   * Fill in the app name and your email where required.
   * **Add your own Google account under "Test users".** See the warning below.
5. **APIs & Services → Credentials → Create Credentials → OAuth client ID**:
   * Application type: **Desktop app**.
   * Name: anything.
6. Copy the **Client ID** and **Client secret** it shows you. Unlike Twitch,
   Google will show these again later.

> [!WARNING]
> While the consent screen is in **Testing** mode — which it is until you go
> through Google's verification process, and you have no reason to — only
> accounts on the **Test users** list may log in. Miss this and the login fails
> with "access_denied" that explains nothing. Add the Google account that owns
> your channel, even though it is your own project.

> [!NOTE]
> A brand-new YouTube channel cannot stream for **24 hours** after live
> streaming is first enabled, and cannot stream at all until it is enabled at
> <https://youtube.com/features>. Neither is something `msm` can work around.

---

## Where credentials go

Three ways, and you can mix them. In order of how most people should do it:

**1. The setup screen.** Run `msm` with nothing configured and it asks for all
four values, saves them, and moves on to logging in. Secrets are shown as dots
as you type them, because this window is often part of a broadcast. Nothing
else is needed.

**2. The config file.** Under `[twitch]` and `[youtube]`:

```toml
[twitch]
client_id = "your-twitch-client-id"
client_secret = "your-twitch-client-secret"

[youtube]
client_id = "your-google-client-id.apps.googleusercontent.com"
client_secret = "your-google-client-secret"
```

**3. Environment variables.** Leave the file value empty and set:

```bash
export MSM_TWITCH_CLIENT_ID="…"
export MSM_TWITCH_CLIENT_SECRET="…"
export MSM_YOUTUBE_CLIENT_ID="…"
export MSM_YOUTUBE_CLIENT_SECRET="…"
```

The file wins when both are set. A variable that exists but is empty counts as
unset. A credential from the environment is never written into the file.

A reasonable middle is the **ids in the file** (they are not secret) and the
**secrets in the environment**, which keeps the secrets out of anything you
copy or commit. The full comparison, and how to have a password manager supply
them at start-up, is in
[configuration.md](configuration.md#credentials-in-the-environment).

### Logging in

Credentials identify the program; logging in identifies you. After the setup
screen, the *Authorise your accounts* screen opens your browser for each
platform you tick. Approve the access and come back — the interface notices on
its own.

Later, that lives on **Config → Accounts** (<kbd>Alt</kbd>+<kbd>5</kbd>):
<kbd>Enter</kbd> logs the selected platform in or out, and <kbd>a</kbd> adds an
*additional* chat account, for reading and answering chat as a bot or a second
identity.

---

## Environment variables

None are required.

| Variable | Effect |
|---|---|
| `MSM_TWITCH_CLIENT_ID` | Twitch client id, when the file's is empty |
| `MSM_TWITCH_CLIENT_SECRET` | Twitch client secret, when the file's is empty |
| `MSM_YOUTUBE_CLIENT_ID` | Google client id, when the file's is empty |
| `MSM_YOUTUBE_CLIENT_SECRET` | Google client secret, when the file's is empty |
| `OBS_WEBSOCKET_PASSWORD` | OBS WebSocket password, when the file's is empty |
| `MSM_CONFIG_DIR` | Keep config, logins and the log in this directory instead of the OS default |
| `MSM_LOG` | Log verbosity: `MSM_LOG=debug`, or narrower like `MSM_LOG=multistream_manager::youtube=trace` |

Every credential variable name is itself configurable — see
`client_id_env`, `client_secret_env` and `password_env` in
[configuration.md](configuration.md).

> [!WARNING]
> A variable exported in your shell profile is **not** visible to a copy of
> `msm` started from a desktop launcher or a systemd unit, because those do not
> read your shell profile. That is the usual reason a credential works in one
> terminal and seems to vanish elsewhere.

---

## The config file

You never have to create or edit it — the interface writes it — but it is a
plain TOML file and hand-editing is supported.

Where it lives depends on your system, and **Config → Files**
(<kbd>Alt</kbd>+<kbd>5</kbd>) shows the exact path along with where the logins
and the log are kept. Typically:

| System | Path |
|---|---|
| Linux | `~/.config/msm/config.toml` |
| macOS | `~/Library/Application Support/msm/config.toml` |
| Windows | `%APPDATA%\msm\config.toml` |

Set `MSM_CONFIG_DIR` to put all of it somewhere else — a dotfiles repository, a
scratch directory for trying something, or one directory per kind of stream:

```bash
MSM_CONFIG_DIR=~/streams/coding msm
MSM_CONFIG_DIR=~/streams/gaming msm
```

Each directory is a complete world of its own, which includes its logins: a
second directory means authorising again there. That is a real cost and worth
knowing before you split things up.

The file has nine sections, all optional, and anything left out falls back to a
working default — an empty file parses. Every section is documented field by
field in **[configuration.md](configuration.md)**:

```toml
[twitch]      # Twitch application credentials
[youtube]     # Google credentials, plus stream-key reuse
[general]     # polling interval, OAuth callback port
[chat]        # scrollback, YouTube polling and quota, chat logging
[notifications]  # desktop alerts for raids, subs and a stopped stream
[appearance]  # theme, motion, mouse, in-app pop-ups
[obs]         # connection to OBS Studio
[keys]        # every key binding
[layout]      # how the Combined tab is arranged
[preset]      # your default stream title, tags and category
```

Files written next to it — `tokens.json` holding your logins, `msm.log`,
and the chat logs — are described at the end of
[configuration.md](configuration.md).

> [!NOTE]
> `tokens.json` holds credentials that are as good as a password and is written
> with owner-only permissions. It is the one file in that directory never to
> copy anywhere or commit.

---

## Checking it worked

Open **Config → Diagnostics** (<kbd>Alt</kbd>+<kbd>5</kbd>). It checks, in the
order things matter:

* the config file, and where it is;
* each platform's credentials — including ones from the environment, which is
  how you confirm a variable is actually visible;
* your saved logins, and how long they are good for;
* the clipboard, which is how a stream key reaches OBS;
* OBS, and whether a password was found;
* the theme, and whether your terminal can show its exact colours;
* where the log file is.

`[warn]` and `[FAIL]` mean different things. A fresh install with no logins yet
is unfinished, not broken, and only genuine breakage is reported as a failure.
Every warning says what to do about it.

If something still does not work, **[troubleshooting.md](troubleshooting.md)**
covers each failure with its cause and its fix, and the log named in Config →
Files has the detail:

```bash
MSM_LOG=debug msm                 # in one terminal
tail -f ~/.config/msm/msm.log     # in another
```
