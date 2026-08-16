<div align="center">

# 📡 multistream-manager

**Set up a Twitch stream and a YouTube broadcast from one terminal form, then press Start Streaming in OBS.**

[![CI](https://img.shields.io/github/actions/workflow/status/worxbend/multistream-manager/ci.yml?branch=main&style=flat-square&logo=github&label=CI)](https://github.com/worxbend/multistream-manager/actions/workflows/ci.yml)
[![Latest release](https://img.shields.io/github/v/release/worxbend/multistream-manager?style=flat-square&label=release&color=success)](https://github.com/worxbend/multistream-manager/releases)
[![Licence: MIT](https://img.shields.io/badge/licence-MIT-blue?style=flat-square)](LICENSE)
[![MSRV 1.88](https://img.shields.io/badge/MSRV-1.88-orange?style=flat-square&logo=rust&logoColor=white)](https://rustup.rs)
[![Documentation](https://img.shields.io/badge/docs-Pages-8A2BE2?style=flat-square&logo=readthedocs&logoColor=white)](https://worxbend.github.io/multistream-manager/)

</div>

```
╭────────────────────────────────────────────────────────────────────────────────────╮
│ multistream-manager                                               Twitch   YouTube │
╰────────────────────────────────────────────────────────────────────────────────────╯
 Twitch and YouTube are ready. Press Start Streaming in OBS now.
╭─ Twitch ─────────────────────────────╮╭─ YouTube ──────────────────────────────────╮
│ READY — start streaming in OBS       ││ READY — start streaming in OBS             │
│                                      ││                                            │
│ Watch     twitch.tv/worxbend         ││ Watch     youtu.be/<video-id>              │
│ Ingest    rtmp://live.twitch.tv/app  ││ Ingest    rtmp://a.rtmp.youtube.com/live2  │
│ Key       ••••••••••••••••           ││ Key       ••••••••••••••••                 │
│                                      ││                                            │
│ ── Live ──                           ││ • Reused your existing stream key          │
│ Status    live                       ││ ── Live ──                                 │
│ Viewers   142                        ││ Status    live                             │
│ Followers 12.4K                      ││ Viewers   38                               │
│ Uptime    1h 23m                     ││ Likes     17                               │
╰──────────────────────────────────────╯╰────────────────────────────────────────────╯
 r refresh   o open watch page   y copy Twitch key   Y copy YouTube key   e edit   q quit
```

---

## 🎯 What it does

You type the title, description, tags, category and language **once**. Press
<kbd>Ctrl</kbd>+<kbd>G</kbd> and both platforms are configured in parallel. Then
you press "Start Streaming" in OBS exactly as you always did.

Once you are live, the same window shows viewer counts, follower and subscriber
totals, likes and uptime side by side, so neither website needs to be open.

| | 🟣 Twitch | 🔴 YouTube |
|---|---|---|
| 📝 Title | ✅ up to 140 characters | ✅ up to 100 characters, plus your tags as `#hashtags` |
| 📄 Description | ❌ Twitch has no description field | ✅ |
| 🏷️ Tags | ✅ up to 10, spaces stripped | ✅ as typed |
| 🎮 Category | ✅ searched live against Twitch's list | ✅ picked from YouTube's list |
| 🌍 Language | ✅ | ✅ |
| 👁️ Visibility | ❌ | ✅ public / unlisted / private |
| 📺 Creates the broadcast | ❌ not needed — the channel always exists | ✅ a new broadcast per session |
| 🔑 Stream key | 🔒 copy-only, never shown | 🔒 copy-only, reused and never regenerated |
| 👥 Live viewer count | ✅ | ✅ |
| ⭐ Followers / subscribers | ✅ | ✅ |
| 👍 Likes | ❌ | ✅ |

> [!IMPORTANT]
> **It does not control OBS.** It gets the platforms ready; you decide when you
> actually go live. Partial success is normal and supported: if one platform
> fails, the other is still configured, still usable, and the failure is shown
> in its own panel rather than rolling everything back.

The two platforms work differently enough that it shows through — Twitch is a
channel you point OBS at, YouTube is an event you create beforehand. The
consequences (character limits, tag rules, what happens when you resubmit) are
written up in [docs/how-it-works.md](docs/how-it-works.md).

---

## 💬 Chat

The window has three top-level tabs, switched with <kbd>Alt</kbd>+<kbd>1</kbd> /
<kbd>Alt</kbd>+<kbd>2</kbd> / <kbd>Alt</kbd>+<kbd>3</kbd>: **Stream Info**
(everything above), **Chat** — Twitch chat on the left, YouTube live chat on the
right, always side by side — and **Combined**, which puts a compact line of
channel state (connected account, live or not, viewers, uptime) above those same
two chat panes for a second monitor. On the combined tab <kbd>Alt</kbd>+<kbd>w</kbd>
swaps which half the keyboard talks to, since both halves want the same letters.
The divider is resizable. Chat behavior is ported from
[twi](https://github.com/worxbend/twi) and [yc](https://github.com/worxbend/yc);
`PLAN.md` tracks the feature-parity matrix.

**Multiple accounts.** The account you stream with is also your first chat
account. Add more identities any time — each becomes its own sub-tab inside
its platform's pane:

```console
$ msm login twitch --add     # authorise a second Twitch account in the browser
$ msm login youtube --add    # same for another YouTube channel
$ msm logout twitch:thatlogin  # forget one added account again
```

Entering the Chat tab connects every logged-in account's **own** chat (lazily —
nothing connects until you open the tab for the first time; accounts whose
sub-tab is off screen still collect messages, so their unread counts are
truthful). Each pane keeps its own message box, so a half-written Twitch
message survives switching to the YouTube side and back. Press <kbd>space</kbd>
then <kbd>c</kbd> to join any other chat through that account: a Twitch
channel name, or for YouTube a video id, `@handle`, channel id or a plain
youtube.com/youtu.be URL.

**Keys** (vim-flavoured):

| Key | Action |
|---|---|
| <kbd>h</kbd> / <kbd>l</kbd>, arrows, <kbd>tab</kbd> | focus the other pane |
| <kbd>{</kbd> / <kbd>}</kbd> | previous / next account sub-tab |
| <kbd>[</kbd> / <kbd>]</kbd> | previous / next open chat in the account |
| <kbd>j</kbd> / <kbd>k</kbd> | move the message selection (view follows) |
| <kbd>PgUp</kbd> / <kbd>PgDn</kbd>, <kbd>g</kbd> / <kbd>G</kbd> | scroll / jump to oldest & newest |
| <kbd>i</kbd> (or <kbd>o</kbd>/<kbd>a</kbd>) | compose; <kbd>Enter</kbd> sends, <kbd>Esc</kbd> keeps the draft |
| <kbd>r</kbd> | reply to the selected message |
| <kbd>d</kbd> / <kbd>b</kbd> | delete the message / ban its author (YouTube; press twice to confirm) |
| <kbd>t</kbd> | time the author out — prompts for a duration (45s / 5m / 2h, max 24h) |
| <kbd>space</kbd> <kbd>c</kbd> / <kbd>space</kbd> <kbd>x</kbd> / <kbd>space</kbd> <kbd>a</kbd> | join a chat / close it / toggle the activity view |
| <kbd>/</kbd>, <kbd>n</kbd> / <kbd>N</kbd> | search messages incrementally; walk older/newer matches |
| <kbd>1</kbd>–<kbd>4</kbd>, <kbd>0</kbd> | view filters (mentions / roles / paid+membership / notices); reset |
| <kbd>K</kbd> | inspect the selected message's normalized fields |
| <kbd>tab</kbd> (composing) | complete the trailing @mention from the chatter roster |
| <kbd>ctrl</kbd>+<kbd>e</kbd> | emoji picker (built-in catalog, filter and insert) |
| <kbd>ctrl</kbd>+<kbd>g</kbd> / <kbd>b</kbd> / <kbd>y</kbd> / <kbd>n</kbd> | cycle layout (inline/grouped/compact) / badges / emote highlight / full usernames |
| <kbd>&lt;</kbd> / <kbd>&gt;</kbd> / <kbd>=</kbd> | resize the split toward/away from the focused pane / reset |
| <kbd>ctrl</kbd>+<kbd>r</kbd> | reconnect (also overrides a YouTube quota pause) |

Each chat shows its connection state in the pane header (connected,
reconnecting, failed, **quota paused** — YouTube's daily API quota is tracked
locally and polling stops before sending would become unaffordable). Messages
render with per-author stable colors, badge glyphs (◉ owner, ⚔ moderator,
★ member/subscriber, ✓ verified …), Super Chat amount chips colored by tier,
and membership events; deleted messages show `[message deleted]` and the
original text never reappears — these terminals are often on stream.

Chat tuning lives in the `[chat]` section of `config.toml`:

```toml
[chat]
scrollback_limit = 1000        # messages kept per chat
poll_interval_floor_ms = 1000  # YouTube never polls faster (server floor still wins)
poll_interval_ceiling_ms = 0   # 0 = no ceiling
daily_quota_units = 10000      # your YouTube API project quota
quota_reserve_percent = 10     # stop polling early so sends keep working
notifications = true           # desktop notifications for off-screen Super Chats etc.
chat_logging = false           # append every message to rotated JSONL files
chat_log_dir = ""              # empty = chatlog/ under the config directory
```

Composer commands: `/me` (Twitch action), `/clip` (clip your own live Twitch
stream — answers with the edit URL), `/chats [target]` (join prompt or direct
join). With `chat_logging` on, `msm export superchats` turns the logs into a
CSV of every paid event — integer-exact amounts, zero API quota.

## 🤔 Why

If you stream to both platforms at once — for example through OBS with the
[Aitum multistream plugin](https://aitum.tv/vertical) — you know the routine
before every session:

1. Open YouTube Studio, create a live broadcast, type the title, paste the
   description, pick a category, set the visibility, tick "not made for kids".
2. Open the Twitch dashboard, type the *same* title, pick a category from a
   search box, retype the *same* tags, set the language.
3. Copy a stream key somewhere.
4. Finally, press "Start Streaming" in OBS.

`msm` collapses steps 1 to 3 into one form.

> **Why `msm` and not `multistream-manager`?** You will type it before every
> stream. Three letters is kinder than twenty.

---

## 📦 Install

**One-liner** — downloads the latest release binary, verifies its checksum, and installs it to `~/.local/bin` (it prints the line to add to your shell config if that directory is not already on your `PATH`):

```bash
curl -fsSL https://raw.githubusercontent.com/worxbend/multistream-manager/main/install.sh | sh
```

**From source** — needs [Rust](https://rustup.rs) 1.88 or newer:

```bash
git clone https://github.com/worxbend/multistream-manager
cd multistream-manager
cargo install --path .
```

Either way you end up with a binary called `msm`. Check it:

```bash
msm --help
```

**Container** — no toolchain needed; the image compiles from source and ships one
binary on a bare Debian base (about 140MB):

```bash
docker build -t msm .
docker run -it -v msm-config:/home/msm/.config/msm msm
```

Authorising an account inside a container is the awkward part, because the OAuth
redirect goes to localhost and that means the *container*. The simplest answer is
to run `msm login all` on your own machine first and mount the config directory
it produced; the `Dockerfile` documents the alternative.

**Snap** — strictly confined, asking only for network, browser and home access:

```bash
snapcraft
sudo snap install --dangerous ./msm_*.snap
```

> [!NOTE]
> Prebuilt release binaries are **Linux x86_64 and aarch64 only**. The code
> itself is portable and the test suite runs on Linux, macOS and Windows in CI,
> but on those platforms you build from source with `cargo install --path .`.

---

## 🚀 Quick start

Both Twitch and Google make you register your own "application" before their
APIs will talk to you. That part is tedious and you only do it once — the
click-by-click walkthrough, including the Google settings that are easy to miss,
is in **[docs/getting-started.md](docs/getting-started.md)**.

```bash
msm               # open the interface — it walks you through the rest
```

On a fresh install the interface opens on a **Set up API access** form: paste
the client id and client secret each developer console gave you (secrets are
shown as dots while you type, because this window is often on screen while you
stream) and press <kbd>Enter</kbd>. It saves them to `config.toml` for you.

Next comes **Authorise your accounts** — tick Twitch, YouTube or both and press
<kbd>Enter</kbd>. Your browser opens for each in turn; approve the access and
come back. The main view then opens on its own, showing whether each channel is
live, how many people are watching, and the stream info that would be applied.
Press <kbd>e</kbd> to edit that info, <kbd>Ctrl</kbd>+<kbd>G</kbd> to apply it,
then "Start Streaming" in OBS.

The command line can still do all of it, if you prefer — and `msm setup` exists
for the times a terminal interface is not available at all, such as a headless
box over ssh:

```bash
msm setup twitch  # prompts for the client id and secret and saves them
msm login all     # authorise Twitch and Google in your browser
msm status        # confirm both are logged in
```

If something is not working, start here:

```bash
msm doctor        # checks config, credentials, logins, clipboard, terminal
```

It reports each check as `[ ok ]`, `[warn]` or `[FAIL]`, and every warning says
what to do about it rather than only what is wrong.

Prefer to stay on the command line? The `[preset]` section of the config file
holds the same fields the form does, so you can skip the interface entirely:

```bash
msm go            # shows a summary and asks for confirmation
msm go --yes      # no prompt
msm go --json     # machine-readable result on stdout, never the stream key
```

Keep one file per kind of stream and choose between them:

```bash
msm --config ~/streams/coding.toml go
msm --config ~/streams/gaming.toml go
```

<details>
<summary><b>📋 The whole command set</b></summary>

| Command | What it does |
|---|---|
| `msm` / `msm tui` | Open the terminal interface (the default) |
| `msm login <twitch\|youtube\|all>` | Authorise a platform in your browser |
| `msm logout <twitch\|youtube\|all>` | Forget a saved login |
| `msm status` | Show which platforms are logged in |
| `msm go [--platforms <LIST>] [-y] [--json]` | Apply the preset without the interface |
| `msm key <twitch\|youtube>` | Print one stream key — a separate command so a key is never printed by accident (inside the interface, <kbd>y</kbd>/<kbd>Y</kbd> copy it to the clipboard instead) |
| `msm categories <QUERY>` | Search Twitch's category list |
| `msm streams [--show-keys]` | List the stream objects on your YouTube channel |
| `msm cleanup [-y]` | List (and with `-y`, delete) broadcasts that never went live |
| `msm doctor` | Check the setup and say what would stop a stream |
| `msm setup <twitch\|youtube>` | Fill in one platform's API credentials from the command line |
| `msm profile list\|show\|set` | Look at and change the colour theme |
| `msm init` | Write a commented starter config file |
| `msm paths` | Show where config, tokens and logs live |

`-c, --config <FILE>` works on every subcommand. Full flag-by-flag detail,
including the shape of the `--json` document, is in
[docs/commands.md](docs/commands.md).

</details>

---

### 🔒 Stream keys are copy-only

The interface never draws a stream key, not even behind a "reveal" key. On the
dashboard (and on the combined tab's stream-info half) <kbd>y</kbd> copies your
Twitch key and <kbd>Y</kbd> your YouTube key straight to the system clipboard —
the value goes from the API to the clipboard inside the background task, so
nothing that could end up on screen, in a recording, or in `msm.log` ever holds
it. Copying uses `wl-copy`/`xclip`/`xsel`/`pbcopy`/`clip` when one is installed
and falls back to the OSC 52 terminal escape sequence, which is what makes it
work over SSH.

---

---

## 🎨 Making it yours

Press <kbd>Ctrl</kbd>+<kbd>P</kbd> if you remember nothing else. That opens the
**command palette**: every action in the program, filtered as you type, each one
showing the key that runs it. Using it teaches you the key you could have
pressed, so over time you stop needing it.

### Themes

<kbd>Ctrl</kbd>+<kbd>T</kbd> opens the theme picker. There are **57 built-in
palettes** — Nord, Dracula, Gruvbox, Solarized, Tokyo Night, all four
Catppuccin flavours, and a good many more — and moving the selection applies one
*immediately*, so you judge a theme by looking at the interface you actually use
rather than at a row of swatches. <kbd>Enter</kbd> keeps it, <kbd>Esc</kbd> puts
back what was there.

Every built-in palette is checked, by a test rather than by eye, to keep its body
text at or above the 4.5:1 contrast ratio the WCAG accessibility guidelines set
for readable text. No theme ships that cannot be read.

From the command line:

```bash
msm profile list                            # every theme name, active one marked
msm profile show nord                       # its nine colours
msm profile set gruvbox                     # switch
msm profile set custom --accent "#ff0055"   # write your own, one colour at a time
```

A theme is nine named roles — background, surface, foreground, muted, border,
accent, warning, error, success — so `[appearance.custom_theme]` in `config.toml`
lets you override one and inherit the other eight.

### Motion

<kbd>Alt</kbd>+<kbd>A</kbd> cycles how much the interface animates: **fast**,
**reduced**, or **off**. `reduced` is not the same animation played slowly —
that would make it last *longer*, which is the opposite of what asking for less
motion means. It takes bigger steps at a slower rate, so effects finish in about
the same time having drawn a third of the frames. `off` draws every animated
element at its finished frame: nothing is hidden, it simply does not move.

The start-up splash obeys the same setting, and any key skips it. `splash =
false` in `[appearance]` turns it off for good.

### Notifications

Notifications follow vim's model. Something happens, a short message appears in
the corner, it does not interrupt you and it goes away on its own — but it is not
lost either. <kbd>Alt</kbd>+<kbd>M</kbd> opens the **message history**, vim's
`:messages`, with everything the session has raised and when.

This is the bit that matters while you are streaming: the activity log lives at
the bottom of the Stream Info tab, so while you are reading chat it is not on
screen at all. A token that failed to refresh or a chat connection that dropped
now reaches you wherever you are looking. Routine progress deliberately does not
pop up — a notification for every ordinary step teaches you to ignore them, and
then you miss the one that mattered.

### The mouse

Clicking a tab switches to it, clicking a chat pane gives it the keyboard, and
the wheel scrolls whatever is scrollable. Everything the mouse does has a key
that does the same — it is there for the first ten minutes, before any of the
keys are in your fingers. `mouse = false` in `[appearance]` turns reporting off
and gives your terminal its own text selection back.

### Everything else in `[appearance]`

| Setting | Default | What it does |
|---|---|---|
| `theme` | `claude` | Which of the 57 palettes, or `custom` |
| `animations` | `fast` | `fast`, `reduced` or `off` |
| `splash` | `true` | The animated start-up screen |
| `mouse` | `true` | React to clicks and the wheel |
| `telemetry` | `false` | Show cpu, memory and frame rate in the tab bar (<kbd>Alt</kbd>+<kbd>T</kbd>) |
| `toasts` | `true` | Pop-up notifications (the log records everything either way) |
| `toast_seconds` | `5` | How long one stays up; warnings get double, errors triple |
| `terminal_background` | `false` | Repaint the terminal window's own background to match the theme |

---

## 🎛️ How it fits OBS and Aitum

Your OBS setup does not change. Configure it once, exactly as you do now:

- **OBS → Settings → Stream** points at Twitch with your Twitch stream key.
- **Aitum multistream** holds your YouTube RTMP URL and stream key as a second
  destination.

Your session then becomes:

```
msm  →  fill the form  →  Ctrl+G  →  OBS "Start Streaming"
```

### 🔑 Why your YouTube stream key never changes

This is the part that makes the whole thing work, so it is worth spelling out.

YouTube's API has two separate objects: a **broadcast** (the event, with a title
and a watch page) and a **stream** (the RTMP pipe that carries the video, with
the key). Creating a new stream object mints a **brand new key**. A tool that
created one per session would hand you a different key every time, and you would
have to paste it into Aitum before every stream — the exact chore this tool
exists to remove.

So `msm` **reuses** a stream that is already on your channel and binds the new
broadcast to that one. Your key stays the same, and the dashboard tells you which
branch it took:

> *Reused your existing stream key "Default stream key" — OBS and Aitum need no changes.*

If the bind fails, or you have no reusable stream yet, it creates one and says so
plainly, so you know to update Aitum that one time. If your channel has several
keys, run `msm streams` to see them and pin the one you want with `stream_id` in
the config.

More on the OBS and Aitum side, including the Aitum fields to fill in:
[docs/obs-and-aitum.md](docs/obs-and-aitum.md).

---

## 📚 Documentation

The detail lives in `docs/`, and is also published at
**[worxbend.github.io/multistream-manager](https://worxbend.github.io/multistream-manager/)**.

| Page | What is in it |
|---|---|
| 📖 [Overview](docs/README.md) | Where to start, and what each page covers |
| 🧭 [Getting started](docs/getting-started.md) | Twitch and Google credentials, first login, first stream |
| ⚙️ [Configuration](docs/configuration.md) | Every setting in `config.toml`, and using it as a preset |
| ⌨️ [Commands](docs/commands.md) | Every subcommand and flag, with output examples |
| 🎥 [OBS and Aitum](docs/obs-and-aitum.md) | Wiring the two destinations up once and leaving them alone |
| 🧩 [How it works](docs/how-it-works.md) | The API calls, the platform differences, the design decisions |
| 🩺 [Troubleshooting](docs/troubleshooting.md) | Error messages, what they mean, what to do about them |

---

## 🛠️ Development

```bash
cargo test               # 192 tests, no network access required
cargo clippy --all-targets
cargo fmt
```

Every test is offline. API responses are checked by parsing recorded JSON
shapes, and the terminal interface is checked by driving real key events through
`App` and rendering frames into an in-memory terminal — so there is no mocking
framework and nothing to set up before running them.

**The layout, roughly:**

| File | Responsibility |
|---|---|
| `model.rs` | The domain types. One `StreamPlan` describes the broadcast; platform limits and validation live here |
| `backend.rs` | The `Backend` trait every platform implements |
| `twitch.rs` / `youtube.rs` | The two API clients. All *platform* API calls live here — the UI never talks to Twitch or YouTube directly |
| `engine.rs` | Fans one plan out to every platform at once, collecting a result per platform |
| `auth/` | The OAuth flow (loopback redirect with PKCE), token storage and silent renewal |
| `ui/app.rs` | All interface state and keyboard handling — pure functions, no I/O, heavily tested |
| `ui/worker.rs` | The background task that does the slow API work, so the interface never freezes |
| `ui/draw.rs` | Rendering. Reads state, never changes it |

Adding a third platform means writing one file that implements `Backend` and
adding a variant to `Platform`; the interface does not need to change.

Contributions are welcome. Please run `cargo fmt`, `cargo clippy --all-targets`
and `cargo test` before opening a pull request — CI runs all three, plus a build
against Rust 1.88 to hold the minimum supported version.

---

## 📄 Licence

MIT. See [LICENSE](LICENSE).
