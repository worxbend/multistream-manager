# How it works

This page describes the architecture and the reasoning behind it. You do not
need any of it to use `msm`, but you do need it to change `msm`, or to
understand why a particular failure looks the way it does.

**Contents**

* [The shape of the program](#the-shape-of-the-program)
* [One plan, many platforms](#one-plan-many-platforms)
* [The Backend trait](#the-backend-trait)
* [The engine](#the-engine)
* [Twitch: one API call](#twitch-one-api-call)
* [YouTube: four API calls](#youtube-four-api-calls)
* [The `videos.update` overwrite trap](#the-videosupdate-overwrite-trap)
* [The bind fallback](#the-bind-fallback)
* [Partial success](#partial-success)
* [Validation before the network](#validation-before-the-network)
* [Authentication and token renewal](#authentication-and-token-renewal)
* [The interface and its worker](#the-interface-and-its-worker)
* [Finding abandoned broadcasts](#finding-abandoned-broadcasts)
* [Where secrets go, and do not go](#where-secrets-go-and-do-not-go)

---

## The shape of the program

| File | Responsibility |
|---|---|
| `src/model.rs` | The domain types. `StreamPlan`, `Platform`, `Privacy`, the per-platform limits, and validation. Knows nothing about HTTP, OAuth or terminals. |
| `src/backend.rs` | The `Backend` trait every platform implements, plus `PlatformResult`. |
| `src/twitch.rs` | The Twitch Helix client. |
| `src/youtube.rs` | The YouTube Data API v3 client. |
| `src/engine.rs` | Owns the backends and drives them. Fans one plan out concurrently and collects one result per platform. |
| `src/auth/` | The OAuth flow (`oauth.rs`), token storage (`store.rs`), and silent renewal (`mod.rs`). |
| `src/config.rs` | Reading and writing `config.toml`, and converting the file's preset shape to and from `StreamPlan`. |
| `src/ui/app.rs` | All interface state and keyboard handling. Pure state transitions, no I/O. |
| `src/ui/worker.rs` | The background task that performs the slow API work, so the interface never freezes. |
| `src/ui/draw.rs` | Rendering. Reads state, never mutates it. |
| `src/main.rs` | Argument parsing and the non-interactive commands. |
| `src/paths.rs` | Where files live, and writing them with owner-only permissions. |
| `src/lang.rs` | The language list behind the language field's search. |

The dependency direction is one-way: `model` knows about nothing, backends know
about `model`, the engine knows about backends, and the interface knows about
the engine. No UI or orchestration code calls a platform API directly.

Three other modules do use `reqwest`, and it is worth being precise about why:
`engine.rs` builds the one shared HTTP client the backends borrow, so
connections are pooled rather than reopened per call, and `auth/oauth.rs` talks
to the Twitch and Google *token* endpoints, which are part of logging in rather
than part of either platform's API. What the layering actually guarantees is
narrower than "nothing else does HTTP": it is that nothing above the backends
knows the shape of a Twitch or YouTube request.

---

## One plan, many platforms

Everything the user says about a broadcast goes into a single value, a
`StreamPlan`: title, description, tags, Twitch category, YouTube category id,
language, privacy, the made-for-kids declaration, and the two auto-start /
auto-stop toggles.

That one value is the only thing handed to each platform. Nothing downstream
gets a Twitch-shaped plan and a YouTube-shaped plan; each backend translates the
same plan into whatever sequence of API calls its platform happens to need.

```
                 ┌───────────────┐
   the form  ──▶ │  StreamPlan   │ ◀──  [preset] in config.toml
                 └───────┬───────┘
                         │
                 ┌───────▼───────┐
                 │    Engine     │
                 └───┬───────┬───┘
                     │       │
          ┌──────────▼─┐   ┌─▼────────────┐
          │ TwitchBack │   │ YouTubeBack  │
          │   end      │   │   end        │
          └──────┬─────┘   └──────┬───────┘
                 │                │
          PlatformResult    PlatformResult
```

The plan also carries the *adaptation* rules, as methods rather than as code
scattered through the backends:

| Method | Does |
|---|---|
| `twitch_title()` | The title cut to Twitch's 140-character limit. |
| `youtube_title()` | The title cut to YouTube's 100, with as many tags appended as `#hashtags` as still fit. |
| `twitch_tags()` | Tags with spaces and punctuation stripped, each capped at 25 characters, at most 10 of them. |
| `youtube_tags()` | The longest prefix of the tag list that fits YouTube's combined 500-character budget. |

Truncation counts **characters, not bytes**, throughout. A single emoji in a
title is four bytes, and a byte-indexed cut would either panic or produce
mojibake.

---

## The Backend trait

One trait, implemented once per platform:

| Method | Purpose |
|---|---|
| `connect` | Verify the credentials and cache identity (the Twitch broadcaster id, the YouTube channel id). Returns the account name, so a wrong account is visible before anything is changed. |
| `go_live` | Apply the plan and leave the platform ready to accept a feed. |
| `fetch_stats` | One statistics snapshot, called on a timer. |
| `search_categories` | Back the category search. Default: an empty list. |
| `set_access_token` | Replace the token used on subsequent requests. |
| `stream_key` | Read the key without changing anything. Default: `None`. |
| `list_ingest_endpoints` | List the platform's RTMP endpoints. Default: empty. |
| `list_stale_broadcasts` | List broadcasts created but never fed. Default: empty. |
| `delete_broadcast` | Delete one by id. Default: refuse, because a platform that lists nothing can never reach this. |

The last five have defaults on purpose. Twitch has no stream objects and no
broadcast objects, so its answers to those questions are genuinely "nothing"
rather than "not implemented" — and returning an empty list is a truthful
answer, not a stub. That is what lets `msm streams` and `msm cleanup` be written
without either of them naming YouTube.

Adding a third platform means writing one file that implements this trait and
adding a variant to `Platform`. The interface does not change.

---

## The engine

`Engine` holds one boxed backend per selected platform and is the only thing
that knows there is more than one.

**Concurrency.** `go_live` and `poll_stats` drive every platform at once rather
than one after another, so going live on both takes as long as the slower one
instead of the sum. Each backend is moved into its own task and moved back
afterwards, which is what keeps a later stats poll able to use the same
connected backend.

**Ordering.** Results are sorted back into the canonical Twitch-then-YouTube
order before they are returned, so the panels do not reshuffle themselves
depending on which request happened to finish first.

**Category resolution.** A hand-edited config says
`twitch_category = "Just Chatting"` with no id, and Twitch's update endpoint will
not accept a name. Before submitting, the engine searches for the name, prefers
an exact case-insensitive match, and otherwise takes the best fuzzy hit rather
than failing — the config was written by a human typing from memory. The
resolved id is what the form saves back, so the lookup does not repeat.

---

## Twitch: one API call

Twitch has no concept of creating a broadcast. Your channel is permanently
there; going live means pointing an encoder at it. So the whole of `go_live` on
Twitch is:

```
PATCH https://api.twitch.tv/helix/channels?broadcaster_id=…
{ "title": …, "game_id": …, "broadcaster_language": …, "tags": [ … ] }
```

A success returns `204 No Content`. There is nothing to create and nothing to
clean up afterwards.

Two other calls surround it, and neither is part of applying the plan:

* `connect` calls `https://id.twitch.tv/oauth2/validate`, which is unusual in
  taking an `OAuth <token>` header rather than `Bearer <token>` and needing no
  client id. It returns the user id, the login name and the granted scopes. The
  scopes are checked immediately: a saved token lacking
  `channel:manage:broadcast` is reported as exactly that, rather than as a bare
  `401` from a later call.
* After the update, the stream key is fetched from `/helix/streams/key`. This is
  best-effort: if the token lacks `channel:read:stream_key`, the dashboard is
  shown without a key rather than the whole go-live failing.

Two details in the request body are worth knowing:

**`game_id`, never a name.** Twitch's API accepts only the numeric category id,
which is why the form makes you select a match rather than accepting typed text,
and why `msm categories` exists at all.

**Empty tags are omitted, not sent as `[]`.** Twitch documents an empty array as
"remove every tag from this channel". Sending one when the user had merely not
set any tags would silently wipe tags they had added elsewhere. Every field on
this endpoint is optional, so the field is left out instead.

---

## YouTube: four API calls

YouTube models a live stream as two objects that have to be joined, as described
in [OBS and Aitum](obs-and-aitum.md#youtube). Applying one plan therefore takes
four calls, in this order:

| # | Call | Why |
|---|---|---|
| 1 | `liveStreams.list` (or `liveStreams.insert`) | Find somewhere for OBS to push to — an existing stream key, or a new one when there is nothing to reuse. |
| 2 | `liveBroadcasts.insert` | Create the event: title, description, scheduled start, privacy, made-for-kids, the auto-start and auto-stop flags. |
| 3 | `liveBroadcasts.bind` | Join the two, so the broadcast knows which pipe feeds it. |
| 4 | `videos.update` | Set the tags, the category and the language — because `liveBroadcasts.insert` has no fields for any of them. |

Step 4 exists purely because of that gap in step 2. A broadcast *is* a video, so
the ordinary `videos.update` endpoint is used with the broadcast id.

Some choices made in step 2, and the reasons:

* **`scheduledStartTime` is one minute in the future.** YouTube requires the
  field and rejects a time in the past; a minute of slack survives clock skew
  between your machine and Google's.
* **`enableDvr` and `recordFromStart` are on**, so the broadcast is watchable
  after the fact.
* **The monitor stream is off.** That is YouTube Studio's preview pane, and
  turning it off removes several seconds of delay before viewers see you.
* **`selfDeclaredMadeForKids`** is sent on every broadcast because YouTube
  requires the declaration.

Step 4 is deliberately **not fatal**. If it fails, the broadcast already exists
and is already bound, so you can still stream; losing the tags is a much better
outcome than aborting a go-live that has half happened. The failure is reported
as a note on the panel telling you to fix it in Studio if it matters.

---

## The `videos.update` overwrite trap

This is the sharpest edge in the YouTube API and it is worth stating on its own.

`videos.update` replaces **every mutable field in the parts you name**. It is
not a merge. Sending `part=snippet` with only `tags` populated does not add tags
to the video — it wipes the title, the description and the category, because
those fields were absent from the snippet you sent.

Since step 4 above sends `part=snippet`, it must therefore send the *complete*
snippet every time: title, description, category id and tags together, even
though only two of them are new information at that point. Anything left out
would be erased from the broadcast created moments earlier in step 2.

The one field handled conditionally is the language. `defaultLanguage` is only
included when the configured code is exactly two characters, because an invalid
code makes YouTube reject the entire update. (`defaultAudioLanguage` is
read-only on the videos resource, so setting it would be dropped rather than
applied, and it is not sent.)

---

## The bind fallback

Step 1 chooses a stream to reuse, and there is a genuine limitation in the API
around that choice.

The obvious filter would be `contentDetails.isReusable` — a flag on the stream
resource saying whether it can be bound more than once. But `liveStreams.list`
does not support the `contentDetails` part at all; its documented part values
are `id`, `snippet`, `cdn` and `status`. Asking for it makes the request
invalid, so the flag cannot be read at listing time.

Rather than guess, the candidate is chosen without it and the **bind in step 3
is what validates the choice**. If binding fails — typically because the stream
found belongs to a single past broadcast — the code creates a fresh stream,
binds that, and replaces the reassuring note with one telling you a new key was
made and needs pasting into OBS. The API itself makes the decision, which is
strictly better than a heuristic.

The user-facing consequences of this are covered in
[OBS and Aitum](obs-and-aitum.md#what-happens-when-the-key-cannot-be-reused).

---

## Partial success

If you are going live on two platforms and one of them fails, you want the other
one's URLs and stream key regardless. So a platform's outcome is not a plain
`Result` that can abort the operation — it is a `PlatformResult`, one per
platform, collected and returned together.

Concretely:

* **Nothing is rolled back.** If Twitch succeeded and YouTube ran out of quota,
  your Twitch channel is genuinely configured. Undoing that would throw away
  work you asked for.
* **Nothing is hidden.** The failing platform's panel shows the error and its
  reason; the succeeding platform's panel shows its URLs and its key.
* **The interface says so explicitly**, rather than leaving you to infer it from
  one panel looking different: *"Some platforms are ready and some are not. The
  ones marked Ready will work if you start streaming now."*
* **`msm go` exits non-zero only when *every* platform failed**, so a wrapper
  script can distinguish "nothing worked" from "one of two worked" without
  parsing output.
* **Statistics keep working the same way.** A failed poll records the error in
  that platform's snapshot and the dashboard marks the numbers as stale, instead
  of the whole refresh loop dying.

The same principle applies inside the YouTube backend, one level down: a failed
`videos.update` costs you the tags, not the broadcast.

---

## Validation before the network

Everything checkable locally is checked before an API call is spent being told
the same thing by Twitch or Google. `StreamPlan::validate` takes the selected
platforms, because most rules are platform-specific, and returns issues that are
either blocking or advisory.

| Condition | Verdict |
|---|---|
| Empty title | Blocking. Both platforms reject it. |
| Title over 140 characters, Twitch selected | Blocking. |
| Title over 100 characters, YouTube selected | Advisory — it is shortened for YouTube only. |
| Description over 5000 characters, YouTube selected | Blocking. |
| No Twitch category, Twitch selected | Blocking. The API needs a numeric id. |
| More than 10 tags, Twitch selected | Advisory — the extras go to YouTube only. |
| A tag containing spaces or punctuation, Twitch selected | Advisory, and it tells you what will actually be sent. |
| No YouTube category, YouTube selected | Blocking. |
| Language that is not a two-letter code | Blocking when Twitch is selected, advisory otherwise. |

The form uses this live: the submit hint turns green only when nothing blocking
remains. `msm go` prints the same issues as `error:` and `note:` lines and stops
on the first kind.

---

## Authentication and token renewal

The login is an OAuth authorisation-code flow with PKCE, redirecting to a
loopback address. Google's device flow does not grant the YouTube scopes needed
here for anything other than a television-style device, so a local redirect is
the supported route.

Two details that are easy to get wrong and are handled explicitly:

* **The listener binds both loopback families.** The registered redirect URI
  uses the name `localhost`, because Twitch documents that as the host permitted
  with a plain `http` redirect. `localhost` then resolves to `127.0.0.1` on some
  machines and `::1` on others, and the browser picks. IPv4 is required — if
  that bind fails the port is genuinely unusable — and IPv6 is best-effort.
* **Google needs `access_type=offline` and `prompt=consent`.** Without the first
  it issues no refresh token at all. Without the second, re-authorising an
  account that has already approved returns no refresh token either, which makes
  a repeat login useless for fixing an expired one.

The listener is bound *before* the browser is opened, because a fast browser can
otherwise hit the callback before anything is listening.

Access tokens are short-lived — Google's last about an hour — while a streaming
session routinely runs longer. So the engine renews tokens before each batch of
work and pushes the fresh token into each backend, rather than each backend
holding whatever token happened to be valid when it was built. A token is
considered due for renewal a minute before it expires. Without this, an evening
session would see every statistics poll fail with a `401` and the dashboard would
freeze on stale numbers — precisely the long-running case the program exists for.

A renewal failure at that point is logged rather than raised: the current token
may still work, and the request that follows will produce a far more specific
error than a pre-emptive one would.

---

## The interface and its worker

The interface is split so that no API call can ever block a keystroke.

* `ui/app.rs` holds every piece of state and handles every key. It performs no
  I/O whatsoever — a key press returns a list of `Command` values describing
  what should happen, and that is all. This is what makes the interface testable
  by driving real key events through it and asserting on the resulting state.
* `ui/worker.rs` receives those commands on a channel, does the slow work
  against the engine, and sends `Event` values back.
* `ui/draw.rs` renders state and never mutates it.

Commands include connecting, searching categories, going live, polling
statistics, and opening a URL in a browser. The last one uses a detached browser
launch: waiting for the browser process to exit would stop the worker answering
the dashboard for the rest of the session.

Two behaviours in there are worth calling out because they exist to avoid
confusing silence:

**A search that cannot be answered is answered anyway, with nothing.** When
nothing is connected yet, a category search still gets an empty reply. An empty
reply is how the interface learns the API list is unavailable and that it should
fall back to its built-in list. Staying silent instead used to leave the YouTube
category field apparently dead on a first run, with nothing on screen explaining
why.

**Stand-in results are distinguished from real ones.** The YouTube category
field falls back to a short built-in list when the API list is unavailable. That
fallback is re-filtered on every keystroke, since filtering it locally is free;
real API results are left on screen while a new search is in flight, so the list
does not flicker empty between requests. A flag on the popup records which kind
is currently displayed.

---

## Finding abandoned broadcasts

Submitting a plan a second time creates a *new* YouTube broadcast rather than
editing the previous one, so abandoned attempts accumulate. `msm cleanup` finds
them by listing `liveBroadcasts` with `mine=true` and `part=id,snippet,status`,
following YouTube's page tokens — 50 per page, with a page cap, because each
page costs quota and an unexpected reply that kept handing back a token would
otherwise loop forever.

The selection rule is deliberately pessimistic, because deleting a broadcast
that holds a recording somebody wanted cannot be undone while leaving an orphan
behind costs nothing but clutter. A broadcast qualifies only when **both** hold:

* Neither `actualStartTime` nor `actualEndTime` is present. Either one means a
  feed reached it at some point.
* Its `lifeCycleStatus` is `created` or `ready` — the two pre-live values.
  `live`, `testing`, `complete`, `revoked` and the transitional states are left
  alone.

A broadcast whose `snippet` or `status` is missing from the reply is treated as
*not* stale: there is then no evidence either way, and the safe answer is to
keep it.

---

## Where secrets go, and do not go

* `config.toml` and `tokens.json` are written with mode `0600` on Unix, set at
  open time so there is no window in which the file exists with looser
  permissions.
* Stream keys are held in memory only. They are never written to the config,
  never written to the log, and never included in `msm go --json`.
* `msm go`'s human report prints a `Key:` line saying the key is hidden and
  naming `msm key`, rather than the key itself.
* `msm streams` hides keys unless `--show-keys` is passed, because that output
  stays in terminal scrollback.
* The dashboard masks the key until <kbd>k</kbd> is pressed, and says so when
  you reveal it, since that window is often on screen while you stream.
* Passwords are never seen by the program at all: authorisation happens on the
  platform's own site and only a token comes back.

---

* [Commands](commands.md) — the user-facing surface of all this.
* [OBS and Aitum](obs-and-aitum.md) — the practical consequences of the
  stream/broadcast split.
* [Troubleshooting](troubleshooting.md) — what the resulting errors mean.
* [Back to the documentation index](README.md).
