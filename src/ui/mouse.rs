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
use crate::ui::app::Tab;

/// What the pointer is over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// One of the top-level tabs, by its index in the tab bar.
    Tab(Tab),
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
    SelectTab(Tab),
    /// Give the keyboard to a chat pane.
    FocusChat(Platform),
    /// Give the keyboard to the stream-info half of the Combined tab.
    FocusStreamInfo,
    /// Scroll one chat pane, named because the pointer was over it rather
    /// than because it has the keyboard.
    ScrollPane { platform: Platform, back: bool },
    /// Scroll back through history (away from the newest).
    ScrollBack,
    /// Scroll forward toward the newest.
    ScrollForward,
}

/// Which tab sits at column `x` of the tab bar, if any.
///
/// The bar is drawn as `" label " " " " label "…` — each label padded with one
/// space either side, separated by one more. The labels come from
/// [`Tab::label`], the same source the bar is drawn from, because a hit box
/// built from a second copy of the strings is a click landing one column out:
/// the kind of bug nobody reports, they just decide the mouse does not work.
pub fn tab_at(x: u16) -> Option<Tab> {
    let mut cursor = 0u16;
    for tab in Tab::ALL {
        // One space before the label, the label, one space after.
        let width = tab.label().chars().count() as u16 + 2;
        if x >= cursor && x < cursor + width {
            return Some(tab);
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

/// How many rows the combined tab's stream-info block takes above the chat
/// panes. Shared so the hit-testing and the chat paging agree about where the
/// panes actually start.
pub const STREAM_INFO_HEIGHT: u16 = 7;

/// What kind of body the current tab is drawing, for hit-testing.
///
/// `target_at` and `action_for` used to take this as two `bool` parameters,
/// `chat_showing` and `combined` — a pair easy to swap at the call site
/// without the compiler noticing, since both are plain `bool`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BodyKind {
    /// Neither chat pane nor the combined tab — nothing below the header is
    /// hit-testable.
    Other,
    /// The Chat tab: the body is the two chat panes, nothing else.
    Chat,
    /// The Combined tab: a stream-info block above the two chat panes.
    Combined,
}

/// What is under the pointer.
pub fn target_at(area: Rect, x: u16, y: u16, body: BodyKind, split_percent: u16) -> Target {
    let layout = Layout::of(area);
    if contains(layout.tab_bar, x, y) {
        return match tab_at(x) {
            Some(tab) => Target::Tab(tab),
            None => Target::Body,
        };
    }
    if !contains(layout.body, x, y) {
        return Target::Body;
    }

    let mut chat_area = layout.body;
    if body == BodyKind::Combined {
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
    } else if body != BodyKind::Chat {
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
    body: BodyKind,
    split_percent: u16,
) -> Option<Action> {
    match event.kind {
        // Scrolling acts on the pane under the pointer, not the focused one.
        // Rolling the wheel over the YouTube pane scrolled Twitch — and since
        // scrolling clears the selection, it silently dropped a reply armed
        // in the other pane.
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
            let back = matches!(event.kind, MouseEventKind::ScrollUp);
            if let Target::ChatPane(platform) =
                target_at(area, event.column, event.row, body, split_percent)
            {
                return Some(Action::ScrollPane { platform, back });
            }
            Some(if back {
                Action::ScrollBack
            } else {
                Action::ScrollForward
            })
        }
        MouseEventKind::Down(crossterm::event::MouseButton::Left) => {
            match target_at(area, event.column, event.row, body, split_percent) {
                Target::Tab(tab) => Some(Action::SelectTab(tab)),
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
        // Wide enough for all five labels plus their separators.
        let width = 80u16;
        let seen: Vec<Option<Tab>> = (0..width).map(tab_at).collect();

        for tab in Tab::ALL {
            assert!(
                seen.contains(&Some(tab)),
                "{} cannot be clicked",
                tab.label()
            );
        }

        // The hit boxes appear in order, left to right, with no interleaving.
        let order: Vec<usize> = seen
            .iter()
            .flatten()
            .map(|tab| Tab::ALL.iter().position(|other| other == tab).unwrap())
            .collect();
        let mut sorted = order.clone();
        sorted.sort_unstable();
        assert_eq!(order, sorted, "the tab hit boxes are out of order");
    }

    /// The hit box for each label has to be exactly as wide as the label the
    /// tab bar draws, or clicks land one column out at the right-hand end.
    /// Both now come from `Tab::label`, and this checks the geometry that
    /// connects them.
    #[test]
    fn a_hit_box_is_exactly_as_wide_as_its_drawn_label() {
        let mut expected_start = 0u16;
        for tab in Tab::ALL {
            let width = tab.label().chars().count() as u16 + 2;
            assert_eq!(
                tab_at(expected_start),
                Some(tab),
                "the first column of {} must hit it",
                tab.label()
            );
            assert_eq!(
                tab_at(expected_start + width - 1),
                Some(tab),
                "the last column of {} must hit it",
                tab.label()
            );
            assert_ne!(
                tab_at(expected_start + width),
                Some(tab),
                "the separator after {} must not hit it",
                tab.label()
            );
            expected_start += width + 1;
        }
    }

    /// The first column of the bar is the space before the first label, which
    /// is part of that label's box; well past the last label is nothing.
    #[test]
    fn clicking_beyond_the_last_tab_hits_nothing() {
        assert_eq!(tab_at(0), Some(Tab::StreamInfo));
        assert_eq!(tab_at(199), None);
    }

    #[test]
    fn clicking_a_tab_selects_it() {
        assert_eq!(
            action_for(click(1, 0), area(), BodyKind::Other, 50),
            Some(Action::SelectTab(Tab::StreamInfo))
        );
        // "2 Chat" starts after "1 Stream Info" (15 cells) plus a separator.
        assert_eq!(
            action_for(click(17, 0), area(), BodyKind::Other, 50),
            Some(Action::SelectTab(Tab::Chat))
        );
    }

    #[test]
    fn clicking_a_chat_pane_focuses_that_platform() {
        // The chat tab: the body is two panes, split down the middle.
        assert_eq!(
            action_for(click(10, 10), area(), BodyKind::Chat, 50),
            Some(Action::FocusChat(Platform::Twitch))
        );
        assert_eq!(
            action_for(click(90, 10), area(), BodyKind::Chat, 50),
            Some(Action::FocusChat(Platform::YouTube))
        );
    }

    /// The split is not always down the middle, and the hit boxes have to
    /// follow it or clicking the pane you can see gives you the other one.
    #[test]
    fn the_pane_hit_boxes_follow_the_split() {
        // With the left pane at 80%, column 70 is still the left pane.
        assert_eq!(
            action_for(click(70, 10), area(), BodyKind::Chat, 80),
            Some(Action::FocusChat(Platform::Twitch))
        );
        // And with it at 20%, the same column is the right one.
        assert_eq!(
            action_for(click(70, 10), area(), BodyKind::Chat, 20),
            Some(Action::FocusChat(Platform::YouTube))
        );
    }

    /// On the combined tab the top block is stream info and the chats are
    /// below it, so a click near the top must not land in a chat pane.
    #[test]
    fn the_combined_tab_separates_the_stream_info_from_the_chats() {
        assert_eq!(
            action_for(click(10, 5), area(), BodyKind::Combined, 50),
            Some(Action::FocusStreamInfo)
        );
        assert_eq!(
            action_for(click(10, 20), area(), BodyKind::Combined, 50),
            Some(Action::FocusChat(Platform::Twitch))
        );
    }

    #[test]
    fn clicking_the_body_of_a_non_chat_screen_does_nothing() {
        assert_eq!(action_for(click(10, 10), area(), BodyKind::Other, 50), None);
    }

    /// Over a chat pane, the wheel scrolls *that* pane rather than whichever
    /// has the keyboard. It used to scroll the focused one, so rolling over
    /// the YouTube pane scrolled Twitch — and since scrolling clears the
    /// selection, it silently dropped a reply armed in the other pane.
    #[test]
    fn the_wheel_scrolls_the_pane_under_the_pointer() {
        assert_eq!(
            action_for(wheel(true), area(), BodyKind::Chat, 50),
            Some(Action::ScrollPane {
                platform: Platform::Twitch,
                back: true
            })
        );
        assert_eq!(
            action_for(wheel(false), area(), BodyKind::Chat, 50),
            Some(Action::ScrollPane {
                platform: Platform::Twitch,
                back: false
            })
        );
    }

    /// Away from the chat panes it is the ordinary scroll, which is what the
    /// log and the message history use.
    #[test]
    fn the_wheel_scrolls_in_both_directions_elsewhere() {
        assert_eq!(
            action_for(wheel(true), area(), BodyKind::Other, 50),
            Some(Action::ScrollBack)
        );
        assert_eq!(
            action_for(wheel(false), area(), BodyKind::Other, 50),
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
                action_for(event, area(), BodyKind::Chat, 50),
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
                        action_for(click(column, row), tiny, BodyKind::Combined, 50);
                    }
                }
            }
        }
    }
}
