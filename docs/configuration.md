# Configuration

Everything `msm` remembers lives in one file, `config.toml`. It holds two quite
different kinds of thing:

* **Credentials** — the client id and secret from each developer console. Set up
  once, then forgotten about.
* **A preset** — the title, tags, category, language and so on that the form
  starts from. This half is meant to be hand-edited, and is what makes
  `msm go` useful without the interface at all.

**Contents**

* [Where the file lives](#where-the-file-lives)
* [Shape of the file](#shape-of-the-file)
* [`[twitch]`](#twitch)
* [`[youtube]`](#youtube)
* [`[general]`](#general)
* [`[preset]`](#preset)
* [A full worked example](#a-full-worked-example)
* [The preset workflow](#the-preset-workflow)
* [Several presets at once](#several-presets-at-once)
* [Environment variables](#environment-variables)
* [Other files in the same directory](#other-files-in-the-same-directory)

---

## Where the file lives

```bash
msm paths
```

prints the config, token and log paths. The directory follows each operating
system's own convention:

| System | Directory |
|---|---|
| Linux | `~/.config/multistream-manager` (or `$XDG_CONFIG_HOME/multistream-manager`) |
| macOS | `~/Library/Application Support/multistream-manager` |
| Windows | `%APPDATA%\multistream-manager` |

Set the `MSM_CONFIG_DIR` environment variable to override it entirely — useful
if you keep the whole thing inside a dotfiles repository.

The file is written with owner-only permissions (mode `0600` on Unix) whenever
`msm` writes it, because it contains client secrets. If you create it by hand,
consider setting those permissions yourself.

---

## Shape of the file

Four sections, all optional. Anything you leave out falls back to the default
listed below, so a partial file is valid and a completely empty file parses.

```toml
[twitch]    # Twitch application credentials
[youtube]   # Google OAuth credentials, plus stream-key reuse settings
[general]   # settings that belong to neither platform
[preset]    # your default stream settings
```

A syntax error is reported with the file path and a reminder to look for a
missing quote or bracket, since a hand-edited file is the usual source of one.

---

## `[twitch]`

Credentials from <https://dev.twitch.tv/console/apps>. See
[Getting started, step 3](getting-started.md#3-twitch-credentials) for how to
obtain them.

| Key | Type | Default | What it does |
|---|---|---|---|
| `client_id` | string | `""` | The **Client ID** shown on your application's page. Sent as the `Client-Id` header on every Twitch API call. |
| `client_secret` | string | `""` | The **Client Secret**, generated with the *New Secret* button. Only ever sent to Twitch's own token endpoint, when exchanging or refreshing tokens. |

Both must be set before anything Twitch-related will run. If either is empty,
`msm` refuses the operation and prints the console URL and the exact steps
rather than a bare authentication failure.

---

## `[youtube]`

Credentials from <https://console.cloud.google.com/apis/credentials> (create an
OAuth client of type **Desktop app**), plus the two settings that control which
stream key your broadcasts bind to.

| Key | Type | Default | What it does |
|---|---|---|---|
| `client_id` | string | `""` | The OAuth client id, usually ending in `.apps.googleusercontent.com`. |
| `client_secret` | string | `""` | The OAuth client secret. Only ever sent to Google's token endpoint. |
| `reuse_stream` | boolean | `true` | Bind each new broadcast to a stream key that already exists on the channel instead of creating a new one. **Leave this on.** |
| `stream_id` | string | `""` | Pin one specific stream by its API id. Empty means "use whichever one YouTube lists first". |

### Why `reuse_stream` defaults to `true`

Every call to YouTube's `liveStreams.insert` mints a **brand new stream key**.
If a new stream object were created for each broadcast, you would have to paste
a fresh key into OBS — or into the Aitum multistream plugin — before every
single session, which is exactly the manual work this tool exists to remove.

With `reuse_stream = true`, an existing stream on the channel is found and the
new broadcast is bound to that instead. Your key never changes and your OBS
configuration is never touched. The dashboard tells you which branch was taken,
in words, so you are never left wondering.

Turning it off is supported but means a new key each time, and a note telling
you to go and paste it somewhere.

### When to set `stream_id`

If the channel has exactly one stream key, leave it empty — that key is the one
that gets bound, and there is nothing to disambiguate.

If the channel has several, whichever one YouTube happens to list first is the
one that gets used, and that ordering is not something you control. Pin the one
your OBS setup is actually configured for:

```bash
msm streams
```

```
ID                         PINNED  TITLE
Vy8dQ...oqA                        Default stream key
9tRk2...LmX                        Old backup key
```

Copy the id you want into the config:

```toml
[youtube]
stream_id = "Vy8dQ...oqA"
```

`msm streams` marks the pinned one in the `PINNED` column afterwards, and warns
loudly if the id in your config is not on the channel at all — that setting left
uncorrected makes every go-live fail, and it is far easier to understand here
than in the middle of a submission.

---

## `[general]`

| Key | Type | Default | What it does |
|---|---|---|---|
| `poll_interval_secs` | integer (seconds) | `15` | How often the dashboard refreshes live statistics. Clamped to the range 5–3600 whatever you write. |
| `oauth_port` | integer (TCP port) | `8017` | The local port the login callback listener binds to. |

### `poll_interval_secs`

Every refresh spends a slice of YouTube's daily API quota, and neither platform
updates its viewer count faster than about this rate anyway, so a very low value
costs quota without telling you anything new. If you are running out of quota
during long sessions, raising this is the first thing to try — see
[Troubleshooting](troubleshooting.md#youtube-quotaexceeded).

The value is clamped rather than trusted: anything below 5 becomes 5, anything
above 3600 becomes 3600.

### `oauth_port`

This port has to agree with the redirect URI you registered in **both**
developer consoles. The redirect URI is built as:

```
http://localhost:<oauth_port>/callback
```

so with the default it is `http://localhost:8017/callback`. If you change the
port here you must edit the redirect URI in the Twitch developer console and in
the Google Cloud console to match, character for character, or both logins will
be rejected before they start.

The listener binds both the IPv4 and the IPv6 loopback address on this port,
because `localhost` resolves to `127.0.0.1` on some machines and `::1` on
others. IPv4 is required; IPv6 is used when the host has it.

---

## `[preset]`

The default stream settings. The form starts from these values, <kbd>Ctrl</kbd>+<kbd>S</kbd>
in the form writes them back here, and `msm go` uses them directly.

| Key | Type | Default | What it does |
|---|---|---|---|
| `title` | string | `""` | The stream title, sent to both platforms. Twitch accepts 140 characters, YouTube 100. |
| `description` | string | `""` | YouTube only — Twitch has no description field. Maximum 5000 characters. Use TOML's `"""…"""` for multiple lines. |
| `tags` | array of strings | `[]` | Sent to both platforms, differently. See [how tags are treated](#how-tags-are-treated). |
| `twitch_category` | string | `""` | The Twitch category spelled as Twitch spells it, e.g. `"Software and Game Development"`. |
| `twitch_category_id` | string | `""` | The numeric id for the name above. Filled in automatically; you do not normally type it. |
| `youtube_category_id` | string | `"20"` | YouTube's numeric video category. 20 is Gaming. |
| `language` | string | `"en"` | ISO 639-1 two-letter code: `en`, `pl`, `de`, `fr`, `es`, `uk`, … |
| `privacy` | `"public"` \| `"unlisted"` \| `"private"` | `"public"` | YouTube visibility. Twitch has no equivalent and ignores it. |
| `made_for_kids` | boolean | `false` | YouTube requires this declaration on every broadcast. |
| `youtube_auto_start` | boolean | `true` | Let YouTube go live by itself as soon as it sees the feed from OBS. |
| `youtube_auto_stop` | boolean | `false` | Let YouTube end the broadcast when the feed stops. |
| `platforms` | array of strings | `["twitch", "youtube"]` | Which platforms are ticked when the interface opens, and which `msm go` uses when `--platforms` is not given. |

### Why there are two Twitch category keys

Twitch's channel-update endpoint does not accept a category *name*. It accepts a
numeric `game_id` and nothing else. A human editing a file wants to write the
name, so the file holds both: `twitch_category` is the name you type, and
`twitch_category_id` is the id looked up for it.

When you run `msm go` with a name but no id, the name is searched against
Twitch's category list first — an exact match wins, otherwise the best match
does. Caching the id means a repeat run does not spend an API call re-resolving
a category that has not changed. Change the name and leave the id stale and the
pair is treated as unresolved, so it gets looked up again.

To find the exact spelling and id of a category:

```bash
msm categories chess
```

### How tags are treated

The same list goes to both platforms, adapted to each one's rules.

| | Twitch | YouTube |
|---|---|---|
| Maximum count | 10 (extras are dropped, and you are told how many) | limited by total length |
| Per-tag length | 25 characters | — |
| Total length | — | 500 characters across all tags combined |
| Spaces and punctuation | not allowed; stripped, so `live coding` is sent as `livecoding` | allowed as typed |
| Also used for | — | appended to the YouTube title as `#hashtags`, as many as fit in 100 characters |

An empty `tags` list is **not** sent to Twitch as an empty array. Twitch
documents an empty array as "delete every tag on this channel", so sending one
when you had merely not set any tags would quietly wipe tags you had put there
by other means. The field is omitted instead.

### `youtube_category_id`

YouTube identifies video categories by number. The ids most useful for a live
stream:

| Id | Category |
|---|---|
| `10` | Music |
| `17` | Sports |
| `20` | Gaming |
| `22` | People & Blogs |
| `23` | Comedy |
| `24` | Entertainment |
| `25` | News & Politics |
| `26` | Howto & Style |
| `27` | Education |
| `28` | Science & Technology |

The form searches YouTube's own live list once you are logged in, which is
longer than this and region-dependent. The table above is the built-in fallback
the field uses before the API list is available — on a first run, when nothing
has been authorised yet, or when the quota has run out — so the field is always
usable and never appears dead.

Categories YouTube marks as not assignable are filtered out of the live list,
because they are historical entries that cannot be set on a new video and
offering them would only produce failures.

### `language`

Both platforms take an ISO 639-1 two-letter code — Twitch calls the field
`broadcaster_language`, YouTube calls it `snippet.defaultLanguage`. Nobody
remembers that Ukrainian is `uk` rather than `ua`, so the form lets you search by
the language's English name or by its own name for itself.

A value that is not two letters is refused outright when Twitch is selected, and
merely flagged when it is not. YouTube's language field is omitted from the
update unless the code is two characters, because an invalid one makes YouTube
reject the whole request.

### `youtube_auto_start` and `youtube_auto_stop`

`youtube_auto_start = true` means YouTube switches the broadcast live the moment
it sees OBS's feed. You never open YouTube Studio. With it off, you must press
**Go live** in Studio yourself after OBS connects.

`youtube_auto_stop` defaults to **off** deliberately. With it on, a momentary
OBS crash or network blip ends the broadcast for good rather than letting you
reconnect into the same one.

---

## A full worked example

A complete file, with credentials replaced by placeholders. Everything here is
copy-pasteable; substitute your own values.

```toml
# multistream-manager configuration

[twitch]
# From https://dev.twitch.tv/console/apps
client_id = "abcdefghijklmnopqrstuvwxyz1234"
client_secret = "0123456789abcdefghijklmnopqrst"

[youtube]
# From https://console.cloud.google.com/ — OAuth client of type "Desktop app"
client_id = "000000000000-xxxxxxxxxxxxxxxxxxxxxxxx.apps.googleusercontent.com"
client_secret = "GOCSPX-xxxxxxxxxxxxxxxxxxxx"

# Bind every broadcast to a stream key that already exists, so OBS and Aitum
# never need changing. Leave this on.
reuse_stream = true

# Pin one specific key. Find the id with `msm streams`. Empty is fine when the
# channel has only one key.
stream_id = ""

[general]
# Seconds between statistics refreshes. Every refresh costs YouTube API quota.
poll_interval_secs = 15

# Local port for the login callback. Must match the redirect URI registered in
# both developer consoles: http://localhost:8017/callback
oauth_port = 8017

[preset]
title = "Building a Rust TUI from scratch"

description = """
Working on multistream-manager today: a terminal tool that sets up Twitch and
YouTube from one form.

Source: https://github.com/worxbend/multistream-manager
"""

tags = ["rust", "programming", "livecoding"]

# Spelled exactly as Twitch spells it. `msm categories rust` finds the name.
twitch_category = "Software and Game Development"
# Filled in automatically the first time the name above is used.
twitch_category_id = ""

# 28 = Science & Technology
youtube_category_id = "28"

language = "en"
privacy = "public"
made_for_kids = false

# YouTube goes live by itself when it sees the OBS feed.
youtube_auto_start = true
# Off, so a brief OBS crash does not end the broadcast.
youtube_auto_stop = false

platforms = ["twitch", "youtube"]
```

---

## The preset workflow

The form and the file are two views of the same thing, and they feed each other.

**File → stream.** Edit `[preset]`, then:

```bash
msm go
```

`msm go` connects to each selected platform and prints which account it resolved
to, resolves the Twitch category name if it has no id yet, checks the plan for
problems it can find without calling an API, shows you a summary, and asks
before doing anything. Answer anything other than `y` and nothing is changed.

```
Twitch   connected as yourname
YouTube  connected as Your Channel

About to apply:
  Title:    Building a Rust TUI from scratch
  Category: Software and Game Development (Twitch)
  Language: en
  Tags:     rust, programming, livecoding
  To:       Twitch, YouTube

Go ahead? [y/N]
```

Skip the prompt when there is nobody there to answer it:

```bash
msm go --yes
```

**Stream → file.** Pressing <kbd>Ctrl</kbd>+<kbd>S</kbd> in the form writes
whatever you have typed back into `[preset]`, including the resolved Twitch
category id. So you can build a preset in the interface once and then use
`msm go` from then on.

Note that `msm go` reads the preset and never writes to it.

---

## Several presets at once

The global `--config` flag points at a different file. Because the file also
holds your credentials, keep the credential sections in each one — or copy an
existing file and change only the `[preset]` half.

```bash
msm --config ~/streams/coding.toml go
msm --config ~/streams/gaming.toml go --yes
```

The flag works before any subcommand, and applies to the interface too:

```bash
msm --config ~/streams/coding.toml
```

Saving from the form writes back to whichever file was loaded, not to the
default one — otherwise `--config` would quietly copy your preset, and a copy of
your client secrets, into the wrong place.

Two commands are exceptions worth knowing about: `msm init` always writes to the
default location, and `msm paths` always prints the default location, regardless
of `--config`.

---

## Environment variables

| Variable | Effect |
|---|---|
| `MSM_CONFIG_DIR` | Use this directory for config, tokens and the log instead of the OS default. Created if it does not exist. |
| `MSM_LOG` | Log verbosity, in the usual `tracing` syntax: `MSM_LOG=debug`, or something narrower like `MSM_LOG=multistream_manager::youtube=trace`. Defaults to `info`. |

---

## Other files in the same directory

| File | Contents |
|---|---|
| `config.toml` | This file. Credentials and preset. |
| `tokens.json` | OAuth access and refresh tokens, written with owner-only permissions. A refresh token is as good as a password; `msm logout` deletes the entry. |
| `msm.log` | Diagnostics. The interface owns the terminal, so nothing can be printed to the screen while it is running. |

Stream keys are never written to any of them.

---

* [Commands](commands.md) — the full command reference.
* [OBS and Aitum](obs-and-aitum.md) — why `reuse_stream` matters.
* [Troubleshooting](troubleshooting.md) — when a setting does not behave.
* [Back to the documentation index](README.md).
