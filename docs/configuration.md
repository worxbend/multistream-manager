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
* [`[chat]`](#chat)
* [`[appearance]`](#appearance)
* [`[obs]`](#obs)
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

Seven sections, all optional. Anything you leave out falls back to the default
listed below, so a partial file is valid and a completely empty file parses.

```toml
[twitch]      # Twitch application credentials
[youtube]     # Google OAuth credentials, plus stream-key reuse settings
[general]     # settings that belong to neither platform
[chat]        # the chat panes: scrollback, polling, quota, logging
[appearance]  # colours, motion, mouse, notifications
[obs]         # controlling OBS Studio
[preset]      # your default stream settings
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

## `[chat]`

Everything about the chat panes. Every value has a working default, so this
section only needs to exist if you want to change one.

```toml
[chat]
scrollback_limit = 1000          # messages kept per chat
poll_interval_floor_ms = 1000    # fastest the YouTube poller may ever poll
poll_interval_ceiling_ms = 0     # 0 = no ceiling
daily_quota_units = 10000        # your project's daily YouTube API quota
quota_reserve_percent = 10       # stop polling below this much quota left
notifications = true             # desktop notifications for off-screen events
chat_logging = false             # write every message to JSON Lines files
chat_log_dir = ""                # empty = chatlog/ under the config directory
chat_log_max_bytes = 10485760    # rotate a log file at this size
chat_log_max_files = 5           # keep this many rotated files
```

### `scrollback_limit`

How many messages each chat keeps in memory. Older ones are discarded, which
is what stops a chat left open for an eight-hour stream growing without bound.
Raising it costs memory in proportion; there is no other penalty.

### The polling and quota settings

These four only affect YouTube. Twitch chat arrives over a connection that
pushes messages, so it costs nothing to keep open; YouTube's live chat has to
be *polled*, and every poll spends part of a daily quota that Google grants per
project.

`poll_interval_floor_ms` is the fastest this program will ever poll. YouTube
sends its own preferred interval with each response and that still wins
whenever it is higher — the server's floor is absolute and this cannot go under
it. `poll_interval_ceiling_ms` caps the interval instead; `0` means no cap.
Raising both stretches the quota over a longer session at the cost of chat
arriving less promptly.

`quota_reserve_percent` keeps a slice of the quota back. When the estimate
falls below it, polling stops but *sending* still works — running out entirely
would leave you unable to answer your own chat, which is the worse of the two
failures.

> [!NOTE]
> `daily_quota_units` describes what Google has given your project; it does not
> request anything. If you have applied for a higher quota, set it here so the
> reserve is calculated against the real figure.

### The chat log settings

With `chat_logging = true`, every message is appended to JSON Lines files —
one object per line, which is a format both a script and a person can read.
`msm export superchats` reads them back into a CSV of every paid event, with no
network access and no API quota spent, which is what makes them worth keeping.
Files rotate at `chat_log_max_bytes` and the oldest are pruned beyond
`chat_log_max_files`.

---

## `[appearance]`

Colours, motion, and the optional parts of the interface. Nothing in this
section can stop a stream going live, and every value falls back to a sensible
default rather than refusing to start — being locked out of your own stream by
a mistyped colour would be an absurd trade.

```toml
[appearance]
theme = "claude"              # one of the 57 built-in names, or "custom"
animations = "fast"           # "fast", "reduced" or "off"
splash = true                 # the animated start-up screen
mouse = true                  # react to clicks and the wheel
telemetry = false             # cpu / memory / frame rate in the tab bar
toasts = true                 # pop-up notifications
toast_seconds = 5             # how long one stays up
terminal_background = false   # repaint the terminal window's own background
```

### `theme`

`msm profile list` prints every name. A name that does not exist logs a warning
and falls back to the default rather than failing to start, so a typo costs a
line in the log and not your stream.

The interface half of this is <kbd>Ctrl</kbd>+<kbd>T</kbd>, which previews each
palette live — the whole screen redraws as you move the selection, because
colours cannot be judged from a row of swatches.

### `[appearance.custom_theme]`

With `theme = "custom"`, these nine colours are used. Each is a `#rrggbb`
value, and anything left out falls back to the default palette's colour for
that role — so you can change one and inherit the other eight.

```toml
[appearance.custom_theme]
background = "#1a1523"   # the page behind everything
surface    = "#241d30"   # a pane raised above that page
foreground = "#f2ede4"   # ordinary readable text
muted      = "#948f9c"   # text you can ignore: hints, timestamps
border     = "#4a4358"   # the lines around panes
accent     = "#d97757"   # the one colour that draws the eye
warning    = "#e0a72e"   # needs attention, nothing is broken
error      = "#e0685a"   # something is broken
success    = "#7fbf8e"   # something worked
```

`msm profile set custom --accent "#ff0055"` writes one of these without
opening the file.

> [!TIP]
> Every built-in palette is checked by a test to keep its body text at or above
> the 4.5:1 contrast ratio the WCAG accessibility guidelines set for readable
> text. A hand-written custom theme is not checked, so if text becomes hard to
> read, that is the first thing to look at.

### `animations`

`fast`, `reduced` or `off`, cycled in the interface with
<kbd>Alt</kbd>+<kbd>A</kbd>.

`reduced` is not the same animation played slowly — that would make effects
last *longer*, which is the opposite of what asking for less motion means. It
takes bigger steps at a slower rate, so an effect finishes in about the same
time having drawn roughly a third of the frames.

`off` draws every animated element at its finished frame. Nothing is hidden by
the setting; things simply do not move.

### `telemetry`

Shows the processor share, resident memory and drawn frame rate at the
right-hand end of the tab bar (<kbd>Alt</kbd>+<kbd>T</kbd>). While it is off,
nothing is measured at all — reading the numbers once a second for a display
nobody is looking at is exactly the sort of cost this is meant to expose.

Processor time and memory are read from `/proc` and are Linux-only; elsewhere
the frame rate is shown by itself.

### `toasts` and `toast_seconds`

Notifications appear in the bottom-right corner and expire on their own. A
warning stays up twice as long as a notice and an error three times, since an
error is likelier to be the thing you have to act on.
<kbd>Alt</kbd>+<kbd>M</kbd> opens the full session history, so nothing that
flashed past while you were reading chat is lost.

`toasts = false` stops them appearing. The activity log still records
everything either way, and the history is still there.

`toast_seconds` is clamped to between 1 and 60: a zero would make messages
vanish before they could be read.

### `terminal_background`

A terminal window is almost always bigger than the content in it, and every
cell this program has not drawn keeps whatever background your terminal itself
was configured with — so a light theme ends up framed in dark. Turning this on
asks the terminal to change its own background to match the theme, which
reaches the parts of the window this program never draws to.

It is off by default because it is not always wanted: a terminal background is
often deliberately transparent or blurred, and this replaces that with a solid
colour. It is undone when `msm` exits.

---

## `[obs]`

Controlling OBS Studio from the OBS tab (<kbd>Alt</kbd>+<kbd>4</kbd>) and the
`msm obs` commands.

OBS has a WebSocket server built in. Turn it on under **Tools → WebSocket
Server Settings**, and note the port and password it shows you.

```toml
[obs]
enabled = true
host = "127.0.0.1"
port = 4455
password = ""                              # prefer the environment variable
password_env = "OBS_WEBSOCKET_PASSWORD"
```

### `enabled`

On by default. Connecting costs one local socket, and anyone running OBS almost
certainly wants the pane; with no OBS listening the attempt fails quietly and
retries in the background, which looks the same as having it turned off.

Set it to `false` to stop even trying — the OBS tab then says so rather than
sitting on "waiting for OBS".

### `password` and `password_env`

`password_env` names an environment variable to read the password from, and is
the better of the two. `config.toml` already holds your Twitch and Google
credentials; a password sitting in a file is one more place it can be read
from, and an OBS password lets whoever has it control your stream.

If both are set, the file wins — naming a value explicitly should beat
inheriting one. An environment variable that exists but is empty counts as
unset, because that is what an unfilled shell variable looks like.

The password is never actually sent. OBS issues a salt fixed to the password
and a challenge fresh for each connection, and what travels over the wire is a
hash of all three — so capturing it does not allow a replay, which matters
because the connection itself is unencrypted.

### Aliases and shortcuts

Four optional tables give scenes and audio inputs shorter names and one-key
bindings. They are written `alias = "the OBS name"`, which is the readable
direction:

```toml
[obs.scene_aliases]
brb = "Be Right Back"
cam = "Main Camera"

[obs.scene_shortcuts]
1 = "Starting Soon"
2 = "Main Camera"
3 = "Be Right Back"

[obs.audio_aliases]
mic = "Mic/Aux"

[obs.audio_shortcuts]
m = "Mic/Aux"
```

An **alias** is a name you can use anywhere a scene or input is named —
`msm obs scene brb` — and it is what the pane shows in the list. The real OBS
name is still displayed beside it, or the pane and the OBS window would
disagree about what everything is called.

A **shortcut** is a single key that acts on the OBS tab: pressing `3` switches
to Be Right Back, pressing `m` toggles the microphone. Keep them to one
character; anything longer can never be typed as a shortcut.

Both are resolved in a fixed order — an exact shortcut, then an exact alias,
then the exact OBS name, then the same three ignoring case. The order is fixed
so that adding a scene can never silently change what an existing alias means.

> [!NOTE]
> Shortcuts take precedence over the tab's own keys, so binding `s` to a scene
> means `s` no longer starts the stream on that tab. Bind digits and letters
> the tab does not use: `s`, `r`, `p`, `m`, `M`, `P`, `C`, `R`, `j`, `k` and
> `q` all already do something.

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
