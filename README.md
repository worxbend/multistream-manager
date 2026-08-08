# multistream-manager

**Set up a Twitch and YouTube stream from one terminal window, then start OBS.**

If you stream to both platforms at once — for example through OBS with the
[Aitum multistream plugin](https://aitum.tv/vertical) — you know the routine
before every session:

1. Open YouTube Studio, create a live broadcast, type the title, paste the
   description, pick a category, set the visibility, tick "not made for kids".
2. Open the Twitch dashboard, type the *same* title, pick a category from a
   search box, retype the *same* tags, set the language.
3. Copy a stream key somewhere.
4. Finally, press "Start Streaming" in OBS.

`msm` collapses steps 1 to 3 into one form. You type the title, description,
tags, category and language **once**, press <kbd>Ctrl</kbd>+<kbd>G</kbd>, and
both platforms are configured in parallel. Then you press "Start Streaming" in
OBS exactly as you always did — this tool never touches OBS itself.

Once you are live it shows viewer counts, follower and subscriber totals, likes
and uptime for both platforms side by side, so you do not need either website
open at all.

```
╭──────────────────────────────────────────────────────────────────────────╮
│ multistream-manager │  Twitch   YouTube                                  │
╰──────────────────────────────────────────────────────────────────────────╯
 Twitch and YouTube ready. Press Start Streaming in OBS now.
╭─ Twitch ─────────────────────────╮╭─ YouTube ────────────────────────────╮
│ READY — you can start streaming  ││ READY — you can start streaming      │
│                                  ││                                      │
│ Watch    https://twitch.tv/you   ││ Watch    https://youtube.com/watch?…  │
│ Ingest   rtmp://live.twitch.tv…  ││ Ingest   rtmp://a.rtmp.youtube.com…  │
│ Key      ••••••••••••••••        ││ Key      ••••••••••••••••            │
│                                  ││                                      │
│ ── Live ──                       ││ • Reused your existing stream key    │
│ Status   live                    ││ ── Live ──                           │
│ Viewers  142                     ││ Status   live                        │
│ Uptime   1h 23m                  ││ Viewers  38                          │
│ Followers 12.4K                  ││ Likes    17                          │
╰──────────────────────────────────╯╰──────────────────────────────────────╯
```

---

## Table of contents

- [What it does](#what-it-does)
- [Installing](#installing)
- [First-time setup](#first-time-setup)
  - [Step 1: Twitch credentials](#step-1-twitch-credentials)
  - [Step 2: YouTube credentials](#step-2-youtube-credentials)
  - [Step 3: Log in](#step-3-log-in)
- [Using it](#using-it)
- [How it works with OBS and Aitum](#how-it-works-with-obs-and-aitum)
- [Configuration file](#configuration-file)
- [Command reference](#command-reference)
- [How the platforms differ](#how-the-platforms-differ)
- [Troubleshooting](#troubleshooting)
- [Security notes](#security-notes)
- [Development](#development)

---

## What it does

| | Twitch | YouTube |
|---|---|---|
| Title | ✅ | ✅ (also gets your tags as `#hashtags`) |
| Description | — *(Twitch has no description field)* | ✅ |
| Tags | ✅ up to 10 | ✅ |
| Category | ✅ searched live against Twitch's list | ✅ picked from YouTube's list |
| Language | ✅ | ✅ |
| Visibility | — | ✅ public / unlisted / private |
| Creates the broadcast | not needed | ✅ |
| Stream key shown | ✅ | ✅ |
| Live viewer count | ✅ | ✅ |
| Followers / subscribers | ✅ | ✅ |
| Likes | — | ✅ |

**It does not control OBS.** It gets the platforms ready; you still press "Start
Streaming" yourself. That is deliberate — you stay in control of when you
actually go live.

---

## Installing

You need [Rust](https://rustup.rs) 1.88 or newer.

```bash
git clone https://github.com/w0rxbend/multistream-manager
cd multistream-manager
cargo install --path .
```

That puts a binary called `msm` in `~/.cargo/bin`. Check it works:

```bash
msm --help
```

> **Why `msm` and not `multistream-manager`?** You will type it before every
> stream. Three letters is kinder than twenty.

---

## First-time setup

This part is genuinely tedious, but you only do it once. Both Twitch and Google
require you to register your own "application" before their APIs will talk to
you — there is no way around it, and it is the same process every tool of this
kind has to ask you for.

Start by writing a config file:

```bash
msm init
```

It will tell you where it put the file. Open that file in an editor; it is full
of comments explaining each setting.

### Step 1: Twitch credentials

1. Go to <https://dev.twitch.tv/console/apps> and sign in.
2. Click **Register Your Application**.
3. Fill in:
   - **Name**: anything, e.g. `my-multistream-manager`. It must be unique across
     all of Twitch, so add a suffix if it complains.
   - **OAuth Redirect URLs**: `http://localhost:8017/callback`
     *(This must match exactly — same scheme, same port, same path.)*
   - **Category**: Application Integration
   - **Client Type**: Confidential
4. Click **Create**, then **Manage** on your new app.
5. Copy the **Client ID** into `client_id` under `[twitch]` in your config file.
6. Click **New Secret**, confirm, and copy the value into `client_secret`.

> The secret is shown **once**. If you lose it, generate a new one.

### Step 2: YouTube credentials

1. Go to <https://console.cloud.google.com/> and sign in with the Google account
   that owns your YouTube channel.
2. Create a project (top bar → project dropdown → **New Project**). Name it
   anything.
3. Go to **APIs & Services → Library**, search for **YouTube Data API v3**, open
   it and click **Enable**.
4. Go to **APIs & Services → OAuth consent screen**:
   - User type: **External**, then **Create**.
   - Fill in the app name, your email for both support fields, and **Save**.
   - Under **Test users**, click **Add users** and add **your own Google
     account**. *This step is easy to miss and Google will refuse the login
     without it while your app is in Testing mode.*
5. Go to **APIs & Services → Credentials → Create Credentials → OAuth client ID**:
   - Application type: **Desktop app**
   - Name: anything
   - Click **Create**.
6. Copy the **Client ID** and **Client secret** into `[youtube]` in your config.

You will also need live streaming enabled on the channel itself, at
<https://youtube.com/features>. If you have never streamed from this channel,
note that YouTube imposes a **24-hour waiting period** the first time you enable
it.

### Step 3: Log in

```bash
msm login all
```

This opens your browser twice — once for Twitch, once for Google. You log in on
their sites; the tool never sees your password. When you approve, the browser
redirects to `localhost:8017`, where `msm` is listening, and it picks up the
authorisation from there.

> `localhost` resolves to `127.0.0.1` on some systems and to `::1` on others, so
> the callback listener binds both. You do not need to care which your machine
> uses.

Google will show a scary "Google hasn't verified this app" warning. That is
expected: the "app" is the one *you* created five minutes ago, and verification
only matters for apps distributed to strangers. Click **Advanced → Go to … (unsafe)**.

Check it worked:

```bash
msm status
```

---

## Using it

```bash
msm
```

**Screen 1 — pick your platforms.** <kbd>↑</kbd>/<kbd>↓</kbd> to move,
<kbd>Space</kbd> to tick, <kbd>a</kbd> to tick everything, <kbd>Enter</kbd> to
connect.

**Screen 2 — the form.** <kbd>Tab</kbd> moves between fields.

| Key | Does |
|---|---|
| <kbd>Tab</kbd> / <kbd>↑</kbd> <kbd>↓</kbd> | Move between fields |
| <kbd>Enter</kbd> | Open the autocomplete list on a category or language field |
| <kbd>Space</kbd> | Flip a yes/no field |
| <kbd>←</kbd> <kbd>→</kbd> | Change a selector (like Privacy) |
| <kbd>Ctrl</kbd>+<kbd>W</kbd> | Delete the previous word |
| <kbd>Ctrl</kbd>+<kbd>U</kbd> | Clear the field |
| <kbd>Ctrl</kbd>+<kbd>S</kbd> | Save what you typed as your new defaults |
| <kbd>Ctrl</kbd>+<kbd>G</kbd> | **Go live** |
| <kbd>Esc</kbd> | Back a screen (or close the autocomplete) |

The **Twitch category** field searches Twitch's real category list as you type.
You must pick a match with <kbd>Enter</kbd> — Twitch's API only accepts a numeric
category id, not a name, so a typed-but-unselected name cannot be submitted. The
form tells you when this is the case, and the footer hint turns green only when
everything is genuinely ready to send.

The **Language** field searches by name in English or in the language's own
name, so typing `polski`, `polish` or `pl` all find Polish.

**Screen 3 — the dashboard.** URLs, stream keys and live statistics.

| Key | Does |
|---|---|
| <kbd>r</kbd> | Refresh statistics now |
| <kbd>k</kbd> | Show/hide the stream key |
| <kbd>e</kbd> | Go back to the form and submit again |
| <kbd>q</kbd> | Quit |

Stream keys are masked by default, because this window is often on screen while
you are streaming.

---

## How it works with OBS and Aitum

Your OBS setup does not change. Configure it once, exactly as you do now:

- **OBS → Settings → Stream** points at Twitch with your Twitch stream key.
- **Aitum multistream** holds your YouTube RTMP URL and stream key as a second
  destination.

`msm` deliberately **reuses your existing YouTube stream key** rather than
creating a new one for each broadcast.

This matters more than it sounds. YouTube's API has two separate objects: a
*broadcast* (the event, with a title and a watch page) and a *stream* (the RTMP
pipe with the key). Creating a new stream object mints a **brand new key** — so a
tool that naively created one per session would hand you a different key every
time, and you would have to paste it into Aitum before every stream. That is the
exact chore this tool exists to remove.

Instead it finds a reusable stream already on your channel and binds the new
broadcast to that. Your key stays the same forever, and the dashboard tells you
which branch it took:

> *Reused your existing stream key "Default stream key" — OBS and Aitum need no changes.*

If you have no reusable key yet, it creates one and says so clearly, so you know
to paste it into Aitum that one time.

If you have several keys and want a specific one, set `stream_id` in the config.
The behaviour is controlled by `reuse_stream = true`, which you should leave on.

**Your normal workflow becomes:**

```
msm  →  fill the form  →  Ctrl+G  →  OBS "Start Streaming"
```

With `youtube_auto_start = true` (the default), YouTube flips the broadcast live
by itself the moment it sees the feed from OBS. You never touch YouTube Studio.

---

## Configuration file

Run `msm paths` to find it. Everything in the form can also be set here, which
is the "just edit a file" workflow:

```toml
[preset]
title = "Building a Rust TUI from scratch"
description = """
Working on multistream-manager today.

Source: https://github.com/w0rxbend/multistream-manager
"""
tags = ["rust", "programming", "livecoding"]
twitch_category = "Software and Game Development"
youtube_category_id = "28"
language = "en"
privacy = "public"
platforms = ["twitch", "youtube"]
```

Then skip the interface entirely:

```bash
msm go            # shows a summary and asks for confirmation
msm go --yes      # no prompt, for scripts
```

Keep several presets and choose between them:

```bash
msm --config ~/streams/coding.toml go
msm --config ~/streams/gaming.toml go
```

Pressing <kbd>Ctrl</kbd>+<kbd>S</kbd> in the form writes whatever you typed back
into `[preset]`, so the two ways of working feed each other.

---

## Command reference

| Command | What it does |
|---|---|
| `msm` | Open the interface (the normal way to use it) |
| `msm login <twitch\|youtube\|all>` | Authorise a platform in your browser |
| `msm logout <twitch\|youtube\|all>` | Forget a saved login |
| `msm status` | Show which platforms are logged in |
| `msm go [--platforms …] [--yes]` | Apply the config preset without the interface |
| `msm key twitch` | Print your Twitch stream key |
| `msm categories <search>` | Search Twitch's category list |
| `msm init` | Write a starter config file |
| `msm paths` | Show where config, tokens and logs live |

---

## How the platforms differ

Worth knowing, because it explains some of the tool's behaviour:

**Twitch is stateless.** Your channel always exists. "Going live" is just
pointing OBS at it. So the Twitch side is a single API call that updates the
channel's title, category, language and tags. There is nothing to create and
nothing to clean up. Twitch has no description field at all, so the description
you type is YouTube-only.

**YouTube is event-based.** Each stream is a distinct broadcast object with its
own watch URL, and it must be created ahead of time. Going live there takes four
calls: find a stream key, create the broadcast, bind them together, then update
the resulting video's tags and category — because, awkwardly, the broadcast
creation endpoint has no fields for tags or category.

Some consequences:

- **Titles**: Twitch allows 140 characters, YouTube only 100. The counter in the
  form shows the tighter of the two limits for whatever you have selected. If a
  title fits Twitch but not YouTube, it is shortened for YouTube only rather than
  failing the whole submission.
- **Tags**: Twitch allows at most 10, each 25 characters, with no spaces or
  punctuation. `live coding` is sent to Twitch as `livecoding` and the form warns
  you when it is about to do that. YouTube accepts them as typed.
- **Hashtags**: your tags are also appended to the **YouTube title** as
  `#hashtags`, as many as fit inside the 100-character limit. YouTube only links
  the first three, so there is no point forcing more.
- **Resubmitting**: pressing <kbd>e</kbd> on the dashboard and going live again
  updates the Twitch channel in place, but creates a **new** YouTube broadcast.
  That is how YouTube works; the old one is left as an unstarted broadcast you
  can delete in Studio.

---

## Troubleshooting

**`could not listen on ... :8017`**
Something else has that port. Change `oauth_port` in the config *and* update the
redirect URI in both developer consoles to match.

**Google says "Access blocked: … has not completed the Google verification process"**
You have not added your own account under **OAuth consent screen → Test users**.
See [step 2](#step-2-youtube-credentials).

**`invalid_grant` when starting up**
The saved refresh token was revoked or expired. Run `msm login <platform>` again.

**Twitch: "your saved token does not include the channel:manage:broadcast permission"**
Your login predates a permission the tool now needs. Run `msm login twitch`.

**YouTube: `quotaExceeded`**
The YouTube Data API has a daily quota (10,000 units by default) and every
statistics refresh spends a little of it. Raise `poll_interval_secs` in the
config. The quota resets at midnight Pacific time.

**YouTube: `liveStreamingNotEnabled`**
Enable live streaming at <https://youtube.com/features>. First-time activation
takes 24 hours.

**My YouTube stream key changed**
It should not. `msm` reuses the reusable stream already on your channel. If it
could not — because the key it found belongs to a single past broadcast and
cannot be bound again — it creates a new one and says so explicitly in the
YouTube panel. Pin the key you want with `stream_id` in the config to be certain.

**One platform worked and the other failed**
That is intentional. Nothing is rolled back, so the platform that succeeded is
genuinely ready and you can stream to it right now. The failure and its reason
are shown in its own panel.

**Everything looks broken and I want to see why**
There is a log file — `msm paths` will tell you where. Watch it live:

```bash
MSM_LOG=debug msm      # in one terminal
tail -f "$(msm paths | awk '/^Log:/{print $2}')"   # in another
```

---

## Security notes

- Your **client secrets** and **OAuth tokens** are stored in your user config
  directory with `0600` permissions — readable only by you.
- The tool never sees your Twitch or Google password. Login happens on their
  sites; it only receives a token afterwards.
- The OAuth flow uses **PKCE** and a random `state` value, so an authorisation
  code intercepted in transit cannot be redeemed by anyone else.
- **Stream keys are masked** in the interface until you press <kbd>k</kbd>, and
  are never written to the log file, never saved to disk, and never printed by
  `msm go`. `msm key` exists as a separate, deliberate command for when you
  actually need one.

---

## Development

```bash
cargo test          # 147 tests, no network access required
cargo clippy --all-targets
cargo fmt
```

The tests are all offline. API responses are tested by parsing recorded JSON
shapes, and the whole terminal interface is tested by driving real key events
through `App` and rendering frames into an in-memory terminal — so there is no
mocking framework and nothing to configure before running them.

**The layout, roughly:**

| File | Responsibility |
|---|---|
| `model.rs` | The domain types. One `StreamPlan` describes the broadcast; platform limits and validation live here. |
| `backend.rs` | The `Backend` trait every platform implements. |
| `twitch.rs` / `youtube.rs` | The two API clients. Nothing else knows any HTTP. |
| `engine.rs` | Fans one plan out to every platform concurrently, collecting per-platform results. |
| `auth/` | The OAuth flow, token storage and silent renewal. |
| `ui/app.rs` | All UI state and keyboard handling — pure functions, no I/O, heavily tested. |
| `ui/worker.rs` | The background task that does the slow API work, so the interface never freezes. |
| `ui/draw.rs` | Rendering. Reads state, never mutates it. |

Adding a third platform means writing one file implementing `Backend` and adding
a variant to `Platform`; the interface does not need to change.

---

## Licence

MIT. See [LICENSE](LICENSE).
