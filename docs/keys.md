# Command reference

The terminal interface is the way `msm` is meant to be used. The subcommands
exist for the things a full-screen interface is bad at: one-off logins, scripted
go-lives, printing a stream key, and housekeeping.

**Every command at a glance**

| Command | Purpose |
|---|---|
| [`msm`](#msm--msm-tui) | Open the interface. This is the default. |
| [`msm tui`](#msm--msm-tui) | The same thing, named explicitly. |
| [`msm login <PLATFORM>`](#msm-login-platform) | Authorise a platform in your browser. |
| [`msm logout <PLATFORM>`](#msm-logout-platform) | Forget a saved login. |
| [`msm status`](#msm-status) | Which platforms are logged in, and where the config lives. |
| [`msm go`](#msm-go) | Apply the config preset without opening the interface. |
| [`msm key <PLATFORM>`](#msm-key-platform) | Print a stream key. |
| [`msm categories <QUERY>`](#msm-categories-query) | Search Twitch's category list. |
| [`msm streams`](#msm-streams) | List the stream keys on your YouTube channel. |
| [`msm cleanup`](#msm-cleanup) | Find, and optionally delete, YouTube broadcasts that never went live. |
| [`msm obs`](#msm-obs) | Control OBS Studio. |
| [`msm doctor`](#msm-doctor) | Check the setup and report anything that would stop a stream. |
| [`msm setup <PLATFORM>`](#msm-setup-platform) | Fill in one platform's API credentials from the command line. |
| [`msm profile`](#msm-profile) | Look at and change the colour theme. |
| [`msm init`](#msm-init) | Write a commented starter config file. |
| [`msm paths`](#msm-paths) | Show where config, tokens and logs live. |

`msm --help` prints the same list; `msm <command> --help` prints the long
description of one command.

---

## The global flag

```
-c, --config <FILE>
```

Use a specific config file instead of the default one. Accepted before or after
any subcommand, and by every subcommand.

```bash
msm --config ~/streams/coding.toml go
msm status --config ~/streams/coding.toml
```

Two commands accept it but do not act on it: `msm init` always writes to the
default location, and `msm paths` always prints the default location.

Also available:

| Flag | Where | Effect |
|---|---|---|
| `-h`, `--help` | Everywhere | Short help. On a subcommand, `--help` gives the long form and `-h` the summary. |
| `-V`, `--version` | Root command only | Print the version. It is **not** accepted after a subcommand — `msm streams --version` is an error. Use `msm --version`. |

---

## Naming a platform

`login` and `logout` act on every platform you name. **`key` does not**: it reads
the first platform in the list and ignores the rest, so give it exactly one —
`msm key twitch`, never `msm key twitch,youtube`.

Commands that take a `<PLATFORM>` argument accept:

| You can write | Meaning |
|---|---|
| `twitch` or `ttv` | Twitch |
| `youtube` or `yt` | YouTube |
| `all` | Both |
| `twitch,youtube` | A comma-separated list; spaces around the commas are fine |

Case does not matter. An unrecognised name is refused with a message naming what
you typed and what was expected.

---

## `msm` / `msm tui`

```bash
msm
msm tui
```

Opens the terminal interface: pick platforms, fill in the form once, submit to
both, then watch the statistics. The three screens and their keys are described
in [Getting started](getting-started.md#7-your-first-stream).

Running `msm` with no arguments is identical to `msm tui`. The explicit form
exists so that scripts and shell aliases can say what they mean.

---

## `msm login <PLATFORM>`

```bash
msm login all
msm login twitch
msm login youtube
```

Runs the browser authorisation for each named platform in turn and saves the
resulting tokens to `tokens.json`.

What happens, and why it is shaped this way, is described in
[Getting started, step 6](getting-started.md#6-log-in). In brief: a listener is
started on the configured `oauth_port` first, then your browser is opened; you
sign in on the platform's own site; the platform redirects back to
`http://localhost:<port>/callback`; the code that arrives there is exchanged for
tokens. If no browser opens, the URL is printed for you to paste in.

The login waits up to five minutes, so a forgotten browser tab cannot leave the
command hanging forever.

Run this again whenever a saved login stops working, or when an update adds a
permission your existing token does not carry.

---

## `msm logout <PLATFORM>`

```bash
msm logout youtube
msm logout all
```

Deletes the saved tokens for each named platform. It prints whether there was
anything to forget, so running it twice is harmless and honest about it.

This removes the local copy only. To withdraw the authorisation at the platform
end as well, do that in your Twitch or Google account settings.

---

## `msm status`

```bash
msm status
```

Prints three things:

* **Logins** — per platform, whether tokens are saved, how long the current
  access token is valid for, and whether a refresh token is present so renewal
  can happen without you.
* **Credentials configured** — whether the client id and secret are filled in.
* **Ready to stream to** — the platforms that are actually usable right now,
  plus the config file path.

This is the first thing to run when something is not working, because it
separates "the credentials are missing" from "the login has expired" without
touching the network.

---

## `msm go`

```
msm go [--platforms <LIST>] [-y|--yes] [--json]
```

Applies the `[preset]` section of your config to the selected platforms without
opening the interface. This is the scriptable path: edit the file, run the
command.

| Flag | Effect |
|---|---|
| `--platforms <LIST>` | Override which platforms to use, e.g. `--platforms twitch` or `--platforms twitch,youtube`. Without it, the `platforms` key from `[preset]` is used. |
| `-y`, `--yes` | Skip the confirmation prompt. |
| `--json` | Print the result as JSON on stdout instead of the human report. Implies `--yes`. No short form. |

The sequence is: connect to each platform and print which account it resolved
to; resolve the Twitch category name to an id if the config has no id yet; check
the plan for problems detectable without an API call; show a summary and ask;
then submit.

```bash
msm go
msm go --yes
msm go --platforms twitch --yes
msm --config ~/streams/gaming.toml go --yes
```

Problems found during the check are printed as `error:` (which stops the run) or
`note:` (which does not). An over-long title is an error for Twitch but only a
note for YouTube, because it can be shortened for YouTube alone rather than
failing everything.

**Exit status.** Non-zero when *every* platform failed, so a wrapper script can
tell without parsing the output. If one platform succeeded and another did not,
the exit status is zero: the platform that worked is genuinely ready and you can
stream to it. See [How it works](how-it-works.md#partial-success).

**Stream keys are never printed.** Where a key exists, the human report shows a
`Key:` line saying it is hidden and pointing at `msm key`. Use
[`msm key`](#msm-key-platform) when you actually need one.

### `--json`

```bash
msm go --json > result.json
```

Emits a pretty-printed JSON array with one object per platform, in the canonical
Twitch-then-YouTube order. Every progress line is routed to stderr, so stdout
holds the document and nothing else — redirecting it produces a file a parser
will accept.

Every field is present on every object, with `null` where the platform supplied
nothing, so a consumer can read `.watch_url` without first checking that it
exists. Keys come out in alphabetical order.

| Field | Type | Meaning |
|---|---|---|
| `error` | string or `null` | Why this platform failed. `null` on success. |
| `ingest_url` | string or `null` | The RTMP address OBS pushes to. |
| `manage_url` | string or `null` | Your own management page: Twitch Stream Manager, or YouTube Studio's live control room. |
| `notes` | array of strings | Human-readable remarks, such as which stream key was reused. Empty on failure. |
| `ok` | boolean | Whether this platform is ready. |
| `platform` | string | `"twitch"` or `"youtube"`. |
| `watch_url` | string or `null` | Where viewers watch. |

```json
[
  {
    "error": null,
    "ingest_url": "rtmp://live.twitch.tv/app",
    "manage_url": "https://dashboard.twitch.tv/u/yourname/stream-manager",
    "notes": [
      "Channel updated. Twitch has no separate \"create broadcast\" step — start streaming in OBS whenever you are ready."
    ],
    "ok": true,
    "platform": "twitch",
    "watch_url": "https://twitch.tv/yourname"
  }
]
```

The stream key is deliberately absent from this document. Output like this gets
piped into other programs, redirected into files and pasted into bug reports,
and unlike a password there is no prompt standing in front of a stream key.

---

## `msm key <PLATFORM>`

```bash
msm key twitch
```

Prints one stream key on stdout and nothing else, so it can be piped or copied.
It is a separate command precisely so that no other command ever prints a key by
accident.

```bash
msm key twitch | wl-copy      # Wayland
msm key twitch | pbcopy       # macOS
```

**YouTube behaves differently.** A YouTube stream key belongs to a stream object
that is bound to a particular broadcast, and this command has no broadcast in
hand, so `msm key youtube` does not print a key. It tells you the two places one
can be read instead: the dashboard after `msm go`, and YouTube Studio. To list
the keys on the channel with their ids, use [`msm streams`](#msm-streams).

If Twitch reports no key, the usual cause is a saved login that predates the
`channel:read:stream_key` permission. Run `msm login twitch` again.

---

## `msm categories <QUERY>`

```bash
msm categories chess
msm categories "software"
```

Searches Twitch's category list and prints the matches as an id and name table.
Twitch's search is fuzzy, so `software` finds *Software and Game Development*.

```
ID             NAME
1469308723     Software and Game Development
509658         Just Chatting
```

Put the **name** into `twitch_category` in your config; the id is filled in
automatically the first time it is used. The command exists because Twitch's
update endpoint accepts only the numeric id, and getting the name exactly right
in a hand-edited file is otherwise guesswork.

Requires Twitch credentials and a Twitch login, since the search is an
authenticated API call.

---

## `msm streams`

```
msm streams [--show-keys]
```

Lists the stream objects — the RTMP ingest endpoints, one per stream key — that
exist on your YouTube channel.

| Flag | Effect |
|---|---|
| `--show-keys` | Also print the key itself. No short form, on purpose. |

```bash
msm streams
```

```
YouTube channel: Your Channel

ID                         PINNED  TITLE
Vy8dQ...oqA                yes     Default stream key
9tRk2...LmX                        multistream-manager (reusable)

Stream keys are hidden. Pass --show-keys to print them — anyone holding a key
can broadcast to your channel, so think twice on a shared screen.
```

The `ID` column is the point of the command: that value is what goes into
`stream_id` under `[youtube]` in your config, pinning one key so that every
broadcast binds to the same one and your OBS or Aitum settings never need
changing. `PINNED` marks the one your config currently names.

The listing also warns you when:

* **`stream_id` names a stream that is not on the channel.** Left uncorrected
  this makes every go-live fail, and the error at that point is much harder to
  interpret than a warning here.
* **Several keys exist and none is pinned.** Whichever one YouTube lists first
  is the one that gets bound, and that ordering is not yours to control.

Keys are hidden by default because a key is enough on its own to broadcast to
your channel and this output lands in terminal scrollback, where it stays. A
stream whose key the API did not report is shown as `(not reported)` rather than
as a blank column.

If the channel has no streams at all, the command says so and explains that the
first one is created the first time you run `msm go` with YouTube selected.

---

## `msm cleanup`

```
msm cleanup [-y|--yes]
```

Finds YouTube broadcasts that were created but never received a video feed, and
optionally deletes them.

| Flag | Effect |
|---|---|
| `-y`, `--yes` | Delete the broadcasts listed instead of only showing them. |

Why these accumulate: submitting a plan a second time creates a **new** YouTube
broadcast rather than editing the previous one — that is how YouTube's API
works. A session where you fixed a typo and resubmitted leaves the earlier
attempt behind, and nothing removes it by itself, so YouTube Studio's list of
upcoming streams fills up with abandoned entries.

```bash
msm cleanup
```

```
YouTube channel: Your Channel

2 broadcasts were created but never went live:

ID                         STATUS    SCHEDULED          TITLE
kR4...Q1                   created   2026-08-01 19:04   Friday coding
mT9...Zz                   ready     2026-08-03 20:15   Friday coding

Nothing was deleted. Run `msm cleanup --yes` to delete the broadcasts above.
Anything that has ever been live is neither listed here nor deleted.
```

Scheduled times are shown in your own time zone, because that is the one you
would have been streaming in and so the one that makes a broadcast
recognisable.

**What can be selected.** The rule is deliberately cautious, because deleting a
broadcast that holds a recording somebody wanted cannot be undone while leaving
an orphan behind costs nothing but clutter. A broadcast is listed only when
**both** hold:

* YouTube reports neither an `actualStartTime` nor an `actualEndTime`. Either of
  those means a feed reached it at some point.
* Its lifecycle status is still `created` (made, not yet bound and validated) or
  `ready` (bound, waiting). Everything else — `live`, `testing`, `complete`,
  `revoked`, and the transitional states — is left alone.

Anything that has ever received a feed is never listed and never deleted. A
broadcast whose details are missing from YouTube's reply is treated as *not*
stale, because there is then no evidence either way and the safe answer is to
keep it.

With `--yes`, each deletion is reported individually and one that refuses to be
deleted does not stop the rest. The command fails only if none of them could be
deleted.

---

## `msm obs`

```bash
msm obs status
msm obs scenes | msm obs audio
msm obs scene <TARGET>
msm obs mute <TARGET> | unmute <TARGET> | toggle-mute <TARGET>
msm obs volume <TARGET> <PERCENT>
msm obs stream | record | pause-recording
msm obs profiles [NAME] | collections [NAME]
```

Drives OBS Studio. The interface's OBS tab (<kbd>Alt</kbd>+<kbd>4</kbd>) is the
comfortable way to do all of this; these exist for a stream deck hotkey, a
shell alias, or a scene change fired from another program.

`<TARGET>` is a scene or input named any of three ways: its OBS name, an alias
from `[obs.scene_aliases]` / `[obs.audio_aliases]`, or a shortcut. A name that
matches nothing is refused with a list of what would have worked.

Each invocation opens its own connection, does one thing and exits — which is
why there is no background daemon to start or manage.

**Reading**

`status` prints everything at once: versions, the current scene, profile and
collection, whether streaming and recording are running and for how long, every
audio input with its mute state and level, and OBS's own performance figures
including frames lost at the encoder and frames dropped on the way out.

`scenes`, `audio`, `profiles` and `collections` list just that, marking the
current one with `*`.

**Changing**

Every changing command prints what it produced, read back from OBS rather than
assumed:

```
$ msm obs stream
Streaming: on
$ msm obs volume mic 40
Mic/Aux: 40%
```

`stream`, `record` and `pause-recording` are toggles, and `toggle-mute` is the
one to bind to a key: a toggle cannot act on a stale idea of which way round
something currently is, which matters when it may also have been changed in OBS
itself a moment earlier.

`volume` takes a percentage of unity gain and is capped at 100. Amplifying past
unity is a deliberate act with real consequences for how a stream sounds, and
not something to reach by mistyping a number.

**When OBS is not there**

The command fails with a non-zero status and says what to check, rather than
retrying — a command line has somebody waiting for it, and silently retrying
for thirty seconds would look like a hang. (The *interface* does retry, because
nobody is waiting on it.)

```
$ msm obs status
Error: could not reach OBS: connecting to OBS at ws://127.0.0.1:4455: Connection refused

Is OBS running, with its WebSocket server turned on under Tools → WebSocket Server Settings?
```

---

## `msm doctor`

```bash
msm doctor
```

Checks everything that has to be right for a stream to start, and says what is
not. Run it first when something is not working.

```
msm doctor

  [ ok ]  config file: /home/you/.config/msm/config.toml
  [ ok ]  Twitch credentials configured
  [warn]  YouTube has no client id and secret
          Run `msm setup youtube`, or fill them in on the interface's first
          screen. Skip this if you do not stream to YouTube.
  [ ok ]  Twitch — token valid for 3 hours
  [ ok ]  clipboard: wl-copy will copy the stream key
  [ ok ]  theme: nord
  [ ok ]  terminal: 24-bit colour
  [ ok ]  log file: /home/you/.config/msm/msm.log

Nothing is broken.
```

The checks run in the order things matter in — a missing client id makes the
login question moot, and a login you do not have makes the stream key question
moot — so the first `[FAIL]` is usually the only one worth acting on. Every
warning says what to *do*, not only what is wrong.

**`[warn]` and `[FAIL]` mean different things.** A fresh install with no logins
yet is unfinished, not broken, and a tool that reports that in red teaches you
to ignore red. Only a `[FAIL]` is counted in the closing summary.

Two checks are worth explaining:

* **clipboard** — stream keys are copied, never displayed, so this reports
  which helper program will do it. It distinguishes "not installed" from
  "installed but with no display to talk to", which is the normal state over
  ssh; in that case nothing needs installing and copying falls back to a
  terminal escape sequence (OSC 52), answered by the terminal on your own desk.
* **terminal** — themes are written as exact 24-bit colours. A terminal that
  does not advertise `COLORTERM=truecolor` will approximate them.

The exit status is `0` when nothing failed and non-zero when something did, so
this works in a script — `msm doctor && msm go` will not go live on a setup
that has just been reported as broken. Warnings do not affect it, for the same
reason they are not printed as failures.

---

## `msm setup <PLATFORM>`

```bash
msm setup twitch
msm setup youtube
msm setup twitch --client-id "abc123"
```

Fills in one platform's API credentials and saves them to `config.toml`. It
prints the developer-console URL and the exact redirect URL to register before
asking for anything.

The interface asks for these itself on first run and that remains the easier
path. This exists for the times a full-screen interface is not available at
all: a headless machine over ssh, a container, or a scripted install.

One platform at a time — `all` is meaningful for `login` and `logout`, but a
single pair of credentials cannot belong to two platforms.

> [!WARNING]
> `--client-secret` exists for scripting, but a secret passed as an argument
> ends up in your shell history and in the process list, where anyone on the
> machine can read it. Leaving the flag off and typing it at the prompt does
> not: the prompt hides what you type.

Where terminal echo cannot be turned off (`stty` is unavailable), the prompt
says so before you type rather than silently showing the secret.

---

## `msm profile`

```bash
msm profile list
msm profile show [NAME]
msm profile set <NAME> [--accent "#rrggbb" ...]
```

The colour theme, from the command line. The interface half is
<kbd>Ctrl</kbd>+<kbd>T</kbd>, which previews each palette live.

**`list`** prints all 57 built-in names plus `custom`, marking the one in use:

```
  nord
* gruvbox
  dracula
```

**`show`** prints a theme's nine colours. With no name it shows the one in use.

```
$ msm profile show nord
nord
  background  #2e3440
  foreground  #eceff4
  accent      #88c0d0
  ...
```

**`set`** switches theme, or writes your own colours:

```bash
msm profile set gruvbox                       # a built-in one
msm profile set custom --accent "#ff0055"     # one colour, keeping the rest
```

Any colour flag writes into `[appearance.custom_theme]`, so you can change one
role and inherit the other eight from the default palette.

Two things are refused rather than quietly accepted:

* **a name that is not a real theme**, because writing it to the config file
  would produce a setup that falls back to the default at every start-up
  without ever explaining why;
* **a colour flag on a built-in theme**, because the built-ins are fixed —
  `custom` is the one that takes overrides.

A value that is not a `#rrggbb` colour is refused too, naming the role it was
given for.

---

## `msm init`

```bash
msm init
```

Writes a commented starter `config.toml` with every setting present and
explained, then prints the next three steps. The file is created with owner-only
permissions.

If the file already exists it is left alone and the command says so — your
credentials are not going to be overwritten by a stray `msm init`. Delete the
file first if you genuinely want a fresh one.

This always writes to the default location. `--config` does not redirect it.

---

## `msm paths`

```bash
msm paths
```

```
Config: /home/you/.config/multistream-manager/config.toml
Tokens: /home/you/.config/multistream-manager/tokens.json
Log:    /home/you/.config/multistream-manager/msm.log
```

Useful on its own, and useful in a pipeline when you want to watch the log:

```bash
MSM_LOG=debug msm                                    # in one terminal
tail -f "$(msm paths | awk '/^Log:/{print $2}')"     # in another
```

This always prints the default location, whatever `--config` says.

---

* [Configuration](configuration.md) — every key in the file the commands read.
* [How it works](how-it-works.md) — what happens between `msm go` and "Ready".
* [Troubleshooting](troubleshooting.md) — when a command fails.
* [Back to the documentation index](README.md).
