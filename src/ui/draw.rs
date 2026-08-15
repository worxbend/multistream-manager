//! Rendering. Every function here reads [`App`] and draws; none of them mutate.

use ratatui::prelude::*;
use ratatui::widgets::{
    Block, BorderType, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap,
};

use super::app::{App, Screen};
use super::worker::LogLevel;
use crate::lang;
use crate::model::{Field, Platform, Privacy};

/// The colour scheme, kept in one place so the whole UI stays consistent.
mod theme {
    use ratatui::style::Color;

    pub const ACCENT: Color = Color::Rgb(145, 71, 255); // Twitch purple
    pub const YOUTUBE: Color = Color::Rgb(255, 0, 0);
    pub const TEXT: Color = Color::Rgb(230, 237, 243);
    pub const DIM: Color = Color::Rgb(139, 148, 158);
    pub const BORDER: Color = Color::Rgb(48, 54, 61);
    pub const OK: Color = Color::Rgb(63, 185, 80);
    pub const WARN: Color = Color::Rgb(210, 153, 34);
    pub const ERROR: Color = Color::Rgb(248, 81, 73);
}

/// Brand colour for a platform, used on its panel border and heading.
fn platform_color(platform: Platform) -> Color {
    match platform {
        Platform::Twitch => theme::ACCENT,
        Platform::YouTube => theme::YOUTUBE,
    }
}

/// Draw the whole screen.
pub fn draw(frame: &mut Frame, app: &App) {
    let areas = Layout::vertical([
        Constraint::Length(1), // top-level tab bar
        Constraint::Length(3), // header
        Constraint::Min(0),    // body
        Constraint::Length(1), // footer / key hints
    ])
    .split(frame.area());

    draw_tab_bar(frame, areas[0], app);
    draw_header(frame, areas[1], app);

    match app.tab {
        super::app::Tab::Chat => {
            super::chat_tab::draw(frame, areas[2], &app.chat, &app.config);
        }
        super::app::Tab::Combined => draw_combined(frame, areas[2], app),
        super::app::Tab::StreamInfo => match app.screen {
            Screen::Setup => draw_setup(frame, areas[2], app),
            Screen::Login => draw_login(frame, areas[2], app),
            Screen::Platforms => draw_platforms(frame, areas[2], app),
            Screen::Form => draw_form(frame, areas[2], app),
            Screen::Dashboard => draw_dashboard(frame, areas[2], app),
        },
    }

    draw_footer(frame, areas[3], app);
}

/// The top-level tab strip: `1 Stream Info` and `2 Chat`.
fn draw_tab_bar(frame: &mut Frame, area: Rect, app: &App) {
    let tab = |label: &str, active: bool| {
        if active {
            Span::styled(
                format!(" {label} "),
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            Span::styled(format!(" {label} "), Style::default().fg(Color::Gray))
        }
    };
    let line = Line::from(vec![
        tab("1 Stream Info", app.tab == super::app::Tab::StreamInfo),
        Span::raw(" "),
        tab("2 Chat", app.tab == super::app::Tab::Chat),
        Span::raw(" "),
        tab("3 Combined", app.tab == super::app::Tab::Combined),
        Span::styled(
            "   alt+1/2/3 to switch",
            Style::default().fg(Color::DarkGray),
        ),
    ]);
    frame.render_widget(Paragraph::new(line), area);
}

/// The first-run credential form.
///
/// Both platforms need an "application" registered in their developer console
/// before anything else can happen; this asks for the two values that console
/// gives you, so a fresh install never has to be fixed by hand-editing a file.
fn draw_setup(frame: &mut Frame, area: Rect, app: &App) {
    use super::app::SetupField;

    let areas = Layout::vertical([Constraint::Length(9), Constraint::Min(0)])
        .horizontal_margin(2)
        .split(area);

    let mut lines = vec![
        Line::from(Span::styled(
            "Set up API access",
            Style::new().fg(theme::TEXT).add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "Each platform needs an application registered in its developer console.",
            Style::new().fg(theme::DIM),
        )),
        Line::from(Span::styled(
            "Twitch:  https://dev.twitch.tv/console/apps  (OAuth redirect URL below)",
            Style::new().fg(theme::DIM),
        )),
        Line::from(Span::styled(
            "YouTube: https://console.cloud.google.com/apis/credentials  (Desktop app)",
            Style::new().fg(theme::DIM),
        )),
        Line::from(""),
        Line::from(vec![
            Span::styled("Redirect URL to register: ", Style::new().fg(theme::DIM)),
            Span::styled(app.config.redirect_uri(), Style::new().fg(theme::TEXT)),
        ]),
        Line::from(Span::styled(
            "Fill in one platform or both — an empty pair is simply skipped.",
            Style::new().fg(theme::DIM),
        )),
    ];
    lines.push(Line::from(""));
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), areas[0]);

    let rows = Layout::vertical(
        SetupField::ORDER
            .iter()
            .map(|_| Constraint::Length(3))
            .chain(std::iter::once(Constraint::Min(0)))
            .collect::<Vec<_>>(),
    )
    .split(areas[1]);

    for (index, field) in SetupField::ORDER.iter().enumerate() {
        let focused = index == app.setup_cursor;
        let value = app
            .setup_inputs
            .get(field)
            .map(|input| input.value().to_string())
            .unwrap_or_default();
        // A secret is drawn as dots even while it is being typed: this window
        // is frequently shared, and a client secret is a credential.
        let shown = if field.is_secret() {
            "•".repeat(value.chars().count())
        } else {
            value
        };

        let border = if focused {
            platform_color(field.platform())
        } else {
            theme::BORDER
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::new().fg(border))
            .title(format!(" {} ", field.label()))
            .padding(ratatui::widgets::Padding::horizontal(1));

        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                shown,
                Style::new().fg(theme::TEXT),
            )))
            .block(block),
            rows[index],
        );
    }
}

/// The login screen: which platforms to authorise in the browser.
fn draw_login(frame: &mut Frame, area: Rect, app: &App) {
    let areas = Layout::vertical([Constraint::Length(6), Constraint::Min(0)])
        .horizontal_margin(2)
        .split(area);

    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(
                "Authorise your accounts",
                Style::new().fg(theme::TEXT).add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
            Line::from(Span::styled(
                "Tick the platforms you want to stream to and press Enter. Your browser",
                Style::new().fg(theme::DIM),
            )),
            Line::from(Span::styled(
                "opens for each one in turn; approve the access and come back here.",
                Style::new().fg(theme::DIM),
            )),
        ])
        .wrap(Wrap { trim: false }),
        areas[0],
    );

    let items: Vec<ListItem> = Platform::ALL
        .iter()
        .enumerate()
        .map(|(index, platform)| {
            let ticked = app.login_selection.contains(platform);
            let focused = index == app.login_cursor;
            let configured = app.config.check_credentials(&[*platform]).is_ok();
            let authorised = app.logged_in.get(platform).copied().unwrap_or(false);

            let state = if !configured {
                " — no credentials yet (press c)"
            } else if authorised {
                " — already authorised, logging in again replaces it"
            } else {
                ""
            };

            let style = if focused {
                Style::new()
                    .fg(platform_color(*platform))
                    .add_modifier(Modifier::BOLD)
            } else if configured {
                Style::new().fg(theme::TEXT)
            } else {
                Style::new().fg(theme::DIM)
            };

            ListItem::new(Line::from(vec![
                Span::styled(
                    format!(
                        "{} {} ",
                        if ticked { "[x]" } else { "[ ]" },
                        platform.label()
                    ),
                    style,
                ),
                Span::styled(state.to_string(), Style::new().fg(theme::DIM)),
            ]))
        })
        .collect();

    frame.render_widget(List::new(items), areas[1]);
}

/// The combined tab: channel state on top, both chats underneath.
///
/// The strip on top is deliberately small — the point of this tab is to watch
/// chat while keeping an eye on whether you are actually live and how many
/// people are watching, not to duplicate the whole dashboard.
fn draw_combined(frame: &mut Frame, area: Rect, app: &App) {
    use super::app::CombinedFocus;

    let areas = Layout::vertical([Constraint::Length(7), Constraint::Min(0)]).split(area);
    let focused_border = |focused: bool| {
        if focused {
            Style::new().fg(theme::ACCENT)
        } else {
            Style::new().fg(theme::BORDER)
        }
    };

    let stream_focused = app.combined_focus == CombinedFocus::StreamInfo;
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(focused_border(stream_focused))
        .title(" Stream info — alt+w swaps halves ")
        .padding(ratatui::widgets::Padding::horizontal(1));
    let inner = block.inner(areas[0]);
    frame.render_widget(block, areas[0]);
    frame.render_widget(
        Paragraph::new(stream_summary_lines(app)).wrap(Wrap { trim: false }),
        inner,
    );

    let chat_block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(focused_border(!stream_focused))
        .title(" Chat ");
    let chat_inner = chat_block.inner(areas[1]);
    frame.render_widget(chat_block, areas[1]);
    super::chat_tab::draw(frame, chat_inner, &app.chat, &app.config);
}

/// A few lines describing where each platform stands right now: which account
/// is connected, whether it is live, and what the stream is called.
fn stream_summary_lines(app: &App) -> Vec<Line<'static>> {
    let plan = app.plan();
    let mut lines = vec![Line::from(vec![
        Span::styled("Title  ", Style::new().fg(theme::DIM)),
        Span::styled(
            if plan.title.is_empty() {
                "(not set — press alt+1 to edit)".to_string()
            } else {
                plan.title.clone()
            },
            Style::new().fg(theme::TEXT),
        ),
    ])];

    for platform in Platform::ALL {
        if !app.is_selected(platform) {
            continue;
        }
        let mut spans = vec![Span::styled(
            format!("{:<9}", platform.label()),
            Style::new()
                .fg(platform_color(platform))
                .add_modifier(Modifier::BOLD),
        )];

        match app.accounts.get(&platform) {
            Some(Ok(name)) => spans.push(Span::styled(
                format!("{name}  "),
                Style::new().fg(theme::TEXT),
            )),
            Some(Err(_)) => spans.push(Span::styled(
                "not connected  ",
                Style::new().fg(theme::ERROR),
            )),
            None => spans.push(Span::styled("connecting…  ", Style::new().fg(theme::DIM))),
        }

        match app.stats_for(platform) {
            Some(stats) if stats.error.is_none() => {
                spans.push(Span::styled(
                    if stats.live {
                        "● live  "
                    } else {
                        "○ offline  "
                    },
                    Style::new().fg(if stats.live { theme::OK } else { theme::DIM }),
                ));
                if let Some(viewers) = stats.viewers {
                    spans.push(Span::styled(
                        format!("{viewers} watching  "),
                        Style::new().fg(theme::TEXT),
                    ));
                }
                if let Some(started) = stats.started_at {
                    spans.push(Span::styled(
                        format!("up {}", uptime(started)),
                        Style::new().fg(theme::DIM),
                    ));
                }
            }
            Some(_) => spans.push(Span::styled(
                "statistics unavailable",
                Style::new().fg(theme::WARN),
            )),
            None => spans.push(Span::styled(
                "no statistics yet",
                Style::new().fg(theme::DIM),
            )),
        }

        lines.push(Line::from(spans));
    }

    if lines.len() == 1 {
        lines.push(Line::from(Span::styled(
            "No platform is connected — alt+1 opens the Stream Info tab.",
            Style::new().fg(theme::DIM),
        )));
    }
    lines
}

fn draw_header(frame: &mut Frame, area: Rect, app: &App) {
    let selected: Vec<Span> = Platform::ALL
        .iter()
        .filter(|p| app.is_selected(**p))
        .map(|p| {
            Span::styled(
                format!(" {} ", p.label()),
                Style::new().fg(platform_color(*p)),
            )
        })
        .collect();

    let mut spans = vec![
        Span::styled(
            " multistream-manager ",
            Style::new().fg(theme::TEXT).add_modifier(Modifier::BOLD),
        ),
        Span::styled("│ ", Style::new().fg(theme::BORDER)),
    ];
    if selected.is_empty() {
        spans.push(Span::styled(
            "no platforms selected",
            Style::new().fg(theme::DIM),
        ));
    } else {
        spans.extend(selected);
    }

    if app.busy {
        spans.push(Span::styled(
            "  working…",
            Style::new().fg(theme::WARN).add_modifier(Modifier::BOLD),
        ));
    }

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(theme::BORDER));

    frame.render_widget(Paragraph::new(Line::from(spans)).block(block), area);
}

fn draw_footer(frame: &mut Frame, area: Rect, app: &App) {
    // A toast takes over the footer while it is showing, because it is always
    // more urgent than a reminder of the key bindings.
    if let Some(toast) = &app.toast {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                format!(" {toast}"),
                Style::new().fg(theme::WARN).add_modifier(Modifier::BOLD),
            ))),
            area,
        );
        return;
    }

    // The hints follow the *tab*, not only the screen. The Stream Info screens
    // and the Chat tab have entirely different keys, and the footer used to
    // advertise whichever Stream Info screen happened to be underneath — so
    // the Chat tab told you to press Enter to connect, which does nothing
    // there, and never mentioned a single chat binding.
    if app.tab == super::app::Tab::Combined {
        let hints = if app.combined_focus == super::app::CombinedFocus::StreamInfo {
            " alt+w chat   r refresh   y copy Twitch key   Y copy YouTube key   q quit"
        } else {
            " alt+w stream info   h/l pane   j/k scroll   i compose   / search   q quit"
        };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(hints, Style::new().fg(theme::DIM)))),
            area,
        );
        return;
    }

    if app.tab == super::app::Tab::Chat {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                " h/l pane   j/k scroll   [ ] chats   { } accounts   i compose   \
                 space,c join   / search   ctrl+r reconnect   q quit",
                Style::new().fg(theme::DIM),
            ))),
            area,
        );
        return;
    }

    // On the form, colour the "go live" hint by whether the plan is actually
    // submittable, so you can see at a glance that something still needs fixing.
    if app.screen == Screen::Form {
        let ready = app.plan().is_submittable(&app.selected);
        let (go_hint, go_style) = if ready {
            (
                "Ctrl+G go live",
                Style::new().fg(theme::OK).add_modifier(Modifier::BOLD),
            )
        } else {
            ("Ctrl+G go live (not ready)", Style::new().fg(theme::DIM))
        };

        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(
                    " Tab/↑↓ field   Enter complete   ",
                    Style::new().fg(theme::DIM),
                ),
                Span::styled(go_hint, go_style),
                Span::styled(
                    "   Ctrl+S save defaults   Esc back",
                    Style::new().fg(theme::DIM),
                ),
            ])),
            area,
        );
        return;
    }

    let hints = match app.screen {
        Screen::Setup => "Tab/↑↓ field   Enter save & continue   Esc back   Ctrl+C quit",
        Screen::Login => {
            "↑↓ move   Space tick   Enter log in   c edit credentials   s skip   q quit"
        }
        Screen::Platforms => "↑↓ move   Space toggle   a all   Enter connect   q quit",
        Screen::Dashboard => {
            "r refresh   o open watch page   y copy Twitch key   Y copy YouTube key   \
             e edit   q quit"
        }
        Screen::Form => unreachable!("handled above"),
    };

    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            format!(" {hints}"),
            Style::new().fg(theme::DIM),
        ))),
        area,
    );
}

// -- Screen 1 ---------------------------------------------------------------

fn draw_platforms(frame: &mut Frame, area: Rect, app: &App) {
    let areas = Layout::vertical([Constraint::Length(8), Constraint::Min(0)])
        .horizontal_margin(1)
        .split(area);

    let items: Vec<ListItem> = Platform::ALL
        .iter()
        .enumerate()
        .map(|(index, platform)| {
            let ticked = app.is_selected(*platform);
            let marker = if ticked { "[x]" } else { "[ ]" };
            let focused = index == app.platform_cursor;

            let style = if focused {
                Style::new()
                    .fg(platform_color(*platform))
                    .add_modifier(Modifier::BOLD)
            } else if ticked {
                Style::new().fg(theme::TEXT)
            } else {
                Style::new().fg(theme::DIM)
            };

            // Show which account each platform is connected to, once known.
            let suffix = match app.accounts.get(platform) {
                Some(Ok(name)) => format!("  — connected as {name}"),
                Some(Err(_)) => "  — connection failed".to_string(),
                None => String::new(),
            };

            ListItem::new(Line::from(vec![
                Span::styled(format!(" {marker} "), style),
                Span::styled(platform.label(), style),
                Span::styled(suffix, Style::new().fg(theme::DIM)),
            ]))
        })
        .collect();

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(theme::BORDER))
        .title(" Where are you streaming? ");

    frame.render_widget(List::new(items).block(block), areas[0]);

    let explanation = vec![
        Line::from(Span::styled(
            "Pick the platforms you want to broadcast to, then press Enter.",
            Style::new().fg(theme::TEXT),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "You will be asked to log in the first time. After that the login is \
             remembered and renews itself.",
            Style::new().fg(theme::DIM),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "Nothing is sent to either platform until you fill in the next screen \
             and press Ctrl+G.",
            Style::new().fg(theme::DIM),
        )),
    ];

    frame.render_widget(
        Paragraph::new(explanation)
            .wrap(Wrap { trim: true })
            .block(Block::default().padding(ratatui::widgets::Padding::new(1, 1, 1, 0))),
        areas[1],
    );
}

// -- Screen 2 ---------------------------------------------------------------

fn draw_form(frame: &mut Frame, area: Rect, app: &App) {
    let areas = Layout::vertical([
        Constraint::Min(0),    // the fields
        Constraint::Length(4), // help for the focused field
        Constraint::Length(6), // recent log lines
    ])
    .horizontal_margin(1)
    .split(area);

    // Width left for a field's value after the marker and the label column.
    let text_width = areas[0].width.saturating_sub(32).max(10) as usize;

    let mut lines = Vec::new();

    for (index, field) in Field::ORDER.iter().enumerate() {
        let focused = index == app.field_cursor;
        let label_style = if focused {
            Style::new().fg(theme::ACCENT).add_modifier(Modifier::BOLD)
        } else {
            Style::new().fg(theme::DIM)
        };

        // Hide YouTube-only fields when YouTube is not selected, so the form
        // shows only what will actually be used.
        if !app.is_selected(Platform::YouTube)
            && matches!(
                field,
                Field::Description
                    | Field::YouTubeCategory
                    | Field::Privacy
                    | Field::MadeForKids
                    | Field::AutoStart
                    | Field::AutoStop
            )
        {
            continue;
        }
        if !app.is_selected(Platform::Twitch) && matches!(field, Field::TwitchCategory) {
            continue;
        }

        let marker = if focused { "▌" } else { " " };

        let mut spans = vec![
            Span::styled(format!(" {marker} "), Style::new().fg(theme::ACCENT)),
            Span::styled(format!("{:<24}", field.label()), label_style),
        ];

        // The focused text field gets a caret and scrolls horizontally, so a
        // title longer than the window is still editable at any position.
        if focused && field.is_text_input() {
            spans.extend(editing_spans(app, *field, text_width));
        } else {
            spans.push(Span::styled(
                field_value(app, *field),
                Style::new().fg(theme::TEXT),
            ));
        }

        // The live character counter on the title.
        if *field == Field::Title {
            let (counter, over) = app.title_counter();
            spans.push(Span::styled(
                format!("   {counter}"),
                Style::new().fg(if over { theme::ERROR } else { theme::DIM }),
            ));
        }

        // Flag an unresolved category, which is the single most common reason a
        // submit gets rejected. Both platforms need this: each stores a resolved
        // id that is cleared as soon as the user types over the name.
        let unresolved = match field {
            Field::TwitchCategory => {
                app.twitch_category.is_none() && app.is_selected(Platform::Twitch)
            }
            Field::YouTubeCategory => {
                app.youtube_category_id.is_empty() && app.is_selected(Platform::YouTube)
            }
            _ => false,
        };
        if unresolved {
            spans.push(Span::styled(
                "   not selected — press Enter to pick",
                Style::new().fg(theme::WARN),
            ));
        }

        lines.push(Line::from(spans));
    }

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(theme::BORDER))
        .title(" Stream settings ");

    frame.render_widget(Paragraph::new(lines).block(block), areas[0]);

    // The help text for whichever field is focused.
    let help = Paragraph::new(vec![
        Line::from(Span::styled(
            app.field().label(),
            Style::new().fg(theme::ACCENT).add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            app.field().help(),
            Style::new().fg(theme::DIM),
        )),
    ])
    .wrap(Wrap { trim: true })
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::new().fg(theme::BORDER)),
    );
    frame.render_widget(help, areas[1]);

    draw_log(frame, areas[2], app);

    // The autocomplete list floats above everything else.
    if app.popup.is_some() {
        draw_popup(frame, area, app);
    }
}

/// Render one field's current value as display text.
fn field_value(app: &App, field: Field) -> String {
    match field {
        Field::Privacy => format!(
            "{}   {}",
            app.privacy.label(),
            Privacy::ALL
                .iter()
                .map(|p| if *p == app.privacy { "●" } else { "○" })
                .collect::<String>()
        ),
        Field::MadeForKids => checkbox(app.made_for_kids),
        Field::AutoStart => checkbox(app.auto_start),
        Field::AutoStop => checkbox(app.auto_stop),
        Field::Language => {
            let code = app.input(Field::Language).map(|i| i.value()).unwrap_or("");
            if code.is_empty() {
                "(not set)".to_string()
            } else {
                format!("{code}  ({})", lang::name_for(code))
            }
        }
        other => {
            let text = app.input(other).map(|i| i.value()).unwrap_or("");
            if text.is_empty() {
                "(empty)".to_string()
            } else {
                // Keep long descriptions from pushing the layout around.
                crate::model::truncate_chars(text, 70)
            }
        }
    }
}

/// Render the focused text field as `before | caret | after`.
///
/// The caret is drawn as a reversed cell rather than moved with the terminal's
/// own cursor, because the fields are laid out inside a `Paragraph` and there is
/// no reliable cell coordinate to point the real cursor at.
fn editing_spans(app: &App, field: Field, width: usize) -> Vec<Span<'static>> {
    let Some(input) = app.input(field) else {
        return vec![Span::raw("")];
    };

    if input.is_empty() {
        // An empty field still needs a visible caret, otherwise it looks
        // unfocused even though typing would go into it.
        return vec![
            Span::styled(" ", Style::new().bg(theme::ACCENT)),
            Span::styled("  (empty)", Style::new().fg(theme::BORDER)),
        ];
    }

    let (visible, caret) = input.visible(width);
    let chars: Vec<char> = visible.chars().collect();

    let before: String = chars.iter().take(caret).collect();
    let at = chars.get(caret).copied().unwrap_or(' ');
    let after: String = chars.iter().skip(caret + 1).collect();

    vec![
        Span::styled(before, Style::new().fg(theme::TEXT)),
        Span::styled(
            at.to_string(),
            Style::new().bg(theme::ACCENT).fg(Color::White),
        ),
        Span::styled(after, Style::new().fg(theme::TEXT)),
    ]
}

fn checkbox(on: bool) -> String {
    if on {
        "[x] yes".to_string()
    } else {
        "[ ] no".to_string()
    }
}

/// The floating autocomplete list.
fn draw_popup(frame: &mut Frame, area: Rect, app: &App) {
    let Some(popup) = &app.popup else {
        return;
    };

    // Centre it horizontally and put it in the lower half, where it will not
    // cover the field being edited.
    //
    // The height is clamped to the area as well as to the item count: on a short
    // terminal an unclamped popup is drawn past the bottom of the frame, and
    // ratatui panics with "index outside of buffer" rather than clipping. The
    // final `intersection` is a belt-and-braces guarantee that the rectangle can
    // never escape its container whatever the arithmetic above produces.
    let width = area.width.saturating_sub(8).min(70);
    let height = (popup.items.len() as u16 + 2).clamp(3, 12).min(area.height);
    let rect = Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + area.height.saturating_sub(height + 2),
        width,
        height,
    }
    .intersection(area);

    if rect.height < 3 || rect.width < 4 {
        // Too little room to draw anything legible; showing nothing beats
        // showing a broken box.
        return;
    }

    frame.render_widget(Clear, rect);

    let title = if popup.loading {
        " searching… ".to_string()
    } else if popup.items.is_empty() {
        " no matches ".to_string()
    } else {
        format!(" {} matches — ↑↓ then Enter ", popup.items.len())
    };

    let items: Vec<ListItem> = popup
        .items
        .iter()
        .map(|(_, label)| ListItem::new(Line::from(Span::raw(label.clone()))))
        .collect();

    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::new().fg(theme::ACCENT))
                .title(title),
        )
        .highlight_style(
            Style::new()
                .bg(theme::ACCENT)
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▸ ");

    let mut state = ListState::default();
    if !popup.items.is_empty() {
        state.select(Some(popup.cursor));
    }

    frame.render_stateful_widget(list, rect, &mut state);
}

// -- Screen 3 ---------------------------------------------------------------

fn draw_dashboard(frame: &mut Frame, area: Rect, app: &App) {
    let areas = Layout::vertical([Constraint::Min(0), Constraint::Length(8)])
        .horizontal_margin(1)
        .split(area);

    let live: Vec<Platform> = Platform::ALL
        .iter()
        .copied()
        .filter(|p| app.is_selected(*p))
        .collect();

    if live.is_empty() {
        frame.render_widget(
            Paragraph::new("Nothing to show.").style(Style::new().fg(theme::DIM)),
            areas[0],
        );
        return;
    }

    // One clear sentence at the top: can you press "Start Streaming" or not?
    let ready: Vec<&str> = live
        .iter()
        .filter(|p| app.outcome_for(**p).is_some())
        .map(|p| p.label())
        .collect();

    let banner = if ready.is_empty() {
        Line::from(Span::styled(
            " Nothing is ready — see the errors below.",
            Style::new().fg(theme::ERROR).add_modifier(Modifier::BOLD),
        ))
    } else {
        Line::from(vec![
            Span::styled(
                format!(" {} ready. ", ready.join(" and ")),
                Style::new().fg(theme::OK).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "Press Start Streaming in OBS now.",
                Style::new().fg(theme::TEXT),
            ),
        ])
    };

    // Split the upper region again: one line for the banner, the rest for the
    // per-platform panels. Named distinctly so it cannot shadow `areas`, whose
    // second slot is still the log strip at the bottom.
    let upper = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).split(areas[0]);
    frame.render_widget(Paragraph::new(banner), upper[0]);

    // Give each platform an equal share of the width, side by side.
    let columns = Layout::horizontal(
        live.iter()
            .map(|_| Constraint::Ratio(1, live.len() as u32))
            .collect::<Vec<_>>(),
    )
    .split(upper[1]);

    for (index, platform) in live.iter().enumerate() {
        draw_platform_panel(frame, columns[index], app, *platform);
    }

    draw_log(frame, areas[1], app);
}

fn draw_platform_panel(frame: &mut Frame, area: Rect, app: &App, platform: Platform) {
    let colour = platform_color(platform);
    let mut lines = Vec::new();

    match app.results.iter().find(|r| r.platform == platform) {
        None => {
            lines.push(Line::from(Span::styled(
                "Not submitted yet.",
                Style::new().fg(theme::DIM),
            )));
        }
        Some(result) => match &result.outcome {
            Err(err) => {
                lines.push(Line::from(Span::styled(
                    "FAILED",
                    Style::new().fg(theme::ERROR).add_modifier(Modifier::BOLD),
                )));
                lines.push(Line::from(""));
                for chunk in err.lines() {
                    lines.push(Line::from(Span::styled(
                        chunk.to_string(),
                        Style::new().fg(theme::ERROR),
                    )));
                }
            }
            Ok(outcome) => {
                lines.push(Line::from(Span::styled(
                    "READY — you can start streaming in OBS",
                    Style::new().fg(theme::OK).add_modifier(Modifier::BOLD),
                )));
                lines.push(Line::from(""));

                if let Some(url) = &outcome.watch_url {
                    lines.push(field_line("Watch", url, theme::TEXT));
                }
                if let Some(url) = &outcome.manage_url {
                    lines.push(field_line("Manage", url, theme::DIM));
                }
                if let Some(url) = &outcome.ingest_url {
                    lines.push(field_line("Ingest", url, theme::DIM));
                }
                if let Some(key) = &outcome.stream_key {
                    // Masked by default: this window is frequently on screen
                    // while streaming, and a leaked key lets anyone broadcast
                    // to your channel.
                    // Always masked. There is no reveal: the key leaves this
                    // program only by being copied to the clipboard (y / Y),
                    // so it cannot be read off a shared screen or a recording.
                    let masked = "•".repeat(key.chars().count().min(24));
                    lines.push(field_line("Key", &masked, theme::WARN));
                }

                lines.push(Line::from(""));
                for note in &outcome.notes {
                    let style = if note.starts_with("Warning") {
                        Style::new().fg(theme::WARN)
                    } else {
                        Style::new().fg(theme::DIM)
                    };
                    lines.push(Line::from(Span::styled(format!("• {note}"), style)));
                }
            }
        },
    }

    // Live statistics for this platform.
    if let Some(stats) = app.stats_for(platform) {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "── Live ──",
            Style::new().fg(colour).add_modifier(Modifier::BOLD),
        )));

        if let Some(error) = &stats.error {
            lines.push(Line::from(Span::styled(
                format!("stats unavailable: {error}"),
                Style::new().fg(theme::WARN),
            )));
        } else {
            let status = if stats.live { "live" } else { "offline" };
            let status_colour = if stats.live { theme::OK } else { theme::DIM };
            lines.push(field_line("Status", status, status_colour));

            if let Some(viewers) = stats.viewers {
                lines.push(field_line("Viewers", &viewers.to_string(), theme::TEXT));
            }
            if let Some(started) = stats.started_at {
                lines.push(field_line("Uptime", &uptime(started), theme::TEXT));
            }
            for stat in &stats.extra {
                lines.push(field_line(&stat.label, &stat.value, theme::DIM));
            }
        }
    }

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(colour))
        .title(format!(" {} ", platform.label()))
        .padding(ratatui::widgets::Padding::horizontal(1));

    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false }),
        area,
    );
}

/// A `label: value` line with aligned labels.
fn field_line(label: &str, value: &str, colour: Color) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{label:<9}"), Style::new().fg(theme::DIM)),
        Span::styled(value.to_string(), Style::new().fg(colour)),
    ])
}

/// Render "how long has this been running" as `1h 23m` or `45s`.
pub fn uptime(since: chrono::DateTime<chrono::Utc>) -> String {
    let elapsed = chrono::Utc::now() - since;
    let seconds = elapsed.num_seconds().max(0);

    let hours = seconds / 3600;
    let minutes = (seconds % 3600) / 60;
    let secs = seconds % 60;

    if hours > 0 {
        format!("{hours}h {minutes:02}m")
    } else if minutes > 0 {
        format!("{minutes}m {secs:02}s")
    } else {
        format!("{secs}s")
    }
}

/// The activity log panel, shared by the form and the dashboard.
fn draw_log(frame: &mut Frame, area: Rect, app: &App) {
    let visible = area.height.saturating_sub(2) as usize;

    // Show the newest lines, which is what matters while something is happening,
    // unless the user has scrolled back with Up to read something older.
    let last = app.log.len().saturating_sub(app.log_scroll_back);
    let start = last.saturating_sub(visible);

    let lines: Vec<Line> = app
        .log
        .iter()
        .skip(start)
        .take(visible)
        .map(|entry| {
            let colour = match entry.level {
                LogLevel::Info => theme::DIM,
                LogLevel::Success => theme::OK,
                LogLevel::Warning => theme::WARN,
                LogLevel::Error => theme::ERROR,
            };
            Line::from(vec![
                Span::styled(
                    entry.at.format("%H:%M:%S ").to_string(),
                    Style::new().fg(theme::BORDER),
                ),
                Span::styled(entry.message.clone(), Style::new().fg(colour)),
            ])
        })
        .collect();

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(theme::BORDER))
        .title(" Activity ");

    frame.render_widget(Paragraph::new(lines).block(block), area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::PlatformResult;
    use crate::config::Config;
    use crate::model::GoLiveOutcome;
    use chrono::Duration;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    /// Render one frame into an in-memory terminal and return it as plain text.
    ///
    /// This exercises the real layout code, which is where an off-by-one in a
    /// `Rect` or a shadowed variable would otherwise go unnoticed until a human
    /// happened to look at the screen.
    fn render(app: &App, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height))
            .expect("the test backend never fails to initialise");
        terminal
            .draw(|frame| draw(frame, app))
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

    fn app() -> App {
        // Credentials present, so the Stream Info tab shows its normal
        // screens rather than the unconfigured empty state.
        let mut config = Config::default();
        config.twitch.client_id = "id".into();
        config.twitch.client_secret = "secret".into();
        config.youtube.client_id = "id".into();
        config.youtube.client_secret = "secret".into();
        // A scratch config directory keeps `App::new` from reading whatever
        // logins happen to exist on the machine running the tests, which would
        // otherwise decide which screen opens.
        // `App::new` picks its opening screen from the saved logins, which
        // differ per machine. Tests that care about the opening screen build
        // their own App under a scratch config directory; this helper is for
        // everything else, so it just lands on the platform picker.
        let mut app = App::new(config);
        app.screen = Screen::Platforms;
        app
    }

    #[test]
    fn the_platform_screen_lists_both_platforms() {
        let screen = render(&app(), 100, 30);
        assert!(screen.contains("Twitch"));
        assert!(screen.contains("YouTube"));
        assert!(screen.contains("Where are you streaming?"));
    }

    /// With no credentials configured at all, the platform picker would only
    /// offer choices that cannot work — the empty state with the real setup
    /// commands replaces it.
    #[test]
    fn an_unconfigured_app_opens_the_credential_form() {
        // A scratch config directory keeps the test off the developer's own
        // saved logins, which would otherwise decide the opening screen.
        let _scratch = crate::paths::test_support::ScratchConfigDir::new("draw-setup");
        let app = App::new(Config::default());
        let screen = render(&app, 100, 30);

        assert!(screen.contains("Set up API access"));
        assert!(screen.contains("Twitch client id"));
        assert!(screen.contains("YouTube client secret"));
        assert!(
            !screen.contains("Where are you streaming?"),
            "the platform picker is useless without credentials"
        );
    }

    /// A typed client secret is dots on screen from the first keystroke.
    #[test]
    fn the_setup_form_never_draws_a_secret() {
        let _scratch = crate::paths::test_support::ScratchConfigDir::new("draw-setup-secret");
        let mut app = App::new(Config::default());
        app.setup_cursor = 1; // the Twitch client secret
        for c in "hunter2secret".chars() {
            app.handle_key(crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Char(c),
                crossterm::event::KeyModifiers::NONE,
            ));
        }

        let screen = render(&app, 100, 30);
        assert!(!screen.contains("hunter2secret"));
        assert!(screen.contains('\u{2022}'));
    }

    /// With credentials but nothing authorised, the login screen comes first.
    #[test]
    fn a_configured_app_with_no_login_opens_the_login_screen() {
        let _scratch = crate::paths::test_support::ScratchConfigDir::new("draw-login");
        let mut config = Config::default();
        config.twitch.client_id = "id".into();
        config.twitch.client_secret = "secret".into();
        let app = App::new(config);
        let screen = render(&app, 100, 30);

        assert!(screen.contains("Authorise your accounts"));
        assert!(screen.contains("Twitch"));
        assert!(screen.contains("YouTube"));
    }

    /// The combined tab really shows both halves at once.
    #[test]
    fn the_combined_tab_shows_stream_state_above_the_chats() {
        let _scratch = crate::paths::test_support::ScratchConfigDir::new("draw-combined");
        let mut app = app();
        app.tab = super::super::app::Tab::Combined;
        app.selected = vec![Platform::Twitch];
        app.accounts
            .insert(Platform::Twitch, Ok("examplestreamer".into()));

        let screen = render(&app, 120, 40);

        assert!(screen.contains("Stream info"));
        assert!(screen.contains("examplestreamer"));
        assert!(
            screen.contains("No Twitch chat accounts yet"),
            "the chat panes are drawn underneath:\n{screen}"
        );
    }

    /// The Chat tab always shows both panes; without accounts each pane
    /// carries its own actionable hint.
    #[test]
    fn the_chat_tab_shows_both_panes_with_empty_state_hints() {
        // Point the token store at an empty scratch directory so the test
        // cannot pick up real logins from the developer's machine.
        let _scratch = crate::paths::test_support::ScratchConfigDir::new("draw-chat-empty");
        let mut app = App::new(Config::default());
        app.tab = super::super::app::Tab::Chat;
        let screen = render(&app, 120, 30);
        assert!(screen.contains("Twitch"));
        assert!(screen.contains("YouTube"));
        assert!(screen.contains("No Twitch chat accounts yet"));
        assert!(screen.contains("No YouTube chat accounts yet"));
        assert!(screen.contains("--add"));
    }

    /// The tab bar names both tabs and how to switch.
    #[test]
    fn the_tab_bar_is_always_visible() {
        let screen = render(&app(), 100, 30);
        assert!(screen.contains("1 Stream Info"));
        assert!(screen.contains("2 Chat"));
        assert!(screen.contains("3 Combined"));
        assert!(screen.contains("alt+1/2/3"));
    }

    #[test]
    fn the_form_screen_shows_its_fields_and_the_title_counter() {
        let mut app = app();
        app.screen = Screen::Form;
        app.selected = Platform::ALL.to_vec();

        let screen = render(&app, 120, 40);
        assert!(screen.contains("Stream settings"));
        assert!(screen.contains("Title"));
        assert!(screen.contains("Twitch category"));
        // The counter uses YouTube's tighter limit when YouTube is selected.
        assert!(
            screen.contains("0 / 100"),
            "missing title counter:\n{screen}"
        );
    }

    #[test]
    fn youtube_only_fields_disappear_when_youtube_is_not_selected() {
        let mut app = app();
        app.screen = Screen::Form;
        app.selected = vec![Platform::Twitch];

        let screen = render(&app, 120, 40);
        assert!(screen.contains("Twitch category"));
        assert!(
            !screen.contains("Made for kids"),
            "a YouTube-only field was shown with YouTube unselected"
        );
    }

    #[test]
    fn the_dashboard_shows_the_log_panel_below_the_platform_panels() {
        // This is the regression test for a shadowed layout variable that made
        // the log panel render on top of the platform columns.
        let mut app = app();
        app.screen = Screen::Dashboard;
        app.selected = Platform::ALL.to_vec();
        app.results = vec![PlatformResult {
            platform: Platform::Twitch,
            outcome: Ok(GoLiveOutcome {
                watch_url: Some("https://twitch.tv/example".into()),
                ..Default::default()
            }),
        }];

        let screen = render(&app, 120, 40);
        let lines: Vec<&str> = screen.lines().collect();

        let panel_row = lines
            .iter()
            .position(|l| l.contains("Twitch"))
            .expect("the Twitch panel should be drawn");
        let log_row = lines
            .iter()
            .position(|l| l.contains("Activity"))
            .expect("the Activity log should be drawn");

        assert!(
            log_row > panel_row,
            "the log panel must sit below the platform panels, not over them"
        );
        assert!(screen.contains("https://twitch.tv/example"));
    }

    #[test]
    fn the_dashboard_never_renders_the_stream_key() {
        let mut app = app();
        app.screen = Screen::Dashboard;
        app.selected = vec![Platform::Twitch];
        app.results = vec![PlatformResult {
            platform: Platform::Twitch,
            outcome: Ok(GoLiveOutcome {
                stream_key: Some("live_123_secretkey".into()),
                ..Default::default()
            }),
        }];

        let screen = render(&app, 120, 40);
        assert!(
            !screen.contains("live_123_secretkey"),
            "a stream key must never reach the screen — it is copy-only"
        );
        assert!(screen.contains('\u{2022}'), "it renders as a row of dots");
        assert!(
            screen.contains("y copy"),
            "the footer offers copying instead of revealing"
        );
    }

    #[test]
    fn a_failed_platform_shows_its_error_on_the_dashboard() {
        let mut app = app();
        app.screen = Screen::Dashboard;
        app.selected = vec![Platform::YouTube];
        app.results = vec![PlatformResult {
            platform: Platform::YouTube,
            outcome: Err("out of quota".into()),
        }];

        let screen = render(&app, 120, 40);
        assert!(screen.contains("FAILED"));
        assert!(screen.contains("out of quota"));
        assert!(screen.contains("Nothing is ready"));
    }

    #[test]
    fn rendering_survives_a_very_small_terminal() {
        // Layout maths that assumes a comfortable size panics on a tiny one.
        for screen in [Screen::Platforms, Screen::Form, Screen::Dashboard] {
            let mut app = app();
            app.screen = screen;
            app.selected = Platform::ALL.to_vec();
            let _ = render(&app, 20, 6);
        }
    }

    #[test]
    fn a_popup_on_a_short_terminal_does_not_panic() {
        // Regression: the popup height was clamped to the item count but not to
        // the frame, so on a short terminal it was drawn past the bottom and
        // ratatui panicked with "index outside of buffer", killing the app.
        // 80x14 with a full language list is the reported reproduction.
        let mut app = app();
        app.screen = Screen::Form;
        app.selected = Platform::ALL.to_vec();
        app.popup = Some(super::super::app::Popup {
            field: Field::Language,
            items: (0..30)
                .map(|i| (format!("c{i}"), format!("Language number {i}")))
                .collect(),
            cursor: 0,
            loading: false,
            fallback: false,
        });

        for height in 3..=20 {
            let _ = render(&app, 80, height);
        }
    }

    #[test]
    fn the_autocomplete_popup_is_drawn_over_the_form() {
        let mut app = app();
        app.screen = Screen::Form;
        app.selected = Platform::ALL.to_vec();
        app.popup = Some(super::super::app::Popup {
            field: Field::Language,
            items: vec![("pl".into(), "Polish (pl) — polski".into())],
            cursor: 0,
            loading: false,
            fallback: false,
        });

        let screen = render(&app, 120, 40);
        assert!(screen.contains("Polish"));
        assert!(screen.contains("1 matches"));
    }

    #[test]
    fn the_dashboard_footer_advertises_every_key_the_dashboard_handles() {
        // A key binding nobody is told about might as well not exist, and the
        // footer is the only place the dashboard's keys are written down.
        let mut app = app();
        app.screen = Screen::Dashboard;
        app.selected = vec![Platform::Twitch];

        let screen = render(&app, 120, 20);
        for hint in [
            "r refresh",
            "o open watch page",
            "y copy Twitch key",
            "Y copy YouTube key",
            "e edit",
        ] {
            assert!(
                screen.contains(hint),
                "missing footer hint {hint:?}:\n{screen}"
            );
        }
    }

    #[test]
    fn uptime_under_a_minute_is_shown_in_seconds() {
        let since = chrono::Utc::now() - Duration::seconds(45);
        assert_eq!(uptime(since), "45s");
    }

    #[test]
    fn uptime_under_an_hour_is_shown_in_minutes_and_seconds() {
        let since = chrono::Utc::now() - Duration::seconds(125);
        assert_eq!(uptime(since), "2m 05s");
    }

    #[test]
    fn uptime_over_an_hour_is_shown_in_hours_and_minutes() {
        let since = chrono::Utc::now() - Duration::minutes(83);
        assert_eq!(uptime(since), "1h 23m");
    }

    #[test]
    fn a_start_time_in_the_future_does_not_produce_a_negative_uptime() {
        // Clock skew between this machine and the platform can put the reported
        // start time slightly ahead of now.
        let since = chrono::Utc::now() + Duration::seconds(30);
        assert_eq!(uptime(since), "0s");
    }

    #[test]
    fn checkboxes_read_as_words_not_just_symbols() {
        assert_eq!(checkbox(true), "[x] yes");
        assert_eq!(checkbox(false), "[ ] no");
    }

    #[test]
    fn each_platform_gets_its_own_brand_colour() {
        assert_ne!(
            platform_color(Platform::Twitch),
            platform_color(Platform::YouTube)
        );
    }

    /// Up and Down used to move a `log_scroll` field that the drawing code never
    /// read, so the activity log always showed the tail and an error that had
    /// scrolled past was unreachable.
    #[test]
    fn scrolling_back_shows_older_log_lines() {
        let mut app = app();
        app.screen = Screen::Form;
        app.selected = Platform::ALL.to_vec();
        for i in 0..50 {
            app.push_log(LogLevel::Info, format!("line {i}"));
        }

        let tail = render(&app, 120, 40);
        assert!(
            tail.contains("line 49"),
            "the newest line should be visible"
        );
        assert!(
            !tail.contains("line 0"),
            "the oldest line should be off-screen"
        );

        // Scroll all the way back, as holding Up would.
        app.log_scroll_back = 49;
        let top = render(&app, 120, 40);
        assert!(
            top.contains("line 0"),
            "scrolling back must reach the oldest line:\n{top}"
        );
        assert!(
            !top.contains("line 49"),
            "the newest line should now be off-screen"
        );

        // Coming back down returns to following the tail.
        app.log_scroll_back = 0;
        assert_eq!(render(&app, 120, 40), tail);
    }

    /// While scrolled back, new activity must not yank the view to the bottom.
    #[test]
    fn a_new_line_does_not_move_a_reader_who_scrolled_back() {
        let mut app = app();
        app.screen = Screen::Form;
        app.selected = Platform::ALL.to_vec();
        for i in 0..50 {
            app.push_log(LogLevel::Info, format!("line {i}"));
        }
        app.log_scroll_back = 40;
        let before = render(&app, 120, 40);

        app.push_log(LogLevel::Info, "something new");
        assert_eq!(
            app.log_scroll_back, 41,
            "the view should stay on the same lines"
        );
        assert_eq!(render(&app, 120, 40), before);
    }
}
