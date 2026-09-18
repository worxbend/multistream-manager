# Changelog

Everything worth knowing about between one release and the next, written for
somebody deciding whether to upgrade rather than for somebody reading the diff.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and versions follow [Semantic Versioning](https://semver.org/spec/v2.0.0.html).
Until 1.0, a minor bump may still change how something behaves; anything that
would break an existing setup is listed under **Changed** with what to do.

## [Unreleased]

## [0.3.0] — 2026-09-18

### Added

- **The OAuth redirect port is now editable on the Setup screen** (`[general]
  oauth_port`). Previously the only way to work around a port conflict during
  first-time login was to hand-edit `config.toml`, which contradicted the
  form's own promise that a fresh install never needs that. The form now
  shows a live preview of the exact redirect URL as you type the port, and
  validates it on save.
- **A pre-flight check before going live.** The go-live key now shows a
  checklist instead of launching straight in: each platform's credentials and
  login, whether a token expires soon *with no refresh token behind it*,
  whether the login predates a permission a newer feature needs, the stream
  details, the selected OBS scene, free disk — and **which OBS audio inputs
  are muted**. Streaming for forty minutes on a muted microphone is the
  classic solo-streamer disaster, and every one of those facts was already
  known to this program and kept to itself until something failed.
  <kbd>Enter</kbd> then writes the metadata to both platforms *and* starts OBS
  streaming, using an explicit start rather than a toggle so a stream you had
  already begun by hand is left running.
- **A health strip in the header, on every tab.** `TW ● live 142 · YT ● live
  38 · OBS ● 6100 kb/s drop 0.2% · 1:23:04`. The colour is computed, not
  decorative: amber for a failed poll (the numbers are older than they look,
  which is not the same as a dead stream) or output-dropped frames over 1%,
  red for the case nothing else on screen reports — OBS still sending while
  the platform has stopped receiving. Every state has its own glyph too, so it
  survives a monochrome terminal.
- **`chat.close`** (<kbd>&lt;Leader&gt;</kbd> <kbd>c</kbd> <kbd>x</kbd>).
  Joining a chat was one keypress and closing one could not be done at all —
  the code existed and no keystroke could reach it.
- **A real text box for chat.** The composer supported appending a character
  and deleting the last one, and nothing else. It now has arrows, Home/End,
  Delete, Ctrl+W and Ctrl+U, plus <kbd>↑</kbd>/<kbd>↓</kbd> through what you
  have already sent in that chat. One Backspace removes a whole emoji,
  including multi-part ones like flags and families.
- **Config → Chat**, with the switch for chat logging — which previously
  could only be turned on by editing config.toml, while Housekeeping's
  paid-event export read the very logs it produces. The section also reports
  the day's YouTube quota estimate and what the log rotation has produced.
- **A test-notification row** in Config → Notifications, and separate switches
  for polls and predictions.
- **Fine volume control on the OBS tab** (<kbd>]</kbd> / <kbd>[</kbd>, 1%),
  for settling on a level rather than finding one.
- **Pasting**, in the metadata form, the chat composer and the credential
  boxes. There was none at all before — a 5000-character description had to be
  retyped, and a client secret is forty random characters nobody types by hand.
- **Pinning a YouTube stream id from the interface.** Housekeeping's listing
  wrote the ids to the log and stopped there; each is now a row you press
  <kbd>Enter</kbd> on. This was the last thing in the program that could only
  be done in a text editor.
- **Replies, cheers and subscription events are drawn.** Twitch reply
  threading was parsed into the message and never rendered, so a reply read as
  an unprompted remark; cheers arrived as ordinary text while YouTube Super
  Chats got a solid chip. Both now show. A message that names you gets an
  accent gutter — the app already worked that out for the mentions filter and
  used it only to hide *other* messages.
- **Streamer mode** (`[appearance] streamer_mode`), borrowed from Chatterino.
  Hides the client ids on the setup screen and the file paths in Config →
  Files whenever OBS reports streaming or recording, for the moment nobody
  plans for: tabbing to a settings screen mid-broadcast. The header says when
  it is on.
- **Highlight rules** (`[[chat.highlight]]`), borrowed from Chatterino's
  separation of highlights from filters. The digit filters *exclude*; these
  *promote*, so "look at this one" can mean something other than your own
  name — a keyword, a moderator speaking, anything paid. With an ignore list
  checked first, so your own bot never lights up the pane. No regular
  expressions, deliberately: phrase, word, user, badge and event cover what
  people write, and a regex engine is a real dependency cost.
- **Twitch stream markers** (`<Leader>cm`, or `/marker [note]`), borrowed from
  Streamer.bot. A bookmark in the VOD, so a moment worth clipping later can be
  found without scrubbing through four hours. Needs no new permission — the
  token already carries the one it uses.
- **Named stream profiles** (`[profile.<name>]`, `<Leader>sp`), borrowed from
  Restream's stream groups and Castr's destination sets. One set of settings
  per kind of stream, instead of retyping the title, tags and both categories
  every time you switch. `[preset]` stays the unnamed default, so existing
  configs are untouched.
- **Twitch tells the program when the stream starts and stops**
  (`stream.online` / `stream.offline`). Going live was learned only from a
  fifteen-second poll, and it is the state change that gates notifications,
  uptime and the health strip. Neither subscription needs a scope.
- **YouTube broadcasts are created with low latency** rather than the default
  30-60 seconds glass-to-glass, which made answering chat a reply to something
  a minute old.
- **Save the OBS replay buffer** (`<Leader>ob`) — the most-pressed OBS hotkey
  a live streamer has, and it was on no key here.
- **Filter the Keys listing and the theme picker** by typing. 110 bindings and
  58 themes were each walked one `j` at a time, with the letters dropped.
- **Open the files in Config → Files** with <kbd>Enter</kbd>.
- **Undo, row resizing and a cursor in the layout editor** — `u`, `>`/`<`, and
  the selected panel outlined in the preview.
- **`[keys.config]`**: the Config tab's keys are named, rebindable actions
  instead of hardcoded matches, so they appear in which-key, `<Leader>?` and
  the palette like every other tab's.
- **<kbd>Ctrl+A</kbd> on the form applies the title, category, tags and
  language to Twitch right now, without going live.** Ctrl+G was previously
  the only way anything typed here ever reached a platform. Twitch, unlike
  YouTube, has no separate "create a broadcast" step — the channel is always
  there, and updating it is a completely independent call from starting the
  feed in OBS — so there was no way to set a title and category up ahead of
  time, or fix one mid-stream, without going live again for real. YouTube
  says plainly that it does not support this yet, rather than pretending to.
- **A modernised visual layer**, built on the existing theme system rather
  than beside it: focused-panel borders and titles now follow one shared
  rule everywhere more than one panel can be on screen at once; status text
  (`READY`/`FAILED`, connection state, OBS's `STREAM`/`RECORD`) reads as
  compact colour badges instead of bare bold text; usage values (OBS's cpu
  meter) are graded on a consistent green-to-red scale; chat gains a subtle
  per-sender colour gutter and breathing room between senders, and a
  previously-untracked sent-but-unconfirmed message now actually renders
  instead of silently vanishing until the real event arrives; the OBS
  scene/audio lists and the Config tab's Accounts list moved onto a real
  scrolling-list widget; and the dashboard shows a genuine per-platform
  viewer-share pie chart when more than one platform is live, built only
  from numbers already being tracked.

### Fixed

- **Turning pop-ups off silenced every error.** `appearance.toasts = false`
  suppressed all notifications, and the activity log is only drawn on the
  Stream Info tab — so a failed go-live while you were reading chat produced
  nothing at all. The setting now means "no *routine* pop-ups"; problems are
  always shown.
- **Twitch bans tombstoned nothing on YouTube.** A YouTube ban emitted the
  banned viewer's display name where the matcher wanted the channel id, so
  banning reported success and left every message they had written on screen.
- **Turning Twitch events off and on again deafened the app** for the rest of
  the session.
- **A dropped command left the interface permanently busy** — every later
  go-live, login and end-stream silently refused with "already working" until
  restart.
- **The statistics polling spent YouTube quota nobody was counting.** At the
  default interval that is roughly 5,700 units a day against a default
  10,000-unit project, invisible to the reserve that exists to keep message
  sending working when reading has to stop.
- **Clicking "4 OBS" or "5 Config" did nothing.** The tab bar drew five labels
  and the mouse hit-testing was built from a separate copy that listed three.
- **The Config tab's footer advertised another tab's keys.**
- **The emoji picker could only ever insert its first match**, though it drew
  a list; and the layout editor's preset key was not a cycle.
- **Twitch tags could be set but never cleared.**
- **Retry ladders that did not climb.** One successful poll wiped YouTube's
  backoff; Twitch reset its reconnect ladder on any accepted connection, so a
  flapping connection retried every two seconds indefinitely; and Twitch
  reconnects had no jitter.
- **A failed save of renewed tokens reported success**, and logging out did an
  unlocked read-modify-write a concurrent refresh could undo.
- **`msm.log` grew without bound**, and a failed logging init was discarded so
  "check msm.log" pointed at a file nothing was writing.
- **A config file that could not be parsed was replaced without a backup.**
  It is now kept as `config.toml.bak`.
- **Diagnostics went stale** — it never retook its snapshot after a login or
  an OBS connection — reported credential *presence* when the documented
  footgun is a question of *source*, and could not be scrolled.
- **Cleanup deleted a list you had never seen.** The confirming press
  re-fetched the abandoned broadcasts and deleted *those*, so one created
  between the two presses — the broadcast you had just set up for tonight —
  went with them. It now deletes only what it showed you, the armed
  confirmation no longer survives navigating away, and the row says which of
  the two things the next press will do.
- **The command palette could run the wrong action.** It replayed a chord
  found in any context, and a bare `j` is `chat.scroll_down`, `obs.down` *and*
  `config.next_section` — so choosing "Scroll forward" from the OBS tab moved
  the scene cursor.
- **Volume up could turn a source down**, on any source OBS holds above unity.
- **`q` did not quit on the Config tab**, though every footer there said it
  did; and adding a panel to a one-panel layout silently did nothing while
  reporting success.
- **The OBS failure reason was never readable** — it was sent and overwritten
  in the same instant, so the pane only ever said "reconnecting". A wrong
  password now also stops retrying after three refusals instead of looping
  forever.
- **The login screen showed nothing while logging in**, including the fallback
  URL that is the only way through on a headless machine.
- **Logging out asked nothing and reported success before the disk write.** It
  now confirms, and reports what actually happened.
- **`<Leader>?` clipped most of the bindings it promised**, and closed on the
  key you pressed to see more. It scrolls.
- **The message history cut off long errors** — the thing it exists to
  preserve. Entries wrap.
- **The metadata form could not scroll**, so on a terminal near 24 rows the
  last fields were unreachable while the cursor still walked onto them.
- **Keys could not round-trip**: `<Insert>`, `<S-F5>` and `<lt>` were written
  back in forms the parser rejects.
- **Broadcast listing and deletion spent YouTube quota nothing counted** — 50
  units per delete. The listing now says what confirming will cost.
- **Extra chat accounts were invisible and could never be removed**, so one
  added by mistake kept a valid refresh token on disk forever.
- **A revoked token left the whole interface saying "logged in"** — the flag
  was a start-up snapshot nothing corrected.
- **The wheel scrolled the focused pane, not the one under the pointer**,
  silently dropping a reply armed in the other one.
- **Volume showed `80%` while OBS showed `-2.0 dB`**; the dB figure was
  fetched, stored and read by nothing. Same for the path of a finished
  recording.
- **The cleanup listing stopped at 500 broadcasts silently**, so the job
  looked finished when it was not.
- **The paid-event export blocked every other command** while it read the
  chat logs.
- **`ListStreams` was the only job that needed a live connection**, so the one
  most needed during first-run setup was the only one that refused then.
- **Tags were silently rewritten** — cut at 25 characters, or stripped to
  nothing — with only the count validated.
- **`~` in the thumbnail path was taken literally**, failing at submit time
  with "there is no file at ~/pics/thumb.png".
- **A flapping connection buried the message history** under identical retry
  lines, which a bounded history then evicted the real error from.
- **Search left you to find the match yourself**, pinned it to the bottom row
  and gave no count; and nothing divided what you had read from what arrived
  while you were away.
- **The activity view looked back two minutes** on a busy chat, for the pane
  that is the "who paid me tonight" list.
- **A click did not skip the splash**, though it says "press any key".
- **Diagnostics graded an expired login green** — `[ ok ] token valid for
  expired`.
- **Unbinding a chord not bound in that context did nothing, silently**, and a
  mistyped action name got no suggestion; a prefix binding could bury a whole
  group with no warning.
- **Backspace threw away a whole part-typed chord** instead of stepping back
  one key, and Esc on the Config tab jumped to another tab instead of stepping
  back to the section list.
- **The OBS status poll ran three requests a second forever**, including
  overnight on an idle machine.
- **The very first TLS connection of the session could crash the program**
  outright, or leave whichever background task hit it first — usually Twitch
  chat — stuck on "connecting" forever with no explanation. `reqwest` and
  `tokio-tungstenite`/`twitch-irc` each linked in a different default `rustls`
  crypto backend, and with two present `rustls` had no way to pick one. A
  provider is now chosen explicitly, once, before anything can need one.
- **A category search that landed while a platform was mid-(re)connect came
  back "no matches"** with no way to tell that apart from Twitch genuinely
  having no such category — typing an exact, real category name and getting
  refused looked like a broken search. It now says which platform was not
  connected and what you typed.
- **A command sent to OBS after it had given up on repeated authentication
  failures vanished with no feedback**, the one place in that file that
  didn't already report a command it couldn't run.
- **Deleting or banning a notice/system row, or a not-yet-confirmed local
  echo, on YouTube did nothing** with no explanation — unlike the equivalent
  Twitch path, which always reaches Helix and reports a real refusal.
- **Arming a moderation action with nothing selected, replying to or timing
  out a non-message row, and reconnecting or joining a chat with none
  open or no logged-in account, all silently did nothing.** Each now says so;
  arming a moderation action and then pressing a key that hits one of these
  no longer leaves the previous arm stuck either.
- **Selecting a scene, toggling mute, or nudging the volume before OBS
  connected silently did nothing** — the same gap `obs_command`'s own warning
  already covered elsewhere in the same screen.
- **An unrecognised `animations` value or a dangling `active_profile` name
  fell back to the default with no trace anywhere that it had happened.**

### Changed

- **The command palette is built from the action set** rather than a
  hand-written list of 31 entries. Most of the OBS actions, chat scrolling,
  account cycling and everything on the Config tab were previously in no list
  anywhere, which quietly broke the palette's whole promise. Matching now
  ranks prefix matches above mid-word ones.
- **Category autocomplete spends one search per word, not one per keystroke.**
  Typing "Baldur's Gate 3" was fifteen Helix calls, fourteen discarded.
- **Chat paging follows the height of the pane** rather than a fixed ten
  lines.
- **An OBS shortcut that collides with a binding is reported.** Such a
  shortcut never fires — the keymap is resolved first, deliberately — and
  nothing said so. docs/configuration.md described the precedence backwards.

## [0.2.0] — 2026-08-18

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

[Unreleased]: https://github.com/worxbend/multistream-manager/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/worxbend/multistream-manager/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/worxbend/multistream-manager/releases/tag/v0.1.0
