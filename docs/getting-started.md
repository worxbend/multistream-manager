# Getting started

This page takes you from an empty machine to a stream that is live on both
platforms. It is long because the one-off setup is long: both Twitch and Google
require you to register your own "application" with them before their APIs will
talk to you at all. There is no way around that, and every tool of this kind has
to ask you for the same thing.

You do it once. After that, going live is one command and one keystroke.

**Contents**

1. [Install](#1-install)
2. [Write a config file](#2-write-a-config-file)
3. [Twitch credentials](#3-twitch-credentials)
4. [Google and YouTube credentials](#4-google-and-youtube-credentials)
5. [Enable live streaming on the channel](#5-enable-live-streaming-on-the-channel)
6. [Log in](#6-log-in)
7. [Your first stream](#7-your-first-stream)

---

## The two traps

Read these before you start. They are the two things that stop people, and
neither produces an error message that explains itself.

### Trap 1: Google refuses the login unless you are on the Test users list

> A newly created Google OAuth client starts in **Testing** mode. In that mode
> Google only allows accounts you have explicitly listed as test users, and
> *your own account is not on the list by default*. If you skip this, the login
> fails with a message about the app not having completed verification, which
> sounds like a problem with the app rather than with a missing list entry.
>
> **Fix**: Google Cloud console → **APIs & Services** → **OAuth consent screen**
> → **Test users** → **Add users** → add the Google account that owns your
> YouTube channel.
>
> Full steps in [step 4](#4-google-and-youtube-credentials).

### Trap 2: a channel that has never streamed cannot stream for 24 hours

> YouTube requires live streaming to be switched on per channel, and the first
> time you switch it on there is a **24-hour waiting period** before it takes
> effect. Nothing you do in `msm` shortens it. Until the wait is over, every
> attempt to create a broadcast fails with `liveStreamingNotEnabled`.
>
> **Fix**: go to <https://youtube.com/features>, request live streaming, and
> come back tomorrow. Do this on day one, before anything else, so the clock is
> already running while you work through the rest of this page.
>
> Full steps in [step 5](#5-enable-live-streaming-on-the-channel).

---

## 1. Install

You need [Rust](https://rustup.rs) 1.88 or newer — that is the crate's minimum
supported Rust version, and older toolchains will fail to compile it.

```bash
git clone https://github.com/worxbend/multistream-manager
cd multistream-manager
cargo install --path .
```

That places a binary called `msm` in `~/.cargo/bin`. Check it is on your path:

```bash
msm --version
```

If the shell cannot find it, add `~/.cargo/bin` to your `PATH`.

---

## 2. Write a config file

```bash
msm init
```

This writes a commented starter `config.toml` and prints where it put it. On
Linux that is `~/.config/multistream-manager/config.toml`; on macOS it is under
`~/Library/Application Support/`, and on Windows under `%APPDATA%`. `msm paths`
prints the location at any time, along with the token and log file paths.

The file is written with owner-only permissions (`0600` on Unix), because it is
about to hold two client secrets.

If the file already exists, `msm init` leaves it alone and says so rather than
overwriting your credentials.

Open it in an editor and keep it open — the next two steps fill it in. Every key
in the file is described in [Configuration](configuration.md).

---

## 3. Twitch credentials

Twitch calls the thing you are registering an **application**. It exists so that
Twitch can attribute API calls to something and revoke them independently of
your account password.

1. Go to <https://dev.twitch.tv/console/apps> and sign in.
2. Click **Register Your Application**.
3. Fill in the form:
   * **Name** — anything, for example `my-multistream-manager`. Twitch requires
     it to be unique across all of Twitch, so add a suffix if it objects.
   * **OAuth Redirect URLs** — exactly `http://localhost:8017/callback`.
     Scheme, host, port and path all have to match what `msm` sends, character
     for character, or Twitch rejects the login. Port 8017 is the default; if
     you change `oauth_port` in the config you must change this too.
   * **Category** — *Application Integration*.
   * **Client Type** — *Confidential*.
4. Click **Create**, then **Manage** on the application you created.
5. Copy the **Client ID** into `client_id` under `[twitch]` in your config file.
6. Click **New Secret**, confirm, and copy the value into `client_secret`.

The secret is displayed once. If you lose it, generate a new one — old secrets
stop working when you do, so update the config at the same time.

Your `[twitch]` section should now look like this, with real values:

```toml
[twitch]
client_id = "abcdefghijklmnopqrstuvwxyz1234"
client_secret = "0123456789abcdefghijklmnopqrst"
```

### Why a secret at all, for a desktop program?

A desktop program cannot keep a secret from the person running it, so in the
abstract a "confidential" client is the wrong shape here. Twitch nonetheless
requires a client secret when redeeming an authorisation code for the
permissions this tool needs; there is no secret-less flow available. The secret
is stored locally with owner-only permissions and is only ever sent to Twitch's
own token endpoint.

---

## 4. Google and YouTube credentials

Google's console is the more involved of the two. Work through it in order.

1. Go to <https://console.cloud.google.com/> and sign in **with the Google
   account that owns the YouTube channel you stream from**. If you have several
   accounts, this is the mistake to avoid: credentials created under the wrong
   account will authorise the wrong channel later.
2. Create a project: top bar → project dropdown → **New Project**. The name is
   for your own benefit.
3. **APIs & Services** → **Library**. Search for **YouTube Data API v3**, open
   it, and click **Enable**. Nothing works until this is done — the API is off
   by default in every new project.
4. **APIs & Services** → **OAuth consent screen**:
   * User type **External**, then **Create**.
   * Fill in the application name and your own email address in both of the
     support-contact fields. Save.
   * Find **Test users** and click **Add users**. **Add your own Google
     account.** This is [trap 1](#trap-1-google-refuses-the-login-unless-you-are-on-the-test-users-list):
     while the client is in Testing mode, Google refuses to authorise any
     account that is not on this list.
5. **APIs & Services** → **Credentials** → **Create Credentials** → **OAuth
   client ID**:
   * Application type **Desktop app**.
   * Name it anything, then **Create**.
   * Add `http://localhost:8017/callback` as an authorised redirect URI, the
     same value you gave Twitch.
6. Copy the **Client ID** and **Client secret** into `[youtube]` in the config:

```toml
[youtube]
client_id = "000000000000-xxxxxxxxxxxxxxxxxxxxxxxx.apps.googleusercontent.com"
client_secret = "GOCSPX-xxxxxxxxxxxxxxxxxxxx"
reuse_stream = true
stream_id = ""
```

Leave `reuse_stream = true`. It is what keeps your YouTube stream key stable
between broadcasts; [OBS and Aitum](obs-and-aitum.md) explains why that matters
so much.

### What permissions are being requested?

When you log in, the consent screen lists what the application may do. It is
worth knowing what you are agreeing to:

| Platform | Scope | Why it is needed |
|---|---|---|
| Twitch | `channel:manage:broadcast` | Set the channel title, category, language and tags. Without it nothing can be changed. |
| Twitch | `channel:read:stream_key` | Show your stream key in the dashboard and in `msm key twitch`. |
| Twitch | `moderator:read:followers` | The follower total on the statistics panel. |
| Twitch | `channel:read:subscriptions` | The subscriber total on the statistics panel. |
| YouTube | `https://www.googleapis.com/auth/youtube` | Create a broadcast, bind a stream to it, and update the resulting video. YouTube's live-streaming endpoints are not available under any narrower scope, so this single broad one is what has to be asked for. |

---

## 5. Enable live streaming on the channel

Separately from the API credentials, the **channel itself** has to be allowed to
stream.

1. Go to <https://youtube.com/features>.
2. Find live streaming and request access. You will need a verified phone
   number on the account.
3. If this channel has never streamed before, YouTube starts a **24-hour
   waiting period**. This is [trap 2](#trap-2-a-channel-that-has-never-streamed-cannot-stream-for-24-hours).

There is nothing to configure in `msm` for this, and nothing that shortens the
wait. Attempts before the wait is over fail with `liveStreamingNotEnabled` —
see [Troubleshooting](troubleshooting.md#youtube-livestreamingnotenabled).

---

## 6. Log in

```bash
msm login all
```

This opens your browser twice, once per platform. What happens:

1. `msm` starts a small web server on `localhost:8017` and opens the platform's
   authorisation page in your browser. If no browser opens, the URL is printed
   in the terminal for you to paste in yourself.
2. You sign in **on Twitch's or Google's own site**. `msm` never sees your
   password.
3. When you approve, the platform redirects your browser back to
   `http://localhost:8017/callback` with a short-lived authorisation code.
4. `msm` receives that request, exchanges the code for tokens, and saves them to
   `tokens.json` beside the config file with owner-only permissions.

The listener binds both IPv4 and IPv6 loopback, because `localhost` resolves to
`127.0.0.1` on some machines and to `::1` on others and the platform decides
which one your browser uses, not you.

The whole exchange uses PKCE and a random `state` value, so an authorisation
code seen by anything else on the machine cannot be redeemed by it.

### The "Google hasn't verified this app" warning

Google shows a full-page warning during the login. This is expected. The "app"
in question is the OAuth client *you* created a few minutes ago; verification is
a review process for applications distributed to strangers, and it is not
relevant to a client only you will ever use.

Click **Advanced**, then **Go to … (unsafe)** to continue.

If instead of a warning you get a hard refusal about verification, you are
looking at [trap 1](#trap-1-google-refuses-the-login-unless-you-are-on-the-test-users-list):
your account is not on the Test users list.

### Check it worked

```bash
msm status
```

This prints, per platform, whether a login is saved and how long the current
access token is valid for, whether credentials are configured, and where the
config file lives. Access tokens are short-lived — Google's last about an hour —
but a refresh token is saved alongside them and renewal happens silently, so a
session lasting all evening keeps working.

You can log in to one platform at a time if you prefer:

```bash
msm login twitch
msm login youtube
```

---

## 7. Your first stream

### Before you go live

Set OBS up as you normally would: OBS's own **Settings → Stream** pointing at
Twitch, and the Aitum multistream plugin holding YouTube as a second
destination. `msm` never touches OBS and never changes those settings. See
[OBS and Aitum](obs-and-aitum.md) for the details, including where the YouTube
stream key comes from the very first time.

If it is your first YouTube broadcast, run this to see which stream keys exist
on the channel:

```bash
msm streams
```

### Using the interface

```bash
msm
```

**Screen 1 — platforms.** <kbd>↑</kbd>/<kbd>↓</kbd> to move, <kbd>Space</kbd> to
tick, <kbd>a</kbd> to tick everything, <kbd>Enter</kbd> to connect. Connecting
verifies both logins and prints which account each resolved to, so streaming to
the wrong channel is caught before anything is changed.

**Screen 2 — the form.** One set of fields for both platforms.

| Key | Does |
|---|---|
| <kbd>Tab</kbd> / <kbd>↑</kbd> <kbd>↓</kbd> | Move between fields |
| <kbd>Enter</kbd> | Open the search list on a category or language field |
| <kbd>Space</kbd> | Flip a yes/no field |
| <kbd>←</kbd> <kbd>→</kbd> | Change a selector such as Privacy |
| <kbd>Ctrl</kbd>+<kbd>W</kbd> | Delete the previous word |
| <kbd>Ctrl</kbd>+<kbd>U</kbd> | Clear the field |
| <kbd>Ctrl</kbd>+<kbd>S</kbd> | Save what you have typed as your new defaults |
| <kbd>Ctrl</kbd>+<kbd>G</kbd> | Go live |
| <kbd>Esc</kbd> | Close the search list, or go back a screen |

Two fields deserve a warning:

* **Twitch category** — you have to pick a match from the list with
  <kbd>Enter</kbd>, not merely type a name. Twitch's update endpoint accepts a
  numeric category id and nothing else, so a typed-but-unselected name cannot be
  sent. The form shows this as a blocking problem until you select something.
* **Language** — a two-letter ISO 639-1 code. You can type the language's name
  in English or in its own name; `polish`, `polski` and `pl` all find Polish.

The submit hint at the bottom of the form turns green only when the plan is
genuinely sendable, so you do not discover a missing category after the API
round trip.

**Screen 3 — the dashboard.** Watch and manage URLs, ingest URL, the masked
stream key, and live statistics for both platforms side by side.

| Key | Does |
|---|---|
| <kbd>r</kbd> | Refresh statistics now |
| <kbd>o</kbd> | Open the watch page in your browser |
| <kbd>k</kbd> | Show or hide the stream key |
| <kbd>e</kbd> | Back to the form to change something and submit again |
| <kbd>q</kbd> | Quit |

### Now start OBS

Press **Start Streaming** in OBS. That is the only thing that puts video on the
wire; nothing `msm` does sends a single frame.

With `youtube_auto_start = true` (the default), YouTube flips the broadcast live
by itself as soon as it sees the incoming feed, so you never open YouTube
Studio. With it off, you have to press **Go live** there yourself once OBS is
connected.

### Doing it without the interface

Once the `[preset]` section of your config says what you usually stream, you can
skip the form entirely:

```bash
msm go          # shows a summary and asks for confirmation
msm go --yes    # no prompt
```

That workflow, including keeping one preset file per kind of stream, is covered
in [Configuration](configuration.md#the-preset-workflow).

---

## Where to go next

* [Configuration](configuration.md) — every setting, explained.
* [Commands](commands.md) — the full command reference.
* [OBS and Aitum](obs-and-aitum.md) — how this slots into your existing setup.
* [Troubleshooting](troubleshooting.md) — when a step above did not work.
* [Back to the documentation index](README.md).
