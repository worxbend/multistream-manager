# Getting started

This page takes you from an empty machine to a stream that is live on both
platforms. It is long because the one-off setup is long: both Twitch and Google
require you to register your own "application" with them before their APIs will
talk to you at all. There is no way around that, and every tool of this kind has
to ask you for the same thing.

You do it once. After that, going live is one keystroke.

`msm` has no command line: you run `msm`, a terminal interface opens, and
everything below happens inside it. There is nothing to type at a shell prompt
except the three letters of the program's name.

**Contents**

1. [Install](#1-install)
2. [Open it for the first time](#2-open-it-for-the-first-time)
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

That places a binary called `msm` in `~/.cargo/bin`. If the shell cannot find it
afterwards, add `~/.cargo/bin` to your `PATH`.

---

## 2. Open it for the first time

```bash
msm
```

With nothing configured yet, the interface opens on a screen headed **Set up API
access**: one box for each platform's client id and client secret, and the
redirect URL you are about to register printed above them.

Leave it open on one side of the screen. The next two steps are about getting
those four values out of Twitch's and Google's developer consoles, and you will
come back here to paste each one in.

Two things worth knowing while you are there:

* **Secrets are drawn as dots even while you type them.** This window is
  frequently on screen while somebody streams, and a client secret is a
  credential.
* **One platform is enough.** An empty pair of boxes is skipped rather than
  treated as an error, so you can set Twitch up today and YouTube next week.

<kbd>Tab</kbd> and the arrow keys move between boxes; <kbd>Enter</kbd> saves.
Saving writes `config.toml` for you, with owner-only permissions (`0600` on
Unix) because it now holds two client secrets. On Linux it lands in
`~/.config/multistream-manager/config.toml`; on macOS under `~/Library/
Application Support/`, and on Windows under `%APPDATA%`. The **Files** section of
the Config tab (<kbd>Alt</kbd>+<kbd>5</kbd>) shows the exact paths at any time.

You can also write that file by hand if you would rather — every key in it is
described in [Configuration](configuration.md), and the setup screen picks up
whatever is already there.

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
5. Copy the **Client ID** into the **Twitch client id** box on the setup screen.
6. Click **New Secret**, confirm, and copy the value into **Twitch client
   secret**.

The secret is displayed once. If you lose it, generate a new one — old secrets
stop working when you do, so paste the replacement in at the same time.

What that saves into `config.toml` is this, with real values:

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
6. Copy the **Client ID** and **Client secret** into the two YouTube boxes on the
   setup screen, and press <kbd>Enter</kbd> to save. In the config file that
   becomes:

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
| Twitch | `channel:read:stream_key` | Copy your stream key to the clipboard with <kbd>y</kbd>. The key is never displayed. |
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

Saving the credential form takes you straight to a screen headed **Authorise your
accounts**. (If you are coming back to this later, it is also the **Accounts**
section of the Config tab, <kbd>Alt</kbd>+<kbd>5</kbd>.)

| Key | Does |
|---|---|
| <kbd>j</kbd> / <kbd>k</kbd>, <kbd>↑</kbd> / <kbd>↓</kbd> | Move between the platforms |
| <kbd>Space</kbd> | Tick or untick one |
| <kbd>Enter</kbd> | Authorise everything ticked |
| <kbd>c</kbd> | Back to the credential form, to fix a typo without quitting |
| <kbd>s</kbd> | Skip, and carry on with whatever logins already exist |

Tick both and press <kbd>Enter</kbd>. Your browser opens twice, once per
platform. What happens:

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

Open the Config tab (<kbd>Alt</kbd>+<kbd>5</kbd>) and look at **Accounts**. Each
platform says whether a login is saved and which account it belongs to. Pressing
<kbd>Enter</kbd> on a row logs that platform out again if you ever need to — for
instance to authorise a different channel.

**Diagnostics**, in the same tab, is the fuller picture: credentials, logins,
token expiry, clipboard, terminal and OBS, each reported as `ok`, `warn` or
`fail`. It is the first place to look when anything misbehaves.

Access tokens are short-lived — Google's last about an hour — but a refresh token
is saved alongside them and renewal happens silently, so a session lasting all
evening keeps working.

You do not have to authorise both platforms at once: tick one, press
<kbd>Enter</kbd>, and come back to the other whenever you like.

---

## 7. Your first stream

### Before you go live

Set OBS up as you normally would: OBS's own **Settings → Stream** pointing at
Twitch, and the Aitum multistream plugin holding YouTube as a second
destination. `msm` never touches OBS and never changes those settings. See
[OBS and Aitum](obs-and-aitum.md) for the details, including where the YouTube
stream key comes from the very first time.

If it is your first YouTube broadcast, it is worth seeing which stream keys
already exist on the channel: **Config → Housekeeping → *List YouTube stream
keys***. It lists the ids, and only the ids — a key itself is never shown,
because this window is often part of the broadcast.

### The five tabs

Once you are set up, `msm` opens on the Stream Info tab. The five tabs are
<kbd>Alt</kbd>+<kbd>1</kbd> Stream Info, <kbd>Alt</kbd>+<kbd>2</kbd> Chat,
<kbd>Alt</kbd>+<kbd>3</kbd> Combined, <kbd>Alt</kbd>+<kbd>4</kbd> OBS and
<kbd>Alt</kbd>+<kbd>5</kbd> Config. Press <kbd>space</kbd> and pause at any point
and a popup lists every key that can follow it; <kbd>Ctrl</kbd>+<kbd>P</kbd>
searches every action by name. The whole set is in
[Keys and actions](keys.md).

### Screen 1 — platforms

<kbd>↑</kbd>/<kbd>↓</kbd> to move, <kbd>Space</kbd> to tick, <kbd>a</kbd> to tick
everything, <kbd>Enter</kbd> to connect. Connecting verifies both logins and
records which account each resolved to, so streaming to the wrong channel is
caught before anything is changed.

### Screen 2 — the form

One set of fields for both platforms.

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

### Screen 3 — the dashboard

Watch and manage URLs, the ingest URL, a masked stream key, and live statistics
for both platforms side by side.

| Key | Does |
|---|---|
| <kbd>r</kbd> | Refresh statistics now |
| <kbd>o</kbd> | Open the watch page in your browser |
| <kbd>y</kbd> / <kbd>Y</kbd> | Copy the Twitch / YouTube stream key to the clipboard |
| <kbd>e</kbd> | Back to the form to change something and submit again |
| <kbd>q</kbd> | Quit |

> [!WARNING]
> A stream key is **copied, never shown** — there is no reveal key anywhere in
> the program. The value goes from the API straight to the system clipboard
> inside a background task, so nothing that could end up on screen, in a
> recording or in the log file ever holds it.

### Now start OBS

Press **Start Streaming** in OBS. That is the only thing that puts video on the
wire; nothing `msm` does sends a single frame.

With `youtube_auto_start = true` (the default), YouTube flips the broadcast live
by itself as soon as it sees the incoming feed, so you never open YouTube
Studio. With it off, you have to press **Go live** there yourself once OBS is
connected.

### Making the next time faster

Press <kbd>Ctrl</kbd>+<kbd>S</kbd> in the form and everything you typed is saved
into the `[preset]` section of `config.toml`, including the Twitch category id it
resolved for you. Next time, the form opens already filled in and going live is
<kbd>Ctrl</kbd>+<kbd>G</kbd> and nothing else. That workflow, and how to keep
more than one preset, is covered in
[Configuration](configuration.md#the-preset-workflow).

---

## Where to go next

* [Configuration](configuration.md) — every setting, explained.
* [Keys and actions](keys.md) — every binding, and how to change one.
* [OBS and Aitum](obs-and-aitum.md) — how this slots into your existing setup.
* [Troubleshooting](troubleshooting.md) — when a step above did not work.
* [Back to the documentation index](README.md).
