# msm documentation

`msm` (crate name `multistream-manager`) sets up a Twitch stream and a YouTube
live broadcast from one form, so that afterwards you press **Start Streaming**
in OBS once and go out on both. It does not control OBS, and it does not send
video anywhere itself.

It has no command line. You run `msm`, a terminal interface opens with five
tabs, and everything the program can do is somewhere in it.

The project [README](../README.md) is the short tour. These pages are the long
version: they explain *why* each piece behaves the way it does, so that when
something goes wrong you can reason about it rather than guess.

## The pages

| Page | What it covers |
|---|---|
| [Running it](running.md) | How to run it, what credentials you need and why, registering the Twitch and Google applications, where credentials go, and every environment variable. Start here for the short version. |
| [Getting started](getting-started.md) | Installing, registering a Twitch application, registering a Google OAuth client, logging in, and running your first stream. |
| [Configuration](configuration.md) | Every key in `config.toml` with its type and default, a full worked example, and the preset workflow that makes a repeat stream two keystrokes. |
| [Keys and actions](keys.md) | Every tab, every default binding, the action names for the `[keys]` section, and where each of the old subcommands now lives. |
| [OBS and Aitum](obs-and-aitum.md) | How this fits an existing OBS + Aitum multistream setup, why your stream keys stay put, and the order to do things in. |
| [How it works](how-it-works.md) | The architecture: one `StreamPlan` fanned out to platform backends, why Twitch needs one API call and YouTube needs four, and how partial success is handled. |
| [Troubleshooting](troubleshooting.md) | The errors you are realistically going to hit, each with its cause and its fix. |

## Where to start

* **Never used it before, want the short version**: [Running it](running.md).
* **Never used it before, want to be walked through it**:
  [Getting started](getting-started.md). Two steps in
  that page are the ones people lose an evening to — Google's *Test users* list
  and YouTube's 24-hour wait before a channel may stream at all — so they are
  called out in boxes rather than buried in prose.
* **Set up already, want to go faster**: [Keys and actions](keys.md) for the
  bindings and how to change them, and [Configuration](configuration.md) for
  the `[preset]` section that makes starting a stream two keystrokes.
* **Something failed**: [Troubleshooting](troubleshooting.md).
* **Curious how it is put together, or thinking of contributing**:
  [How it works](how-it-works.md).

## A note on vocabulary

Streaming APIs use several words for the same idea, and the two platforms do not
agree on any of them. These pages use the following, consistently:

| Term | Meaning here |
|---|---|
| **Stream key** | The secret string OBS sends along with your video. Anyone holding it can broadcast to your channel, so treat it like a password. |
| **Ingest URL** | The RTMP address OBS pushes the video to, e.g. `rtmp://a.rtmp.youtube.com/live2`. Not secret on its own. |
| **Broadcast** | A YouTube live event: a title, a watch page, a video id. Twitch has no equivalent — a Twitch channel is permanently there. |
| **Stream object** | YouTube's name for the RTMP pipe that holds a stream key. Separate from the broadcast, and joined to it by *binding*. |
| **Category** | What you are streaming. Twitch calls it a "game" internally and needs a numeric id; YouTube calls it a video category and also uses a numeric id. |
| **Scope** | One permission inside an OAuth login, such as "may change this channel's title". |

---

## Licence

`msm` is released under the MIT licence, and so is this documentation. The full
text is in [LICENSE](../LICENSE) at the root of the repository.

In practice that means you may use, modify and redistribute it, including in a
commercial product, provided the copyright notice and the licence text travel
with it. There is no warranty.
