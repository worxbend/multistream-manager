//! Mouse support: what is under the pointer, and what a click there means.
//!
//! Keys stay the primary way to drive this program and always will. The mouse
//! is for the handful of things a pointer is genuinely better at — "that pane,
//! the one I am looking at", "that tab", "scroll this back a bit" — and for
//! the first ten minutes of using the program, before any of the keys are in
//! your fingers yet. Nothing here is the *only* way to do anything.
//!
//! Everything in this module is pure. Working out what is under the pointer
//! means recomputing the layout the drawing code produced, which is exactly
//! the sort of thing that silently drifts once the drawing changes — so it is
//! written as functions of a rectangle and a position, with tests that state
//! where things are.
//!
//! Mouse reporting can be turned off (`mouse = false` in `[appearance]`),
//! which hands the terminal back its own text selection. That is a real
//! trade: with reporting on, dragging to select text is the program's to
//! interpret rather than the terminal's, and some people would much rather
//! have the selection.

use crossterm::event::{MouseEvent, MouseEventKind};
use ratatui::layout::Rect;

use crate::model::Platform;

/// What the pointer is over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// One of the top-level tabs, by its index in the tab bar.
    Tab(usize),
    /// A chat pane, on the Chat or Combined tab.
    ChatPane(Platform),
    /// The stream-info half of the Combined tab.
    StreamInfo,
    /// The body of the interface, but nothing in particular.
    Body,
}

/// What a mouse event should do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Switch to a top-level tab.
    SelectTab(usize),
    /// Give the keyboard to a chat pane.
    FocusChat(Platform),
    /// Give the keyboard to the stream-info half of the Combined tab.
    FocusStreamInfo,
    /// Scroll back through history (away from the newest).
    ScrollBack,
    /// Scroll forward toward the newest.
    ScrollForward,
}

/// The tab labels, in the order the tab bar draws them.
///
/// Written here as well as in the drawing code because the hit boxes have to
/// be built from the same strings the labels are drawn from — a click landing
/// one column out is the kind of bug nobody reports, they just decide the
/// mouse does not work.
pub const TAB_LABELS: [&str; 3] = ["1 Stream Info", "2 Chat", "3 Combined"];

/// Which tab label sits at column `x` of the tab bar, if any.
///
/// The bar is drawn as `" label " " " " label "…` — each label padded with one
/// space either side, separated by one more.
pub fn tab_at(x: u16) -> Option<usize> {
    let mut cursor = 0u16;
    for (index, label) in TAB_LABELS.iter().enumerate() {
        // One space before the label, the label, one space after.
        let width = label.chars().count() as u16 + 2;
        if x >= cursor && x < cursor + width {
            return Some(index);
        }
        // The separating space between one tab and the next.
        cursor += width + 1;
    }
    None
}

/// Split the body between the two chat panes exactly as the chat tab does.
fn chat_panes(body: Rect, split_percent: u16) -> (Rect, Rect) {
    let left_width = body.width * split_percent / 100;
    let left = Rect {
        width: left_width,
        ..body
    };
    let right = Rect {
        x: body.x + left_width,
        width: body.width.saturating_sub(left_width),
        ..body
    };
    (left, right)
}

/// Whether a point is inside a rectangle.
fn contains(rect: Rect, x: u16, y: u16) -> bool {
    x >= rect.x && x < rect.x + rect.width && y >= rect.y && y < rect.y + rect.height
}

/// The layout the drawing code produces, in the terms hit-testing needs.
///
/// Built from the same constants `draw` uses: a one-row tab bar, a
/// three-row header, the body, and a one-row footer.
#[derive(Debug, Clone, Copy)]
pub struct Layout {
    pub tab_bar: Rect,
    pub body: Rect,
}

impl Layout {
    pub fn of(area: Rect) -> Self {
        const TAB_BAR_HEIGHT: u16 = 1;
        const HEADER_HEIGHT: u16 = 3;
        const FOOTER_HEIGHT: u16 = 1;
        let tab_bar = Rect {
            height: TAB_BAR_HEIGHT.min(area.height),
            ..area
        };
        let used = TAB_BAR_HEIGHT + HEADER_HEIGHT + FOOTER_HEIGHT;
        let body = Rect {
            y: area.y + (TAB_BAR_HEIGHT + HEADER_HEIGHT).min(area.height),
            height: area.height.saturating_sub(used),
            ..area
        };
        Self { tab_bar, body }
    }
}

/// What is under the pointer.
///
/// `chat_showing` says whether the body is chat panes at all, and
/// `combined` whether the body is the combined tab — which puts a
/// seven-row stream-info block above the panes.
pub fn target_at(
    area: Rect,
    x: u16,
    y: u16,
    chat_showing: bool,
    combined: bool,
    split_percent: u16,
) -> Target {
    let layout = Layout::of(area);
    if contains(layout.tab_bar, x, y) {
        return match tab_at(x) {
            Some(index) => Target::Tab(index),
            None => Target::Body,
        };
    }
    if !contains(layout.body, x, y) {
        return Target::Body;
    }

    let mut chat_area = layout.body;
    if combined {
        const STREAM_INFO_HEIGHT: u16 = 7;
        let stream_info = Rect {
            height: STREAM_INFO_HEIGHT.min(chat_area.height),
            ..chat_area
        };
        if contains(stream_info, x, y) {
            return Target::StreamInfo;
        }
        chat_area = Rect {
            y: chat_area.y + STREAM_INFO_HEIGHT,
            height: chat_area.height.saturating_sub(STREAM_INFO_HEIGHT),
            ..chat_area
        };
        // The combined tab wraps its chat half in a border, so the panes
        // inside it start one row and one column in.
        chat_area = Rect {
            x: chat_area.x + 1,
            y: chat_area.y + 1,
            width: chat_area.width.saturating_sub(2),
            height: chat_area.height.saturating_sub(2),
        };
    } else if !chat_showing {
        return Target::Body;
    }

    let (left, right) = chat_panes(chat_area, split_percent);
    if contains(left, x, y) {
        Target::ChatPane(Platform::Twitch)
    } else if contains(right, x, y) {
        Target::ChatPane(Platform::YouTube)
    } else {
        Target::Body
    }
}

/// What a mouse event means, or `None` for one that should be ignored.
///
/// Only three kinds of event do anything: the two wheel directions and a
/// press of the left button. Motion, drags and the other buttons are
/// deliberately ignored — a program that acted on pointer motion would fight
/// whatever the pointer was passing over on its way somewhere else.
pub fn action_for(
    event: MouseEvent,
    area: Rect,
    chat_showing: bool,
    combined: bool,
    split_percent: u16,
) -> Option<Action> {
    match event.kind {
        MouseEventKind::ScrollUp => Some(Action::ScrollBack),
        MouseEventKind::ScrollDown => Some(Action::ScrollForward),
        MouseEventKind::Down(crossterm::event::MouseButton::Left) => {
            match target_at(
                area,
                event.column,
                event.row,
                chat_showing,
                combined,
                split_percent,
            ) {
                Target::Tab(index) => Some(Action::SelectTab(index)),
                Target::ChatPane(platform) => Some(Action::FocusChat(platform)),
                Target::StreamInfo => Some(Action::FocusStreamInfo),
                Target::Body => None,
            }
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyModifiers, MouseButton};

    fn area() -> Rect {
        Rect {
            x: 0,
            y: 0,
            width: 100,
            height: 30,
        }
    }

    fn click(column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    fn wheel(up: bool) -> MouseEvent {
        MouseEvent {
            kind: if up {
                MouseEventKind::ScrollUp
            } else {
                MouseEventKind::ScrollDown
            },
            column: 10,
            row: 10,
            modifiers: KeyModifiers::NONE,
        }
    }

    /// Every tab has to be clickable somewhere, and the boxes must not
    /// overlap — a click landing on the wrong tab is worse than a click
    /// landing on none.
    #[test]
    fn each_tab_label_has_its_own_hit_box() {
        let mut seen = vec![None; 60];
        for x in 0..60u16 {
            seen[x as usize] = tab_at(x);
        }
        for index in 0..TAB_LABELS.len() {
            assert!(seen.contains(&Some(index)), "tab {index} cannot be clicked");
        }
        // The hit boxes appear in order, left to right, with no interleaving.
        let order: Vec<usize> = seen.iter().flatten().copied().collect();
        let mut sorted = order.clone();
        sorted.sort_unstable();
        assert_eq!(order, sorted, "the tab hit boxes are out of order");
    }

    /// The first column of the bar is the space before the first label, which
    /// is part of that label's box; well past the last label is nothing.
    #[test]
    fn clicking_beyond_the_last_tab_hits_nothing() {
        assert_eq!(tab_at(0), Some(0));
        assert_eq!(tab_at(99), None);
    }

    #[test]
    fn clicking_a_tab_selects_it() {
        assert_eq!(
            action_for(click(1, 0), area(), false, false, 50),
            Some(Action::SelectTab(0))
        );
        // "2 Chat" starts after "1 Stream Info" (15 cells) plus a separator.
        assert_eq!(
            action_for(click(17, 0), area(), false, false, 50),
            Some(Action::SelectTab(1))
        );
    }

    #[test]
    fn clicking_a_chat_pane_focuses_that_platform() {
        // The chat tab: the body is two panes, split down the middle.
        assert_eq!(
            action_for(click(10, 10), area(), true, false, 50),
            Some(Action::FocusChat(Platform::Twitch))
        );
        assert_eq!(
            action_for(click(90, 10), area(), true, false, 50),
            Some(Action::FocusChat(Platform::YouTube))
        );
    }

    /// The split is not always down the middle, and the hit boxes have to
    /// follow it or clicking the pane you can see gives you the other one.
    #[test]
    fn the_pane_hit_boxes_follow_the_split() {
        // With the left pane at 80%, column 70 is still the left pane.
        assert_eq!(
            action_for(click(70, 10), area(), true, false, 80),
            Some(Action::FocusChat(Platform::Twitch))
        );
        // And with it at 20%, the same column is the right one.
        assert_eq!(
            action_for(click(70, 10), area(), true, false, 20),
            Some(Action::FocusChat(Platform::YouTube))
        );
    }

    /// On the combined tab the top block is stream info and the chats are
    /// below it, so a click near the top must not land in a chat pane.
    #[test]
    fn the_combined_tab_separates_the_stream_info_from_the_chats() {
        assert_eq!(
            action_for(click(10, 5), area(), true, true, 50),
            Some(Action::FocusStreamInfo)
        );
        assert_eq!(
            action_for(click(10, 20), area(), true, true, 50),
            Some(Action::FocusChat(Platform::Twitch))
        );
    }

    #[test]
    fn clicking_the_body_of_a_non_chat_screen_does_nothing() {
        assert_eq!(action_for(click(10, 10), area(), false, false, 50), None);
    }

    #[test]
    fn the_wheel_scrolls_in_both_directions() {
        assert_eq!(
            action_for(wheel(true), area(), true, false, 50),
            Some(Action::ScrollBack)
        );
        assert_eq!(
            action_for(wheel(false), area(), true, false, 50),
            Some(Action::ScrollForward)
        );
    }

    /// Pointer motion must be ignored, or the interface would react to the
    /// pointer merely passing over it on the way somewhere else.
    #[test]
    fn motion_and_other_buttons_are_ignored() {
        for kind in [
            MouseEventKind::Moved,
            MouseEventKind::Down(MouseButton::Right),
            MouseEventKind::Up(MouseButton::Left),
            MouseEventKind::Drag(MouseButton::Left),
        ] {
            let event = MouseEvent {
                kind,
                column: 10,
                row: 10,
                modifiers: KeyModifiers::NONE,
            };
            assert_eq!(
                action_for(event, area(), true, false, 50),
                None,
                "{kind:?} should be ignored"
            );
        }
    }

    /// A terminal small enough that the body has no rows at all must not
    /// produce nonsense — or panic.
    #[test]
    fn a_tiny_terminal_is_handled_without_panicking() {
        for height in 0..8u16 {
            for width in 0..8u16 {
                let tiny = Rect {
                    x: 0,
                    y: 0,
                    width,
                    height,
                };
                for row in 0..height.max(1) {
                    for column in 0..width.max(1) {
                        action_for(click(column, row), tiny, true, true, 50);
                    }
                }
            }
        }
    }
}
