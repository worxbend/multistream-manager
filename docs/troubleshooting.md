# Troubleshooting

Each entry below is a real failure, what causes it, and what to do about it.
Many of them are already explained inline by `msm` itself when they happen —
this page is the longer version, with the reasoning.

**First two things to try**

```bash
msm status
```

separates "the credentials are missing" from "the login has expired" without
touching the network, and

```bash
msm paths
```

tells you where the log file is. The interface owns the terminal, so nothing can
be printed to the screen while it runs and every diagnostic goes to that file
instead:

```bash
MSM_LOG=debug msm                                    # in one terminal
tail -f "$(msm paths | awk '/^Log:/{print $2}')"     # in another
```

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
* [msm key youtube does not print a key](#msm-key-youtube-does-not-print-a-key)

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

**In the meantime**, Twitch is unaffected. Run `msm go --platforms twitch` and
stream to Twitch alone while you wait.

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
logged in as the right channel: `msm status` and the "connected as" line printed
at the start of `msm go` both name the account that will actually be used. A
Google account with several channels can easily authorise the wrong one — if it
did, run `msm logout youtube`, then `msm login youtube`, and pick the right
channel on the consent screen.

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

**Fix.**

```bash
msm login twitch
```

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
Another process already holds that port. The two common cases are an earlier
`msm login` that is still waiting for a browser tab you never finished, and an
unrelated program that happens to use 8017.

**Fix, in order of preference.**

1. **Find the earlier login and finish or stop it.** If a terminal is still
   sitting at "Opening your browser to authorise…", press <kbd>Ctrl</kbd>+<kbd>C</kbd>
   there and try again. A login that is never completed gives up after five
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
channel. Then run `msm login youtube` again.

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
* **In the config**: find the exact spelling first.

  ```bash
  msm categories software
  ```

  ```
  ID             NAME
  1469308723     Software and Game Development
  ```

  Then put the **name** in `twitch_category`. The id is filled in automatically
  the first time it is used, and cached so the lookup does not repeat.

Twitch's search is fuzzy, so a partial query is usually enough — but it is a
search over Twitch's real catalogue, and a category you invented will not be
found however you spell it. Twitch's non-game categories are in there too:
`msm categories just chatting` finds *Just Chatting*.

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
Twitch is selected, because Twitch rejects it outright. The form will not submit
and `msm go` stops with `error:`.

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
you pick that channel on Google's consent screen during `msm login youtube`.

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

**To make it deterministic**, list the keys and pin the one your encoder is
configured for:

```bash
msm streams
```

```toml
[youtube]
stream_id = "Vy8dQ...oqA"
```

With `stream_id` set, that stream and no other is bound, and if it is missing
you get an explicit error instead of a silent substitution.

Reveal the key when you need it, with <kbd>k</kbd> on the dashboard or:

```bash
msm streams --show-keys
```

---

## stream_id does not exist on the channel

```
Warning: `stream_id` in your config is "Vy8dQ...oqA", which is not on this
channel. Going live will fail until you correct that setting or clear it.
```

from `msm streams`, or during a go-live:

```
the stream id "Vy8dQ...oqA" set as `stream_id` in your config does not exist on
your channel. Remove that setting to let the application pick one automatically.
```

**Cause.** The pinned stream was deleted in YouTube Studio, or the id was
mistyped, or it belongs to a different channel than the one you are logged in
as.

**Fix.** Run `msm streams`, copy an id that is actually listed, and put that in
the config — or clear `stream_id = ""` entirely, which is the right answer when
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
dashboard to edit and resubmit, or run `msm go --platforms <the failed one>`.

Resubmitting to Twitch updates the channel in place and is harmless to repeat.
Resubmitting to YouTube creates a *new* broadcast with a new watch URL, leaving
the previous attempt behind as an unstarted broadcast — `msm cleanup` finds and
removes those. See [Commands](commands.md#msm-cleanup).

`msm go` exits non-zero only when every platform failed, so a wrapper script can
tell the two situations apart without parsing the output.

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

## msm key youtube does not print a key

```
YouTube's stream key is shown on the dashboard after `msm go`, or at
https://studio.youtube.com > Go live > Stream settings.
```

**Cause.** This is not a failure. A Twitch stream key belongs to the channel and
can be read at any time. A YouTube stream key belongs to a *stream object*, and
`msm key` has no broadcast in hand from which to choose one.

**What to use instead.**

```bash
msm streams --show-keys
```

lists every stream on the channel with its id, title and key — which is also how
you find the id to pin as `stream_id`. Or press <kbd>k</kbd> on the dashboard
after going live, which shows the key for the stream actually bound to the
broadcast you have created.

---

* [Getting started](getting-started.md) — the setup these errors come out of.
* [Configuration](configuration.md) — the settings several fixes above refer to.
* [Commands](commands.md) — what each command does and prints.
* [How it works](how-it-works.md) — why the failures are shaped this way.
* [Back to the documentation index](README.md).
