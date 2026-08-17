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

use super::app::App;
use crate::layout::{Direction, Layout as PaneLayout, Panel};
use crate::theme;

/// Which part of the configuration is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    Layout,
    Appearance,
    Notifications,
    Keys,
    Obs,
    Accounts,
    Maintenance,
    Diagnostics,
    Paths,
}

impl Section {
    pub const ALL: [Section; 9] = [
        Section::Layout,
        Section::Appearance,
        Section::Notifications,
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
        }
    }

    /// How many rows the chosen section offers, for clamping the cursor.
    pub fn rows(&self, app: &App) -> usize {
        match self.section {
            Section::Layout => self.draft.panels().len(),
            Section::Appearance => APPEARANCE_ROWS,
            Section::Notifications => NOTIFICATION_ROWS,
            Section::Keys => app.keymap.all().len(),
            Section::Obs => 0,
            Section::Accounts => crate::model::Platform::ALL.len(),
            Section::Maintenance => MAINTENANCE_ROWS,
            Section::Diagnostics => 0,
            Section::Paths => 0,
        }
    }
}

/// How many settings the Appearance section lists.
pub const APPEARANCE_ROWS: usize = 7;

/// How many switches the Notifications section lists.
pub const NOTIFICATION_ROWS: usize = 8;

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
            if selected && config.focus == Focus::Sections {
                line = line.style(Style::new().bg(sk.selection));
            }
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
        Section::Keys => draw_keys(frame, inner, app, config),
        Section::Obs => draw_obs(frame, inner, app),
        Section::Accounts => draw_accounts(frame, inner, app, config),
        Section::Maintenance => draw_maintenance(frame, inner, config),
        Section::Diagnostics => draw_diagnostics(frame, inner, app),
        Section::Paths => draw_paths(frame, inner),
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

    draw_preview(frame, rows[0], &config.draft);

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
            if selected {
                line = line.style(Style::new().bg(sk.selection));
            }
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
fn draw_preview(frame: &mut Frame, area: Rect, layout: &PaneLayout) {
    let sk = theme::skin();
    for (panel, rect) in layout.resolve(area) {
        if rect.width < 2 || rect.height < 1 {
            continue;
        }
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::new().fg(sk.border));
        let inner = block.inner(rect);
        frame.render_widget(block, rect);
        if inner.height > 0 {
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    panel.title().to_string(),
                    Style::new().fg(sk.accent),
                )))
                .wrap(Wrap { trim: true }),
                inner,
            );
        }
    }
}

fn draw_appearance(frame: &mut Frame, area: Rect, app: &App, config: &ConfigTab) {
    let sk = theme::skin();
    let appearance = &app.config.appearance;
    let settings: [(&str, String); APPEARANCE_ROWS] = [
        ("Theme", appearance.theme.clone()),
        ("Animations", appearance.animations.clone()),
        ("Splash screen", on_off(appearance.splash)),
        ("Mouse", on_off(appearance.mouse)),
        ("Telemetry", on_off(appearance.telemetry)),
        // Named for what it is, because the Notifications section next door
        // is about the *desktop's* pop-ups and confusing the two would send
        // somebody to the wrong switch.
        ("In-app pop-ups", on_off(appearance.toasts)),
        (
            "Terminal background",
            on_off(appearance.terminal_background),
        ),
    ];

    let lines: Vec<Line> = settings
        .iter()
        .enumerate()
        .map(|(index, (name, value))| {
            let selected = index == config.cursor && config.focus == Focus::Contents;
            let mut line = Line::from(vec![
                Span::styled(
                    if selected { "▸ " } else { "  " },
                    Style::new().fg(sk.accent),
                ),
                Span::styled(format!("{name:<22}"), Style::new().fg(sk.foreground)),
                Span::styled(value.clone(), Style::new().fg(sk.accent)),
            ]);
            if selected {
                line = line.style(Style::new().bg(sk.selection));
            }
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
    let rows: [(&str, String); NOTIFICATION_ROWS] = [
        ("Desktop notifications", on_off(settings.enabled)),
        ("Raids", on_off(settings.raids)),
        ("Subscriptions & gifts", on_off(settings.subscriptions)),
        ("Cheers & bits", on_off(settings.cheers)),
        ("Super Chats", on_off(settings.paid)),
        ("Memberships", on_off(settings.memberships)),
        ("Stream started/stopped", on_off(settings.stream_state)),
        (
            "Only when chat is hidden",
            on_off(settings.only_when_hidden),
        ),
    ];

    let mut lines: Vec<Line> = rows
        .iter()
        .enumerate()
        .map(|(index, (name, value))| {
            let selected = index == config.cursor && config.focus == Focus::Contents;
            // Every switch below the first is meaningless while the master
            // switch is off, and greying them says that without hiding them.
            let dimmed = index > 0 && !settings.enabled;
            let name_colour = if dimmed { sk.muted } else { sk.foreground };
            let value_colour = if dimmed { sk.muted } else { sk.accent };
            let mut line = Line::from(vec![
                Span::styled(
                    if selected { "▸ " } else { "  " },
                    Style::new().fg(sk.accent),
                ),
                Span::styled(format!("{name:<26}"), Style::new().fg(name_colour)),
                Span::styled(value.clone(), Style::new().fg(value_colour)),
            ]);
            if selected {
                line = line.style(Style::new().bg(sk.selection));
            }
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
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "enter change · every change is saved straight away",
        Style::new().fg(sk.muted),
    )));

    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }), area);
}

fn on_off(value: bool) -> String {
    if value { "on" } else { "off" }.to_string()
}

/// Every binding, so the answer to "what key does that?" is in the program
/// rather than in a document.
fn draw_keys(frame: &mut Frame, area: Rect, app: &App, config: &ConfigTab) {
    let sk = theme::skin();
    let bindings = app.keymap.all();
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
            if selected {
                line = line.style(Style::new().bg(sk.selection));
            }
            line
        })
        .collect();

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "Change these under [keys] in config.toml. <Leader>? shows them as a map.",
        Style::new().fg(sk.muted),
    )));
    frame.render_widget(Paragraph::new(lines), area);
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

fn draw_accounts(frame: &mut Frame, area: Rect, app: &App, config: &ConfigTab) {
    let sk = theme::skin();
    let mut lines: Vec<Line> = crate::model::Platform::ALL
        .iter()
        .enumerate()
        .map(|(index, platform)| {
            let selected = index == config.cursor && config.focus == Focus::Contents;
            let state = match app.logged_in.get(platform) {
                Some(true) => ("logged in", sk.success),
                _ => ("not logged in", sk.muted),
            };
            let mut line = Line::from(vec![
                Span::styled(
                    if selected { "▸ " } else { "  " },
                    Style::new().fg(sk.accent),
                ),
                Span::styled(
                    format!("{:<10}", platform.label()),
                    Style::new().fg(sk.foreground),
                ),
                Span::styled(state.0, Style::new().fg(state.1)),
            ]);
            if selected {
                line = line.style(Style::new().bg(sk.selection));
            }
            line
        })
        .collect();

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "enter log in or out · a add another chat account",
        Style::new().fg(sk.muted),
    )));
    frame.render_widget(Paragraph::new(lines), area);
}

fn draw_maintenance(frame: &mut Frame, area: Rect, config: &ConfigTab) {
    let sk = theme::skin();
    let mut lines = Vec::new();
    for (index, (name, explanation)) in MAINTENANCE_JOBS.iter().enumerate() {
        let selected = index == config.cursor && config.focus == Focus::Contents;
        let mut line = Line::from(vec![
            Span::styled(
                if selected { "▸ " } else { "  " },
                Style::new().fg(sk.accent),
            ),
            Span::styled((*name).to_string(), Style::new().fg(sk.foreground)),
        ]);
        if selected {
            line = line.style(Style::new().bg(sk.selection));
        }
        lines.push(line);
        if selected {
            lines.push(Line::from(Span::styled(
                format!("   {explanation}"),
                Style::new().fg(sk.muted),
            )));
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
    let mut lines = Vec::new();
    for check in crate::diagnostics::run(&app.config) {
        let (marker, colour) = match check.status {
            crate::diagnostics::Status::Ok => ("[ ok ]", sk.success),
            crate::diagnostics::Status::Warning => ("[warn]", sk.warning),
            crate::diagnostics::Status::Failed => ("[FAIL]", sk.error),
        };
        lines.push(Line::from(vec![
            Span::styled(format!("{marker} "), Style::new().fg(colour)),
            Span::styled(check.summary.clone(), Style::new().fg(sk.foreground)),
        ]));
        if !check.advice.is_empty() {
            lines.push(Line::from(Span::styled(
                format!("       {}", check.advice),
                Style::new().fg(sk.muted),
            )));
        }
    }
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), area);
}

fn draw_paths(frame: &mut Frame, area: Rect) {
    let sk = theme::skin();
    let path = |result: anyhow::Result<std::path::PathBuf>| match result {
        Ok(path) => path.display().to_string(),
        Err(err) => format!("unavailable: {err}"),
    };
    let lines = vec![
        setting_line("Config", path(crate::paths::config_file()), sk),
        setting_line("Logins", path(crate::paths::token_file()), sk),
        setting_line("Log", path(crate::paths::log_file()), sk),
        Line::from(""),
        Line::from(Span::styled(
            "The log is where the detail goes: this window belongs to the interface.",
            Style::new().fg(sk.muted),
        )),
    ];
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
        walk_weights(&mut layout.root, &mut 0, index, delta);
    }

    fn walk_weights(node: &mut crate::layout::Node, seen: &mut usize, target: usize, delta: i16) {
        match node {
            crate::layout::Node::Panel(_) => {
                *seen += 1;
            }
            crate::layout::Node::Split { children, .. } => {
                for child in children.iter_mut() {
                    if matches!(child.node, crate::layout::Node::Panel(_)) {
                        if *seen == target {
                            // Never below one: a weight of zero is a panel
                            // that is present and invisible, which looks
                            // exactly like a bug from the outside.
                            child.weight =
                                (child.weight as i32 + delta as i32).clamp(1, 100) as u16;
                        }
                        *seen += 1;
                    } else {
                        walk_weights(&mut child.node, seen, target, delta);
                    }
                }
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
        let mut seen = 0;
        move_walk(&mut layout.root, &mut seen, index, delta)
    }

    fn move_walk(
        node: &mut crate::layout::Node,
        seen: &mut usize,
        target: usize,
        delta: isize,
    ) -> bool {
        let crate::layout::Node::Split { children, .. } = node else {
            return false;
        };
        let mut swap = None;
        for (position, child) in children.iter_mut().enumerate() {
            match &mut child.node {
                crate::layout::Node::Panel(_) => {
                    if *seen == target {
                        swap = Some(position);
                    }
                    *seen += 1;
                }
                other => {
                    if move_walk(other, seen, target, delta) {
                        return true;
                    }
                }
            }
        }

        let Some(position) = swap else { return false };
        let destination = position as isize + delta;
        // Stopping at the ends rather than wrapping: a panel that leapt from
        // the bottom of a column to the top would look like a different
        // action from the one that was asked for.
        if destination < 0 || destination >= children.len() as isize {
            return false;
        }
        children.swap(position, destination as usize);
        true
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
    pub fn add(layout: &mut PaneLayout, panel: Panel) {
        if let crate::layout::Node::Split { children, .. } = &mut layout.root {
            children.push(crate::layout::Child {
                weight: 1,
                node: crate::layout::Node::Panel(panel),
            });
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
        let mut seen = 0;
        let removed = remove_walk(&mut layout.root, &mut seen, index);
        if removed {
            tidy(&mut layout.root);
        }
        removed
    }

    fn remove_walk(node: &mut crate::layout::Node, seen: &mut usize, target: usize) -> bool {
        if let crate::layout::Node::Split { children, .. } = node {
            let mut remove_at = None;
            for (position, child) in children.iter_mut().enumerate() {
                match &mut child.node {
                    crate::layout::Node::Panel(_) => {
                        if *seen == target {
                            remove_at = Some(position);
                        }
                        *seen += 1;
                    }
                    other => {
                        if remove_walk(other, seen, target) {
                            return true;
                        }
                    }
                }
            }
            if let Some(position) = remove_at {
                children.remove(position);
                return true;
            }
        }
        false
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
