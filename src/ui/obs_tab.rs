//! The OBS tab: scenes on the left, microphones on the right, what OBS is
//! doing along the top.
//!
//! The layout follows what someone actually reaches for mid-stream. Scene
//! switching is the commonest action by a wide margin, so it gets the left
//! column where the eye lands first. Muting a microphone is the second
//! commonest and the most urgent — it is the one you need *now*, not in a
//! moment — so it is one key away rather than behind a menu. Everything else
//! is a status line.
//!
//! Nothing here talks to OBS. It draws [`crate::obs::state::ObsState`], which
//! the connection task keeps up to date, so a slow or missing OBS shows as
//! stale or absent data rather than as a slow interface.

use ratatui::prelude::*;
use ratatui::widgets::{Block, BorderType, Borders, Paragraph, Wrap};
use tui_widget_list::{ListBuildContext, ListBuilder, ListState, ListView};

use super::app::{App, ObsFocus};
use super::style_kit;
use crate::obs::state::{AudioInput, Connection, ObsState, Scene};
use crate::theme;

/// Draw the whole tab.
pub fn draw(frame: &mut Frame, area: Rect, app: &App) {
    let areas = Layout::vertical([
        Constraint::Length(4), // what OBS is doing
        Constraint::Min(0),    // scenes and audio
        Constraint::Length(4), // performance
    ])
    .split(area);

    draw_status(frame, areas[0], &app.obs);

    let columns = Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(areas[1]);
    draw_scenes(frame, columns[0], app);
    draw_audio(frame, columns[1], app);

    draw_stats(frame, areas[2], &app.obs);
}

/// The top strip: connected or not, what is live, what is recording.
fn draw_status(frame: &mut Frame, area: Rect, obs: &ObsState) {
    draw_status_lines(frame, area, obs);
}

/// The status strip on its own, for the Combined tab to place.
pub fn draw_status_lines(frame: &mut Frame, area: Rect, obs: &ObsState) {
    let sk = theme::skin();

    // The four connection states are also the four tones `badge`/`dot` draw
    // from everywhere else, so the same colour that used to live only in this
    // glyph now marks the pill too — one state, one colour, wherever it shows
    // up on screen.
    let colour = match &obs.connection {
        Connection::Connected => sk.success,
        Connection::Connecting | Connection::Reconnecting { .. } => sk.warning,
        Connection::Failed(_) => sk.error,
        Connection::Idle => sk.muted,
    };

    let mut first = style_kit::badge("OBS", colour, &sk);
    first.push(Span::styled(
        format!(" {}", obs.connection.label()),
        Style::new().fg(sk.foreground),
    ));
    if let Some(version) = &obs.obs_version {
        first.push(Span::styled(
            format!("  ·  Studio {version}"),
            Style::new().fg(sk.muted),
        ));
    }
    // Whatever the connection has to say — a refused password reaches the
    // screen here whether the task has given up on it or is still retrying.
    if let Some(reason) = obs.connection.detail() {
        let colour = if matches!(obs.connection, Connection::Failed(_)) {
            sk.error
        } else {
            sk.warning
        };
        first.push(Span::styled(
            format!("  ·  {reason}"),
            Style::new().fg(colour),
        ));
    }

    // The live indicators. Streaming and recording are separate on purpose:
    // recording a stream you are not broadcasting, and broadcasting without
    // recording, are both ordinary, and a single "live" light would hide
    // which of them is happening.
    let mut second = output_span("STREAM", obs.streaming, false, obs.stream_duration, sk);
    second.push(Span::raw("   "));
    second.extend(output_span(
        "RECORD",
        obs.recording,
        obs.record_paused,
        obs.record_duration,
        sk,
    ));
    second.extend(vec![
        Span::raw("   "),
        Span::styled(
            match obs.stream_bitrate_kbps {
                Some(kbps) => format!("{kbps:.0} kb/s"),
                None => String::new(),
            },
            Style::new().fg(sk.muted),
        ),
        // The file the last recording wrote. "Which take was that" is asked
        // the moment recording stops, and the path arrives on that very
        // event — it was parsed away with the rest of the payload.
        Span::styled(
            match (&obs.last_recording, obs.recording) {
                (Some(path), false) => {
                    let name = std::path::Path::new(path)
                        .file_name()
                        .map(|name| name.to_string_lossy().to_string())
                        .unwrap_or_else(|| path.clone());
                    format!("   saved {name}")
                }
                _ => String::new(),
            },
            Style::new().fg(sk.muted),
        ),
    ]);

    let third = vec![
        Span::styled("scene ", Style::new().fg(sk.muted)),
        Span::styled(
            obs.current_scene.clone().unwrap_or_else(|| "—".to_string()),
            Style::new().fg(sk.accent).add_modifier(Modifier::BOLD),
        ),
        Span::styled("   profile ", Style::new().fg(sk.muted)),
        Span::styled(
            obs.current_profile
                .clone()
                .unwrap_or_else(|| "—".to_string()),
            Style::new().fg(sk.foreground),
        ),
        Span::styled("   collection ", Style::new().fg(sk.muted)),
        Span::styled(
            obs.current_scene_collection
                .clone()
                .unwrap_or_else(|| "—".to_string()),
            Style::new().fg(sk.foreground),
        ),
    ];

    frame.render_widget(
        Paragraph::new(vec![
            Line::from(first),
            Line::from(second),
            Line::from(third),
        ])
        .block(
            Block::default()
                .borders(Borders::BOTTOM)
                .border_style(Style::new().fg(sk.border)),
        ),
        area,
    );
}

/// One output indicator, e.g. a `STREAM` badge followed by `on 01:23:45`.
///
/// Live and paused get an actual pill, not just bold text, so "is this
/// broadcasting right now" reads as a shape at a glance rather than requiring
/// the colour of plain text to be picked out from the rest of the line. Off
/// stays a hollow dot plus plain text — a badge for "nothing is happening" is
/// a badge with nothing to say.
fn output_span(
    label: &str,
    active: bool,
    paused: bool,
    duration: Option<std::time::Duration>,
    sk: theme::Skin,
) -> Vec<Span<'static>> {
    if !active {
        return vec![
            Span::styled(
                format!("{} ", style_kit::DOT_HOLLOW),
                Style::new().fg(sk.muted),
            ),
            Span::styled(format!("{label} off"), Style::new().fg(sk.muted)),
        ];
    }
    let elapsed = duration.map(format_duration).unwrap_or_default();
    let (colour, word) = if paused {
        (sk.warning, "paused")
    } else {
        (sk.error, "on")
    };
    let mut spans = style_kit::badge(label, colour, &sk);
    spans.push(Span::styled(
        format!(" {word} {elapsed}").trim_end().to_string(),
        Style::new().fg(colour).add_modifier(Modifier::BOLD),
    ));
    spans
}

/// `hh:mm:ss`, or `mm:ss` under an hour.
fn format_duration(duration: std::time::Duration) -> String {
    let total = duration.as_secs();
    let (hours, minutes, seconds) = (total / 3600, (total % 3600) / 60, total % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

fn draw_scenes(frame: &mut Frame, area: Rect, app: &App) {
    let sk = theme::skin();
    let focused = app.obs_focus == ObsFocus::Scenes;
    let block = pane_block("Scenes", focused, sk);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    draw_scene_list(frame, inner, app);
}

/// The scene list on its own, with no border of its own.
///
/// The Combined tab draws its own frames around whatever the layout places,
/// so the panels it can hold have to be available without one.
pub fn draw_scene_list(frame: &mut Frame, inner: Rect, app: &App) {
    let sk = theme::skin();
    let focused = app.obs_focus == ObsFocus::Scenes;
    if app.obs.scenes.is_empty() {
        frame.render_widget(
            empty_note(&app.obs, app.config.obs.enabled, "No scenes."),
            inner,
        );
        return;
    }

    let scenes = &app.obs.scenes;
    let obs = &app.obs;
    let cursor = app.obs_scene_cursor;
    // `tui-widget-list` owns the scrolling window itself, so the cursor is
    // the only piece of App state a frame needs to hand it — the same window
    // math that used to live in `visible_window` now happens once, inside a
    // widget with its own test suite, instead of once per screen in this one.
    let builder = ListBuilder::new(move |context: &ListBuildContext| {
        let line = scene_line(
            &scenes[context.index],
            context.index == cursor,
            focused,
            obs,
            sk,
        );
        (line, 1)
    });
    let list = ListView::new(builder, app.obs.scenes.len());
    // Built fresh every frame from the cursor already on App — nothing here
    // is state the widget remembers between frames, so there is nothing new
    // to keep in sync with the cursor App already tracks.
    let mut state = ListState::new_with_index(Some(cursor));
    frame.render_stateful_widget(list, inner, &mut state);
}

fn scene_line(
    scene: &Scene,
    selected: bool,
    focused: bool,
    obs: &ObsState,
    sk: theme::Skin,
) -> Line<'static> {
    let live = obs.current_scene.as_deref() == Some(scene.name.as_str());
    let mut spans = vec![Span::styled(
        // The live scene is marked, not merely highlighted: highlighting is
        // already doing the job of showing the cursor, and confusing "where
        // the cursor is" with "what is on air" is a good way to switch scenes
        // by accident.
        if live { "▶ " } else { "  " },
        Style::new().fg(sk.error),
    )];

    if let Some(shortcut) = &scene.shortcut {
        spans.push(Span::styled(
            format!("[{shortcut}] "),
            Style::new().fg(sk.accent),
        ));
    }

    spans.push(Span::styled(
        scene.label().to_string(),
        if live {
            Style::new().fg(sk.foreground).add_modifier(Modifier::BOLD)
        } else {
            Style::new().fg(sk.foreground)
        },
    ));

    // An alias replaces the real name in the list, so the real name is shown
    // beside it — otherwise a config with aliases makes the pane and the OBS
    // window disagree about what everything is called.
    if scene.alias.is_some() {
        spans.push(Span::styled(
            format!("  ({})", scene.name),
            Style::new().fg(sk.muted),
        ));
    }

    let mut line = Line::from(spans);
    if selected && focused {
        line = line.style(Style::new().bg(sk.selection));
    }
    line
}

fn draw_audio(frame: &mut Frame, area: Rect, app: &App) {
    let sk = theme::skin();
    let focused = app.obs_focus == ObsFocus::Audio;
    let block = pane_block("Audio", focused, sk);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    draw_audio_list(frame, inner, app);
}

/// The audio list on its own, with no border of its own.
pub fn draw_audio_list(frame: &mut Frame, inner: Rect, app: &App) {
    let sk = theme::skin();
    let focused = app.obs_focus == ObsFocus::Audio;
    if app.obs.audio.is_empty() {
        frame.render_widget(
            empty_note(&app.obs, app.config.obs.enabled, "No audio inputs."),
            inner,
        );
        return;
    }

    let audio = &app.obs.audio;
    let cursor = app.obs_audio_cursor;
    let width = inner.width;
    let builder = ListBuilder::new(move |context: &ListBuildContext| {
        let line = audio_line(
            &audio[context.index],
            context.index == cursor,
            focused,
            width,
            sk,
        );
        (line, 1)
    });
    let list = ListView::new(builder, app.obs.audio.len());
    let mut state = ListState::new_with_index(Some(cursor));
    frame.render_stateful_widget(list, inner, &mut state);
}

fn audio_line(
    input: &AudioInput,
    selected: bool,
    focused: bool,
    width: u16,
    sk: theme::Skin,
) -> Line<'static> {
    // Muted is the state that matters, so it gets the loud colour and the
    // filled glyph: talking into a muted microphone for ten minutes is the
    // failure this pane exists to prevent.
    let (glyph, colour) = match input.muted {
        Some(true) => ("🔇", sk.error),
        Some(false) => ("🔊", sk.success),
        None => ("··", sk.muted),
    };

    let mut spans = vec![Span::styled(format!("{glyph} "), Style::new().fg(colour))];

    if let Some(shortcut) = &input.shortcut {
        spans.push(Span::styled(
            format!("[{shortcut}] "),
            Style::new().fg(sk.accent),
        ));
    }

    spans.push(Span::styled(
        input.label().to_string(),
        Style::new().fg(sk.foreground),
    ));

    // Both figures when there is room. `volume_db` is fetched, stored and
    // documented as "kept as sent rather than derived" — and then read by
    // nothing, so this pane said `80%` while OBS's own mixer beside it said
    // `-2.0 dB`, and the two never appeared to be about the same thing.
    let level = match (input.volume_percent(), input.volume_db) {
        (Some(percent), Some(db)) if width >= 34 => format!(" {percent}% ({db:.1} dB)"),
        (Some(percent), _) => format!(" {percent}%"),
        // Not "0%": an unknown volume and a silent one are different things,
        // and only one of them is a problem.
        (None, _) => " —".to_string(),
    };
    spans.push(Span::styled(level, Style::new().fg(sk.muted)));

    // A bar, if there is room for one after the text. This stays a hand-rolled
    // fill rather than `tui-equalizer`: that widget renders a live level
    // meter, and `volume_percent` is a static gain the user set, not a
    // sampled peak — OBS never sends one over this connection, so wrapping
    // the figure in an equalizer would claim a moving reading that does not
    // exist. It is drawn in the mute colour so a muted input still reads as
    // muted at a glance even at full volume; otherwise it is graded through
    // the same green-to-red ramp every other usage figure in the app uses,
    // rather than a single fixed accent colour.
    if let Some(percent) = input.volume_percent() {
        let used: usize = spans
            .iter()
            .map(|span| unicode_width::UnicodeWidthStr::width(span.content.as_ref()))
            .sum();
        let room = (width as usize).saturating_sub(used + 2);
        if room >= 6 {
            let bar_width = room.min(20);
            let filled = (percent as usize * bar_width / 100).min(bar_width);
            let fill_colour = if input.muted == Some(true) {
                sk.muted
            } else {
                theme::threshold_color(percent as f64, &sk)
            };
            spans.push(Span::raw("  "));
            spans.push(Span::styled(
                "█".repeat(filled),
                Style::new().fg(fill_colour),
            ));
            spans.push(Span::styled(
                "░".repeat(bar_width - filled),
                Style::new().fg(sk.border),
            ));
        }
    }

    let mut line = Line::from(spans);
    if selected && focused {
        line = line.style(Style::new().bg(sk.selection));
    }
    line
}

/// The bottom strip: how hard OBS is working, and whether it is losing frames.
fn draw_stats(frame: &mut Frame, area: Rect, obs: &ObsState) {
    let sk = theme::skin();
    let block = Block::default()
        .borders(Borders::TOP)
        .border_style(Style::new().fg(sk.border));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let Some(stats) = &obs.stats else {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "No statistics yet.",
                Style::new().fg(sk.muted),
            ))),
            inner,
        );
        return;
    };

    // `cpu_usage_percent` is a true 0-100 share of a ceiling, so it takes the
    // shared four-stop ramp everyone else grades a usage figure with. `mem`
    // and `fps` do not get the same treatment even though the task that asked
    // for this pass named all three: OBS reports no memory ceiling to take a
    // share of, and `fps` runs the ramp backwards — a *higher* frame rate is
    // better, not worse, so colouring it as if 92 were the dangerous end
    // would tell the opposite of the truth. `disk` and `frame` render time
    // are left alone for the same reason: an unbounded quantity, not a share
    // of one.
    let first = vec![
        stat(
            "cpu",
            format!("{:.1}%", stats.cpu_usage_percent),
            theme::threshold_color(stats.cpu_usage_percent, &sk),
            sk,
        ),
        stat(
            "mem",
            format!("{:.0}MB", stats.memory_usage_mb),
            sk.foreground,
            sk,
        ),
        stat(
            "disk",
            format!("{:.1}GB free", stats.available_disk_space_mb / 1024.0),
            sk.foreground,
            sk,
        ),
        stat("fps", format!("{:.0}", stats.active_fps), sk.foreground, sk),
        stat(
            "frame",
            format!("{:.1}ms", stats.average_frame_render_time_ms),
            sk.foreground,
            sk,
        ),
    ];

    // Skipped frames are the numbers that matter to a viewer, so they are
    // shown as a badge rather than left as plain text among the others.
    let mut second = skipped_stat(
        "encoder skipped",
        stats.render_skipped_frames,
        stats.render_skipped_percent(),
        sk,
    );
    second.push(Span::raw("   "));
    second.extend(skipped_stat(
        "dropped on send",
        stats.output_skipped_frames,
        stats.output_skipped_percent(),
        sk,
    ));

    frame.render_widget(
        Paragraph::new(vec![
            Line::from(first.into_iter().flatten().collect::<Vec<_>>()),
            Line::from(second),
        ])
        .wrap(Wrap { trim: false }),
        inner,
    );
}

fn stat(label: &str, value: String, colour: Color, sk: theme::Skin) -> Vec<Span<'static>> {
    vec![
        Span::styled(format!("{label} "), Style::new().fg(sk.muted)),
        Span::styled(value, Style::new().fg(colour)),
        Span::raw("   "),
    ]
}

/// A skipped-frame figure, badged by how bad it is.
///
/// The thresholds stay their own, lower than [`theme::threshold_color`]'s
/// shared four-stop ramp: that ramp's first cutoff sits at 60%, a share of
/// dropped frames no stream would ever reach with a watchable picture left.
/// A stream losing one frame in a hundred is already visibly stuttering to
/// the people watching it, long before anything feels wrong at the desk, so
/// this keeps its own far more sensitive cutoffs rather than adopting ones
/// tuned for a different kind of figure.
fn skipped_stat(label: &str, frames: u64, percent: f64, sk: theme::Skin) -> Vec<Span<'static>> {
    let colour = if percent >= 1.0 {
        sk.error
    } else if percent >= 0.1 {
        sk.warning
    } else {
        sk.muted
    };
    let mut spans = style_kit::badge(label, colour, &sk);
    spans.push(Span::styled(
        format!(" {frames} ({percent:.2}%)"),
        Style::new().fg(colour),
    ));
    spans
}

fn pane_block(title: &str, focused: bool, sk: theme::Skin) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(style_kit::panel_border_style(focused, &sk))
        .title(Line::styled(
            format!(" {title} "),
            style_kit::panel_title_style(focused, &sk),
        ))
        .padding(ratatui::widgets::Padding::horizontal(1))
}

/// What to say in an empty list, which depends on *why* it is empty.
///
/// `enabled` is what the config says, which is not the same question as what
/// the connection is doing: "turned off" and "not connected yet" call for
/// completely different advice, and telling someone to check a setting they
/// have already set would be worse than saying nothing.
fn empty_note(obs: &ObsState, enabled: bool, when_connected: &str) -> Paragraph<'static> {
    let sk = theme::skin();
    let text = match &obs.connection {
        _ if !enabled => {
            "OBS control is turned off.\n\nSet `enabled = true` under `[obs]` in config.toml."
                .to_string()
        }
        Connection::Connected => when_connected.to_string(),
        Connection::Failed(reason) => format!("Not connected: {reason}"),
        // A retry that knows why the last one failed says so, rather than
        // leaving somebody to guess between "OBS is not running" and "the
        // password is wrong".
        other if other.detail().is_some() => format!(
            "Waiting for OBS…\n\n{}",
            other.detail().unwrap_or_default()
        ),
        _ => "Waiting for OBS…\n\nTurn its WebSocket server on under\nTools → WebSocket Server Settings."
            .to_string(),
    };
    Paragraph::new(text)
        .style(Style::new().fg(sk.muted))
        .wrap(Wrap { trim: false })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::obs::state::Stats;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn render(app: &App, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height))
            .expect("the test backend never fails to initialise");
        terminal
            .draw(|frame| draw(frame, frame.area(), app))
            .expect("drawing must not fail");
        let buffer = terminal.backend().buffer().clone();
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn app_with(obs: ObsState) -> App {
        let mut app = App::new(crate::config::Config::default());
        app.splash_skipped = true;
        app.obs = obs;
        app
    }

    fn populated() -> ObsState {
        ObsState {
            connection: Connection::Connected,
            obs_version: Some("30.1.2".into()),
            current_scene: Some("Main Camera".into()),
            scenes: vec![
                Scene {
                    name: "Starting Soon".into(),
                    alias: Some("intro".into()),
                    shortcut: Some("1".into()),
                },
                Scene {
                    name: "Main Camera".into(),
                    alias: None,
                    shortcut: Some("2".into()),
                },
            ],
            audio: vec![
                AudioInput {
                    name: "Mic/Aux".into(),
                    alias: Some("mic".into()),
                    shortcut: Some("m".into()),
                    kind: None,
                    muted: Some(false),
                    volume_mul: Some(0.8),
                    volume_db: Some(-2.0),
                },
                AudioInput {
                    name: "Desktop Audio".into(),
                    alias: None,
                    shortcut: None,
                    kind: None,
                    muted: Some(true),
                    volume_mul: Some(1.0),
                    volume_db: Some(0.0),
                },
            ],
            streaming: true,
            stream_duration: Some(std::time::Duration::from_secs(3725)),
            stats: Some(Stats {
                cpu_usage_percent: 12.5,
                active_fps: 60.0,
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn the_pane_shows_scenes_audio_and_status() {
        let screen = render(&app_with(populated()), 100, 30);
        assert!(screen.contains("intro"), "an aliased scene");
        assert!(screen.contains("Main Camera"), "a scene without an alias");
        assert!(screen.contains("mic"), "an aliased input");
        assert!(screen.contains("Desktop Audio"));
        assert!(screen.contains("STREAM"));
    }

    /// The real OBS name has to stay visible beside an alias, or the pane and
    /// the OBS window disagree about what everything is called.
    #[test]
    fn an_alias_does_not_hide_the_real_obs_name() {
        let screen = render(&app_with(populated()), 100, 30);
        assert!(screen.contains("Starting Soon"));
    }

    /// A live scene is marked, not merely highlighted — the highlight already
    /// means "the cursor is here", and confusing the two invites switching
    /// scenes by accident.
    #[test]
    fn the_live_scene_is_marked() {
        let screen = render(&app_with(populated()), 100, 30);
        // The scene row, not the status line that also names the live scene.
        let live_row = screen
            .lines()
            .find(|line| line.contains("Main Camera") && line.contains('▶'))
            .unwrap_or_else(|| panic!("no marked scene row in:\n{screen}"));
        assert!(!live_row.contains("profile"), "that is the status line");
    }

    #[test]
    fn a_running_stream_shows_how_long_it_has_been_running() {
        let screen = render(&app_with(populated()), 100, 30);
        assert!(
            screen.contains("1:02:05"),
            "an hour, two minutes, five seconds"
        );
    }

    #[test]
    fn durations_read_as_clock_times() {
        assert_eq!(format_duration(std::time::Duration::from_secs(5)), "0:05");
        assert_eq!(format_duration(std::time::Duration::from_secs(65)), "1:05");
        assert_eq!(
            format_duration(std::time::Duration::from_secs(3600)),
            "1:00:00"
        );
        assert_eq!(
            format_duration(std::time::Duration::from_secs(3725)),
            "1:02:05"
        );
    }

    /// Someone who has never set OBS up will see this pane by accident. It
    /// has to say what to do, not just fail to list anything.
    #[test]
    fn an_unconnected_pane_says_how_to_connect() {
        let screen = render(&app_with(ObsState::default()), 100, 30);
        assert!(
            screen.contains("WebSocket Server Settings"),
            "the empty state must name the OBS setting: {screen}"
        );
    }

    /// Turning it off and not having connected yet are different situations
    /// with different advice.
    #[test]
    fn a_disabled_pane_says_it_is_disabled_rather_than_waiting() {
        let mut app = app_with(ObsState::default());
        app.config.obs.enabled = false;
        let screen = render(&app, 100, 30);
        assert!(screen.contains("turned off"), "got {screen}");
    }

    #[test]
    fn a_failed_connection_shows_the_reason() {
        let obs = ObsState {
            connection: Connection::Failed("the password is probably wrong".into()),
            ..Default::default()
        };
        let screen = render(&app_with(obs), 100, 30);
        assert!(screen.contains("password"), "got {screen}");
    }

    /// The pane is drawn from cached state, so every size has to be safe
    /// whatever OBS has or has not said yet.
    #[test]
    fn drawing_never_panics_at_any_size() {
        for (width, height) in [(1, 1), (10, 4), (40, 12), (80, 24), (200, 60)] {
            render(&app_with(populated()), width, height);
            render(&app_with(ObsState::default()), width, height);
        }
    }

    /// An input whose volume OBS has not reported yet must not read as
    /// silent: unknown and zero are different things, and only one is a
    /// problem.
    #[test]
    fn an_unknown_volume_is_not_drawn_as_zero() {
        let obs = ObsState {
            connection: Connection::Connected,
            audio: vec![AudioInput {
                name: "Mic".into(),
                alias: None,
                shortcut: None,
                kind: None,
                muted: None,
                volume_mul: None,
                volume_db: None,
            }],
            ..Default::default()
        };
        let screen = render(&app_with(obs), 60, 20);
        let row = screen
            .lines()
            .find(|line| line.contains("Mic"))
            .expect("the input is listed");
        assert!(!row.contains("0%"), "got {row:?}");
    }
}
