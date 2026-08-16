# Troubleshooting

Each entry below is a real failure, what causes it, and what to do about it.
Many of them are already explained inline by `msm` itself when they happen —
this page is the longer version, with the reasoning.

**First thing to try: Config → Diagnostics.** Press <kbd>Alt</kbd>+<kbd>5</kbd>
for the Config tab, then move to the **Diagnostics** section. It checks the
config file, both platforms' credentials, the saved logins, the clipboard, the
terminal's colour support, the log location and the OBS connection in one go.
Each line reads `ok`, `warn` or `fail`, and every warning says what to do about
it rather than only what is wrong.

**Then Config → Accounts**, which separates "the credentials are missing" from
"the login has expired" without touching the network, and **Config → Files**,
which tells you where the log file is.

The interface owns the terminal, so nothing can be printed to the screen while it
runs and every diagnostic goes to that log file instead. To watch it live, with
more detail than usual:

```bash
MSM_LOG=debug msm                                  # in one terminal
tail -f ~/.config/multistream-manager/msm.log      # in another
```

(That is the Linux path. Config → Files shows the real one on your machine.)

> [!NOTE]
> Some error messages quoted on this page still end with wording like
> *"Run `msm login twitch`"*. There is no command line any more, so read those as
> **"log in again under Config → Accounts"** — press <kbd>Enter</kbd> on the
> platform's row to log out, then <kbd>Enter</kbd> again to log back in. The
> messages are quoted as the program prints them so you can match what you see on
> screen against what is written here.

**Contents**

* [YouTube: quotaExceeded](#youtube-quotaexceeded)
* [YouTube: liveStreamingNotEnabled](#youtube-livestreamingnotenabled)
* [invalid_grant when a saved login is used](#invalid_grant-when-a-saved-login-is-used)
* [Twitch: your saved token does not include a permission](#twitch-your-saved-token-does-not-include-a-permission)
* [could not listen on port 8017](#could-not-listen-on-port-8017)
* [Google says the app is not verified](#google-says-the-app-is-not-verified)
* [Could not find a Twitch category matching that name](#could-not-find-a-twitch-category-matching-that-name)
* [The title is too long for YouTube](#the-title-is-too-long-for-youtube)
* [Your Google account has no YouTube channel](#your-google-account-has-no-youtube-channel)
* [The YouTube stream key changed](#the-youtube-stream-key-changed)
* [stream_id does not exist on the channel](#stream_id-does-not-exist-on-the-channel)
* [One platform worked and the other did not](#one-platform-worked-and-the-other-did-not)
* [The YouTube category field will not search](#the-youtube-category-field-will-not-search)
* [The YouTube stream key is not there to copy](#the-youtube-stream-key-is-not-there-to-copy)
* [Copying a stream key does nothing](#copying-a-stream-key-does-nothing)
* [The theme looks wrong, or colours are approximate](#the-theme-looks-wrong-or-colours-are-approximate)
* [The OBS tab will not connect](#the-obs-tab-will-not-connect)

---

## YouTube: quotaExceeded

```
… failed (HTTP 403): The request cannot be completed because you have exceeded your quota.
  You have used up today's YouTube API quota. It resets at midnight Pacific
  time. Raising `poll_interval_secs` in your config makes the quota last
  longer, since every statistics refresh spends some of it.
```

`dailyLimitExceeded` is the same thing under a different name.

**Cause.** The YouTube Data API charges each request a number of "units" against
a per-project daily allowance — 10,000 units a day for a new project by default.
Different calls cost different amounts, and reads are much cheaper than writes,
but a dashboard left open all evening refreshing every few seconds adds up. So
does a long series of test go-lives, since each one creates a broadcast.

**Fix.**

1. Raise `poll_interval_secs` in `[general]`. The default is 15 seconds; 30 or
   60 costs a fraction as much and tells you almost as much, because neither
   platform updates its viewer count faster than that anyway.
2. Close the dashboard when you are not looking at it. Statistics polling stops
   when `msm` exits.
3. Wait. The quota resets at midnight Pacific time, not at midnight where you
   are.
4. If you genuinely need more, request a quota increase in the Google Cloud
   console for your project. That is a review process, not a switch.

**In the meantime**, Twitch is unaffected. Untick YouTube on the platform screen
and stream to Twitch alone while you wait.

See [Configuration](configuration.md#general) for `poll_interval_secs`.

---

## YouTube: liveStreamingNotEnabled

```
creating the YouTube broadcast failed (HTTP 403): …
  Live streaming is not enabled on this YouTube channel. Enable it at
  youtube.com/features — note that first-time activation takes 24 hours.
```

**Cause.** Live streaming is a per-channel capability that is off until you turn
it on, and separate from anything you did in the Google Cloud console. Enabling
the YouTube Data API for your project does not enable streaming for your
channel.

**Fix.**

1. Go to <https://youtube.com/features> while signed in as the channel you
   stream from.
2. Request live streaming access. You will need a verified phone number on the
   account.
3. **Wait 24 hours.** The first activation has a mandatory delay. Nothing in
   `msm` shortens it, and retrying during the wait produces this same error.

If you are certain streaming is enabled and you still see this, check you are
logged in as the right channel. **Config → Accounts** names the account each
platform resolved to, and so does the "connected as" line in the activity log
when you connect. A Google account with several channels can easily authorise the
wrong one — if it did, press <kbd>Enter</kbd> on the YouTube row to log out,
<kbd>Enter</kbd> again to log back in, and pick the right channel on Google's
consent screen.

---

## invalid_grant when a saved login is used

```
could not renew your YouTube access token. Run `msm login youtube` to authorise again.
```

with `invalid_grant` in the underlying detail or in the log.

**Cause.** The saved refresh token is no longer accepted. Something invalidated
it, and the honest answer is that the provider will not say which:

* You changed your Google or Twitch password.
* You revoked the application's access in your account's security settings.
* The refresh token expired through disuse. Google expires refresh tokens issued
  by an OAuth client still in **Testing** mode after seven days, which is the
  most common cause by far for this project's shape of setup.
* You deleted and recreated the OAuth client, so the old token belongs to a
  client that no longer exists.

**Fix.**

```bash
msm login youtube      # or: msm login twitch
```

That is the whole remedy. The new tokens replace the old ones.

**To stop it recurring every week**, publish the OAuth client rather than
leaving it in Testing mode: Google Cloud console → **APIs & Services** → **OAuth
consent screen** → **Publish app**. For a client only you use, this changes the
seven-day expiry, not the verification warning at login.

If a fresh login still fails, check that `client_id` and `client_secret` in the
config match the OAuth client you are actually using.

---

## Twitch: your saved token does not include a permission

```
your saved Twitch token does not include the channel:manage:broadcast permission,
so it cannot change your stream title or category. Run `msm login twitch` to
re-authorise with the current permissions.
```

**Cause.** An OAuth token carries the exact set of permissions ("scopes") that
were requested at the moment it was issued. A token saved before a scope was
added does not gain it retrospectively. This most often happens after updating
`msm`, or after logging in through some other tool that requested fewer scopes.

`msm` checks for `channel:manage:broadcast` during `connect`, on purpose, so
that a missing scope is reported as a missing scope rather than as a bare `401`
in the middle of a go-live.

**Fix.** Open **Config → Accounts**, press <kbd>Enter</kbd> on the Twitch row to
log out and <kbd>Enter</kbd> again to log back in. The new token is issued with
the current set of permissions.

The scopes requested and what each is for are listed in
[Getting started](getting-started.md#what-permissions-are-being-requested).

### The related, quieter symptom

```
could not read your Twitch stream key. The saved login may predate the
`channel:read:stream_key` permission — run `msm login twitch` again.
```

Reading the stream key is best-effort: a token without that scope still lets you
set the title and category, so the go-live succeeds and only the key is missing.
The same re-login fixes it.

**Subscriber count missing but everything else working** is not a scope problem.
Twitch returns `403` on the subscriptions endpoint for channels that are not
affiliates or partners, so the row is left out of the statistics panel.

---

## could not listen on port 8017

```
could not listen on 127.0.0.1:8017 for the login redirect.
Something else is probably using that port. Change `oauth_port`
in your config (and update the redirect URI in the Twitch developer
console to match) or stop the other program.
```

**Cause.** The login needs to receive the browser's redirect, so it starts a
listener on the loopback address at `oauth_port` before opening the browser.
Another process already holds that port. The two common cases are another copy of
`msm` still waiting for a browser tab you never finished, and an unrelated
program that happens to use 8017.

**Fix, in order of preference.**

1. **Find the earlier login and finish or stop it.** If another `msm` window is
   still waiting for the browser, finish the login there or press
   <kbd>Ctrl</kbd>+<kbd>C</kbd> to quit it, then try again. A login that is never completed gives up after five
   minutes on its own.
2. **Find out what holds the port:**

   ```bash
   ss -ltnp 'sport = :8017'      # Linux
   lsof -nP -iTCP:8017 -sTCP:LISTEN   # macOS
   ```

3. **Move to another port.** This is a three-part change and all three parts
   must agree, character for character:

   ```toml
   [general]
   oauth_port = 8123
   ```

   * Twitch developer console → your application → **OAuth Redirect URLs** →
     `http://localhost:8123/callback`
   * Google Cloud console → **Credentials** → your OAuth client → authorised
     redirect URI → `http://localhost:8123/callback`

   A mismatch here is rejected by the provider before the login screen even
   appears, usually as `redirect_uri_mismatch`.

**Only IPv6 failed** is not an error. The listener binds IPv4 and, when the host
has it, IPv6 as well; a machine without IPv6 loopback is perfectly functional
and the failure is logged at debug level only.

---

## Google says the app is not verified

Two different messages, with two different meanings. Read which one you got.

### "Google hasn't verified this app" — a warning you can pass

This is expected and harmless. The "app" is the OAuth client *you* created;
verification is a review process for applications distributed to strangers, and
it has no bearing on a client only you will ever use.

Click **Advanced**, then **Go to … (unsafe)**, then continue as normal.

### "Access blocked" — a refusal you cannot pass

If Google refuses outright rather than warning, your account is not on the
client's **Test users** list. While an OAuth client is in Testing mode Google
authorises listed accounts only, and your own account is not listed by default.

**Fix.** Google Cloud console → **APIs & Services** → **OAuth consent screen**
→ **Test users** → **Add users** → add the Google account that owns your YouTube
channel. Then log in to YouTube again under **Config → Accounts**.

This is the trap most people lose an evening to. It is described in full in
[Getting started](getting-started.md#trap-1-google-refuses-the-login-unless-you-are-on-the-test-users-list).

---

## Could not find a Twitch category matching that name

```
could not find a Twitch category matching "Softwear and Game Development" from
your config. Check the spelling against Twitch's own category list.
```

or, in the form, the submit hint stays grey with:

> Pick a Twitch category — type part of its name and press Enter to select a match.

**Cause.** Twitch's channel-update endpoint accepts a numeric `game_id` and
nothing else. A name has to be looked up first, and a name that matches nothing
cannot be turned into an id. In the interface, typing a name without selecting a
match from the list leaves the plan with no id at all, which is the same
situation.

**Fix.**

* **In the form**: type part of the name, wait for the list, and press
  <kbd>Enter</kbd> to select an entry. Typing over a previously selected
  category clears the selection, so re-select after editing.
* **In the config**: find the exact spelling first, by typing part of it into
  the form's category field and reading the matches back:

  ```
  ID             NAME
  1469308723     Software and Game Development
  ```

  Then put the **name** in `twitch_category`. The id is filled in automatically
  the first time it is used, and cached so the lookup does not repeat.

Twitch's search is fuzzy, so a partial query is usually enough — but it is a
search over Twitch's real catalogue, and a category you invented will not be
found however you spell it. Twitch's non-game categories are in there too:
typing `just chatting` finds *Just Chatting*.

If a stale `twitch_category_id` is left next to a changed `twitch_category`, the
pair is treated as unresolved and looked up again, so you do not have to clear
the id by hand.

---

## The title is too long for YouTube

In the form, the character counter turns red and the issue list says:

> Title is 118 characters; YouTube allows 100. It will be shortened for YouTube
> only.

**Cause.** The two platforms disagree: Twitch accepts 140 characters, YouTube
100. There is no single limit that satisfies both.

**What actually happens.** This is a warning, not a blocking error. The title is
cut to 100 characters for YouTube and sent in full to Twitch. Nothing fails.

The related detail: your tags are also appended to the **YouTube title** as
`#hashtags`, and only as many as still fit inside the 100 characters. A long
title therefore silently costs you hashtags before it costs you title text.
YouTube only turns the first three hashtags in a title into links anyway, so
there is nothing to gain from forcing more.

**Fix, if you would rather not be truncated.** Shorten the title to 100
characters or fewer. The counter in the form shows the tighter of the two
selected platforms' limits, so with both ticked it counts against 100 and you
can see the point where the cut would fall.

**Over 140 characters is a different matter.** That is a blocking error while
Twitch is selected, because Twitch rejects it outright. The submit hint stays
grey and the form will not send.

Description length behaves similarly: over 5000 characters is blocking for
YouTube, and Twitch has no description field at all, so the description you type
is YouTube-only in every case.

---

## Your Google account has no YouTube channel

```
your Google account has no YouTube channel. Open youtube.com, create a channel,
then run `msm login youtube` again.
```

**Cause.** A Google account and a YouTube channel are not the same thing. An
account that has never created a channel has nothing for the API to talk about.
This also appears when you authorised the wrong account out of several.

**Fix.** Create the channel at <https://youtube.com>, then log in again. If the
account is right but the channel is a brand account you switch into, make sure
you pick that channel on Google's consent screen while logging in.

---

## The YouTube stream key changed

It should not. `msm` binds each new broadcast to a stream that already exists on
your channel precisely so that the key never changes — see
[OBS and Aitum](obs-and-aitum.md#why-the-youtube-stream-key-stays-the-same).

There are three reasons it can happen anyway, and the panel tells you which:

1. **The channel had no stream key at all.** Expected on a first run. The note
   says a new one was created and that you should copy it into OBS or Aitum
   once.
2. **The existing key could not be bound.** The stream found belonged to a
   single past broadcast and cannot be reused. Rather than abandon the go-live,
   a fresh stream is created and bound, and the note says so. Paste the new key
   into Aitum; it is a reusable stream, so this will not recur.
3. **`reuse_stream = false` in your config.** Set it back to `true`.

**To make it deterministic**, list the stream ids and pin the one your encoder
is configured for. **Config → Housekeeping → *List YouTube stream keys*** writes
them to the activity log; copy the id you want into the config:

```toml
[youtube]
stream_id = "Vy8dQ...oqA"
```

With `stream_id` set, that stream and no other is bound, and if it is missing you
get an explicit error instead of a silent substitution.

To get at the key itself, press <kbd>Y</kbd> on the Stream Info tab after going
live — it copies the key of the stream actually bound to your broadcast straight
to the clipboard. Nothing in the program ever displays it.

---

## stream_id does not exist on the channel

```
Warning: `stream_id` in your config is "Vy8dQ...oqA", which is not on this
channel. Going live will fail until you correct that setting or clear it.
```

from the housekeeping listing, or during a go-live:

```
the stream id "Vy8dQ...oqA" set as `stream_id` in your config does not exist on
your channel. Remove that setting to let the application pick one automatically.
```

**Cause.** The pinned stream was deleted in YouTube Studio, or the id was
mistyped, or it belongs to a different channel than the one you are logged in
as.

**Fix.** Run **Config → Housekeeping → *List YouTube stream keys***, copy an id
that is actually listed, and put that in the config — or clear `stream_id = ""` entirely, which is the right answer when
the channel only has one key.

This is deliberately a hard failure rather than a quiet fallback. Binding some
other stream would produce a broadcast that your encoder is not sending to,
which looks like everything working right up until nobody sees any video.

---

## One platform worked and the other did not

This is intended behaviour, not a bug, and nothing is rolled back.

```
Some platforms are ready and some are not. The ones marked Ready will work if
you start streaming now.
```

**What it means.** The platform marked Ready is genuinely configured. You can
press Start Streaming and go out on it immediately. The failing platform's panel
carries its own error and reason; fix that and press <kbd>e</kbd> on the
dashboard to edit and resubmit, ticking only the platform that failed.

Resubmitting to Twitch updates the channel in place and is harmless to repeat.
Resubmitting to YouTube creates a *new* broadcast with a new watch URL, leaving
the previous attempt behind as an unstarted broadcast. **Config → Housekeeping →
*Find abandoned broadcasts*** finds and removes those: the first
<kbd>Enter</kbd> lists them, a second deletes the ones listed, and anything that
has ever been live is neither listed nor touched.

---

## The YouTube category field will not search

If the field shows the built-in short list rather than YouTube's full one, the
API list is unavailable: nothing is connected yet, the login has expired, or the
quota has run out. The field falls back to a built-in list of the ten categories
a live stream realistically uses, filtered locally, so it is always usable.

The full list replaces it as soon as the API answers. If it never does, work
through [quotaExceeded](#youtube-quotaexceeded) and
[invalid_grant](#invalid_grant-when-a-saved-login-is-used) above, and check the
log for a line beginning "Category search failed".

The ids in the built-in list are in
[Configuration](configuration.md#youtube_category_id) if you would rather set one
by hand.

---

## The YouTube stream key is not there to copy

<kbd>Y</kbd> on the Stream Info tab reports that there is no YouTube key to copy.

**Cause.** This is usually not a failure. A Twitch stream key belongs to the
channel and can be read at any time. A YouTube stream key belongs to a *stream
object*, and until you have gone live there is no broadcast in hand from which to
choose one — so before the first go-live of a session, there is nothing to copy.

**What to do.** Go live first (<kbd>Ctrl</kbd>+<kbd>G</kbd>), then press
<kbd>Y</kbd>. That copies the key of the stream actually bound to the broadcast
you just created, which is the one your encoder needs.

If you need it before then, it is also at <https://studio.youtube.com> under
**Go live → Stream settings**. And **Config → Housekeeping → *List YouTube stream
keys*** shows the *ids* of the reusable streams on the channel, which is how you
find the value for `stream_id` — the ids only, never a key.

---

## Copying a stream key does nothing

You press <kbd>y</kbd>, the interface says the key was copied, and pasting into
OBS gives you nothing — or something you copied earlier.

Stream keys are never displayed, only copied, so this is the one place where
copying failing silently would leave you stuck. There are two routes and `msm`
tries both:

1. **A helper program** — `wl-copy` (Wayland), `xclip` or `xsel` (X11),
   `pbcopy` (macOS), `clip` (Windows). This is the reliable route when `msm` is
   running on the same machine as your desktop.
2. **OSC 52**, an escape sequence asking the *terminal emulator* to set its own
   clipboard. This is the only route that can work over ssh, because the
   terminal doing the pasting is the one in front of you rather than the
   machine `msm` runs on.

Start with the **clipboard** line in **Config → Diagnostics**.

**"no clipboard helper is installed"** — install one (`wl-copy` on Wayland,
`xclip` on X11). Copying will still try the escape sequence in the meantime.

**"installed but has no display to talk to"** — normal over ssh, and nothing
needs installing. Copying uses the escape sequence, which many terminals
disable by default. Turn it on:

| Terminal | Setting |
|---|---|
| xterm | `XTerm*disallowedWindowOps: 20,21,SetXprop` in `.Xresources` |
| Alacritty | on by default |
| kitty | `clipboard_control write-clipboard write-primary` |
| WezTerm | on by default |
| tmux | `set -g set-clipboard on`, and the outer terminal must allow it too |
| screen | does not forward OSC 52; use tmux or a direct connection |

If you are inside tmux or screen, both the multiplexer *and* the terminal
underneath it have to allow it — a single "no" anywhere in the chain is enough.

There is no last-resort "print the key" anywhere in the program, by design: a
key on standard output lands in your scrollback, and this terminal is often on
stream. If no clipboard route can be made to work, read the key from the
platform's own dashboard — Twitch's is under **Settings → Stream** and YouTube's
under **Go live → Stream settings**.

---

## The theme looks wrong, or colours are approximate

Themes are written as exact 24-bit colours. A terminal that cannot show them
approximates each to the nearest colour it has, which can turn a carefully
chosen palette into something muddy.

Check the **terminal** line in **Config → Diagnostics**.

If that warns, your terminal is not advertising 24-bit colour. Most modern ones
support it and simply need telling to say so:

```bash
export COLORTERM=truecolor
```

Inside tmux, also make sure the outer terminal's capability is passed through:

```
set -g default-terminal "tmux-256color"
set -as terminal-features ",*:RGB"
```

Two related things that are *not* faults:

* **A theme name that does not exist** falls back to the default palette and
  logs a warning rather than failing to start. The theme picker
  (<kbd>&lt;Leader&gt;</kbd> <kbd>u</kbd> <kbd>t</kbd>) lists every valid name,
  and Config → Diagnostics reports the fallback.
* **A hand-written `[appearance.custom_theme]` can be unreadable.** Every
  built-in palette is checked by a test to keep body text at or above the 4.5:1
  contrast ratio the WCAG guidelines set for readable text; a custom one is
  not. If text has become hard to read, that is the first thing to look at.

If the terminal's own background does not match the theme — a light theme
framed in dark, say — that is `terminal_background = false` in `[appearance]`,
which is the default. Turning it on repaints the whole window; it is off by
default because it replaces a deliberately transparent or blurred background
with a solid colour.

---

## The OBS tab will not connect

The tab says "waiting for OBS". The **OBS** section of the Config tab, and the
OBS line in **Config → Diagnostics**, say which of the four usual causes it is.

**"Connection refused"** — nothing is listening. Either OBS is not running, or
its WebSocket server is off: **Tools → WebSocket Server Settings** in OBS, tick
"Enable WebSocket server". Check the port there matches `port` under `[obs]`;
the default is 4455 on both sides.

**"the password is probably wrong"** — OBS closes the connection without a word
when authentication fails, so this is inferred rather than reported by OBS.
Press "Show Connect Info" in that same settings window to see the real
password. If you are using `password_env`, check the variable is actually set
in the environment `msm` runs in:

```bash
echo "${OBS_WEBSOCKET_PASSWORD:-(not set)}"
```

A variable set in your shell profile will not be visible to a `msm` started
from a desktop launcher, which is a common way for this to work in one terminal
and not another.

**"OBS control is turned off"** — `enabled = false` under `[obs]`.

**It connects and then drops repeatedly** — OBS restarting, or a firewall
between two machines. The interface reconnects on its own with a backoff that
stops growing at thirty seconds; <kbd>R</kbd> on the OBS tab retries
immediately rather than waiting it out.

### A scene or input cannot be found

```
Error: no scene called "brb". Try one of: Starting Soon, Main Camera
```

The name is matched against the OBS name, the aliases in
`[obs.scene_aliases]` / `[obs.audio_aliases]`, and the shortcuts — so this
means none of the three matched. The scene and audio lists on the OBS tab show
exactly what OBS reports, which is the authority — the real OBS name is always
displayed beside any alias, or the pane and the OBS window would disagree about
what everything is called.

Audio inputs are picked out of OBS's full input list by kind, so a source that
is not an audio capture will not appear — that is deliberate, not a fault.

### A shortcut key stopped doing what it used to

Shortcuts from the config take precedence over the OBS tab's built-in keys. If
you bind `s` to a scene, `s` no longer starts the stream on that tab. The keys
the tab uses itself are `h`, `j`, `k`, `l`, `m`, `M`, `s`, `r`, `p`, `P`, `C`,
`R`, `u` and `q` — the full list is in [Keys and actions](keys.md#obs).

---

* [Getting started](getting-started.md) — the setup these errors come out of.
* [Configuration](configuration.md) — the settings several fixes above refer to.
* [Keys and actions](keys.md) — where each part of the interface lives.
* [How it works](how-it-works.md) — why the failures are shaped this way.
* [Back to the documentation index](README.md).
