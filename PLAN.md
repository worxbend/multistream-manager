# PLAN — Multi-account Twitch/YouTube chat integration

Phase 0 reconnaissance output. This document is the contract for the port: the
parity matrix below is updated as rows land, and any row that is ultimately not
ported carries a written justification.

Sources of truth (behavioral authority, read exhaustively at these commits):

- `../twi` — Twitch chat TUI (Go, Bubble Tea, go-twitch-irc v4.4.1),
  commit `7c6ad6bbbc3dec1b6af2ddbd03b55f96c0c162cf`, ~50.6k lines.
- `../yc` — YouTube live chat client (Go, Bubble Tea, official Data API v3),
  commit `9e67efd10c0790ec22df2c944bcee6be1bc37cf8`, ~65.5k lines.

## 1. Current architecture of multistream-manager

- Rust 2021, MSRV 1.88. `tokio` (multi-thread) + `ratatui` 0.30 + `crossterm`
  0.29 (event-stream). HTTP: `reqwest` 0.12 (rustls). Errors: `anyhow` with
  user-facing `.context()` prose; `thiserror` available. Logging: `tracing` to
  `msm.log` (stdout belongs to the TUI).
- Layering: `model.rs` (pure domain) → `backend.rs` (`Backend` trait, hand-rolled
  `BoxFuture`) → `twitch.rs`/`youtube.rs` (hand-rolled Helix / Data API v3
  clients) → `engine.rs` (per-platform fan-out, partial success) → `main.rs`
  (clap CLI) and `ui/` (TUI).
- TUI is Elm-ish: `ui/mod.rs` owns the terminal + `tokio::select!` event loop;
  `ui/worker.rs` is a single task owning the `Engine` (commands in via bounded
  mpsc(32), events out via unbounded mpsc); `ui/app.rs` is pure state with zero
  `await` (`handle_key -> Vec<Command>`, `handle_event`); `ui/draw.rs` is a pure
  render of `&App`. Screens: `Platforms` → `Form` → `Dashboard`. No tab concept
  yet.
- Auth: authorization-code + PKCE over a loopback listener (`auth/oauth.rs`),
  tokens in `tokens.json` (`BTreeMap<String, TokenSet>` keyed by platform slug,
  0600, atomic rename, advisory-locked read-modify-write via `StoreLock`).
  Config: `config.toml` (real TOML via serde + `toml_edit` for comment-preserving
  saves). Current Twitch scopes: `channel:manage:broadcast`,
  `channel:read:stream_key`, `moderator:read:followers`,
  `channel:read:subscriptions`. YouTube scope: `auth/youtube` (full — subsumes
  `force-ssl`, so chat send is already permitted).
- Login flow the empty states must reference accurately: `msm login twitch`,
  `msm login youtube` (browser OAuth), credentials configured in `config.toml`
  (`msm init` writes a commented starter; `msm status` shows state).

## 2. Transport strategy

### Twitch — `twitch-irc` crate (v6) over rustls

twi itself is app logic layered on a library (gempir/go-twitch-irc); the port
mirrors that shape: app-level behavior ported from twi, transport from the
maintained `twitch-irc` crate (tokio-native, rustls features, cap negotiation
for `twitch.tv/tags`/`commands`/`membership`, typed IRCv3 tag parsing,
reconnection with rejoin, join/send rate limiting, `LoginCredentials` trait that
plugs into our token store). Endpoint `irc.chat.twitch.tv:6697` (TLS). IRC is
"supported legacy" as of 2026 (no sunset announced; Twitch recommends EventSub) —
the `ChatSource` seam keeps the transport swappable.

Ported *on top* of the crate, from twi (provenance comments per module):

- app-level reconnect backoff (2s doubling to 60s, 10 attempts, manual retry),
- send limits: 20 msgs/30s sliding window + 30s duplicate suppression,
- send sanitizing (CR/LF→space, C0 dropped, 500-char cap, ACTION framing),
- local echo from USERSTATE identity, reply threading via
  `reply-parent-msg-id`, `/me`, event normalization (USERNOTICE msg-ids,
  CLEARCHAT/CLEARMSG semantics: mark deleted, never reprint).

New scopes requested at Twitch login: `chat:read chat:edit` (+ existing four).
Existing logins lack them; the chat pane surfaces "re-run `msm login twitch`"
when the token misses a scope (capability disabled with a reason, never hidden).

### YouTube — hand-rolled `reqwest` poller (mirrors yc exactly)

yc uses only the official Data API v3 `liveChatMessages.list` (`streamList` is
documented in HTML but absent from the discovery document; yc deliberately does
not use it — we follow the behavioral authority, transport factored so it could
be substituted). This repo already hand-rolls the same API with the same
`reqwest` client and token store, so no crate is added. Port of yc's poller:

- states idle/resolving/priming/streaming/backoff/ended/offline/quota-paused,
- `pollingIntervalMillis` as an **absolute floor** (±10% jitter above it, never
  below), config floor/ceiling,
- backoff ladder: quota exceeded → hard pause (never retried; Pacific-midnight
  reset), stale page token → one re-prime per session, transient → ×2 climb
  capped 60s (120s for rate limit), success decays one step,
- offline grace: 2 minutes of continued polling after `offlineAt`, restarted by
  any message,
- 8000-entry message-ID dedupe ring; retained page token never cleared by an
  empty `nextPageToken`,
- resolve ladder (cheapest first): live-chat id → `videos.list` (1u) →
  `channels.list` by handle/id (1u); `search.list` never called,
- send via `liveChatMessages.insert` with a local token bucket (burst 3 / 2s),
  200-**grapheme** cap, local echo reconciled by returned message id,
- every dispatched request charged to a local quota ledger (including failures),
  quota state surfaced in the connection indicator.

Accepted chat-target forms (ported from yc `ParseChatTarget`): `livechat:` id,
bare live-chat id, 11-char video id, `UC…` channel id, `@handle`, bare word as
handle, and the youtube.com/youtu.be URL family. This answers the spec question
"video ID or channel": **both, plus handles and URLs** — whatever yc accepts.

## 3. Seam and concurrency design

- `src/chat/mod.rs` — normalized model. One `ChatMessage` (id, timestamp,
  author, text fragments, message type, deleted/historical/local-echo flags)
  with `meta: Option<PlatformMeta>` (`Twitch(TwitchMeta)` /
  `YouTube(YouTubeMeta)`) for platform extras (badges raw, bits, Super Chat
  amount/tier, membership details…). Events: `ChatEvent::{Message, Connection,
  Moderation, Room}`; `ConnectionStatus::{Connecting, Connected, Reconnecting,
  Disconnected, Failed, QuotaPaused, Closed}`.
- `ChatSource` trait: `connect`, `disconnect`, `join`, `leave`, `send` — but
  dispatch is a closed **enum** `ChatBackend { Twitch(..), YouTube(..) }`
  (two variants, never user-extensible; enum dispatch avoids `Box<dyn>` object
  lifetimes around `&mut self` async methods and matches `backend.rs` precedent
  of a closed platform set).
- Each open chat runs one tokio task; events flow over an **unbounded** mpsc to
  the UI (same pattern as `ui/worker.rs` events) with the twi/yc invariant
  adapted: the render loop drains non-blockingly per frame; per-chat ring buffer
  bounds memory (default 1000 per the task spec; config `chat.scrollback_limit`).
  Task shutdown is deterministic: dropping the command sender + a
  `tokio_util::sync::CancellationToken`-free design — each task selects on its
  command channel; closing it ends the task (repo precedent: worker shutdown by
  sender drop).
- Lazy connection: a chat connects when its sub-tab first becomes visible;
  hidden chats keep their task and buffer (cheap: messages are just appended to
  a ring) but YouTube pollers of *hidden* chats stretch to the config ceiling to
  conserve quota. Justification: killing hidden connections would re-pay the
  resolve/prime quota on every tab switch (yc's own reasoning for in-place
  reconnect), while Twitch IRC joins are effectively free.

## 4. Accounts and config schema

- Token store (`tokens.json`) already maps arbitrary string keys → `TokenSet`.
  Primary accounts stay under `twitch` / `youtube` (fully backward compatible).
  Additional chat accounts are stored under `twitch:<login>` /
  `youtube:<channel-id>`; `msm login twitch --add` (and `youtube --add`) runs
  the same OAuth flow and keys the result by the identity resolved from
  `/validate` (Twitch) or `channels.list?mine=true` (YouTube).
- `[chat]` config table (serde `#[serde(default)]`, so old files load
  unchanged): `scrollback_limit` (1000), `poll_interval_floor_ms` (1000),
  `poll_interval_ceiling_ms` (0 = none), `quota_reserve_percent` (10),
  `daily_quota_units` (10000), `badge_mode` (`glyph`), `message_layout`
  (`inline`), `highlight_emotes` (true), `timestamps` (true).
- Chat accounts are discovered from token-store keys; no config migration is
  needed (additive keys only). Identity (login/display name) is cached in a new
  optional `identity` field on `TokenSet` (`#[serde(default)]` — old files
  deserialize with `None`).

## 5. UI structure

- Top level: `Tab::{StreamInfo, Chat, Combined}` in `ui/app.rs`; `alt+1`/
  `alt+2`/`alt+3`
  switch (both Go apps use alt+digit because terminals can't distinguish
  `ctrl+1` from `1`; matches ported muscle memory and avoids every existing
  binding). Tab bar rendered as the first line.
- Stream Info tab = existing screens untouched. (An earlier design had an
  empty state here naming the commands to run when nothing was configured;
  the Setup and Login screens below replaced it, since asking for the values
  beats telling someone which file to edit.)
- Before the streaming flow, two host screens added after the port (not twi/yc
  features): `Screen::Setup` (API credentials typed in-app, secrets masked,
  saved through the comment-preserving `Config::save`) and `Screen::Login`
  (browser OAuth run by the worker, progress routed to the activity log via
  `auth::login_with`, never printed). `App::new` picks the opening screen from
  what is configured and which logins exist. A finished login connects and
  opens the dashboard, which shows current channel state before any go-live.
  Stream keys are copy-only (`y`/`Y` → `clipboard::copy`, helper program or
  OSC 52); the former reveal toggle is gone.
- Appearance surfaces, all host-owned and all reachable without the config
  file: `ctrl+p` command palette (`ui/command_palette.rs`, replays key events),
  `ctrl+t` theme picker (`ui/theme_picker.rs`, live preview over the whole
  screen), `alt+m` message history (`ui/toast.rs`, modal), `alt+a` animation
  mode, `alt+t` telemetry. The start-up splash (`ui/splash.rs`) covers
  everything until it expires or any key skips it, and swallows that key.
- Combined tab: `draw_combined` puts a seven-line channel-state strip above
  the same chat split; `alt+w` moves the keyboard between the halves because
  both want the same letters (`r`, `y`, `i`).
- Chat tab: left pane Twitch, right pane YouTube (both always present; per-pane
  empty state mirrors the hints + `msm login <platform> --add` for more
  accounts). Per-pane sub-tab row: one per account of that platform. Activating
  an account sub-tab opens + connects that account's **own** chat (Twitch: own
  channel; YouTube: own current broadcast via the resolve ladder). `space c`
  (twi/yc chord) opens a join prompt for an arbitrary channel/target; `[`/`]`
  cycle chats within a pane; `tab` moves focus between panes/composer; `i`
  focuses composer, `esc` returns; `j`/`k`/`pgup`/`pgdn` scroll; `ctrl+r`
  reconnect. Connection state is always visible in each pane's header.

## 6. Feature-parity matrix

Status legend: **ported** (Rust module listed) · **planned** (ordered backlog)
· **not ported** (justification given — mostly host-owned concerns per the
"standalone-app trappings" analyses in both inventories).

### Twitch (`../twi`)

| # | Feature (twi source) | Status → Rust module |
|---|---|---|
| T1 | IRC TLS transport, caps tags/commands/membership, PING/PONG (internal/twitch/irc_client.go) | ported → `chat/twitch.rs` via `twitch-irc` crate |
| T2 | IRCv3 tag parsing incl. escaping (go-twitch-irc) | ported → `twitch-irc` crate (typed messages) |
| T3 | PRIVMSG normalization: badges, badge-info, color, display-name, ids, tmi-sent-ts, emotes ranges, bits, first-msg | ported → `chat/twitch.rs::normalize_privmsg` |
| T4 | USERNOTICE (sub/resub/raid/…; system-msg text, msg-id labels) | ported → `chat/twitch.rs` |
| T5 | CLEARCHAT (clear / ban / timeout → mark author's messages deleted) | ported → `chat/twitch.rs` + `chat/state.rs` |
| T6 | CLEARMSG (single message deleted; text never reprinted) | ported → same |
| T7 | NOTICE incl. auth-failure classification | ported → `chat/twitch.rs` |
| T8 | ROOMSTATE / USERSTATE (self badges/color for local echo) | ported → `chat/twitch.rs` |
| T9 | JOIN/PART membership events | ported → `chat/twitch.rs` (join/part logged as events) |
| T10 | Reconnect: transport auto + app backoff 2s→60s ×10 + manual ctrl+r + auth-refresh retry | ported → crate + `chat/task.rs` backoff |
| T11 | Send: PRIVMSG, reply-parent, /me ACTION, sanitize CR/LF/C0, 500-rune cap | ported → `chat/twitch.rs::send` |
| T12 | Send rate limit 20/30s + duplicate suppression 30s | ported → `chat/ratelimit.rs` |
| T13 | Local echo w/ USERSTATE identity, reconciled by message id | ported → `chat/state.rs` |
| T14 | Multi-channel: join arbitrary channels, per-channel state, unread counts | ported → `chat/state.rs`, `ui/chat_tab.rs` |
| T15 | Scrollback ring (twi: 2000; spec default 1000, configurable) | ported → `chat/ring.rs` |
| T16 | Author identity color (FNV-1a hash, WCAG 4.5:1 correction) | ported → `chat/render.rs::identity_color` |
| T17 | Badge glyph rendering (◉ ⚔ ◆ ★ ♦ ⚙ ✓ ↯ ♛ ◈ ♥ ✎ …, width-1 glyphs) | ported → `chat/render.rs` |
| T18 | Timestamps HH:MM; action `*` prefix; deleted `[message deleted]` muted strikethrough; `[notice]` prefix | ported → `chat/render.rs` |
| T19 | Mention fragments (@word), emote-name fragments, grapheme-aware wrapping | ported → `chat/render.rs` |
| T20 | Message filters (1 mentions / 2 roles / 3 events / 4 notices, 0 reset) | ported → `chat/state.rs::Filters` + keys in `ui/app.rs` (the errors filter folded into notices — msm surfaces transport errors via the connection indicator instead of raw-tag scanning) |
| T21 | Chatter roster + @mention autocomplete | ported → `chat/roster.rs` + tab-completion in the composer (speakers-only: twitch-irc v6 cannot request the membership capability — see deviations) |
| T22 | Emote picker + Helix emote index | partially ported → `chat/emoji.rs` picker (ctrl+e) over the built-in catalog. The Helix channel-emote index is not ported: emotes render as text in this port, so the index would only autocomplete channel-emote names — deferred as low value for its two extra API surfaces |
| T23 | Activity view (cheers, events, removals) | ported → `ui/chat_tab.rs::draw_activity` (space-a; a projection over history per yc's design; follow events are not available — the follower poll belongs to the host dashboard, see T26) |
| T24 | Inspect panel | ported → `ui/chat_tab.rs::draw_inspect` (K; normalized fields — raw IRC tags are consumed by the twitch-irc crate and not retained, so the panel shows the normalized model, deleted text as `removed`) |
| T25 | Grouped/compact layouts | ported → `chat/render.rs::MessageLayout` (ctrl+g; the author-meta line needs per-author follow/seen data the host does not track per chat — folded into the inspect panel instead) |
| T26 | Helix polling: stream live/viewers (60s), followers/subs (120s) | not ported — the host Dashboard already polls exactly these via its own `twitch.rs` backend; duplicating the pollers per chat account would double API traffic for data already on screen |
| T27 | `/clip` command | ported → `chat/twitch.rs::handle_clip` + clips:edit scope |
| T28 | `/channels` picker command | ported → join prompt (`ui/chat_tab.rs`) |
| T29 | Desktop notifications | ported → `chat/notify.rs` (Windows toast omitted — documented deviation) |
| T30 | Reveal animations, gradients, pulsing chrome | ported → `anim.rs` (one 100ms clock, five effects: typewriter, gradient wave, shimmer, bounce, pulse; every effect a pure function of elapsed time, so frames are reproducible in tests). `animations = fast\|reduced\|off` in `[appearance]`, cycled with `alt+a`. Reveal is applied to chrome only, never to chat rows — see the deviations log |
| T31 | Themes (57 presets), theme picker, OSC 11/111 | ported → `theme.rs` (all 57 presets, nine roles, gradient/mix/darken/contrast helpers) + `ui/theme_picker.rs` (`ctrl+t`, live preview, swatch strip) + OSC 11/111 behind `terminal_background`. `[appearance]` holds the name and a `custom_theme` table; `msm profile list\|show\|set` is the command-line half |
| T32 | Splash/mascot, CLI (doctor/setup/profile), Docker/snap packaging, status-bar process telemetry (cpu/mem/fps), command palette, mouse support, Stream Info + Misc tabs | ported → `ui/splash.rs` (logo, typed tagline, scripted mascot chat, skippable), `msm doctor\|setup\|profile` in `main.rs`, `Dockerfile` + `snap/snapcraft.yaml`, `telemetry.rs` (`alt+t`), `ui/command_palette.rs` (`ctrl+p`), `ui/mouse.rs`. Stream Info is the host's own tab and stays that way; the Misc tab is not ported (its contents are host settings, which live in `[appearance]` and the pickers) |
| T33 | Anonymous justinfan mode | not ported — twi itself never calls it (library-only capability); no twi feature row exists |
| T34 | Debug logging (redacted structured) | ported → existing `tracing` to msm.log with the repo's redaction discipline |

### YouTube (`../yc`)

| # | Feature (yc source) | Status → Rust module |
|---|---|---|
| Y1 | liveChatMessages.list poller, `pollingIntervalMillis` absolute floor + jitter (internal/youtube/poll.go) | ported → `chat/youtube.rs` |
| Y2 | Poller states + backoff ladder + offline grace + park-on-terminal | ported → `chat/youtube.rs` |
| Y3 | Dedupe ring (8000 ids) + retained page token discipline | ported → `chat/youtube.rs` |
| Y4 | Target parsing (livechat:/id/video/UC…/@handle/URLs) | ported → `chat/youtube.rs::parse_target` |
| Y5 | Resolve ladder videos.list → channels.list; search never called | ported → `chat/youtube.rs` |
| Y6 | Snippet types: text, superChat, superSticker, newSponsor, memberMilestone, membershipGifting, giftMembershipReceived, gift, poll, fanFunding, deleted/retracted, userBanned, sponsorOnlyMode on/off, chatEnded, tombstone, unknown-degrades-readable | ported → `chat/youtube.rs::normalize` |
| Y7 | Money as integer micros (uint64-string parse, clamp, no floats) | ported → `chat/youtube.rs` |
| Y8 | Author badges owner/mod/member/verified (fixed order, width-1 glyphs) | ported → `chat/render.rs` |
| Y9 | Super Chat amount chip + 11-tier→6-step color ladder | ported → `chat/render.rs::tier_color` |
| Y10 | Membership chips (new/upgrade/milestone/gifting/received) | ported → `chat/render.rs` |
| Y11 | Send via insert: token bucket 3/2s, 200-grapheme cap, local echo reconcile | ported → `chat/youtube.rs` + `chat/ratelimit.rs` |
| Y12 | Reply convention `@DisplayName ` prefix (no threading) | ported → `ui/chat_tab.rs` |
| Y13 | Quota ledger: every dispatch charged, Pacific reset, reserve pause, persistence | ported → `chat/youtube.rs::QuotaStore` — one shared count across every poller, persisted to quota.json across sessions, Pacific-day rollover (the budget-floor stretch mode is subsumed by the reserve pause + config ceiling; noted as the one simplification) |
| Y14 | 401 single-flight refresh, one structural retry | ported → existing `auth::access_tokens` + per-poll token refresh (repo's holder pattern) |
| Y15 | Redaction: never emit request URLs (API-key-in-query) | ported — no API-key mode exists in msm (OAuth only), and error paths never embed URLs; discipline documented in module comment |
| Y16 | Shortcode `:emote:` fragments, emoji chips, URL fragments, mention fragments | ported → `chat/render.rs` (shared fragmenter) |
| Y17 | Deleted text never reprinted anywhere | ported → `chat/state.rs` invariant + render |
| Y18 | Moderation writes: delete message, ban, timeout | ported → `chat/youtube.rs` + d/t/b keys in `ui/chat_tab.rs` (unban has no UI in yc either — deliberately not exposed) |
| Y19 | Search (`/`, n/N), filters 1–4 | ported → `ui/chat_tab.rs` search + `chat/state.rs::Filters` |
| Y20 | Activity column, roster, autocomplete, inspect | ported → see T21/T23/T24 |
| Y21 | Chat logging JSONL + `export superchats` CSV | ported → `chat/chatlog.rs` + `msm export superchats` |
| Y22 | Auto-follow (re-resolve after stream end) | not ported — ctrl+r reconnects an ended chat in one keystroke, and this host already knows when its own broadcast starts (it created it); an opt-in background re-resolver spending quota on a maybe-live channel is deliberately left out |
| Y23 | Desktop notifications | ported → `chat/notify.rs` (2s throttle replaces burst coalescing — documented deviation) |
| Y24 | API-key-only read mode | not ported — msm is OAuth-only by design (its whole auth stack is authorization-code); adding an API-key credential class would fork the repo's credential model for a mode yc itself calls limited. Recorded as a deliberate scope decision. |
| Y25 | Mock mode | ported (test-level) → fake sources in unit tests rather than a user-facing flag |
| Y26 | Splash, CLI surface (doctor/quota/setup/profile/export as commands), themes, palette, OSC, alt-screen, non-interactive frame, FPS | not ported — standalone-app trappings per inventory §11 |
| Y27 | Stream Info / Quota tabs | not ported as tabs — quota state surfaces in the chat pane header/status (yc inventory itself recommends host-shaped presentation) |

Shared invariants ported as code + tests: drop-rather-than-block with visible
drop counts; absent data is never a negative fact; unknown types degrade to
readable rows; grapheme-cluster discipline in caps/wrapping; capability
disabled-with-reason.

## 7. Module layout

```
src/chat/mod.rs        normalized model, ChatEvent, ConnectionStatus
src/chat/ring.rs       bounded scrollback ring buffer
src/chat/ratelimit.rs  sliding-window sender limit, duplicate suppression, token bucket
src/chat/state.rs      per-chat state (messages, unread, echoes, deletions)
src/chat/source.rs     ChatSource trait + ChatBackend enum + task spawning
src/chat/twitch.rs     Twitch adapter over twitch-irc (provenance: twi@7c6ad6b)
src/chat/youtube.rs    YouTube poller adapter (provenance: yc@9e67efd)
src/chat/render.rs     fragments → ratatui text; identity colors, badges, chips
src/ui/chat_tab.rs     split view, account sub-tabs, composer, join prompt
```

## 8. Impedance mismatches (Go → Rust)

- goroutine-per-poller + five channels → one tokio task per chat emitting a
  single `ChatEvent` enum over one unbounded mpsc (UI already demultiplexes
  worker events this way); drop-rather-than-block becomes bounded `try_send`
  only where a bounded channel exists (commands), with drop counters.
- Go's `context.Context` cancellation → closing the per-task command channel
  (repo precedent) — deterministic shutdown, no leaked tasks.
- go-twitch-irc callbacks → `twitch-irc`'s message stream (`UnboundedReceiver`).
- Bubble Tea Model/View → existing `App`/`draw` split; lipgloss fragments →
  ratatui `Span`/`Line` with explicit styles.
- Go `uniseg` grapheme handling → `unicode-segmentation` (+ `unicode-width`)
  crates for the 200-grapheme cap and wrapping.
- twi/yc flat config files → this repo's real TOML `[chat]` table.

## 9. Deviations log

- (2026-08-15, adapters) `twitch-irc` v6 hard-codes its capability request to
  `tags`+`commands`; the `membership` capability cannot be requested, so
  JOIN/PART roster events are unavailable. Affects T1/T9 (partial) and T21
  (roster falls back to speakers-only when built). Documented in
  `chat/twitch.rs`.
- (2026-08-15, adapters) YouTube local echo shows author "you" until the
  poller's authoritative copy replaces it — resolving own authorDetails would
  cost an extra API unit per send.
- (2026-08-16, UI) Entering the Chat tab now opens the own chat of *every*
  logged-in account, not only the two whose sub-tabs are on screen: an unread
  count for an account that was never connected is meaningless. Lazy connection
  is preserved at the tab boundary (nothing opens until the tab is entered).
- (2026-08-15, UI) Timeout duration is a fixed 10 minutes behind a
  double-press confirm; yc prompts for a duration. A duration prompt is
  backlog alongside the other overlay work.

- **Animation is for chrome, not chat.** `anim.rs` ports twi's reveal effects,
  but they are applied to the splash, headings and indicators only. twi reveals
  chat rows themselves; this host does not. Chat here is two panes of two
  platforms at once and is the thing being read while something else is being
  done, so animating the text competes with reading it. The effects, the clock
  and the reduced-motion mode are all ported; the one place they are pointed at
  differs.

- **The active theme is one shared value, not a threaded parameter.** Drawing
  spans three modules and reads a colour in a few hundred places. `draw`
  publishes the frame's palette into `theme::skin()` once at the top of each
  frame and everything reads it from there. Single writer, so a frame is never
  drawn half from one theme and half from another; the tests that render take a
  lock because parallel test threads would otherwise each see the other's
  colours.

- **The command palette replays keys rather than implementing actions.** Each
  entry names the key events it stands for and choosing it feeds them back
  through `handle_key`. twi's palette dispatches its own action enum. Replaying
  makes drift impossible and makes the whole list testable: a test replays every
  entry across all tabs and screens and fails the build if one changes nothing.

- **Notifications are vim's `:messages`, not twi's status-line flash.** The old
  single-string toast was cleared by the next keypress, which loses messages
  that arrive while you are typing — and they arrive precisely then. Pop-ups now
  expire on their own timer and `alt+m` opens the full session history.

- **`msm profile` manages the theme, not config files.** This matches twi's
  `profile list|show|set` (which is its theme command). msm already has
  `--config <FILE>` for keeping one preset per kind of stream, so the word is
  not needed twice.

- **Process telemetry reads `/proc` directly.** twi uses Go's runtime for heap
  and a syscall for processor time. Here the figures come from
  `/proc/self/stat` and `/proc/self/statm` on Linux, and are simply left out
  elsewhere, rather than taking a libc dependency the program otherwise does
  not need for two numbers.

### Historical deviations

- (2026-08-15) `twitch-irc` chosen over hand-rolling despite repo's hand-rolled
  Helix precedent: twi itself sits on a library; the crate is the maintained
  mirror of that division of labor. Recorded per spec §Working style.
- (2026-08-15) yc does not use `streamList`; port follows yc (list-polling) and
  keeps the transport swappable.

## 10. Commit plan

1. `PLAN.md` (this file).
2. `chat` model + ring buffer + rate limiters (+tests).
3. Config `[chat]` table + multi-account token keys + `msm login --add` (+tests).
4. YouTube adapter: target parse, resolve, poller, normalize (+tests).
5. Twitch adapter over `twitch-irc`: normalize, send path (+tests).
6. Source seam + per-chat tasks.
7. UI: tab bar + Chat tab split view + sub-tabs + empty states.
8. Composer, join prompt, connection indicators, scrolling.
9. Rendering parity items (badges, chips, colors) (+tests).
10. README + docs; matrix status sweep.
11. Iterate on the `planned` rows (filters, roster, moderation, activity, …),
    one commit each, until the matrix has no unexplained gaps.
