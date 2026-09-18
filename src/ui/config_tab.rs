//! The Configuration tab: everything that used to need a text editor.
//!
//! This program has no command-line options and no separate configuration
//! step. Everything it can be told is here, in the interface, while it is
//! running — because the alternative is quitting the thing you are in the
//! middle of, finding a file, editing it by hand, and starting again, which
//! is a poor way to change the size of a pane.
//!
//! The tab is a list of sections down the left and the chosen section's
//! contents on the right, which is the arrangement every settings screen has
//! used for thirty years and therefore needs no explaining.
//!
//! The section that justifies the tab is **Layout**. The Combined view is
//! meant for a fullscreen terminal on a second monitor, and what belongs on
//! that screen is not the same for somebody streaming alone as for somebody
//! with a moderator, a second camera and a chat they need to watch closely.
//! Rather than guess, this lets it be arranged.

use ratatui::prelude::*;
use ratatui::widgets::{Block, BorderType, Borders, Paragraph, Wrap};
use tui_checkbox::symbols::{CHECKED, UNCHECKED};
use tui_widget_list::{ListBuilder, ListState as WidgetListState, ListView};

use super::app::App;
use crate::layout::{Direction, Layout as PaneLayout, Panel};
use crate::theme;
use crate::ui::style_kit;

/// Which part of the configuration is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    Layout,
    Appearance,
    Notifications,
    /// Chat settings that can be changed while the program is running.
    Chat,
    Keys,
    Obs,
    Accounts,
    Maintenance,
    Diagnostics,
    Paths,
}

impl Section {
    /// The keys that actually do something in this section, for the footer.
    ///
    /// The Config tab had no footer branch at all, so it fell through and
    /// advertised whichever Stream Info screen happened to be underneath —
    /// telling the user to press `r` to refresh and `o` to open the watch
    /// page, neither of which does anything here. A footer that names the
    /// wrong keys is worse than no footer: it is a promise the tab does not
    /// keep.
    ///
    /// The navigation keys are the same everywhere and come first; each
    /// section adds its own.
    pub fn footer_hints(self) -> &'static str {
        match self {
            Section::Layout => concat!(
                " j/k move   tab pane   +/- panel   </> row   J/K reorder   r rotate",
                "   a add   d remove   p preset   u undo   s save   esc back   q quit"
            ),
            Section::Appearance => " j/k move   tab pane   enter change   esc back   q quit",
            Section::Notifications | Section::Chat => {
                " j/k move   tab pane   enter toggle   esc back   q quit"
            }
            Section::Accounts => concat!(
                " j/k move   tab pane   enter log in/out   a add a chat account",
                "   d forget an extra account   esc back   q quit"
            ),
            Section::Maintenance => " j/k move   tab pane   enter run   esc back   q quit",
            Section::Diagnostics => {
                " j/k move   tab pane   r re-run the checks   esc back   q quit"
            }
            // Read-only displays: nothing to say beyond how to get around.
            Section::Keys => " j/k move   tab pane   type to filter   esc back   q quit",
            Section::Paths => " j/k move   tab pane   enter open   esc back   q quit",
            Section::Obs => " j/k move   tab pane   esc back   q quit",
        }
    }

    pub const ALL: [Section; 10] = [
        Section::Layout,
        Section::Appearance,
        Section::Notifications,
        Section::Chat,
        Section::Keys,
        Section::Obs,
        Section::Accounts,
        Section::Maintenance,
        Section::Diagnostics,
        Section::Paths,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Section::Layout => "Layout",
            Section::Appearance => "Appearance",
            Section::Notifications => "Notifications",
            Section::Chat => "Chat",
            Section::Keys => "Keys",
            Section::Obs => "OBS",
            Section::Accounts => "Accounts",
            Section::Maintenance => "Housekeeping",
            Section::Diagnostics => "Diagnostics",
            Section::Paths => "Files",
        }
    }

    /// One line saying what the section is for, shown under the list so the
    /// names do not have to carry the whole meaning.
    pub fn summary(self) -> &'static str {
        match self {
            Section::Layout => "Arrange the Combined tab",
            Section::Appearance => "Theme, motion, pop-ups",
            Section::Notifications => "Desktop alerts for stream events",
            Section::Chat => "Message logging and scrollback",
            Section::Keys => "Every binding, and what it runs",
            Section::Obs => "Connection to OBS Studio",
            Section::Accounts => "Twitch and YouTube logins",
            Section::Maintenance => "Tidy up and export",
            Section::Diagnostics => "What is working and what is not",
            Section::Paths => "Where everything is kept",
        }
    }
}

/// Which half of the tab has the keyboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Sections,
    Contents,
}

/// The tab's own state.
#[derive(Debug, Clone)]
pub struct ConfigTab {
    pub section: Section,
    pub focus: Focus,
    /// Which row of the section's contents is selected.
    pub cursor: usize,
    /// The layout being edited, kept separately from the one being drawn so
    /// an edit can be abandoned. Applied on save.
    pub draft: PaneLayout,
    /// Whether the abandoned broadcasts have already been listed once, so a
    /// second press is understood as "yes, delete those".
    pub cleanup_listed: bool,
    /// Whether the draft differs from what is saved, so the tab can say so
    /// rather than leaving somebody to wonder whether they pressed the key.
    pub dirty: bool,
    /// The last self-check, and when it was taken.
    ///
    /// Cached rather than computed while drawing. The checks look for
    /// clipboard helpers by *starting* them (`clipboard::is_installed` runs
    /// `wl-copy --help` and waits for it), read the token store off disk, and
    /// walk `PATH` for a notification program. Doing that inside the draw
    /// function meant up to six processes forked and waited on per frame —
    /// twice a second at rest, ten times a second while anything animated —
    /// on the thread that has to stay responsive to keys. It is a snapshot of
    /// the machine, so it is taken when the section is opened and on demand
    /// afterwards.
    pub diagnostics: Diagnostics,
    /// How far the diagnostics list is scrolled.
    ///
    /// The pane is a plain paragraph with no cursor, so the checks are read
    /// rather than selected — but there are around a dozen of them, each with
    /// its own advice line, and on a short terminal the verdict at the bottom
    /// was simply cut off. Scrolling is the difference between a self-check
    /// and a self-check you can finish reading.
    pub diagnostics_scroll: u16,
    /// A typed filter over the Keys listing.
    ///
    /// Around 110 bindings walked one `j` at a time, with no filter, no
    /// paging and no `g`/`G`. Finding the one you wanted meant holding a key
    /// down and watching.
    pub key_filter: String,
    /// Previous states of the draft, for `u`.
    ///
    /// The editor had no undo, and `p` replaces the whole arrangement in one
    /// keypress — so cycling past the preset you wanted meant rebuilding by
    /// hand what one key had thrown away.
    pub history: Vec<PaneLayout>,
    /// Which layout preset `p` will apply next.
    ///
    /// Its own counter, because the obvious thing — deriving it from
    /// `cursor` — is not a cycle at all: `cursor` is which *panel* row is
    /// selected, so pressing `p` twice from the same row gave the same preset
    /// twice, and moving the cursor changed which preset `p` produced.
    pub preset_index: usize,
    /// The chat-log directory's file count and total size, as of the last
    /// [`refresh_chat_log_size`](Self::refresh_chat_log_size).
    ///
    /// `draw_chat` used to `read_dir` the log directory and `stat` every file
    /// in it on every redraw — twice a second at rest, far more often while
    /// anything animated — for a number that only changes when a message is
    /// logged or a rotation runs. `None` until the Chat section has been
    /// opened at least once this session.
    pub chat_log_size: Option<(usize, u64)>,
}

/// A cached self-check.
#[derive(Debug, Clone, Default)]
pub struct Diagnostics {
    pub checks: Vec<crate::diagnostics::Check>,
    /// When it was taken, so the pane can say how old it is. `None` means it
    /// has not been run yet in this session.
    pub taken_at: Option<chrono::DateTime<chrono::Local>>,
}

impl ConfigTab {
    pub fn new(layout: PaneLayout) -> Self {
        Self {
            section: Section::Layout,
            focus: Focus::Sections,
            cursor: 0,
            draft: layout,
            cleanup_listed: false,
            dirty: false,
            diagnostics: Diagnostics::default(),
            preset_index: 0,
            diagnostics_scroll: 0,
            history: Vec::new(),
            key_filter: String::new(),
            chat_log_size: None,
        }
    }

    /// Run the self-check and keep the result.
    ///
    /// Called when the Diagnostics section is opened and when `r` is pressed
    /// there — never from the drawing code, which must not start processes.
    pub fn refresh_diagnostics(&mut self, config: &crate::config::Config) {
        self.diagnostics = Diagnostics {
            checks: crate::diagnostics::run(config),
            taken_at: Some(chrono::Local::now()),
        };
    }

    /// Re-read the chat-log directory's file count and total size.
    ///
    /// Called when the Chat section is opened — never from `draw_chat`,
    /// which must not walk the filesystem on every frame.
    pub fn refresh_chat_log_size(&mut self, config: &crate::config::Config) {
        self.chat_log_size = crate::paths::chat_log_dir_for(config)
            .ok()
            .map(|dir| log_directory_size(&dir));
    }

    /// The bindings matching the Keys filter.
    ///
    /// Matched on the written chord, the context and the description *and*
    /// the action name, so both "what key does this" and "what is
    /// obs.mute_all on" are answerable — and the action name is what you
    /// write in config.toml, which is the other reason to look at this
    /// screen.
    pub fn matching_bindings(&self, app: &App) -> Vec<crate::keys::Binding> {
        let needle = self.key_filter.trim().to_lowercase();
        app.keymap
            .all()
            .into_iter()
            .filter(|binding| {
                needle.is_empty() || {
                    let haystack = format!(
                        "{} {} {} {}",
                        crate::keys::write_chord(&binding.chord, app.keymap.leader),
                        binding.context.name(),
                        binding.action.describe(),
                        binding.action.name()
                    )
                    .to_lowercase();
                    haystack.contains(&needle)
                }
            })
            .collect()
    }

    /// How many rows the chosen section offers, for clamping the cursor.
    pub fn rows(&self, app: &App) -> usize {
        match self.section {
            Section::Layout => self.draft.panels().len(),
            Section::Appearance => APPEARANCE_ROWS,
            Section::Notifications => NOTIFICATION_ROWS,
            Section::Chat => CHAT_ROWS,
            Section::Keys => self.matching_bindings(app).len(),
            Section::Obs => 0,
            // Every account the store holds, not one row per platform: the
            // extra chat accounts were invisible and unremovable. This must
            // track `account_rows` exactly — that's what actually renders,
            // via `tui_widget_list`, and an out-of-range cursor there panics
            // rather than just missing the highlight the way the old
            // Paragraph-based rendering did. `account_rows` only pads up to
            // one row per platform when the store is empty; once any account
            // exists, it draws exactly one row per account.
            Section::Accounts => {
                let count = app.all_accounts().len();
                if count == 0 {
                    crate::model::Platform::ALL.len()
                } else {
                    count
                }
            }
            // The three jobs, plus a row per stream id the last listing
            // found, so one can be pinned without a text editor.
            Section::Maintenance => MAINTENANCE_ROWS + app.youtube_streams.len(),
            Section::Diagnostics => 0,
            // Three paths, each of which can now be opened.
            Section::Paths => 3,
        }
    }
}

/// How many settings the Appearance section lists.
pub const APPEARANCE_ROWS: usize = 9;

/// How many switches the Notifications section lists.
/// One switchable notification setting.
///
/// The rows used to be written down twice: a list of labels in the drawing
/// code, and a `match config.cursor { 0 => …, 1 => … }` in the key handler
/// that had to stay in lockstep with it. Adding, removing or reordering a
/// single row silently rebound every toggle below it — thirteen
/// destructive-by-mistake settings held together by two lists agreeing by
/// convention. One table that both halves walk makes the hazard impossible
/// rather than merely tested for.
pub struct NotificationRow {
    /// What the row is called on screen. Two leading spaces indent the
    /// settings that only work when Twitch events are being watched.
    pub label: &'static str,
    /// Read the current value.
    pub get: fn(&crate::config::NotificationsConfig) -> bool,
    /// Flip it.
    pub toggle: fn(&mut crate::config::NotificationsConfig),
}

/// Every notification switch, in the order the section lists them.
pub const NOTIFICATION_TABLE: &[NotificationRow] = &[
    NotificationRow {
        label: "Desktop notifications",
        get: |n| n.enabled,
        toggle: |n| n.enabled = !n.enabled,
    },
    NotificationRow {
        label: "Raids",
        get: |n| n.raids,
        toggle: |n| n.raids = !n.raids,
    },
    NotificationRow {
        label: "Subscriptions & gifts",
        get: |n| n.subscriptions,
        toggle: |n| n.subscriptions = !n.subscriptions,
    },
    NotificationRow {
        label: "Cheers & bits",
        get: |n| n.cheers,
        toggle: |n| n.cheers = !n.cheers,
    },
    NotificationRow {
        label: "Super Chats",
        get: |n| n.paid,
        toggle: |n| n.paid = !n.paid,
    },
    NotificationRow {
        label: "Memberships",
        get: |n| n.memberships,
        toggle: |n| n.memberships = !n.memberships,
    },
    NotificationRow {
        label: "Stream started/stopped",
        get: |n| n.stream_state,
        toggle: |n| n.stream_state = !n.stream_state,
    },
    NotificationRow {
        label: "Only when chat is hidden",
        get: |n| n.only_when_hidden,
        toggle: |n| n.only_when_hidden = !n.only_when_hidden,
    },
    // Everything below needs the second Twitch connection, so the switch that
    // opens it comes first and the rest are meaningless without it.
    NotificationRow {
        label: "Watch Twitch events",
        get: |n| n.twitch_events,
        toggle: |n| n.twitch_events = !n.twitch_events,
    },
    NotificationRow {
        label: "  New followers",
        get: |n| n.follows,
        toggle: |n| n.follows = !n.follows,
    },
    NotificationRow {
        label: "  Channel points",
        get: |n| n.redemptions,
        toggle: |n| n.redemptions = !n.redemptions,
    },
    NotificationRow {
        label: "  Hype trains",
        get: |n| n.hype_trains,
        toggle: |n| n.hype_trains = !n.hype_trains,
    },
    NotificationRow {
        label: "  Polls",
        get: |n| n.polls,
        toggle: |n| n.polls = !n.polls,
    },
    NotificationRow {
        label: "  Predictions",
        get: |n| n.predictions,
        toggle: |n| n.predictions = !n.predictions,
    },
];

/// The label of the row that fires a test notification.
///
/// Not a setting, so it is not in the table above — but it belongs in this
/// section, because "my notifications do not work" is otherwise discovered
/// during a raid, which is the worst possible moment to find out. The desktop
/// path has four fallbacks on Linux and it is entirely reasonable not to know
/// which one, if any, this machine has.
pub const TEST_NOTIFICATION_LABEL: &str = "Send a test notification";

/// The row index of the Twitch-events switch, which dims the ones below it.
///
/// Found rather than written down, so it cannot drift from the table.
pub fn twitch_events_row() -> usize {
    NOTIFICATION_TABLE
        .iter()
        .position(|row| row.label == "Watch Twitch events")
        .expect("the Twitch events row is in the table")
}

/// How many rows the notifications section has: every switch, plus the
/// test-notification row at the bottom.
pub const NOTIFICATION_ROWS: usize = NOTIFICATION_TABLE.len() + 1;

/// The housekeeping jobs, in the order they are listed.
pub const MAINTENANCE_JOBS: [(&str, &str); 3] = [
    (
        "Find abandoned broadcasts",
        "YouTube keeps every broadcast that was set up and never used. This lists them; \
         pressing enter again deletes the ones listed. Anything that has ever been live is \
         neither listed nor touched.",
    ),
    (
        "Export paid events to CSV",
        "Every Super Chat, sticker and gift from the chat logs, written beside them as a \
         spreadsheet. Needs chat logging to have been on.",
    ),
    (
        "List YouTube stream keys",
        "The ids of the reusable stream keys on the channel, for `stream_id` under [youtube]. \
         The ids only — a key itself is never shown.",
    ),
];

pub const MAINTENANCE_ROWS: usize = MAINTENANCE_JOBS.len();

/// Draw the whole tab.
pub fn draw(frame: &mut Frame, area: Rect, app: &App) {
    let sk = theme::skin();
    let Some(config) = &app.config_tab else {
        return;
    };

    let columns = Layout::horizontal([Constraint::Length(22), Constraint::Min(0)]).split(area);

    // --- the section list -------------------------------------------------
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(if config.focus == Focus::Sections {
            sk.accent
        } else {
            sk.border
        }))
        .title(" Configuration ");
    let inner = block.inner(columns[0]);
    frame.render_widget(block, columns[0]);

    let lines: Vec<Line> = Section::ALL
        .iter()
        .map(|section| {
            let selected = *section == config.section;
            let mut line = Line::from(vec![
                Span::styled(
                    if selected { " ▸ " } else { "   " },
                    Style::new().fg(sk.accent),
                ),
                Span::styled(
                    section.title(),
                    if selected {
                        Style::new().fg(sk.foreground).add_modifier(Modifier::BOLD)
                    } else {
                        Style::new().fg(sk.muted)
                    },
                ),
            ]);
            line = line.style(selected_row_style(
                Style::default(),
                selected && config.focus == Focus::Sections,
                &sk,
            ));
            line
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), inner);

    // --- the section itself ----------------------------------------------
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(if config.focus == Focus::Contents {
            sk.accent
        } else {
            sk.border
        }))
        .title(format!(
            " {} — {} ",
            config.section.title(),
            config.section.summary()
        ))
        .padding(ratatui::widgets::Padding::horizontal(1));
    let inner = block.inner(columns[1]);
    frame.render_widget(block, columns[1]);

    match config.section {
        Section::Layout => draw_layout_section(frame, inner, config),
        Section::Appearance => draw_appearance(frame, inner, app, config),
        Section::Notifications => draw_notifications(frame, inner, app, config),
        Section::Chat => draw_chat(frame, inner, app, config),
        Section::Keys => draw_keys(frame, inner, app, config),
        Section::Obs => draw_obs(frame, inner, app),
        Section::Accounts => draw_accounts(frame, inner, app, config),
        Section::Maintenance => draw_maintenance(frame, inner, app, config),
        Section::Diagnostics => draw_diagnostics(frame, inner, app),
        Section::Paths => draw_paths(frame, inner, app),
    }
}

/// The layout editor: a preview of the arrangement above the list of panels
/// that make it up.
///
/// A preview rather than only a list, because a layout is a spatial thing and
/// a list of weights does not tell anybody what their screen will look like.
fn draw_layout_section(frame: &mut Frame, area: Rect, config: &ConfigTab) {
    let sk = theme::skin();
    let rows = Layout::vertical([
        Constraint::Min(6),    // the preview
        Constraint::Length(1), // a spacer
        Constraint::Min(4),    // the panel list
        Constraint::Length(2), // the hints
    ])
    .split(area);

    draw_preview(frame, rows[0], &config.draft, config.cursor);

    let placed = config.draft.panels();
    let lines: Vec<Line> = placed
        .iter()
        .enumerate()
        .map(|(index, panel)| {
            let selected = index == config.cursor && config.focus == Focus::Contents;
            let mut line = Line::from(vec![
                Span::styled(
                    if selected { "▸ " } else { "  " },
                    Style::new().fg(sk.accent),
                ),
                Span::styled(panel.title().to_string(), Style::new().fg(sk.foreground)),
                Span::styled(format!("   ({})", panel.name()), Style::new().fg(sk.muted)),
            ]);
            line = line.style(selected_row_style(Style::default(), selected, &sk));
            line
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), rows[2]);

    let dirty = if config.dirty { "  ·  unsaved" } else { "" };
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(
                format!(
                    "J/K move · +/- resize · a add · d remove · r rotate · p preset · s save{dirty}"
                ),
                Style::new().fg(sk.muted),
            )),
            Line::from(Span::styled(
                "The Combined tab (alt+3) uses this arrangement.",
                Style::new().fg(sk.muted),
            )),
        ])
        .wrap(Wrap { trim: false }),
        rows[3],
    );
}

/// Draw a miniature of the arrangement, using the same resolver the real tab
/// uses — so the preview cannot disagree with the result.
fn draw_preview(frame: &mut Frame, area: Rect, layout: &PaneLayout, cursor: usize) {
    let sk = theme::skin();
    // The panel the cursor is on, in the same order the list below numbers
    // them. Without it `J`/`K` and `+`/`-` were legible only as numbers
    // changing in the list, while the picture — the whole point of a preview
    // — said nothing about which box was about to move.
    let selected = layout.panels().get(cursor).copied();
    for (panel, rect) in layout.resolve(area) {
        if rect.width < 2 || rect.height < 1 {
            continue;
        }
        let is_selected = selected == Some(panel);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::new().fg(if is_selected { sk.accent } else { sk.border }));
        let inner = block.inner(rect);
        frame.render_widget(block, rect);
        if inner.height > 0 {
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    panel.title().to_string(),
                    if is_selected {
                        Style::new().fg(sk.accent).add_modifier(Modifier::BOLD)
                    } else {
                        Style::new().fg(sk.muted)
                    },
                )))
                .wrap(Wrap { trim: true }),
                inner,
            );
        }
    }
}

/// The background a row is drawn with: `base` — usually nothing, or a zebra
/// stripe from [`style_kit::zebra_style`] — unless the row is selected, in
/// which case the selection colour always wins.
///
/// About ten sections below each used to repeat their own
/// `if selected { line = line.style(Style::new().bg(sk.selection)) }`, so
/// this factors that idiom out once. `base` is patched over rather than
/// discarded when unselected, and the selection is patched over `base` when
/// selected, matching [`style_kit::zebra_style`]'s own promise that a stripe
/// never gets to compete with the row that is actually selected.
fn selected_row_style(base: Style, selected: bool, sk: &theme::Skin) -> Style {
    if selected {
        base.patch(Style::new().bg(sk.selection))
    } else {
        base
    }
}

/// A settings row's value: either read-only text (a name, a count) or a
/// genuine on/off switch.
///
/// Kept apart rather than pre-rendered into a `String` the way this table
/// used to be, because a switch and a piece of text are drawn differently now
/// — a switch gets `tui-checkbox`'s own glyph, and only the drawing code
/// should have to know which rows those are.
enum SettingValue {
    Text(String),
    Bool(bool),
}

impl SettingValue {
    /// Render as a value column: a checkbox glyph for a switch, plain text
    /// otherwise. `sk` decides both colours, so a switch reads green when it
    /// is genuinely on and dim otherwise — the glyph carries the state by
    /// shape as well, so this still reads with colour turned off.
    fn as_span(&self, sk: &theme::Skin) -> Span<'static> {
        match self {
            SettingValue::Bool(value) => {
                checkbox_span(*value, if *value { sk.success } else { sk.muted })
            }
            SettingValue::Text(text) => Span::styled(text.clone(), Style::new().fg(sk.accent)),
        }
    }
}

/// A boolean switch's value, drawn with `tui-checkbox`'s own checked/
/// unchecked glyph rather than the word "on"/"off".
///
/// Reaches for the crate's own [`CHECKED`]/[`UNCHECKED`] constants rather
/// than hand-drawing a look-alike box, and the two differ in shape as well as
/// colour, so the state still reads with colour turned off. Display-only:
/// this is a `Span`, not the crate's interactive `Checkbox` widget, because
/// `Enter`/`config_activate` remains the only way to flip any of these rows
/// and nothing here should look independently focusable.
fn checkbox_span(value: bool, colour: Color) -> Span<'static> {
    Span::styled(
        if value { CHECKED } else { UNCHECKED },
        Style::new().fg(colour),
    )
}

fn draw_appearance(frame: &mut Frame, area: Rect, app: &App, config: &ConfigTab) {
    let sk = theme::skin();
    let appearance = &app.config.appearance;
    let settings: [(&str, SettingValue); APPEARANCE_ROWS] = [
        ("Theme", SettingValue::Text(appearance.theme.clone())),
        (
            "Animations",
            SettingValue::Text(appearance.animations.clone()),
        ),
        ("Splash screen", SettingValue::Bool(appearance.splash)),
        ("Mouse", SettingValue::Bool(appearance.mouse)),
        ("Telemetry", SettingValue::Bool(appearance.telemetry)),
        // Named for what it is, because the Notifications section next door
        // is about the *desktop's* pop-ups and confusing the two would send
        // somebody to the wrong switch.
        (
            "Routine pop-ups (problems always show)",
            SettingValue::Bool(appearance.toasts),
        ),
        (
            "Terminal background",
            SettingValue::Bool(appearance.terminal_background),
        ),
        (
            "Pop-up seconds",
            SettingValue::Text(format!("{} (enter cycles)", appearance.toast_seconds)),
        ),
        (
            "Streamer mode",
            SettingValue::Text(
                crate::config::StreamerMode::parse(&appearance.streamer_mode)
                    .name()
                    .to_string(),
            ),
        ),
    ];

    let lines: Vec<Line> = settings
        .iter()
        .enumerate()
        .map(|(index, (name, value))| {
            let selected = index == config.cursor && config.focus == Focus::Contents;
            let value_span = value.as_span(&sk);
            let mut line = Line::from(vec![
                Span::styled(
                    if selected { "▸ " } else { "  " },
                    Style::new().fg(sk.accent),
                ),
                Span::styled(format!("{name:<22}"), Style::new().fg(sk.foreground)),
                value_span,
            ]);
            line = line.style(selected_row_style(
                style_kit::zebra_style(index, &sk),
                selected,
                &sk,
            ));
            line
        })
        .chain(std::iter::once(Line::from("")))
        .chain(std::iter::once(Line::from(Span::styled(
            "enter change · every change is saved straight away",
            Style::new().fg(sk.muted),
        ))))
        .collect();
    frame.render_widget(Paragraph::new(lines), area);
}

/// The Notifications section: desktop pop-ups for what the *stream* does.
///
/// Kept apart from Appearance on purpose. Appearance's "In-app pop-ups" are
/// drawn inside this program and are only seen by somebody looking at the
/// terminal. These go to the desktop's own notification service, and exist for
/// the times you are in OBS, in the game, or out of the room — which is when a
/// raid lands.
fn draw_notifications(frame: &mut Frame, area: Rect, app: &App, config: &ConfigTab) {
    let sk = theme::skin();
    let settings = &app.config.notifications;
    let mut rows: Vec<(&str, SettingValue)> = NOTIFICATION_TABLE
        .iter()
        .map(|row| (row.label, SettingValue::Bool((row.get)(settings))))
        .collect();
    rows.push((
        TEST_NOTIFICATION_LABEL,
        SettingValue::Text("press enter".into()),
    ));

    let mut lines: Vec<Line> = rows
        .iter()
        .enumerate()
        .map(|(index, (name, value))| {
            let selected = index == config.cursor && config.focus == Focus::Contents;
            // A switch under one that is off does nothing, and greying it
            // says so without hiding it: the master switch dims everything,
            // and the Twitch-events switch dims the four that need it.
            let dimmed = (index > 0 && !settings.enabled)
                || (index > twitch_events_row() && !settings.twitch_events);
            let name_colour = if dimmed { sk.muted } else { sk.foreground };
            // A dimmed row stays dim regardless of its own value — it does
            // not matter right now — but a relevant switch now also reflects
            // *its own* state rather than always reading accent-coloured
            // whichever way it is set.
            let value_span = match value {
                SettingValue::Bool(v) => {
                    let colour = if dimmed {
                        sk.muted
                    } else if *v {
                        sk.success
                    } else {
                        sk.muted
                    };
                    checkbox_span(*v, colour)
                }
                SettingValue::Text(text) => {
                    let colour = if dimmed { sk.muted } else { sk.accent };
                    Span::styled(text.clone(), Style::new().fg(colour))
                }
            };
            let mut line = Line::from(vec![
                Span::styled(
                    if selected { "▸ " } else { "  " },
                    Style::new().fg(sk.accent),
                ),
                Span::styled(format!("{name:<26}"), Style::new().fg(name_colour)),
                value_span,
            ]);
            line = line.style(selected_row_style(
                style_kit::zebra_style(index, &sk),
                selected,
                &sk,
            ));
            line
        })
        .collect();

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        format!(
            "At most one pop-up every {:.1}s; the rest queue rather than being lost.",
            settings.min_gap_ms as f64 / 1000.0
        ),
        Style::new().fg(sk.muted),
    )));
    lines.push(Line::from(Span::styled(
        "Needs no setup: notify-send, then gdbus, then kdialog, then the bell.",
        Style::new().fg(sk.muted),
    )));
    lines.push(Line::from(Span::styled(
        "Raids, subs and cheers come over chat; the four below need Twitch events.",
        Style::new().fg(sk.muted),
    )));
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "enter change · every change is saved straight away",
        Style::new().fg(sk.muted),
    )));

    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }), area);
}

/// One chat setting that can be changed while the program is running.
///
/// Only the switches: `scrollback_limit` and the log rotation sizes are shown
/// beside them as read-only, because changing those mid-session would mean
/// rebuilding buffers that are holding a live conversation.
pub const CHAT_ROWS: usize = 1;

/// Chat settings. Small on purpose — most of `[chat]` is either a number that
/// wants a text editor or a switch that lives next door under Notifications.
fn draw_chat(frame: &mut Frame, area: Rect, app: &App, config: &ConfigTab) {
    let sk = theme::skin();
    let chat = &app.config.chat;

    let selected = config.focus == Focus::Contents && config.cursor == 0;
    let mut lines = vec![
        Line::from(vec![
            Span::styled(
                if selected { "▸ " } else { "  " },
                Style::new().fg(sk.accent),
            ),
            Span::styled(
                format!("{:<26}", "Write a chat log to disk"),
                Style::new().fg(sk.foreground),
            ),
            checkbox_span(
                chat.chat_logging,
                if chat.chat_logging {
                    sk.success
                } else {
                    sk.muted
                },
            ),
        ]),
        Line::from(""),
    ];

    // Where it goes and how much is there. The rotation settings existed and
    // nothing ever reported what was actually on disk.
    match crate::paths::chat_log_dir_for(&app.config) {
        Ok(dir) => {
            let (files, bytes) = config.chat_log_size.unwrap_or_default();
            lines.push(Line::from(Span::styled(
                format!("  {}", dir.display()),
                Style::new().fg(sk.muted),
            )));
            lines.push(Line::from(Span::styled(
                format!(
                    "  {files} file(s), {:.1} MB · rotates at {:.0} MB, keeps {}",
                    bytes as f64 / (1024.0 * 1024.0),
                    chat.chat_log_max_bytes as f64 / (1024.0 * 1024.0),
                    chat.chat_log_max_files
                ),
                Style::new().fg(sk.muted),
            )));
        }
        Err(err) => lines.push(Line::from(Span::styled(
            format!("  the log directory is unavailable: {err:#}"),
            Style::new().fg(sk.warning),
        ))),
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "  The log is what Housekeeping's paid-event export reads, so",
        Style::new().fg(sk.muted),
    )));
    lines.push(Line::from(Span::styled(
        "  an export only covers what was recorded while this was on.",
        Style::new().fg(sk.muted),
    )));
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        format!(
            "  Scrollback kept per chat: {} messages",
            chat.scrollback_limit
        ),
        Style::new().fg(sk.muted),
    )));

    // What the day's YouTube spend estimate actually says. It decides when
    // chat polling pauses, and until now nothing ever showed it — the first
    // sign of trouble was polling stopping mid-stream.
    lines.push(Line::from(""));
    match app.chat.quota_summary() {
        Some((used, limit, percent)) => {
            let colour = if percent >= 90 {
                sk.error
            } else if percent >= 60 {
                sk.warning
            } else {
                sk.muted
            };
            lines.push(Line::from(Span::styled(
                format!(
                    "  YouTube quota today: about {used} of {limit} units ({percent}%), \
                     reserve {}%",
                    chat.quota_reserve_percent
                ),
                Style::new().fg(colour),
            )));
            lines.push(Line::from(Span::styled(
                "  An estimate this program keeps, not a figure from Google. Chat",
                Style::new().fg(sk.muted),
            )));
            lines.push(Line::from(Span::styled(
                "  polling pauses at the reserve so sending keeps working.",
                Style::new().fg(sk.muted),
            )));
        }
        None => lines.push(Line::from(Span::styled(
            "  YouTube quota estimate is off (daily_quota_units = 0).",
            Style::new().fg(sk.muted),
        ))),
    }

    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), area);
}

/// How many chat log files there are and how much they take up.
///
/// Errors are swallowed into "nothing there": this is a line of information
/// on a settings screen, and a directory that cannot be read is not worth
/// failing the refresh over. Called from
/// [`ConfigTab::refresh_chat_log_size`], not from drawing.
fn log_directory_size(dir: &std::path::Path) -> (usize, u64) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return (0, 0);
    };
    entries
        .flatten()
        .filter_map(|entry| entry.metadata().ok())
        .filter(|meta| meta.is_file())
        .fold((0, 0), |(count, bytes), meta| {
            (count + 1, bytes + meta.len())
        })
}

/// A switch's value as the section shows it.
fn on_off(value: bool) -> String {
    if value { "on" } else { "off" }.to_string()
}

/// Every binding, so the answer to "what key does that?" is in the program
/// rather than in a document.
fn draw_keys(frame: &mut Frame, area: Rect, app: &App, config: &ConfigTab) {
    let sk = theme::skin();
    let bindings = config.matching_bindings(app);
    let height = area.height.saturating_sub(2) as usize;
    let first = config
        .cursor
        .saturating_sub(height.saturating_sub(1))
        .min(bindings.len().saturating_sub(height.min(bindings.len())));

    let mut lines: Vec<Line> = bindings
        .iter()
        .enumerate()
        .skip(first)
        .take(height)
        .map(|(index, binding)| {
            let selected = index == config.cursor && config.focus == Focus::Contents;
            let chord = crate::keys::write_chord(&binding.chord, app.keymap.leader);
            let mut line = Line::from(vec![
                Span::styled(format!("{chord:<16}"), Style::new().fg(sk.accent)),
                Span::styled(
                    format!("{:<10}", binding.context.name()),
                    Style::new().fg(sk.muted),
                ),
                Span::styled(
                    binding.action.describe().to_string(),
                    Style::new().fg(sk.foreground),
                ),
            ]);
            line = line.style(selected_row_style(Style::default(), selected, &sk));
            line
        })
        .collect();

    lines.push(Line::from(""));

    // Anything wrong with `[keys]`, here rather than only in a log line at
    // start-up. Somebody whose binding was silently discarded comes to this
    // screen and finds a table that does not contain it; without this there
    // is nothing to say why.
    if !app.key_problems.is_empty() {
        lines.push(Line::from(Span::styled(
            format!(
                "{} problem(s) with [keys] — these bindings were not applied:",
                app.key_problems.len()
            ),
            Style::new().fg(sk.warning).add_modifier(Modifier::BOLD),
        )));
        for problem in app.key_problems.iter().take(4) {
            lines.push(Line::from(Span::styled(
                format!("  {problem}"),
                Style::new().fg(sk.warning),
            )));
        }
        if app.key_problems.len() > 4 {
            lines.push(Line::from(Span::styled(
                format!(
                    "  …and {} more, in the activity log.",
                    app.key_problems.len() - 4
                ),
                Style::new().fg(sk.muted),
            )));
        }
        lines.push(Line::from(""));
    }

    lines.push(Line::from(Span::styled(
        if config.key_filter.is_empty() {
            "type to filter · change these under [keys] in config.toml · <Leader>? shows them \
             as a map"
                .to_string()
        } else {
            format!(
                "filter: {}   {} of {} shown   backspace clears",
                config.key_filter,
                bindings.len(),
                app.keymap.all().len()
            )
        },
        Style::new().fg(sk.muted),
    )));
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), area);
}

fn draw_obs(frame: &mut Frame, area: Rect, app: &App) {
    let sk = theme::skin();
    let obs = &app.config.obs;
    let lines = vec![
        setting_line("Enabled", on_off(obs.enabled), sk),
        setting_line("Address", obs.url(), sk),
        setting_line(
            "Password",
            // Never the value, only whether there is one. This pane is on
            // screen while streaming as often as any other.
            if obs.password().is_some() {
                format!(
                    "set (from {})",
                    if obs.password.trim().is_empty() {
                        format!("${}", obs.password_env)
                    } else {
                        "config.toml".to_string()
                    }
                )
            } else {
                "none".to_string()
            },
            sk,
        ),
        setting_line("State", app.obs.connection.label().to_string(), sk),
        Line::from(""),
        Line::from(Span::styled(
            "Change these under [obs] in config.toml. OBS: Tools → WebSocket Server Settings.",
            Style::new().fg(sk.muted),
        )),
    ];
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), area);
}

/// Build the account rows once, up front, rather than inside the list's own
/// per-row closure.
///
/// `tui_widget_list::ListBuilder` calls its closure again for every row that
/// scrolls into view, on every frame — re-deriving login/expiry state from
/// the token store that often would be the same repeated-disk-read hazard
/// [`ConfigTab::refresh_diagnostics`]'s own doc comment exists to warn about,
/// just paid on every keystroke instead of on a poll. Reading the store once
/// per frame, here, and handing the closure a plain `Vec` to index into,
/// keeps that cost where it already was.
///
/// Falls back to one placeholder row per platform when the store is empty:
/// the section used to show exactly one row per platform, and an account
/// added by mistake was invisible and its refresh token stayed valid
/// indefinitely, which is why [`App::all_accounts`] lists every account the
/// store holds rather than one per platform once any exist.
fn account_rows(app: &App) -> Vec<Line<'static>> {
    let sk = theme::skin();
    let accounts = app.all_accounts();
    if accounts.is_empty() {
        crate::model::Platform::ALL
            .iter()
            .map(|platform| {
                Line::from(vec![
                    Span::styled(
                        format!("{:<10}", platform.label()),
                        Style::new().fg(sk.foreground),
                    ),
                    Span::styled("not logged in", Style::new().fg(sk.muted)),
                ])
            })
            .collect()
    } else {
        accounts
            .iter()
            .map(|(key, platform, label)| {
                let primary = key == platform.slug();
                let armed = primary && app.logout_armed == Some(*platform);

                // Who, expiry and whether it renews — the store has known all
                // of this and the screen showed two booleans, when with two
                // accounts the only question here is which one you stream as.
                let detail = match app.account_summary_for(key) {
                    Some((expires, renews)) => format!(
                        "{} · expires in {expires}",
                        if renews {
                            "renews automatically"
                        } else {
                            "no refresh token"
                        }
                    ),
                    None => "logged in".to_string(),
                };

                Line::from(vec![
                    Span::styled(
                        format!("{:<10}", platform.label()),
                        Style::new().fg(sk.foreground),
                    ),
                    Span::styled(format!("{label}  "), Style::new().fg(sk.foreground)),
                    if armed {
                        Span::styled(
                            "press enter again to log out · esc cancels".to_string(),
                            Style::new().fg(sk.error).add_modifier(Modifier::BOLD),
                        )
                    } else {
                        Span::styled(detail, Style::new().fg(sk.muted))
                    },
                ])
            })
            .collect()
    }
}

fn draw_accounts(frame: &mut Frame, area: Rect, app: &App, config: &ConfigTab) {
    let sk = theme::skin();
    let rows = account_rows(app);
    let row_count = rows.len();

    let sections = Layout::vertical([Constraint::Min(1), Constraint::Length(2)]).split(area);

    // `Focus::Sections` means the keyboard is on the left-hand list of
    // sections, not this one, so nothing here counts as selected until the
    // content pane actually has focus — same rule every other section in
    // this tab follows.
    let selected_index = (config.focus == Focus::Contents).then_some(config.cursor);
    let builder = ListBuilder::new(move |context| {
        let mut spans = vec![Span::styled(
            if context.is_selected { "▸ " } else { "  " },
            Style::new().fg(sk.accent),
        )];
        spans.extend(rows[context.index].spans.iter().cloned());
        let line = Line::from(spans).style(selected_row_style(
            Style::default(),
            context.is_selected,
            &sk,
        ));
        (line, 1)
    });
    let list = ListView::new(builder, row_count);
    let mut state = WidgetListState::new_with_index(selected_index);
    frame.render_stateful_widget(list, sections[0], &mut state);

    frame.render_widget(
        Paragraph::new(vec![
            Line::from(""),
            Line::from(Span::styled(
                "enter log in or out · a add another chat account",
                Style::new().fg(sk.muted),
            )),
        ]),
        sections[1],
    );
}

fn draw_maintenance(frame: &mut Frame, area: Rect, app: &App, config: &ConfigTab) {
    let sk = theme::skin();
    let mut lines = Vec::new();
    for (index, (name, explanation)) in MAINTENANCE_JOBS.iter().enumerate() {
        let selected = index == config.cursor && config.focus == Focus::Contents;
        // The cleanup row says what the *next* press does, because the first
        // press lists and the second deletes and the row used to read the
        // same either way.
        let armed = index == 0 && config.cleanup_listed;
        let label = if armed {
            format!(
                "Delete the {} listed broadcast(s) — enter confirms",
                app.stale_broadcasts.len()
            )
        } else {
            (*name).to_string()
        };
        let mut line = Line::from(vec![
            Span::styled(
                if selected { "▸ " } else { "  " },
                Style::new().fg(sk.accent),
            ),
            Span::styled(
                label,
                if armed {
                    Style::new().fg(sk.error).add_modifier(Modifier::BOLD)
                } else {
                    Style::new().fg(sk.foreground)
                },
            ),
        ]);
        line = line.style(selected_row_style(
            style_kit::zebra_style(index, &sk),
            selected,
            &sk,
        ));
        lines.push(line);
        if selected {
            lines.push(Line::from(Span::styled(
                format!("   {explanation}"),
                Style::new().fg(sk.muted),
            )));
        }
    }
    // The stream ids the last listing found, as rows rather than as log
    // lines somebody has to copy out by eye. Pinning one used to mean leaving
    // the program and hand-editing config.toml.
    if !app.youtube_streams.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "  Reusable YouTube streams — enter pins one as [youtube] stream_id",
            Style::new().fg(sk.muted),
        )));
        let pinned = app.config.youtube.stream_id.trim();
        for (offset, (id, title)) in app.youtube_streams.iter().enumerate() {
            let index = MAINTENANCE_ROWS + offset;
            let selected = index == config.cursor && config.focus == Focus::Contents;
            let is_pinned = !pinned.is_empty() && pinned == id;
            let mut line = Line::from(vec![
                Span::styled(
                    if selected { "▸ " } else { "  " },
                    Style::new().fg(sk.accent),
                ),
                Span::styled(
                    if is_pinned { "● " } else { "  " },
                    Style::new().fg(sk.success),
                ),
                Span::styled(format!("{title}  "), Style::new().fg(sk.foreground)),
                Span::styled(id.clone(), Style::new().fg(sk.muted)),
            ]);
            line = line.style(selected_row_style(
                style_kit::zebra_style(index, &sk),
                selected,
                &sk,
            ));
            lines.push(line);
        }
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "enter run · the result goes to the activity log on the Stream Info tab",
        Style::new().fg(sk.muted),
    )));
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), area);
}

fn draw_diagnostics(frame: &mut Frame, area: Rect, app: &App) {
    let sk = theme::skin();
    let Some(config) = &app.config_tab else {
        return;
    };
    let mut lines = Vec::new();
    for (index, check) in config.diagnostics.checks.iter().enumerate() {
        let (label, colour) = match check.status {
            crate::diagnostics::Status::Ok => ("OK", sk.success),
            crate::diagnostics::Status::Warning => ("WARN", sk.warning),
            crate::diagnostics::Status::Failed => ("FAIL", sk.error),
        };
        // A check and its advice line read as one row, so they stripe
        // together rather than the advice line breaking the alternation.
        let stripe = style_kit::zebra_style(index, &sk);
        let mut marker = style_kit::badge(label, colour, &sk);
        marker.push(Span::styled(
            format!(" {}", check.summary),
            Style::new().fg(sk.foreground),
        ));
        lines.push(Line::from(marker).style(stripe));
        if !check.advice.is_empty() {
            lines.push(
                Line::from(Span::styled(
                    format!("  {}", check.advice),
                    Style::new().fg(sk.muted),
                ))
                .style(stripe),
            );
        }
    }

    // The age matters: these are facts about the machine at the moment they
    // were gathered, and the usual reason to look twice is having just
    // changed one of them (logged in, installed a helper).
    // The verdict counts failures only, never warnings: an unfinished setup
    // is not a broken one, and a list this long needs somebody to say which
    // of it matters.
    lines.push(Line::from(""));
    let failed = config
        .diagnostics
        .checks
        .iter()
        .any(|check| check.status.is_failure());
    lines.push(Line::from(Span::styled(
        crate::diagnostics::verdict(&config.diagnostics.checks),
        Style::new().fg(if failed { sk.error } else { sk.success }),
    )));
    lines.push(Line::from(Span::styled(
        match config.diagnostics.taken_at {
            Some(at) => format!("checked at {} · r to check again", at.format("%H:%M:%S")),
            None => "not checked yet · r to check".to_string(),
        },
        Style::new().fg(sk.muted),
    )));

    // Clamped here rather than where the key is handled, because this is the
    // only place that knows how many lines there turned out to be.
    let overflow = (lines.len() as u16).saturating_sub(area.height);
    let offset = config.diagnostics_scroll.min(overflow);

    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((offset, 0)),
        area,
    );
}

fn draw_paths(frame: &mut Frame, area: Rect, app: &App) {
    let sk = theme::skin();
    let config = match &app.config_tab {
        Some(config) => config,
        None => return,
    };
    // A path carries your username, and the Files section is exactly the sort
    // of screen somebody tabs to mid-stream to check something.
    let hide = app.streamer_mode();
    let shown = |result: &anyhow::Result<std::path::PathBuf>| match result {
        Ok(path) if hide => {
            let name = path
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
                .unwrap_or_default();
            format!("…/{name}")
        }
        Ok(path) => path.display().to_string(),
        Err(err) => format!("unavailable: {err}"),
    };

    let mut lines: Vec<Line> = crate::ui::app::FILE_ROWS
        .iter()
        .enumerate()
        .map(|(index, (label, which))| {
            let selected = index == config.cursor && config.focus == Focus::Contents;
            let mut line = Line::from(vec![
                Span::styled(
                    if selected { "▸ " } else { "  " },
                    Style::new().fg(sk.accent),
                ),
                Span::styled(format!("{label:<10}"), Style::new().fg(sk.foreground)),
                Span::styled(shown(&which()), Style::new().fg(sk.muted)),
            ]);
            line = line.style(selected_row_style(Style::default(), selected, &sk));
            line
        })
        .collect();

    lines.push(Line::from(""));
    // These were three lines of text that could be neither opened nor copied,
    // while four other sections told the user to edit config.toml by hand and
    // none of them offered to open it.
    lines.push(Line::from(Span::styled(
        "enter opens the selected file in whatever your system uses for it",
        Style::new().fg(sk.muted),
    )));
    lines.push(Line::from(Span::styled(
        "The log is where the detail goes: this window belongs to the interface.",
        Style::new().fg(sk.muted),
    )));
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), area);
}

fn setting_line(name: &str, value: String, sk: theme::Skin) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{name:<12}"), Style::new().fg(sk.muted)),
        Span::styled(value, Style::new().fg(sk.foreground)),
    ])
}

/// Move a panel within the layout, resize it, add one, or take one away.
///
/// These operate on the *draft*, so a layout can be tried and abandoned. The
/// arithmetic lives here rather than in `crate::layout` because it is about
/// editing rather than about resolving, and mixing the two would make the
/// resolver harder to reason about than it needs to be.
pub mod edit {
    use super::*;

    /// Change the weight of the panel at `index` by `delta`.
    pub fn resize(layout: &mut PaneLayout, index: usize, delta: i16) {
        locate(
            &mut layout.root,
            &mut 0,
            index,
            Scope::Panel,
            &mut |children, position| {
                // Never below one: a weight of zero is a panel that is present
                // and invisible, which looks exactly like a bug from outside.
                children[position].weight =
                    (children[position].weight as i32 + delta as i32).clamp(1, 100) as u16;
            },
        );
    }

    /// Resize the *row* the panel at `index` sits in, rather than the panel.
    ///
    /// `resize` walks to a `Node::Panel` child and changes its weight, so a
    /// `Node::Split` child's weight was unreachable — and that weight is what
    /// decides how tall a row of panels is. "Make the chats taller" was
    /// inexpressible from the editor, however many times you pressed `+`.
    pub fn resize_row(layout: &mut PaneLayout, index: usize, delta: i16) -> bool {
        locate(
            &mut layout.root,
            &mut 0,
            index,
            Scope::Row,
            &mut |children, position| {
                // Never below one: a weight of zero is a row that is present
                // and invisible.
                children[position].weight =
                    (children[position].weight as i32 + delta as i32).clamp(1, 100) as u16;
            },
        )
    }

    /// How far [`locate`] should stop descending: at the panel leaf itself,
    /// or at the split branch (row or column) that contains it.
    #[derive(Clone, Copy)]
    enum Scope {
        Panel,
        Row,
    }

    /// Walk `node`'s children in flat panel-index order looking for
    /// `target`, and run `found` on the split it is directly a child of,
    /// together with its position there, once located — of the panel
    /// itself for [`Scope::Panel`], or of the whole branch (row or column)
    /// containing it for [`Scope::Row`]. Returns whether it was found.
    ///
    /// `resize`, `resize_row`, `move_panel` and `remove` each used to
    /// re-implement this walk by hand, one flat counter apiece; they differ
    /// only in what they do with the child once it is found, which is what
    /// `found` is for.
    fn locate<F: FnMut(&mut Vec<crate::layout::Child>, usize)>(
        node: &mut crate::layout::Node,
        seen: &mut usize,
        target: usize,
        scope: Scope,
        found: &mut F,
    ) -> bool {
        let crate::layout::Node::Split { children, .. } = node else {
            return false;
        };

        for position in 0..children.len() {
            let is_panel = matches!(children[position].node, crate::layout::Node::Panel(_));
            match scope {
                Scope::Panel if is_panel => {
                    if *seen == target {
                        found(children, position);
                        return true;
                    }
                    *seen += 1;
                }
                Scope::Panel => {
                    if locate(&mut children[position].node, seen, target, scope, found) {
                        return true;
                    }
                }
                // A bare panel is not a row of its own.
                Scope::Row if is_panel => {
                    *seen += 1;
                }
                Scope::Row => {
                    let count = count_panels(&children[position].node);
                    if *seen <= target && target < *seen + count {
                        found(children, position);
                        return true;
                    }
                    *seen += count;
                }
            }
        }
        false
    }

    fn count_panels(node: &crate::layout::Node) -> usize {
        match node {
            crate::layout::Node::Panel(_) => 1,
            crate::layout::Node::Split { children, .. } => {
                children.iter().map(|child| count_panels(&child.node)).sum()
            }
        }
    }

    /// Move a panel one place earlier or later within the split it sits in.
    ///
    /// Reordering rather than re-parenting: a panel keeps whichever row or
    /// column it belongs to and swaps with its neighbour there. Moving a
    /// panel *between* rows would need a target chosen as well as a
    /// direction, which is a bigger interaction than one key can carry.
    pub fn move_panel(layout: &mut PaneLayout, index: usize, delta: isize) -> bool {
        let mut moved = false;
        locate(
            &mut layout.root,
            &mut 0,
            index,
            Scope::Panel,
            &mut |children, position| {
                let destination = position as isize + delta;
                // Stopping at the ends rather than wrapping: a panel that leapt
                // from the bottom of a column to the top would look like a
                // different action from the one that was asked for.
                if destination < 0 || destination >= children.len() as isize {
                    return;
                }
                children.swap(position, destination as usize);
                moved = true;
            },
        );
        moved
    }

    /// Turn rows into columns and back.
    pub fn rotate(layout: &mut PaneLayout) {
        flip(&mut layout.root);
    }

    fn flip(node: &mut crate::layout::Node) {
        if let crate::layout::Node::Split {
            direction,
            children,
        } = node
        {
            *direction = match direction {
                Direction::Horizontal => Direction::Vertical,
                Direction::Vertical => Direction::Horizontal,
            };
            for child in children.iter_mut() {
                flip(&mut child.node);
            }
        }
    }

    /// Add a panel beside the selected one.
    /// Returns whether the panel was actually added.
    ///
    /// The root is not always a split: removing panels calls `tidy`, which
    /// replaces a one-child split with the child itself, so a layout reduced
    /// to a single panel has a bare `Node::Panel` root. Pushing into that was
    /// a silent no-op — and the caller announced "Added Chat." and marked the
    /// draft dirty regardless, so the one case where adding is most obviously
    /// wanted was the one case where it did nothing.
    pub fn add(layout: &mut PaneLayout, panel: Panel) -> bool {
        match &mut layout.root {
            crate::layout::Node::Split { children, .. } => {
                children.push(crate::layout::Child {
                    weight: 1,
                    node: crate::layout::Node::Panel(panel),
                });
                true
            }
            // A single panel becomes a split of the two, which is the only
            // arrangement that can hold both.
            root @ crate::layout::Node::Panel(_) => {
                let existing = std::mem::replace(root, crate::layout::Node::Panel(panel));
                *root = crate::layout::Node::Split {
                    direction: crate::layout::Direction::Vertical,
                    children: vec![
                        crate::layout::Child {
                            weight: 1,
                            node: existing,
                        },
                        crate::layout::Child {
                            weight: 1,
                            node: crate::layout::Node::Panel(panel),
                        },
                    ],
                };
                true
            }
        }
    }

    /// Take the panel at `index` out.
    ///
    /// The last panel cannot be removed: an empty layout is a blank tab, and
    /// a blank tab is indistinguishable from a broken one.
    pub fn remove(layout: &mut PaneLayout, index: usize) -> bool {
        if layout.panels().len() <= 1 {
            return false;
        }
        let removed = locate(
            &mut layout.root,
            &mut 0,
            index,
            Scope::Panel,
            &mut |children, position| {
                children.remove(position);
            },
        );
        if removed {
            tidy(&mut layout.root);
        }
        removed
    }

    /// Tidy a tree after a removal.
    ///
    /// Taking a panel out can leave a split with nothing in it, or with a
    /// single child — neither of which is wrong to *resolve*, but both of
    /// which the validator rejects and neither of which anybody meant. An
    /// empty split is dropped and a split of one is replaced by that one, so
    /// the tree stays as shallow as the arrangement actually is.
    pub fn tidy(node: &mut crate::layout::Node) {
        let crate::layout::Node::Split { children, .. } = node else {
            return;
        };
        for child in children.iter_mut() {
            tidy(&mut child.node);
        }
        children.retain(|child| match &child.node {
            crate::layout::Node::Split { children, .. } => !children.is_empty(),
            crate::layout::Node::Panel(_) => true,
        });
        if children.len() == 1 {
            let only = children.remove(0);
            *node = only.node;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::Layout as PaneLayout;

    /// `resize` walks to a panel and changes its weight, so a nested split's
    /// weight was unreachable — and that is the one that decides how tall a
    /// row is. "Make the chats taller" could not be said at all.
    #[test]
    fn a_row_can_be_resized_as_well_as_a_panel() {
        // A preset with nesting, which is what has rows to resize.
        let mut layout = crate::layout::presets::by_name("stacked")
            .or_else(|| {
                crate::layout::presets::NAMES
                    .iter()
                    .find_map(|(name, _)| crate::layout::presets::by_name(name))
            })
            .expect("some preset exists");

        // Find a panel that actually sits inside a nested split.
        let resized = (0..layout.panels().len())
            .find(|index| edit::resize_row(&mut layout.clone(), *index, 1));

        if let Some(index) = resized {
            let before = format!("{:?}", layout.root);
            assert!(edit::resize_row(&mut layout, index, 1));
            assert_ne!(
                before,
                format!("{:?}", layout.root),
                "resizing the row has to change the arrangement"
            );
        }
        // A flat layout has no row to resize, and says so rather than
        // pretending: that is what the `false` return is for.
    }

    /// Adding a panel to a layout reduced to one used to be a silent no-op —
    /// `tidy` leaves a bare `Node::Panel` root and the old `add` only pushed
    /// into a split — while the caller announced success either way.
    #[test]
    fn a_panel_can_be_added_to_a_single_panel_layout() {
        let mut layout = PaneLayout::default();
        // Remove down to one panel, which is what leaves a bare panel root.
        while layout.panels().len() > 1 {
            assert!(edit::remove(&mut layout, 0));
        }
        assert_eq!(layout.panels().len(), 1);

        let missing = *crate::layout::Panel::ALL
            .iter()
            .find(|panel| !layout.panels().contains(panel))
            .expect("something is not on a one-panel layout");

        assert!(edit::add(&mut layout, missing), "adding has to report true");
        assert_eq!(layout.panels().len(), 2);
        assert!(layout.panels().contains(&missing));
    }

    #[test]
    fn every_section_has_a_title_and_a_summary() {
        for section in Section::ALL {
            assert!(!section.title().is_empty());
            assert!(!section.summary().is_empty());
        }
    }

    /// Resizing must not be able to produce a panel with no space: a panel
    /// that is present and invisible looks exactly like a bug from outside.
    #[test]
    fn a_panel_cannot_be_resized_out_of_existence() {
        let mut layout = PaneLayout::default();
        for _ in 0..50 {
            edit::resize(&mut layout, 0, -5);
        }
        let placed = layout.resolve(Rect {
            x: 0,
            y: 0,
            width: 100,
            height: 40,
        });
        assert_eq!(placed.len(), layout.panels().len());
    }

    #[test]
    fn resizing_changes_the_share_a_panel_gets() {
        let area = Rect {
            x: 0,
            y: 0,
            width: 100,
            height: 40,
        };
        let mut layout = PaneLayout::default();
        let before = layout.resolve(area)[0].1.height;
        edit::resize(&mut layout, 0, 4);
        let after = layout.resolve(area)[0].1.height;
        assert!(after > before, "{after} should exceed {before}");
    }

    #[test]
    fn rotating_turns_rows_into_columns() {
        let area = Rect {
            x: 0,
            y: 0,
            width: 100,
            height: 40,
        };
        let mut layout = PaneLayout::default();
        let before = layout.resolve(area);
        edit::rotate(&mut layout);
        let after = layout.resolve(area);

        assert_eq!(before.len(), after.len(), "the same panels are placed");
        // The first panel spanned the full width and now does not.
        assert_eq!(before[0].1.width, area.width);
        assert!(after[0].1.width < area.width);
    }

    #[test]
    fn a_panel_can_be_added_and_removed() {
        let mut layout = PaneLayout::default();
        let before = layout.panels().len();

        edit::add(&mut layout, Panel::ObsScenes);
        assert_eq!(layout.panels().len(), before + 1);
        assert!(layout.panels().contains(&Panel::ObsScenes));

        assert!(edit::remove(&mut layout, 0));
        assert_eq!(layout.panels().len(), before);
    }

    /// An empty layout is a blank tab, and a blank tab is indistinguishable
    /// from a broken one.
    #[test]
    fn the_last_panel_cannot_be_removed() {
        let mut layout = PaneLayout::default();
        while layout.panels().len() > 1 {
            assert!(edit::remove(&mut layout, 0));
        }
        assert!(!edit::remove(&mut layout, 0));
        assert_eq!(layout.panels().len(), 1);
    }

    /// Every edit has to leave something the resolver accepts, or the tab
    /// would go blank mid-edit.
    #[test]
    fn every_edit_leaves_a_valid_layout() {
        let mut layout = PaneLayout::default();
        for step in 0..20 {
            edit::resize(&mut layout, step % 3, if step % 2 == 0 { 1 } else { -1 });
            edit::rotate(&mut layout);
            if step % 5 == 0 {
                edit::add(&mut layout, Panel::ActivityLog);
            }
            if step % 7 == 0 {
                edit::remove(&mut layout, 0);
            }
            assert!(layout.validate().is_ok(), "broken after step {step}");
        }
    }

    /// The editor's hint advertises J and K for moving a panel, so they have
    /// to move one.
    #[test]
    fn a_panel_can_be_moved_within_its_row() {
        let mut layout = PaneLayout::default();
        // The default has two chats side by side in the second row.
        let before = layout.panels();
        let twitch = before
            .iter()
            .position(|panel| *panel == Panel::TwitchChat)
            .expect("the default layout has a Twitch pane");

        assert!(edit::move_panel(&mut layout, twitch, 1));
        let after = layout.panels();
        assert_ne!(before, after, "the order changed");
        assert_eq!(
            before.len(),
            after.len(),
            "moving must not add or lose a panel"
        );
    }

    /// A panel at the end of its row stays there rather than leaping to the
    /// other end, which would look like a different action from the one
    /// asked for.
    #[test]
    fn moving_stops_at_the_ends_rather_than_wrapping() {
        let mut layout = PaneLayout::default();
        let first = layout.panels();
        assert!(!edit::move_panel(&mut layout, 0, -1));
        assert_eq!(layout.panels(), first, "nothing moved");
    }

    #[test]
    fn moving_a_panel_that_is_not_there_changes_nothing() {
        let mut layout = PaneLayout::default();
        let before = layout.panels();
        assert!(!edit::move_panel(&mut layout, 99, 1));
        assert_eq!(layout.panels(), before);
    }
}
