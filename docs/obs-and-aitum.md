# OBS and Aitum

`msm` does not control OBS, does not send video anywhere, and does not know
whether OBS is even installed. It configures the two platforms so that when you
press **Start Streaming** yourself, the stream that goes out is correctly
titled, categorised and tagged on both.

That division is deliberate. You keep control of the moment you actually go
live, and nothing in your encoder setup is ever touched by a tool that has no
business touching it.

**Contents**

* [The setup this assumes](#the-setup-this-assumes)
* [Your OBS configuration never changes](#your-obs-configuration-never-changes)
* [Why the YouTube stream key stays the same](#why-the-youtube-stream-key-stays-the-same)
* [What happens when the key cannot be reused](#what-happens-when-the-key-cannot-be-reused)
* [Pinning a specific key](#pinning-a-specific-key)
* [The recommended workflow](#the-recommended-workflow)
* [First-time setup, in order](#first-time-setup-in-order)
* [Things that do not work the way you might expect](#things-that-do-not-work-the-way-you-might-expect)

---

## The setup this assumes

One OBS instance sending the same video to two platforms at once, using the
[Aitum multistream plugin](https://aitum.tv/vertical) — or any other plugin or
service that takes one encode and fans it out to several RTMP destinations.

Concretely that usually means:

| Where | Holds |
|---|---|
| **OBS → Settings → Stream** | Twitch, with your Twitch stream key. |
| **Aitum multistream, destination 2** | YouTube's RTMP ingest URL and your YouTube stream key. |

Which platform lives in OBS's own settings and which lives in the plugin does
not matter to `msm`. What matters is that both are configured once and then left
alone.

---

## Your OBS configuration never changes

Nothing `msm` does writes to an OBS configuration file, connects to the OBS
WebSocket, or asks OBS to do anything. It talks to the Twitch and YouTube APIs
and to nothing else on your machine.

So the ingest URLs and stream keys you paste into OBS and Aitum on day one stay
correct indefinitely — provided the platforms keep handing out the same keys.
Twitch does that on its own. YouTube does not, unless you ask it to, and making
sure it does is most of what the YouTube half of this tool is being careful
about.

### Twitch

A Twitch channel is permanently there. Its stream key belongs to the channel, is
stable, and going live means no more than pointing OBS at it. `msm` changes the
channel's title, category, language and tags, which has no effect on the key.

You can copy the key to your clipboard at any time with <kbd>y</kbd> on the
Stream Info tab. It is copied, never displayed — the value goes from the API
straight to the clipboard, so nothing that could end up on screen or in a
recording ever holds it.

### YouTube

YouTube models a live stream as **two separate objects**:

* A **broadcast** — the event. It has a title, a description, a scheduled start
  time and a watch page, and it is also a video with a video id. A new one is
  created for every stream.
* A **stream** — the pipe. It holds the RTMP ingest URL and the stream key that
  OBS actually pushes bytes into. It exists independently of any broadcast.

They are joined by a **bind** operation, which tells a broadcast which pipe
feeds it. This separation is exactly what makes a stable key possible: the
broadcast is new every time, and the pipe is not.

---

## Why the YouTube stream key stays the same

Every call to YouTube's `liveStreams.insert` mints a **brand new stream key**.
A tool that created a stream object per broadcast would therefore hand you a
different key before every single session, and you would have to paste it into
Aitum each time — which is precisely the chore this whole program exists to
remove.

So `msm` does not create one. With `reuse_stream = true` (the default) it looks
for a stream that already exists on your channel and binds the new broadcast to
that. The key is untouched, and your encoder configuration goes on working.

The dashboard says which branch was taken, in words, so you are never left
guessing:

> *Reused your existing stream key "Default stream key" — OBS and Aitum need no changes.*

or, when you have pinned one:

> *Bound to your pinned stream key "Default stream key" — your OBS settings are unchanged.*

There is one thing to understand about how the candidate is chosen. YouTube
marks streams as reusable or not, but that flag lives in a part of the stream
resource (`contentDetails`) that the list endpoint does not support — asking for
it makes the request invalid, so the flag cannot be read before choosing. Rather
than guess, `msm` picks a candidate without it and lets the **bind** decide:
binding a stream that cannot be reused fails, and that failure is handled.

---

## What happens when the key cannot be reused

If the bind fails — the usual reason is that the stream found belongs to a
single past broadcast and cannot be bound again — `msm` does not abandon the
go-live. It creates a fresh stream, binds that instead, and tells you plainly:

> *Your existing stream key could not be reused, so a new one was created. Copy
> the stream key below into OBS (or Aitum) — it will be reused from now on.*

This is the one case where you have to act. Press <kbd>k</kbd> on the dashboard
to reveal the key, paste it into Aitum's YouTube destination, and you are back
to a stable setup: the newly created stream is a reusable one, so from the next
session onwards it will be found and bound like any other.

The same message appears when the channel had no stream at all, which is what
you see on a brand new channel.

---

## Pinning a specific key

If your channel has more than one stream key — an old one from YouTube Studio,
plus one this tool created, say — then which one gets bound depends on the order
YouTube lists them in, and that ordering is not yours to control. The key you
have configured in Aitum might not be the one the broadcast is bound to, and the
symptom is a stream that appears to go nowhere.

List them with **Config → Housekeeping → *List YouTube stream keys***
(<kbd>Alt</kbd>+<kbd>5</kbd>). The results go to the activity log:

```
ID                         PINNED  TITLE
Vy8dQ...oqA                        Default stream key
9tRk2...LmX                        multistream-manager (reusable)
```

Then pin the one Aitum is configured for:

```toml
[youtube]
stream_id = "Vy8dQ...oqA"
```

From then on every broadcast binds to that stream and nothing else. If the id
stops existing — you deleted the stream in Studio, for instance — that listing
warns you about it and going live fails with a clear message rather than
silently binding something else.

Only ids are ever listed; a key itself is never shown anywhere in the program,
because this window is often part of the broadcast. To get the key of the stream
your current broadcast is actually bound to, press <kbd>Y</kbd> on the Stream
Info tab after going live and paste it wherever you need it.

---

## The recommended workflow

Do it in this order every time:

1. **`msm`** — open the interface.
2. **Pick platforms**, <kbd>Enter</kbd> to connect. The account each login
   resolved to is printed, so a wrong account is caught before anything changes.
3. **Fill in the form**, or accept what your preset already says.
4. **<kbd>Ctrl</kbd>+<kbd>G</kbd>** — both platforms are configured in parallel.
   Wait for the panels to say ready.
5. **Check the YouTube note.** In the normal case it says your key was reused
   and nothing needs doing. In the rare case it says a new key was created,
   paste it into Aitum now, before step 6.
6. **Press Start Streaming in OBS.**

With `youtube_auto_start = true` (the default) YouTube flips the broadcast live
by itself the moment it sees the feed, so step 6 is the last thing you do. With
it off, you must also press **Go live** in YouTube Studio once OBS has
connected.

The dashboard then shows viewers, followers, subscribers, likes and uptime for
both platforms side by side, refreshed on a timer, so neither website needs to
be open. Press <kbd>o</kbd> to open the watch page in a browser when you want to
see what viewers see.

When your `[preset]` already says what you usually stream, steps 2 to 4 collapse
into pressing <kbd>Ctrl</kbd>+<kbd>G</kbd> on a form that is already filled in.

---

## First-time setup, in order

The very first time, there is a chicken-and-egg problem: YouTube has no stream
key until something creates one, and Aitum needs a key before it can be
configured. Resolve it like this:

1. Make sure live streaming is enabled on the channel, and remember that the
   first activation takes 24 hours — see
   [Getting started](getting-started.md#trap-2-a-channel-that-has-never-streamed-cannot-stream-for-24-hours).
2. Set up credentials and log in, as in
   [Getting started](getting-started.md).
3. Open **Config → Housekeeping → *List YouTube stream keys***. If it lists
   nothing, the channel has no stream key yet.
4. Submit the form once with <kbd>Ctrl</kbd>+<kbd>G</kbd>. This creates a
   reusable stream and tells you a new key was made.
5. Copy the key with <kbd>Y</kbd> on the Stream Info tab and paste it, with the
   ingest URL, into Aitum's YouTube destination.
6. Optionally pin it with `stream_id`, so nothing else can ever be chosen.

From session two onwards, none of this recurs.

---

## Things that do not work the way you might expect

**Resubmitting creates a new YouTube broadcast.** Pressing <kbd>e</kbd> on the
dashboard, changing the title and going live again updates your Twitch channel
in place, but on YouTube it creates a *second* broadcast with a *new* watch URL.
That is how YouTube's API works — a broadcast is an event, not a mutable
setting. The first one is left behind unstarted; **Config → Housekeeping →
*Find abandoned broadcasts*** finds and removes those — the first
<kbd>Enter</kbd> lists them and a second deletes the ones listed. See
[Keys and actions](keys.md#housekeeping).

**The watch URL changes between sessions.** Each YouTube broadcast is a distinct
video with its own id, so any link you shared for last week's stream points at
last week's stream. The Twitch watch URL is your channel page and never changes.

**`msm` cannot tell whether OBS is running.** The "live" indicator on the
dashboard comes from the platforms: Twitch reports an active stream, and YouTube
reports an actual start time on the broadcast. If OBS is not sending anything,
neither platform will say you are live, which is the honest answer rather than a
guess.

**Nothing is rolled back if one platform fails.** If YouTube fails and Twitch
succeeds, Twitch is genuinely configured and ready, and you can stream to it
immediately. See [How it works](how-it-works.md#partial-success).

---

* [Getting started](getting-started.md) — from nothing to a first stream.
* [Configuration](configuration.md#youtube) — `reuse_stream` and `stream_id`.
* [How it works](how-it-works.md) — the API calls behind all of this.
* [Troubleshooting](troubleshooting.md) — when a stream key changes on you.
* [Back to the documentation index](README.md).
