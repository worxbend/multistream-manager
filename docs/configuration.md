# Configuration

Everything `msm` remembers lives in one file, `config.toml`. It holds two quite
different kinds of thing:

* **Credentials** — the client id and secret from each developer console. Set up
  once, then forgotten about.
* **Everything else** — the preset the form starts from, the colours, the key
  bindings, the layout of the Combined tab, the chat and OBS settings. All of it
  can be changed inside the interface, and all of it can equally be hand-edited
  here; the file is written to be read by a person.

**Contents**

* [Where the file lives](#where-the-file-lives)
* [Shape of the file](#shape-of-the-file)
* [`[twitch]`](#twitch)
* [`[youtube]`](#youtube)
* [`[general]`](#general)
* [`[chat]`](#chat)
* [`[appearance]`](#appearance)
* [`[obs]`](#obs)
* [`[keys]`](#keys)
* [`[layout]`](#layout)
* [`[preset]`](#preset)
* [A full worked example](#a-full-worked-example)
* [The preset workflow](#the-preset-workflow)
* [Keeping more than one preset](#keeping-more-than-one-preset)
* [Environment variables](#environment-variables)
* [Other files in the same directory](#other-files-in-the-same-directory)

---

## Where the file lives

**Config → Files** (<kbd>Alt</kbd>+<kbd>5</kbd>, then the *Files* section) shows
the config, token and log paths. The directory follows each operating system's
own convention:

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

You do not have to create it at all. On a first run the interface opens on its
**Set up API access** screen, and saving that form writes the file for you —
which is what the old `msm init` command used to do.

---

## Shape of the file

Nine sections, all optional. Anything you leave out falls back to the default
listed below, so a partial file is valid and a completely empty file parses.

```toml
[twitch]      # Twitch application credentials
[youtube]     # Google OAuth credentials, plus stream-key reuse settings
[general]     # settings that belong to neither platform
[chat]        # the chat panes: scrollback, polling, quota, logging
[appearance]  # colours, motion, mouse, notifications
[obs]         # controlling OBS Studio
[keys]        # key bindings, written the way vim writes them
[layout]      # how the Combined tab is arranged
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
your OBS setup is actually configured for. To see the ids, open
**Config → Housekeeping** and run *List YouTube stream keys*; the results appear
in the activity log:

```
ID                         PINNED  TITLE
Vy8dQ...oqA                        Default stream key
9tRk2...LmX                        Old backup key
```

Only the ids are ever listed. A stream key itself is never shown, because this
window is often part of the broadcast.

Copy the id you want into the config:

```toml
[youtube]
stream_id = "Vy8dQ...oqA"
```

The listing marks the pinned one in the `PINNED` column afterwards, and warns
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
**Config → Housekeeping → *Export paid events to CSV*** reads them back into a
CSV of every paid event, with no network access and no API quota spent, which is
what makes them worth keeping.
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

A name that does not exist logs a warning and falls back to the default rather
than failing to start, so a typo costs a line in the log and not your stream.

The interface half of this is the theme picker, <kbd>&lt;Leader&gt;</kbd>
<kbd>u</kbd> <kbd>t</kbd>, which lists every name and previews each palette
live — the whole screen redraws as you move the selection, because
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

The theme picker writes `theme` back here when you keep a palette, so you can
choose in the interface and still hand-edit the result afterwards.

> [!TIP]
> Every built-in palette is checked by a test to keep its body text at or above
> the 4.5:1 contrast ratio the WCAG accessibility guidelines set for readable
> text. A hand-written custom theme is not checked, so if text becomes hard to
> read, that is the first thing to look at.

### `animations`

`fast`, `reduced` or `off`, cycled in the interface with
<kbd>&lt;Leader&gt;</kbd> <kbd>u</kbd> <kbd>a</kbd>.

`reduced` is not the same animation played slowly — that would make effects
last *longer*, which is the opposite of what asking for less motion means. It
takes bigger steps at a slower rate, so an effect finishes in about the same
time having drawn roughly a third of the frames.

`off` draws every animated element at its finished frame. Nothing is hidden by
the setting; things simply do not move.

### `telemetry`

Shows the processor share, resident memory and drawn frame rate at the
right-hand end of the tab bar (<kbd>&lt;Leader&gt;</kbd> <kbd>u</kbd>
<kbd>y</kbd>). While it is off,
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

Controlling OBS Studio from the OBS tab (<kbd>Alt</kbd>+<kbd>4</kbd>) and from
the <kbd>&lt;Leader&gt;</kbd> <kbd>o</kbd> bindings, which work from any tab.

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

An **alias** is a shorter name for a scene or an input, and it is what the pane
shows in the list. The real OBS
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
> the tab does not use: `s`, `r`, `p`, `m`, `M`, `P`, `C`, `R`, `u`, `h`, `l`,
> `j`, `k` and `q` all already do something — the full list is in
> [Keys and actions](keys.md#obs).

---

## `[keys]`

Every key in the program is configurable here. The built-in bindings are the
starting point and this section is applied on top, so you write only what you
want to be different — there is no need to restate the ones you are happy with.

```toml
[keys]
# The key every mnemonic sequence starts with. Changing it moves every
# <Leader>… binding at once, including the built-in ones, so nothing else
# has to be rewritten.
leader = "<Space>"

[keys.global]
"<C-g>" = "stream.go_live"     # go live from anywhere
"<Leader>q" = ""               # an empty action removes a binding

[keys.chat]
"<C-j>" = "chat.next"

[keys.obs]
"<F1>" = "obs.mute_all"
```

### The four contexts

| Table | Where its bindings apply |
|---|---|
| `[keys.global]` | Everywhere |
| `[keys.stream_info]` | The Stream Info tab |
| `[keys.chat]` | The chat panes, on either the Chat or the Combined tab |
| `[keys.obs]` | The OBS tab |

A key press is looked up in the active tab's context first and in `global`
second. That is what lets <kbd>j</kbd> scroll chat on one tab and move down the
scene list on another without either having to know about the other, and it
means a tab can give a key a local meaning without you restating everything else.

### How a binding is written

The left-hand side is a key, or several in a row, written the way vim writes
them: `j`, `J`, `<C-p>`, `<A-4>`, `<CR>`, `<Leader>os`. The notation is vim's
because anyone who would want to rebind keys in a terminal program already knows
it. The full table of forms is in
[Keys and actions](keys.md#how-a-key-is-written).

The right-hand side is an **action name** such as `obs.stream` or `chat.compose`
— a thing the program does, named independently of whichever key happens to run
it. That separation is the reason the keys are configurable at all. Every valid
name is listed in [Keys and actions](keys.md#every-action-name).

An empty string removes a binding: `"<Leader>q" = ""` means the leader followed
by `q` no longer quits.

> [!NOTE]
> A binding that cannot be understood — a key that will not parse, an action
> name that does not exist — is reported and skipped rather than treated as a
> reason to refuse to start. Being locked out of your own stream by a mistyped
> binding would be an absurd trade. **Config → Keys** lists every binding
> actually in force, which is where to look when a change did not take effect.

---

## `[layout]`

How the **Combined tab** (<kbd>Alt</kbd>+<kbd>3</kbd>) is arranged. That tab is
the one you put on a second monitor and leave there for the whole stream, and
what belongs on that screen is not the same for somebody streaming alone as for
somebody with a moderator, a second camera and a chat they need to watch closely.

The easiest way to change it is **Config → Layout**, which draws a live preview
of the arrangement while you edit it — see
[Keys and actions](keys.md#layout) for its keys. This section is what that
editor saves.

### The eight panels

| Written as | On screen | What it shows |
|---|---|---|
| `stream_info` | Stream info | Title, category, which platforms are live |
| `twitch_chat` | Twitch chat | Twitch chat messages |
| `youtube_chat` | YouTube chat | YouTube live chat messages |
| `obs_scenes` | Scenes | The OBS Studio scene list |
| `obs_audio` | Audio | OBS audio inputs and their levels |
| `obs_status` | OBS status | The OBS connection, and whether it is recording or streaming |
| `activity_log` | Activity | The rolling log of what the program has been doing |
| `stats` | Statistics | Live viewer counts and stream health |

### The file format

A layout is a list of rows, and each row is a list of panels:

```toml
[layout]
# "vertical" stacks the rows top to bottom; "horizontal" makes them columns.
direction = "vertical"

[[layout.rows]]
weight = 1
panels = [{ panel = "stream_info", weight = 1 }]

[[layout.rows]]
weight = 3
panels = [
    { panel = "twitch_chat", weight = 1 },
    { panel = "youtube_chat", weight = 1 },
]
```

That example is the default: a strip of stream information across the top, and
the two chats side by side taking three times as much room beneath it. It is
also exactly what the Combined tab looked like before it was configurable, so
upgrading changes nothing for anyone who has not gone looking for this.

### `weight` is a share, not a percentage

The numbers are **proportional shares**. A row of `weight = 1` next to a row of
`weight = 3` gets a quarter of the height, because 1 out of 1 + 3 is a quarter.
Percentages were the obvious alternative and are worse for hand editing: they
have to add up to a hundred, so adding one panel forces you to re-edit every
other number. Shares always add up, whatever they are.

Cells are handed out by largest remainder, so the parts always add back up to the
whole and no row is left one character short at the bottom of the screen.

A weight of `0` hides a panel. If *every* panel in one split is zero the split
would take up no space at all, so that is refused with an explanation rather than
drawn as nothing.

### Why rows rather than a tree

Internally a layout is a tree of splits, which is what the arithmetic naturally
works on. Writing that tree into TOML directly would produce something like
`[layout.root.split.children.split]`, with the panels buried several tables down
and indentation carrying a meaning TOML does not actually give it. People edit
this file by hand — that is the whole point of having one — so the written form
is chosen for the reader.

The cost is that the file can express two levels: rows, and panels within a row.
That covers the arrangements people actually want on a second monitor. Nesting
deeper than that is refused, and a tree more than eight levels deep is refused
outright, on the grounds that it is far more likely to be a mistake than an
intention.

### Presets

Four ready-made arrangements exist as starting points, cycled with <kbd>p</kbd>
in the layout editor. Nobody wants to design a layout from nothing on their first
evening; pick the nearest one, save it, then move the numbers around.

| Preset | What it is |
|---|---|
| `default` | Stream info across the top, both chats beneath |
| `chat_focus` | Big chats, a thin strip of stream info |
| `obs_focus` | OBS scenes and audio down one side |
| `everything` | All eight panels at once, for a large monitor |

> [!NOTE]
> A layout that cannot be read — a panel name that does not exist, a shape the
> file format cannot express — falls back to the default arrangement and says
> why in the activity log. A blank tab is indistinguishable from a broken one,
> so it never leaves you with either.


---

## `[preset]`

The default stream settings — what the form is filled in with when the interface
opens. <kbd>Ctrl</kbd>+<kbd>S</kbd> in the form writes whatever you have typed
back here, so you can build a preset once by hand and never retype it.

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
| `platforms` | array of strings | `["twitch", "youtube"]` | Which platforms are ticked when the interface opens. |

### Why there are two Twitch category keys

Twitch's channel-update endpoint does not accept a category *name*. It accepts a
numeric `game_id` and nothing else. A human editing a file wants to write the
name, so the file holds both: `twitch_category` is the name you type, and
`twitch_category_id` is the id looked up for it.

When you go live with a name but no id, the name is searched against
Twitch's category list first — an exact match wins, otherwise the best match
does. Caching the id means a repeat run does not spend an API call re-resolving
a category that has not changed. Change the name and leave the id stale and the
pair is treated as unresolved, so it gets looked up again.

To find the exact spelling and id of a category, use the category field in the
form: press <kbd>Enter</kbd> on it and type. It searches Twitch's own list and
fills in both keys for you.

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

# Pin one specific key. Find the id under Config -> Housekeeping -> "List
# YouTube stream keys". Empty is fine when the channel has only one key.
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

# Spelled exactly as Twitch spells it. The form's category field searches
# Twitch's own list and fills both this and the id in for you.
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

The `[keys]` and `[layout]` sections are left out of this example on purpose:
they are long, and everything in them has a working default. Add them only when
you want something different from what the interface already does.

---

## The preset workflow

The form and the file are two views of the same thing, and they feed each other.

**File → form.** Edit `[preset]`, start `msm`, and the form on the Stream Info
tab opens already filled in. Look it over, press <kbd>Ctrl</kbd>+<kbd>G</kbd>,
and both platforms are configured. Getting a session started is then two
keystrokes rather than a form to retype.

Before it sends anything, the go-live step connects to each selected platform and
records in the activity log which account it resolved to, resolves the Twitch
category name if it has no id yet, and checks the plan for the problems it can
find without calling an API — a missing category, a language code that is not two
letters. The submit hint at the bottom of the form turns green only when the plan
is genuinely sendable, so you do not discover a missing category after the round
trip.

**Form → file.** Pressing <kbd>Ctrl</kbd>+<kbd>S</kbd> in the form writes
whatever you have typed back into `[preset]`, including the resolved Twitch
category id. So you can build a preset in the interface once and never edit the
file by hand at all.

---

## Keeping more than one preset

Earlier versions had a `--config` flag for pointing at a different file. There is
no command line any more, so that is gone; what remains is the `MSM_CONFIG_DIR`
environment variable, which moves the **whole** directory — config, tokens and
log together:

```bash
MSM_CONFIG_DIR=~/streams/coding msm
MSM_CONFIG_DIR=~/streams/gaming msm
```

Two shell aliases, and each kind of stream keeps its own preset, its own theme
and its own layout. The trade compared with the old flag is that each directory
also holds its own logins, so you authorise once per directory rather than once
per machine.

If that is more separation than you want, the alternative is to keep one config
and change the title in the form before each stream. <kbd>Ctrl</kbd>+<kbd>S</kbd>
saves it back, so the file always reflects the last stream you set up.

---

## Environment variables

Everything here is optional: `msm` runs with none of them set.

| Variable | Effect |
|---|---|
| `MSM_TWITCH_CLIENT_ID` | The Twitch client id, when `[twitch] client_id` is empty. |
| `MSM_TWITCH_CLIENT_SECRET` | The Twitch client secret, when `[twitch] client_secret` is empty. |
| `MSM_YOUTUBE_CLIENT_ID` | The Google client id, when `[youtube] client_id` is empty. |
| `MSM_YOUTUBE_CLIENT_SECRET` | The Google client secret, when `[youtube] client_secret` is empty. |
| `OBS_WEBSOCKET_PASSWORD` | The OBS WebSocket password, when `[obs] password` is empty. |
| `MSM_CONFIG_DIR` | Use this directory for config, tokens and the log instead of the OS default. Created if it does not exist. |
| `MSM_LOG` | Log verbosity, in the usual `tracing` syntax: `MSM_LOG=debug`, or something narrower like `MSM_LOG=multistream_manager::youtube=trace`. Defaults to `info`. |
| `COLORTERM` | Not read for configuration, but `truecolor` here is what tells the interface your terminal can show a theme's exact colours. |

The four credential variables and the OBS one are the *default* names. Each is
set by the matching `*_env` key, so you can point them wherever your setup
already keeps things:

```toml
[twitch]
client_secret_env = "TWITCH_SECRET_FROM_MY_PASSWORD_MANAGER"
```

### Credentials in the environment

Every credential can come from either the config file or the environment. The
rules are the same for all of them:

* **The file wins when both are set.** Naming a value explicitly should beat
  inheriting one.
* **A variable that is set but empty counts as unset.** That is what an
  unfilled shell variable looks like, and treating it as a real empty
  credential would fail later in a way nobody could read.
* **Nothing is written back.** A credential that came from the environment
  stays there; saving from the interface never copies it into the file.

Which to use is a real choice rather than a formality:

| | In `config.toml` | In the environment |
|---|---|---|
| Survives a reboot | ✅ | only if exported from a shell profile |
| Set up once, from the interface | ✅ | ❌ — you provide it every session |
| Copied when you copy your dotfiles | ⚠️ **yes, including the secret** | ✅ no |
| Can come from a password manager | ❌ | ✅ |
| Visible to other programs you run | ❌ | ⚠️ yes, child processes inherit it |

A reasonable middle: keep the **client ids** in the file, since they are not
secret, and the **secrets** in the environment.

```bash
# ~/.profile, or whatever your shell reads at login
export MSM_TWITCH_CLIENT_SECRET="…"
export MSM_YOUTUBE_CLIENT_SECRET="…"
export OBS_WEBSOCKET_PASSWORD="…"
```

Better still, have a password manager supply them at the moment `msm` starts,
so they are never written to disk in plain text at all:

```bash
# One example; every password manager has an equivalent.
MSM_TWITCH_CLIENT_SECRET="$(pass show msm/twitch-secret)" msm
```

> [!WARNING]
> A variable exported in a shell profile is **not** visible to a copy of `msm`
> started from a desktop launcher or a systemd unit, because those do not read
> your shell profile. This is the usual reason a credential works in one
> terminal and appears to be missing everywhere else. Config → Diagnostics
> reports which credentials it can actually see.

---

## Other files in the same directory

| File | Contents |
|---|---|
| `config.toml` | This file. Credentials and preset. |
| `tokens.json` | OAuth access and refresh tokens, written with owner-only permissions. A refresh token is as good as a password; logging out under Config → Accounts deletes the entry. |
| `msm.log` | Diagnostics. The interface owns the terminal, so nothing can be printed to the screen while it is running. |

Stream keys are never written to any of them.

---

* [Keys and actions](keys.md) — every binding, and the action names for `[keys]`.
* [OBS and Aitum](obs-and-aitum.md) — why `reuse_stream` matters.
* [Troubleshooting](troubleshooting.md) — when a setting does not behave.
* [Back to the documentation index](README.md).
