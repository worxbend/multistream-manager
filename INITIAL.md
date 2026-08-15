# Task: Integrate multi-account Twitch/YouTube chat into `multistream-manager` (Rust TUI)

## Context

You are working inside the existing repository `multistream-manager` — a **Rust** TUI streaming tool targeting Twitch and YouTube. Its current capability: configuring stream info (title, category, etc.) for both platforms from a single place. Two sibling repositories exist on disk and are the designated sources for chat functionality:

- `../twi` — Twitch chat TUI (IRC-based), **written in Go**.
- `../yc` — YouTube chat client, **written in Go**.

**All new code must be written in Rust**, inside this repository. The sibling repos are Go, so nothing can be imported or linked — they serve two roles simultaneously:

1. **Reference implementations**: the authority on protocol behavior — connection handshake, auth flow, message parsing, rate limiting, reconnection behavior, API quirks.
2. **Feature parity targets**: **every feature implemented in `../twi` and `../yc` must be reimplemented in Rust in this repository.** This is a full port, not a minimal extraction. If `twi` supports sending messages, message coloring, badge parsing, `/commands`, multiple simultaneous channels, or moderation views — those come over. If `yc` supports author-type distinctions (owner/moderator/member), Super Chat rendering, or membership events — those come over. The definitive feature list is whatever Phase 0 recon discovers, not this enumeration.

Never shell out to, embed, or FFI-bridge the Go binaries.

## Phase 0 — Mandatory reconnaissance (do this before writing any code)

1. Read the full `multistream-manager` tree: `Cargo.toml`/workspace layout, entrypoint, TUI framework in use (likely `ratatui` + `crossterm` — verify, don't assume), async runtime (likely `tokio` — verify), event loop, state model, how tabs/views are currently rendered, how stream backends (Twitch/YouTube accounts) are configured, where credentials/config are persisted (serde format, file location), and the existing auth flows.
2. Read `../twi` **exhaustively** and produce a complete feature inventory: the chat connection layer (IRC transport — likely Twitch IRC over TLS/WebSocket, capability negotiation `twitch.tv/tags`/`commands`/`membership`, PING/PONG keepalive, IRCv3 tag parsing, reconnection, rate limiting), auth flow, message send path, rendering features (colors, badges, emote text, timestamps), commands, configuration surface, and any behavior not obvious from the README. Every discovered feature is a port target.
3. Read `../yc` **exhaustively**, same treatment: connection mechanism (polling `liveChatMessages.list` with `pollingIntervalMillis` vs. any streaming path, API quota costs, OAuth scopes), message model and author-type handling (owner/moderator/member/Super Chat/membership events — whatever it actually implements), send path if present, configuration surface. Every discovered feature is a port target.
4. Survey the Rust ecosystem for the **transport layer only**: evaluate whether existing crates (e.g., `twitch-irc`, `irc`, or raw `reqwest` + `serde` against the YouTube Live Streaming API) can replace the low-level protocol plumbing. Prefer a well-maintained crate over a hand-rolled protocol implementation — but the Go repos remain the behavioral authority (rate limits, edge cases, reconnect semantics), and all feature-level logic above the transport is ported regardless.
5. Produce `PLAN.md` at repo root containing: current architecture of `multistream-manager`; the **complete feature-parity matrix** (one row per twi/yc feature → its Rust counterpart module/status); chosen transport strategy per platform (crate vs. hand-rolled); identified impedance mismatches (Go goroutine/channel patterns → tokio task/mpsc equivalents, Go's IRC libs → Rust counterparts, auth token storage); and the crate/module layout you will create. **Wait for nothing — proceed after writing the plan**, but the plan must exist, and the parity matrix is the contract for the rest of the task.

## Requirements

### Tab structure (top level)

1. **Tab 1 — "Stream Info"** (existing functionality, first/default tab):
   - If **no stream backend is configured**, replace the normal content with an empty-state view: an explanation that no accounts are connected, plus concrete, keybinding-level hints on how to add a Twitch account and a YouTube account (whatever the actual flow is in this repo — discover it in Phase 0 and reference it accurately; do not invent commands).
   - If backends exist, behave as today.

2. **Tab 2 — "Chat"**:
   - **Split view**: left pane = Twitch, right pane = YouTube. Both panes always present; a pane with no configured accounts shows its own empty-state hint (mirroring the Stream Info hint for that platform).
   - **Multiple accounts per platform**: N Twitch accounts and M YouTube accounts must be supported.
   - **Sub-tabs within each pane**: one sub-tab per logged-in account of that platform.
   - **Within an account sub-tab**: the user can open **any channel's chat**, not only their own. Default on account sub-tab activation: the account's **own** chat is opened and connected. Provide an input/command to join an arbitrary channel (Twitch: channel name; YouTube: live video ID or channel — pick the mechanism `../yc` actually supports and document the choice).

### Behavioral requirements

- Chat connections are **lazy**: connect when a sub-tab/chat is first opened, not for all accounts at startup. Disconnect (or at minimum stop rendering work) for chats not visible, unless the existing architecture already has a cheap background-buffer pattern — justify whichever you choose in `PLAN.md`.
- Reconnection with backoff on transport failure; visible connection-state indicator per chat (connecting / connected / disconnected / error).
- Message rendering: at minimum author, message text, timestamps — plus **everything twi/yc render** (colors, badges, emote text, author types, Super Chat/membership events, etc., per the Phase 0 parity matrix). Terminal-native rendering (ANSI colors, unicode) is expected; graphical emote images are the only rendering feature that does not carry over unless twi itself renders them (see Non-goals).
- **Feature parity is a behavioral requirement, not aspiration**: every row of the Phase 0 parity matrix must land in Rust, including message sending, commands, and moderation features if the Go repos implement them.
- Scrollback with a bounded ring buffer per chat (configurable cap, sane default ~1000 messages) — no unbounded memory growth.
- Keyboard-driven navigation: switch top-level tabs, move focus between Twitch/YouTube panes, cycle account sub-tabs, open/close a channel chat, scroll. Follow the keybinding conventions already established in the repo; extend, don't reinvent.

### Architecture constraints

- **Rust throughout.** No non-Rust runtime dependencies, no shelling out to sibling binaries, no FFI to their code.
- Keep a **strict seam between chat transport and TUI rendering**: define a `ChatSource` trait (`connect`/`disconnect`/`join`/`leave`) whose implementations emit normalized `ChatMessage` events over a `tokio::sync::mpsc` channel (or the channel/event mechanism this repo already uses — discover in Phase 0). The UI consumes only the normalized model. Twitch/YouTube specifics live behind adapter types implementing the trait. Prefer enum dispatch (`enum ChatBackend { Twitch(..), YouTube(..) }`) over `Box<dyn ChatSource>` if the set of backends is closed and it simplifies lifetimes — justify the choice in `PLAN.md`.
- One normalized `ChatMessage` struct; platform-specific extras go in an optional metadata field (e.g., `enum PlatformMeta { Twitch(TwitchMeta), YouTube(YouTubeMeta) }`), not in the core shape.
- Concurrency: each active chat connection runs as its own tokio task; chat I/O must never block the render loop. Task shutdown on leave/disconnect must be deterministic (`CancellationToken` or dropping the task's channel — no leaked tasks). The render loop drains message channels non-blockingly per frame.
- YouTube polling adapter must respect `pollingIntervalMillis` from the API response — never poll faster; quota exhaustion is a hard failure mode to surface in the connection-state indicator.
- Auth/token storage for new accounts must reuse the repo's existing credential persistence — same serde format, file location, and conventions. Multiple accounts per platform must be representable in that config schema; migrate the schema if needed with a backward-compatible deserialization path (`#[serde(default)]`, untagged fallback, or explicit version field — match repo precedent).
- Error handling: match the repo's existing idiom (`thiserror`/`anyhow`/custom) — do not introduce a competing error strategy. Transport errors surface as connection-state events, never panics; `unwrap`/`expect` only where invariants are provably upheld and documented.
- Code must pass `cargo clippy --all-targets -- -D warnings` and `cargo fmt --check`. If the repo has stricter lint configuration, honor it.
- Do not fork-and-diverge silently: when porting logic from `../twi`/`../yc`, record provenance (source repo + commit + Go file) in a module-level comment on the Rust counterpart. Port the *behavior*, not Go idioms — no goroutine-shaped code transliterated into `tokio::spawn` spaghetti; restructure into idiomatic Rust ownership and error flow.

## Non-goals (explicitly out of scope)

Scope is defined by the parity matrix: **a feature is in scope iff twi/yc implement it, or this document requires it.** Consequently:

- **New** features absent from both twi/yc and this spec (e.g., third-party emote providers 7TV/BTTV/FFZ, graphical emote images, moderation dashboards) are out of scope — *unless* the Go repos implement them, in which case they are in.
- Any redesign of the Stream Info tab beyond the empty-state view.
- Refactoring existing `multistream-manager` code beyond what the integration structurally requires.

## Deliverables & acceptance criteria

1. `PLAN.md` — recon summary, feature-parity matrix, and design decisions (Phase 0 output). Final state of the matrix: every row marked ported, with its Rust module path; any row not ported requires an explicit written justification (e.g., API removed upstream, feature broken in the Go original).
2. Working build (`cargo build` clean, `cargo clippy` clean, `cargo test` green) with the two-tab structure; all existing functionality intact.
3. Empty states with actionable hints on both tabs when no accounts exist.
4. Demonstrable: two Twitch accounts + one YouTube account configured → Chat tab shows split view, correct sub-tabs, own chats auto-opened, arbitrary channel joinable on the Twitch side.
5. Tests: unit tests for the normalized message adapter layer (Twitch raw → normalized, YouTube raw → normalized), ring-buffer bounds, and config schema migration. UI smoke coverage to the extent the repo's existing test approach allows.
6. Update the repo README: new tab documentation, keybindings, multi-account config example.

## Working style

- Small, reviewable commits per logical unit (extraction, adapter, UI pane, sub-tabs, empty states, docs).
- When `../twi` or `../yc` internals surprise you (different auth model, quota constraints, coupling worse than expected), update `PLAN.md` with the deviation and the new approach rather than silently improvising.
- Match the existing code style, error-handling idioms, and logging conventions of `multistream-manager` exactly.