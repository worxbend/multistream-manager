# Changelog

Everything worth knowing about between one release and the next, written for
somebody deciding whether to upgrade rather than for somebody reading the diff.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and versions follow [Semantic Versioning](https://semver.org/spec/v2.0.0.html).
Until 1.0, a minor bump may still change how something behaves; anything that
would break an existing setup is listed under **Changed** with what to do.

## [Unreleased]

### Added

- **Desktop notifications for stream events.** Raids, subscriptions, gifted
  subs, cheers, Super Chats and memberships now reach your desktop's own
  notification service, not just the Chat tab — because during a stream the
  terminal is usually behind OBS. Raids are sent as *critical*, which most
  desktops show even under do-not-disturb. Needs nothing installed in the
  common case: `notify-send`, then `gdbus`, then `kdialog`, then the terminal
  bell. Everything is switchable in **Config → Notifications** or the new
  `[notifications]` section.
- **Twitch events that never touch chat** — new followers, channel-point
  redemptions with the viewer's text, hype trains, polls and predictions —
  over a second connection (EventSub). A follow does not appear in any chat
  window, so until now this program could not see one at all.
- **Finish the broadcast** (<kbd>Space</kbd> <kbd>s</kbd> <kbd>x</kbd>). Going
  live created a YouTube broadcast and nothing could close one, so the only way
  to end a session cleanly was YouTube Studio. Asks twice, because a completed
  broadcast cannot be reopened, and deliberately does not stop OBS.
- **Twitch moderation from the chat pane.** <kbd>d</kbd> delete, <kbd>b</kbd>
  ban and <kbd>t</kbd> time out worked on YouTube and refused on Twitch; they
  now work on both, via Twitch's Helix endpoints.
- **`/raid <channel>` and `/unraid`**, the usual way a Twitch stream ends.
- **YouTube thumbnails and scheduling.** A `Thumbnail (YouTube)` field takes a
  JPEG or PNG up to 2MB, and `Start time (YouTube)` accepts `20:00`,
  `2026-08-20 20:00`, or `+2h` — scheduling ahead creates the watch page
  immediately so it can be shared and viewers can set a reminder.
- **Credentials from the environment**, for every one of them, each with its
  own `*_env` key so it can be pointed at whatever your password manager
  already uses. The OBS host and port can now come from the environment too,
  which is what makes one dotfiles repository work across machines.
- **`--version` and `--help`** as real options.
- **Advisory and licence checking in CI** (`cargo-deny`, weekly as well as on
  every push), and this changelog.

### Changed

- **Slash commands are refused rather than posted.** Typing `/ban someviewer`
  into the composer used to post those words to everybody watching: Twitch
  removed chat commands from IRC in 2023 and YouTube never had them. Anything
  beginning with a slash that this program does not recognise is now refused,
  with a note saying what to use instead. `//text` posts a leading slash on
  purpose.
- **Chat notifications no longer require the chat to be off-screen.** The old
  rule assumed you were reading chat in this program. Set
  `[notifications] only_when_hidden = true` for the previous behaviour.
- **A burst of events is paced, not dropped.** The old notifier discarded
  anything arriving within two seconds of the last one, so a raid landing in
  the middle of a gift drop was lost. They queue and release one per gap.
- **Config → Appearance's "Notifications" row is now "In-app pop-ups"**, to
  tell it apart from the new Notifications section next door.
- **Logging in again is needed for the new Twitch features.** Moderation,
  raids and events each need permissions Twitch only grants at authorisation,
  so a saved login cannot acquire them: **Config → Accounts**, log out and back
  in. Everything else keeps working meanwhile, and anything that cannot work
  says which permission it is missing.

### Fixed

- **Config → Diagnostics no longer runs its checks on every frame.** It looked
  for clipboard helpers by starting them, up to six process launches per
  redraw — twice a second at rest and ten times a second while animating, on
  the thread that has to answer keystrokes. It is a snapshot now, taken when
  the section opens, with `r` to take a fresh one.
- Running without a terminal (piped, or under a service manager) explains
  itself instead of failing with `No such device or address (os error 6)`.
- An unrecognised command-line argument exits 2 rather than 0.

## [0.1.0] — 2026-08-08

First release: configure and go live on Twitch and YouTube from one terminal,
read and answer both chats side by side, and drive OBS — scenes, audio,
streaming and recording — from a fourth tab. Fifty-seven themes, a configurable
keymap with AstroNvim-shaped defaults, an arrangeable combined view for a second
monitor, and no command line at all.

[Unreleased]: https://github.com/worxbend/multistream-manager/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/worxbend/multistream-manager/releases/tag/v0.1.0
