//! The Chat tab: a Twitch pane and a YouTube pane side by side.
//!
//! Layout contract (from the integration spec): both panes are always
//! present. A pane whose platform has no logged-in accounts shows an
//! empty-state with the exact commands that add one; a pane with accounts
//! shows one sub-tab per account.
//!
//! This module owns only *state and drawing* for the tab chrome — pane focus,
//! the resizable split, account sub-tabs, empty states. Live chat content
//! plugs into the pane bodies as the adapters land (PLAN.md §10).

use std::collections::BTreeMap;

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::Frame;

use crate::auth::store::TokenStore;
use crate::config::Config;
use crate::model::Platform;

/// One account sub-tab: the token-store key it speaks as, and the label shown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountTab {
    pub key: String,
    pub label: String,
}

/// Everything the Chat tab remembers between frames.
#[derive(Debug, Clone)]
pub struct ChatTabState {
    /// Which pane keyboard input goes to.
    pub focus: Platform,
    /// The accounts available per platform, discovered from the token store
    /// once at startup (primary first, extras after).
    pub accounts: BTreeMap<Platform, Vec<AccountTab>>,
    /// The selected account sub-tab per platform.
    pub selected: BTreeMap<Platform, usize>,
    /// The Twitch pane's share of the width, in percent. Resizable with
    /// `<`/`>` and reset with `=` — the same keys the reference TUIs use.
    pub split_percent: u16,
}

/// The default even split.
const SPLIT_DEFAULT: u16 = 50;
/// How far the divider may be pushed either way. Beyond this a pane is too
/// narrow to render a chat line meaningfully.
const SPLIT_MIN: u16 = 20;
const SPLIT_MAX: u16 = 80;
/// How much one keypress moves the divider.
const SPLIT_STEP: u16 = 2;

impl ChatTabState {
    /// Build the tab state, reading the account list from the token store.
    ///
    /// A store that cannot be read is treated as "no accounts": the panes
    /// then show their empty-state hints, which include the commands that
    /// would also surface the underlying problem.
    pub fn new() -> Self {
        let accounts = match TokenStore::load() {
            Ok(store) => discover_accounts(&store),
            Err(err) => {
                tracing::warn!(error = %format!("{err:#}"), "could not read chat accounts");
                BTreeMap::new()
            }
        };
        Self {
            focus: Platform::Twitch,
            accounts,
            selected: BTreeMap::new(),
            split_percent: SPLIT_DEFAULT,
        }
    }

    /// The accounts for one platform (empty slice when none).
    pub fn accounts_for(&self, platform: Platform) -> &[AccountTab] {
        self.accounts
            .get(&platform)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// The selected account in one pane, if the pane has any.
    // The live-chat wiring (next commits) reads this; the allow leaves with it.
    #[allow(dead_code)]
    pub fn selected_account(&self, platform: Platform) -> Option<&AccountTab> {
        let accounts = self.accounts_for(platform);
        accounts.get(*self.selected.get(&platform).unwrap_or(&0))
    }

    /// Move focus to the other pane.
    pub fn focus_other(&mut self) {
        self.focus = match self.focus {
            Platform::Twitch => Platform::YouTube,
            Platform::YouTube => Platform::Twitch,
        };
    }

    /// Cycle the focused pane's account sub-tab forward or backward.
    pub fn cycle_account(&mut self, forward: bool) {
        let count = self.accounts_for(self.focus).len();
        if count < 2 {
            return;
        }
        let current = *self.selected.get(&self.focus).unwrap_or(&0);
        let next = if forward {
            (current + 1) % count
        } else {
            (current + count - 1) % count
        };
        self.selected.insert(self.focus, next);
    }

    /// Grow or shrink the focused pane by one step.
    pub fn resize(&mut self, grow_focused: bool) {
        // The split percentage names the Twitch (left) pane, so growing the
        // YouTube pane means shrinking the number.
        let grow_left = match self.focus {
            Platform::Twitch => grow_focused,
            Platform::YouTube => !grow_focused,
        };
        let next = if grow_left {
            self.split_percent.saturating_add(SPLIT_STEP)
        } else {
            self.split_percent.saturating_sub(SPLIT_STEP)
        };
        self.split_percent = next.clamp(SPLIT_MIN, SPLIT_MAX);
    }

    pub fn reset_split(&mut self) {
        self.split_percent = SPLIT_DEFAULT;
    }
}

/// Turn the token store into per-platform account tab lists.
fn discover_accounts(store: &TokenStore) -> BTreeMap<Platform, Vec<AccountTab>> {
    let mut out = BTreeMap::new();
    for platform in Platform::ALL {
        let tabs: Vec<AccountTab> = store
            .accounts(platform)
            .into_iter()
            .map(|(key, tokens)| AccountTab {
                key: key.to_string(),
                label: tokens
                    .identity
                    .as_ref()
                    .map(|identity| {
                        if identity.display_name.is_empty() {
                            identity.login.clone()
                        } else {
                            identity.display_name.clone()
                        }
                    })
                    // Tokens saved before identities existed have no name to
                    // show; the store key is at least unambiguous.
                    .unwrap_or_else(|| key.to_string()),
            })
            .collect();
        if !tabs.is_empty() {
            out.insert(platform, tabs);
        }
    }
    out
}

/// Draw the whole Chat tab into `area`.
pub fn draw(frame: &mut Frame, area: Rect, state: &ChatTabState, config: &Config) {
    let panes = Layout::horizontal([
        Constraint::Percentage(state.split_percent),
        Constraint::Percentage(100 - state.split_percent),
    ])
    .split(area);

    draw_pane(frame, panes[0], state, config, Platform::Twitch);
    draw_pane(frame, panes[1], state, config, Platform::YouTube);
}

fn draw_pane(
    frame: &mut Frame,
    area: Rect,
    state: &ChatTabState,
    config: &Config,
    platform: Platform,
) {
    let focused = state.focus == platform;
    let border_style = if focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style)
        .title(format!(" {} ", platform.label()));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let accounts = state.accounts_for(platform);
    if accounts.is_empty() {
        draw_empty_state(frame, inner, config, platform);
        return;
    }

    let rows = Layout::vertical([
        Constraint::Length(1), // account sub-tabs
        Constraint::Min(0),    // chat body
    ])
    .split(inner);

    // Account sub-tab strip.
    let selected = *state.selected.get(&platform).unwrap_or(&0);
    let mut spans = Vec::new();
    for (index, account) in accounts.iter().enumerate() {
        let style = if index == selected {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Gray)
        };
        spans.push(Span::styled(format!(" {} ", account.label), style));
        spans.push(Span::raw(" "));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), rows[0]);

    // The live chat body plugs in here as the adapters land.
    let placeholder = Paragraph::new(vec![
        Line::from(""),
        Line::from(Span::styled(
            "Chat is being wired up — this pane will show live messages shortly.",
            Style::default().fg(Color::DarkGray),
        )),
    ])
    .wrap(Wrap { trim: true });
    frame.render_widget(placeholder, rows[1]);
}

/// The per-pane empty state: what is missing and the exact commands that fix
/// it. The commands are the real ones this repository ships — discovered in
/// recon, not invented.
fn draw_empty_state(frame: &mut Frame, area: Rect, config: &Config, platform: Platform) {
    let slug = platform.slug();
    let credentials_ready = config.check_credentials(&[platform]).is_ok();

    let mut lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            format!("No {} chat accounts yet.", platform.label()),
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
    ];
    if !credentials_ready {
        lines.push(Line::from(
            "1. Create API credentials first: run `msm init`, then fill in the",
        ));
        lines.push(Line::from(format!(
            "   [{slug}] section of config.toml (`msm paths` shows where it lives)."
        )));
        lines.push(Line::from(format!(
            "2. Log in: quit and run `msm login {slug}`."
        )));
    } else {
        lines.push(Line::from(format!(
            "Log in: quit and run `msm login {slug}`."
        )));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(format!(
        "Add more accounts any time with `msm login {slug} --add`;"
    )));
    lines.push(Line::from("each appears here as its own sub-tab."));

    let paragraph = Paragraph::new(lines)
        .style(Style::default().fg(Color::Gray))
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, area);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state_with(twitch: usize, youtube: usize) -> ChatTabState {
        let mut accounts = BTreeMap::new();
        for (platform, count) in [(Platform::Twitch, twitch), (Platform::YouTube, youtube)] {
            if count > 0 {
                accounts.insert(
                    platform,
                    (0..count)
                        .map(|i| AccountTab {
                            key: format!("{}:{i}", platform.slug()),
                            label: format!("acct{i}"),
                        })
                        .collect(),
                );
            }
        }
        ChatTabState {
            focus: Platform::Twitch,
            accounts,
            selected: BTreeMap::new(),
            split_percent: SPLIT_DEFAULT,
        }
    }

    #[test]
    fn focus_toggles_between_the_two_panes() {
        let mut state = state_with(1, 1);
        assert_eq!(state.focus, Platform::Twitch);
        state.focus_other();
        assert_eq!(state.focus, Platform::YouTube);
        state.focus_other();
        assert_eq!(state.focus, Platform::Twitch);
    }

    #[test]
    fn cycling_accounts_wraps_and_ignores_single_account_panes() {
        let mut state = state_with(3, 1);
        state.cycle_account(true);
        state.cycle_account(true);
        assert_eq!(state.selected[&Platform::Twitch], 2);
        state.cycle_account(true);
        assert_eq!(state.selected[&Platform::Twitch], 0, "wraps forward");
        state.cycle_account(false);
        assert_eq!(state.selected[&Platform::Twitch], 2, "wraps backward");

        // A single-account pane has nothing to cycle.
        state.focus = Platform::YouTube;
        state.cycle_account(true);
        assert_eq!(*state.selected.get(&Platform::YouTube).unwrap_or(&0), 0);
    }

    #[test]
    fn the_split_resizes_toward_the_focused_pane_and_clamps() {
        let mut state = state_with(1, 1);
        // Growing the focused (Twitch, left) pane raises the percentage.
        state.resize(true);
        assert_eq!(state.split_percent, SPLIT_DEFAULT + SPLIT_STEP);
        // From the YouTube pane, "grow" means shrinking the left share.
        state.focus = Platform::YouTube;
        for _ in 0..100 {
            state.resize(true);
        }
        assert_eq!(state.split_percent, SPLIT_MIN, "clamped at the minimum");
        state.focus = Platform::Twitch;
        for _ in 0..100 {
            state.resize(true);
        }
        assert_eq!(state.split_percent, SPLIT_MAX, "clamped at the maximum");
        state.reset_split();
        assert_eq!(state.split_percent, SPLIT_DEFAULT);
    }

    #[test]
    fn selected_account_is_none_only_when_the_pane_is_empty() {
        let state = state_with(2, 0);
        assert!(state.selected_account(Platform::Twitch).is_some());
        assert!(state.selected_account(Platform::YouTube).is_none());
    }
}
