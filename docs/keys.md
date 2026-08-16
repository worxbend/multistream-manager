# Keys and actions

`msm` has no command line. There are no subcommands and no flags: you run `msm`,
the interface opens, and everything the program can do is somewhere inside it.
This page is the reference for reaching those things from the keyboard, and for
changing which key reaches what.

If you type an option anyway — `msm --help`, `msm login`, anything at all — the
program prints a short note saying there are no options and where each tab is,
then exits without opening the interface. That is deliberate: starting up as
though nothing had been typed would look like the argument had been understood.

**Contents**

* [The shape of the keys](#the-shape-of-the-keys)
* [The which-key popup](#the-which-key-popup)
* [The five tabs](#the-five-tabs)
* [Everywhere](#everywhere)
* [Stream Info](#stream-info)
* [Chat](#chat)
* [OBS](#obs)
* [Config](#config)
* [The setup and login screens](#the-setup-and-login-screens)
* [Rebinding a key](#rebinding-a-key)
* [How a key is written](#how-a-key-is-written)
* [Every action name](#every-action-name)
* [Where the old subcommands went](#where-the-old-subcommands-went)

---

## The shape of the keys

The defaults are shaped the way [AstroNvim](https://astronvim.com) shapes
Neovim's, because that is a shape a great many people who live in a terminal
already have in their fingers. In practice that means five things.

* **A leader key**, <kbd>space</kbd> by default, in front of everything
  memorable.
* **Two-letter mnemonic groups** after it. <kbd>&lt;Leader&gt;</kbd> then
  <kbd>o</kbd> is the OBS group, so <kbd>&lt;Leader&gt;</kbd> <kbd>o</kbd>
  <kbd>s</kbd> is "OBS → stream" and <kbd>&lt;Leader&gt;</kbd> <kbd>o</kbd>
  <kbd>m</kbd> is "OBS → mute". The first letter names a subject and the second
  names a verb, so neither has to be remembered as a whole.
* **A which-key popup.** Press the leader, wait, and the choices appear.
* **<kbd>]</kbd> and <kbd>[</kbd> for next and previous** over whatever the
  current thing is: <kbd>]</kbd><kbd>t</kbd> for tabs, <kbd>]</kbd> on its own
  for the next chat.
* **vim's own movement keys left alone.** <kbd>j</kbd>, <kbd>k</kbd>,
  <kbd>g</kbd>, <kbd>G</kbd> and their friends mean here what they mean
  everywhere else, so they are not hidden behind a leader.

A handful of control chords sit outside all of that, for the two or three things
you might want regardless of where you are: <kbd>Ctrl</kbd>+<kbd>P</kbd> for the
command palette and <kbd>Ctrl</kbd>+<kbd>C</kbd> to quit.

> [!NOTE]
> The tabs are on <kbd>Alt</kbd>+<kbd>1</kbd>…<kbd>5</kbd> rather than
> <kbd>Ctrl</kbd>+<kbd>1</kbd>…<kbd>5</kbd>. A terminal cannot tell
> <kbd>Ctrl</kbd>+<kbd>1</kbd> apart from a plain <kbd>1</kbd> — the two send
> the same bytes — so a control-digit binding could not be detected at all.

---

## The which-key popup

Press <kbd>space</kbd> and pause. A popup lists every key that can follow it,
each with what it does. Keys that open a further group are shown as `+obs`,
`+chat` and so on; keys that finish a binding show the action's description.
Keep pressing and the popup narrows to what is still reachable.

Two other ways to find something:

* <kbd>&lt;Leader&gt;</kbd> <kbd>?</kbd> lists **every** binding in force,
  including the ones you have changed yourself.
* <kbd>Ctrl</kbd>+<kbd>P</kbd> opens the **command palette**: every action in
  the program, filtered as you type, each row showing the key that runs it. Using
  the palette teaches you the key you could have pressed, so over time you stop
  needing it.

When an action has several bindings, the one shown beside it is the one worth
learning — the fewest keys, and among equals the fewest modifiers. That is why
the Stream Info tab advertises <kbd>y</kbd> rather than
<kbd>&lt;Leader&gt;</kbd> <kbd>s</kbd> <kbd>y</kbd>, even though both copy the
Twitch key.

---

## The five tabs

| Key | Tab | What lives there |
|---|---|---|
| <kbd>Alt</kbd>+<kbd>1</kbd> | **Stream Info** | The title, description, tags, category and language; going live; the watch and ingest URLs; live statistics; the activity log |
| <kbd>Alt</kbd>+<kbd>2</kbd> | **Chat** | Twitch chat and YouTube live chat, one account sub-tab each |
| <kbd>Alt</kbd>+<kbd>3</kbd> | **Combined** | Both chats plus whatever else you have arranged, for a second monitor |
| <kbd>Alt</kbd>+<kbd>4</kbd> | **OBS** | Scenes, audio inputs, and OBS's streaming and recording state |
| <kbd>Alt</kbd>+<kbd>5</kbd> | **Config** | Layout, appearance, keys, OBS connection, accounts, housekeeping, diagnostics, file paths |

<kbd>]</kbd><kbd>t</kbd> and <kbd>[</kbd><kbd>t</kbd> walk through them in
order, and the same two tabs are also under <kbd>&lt;Leader&gt;</kbd>
<kbd>b</kbd> (`b` for buffers, which is AstroNvim's word for the thing you
switch between).

---

## Everywhere

These apply on every tab. A tab may give one of these keys a local meaning, and
where it does, the tab wins.

### Control chords

| Key | Action name | What it does |
|---|---|---|
| <kbd>Ctrl</kbd>+<kbd>C</kbd> | `app.quit` | Quit |
| <kbd>Ctrl</kbd>+<kbd>P</kbd> | `app.command_palette` | Command palette |
| <kbd>Alt</kbd>+<kbd>1</kbd> … <kbd>Alt</kbd>+<kbd>5</kbd> | `tab.stream_info`, `tab.chat`, `tab.combined`, `tab.obs`, `tab.config` | Go to that tab |
| <kbd>Alt</kbd>+<kbd>W</kbd> | `tab.swap_focus` | Swap which half of the Combined tab has the keyboard |
| <kbd>Alt</kbd>+<kbd>M</kbd> | `app.messages` | Message history — vim's `:messages` |
| <kbd>]</kbd><kbd>t</kbd> / <kbd>[</kbd><kbd>t</kbd> | `tab.next` / `tab.previous` | Next / previous tab |

### Straight after the leader

| Key | Action name | What it does |
|---|---|---|
| <kbd>&lt;Leader&gt;</kbd> <kbd>q</kbd> | `app.quit` | Quit |
| <kbd>&lt;Leader&gt;</kbd> <kbd>?</kbd> | `app.which_key` | Show every binding |
| <kbd>&lt;Leader&gt;</kbd> <kbd>/</kbd> | `chat.search` | Search chat |

### <kbd>&lt;Leader&gt;</kbd> <kbd>b</kbd> — tabs

| Key | Action name | What it does |
|---|---|---|
| <kbd>b</kbd> <kbd>s</kbd> | `tab.stream_info` | Stream Info tab |
| <kbd>b</kbd> <kbd>c</kbd> | `tab.chat` | Chat tab |
| <kbd>b</kbd> <kbd>b</kbd> | `tab.combined` | Combined tab |
| <kbd>b</kbd> <kbd>o</kbd> | `tab.obs` | OBS tab |
| <kbd>b</kbd> <kbd>g</kbd> | `tab.config` | Config tab |
| <kbd>b</kbd> <kbd>n</kbd> / <kbd>b</kbd> <kbd>p</kbd> | `tab.next` / `tab.previous` | Next / previous tab |

### <kbd>&lt;Leader&gt;</kbd> <kbd>f</kbd> — find

| Key | Action name | What it does |
|---|---|---|
| <kbd>f</kbd> <kbd>f</kbd> | `app.command_palette` | Command palette |
| <kbd>f</kbd> <kbd>m</kbd> | `app.messages` | Message history |
| <kbd>f</kbd> <kbd>c</kbd> | `chat.search` | Search chat |
| <kbd>f</kbd> <kbd>k</kbd> | `app.which_key` | Show every binding |

### <kbd>&lt;Leader&gt;</kbd> <kbd>u</kbd> — interface toggles

| Key | Action name | What it does |
|---|---|---|
| <kbd>u</kbd> <kbd>c</kbd> | `tab.config` | Config tab |
| <kbd>u</kbd> <kbd>t</kbd> | `ui.theme` | Choose a theme |
| <kbd>u</kbd> <kbd>a</kbd> | `ui.animations` | Cycle animations: fast, reduced, off |
| <kbd>u</kbd> <kbd>y</kbd> | `ui.telemetry` | Show or hide cpu, memory and frame rate |
| <kbd>u</kbd> <kbd>n</kbd> | `app.messages` | Message history |

`u` is AstroNvim's letter for interface toggles, and `uc` opens the Config tab
because that is where the rest of them can also be reached by anyone who would
rather read a list than remember a letter.

### <kbd>&lt;Leader&gt;</kbd> <kbd>s</kbd> — the stream

| Key | Action name | What it does |
|---|---|---|
| <kbd>s</kbd> <kbd>g</kbd> | `stream.go_live` | Go live |
| <kbd>s</kbd> <kbd>e</kbd> | `stream.edit` | Edit the stream info |
| <kbd>s</kbd> <kbd>r</kbd> | `stream.refresh` | Refresh statistics |
| <kbd>s</kbd> <kbd>y</kbd> | `stream.copy_twitch_key` | Copy the Twitch stream key |
| <kbd>s</kbd> <kbd>Y</kbd> | `stream.copy_youtube_key` | Copy the YouTube stream key |
| <kbd>s</kbd> <kbd>o</kbd> | `stream.open_watch_page` | Open the watch page in a browser |

AstroNvim puts search on `s`. This program's whole subject is streaming, so the
letter goes to that and find keeps `f`.

### <kbd>&lt;Leader&gt;</kbd> <kbd>c</kbd> — chat

| Key | Action name | What it does |
|---|---|---|
| <kbd>c</kbd> <kbd>c</kbd> | `chat.compose` | Write a message |
| <kbd>c</kbd> <kbd>j</kbd> | `chat.join` | Join a channel |
| <kbd>c</kbd> <kbd>r</kbd> | `chat.reconnect` | Reconnect chat |
| <kbd>c</kbd> <kbd>s</kbd> | `chat.search` | Search chat |
| <kbd>c</kbd> <kbd>e</kbd> | `chat.emoji` | Emoji picker |
| <kbd>c</kbd> <kbd>a</kbd> | `chat.activity` | Toggle the activity view |
| <kbd>c</kbd> <kbd>i</kbd> | `chat.inspect` | Toggle the inspect panel |
| <kbd>c</kbd> <kbd>0</kbd> | `chat.clear_filters` | Clear the message filters |

### <kbd>&lt;Leader&gt;</kbd> <kbd>o</kbd> — OBS

| Key | Action name | What it does |
|---|---|---|
| <kbd>o</kbd> <kbd>s</kbd> | `obs.stream` | Start or stop streaming |
| <kbd>o</kbd> <kbd>r</kbd> | `obs.record` | Start or stop recording |
| <kbd>o</kbd> <kbd>p</kbd> | `obs.pause_recording` | Pause or resume a recording |
| <kbd>o</kbd> <kbd>m</kbd> | `obs.mute` | Toggle mute on the selected input |
| <kbd>o</kbd> <kbd>M</kbd> | `obs.mute_all` | Mute everything — the panic key |
| <kbd>o</kbd> <kbd>P</kbd> | `obs.next_profile` | Next OBS profile |
| <kbd>o</kbd> <kbd>C</kbd> | `obs.next_collection` | Next scene collection |
| <kbd>o</kbd> <kbd>R</kbd> | `obs.reconnect` | Reconnect to OBS |
| <kbd>o</kbd> <kbd>u</kbd> | `obs.refresh` | Refresh everything from OBS |

These work from any tab, which is the point of them: muting a microphone should
not require first finding the OBS tab.

---

## Stream Info

<kbd>Alt</kbd>+<kbd>1</kbd>. The tab that holds the title, the category, going
live, and the live statistics afterwards.

| Key | Action name | What it does |
|---|---|---|
| <kbd>r</kbd> | `stream.refresh` | Refresh statistics now |
| <kbd>e</kbd> | `stream.edit` | Edit the stream info |
| <kbd>o</kbd> | `stream.open_watch_page` | Open the watch page |
| <kbd>y</kbd> | `stream.copy_twitch_key` | Copy the Twitch stream key |
| <kbd>Y</kbd> | `stream.copy_youtube_key` | Copy the YouTube stream key |
| <kbd>q</kbd> | `app.quit` | Quit |

Going live is <kbd>Ctrl</kbd>+<kbd>G</kbd> from inside the form, and
<kbd>&lt;Leader&gt;</kbd> <kbd>s</kbd> <kbd>g</kbd> from anywhere.

> [!WARNING]
> <kbd>y</kbd> and <kbd>Y</kbd> **copy** a stream key; nothing anywhere
> **shows** one. The value travels from the API to the system clipboard inside a
> background task, so it never passes through anything that could end up on
> screen, in a recording, or in the log file.

### The form

The form is where you type what the stream is. It replaces what used to be
`msm go` on a command line.

| Key | Does |
|---|---|
| <kbd>Tab</kbd>, <kbd>↑</kbd> / <kbd>↓</kbd> | Move between fields |
| <kbd>Enter</kbd> | Open the search list on a category or language field |
| <kbd>Space</kbd> | Flip a yes/no field |
| <kbd>←</kbd> / <kbd>→</kbd> | Change a selector such as Privacy |
| <kbd>Ctrl</kbd>+<kbd>W</kbd> | Delete the previous word |
| <kbd>Ctrl</kbd>+<kbd>U</kbd> | Clear the field |
| <kbd>Ctrl</kbd>+<kbd>S</kbd> | Save what you have typed as your new defaults, in `[preset]` |
| <kbd>Ctrl</kbd>+<kbd>G</kbd> | Go live |
| <kbd>Esc</kbd> | Close the search list, or go back a screen |

The Twitch category field is a search rather than free text: Twitch's update
endpoint accepts a numeric category id and nothing else, so a typed-but-unpicked
name could not be sent. Press <kbd>Enter</kbd> and choose a match.

---

## Chat

<kbd>Alt</kbd>+<kbd>2</kbd>, and the same keys apply inside the chat panes of the
Combined tab.

| Key | Action name | What it does |
|---|---|---|
| <kbd>j</kbd> / <kbd>k</kbd>, <kbd>↓</kbd> / <kbd>↑</kbd> | `chat.scroll_down` / `chat.scroll_up` | Move the selection |
| <kbd>PgDn</kbd> / <kbd>PgUp</kbd> | `chat.page_down` / `chat.page_up` | Page forward / back |
| <kbd>g</kbd> / <kbd>G</kbd> | `chat.oldest` / `chat.newest` | Jump to the oldest / newest message |
| <kbd>h</kbd> / <kbd>l</kbd>, <kbd>Tab</kbd> | `chat.previous_pane` / `chat.next_pane` | Focus the other pane |
| <kbd>[</kbd> / <kbd>]</kbd> | `chat.previous` / `chat.next` | Previous / next open chat in this account |
| <kbd>{</kbd> / <kbd>}</kbd> | `chat.previous_account` / `chat.next_account` | Previous / next account sub-tab |
| <kbd>i</kbd> | `chat.compose` | Write a message. <kbd>Enter</kbd> sends, <kbd>Esc</kbd> keeps the draft |
| <kbd>r</kbd> | `chat.reply` | Reply to the selected message |
| <kbd>/</kbd> | `chat.search` | Search messages as you type |
| <kbd>n</kbd> / <kbd>N</kbd> | `chat.search_next` / `chat.search_previous` | Walk newer / older matches |
| <kbd>&lt;</kbd> / <kbd>&gt;</kbd> / <kbd>=</kbd> | `chat.widen` / `chat.narrow` / `chat.reset_panes` | Resize the split, or put it back |
| <kbd>Ctrl</kbd>+<kbd>R</kbd> | `chat.reconnect` | Reconnect — this also overrides a YouTube quota pause |
| <kbd>Ctrl</kbd>+<kbd>E</kbd> | `chat.emoji` | Emoji picker |
| <kbd>q</kbd> | `app.quit` | Quit |

Joining another chat is <kbd>&lt;Leader&gt;</kbd> <kbd>c</kbd> <kbd>j</kbd>: give
it a Twitch channel name, or for YouTube a video id, an `@handle`, a channel id
or a plain youtube.com / youtu.be URL.

The moderation keys (<kbd>d</kbd> delete, <kbd>b</kbd> ban, <kbd>t</kbd> time
out), the view filters (<kbd>1</kbd>–<kbd>4</kbd>, <kbd>0</kbd> to reset) and the
display toggles are handled by the chat pane itself rather than through the
keymap, so they are not rebindable and do not appear in the table above.

---

## OBS

<kbd>Alt</kbd>+<kbd>4</kbd>. Scenes on the left, audio inputs on the right.

| Key | Action name | What it does |
|---|---|---|
| <kbd>j</kbd> / <kbd>k</kbd>, <kbd>↓</kbd> / <kbd>↑</kbd> | `obs.down` / `obs.up` | Move within the focused list |
| <kbd>h</kbd> / <kbd>l</kbd>, <kbd>Tab</kbd> | `obs.swap_pane` | Swap between scenes and audio |
| <kbd>Enter</kbd> | `obs.activate` | Switch to the scene, or toggle the input's mute |
| <kbd>m</kbd> | `obs.mute` | Mute or unmute the selected input, from either list |
| <kbd>M</kbd> | `obs.mute_all` | **Mute everything** — the panic key |
| <kbd>+</kbd> / <kbd>=</kbd> / <kbd>-</kbd> | `obs.volume_up` / `obs.volume_down` | Nudge the selected input's level |
| <kbd>s</kbd> / <kbd>r</kbd> | `obs.stream` / `obs.record` | Start or stop streaming / recording |
| <kbd>p</kbd> | `obs.pause_recording` | Pause or resume a recording |
| <kbd>P</kbd> / <kbd>C</kbd> | `obs.next_profile` / `obs.next_collection` | Cycle profiles / scene collections |
| <kbd>R</kbd> | `obs.reconnect` | Reconnect now |
| <kbd>u</kbd> | `obs.refresh` | Refresh everything from OBS |
| <kbd>q</kbd> | `app.quit` | Quit |

Scene and audio **shortcuts** from `[obs.scene_shortcuts]` and
`[obs.audio_shortcuts]` also act on this tab, so <kbd>3</kbd> can be "switch to
Be Right Back". Those take precedence over the tab's own keys, so pick keys the
table above does not already use — see
[Configuration](configuration.md#aliases-and-shortcuts).

---

## Config

<kbd>Alt</kbd>+<kbd>5</kbd>, or <kbd>&lt;Leader&gt;</kbd> <kbd>u</kbd>
<kbd>c</kbd>. A list of sections on the left, the chosen section on the right.

| Section | What it is for |
|---|---|
| **Layout** | Arrange the Combined tab |
| **Appearance** | Theme, motion, notifications |
| **Keys** | Every binding, and what it runs |
| **OBS** | Connection to OBS Studio |
| **Accounts** | Twitch and YouTube logins |
| **Housekeeping** | Tidy up and export |
| **Diagnostics** | What is working and what is not |
| **Files** | Where everything is kept |

Getting around the tab:

| Key | Does |
|---|---|
| <kbd>j</kbd> / <kbd>k</kbd>, <kbd>↓</kbd> / <kbd>↑</kbd> | Move — through the sections on the left, or through the rows on the right |
| <kbd>h</kbd> / <kbd>l</kbd>, <kbd>Tab</kbd> | Move the keyboard between the section list and the section's contents |
| <kbd>Enter</kbd> | Run the selected row, in Accounts and Housekeeping |
| <kbd>Esc</kbd> | Leave the tab |

### Accounts

One row per platform, saying whether it is logged in. <kbd>Enter</kbd> logs in if
it is not and logs out if it is, so this one row is where `msm login`,
`msm logout` and `msm status` all ended up. Logging in opens your browser.

### Housekeeping

Three jobs; <kbd>Enter</kbd> runs the selected one, and the results go to the
activity log rather than into the pane.

| Job | What it does |
|---|---|
| Find abandoned broadcasts | YouTube keeps every broadcast that was set up and never used. The first <kbd>Enter</kbd> **lists** them; a second <kbd>Enter</kbd> deletes the ones listed. Anything that has ever been live is neither listed nor touched. |
| Export paid events to CSV | Every Super Chat, sticker and gift from the chat logs, written beside them as a spreadsheet. Needs `chat_logging` to have been on. |
| List YouTube stream keys | The **ids** of the reusable stream keys on the channel, for `stream_id` under `[youtube]`. |

> [!NOTE]
> Cleanup lists before it deletes because deleting things you made, without
> showing them to you first, would be asking for a kind of trust this program has
> no way to earn. And the stream listing shows ids only, never keys — this window
> is often part of the broadcast.

### Layout

The layout editor: a live preview of the arrangement above the list of panels
that make it up. The preview is drawn by the same code the real Combined tab
uses, so it cannot disagree with the result.

| Key | Does |
|---|---|
| <kbd>j</kbd> / <kbd>k</kbd> | Select a panel in the list |
| <kbd>+</kbd> / <kbd>-</kbd> | Give the selected panel a larger or smaller share of the space |
| <kbd>a</kbd> | Add the first panel that is not on the layout yet |
| <kbd>d</kbd> | Remove the selected panel. The last one cannot be removed — a blank tab is indistinguishable from a broken one |
| <kbd>r</kbd> | Rotate: turn rows into columns and back |
| <kbd>p</kbd> | Cycle through the four presets |
| <kbd>s</kbd> | Save. Until you press this, nothing is applied |

Leaving the tab with an unsaved edit throws the edit away and says so, rather
than leaving it half applied. The eight panels and the file format behind all of
this are in [Configuration](configuration.md#layout).

### Diagnostics

What used to be `msm doctor`. Each check reports `ok`, `warn` or `fail`, and
every warning says what to do about it rather than only what is wrong: config
file, credentials, saved logins, the clipboard tool, the terminal's
capabilities, and the OBS connection.

### Files

What used to be `msm paths`: where the config file, the token file, the log file
and the chat logs are kept. See
[Configuration](configuration.md#other-files-in-the-same-directory).

---

## The setup and login screens

On a first run the interface opens on these two rather than on a tab, because
there is nothing to show until credentials exist.

**Set up API access** — one box per credential: the client id and client secret
from each developer console. Secrets are drawn as dots even while you type them,
because this window is often on screen while you stream.

| Key | Does |
|---|---|
| <kbd>Tab</kbd>, <kbd>↑</kbd> / <kbd>↓</kbd> | Move between boxes |
| <kbd>←</kbd> / <kbd>→</kbd>, <kbd>Backspace</kbd> | Edit within a box |
| <kbd>Enter</kbd> or <kbd>Ctrl</kbd>+<kbd>S</kbd> | Save to `config.toml` and go on |
| <kbd>Esc</kbd> | Back to the login screen, or quit if nothing is configured yet |

Filling in one platform is fine — an empty pair is skipped rather than treated as
an error.

**Authorise your accounts** — tick what you want to stream to and press
<kbd>Enter</kbd>; your browser opens for each in turn.

| Key | Does |
|---|---|
| <kbd>j</kbd> / <kbd>k</kbd>, <kbd>↑</kbd> / <kbd>↓</kbd> | Move |
| <kbd>Space</kbd> | Tick or untick a platform |
| <kbd>Enter</kbd> | Authorise everything ticked |
| <kbd>c</kbd> | Back to the credential form, to fix a typo without quitting |
| <kbd>s</kbd> | Skip, carrying on with whatever logins already exist |
| <kbd>q</kbd> or <kbd>Esc</kbd> | Quit |

Later on, the Accounts section of the Config tab does the same job.

---

## Rebinding a key

Every binding above can be changed in the `[keys]` section of `config.toml`. The
built-in bindings are the starting point and your file is applied on top, so you
only write the ones you want to be different.

```toml
[keys]
# The key every mnemonic sequence starts with. Changing it moves every
# <Leader>… binding at once, including the built-in ones.
leader = "<Space>"

[keys.global]
"<C-g>" = "stream.go_live"     # go live from anywhere
"<Leader>q" = ""               # an empty action removes a binding

[keys.chat]
"<C-j>" = "chat.next"

[keys.stream_info]
"R" = "stream.refresh"

[keys.obs]
"<F1>" = "obs.mute_all"
```

The four tables are the four **contexts** a binding can belong to:

| Table | Where it applies |
|---|---|
| `[keys.global]` | Everywhere |
| `[keys.stream_info]` | The Stream Info tab |
| `[keys.chat]` | The chat panes, on either the Chat or the Combined tab |
| `[keys.obs]` | The OBS tab |

A key is looked up in the active tab's context first and in `global` second, so a
tab can give a key a local meaning without you having to restate everything else.
That is why <kbd>j</kbd> scrolls chat on one tab and moves down a scene list on
another.

Something wrong in `[keys]` — a key that cannot be parsed, an action that does
not exist — is **reported and skipped**, not treated as a reason to refuse to
start. Being locked out of your own stream by a mistyped binding would be an
absurd trade. The Keys section of the Config tab lists what is actually in force,
which is where to look when a change did not take.

---

## How a key is written

The notation is vim's, because anyone who would want to rebind keys in a
terminal program already knows it, and inventing a second notation would mean
learning something new to say something you can already say.

| Written | Means |
|---|---|
| `j` | the letter j |
| `J` | shift+j |
| `<Space>` | the space bar |
| `<Leader>` | whatever the leader is set to (space by default) |
| `<C-p>` | ctrl+p |
| `<A-4>` or `<M-4>` | alt+4 |
| `<S-Tab>` | shift+tab |
| `<CR>` or `<Enter>` | return |
| `<Esc>`, `<Tab>`, `<BS>`, `<Up>`, `<Down>`, `<Left>`, `<Right>` | those keys |
| `<PageUp>`, `<PageDown>`, `<Home>`, `<End>`, `<Del>` | those keys |
| `<F1>` … `<F12>` | the function keys |
| `<lt>` | a literal `<`, which cannot be written on its own |

Several of them in a row make a **chord**: `<Leader>os` is three key presses —
the leader, then `o`, then `s`. A chord that is the beginning of a longer one
waits for the rest rather than doing nothing, which is what makes the which-key
popup appear after `<Leader>o`.

---

## Every action name

These are the names `[keys]` accepts on the right-hand side. The part before the
dot is the group the action appears under in the which-key popup.

### Getting around

| Name | What it does |
|---|---|
| `tab.stream_info` | Stream Info tab |
| `tab.chat` | Chat tab |
| `tab.combined` | Combined tab |
| `tab.obs` | OBS tab |
| `tab.config` | Configuration tab |
| `tab.next` | Next tab |
| `tab.previous` | Previous tab |
| `tab.swap_focus` | Swap combined halves |

### The program itself

| Name | What it does |
|---|---|
| `app.quit` | Quit |
| `app.command_palette` | Command palette |
| `app.messages` | Message history |
| `app.which_key` | Show every binding |
| `ui.theme` | Choose a theme |
| `ui.animations` | Cycle animations |
| `ui.telemetry` | Toggle telemetry |

### Streaming

| Name | What it does |
|---|---|
| `stream.go_live` | Go live |
| `stream.edit` | Edit stream info |
| `stream.refresh` | Refresh statistics |
| `stream.copy_twitch_key` | Copy Twitch stream key |
| `stream.copy_youtube_key` | Copy YouTube stream key |
| `stream.open_watch_page` | Open watch page |

### Chat

| Name | What it does |
|---|---|
| `chat.compose` | Write a message |
| `chat.search` | Search chat |
| `chat.search_next` | Next match |
| `chat.search_previous` | Previous match |
| `chat.join` | Join a channel |
| `chat.reconnect` | Reconnect chat |
| `chat.next` | Next chat |
| `chat.previous` | Previous chat |
| `chat.next_account` | Next account |
| `chat.previous_account` | Previous account |
| `chat.scroll_up` | Scroll back |
| `chat.scroll_down` | Scroll forward |
| `chat.page_up` | Page back |
| `chat.page_down` | Page forward |
| `chat.oldest` | Oldest message |
| `chat.newest` | Newest message |
| `chat.next_pane` | Next pane |
| `chat.previous_pane` | Previous pane |
| `chat.widen` | Widen left pane |
| `chat.narrow` | Narrow left pane |
| `chat.reset_panes` | Reset pane sizes |
| `chat.activity` | Toggle activity view |
| `chat.inspect` | Toggle inspect panel |
| `chat.emoji` | Emoji picker |
| `chat.reply` | Reply to selection |
| `chat.clear_filters` | Clear message filters |

### OBS

| Name | What it does |
|---|---|
| `obs.up` | Move up |
| `obs.down` | Move down |
| `obs.swap_pane` | Swap scenes/audio |
| `obs.activate` | Switch scene or toggle mute |
| `obs.mute` | Toggle mute |
| `obs.mute_all` | Mute everything |
| `obs.volume_up` | Volume up |
| `obs.volume_down` | Volume down |
| `obs.stream` | Start/stop streaming |
| `obs.record` | Start/stop recording |
| `obs.pause_recording` | Pause/resume recording |
| `obs.next_profile` | Next profile |
| `obs.next_collection` | Next scene collection |
| `obs.reconnect` | Reconnect to OBS |
| `obs.refresh` | Refresh from OBS |

---

## Where the old subcommands went

Earlier versions had fifteen subcommands. They are all still here, as places in
the interface rather than as things to type. The reason for the change is that a
streaming setup is driven with one hand while the other is doing something else,
and the moment you want to mute a microphone or fix a title is never a moment you
would choose to leave what you are looking at, find a terminal, and remember a
subcommand.

| Used to be | Now |
|---|---|
| `msm login`, `msm logout`, `msm status` | Config → Accounts |
| `msm go` | The Stream Info form, <kbd>Ctrl</kbd>+<kbd>G</kbd> |
| `msm key twitch` / `msm key youtube` | <kbd>y</kbd> / <kbd>Y</kbd> on Stream Info — copied to the clipboard, never printed |
| `msm categories` | The category field in the form searches Twitch's list as you type |
| `msm streams` | Config → Housekeeping → *List YouTube stream keys* |
| `msm cleanup` | Config → Housekeeping → *Find abandoned broadcasts* |
| `msm export superchats` | Config → Housekeeping → *Export paid events to CSV* |
| `msm doctor` | Config → Diagnostics |
| `msm setup`, `msm init` | The first-run **Set up API access** screen |
| `msm profile list` / `set` | <kbd>&lt;Leader&gt;</kbd> <kbd>u</kbd> <kbd>t</kbd>, the theme picker, and Config → Appearance |
| `msm paths` | Config → Files |
| `msm obs …` | The OBS tab (<kbd>Alt</kbd>+<kbd>4</kbd>) and <kbd>&lt;Leader&gt;</kbd> <kbd>o</kbd> |
| `msm --config <FILE>` | Gone. See [Configuration](configuration.md#keeping-more-than-one-preset) for what to do instead |

---

* [Configuration](configuration.md) — every setting in `config.toml`.
* [Getting started](getting-started.md) — credentials, first login, first stream.
* [Troubleshooting](troubleshooting.md) — when something does not work.
* [Back to the documentation index](README.md).
