//! A configurable panel layout for the Combined tab.
//!
//! The Combined tab is the one you put on a second monitor and leave there for
//! the whole stream. Today its arrangement is fixed in the drawing code: a
//! seven-row stream-info strip across the top, and the two chats side by side
//! underneath. That is a reasonable default and a poor rule — somebody running
//! OBS Studio on the same machine wants the scene list up there instead,
//! somebody with one busy chat and one quiet one wants them at different sizes,
//! and somebody with a very wide monitor wants three columns.
//!
//! This module is the data model and the arithmetic behind that choice. It
//! answers one question: *given a rectangle and a description of the wanted
//! arrangement, which panel goes where?* It does not draw anything, does not
//! know what a widget is, and does not know what any panel's contents look
//! like. The drawing code asks [`Layout::resolve`] for a list of
//! `(panel, rectangle)` pairs and renders each one however it already does.
//!
//! Two shapes of the same information live in here:
//!
//! * [`Node`] — a tree of splits and panels. This is the internal model,
//!   because recursive splitting is what the arithmetic naturally works on.
//! * [`LayoutFile`] — a flat list of rows. This is the on-disk model, because
//!   a recursive tree serialises into TOML as a pile of nested tables that
//!   nobody wants to hand-edit. See the comment on [`LayoutFile`] for the full
//!   reasoning.

// Nothing outside this module calls into it yet: the Combined tab still draws
// its fixed arrangement, and wiring the two together is a separate change. The
// allowance is here rather than on each item so it can be deleted in one line
// once the drawing code reads its layout from here.
#![allow(dead_code)]

use serde::{Deserialize, Serialize};

use ratatui::layout::Rect;

/// How deep a layout tree is allowed to nest.
///
/// A split inside a split inside a split is already hard to picture; eight
/// levels is far past anything a person would arrange on purpose, so a tree
/// deeper than this is much more likely to be a mistake (or a hand-written
/// file gone wrong) than an intention. Refusing it early gives a readable
/// error instead of a layout where panels are one cell tall.
pub const MAX_DEPTH: usize = 8;

/// Something that can be placed in the Combined tab.
///
/// These are the parts of the interface that make sense on their own, away
/// from the tab they normally live in. Adding a new one means adding a variant
/// here, a name, a title, and a line in [`Panel::ALL`] — and then teaching the
/// drawing code to render it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Panel {
    /// The stream summary: title, category, which platforms are live.
    StreamInfo,
    /// Twitch chat messages.
    TwitchChat,
    /// YouTube chat messages.
    YouTubeChat,
    /// The OBS Studio scene list.
    ObsScenes,
    /// OBS Studio audio inputs and their levels.
    ObsAudio,
    /// The OBS Studio connection and recording/streaming state.
    ObsStatus,
    /// The rolling log of what the program has been doing.
    ActivityLog,
    /// Live viewer counts and stream health.
    Stats,
}

impl Panel {
    /// Every panel, in the order they are offered to a user.
    ///
    /// Keeping this as one list means the config documentation, the
    /// `everything` preset and any future picker all stay in step with the
    /// enum instead of each carrying their own copy that can fall behind.
    pub const ALL: [Panel; 8] = [
        Panel::StreamInfo,
        Panel::TwitchChat,
        Panel::YouTubeChat,
        Panel::ObsScenes,
        Panel::ObsAudio,
        Panel::ObsStatus,
        Panel::ActivityLog,
        Panel::Stats,
    ];

    /// The name this panel is written as in the config file.
    ///
    /// snake_case, to match every other name in `config.toml`.
    pub fn name(self) -> &'static str {
        match self {
            Panel::StreamInfo => "stream_info",
            Panel::TwitchChat => "twitch_chat",
            Panel::YouTubeChat => "youtube_chat",
            Panel::ObsScenes => "obs_scenes",
            Panel::ObsAudio => "obs_audio",
            Panel::ObsStatus => "obs_status",
            Panel::ActivityLog => "activity_log",
            Panel::Stats => "stats",
        }
    }

    /// The human title drawn on the pane's border.
    pub fn title(self) -> &'static str {
        match self {
            Panel::StreamInfo => "Stream info",
            Panel::TwitchChat => "Twitch chat",
            Panel::YouTubeChat => "YouTube chat",
            Panel::ObsScenes => "Scenes",
            Panel::ObsAudio => "Audio",
            Panel::ObsStatus => "OBS status",
            Panel::ActivityLog => "Activity",
            Panel::Stats => "Statistics",
        }
    }

    /// Read a panel back from its config-file name.
    ///
    /// Case and surrounding whitespace are forgiven, and `-` is accepted for
    /// `_`, because a config file is typed by hand and `twitch-chat` is an
    /// honest mistake rather than a different request.
    pub fn parse(text: &str) -> Result<Panel, String> {
        let wanted = text.trim().to_ascii_lowercase().replace('-', "_");
        Panel::ALL
            .into_iter()
            .find(|panel| panel.name() == wanted)
            .ok_or_else(|| {
                let names: Vec<&str> = Panel::ALL.iter().map(|panel| panel.name()).collect();
                format!(
                    "unknown panel {text:?}; the panels available are: {}",
                    names.join(", ")
                )
            })
    }
}

impl std::fmt::Display for Panel {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.name())
    }
}

impl Serialize for Panel {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.name())
    }
}

impl<'de> Deserialize<'de> for Panel {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Panel, D::Error> {
        let text = String::deserialize(deserializer)?;
        Panel::parse(&text).map_err(serde::de::Error::custom)
    }
}

/// Which way a split cuts its area.
///
/// `Horizontal` places its children left to right (so it cuts vertical lines),
/// matching the sense ratatui uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    /// Children sit side by side, sharing the width.
    Horizontal,
    /// Children sit one above the other, sharing the height.
    Vertical,
}

/// A layout tree: either a single panel, or an area divided between children.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Node {
    /// A leaf: this rectangle belongs to one panel.
    Panel(Panel),
    /// A branch: this rectangle is shared out between the children.
    Split {
        /// Which way the area is cut.
        direction: Direction,
        /// The children, in order (left to right, or top to bottom).
        children: Vec<Child>,
    },
}

/// One child of a split, together with the share of the space it asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Child {
    /// This child's share of the parent's space.
    ///
    /// Weights are *proportional shares*, in the way a CSS flex-grow value is,
    /// and deliberately not percentages. Two reasons, both of which show up
    /// the first time somebody edits their config:
    ///
    /// 1. Shares always add up. Percentages have to sum to 100, and a file
    ///    saying `40, 40, 40` has no correct reading — the program must either
    ///    reject a layout that is otherwise perfectly clear, or silently pick
    ///    a meaning. With shares, `40, 40, 40` is three equal columns, and so
    ///    is `1, 1, 1`.
    /// 2. Adding a panel does not mean re-editing every other number. Dropping
    ///    a fourth column into `1, 1, 1` is one new line. Dropping one into
    ///    `34, 33, 33` means recomputing all four values by hand.
    ///
    /// A weight of `0` is allowed and means "no space": a way to park a panel
    /// in the file without it taking up room.
    pub weight: u16,
    /// The panel or nested split that occupies this child's share.
    pub node: Node,
}

impl Child {
    /// A child holding a single panel.
    pub fn panel(weight: u16, panel: Panel) -> Child {
        Child {
            weight,
            node: Node::Panel(panel),
        }
    }

    /// A child holding a nested split.
    pub fn split(weight: u16, direction: Direction, children: Vec<Child>) -> Child {
        Child {
            weight,
            node: Node::Split {
                direction,
                children,
            },
        }
    }
}

/// A complete arrangement for the Combined tab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    /// The top of the tree. A layout with exactly one panel is a bare
    /// `Node::Panel`, so the simple case does not need a pointless split.
    pub root: Node,
}

impl Default for Layout {
    /// The arrangement the Combined tab has today.
    ///
    /// This must keep matching the hard-coded arrangement in
    /// `ui::draw::draw_combined`: stream info across the top, the two chats
    /// side by side beneath it. Anyone upgrading who has never opened the
    /// layout settings should see the tab they are used to, not a surprise.
    ///
    /// The weights `1` and `3` are the closest proportional reading of the old
    /// fixed seven-row strip: on a typical full-height terminal the strip took
    /// roughly a quarter of the tab.
    fn default() -> Self {
        Layout {
            root: Node::Split {
                direction: Direction::Vertical,
                children: vec![
                    Child::panel(1, Panel::StreamInfo),
                    Child::split(
                        3,
                        Direction::Horizontal,
                        vec![
                            Child::panel(1, Panel::TwitchChat),
                            Child::panel(1, Panel::YouTubeChat),
                        ],
                    ),
                ],
            },
        }
    }
}

impl Layout {
    /// Work out which rectangle each panel occupies inside `area`.
    ///
    /// The returned rectangles tile `area` exactly: none of them falls outside
    /// it, none of them overlaps another, and — apart from panels whose weight
    /// is zero, and rows too thin to be divided — together they cover it with
    /// no gaps. The order matches [`Layout::panels`].
    ///
    /// Nothing in here can panic. A zero-sized area, weights that are all
    /// zero, a rectangle narrower than the number of columns asked for: each
    /// of those produces empty rectangles rather than an arithmetic failure,
    /// because a terminal being dragged very small is normal use, not an
    /// error.
    pub fn resolve(&self, area: Rect) -> Vec<(Panel, Rect)> {
        let mut out = Vec::new();
        resolve_into(&self.root, area, &mut out);
        out
    }

    /// Every panel in the tree, in the order they are laid out.
    pub fn panels(&self) -> Vec<Panel> {
        let mut out = Vec::new();
        collect_panels(&self.root, &mut out);
        out
    }

    /// Check the layout makes sense, returning a reason a person can act on.
    ///
    /// This is for hand-edited config files. The errors name the problem in
    /// the words of the file rather than of the tree, so the reader knows
    /// which line to go and change.
    pub fn validate(&self) -> Result<(), String> {
        validate_node(&self.root, 1)
    }

    /// Build a layout from its flat, on-disk form.
    pub fn from_file(file: &LayoutFile) -> Result<Layout, String> {
        file.to_layout()
    }

    /// Convert this layout to the flat, on-disk form.
    ///
    /// Only the two-level shape the file format can express survives the trip
    /// — see [`LayoutFile`] for why the file format is deliberately shallower
    /// than the tree — so a deeper tree comes back as `None`.
    pub fn to_file(&self) -> Option<LayoutFile> {
        LayoutFile::from_layout(self)
    }
}

/// Share `total` cells out between `weights`, using every cell exactly once.
///
/// The obvious version of this — round each share on its own — loses cells to
/// rounding and leaves a gap at the end of the row. This uses the
/// largest-remainder method instead: hand out the whole-number part of each
/// share, then give the leftover cells, one each, to whichever children were
/// cut by the most. That keeps the proportions as close as integers allow and
/// guarantees the parts add back up to `total`.
fn share(total: u16, weights: &[u16]) -> Vec<u16> {
    if weights.is_empty() {
        return Vec::new();
    }

    // Every weight being zero would mean dividing by zero, and it is also the
    // shape of a file where somebody wrote the panels but not the numbers.
    // Treating that as "equal shares" keeps their panels visible, which is
    // closer to what they meant than a blank tab. A *mixture* of zero and
    // non-zero weights is a deliberate "hide this one", so it is left alone.
    let sum: u64 = weights.iter().map(|&w| u64::from(w)).sum();
    let effective: Vec<u64> = if sum == 0 {
        vec![1; weights.len()]
    } else {
        weights.iter().map(|&w| u64::from(w)).collect()
    };
    let sum: u64 = effective.iter().sum();

    let total_u = u64::from(total);
    let mut sizes: Vec<u16> = Vec::with_capacity(effective.len());
    let mut remainders: Vec<(u64, usize)> = Vec::with_capacity(effective.len());
    let mut handed_out: u64 = 0;
    for (index, &weight) in effective.iter().enumerate() {
        let exact = total_u * weight;
        let whole = exact / sum;
        sizes.push(whole as u16);
        remainders.push((exact % sum, index));
        handed_out += whole;
    }

    // Biggest shortfall first; ties go to the earlier child so the result does
    // not depend on the sort being stable in any particular way.
    remainders.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    let mut leftover = total_u - handed_out;
    for &(_, index) in &remainders {
        if leftover == 0 {
            break;
        }
        sizes[index] += 1;
        leftover -= 1;
    }
    sizes
}

/// Cut `area` into the pieces described by `children`.
fn cut(area: Rect, direction: Direction, children: &[Child]) -> Vec<Rect> {
    let weights: Vec<u16> = children.iter().map(|child| child.weight).collect();
    let along = match direction {
        Direction::Horizontal => area.width,
        Direction::Vertical => area.height,
    };
    let sizes = share(along, &weights);

    let mut rects = Vec::with_capacity(children.len());
    let mut offset: u16 = 0;
    for size in sizes {
        rects.push(match direction {
            Direction::Horizontal => Rect {
                x: area.x + offset,
                y: area.y,
                width: size,
                height: area.height,
            },
            Direction::Vertical => Rect {
                x: area.x,
                y: area.y + offset,
                width: area.width,
                height: size,
            },
        });
        offset += size;
    }
    rects
}

fn resolve_into(node: &Node, area: Rect, out: &mut Vec<(Panel, Rect)>) {
    match node {
        Node::Panel(panel) => out.push((*panel, area)),
        Node::Split {
            direction,
            children,
        } => {
            for (child, rect) in children.iter().zip(cut(area, *direction, children)) {
                resolve_into(&child.node, rect, out);
            }
        }
    }
}

fn collect_panels(node: &Node, out: &mut Vec<Panel>) {
    match node {
        Node::Panel(panel) => out.push(*panel),
        Node::Split { children, .. } => {
            for child in children {
                collect_panels(&child.node, out);
            }
        }
    }
}

fn validate_node(node: &Node, depth: usize) -> Result<(), String> {
    match node {
        Node::Panel(_) => Ok(()),
        Node::Split { children, .. } => {
            if depth > MAX_DEPTH {
                return Err(format!(
                    "this layout nests splits {depth} deep, and the limit is {MAX_DEPTH}; \
                     flatten it by putting more panels in one row instead of splitting \
                     a split of a split"
                ));
            }
            if children.is_empty() {
                return Err(
                    "a split has no panels in it; either give it at least one panel or \
                     remove the split"
                        .to_string(),
                );
            }
            if children.iter().all(|child| child.weight == 0) {
                return Err(
                    "every panel in one split has a weight of 0, so the split would take \
                     up no space at all; give at least one of them a weight of 1 or more"
                        .to_string(),
                );
            }
            for child in children {
                validate_node(&child.node, depth + 1)?;
            }
            Ok(())
        }
    }
}

// ---------------------------------------------------------------------------
// The on-disk form
// ---------------------------------------------------------------------------

/// The layout as it is written in `config.toml`.
///
/// **Why this is not the tree.** Serialising [`Node`] directly would work, and
/// the result would be horrible to read or type. An enum with a struct variant
/// becomes a nested TOML table, so a two-level layout ends up as something
/// like `[layout.root.split.children.split]` with the panels buried several
/// tables down, and the indentation carrying meaning that TOML does not
/// actually give it. People edit this file by hand — that is the whole point
/// of having a config file — so the written form is chosen for the reader,
/// not for the serialiser.
///
/// The written form is a list of rows, each row a list of panels:
///
/// ```toml
/// [layout]
/// # "vertical" stacks the rows top to bottom; "horizontal" makes them columns.
/// direction = "vertical"
///
/// [[layout.rows]]
/// weight = 1
/// panels = [{ panel = "stream_info", weight = 1 }]
///
/// [[layout.rows]]
/// weight = 3
/// panels = [
///     { panel = "twitch_chat", weight = 1 },
///     { panel = "youtube_chat", weight = 1 },
/// ]
/// ```
///
/// That reads top to bottom in the same order it appears on screen, every
/// number sits next to the thing it sizes, and adding a panel is one more
/// line. The cost is that the file can only express two levels — rows, and
/// panels within a row — where the tree can nest without limit. That is an
/// accepted trade: two levels covers the arrangements people actually want on
/// a second monitor, and the tree stays the internal model so a future format
/// (or a layout built in code) can still go deeper without changing
/// [`Layout::resolve`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct LayoutFile {
    /// Which way the rows are stacked.
    ///
    /// `vertical`, the default, means each row is a band across the screen and
    /// the panels inside it sit side by side — the arrangement the Combined
    /// tab has always had. `horizontal` turns the rows into columns, which
    /// suits a very wide monitor.
    pub direction: Direction,

    /// The rows, in the order they appear on screen.
    ///
    /// An empty list means "use the default arrangement", so deleting the
    /// section from the file is a way back rather than a blank tab.
    pub rows: Vec<Row>,
}

impl Default for LayoutFile {
    /// The written form of [`Layout::default`], so `msm init` can print the
    /// current arrangement rather than an empty section.
    fn default() -> Self {
        LayoutFile::from_layout(&Layout::default())
            .expect("the default layout is two levels deep and always fits the file format")
    }
}

/// One row of the written layout: a band of the screen shared between panels.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Row {
    /// This row's share of the screen, relative to the other rows.
    ///
    /// Shares, not percentages: see [`Child::weight`] for why.
    pub weight: u16,

    /// The panels in this row, left to right.
    pub panels: Vec<Cell>,
}

impl Default for Row {
    fn default() -> Self {
        Row {
            // A row that names panels but forgets its weight almost certainly
            // wants an ordinary share rather than to be invisible.
            weight: 1,
            panels: Vec::new(),
        }
    }
}

/// One panel inside a row, and its share of that row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Cell {
    /// Which panel this is, by its config-file name.
    pub panel: Option<Panel>,
    /// This panel's share of the row, relative to the other panels in it.
    pub weight: u16,
}

impl Default for Cell {
    fn default() -> Self {
        Cell {
            panel: None,
            weight: 1,
        }
    }
}

impl LayoutFile {
    /// Turn the written form into the tree the arithmetic works on.
    ///
    /// This also validates, so a bad file is reported once, in the reader's
    /// terms, instead of quietly producing a strange screen.
    pub fn to_layout(&self) -> Result<Layout, String> {
        if self.rows.is_empty() {
            return Ok(Layout::default());
        }

        let mut children = Vec::with_capacity(self.rows.len());
        for (index, row) in self.rows.iter().enumerate() {
            let number = index + 1;
            if row.panels.is_empty() {
                return Err(format!(
                    "row {number} has no panels in it; give it at least one \
                     `{{ panel = \"...\" }}` or delete the row"
                ));
            }
            let mut cells = Vec::with_capacity(row.panels.len());
            for cell in &row.panels {
                let panel = cell.panel.ok_or_else(|| {
                    format!("a panel in row {number} has no `panel = \"...\"` name")
                })?;
                cells.push(Child::panel(cell.weight, panel));
            }
            let inner = if cells.len() == 1 {
                // One panel in a row does not need a split around it, and a
                // bare panel keeps the tree (and any error message about its
                // depth) as shallow as the file looks.
                cells.remove(0).node
            } else {
                Node::Split {
                    direction: self.direction.flipped(),
                    children: cells,
                }
            };
            children.push(Child {
                weight: row.weight,
                node: inner,
            });
        }

        let root = if children.len() == 1 {
            children.remove(0).node
        } else {
            Node::Split {
                direction: self.direction,
                children,
            }
        };
        let layout = Layout { root };
        layout.validate()?;
        Ok(layout)
    }

    /// Write a tree back out in the flat form, if it is shallow enough.
    ///
    /// Returns `None` for a tree deeper than the file format can hold, so the
    /// caller can keep the old text rather than saving something that would
    /// read back as a different layout.
    pub fn from_layout(layout: &Layout) -> Option<LayoutFile> {
        // A single panel is a one-row, one-panel file.
        let (direction, children) = match &layout.root {
            Node::Panel(panel) => {
                return Some(LayoutFile {
                    direction: Direction::Vertical,
                    rows: vec![Row {
                        weight: 1,
                        panels: vec![Cell {
                            panel: Some(*panel),
                            weight: 1,
                        }],
                    }],
                })
            }
            Node::Split {
                direction,
                children,
            } => (*direction, children),
        };

        let mut rows = Vec::with_capacity(children.len());
        for child in children {
            let panels = match &child.node {
                Node::Panel(panel) => vec![Cell {
                    panel: Some(*panel),
                    weight: 1,
                }],
                Node::Split {
                    direction: inner,
                    children: cells,
                } => {
                    // A row's panels must run across the row, so an inner
                    // split that cuts the same way as the outer one is a shape
                    // the flat form cannot represent.
                    if *inner != direction.flipped() {
                        return None;
                    }
                    let mut out = Vec::with_capacity(cells.len());
                    for cell in cells {
                        match &cell.node {
                            Node::Panel(panel) => out.push(Cell {
                                panel: Some(*panel),
                                weight: cell.weight,
                            }),
                            // A third level: too deep for the flat form.
                            Node::Split { .. } => return None,
                        }
                    }
                    out
                }
            };
            rows.push(Row {
                weight: child.weight,
                panels,
            });
        }
        Some(LayoutFile { direction, rows })
    }
}

impl Direction {
    /// The other direction. A row of a vertical stack runs horizontally, so
    /// the file's single `direction` is enough to place both levels.
    pub fn flipped(self) -> Direction {
        match self {
            Direction::Horizontal => Direction::Vertical,
            Direction::Vertical => Direction::Horizontal,
        }
    }
}

// ---------------------------------------------------------------------------
// Presets
// ---------------------------------------------------------------------------

/// Ready-made arrangements to start from.
///
/// Nobody wants to design a layout from nothing on their first evening. A
/// preset is a starting point: pick the one nearest what you want, save it,
/// then move the numbers around.
pub mod presets {
    use super::{Child, Direction, Layout, Node, Panel};

    /// Every preset, as `(name, description)`, for listing in the interface
    /// and in the generated config comments.
    pub const NAMES: [(&str, &str); 4] = [
        ("default", "Stream info across the top, both chats beneath"),
        ("chat_focus", "Big chats, a thin strip of stream info"),
        ("obs_focus", "OBS scenes and audio down one side"),
        (
            "everything",
            "All eight panels at once, for a large monitor",
        ),
    ];

    /// Look a preset up by name. Unknown names return `None` so the caller can
    /// say which names do exist.
    pub fn by_name(name: &str) -> Option<Layout> {
        match name.trim().to_ascii_lowercase().replace('-', "_").as_str() {
            "default" => Some(default()),
            "chat_focus" => Some(chat_focus()),
            "obs_focus" => Some(obs_focus()),
            "everything" => Some(everything()),
            _ => None,
        }
    }

    /// Today's Combined tab.
    pub fn default() -> Layout {
        Layout::default()
    }

    /// For when the chat is the show: the stream summary shrinks to a
    /// reminder strip and the two chats take everything else.
    pub fn chat_focus() -> Layout {
        Layout {
            root: Node::Split {
                direction: Direction::Vertical,
                children: vec![
                    Child::panel(1, Panel::StreamInfo),
                    Child::split(
                        9,
                        Direction::Horizontal,
                        vec![
                            Child::panel(1, Panel::TwitchChat),
                            Child::panel(1, Panel::YouTubeChat),
                        ],
                    ),
                ],
            },
        }
    }

    /// For running OBS Studio on the same machine: scenes and audio sit in a
    /// column down the left, where they can be clicked without leaving the
    /// tab, and the chats share the rest.
    pub fn obs_focus() -> Layout {
        Layout {
            root: Node::Split {
                direction: Direction::Horizontal,
                children: vec![
                    Child::split(
                        1,
                        Direction::Vertical,
                        vec![
                            Child::panel(2, Panel::ObsScenes),
                            Child::panel(2, Panel::ObsAudio),
                            Child::panel(1, Panel::ObsStatus),
                        ],
                    ),
                    Child::split(
                        2,
                        Direction::Vertical,
                        vec![
                            Child::panel(1, Panel::StreamInfo),
                            Child::panel(3, Panel::TwitchChat),
                            Child::panel(3, Panel::YouTubeChat),
                        ],
                    ),
                ],
            },
        }
    }

    /// Everything at once. This wants a large monitor; on a small terminal the
    /// panels will be a few rows each, which is honest rather than useful.
    pub fn everything() -> Layout {
        Layout {
            root: Node::Split {
                direction: Direction::Vertical,
                children: vec![
                    Child::split(
                        1,
                        Direction::Horizontal,
                        vec![
                            Child::panel(2, Panel::StreamInfo),
                            Child::panel(1, Panel::Stats),
                        ],
                    ),
                    Child::split(
                        3,
                        Direction::Horizontal,
                        vec![
                            Child::panel(1, Panel::TwitchChat),
                            Child::panel(1, Panel::YouTubeChat),
                        ],
                    ),
                    Child::split(
                        2,
                        Direction::Horizontal,
                        vec![
                            Child::panel(1, Panel::ObsScenes),
                            Child::panel(1, Panel::ObsAudio),
                            Child::panel(1, Panel::ObsStatus),
                            Child::panel(2, Panel::ActivityLog),
                        ],
                    ),
                ],
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A rectangle's cells, as (x, y) pairs, for the coverage checks.
    fn cells(rect: Rect) -> Vec<(u16, u16)> {
        let mut out = Vec::new();
        for y in rect.y..rect.y.saturating_add(rect.height) {
            for x in rect.x..rect.x.saturating_add(rect.width) {
                out.push((x, y));
            }
        }
        out
    }

    /// Every layout used by the sweeping tests below.
    fn every_layout() -> Vec<(&'static str, Layout)> {
        vec![
            ("default", Layout::default()),
            ("chat_focus", presets::chat_focus()),
            ("obs_focus", presets::obs_focus()),
            ("everything", presets::everything()),
            (
                "single panel",
                Layout {
                    root: Node::Panel(Panel::TwitchChat),
                },
            ),
        ]
    }

    #[test]
    fn every_panel_name_reads_back_as_the_same_panel() {
        for panel in Panel::ALL {
            assert_eq!(Panel::parse(panel.name()), Ok(panel));
        }
    }

    #[test]
    fn a_panel_name_is_forgiven_its_case_and_its_hyphens() {
        assert_eq!(Panel::parse("  Twitch-Chat "), Ok(Panel::TwitchChat));
    }

    #[test]
    fn an_unknown_panel_name_is_rejected_with_the_list_of_real_ones() {
        let message = Panel::parse("irc").unwrap_err();
        assert!(message.contains("irc"), "{message}");
        assert!(message.contains("twitch_chat"), "{message}");
    }

    #[test]
    fn no_two_panels_share_a_config_file_name() {
        let mut names: Vec<&str> = Panel::ALL.iter().map(|panel| panel.name()).collect();
        names.sort_unstable();
        let count = names.len();
        names.dedup();
        assert_eq!(names.len(), count, "two panels answer to the same name");
    }

    #[test]
    fn the_default_layout_reproduces_todays_combined_tab() {
        let layout = Layout::default();
        assert_eq!(
            layout.panels(),
            vec![Panel::StreamInfo, Panel::TwitchChat, Panel::YouTubeChat]
        );

        let resolved = layout.resolve(Rect::new(0, 0, 100, 40));
        let stream = resolved[0].1;
        let twitch = resolved[1].1;
        let youtube = resolved[2].1;

        // Stream info spans the full width along the top.
        assert_eq!(stream, Rect::new(0, 0, 100, 10));
        // The two chats sit side by side underneath, at equal widths.
        assert_eq!(twitch, Rect::new(0, 10, 50, 30));
        assert_eq!(youtube, Rect::new(50, 10, 50, 30));
    }

    #[test]
    fn resolved_rectangles_never_leave_the_area_they_were_given() {
        for (name, layout) in every_layout() {
            for width in 0..60u16 {
                for height in [0u16, 1, 2, 3, 7, 24, 41] {
                    let area = Rect::new(3, 5, width, height);
                    for (panel, rect) in layout.resolve(area) {
                        assert!(
                            rect.x >= area.x
                                && rect.y >= area.y
                                && rect.right() <= area.right()
                                && rect.bottom() <= area.bottom(),
                            "{name}: {panel} escaped {area:?} as {rect:?}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn resolved_rectangles_never_overlap_one_another() {
        for (name, layout) in every_layout() {
            for width in [0u16, 1, 5, 13, 40, 97] {
                for height in [0u16, 1, 4, 11, 30] {
                    let area = Rect::new(0, 0, width, height);
                    let mut seen = std::collections::HashSet::new();
                    for (panel, rect) in layout.resolve(area) {
                        for cell in cells(rect) {
                            assert!(
                                seen.insert(cell),
                                "{name}: {panel} covers {cell:?} twice at {width}x{height}"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn resolved_rectangles_cover_the_whole_area_with_no_gaps() {
        for (name, layout) in every_layout() {
            for width in [0u16, 1, 3, 8, 40, 97, 200] {
                for height in [0u16, 1, 2, 9, 30, 77] {
                    let area = Rect::new(0, 0, width, height);
                    let covered: usize = layout
                        .resolve(area)
                        .into_iter()
                        .map(|(_, rect)| usize::from(rect.width) * usize::from(rect.height))
                        .sum();
                    assert_eq!(
                        covered,
                        usize::from(width) * usize::from(height),
                        "{name}: {width}x{height} was not covered exactly once"
                    );
                }
            }
        }
    }

    #[test]
    fn the_shares_a_split_hands_out_add_up_to_the_space_it_was_given() {
        for total in 0..200u16 {
            for weights in [
                vec![1u16],
                vec![1, 1],
                vec![1, 3],
                vec![2, 2, 1],
                vec![7, 11, 13, 17],
                vec![0, 1],
                vec![0, 0, 0],
                vec![u16::MAX, 1],
            ] {
                let sizes = share(total, &weights);
                assert_eq!(sizes.len(), weights.len());
                let sum: u32 = sizes.iter().map(|&s| u32::from(s)).sum();
                assert_eq!(sum, u32::from(total), "{weights:?} at {total}");
            }
        }
    }

    #[test]
    fn a_panel_with_twice_the_weight_gets_twice_the_space_within_rounding() {
        let layout = Layout {
            root: Node::Split {
                direction: Direction::Horizontal,
                children: vec![
                    Child::panel(1, Panel::TwitchChat),
                    Child::panel(2, Panel::YouTubeChat),
                ],
            },
        };
        for width in 2..500u16 {
            let resolved = layout.resolve(Rect::new(0, 0, width, 10));
            let small = f64::from(resolved[0].1.width);
            let large = f64::from(resolved[1].1.width);
            let wanted = f64::from(width) / 3.0;
            // Integer cells cannot land on the exact ratio, so allow the one
            // cell of slack that rounding can cost each side.
            assert!(
                (small - wanted).abs() <= 1.0,
                "{width}: {small} vs {wanted}"
            );
            assert!(
                (large - 2.0 * wanted).abs() <= 1.0,
                "{width}: {large} vs {}",
                2.0 * wanted
            );
        }
    }

    #[test]
    fn a_weight_of_zero_takes_no_space_and_leaves_the_rest_untouched() {
        let layout = Layout {
            root: Node::Split {
                direction: Direction::Vertical,
                children: vec![
                    Child::panel(0, Panel::Stats),
                    Child::panel(1, Panel::TwitchChat),
                ],
            },
        };
        let resolved = layout.resolve(Rect::new(0, 0, 20, 10));
        assert_eq!(resolved[0].1.height, 0);
        assert_eq!(resolved[1].1, Rect::new(0, 0, 20, 10));
    }

    #[test]
    fn a_split_whose_weights_are_all_zero_still_shows_its_panels() {
        // The file says nothing about sizes, so equal shares is the reading
        // closest to what was written — and it keeps the tab from going blank.
        let sizes = share(10, &[0, 0]);
        assert_eq!(sizes, vec![5, 5]);
    }

    #[test]
    fn an_area_with_no_cells_in_it_produces_empty_rectangles_and_no_panic() {
        for (name, layout) in every_layout() {
            let resolved = layout.resolve(Rect::new(0, 0, 0, 0));
            assert_eq!(resolved.len(), layout.panels().len(), "{name}");
            for (_, rect) in resolved {
                assert_eq!(rect.width, 0);
                assert_eq!(rect.height, 0);
            }
        }
    }

    #[test]
    fn an_area_too_small_to_divide_gives_some_panels_nothing_rather_than_failing() {
        // Three columns in two cells: somebody has to miss out, and the ones
        // that do get an empty rectangle instead of an arithmetic error.
        let resolved = presets::everything().resolve(Rect::new(0, 0, 2, 1));
        assert!(resolved.iter().any(|(_, rect)| rect.width == 0));
        assert_eq!(resolved.len(), 8);
    }

    #[test]
    fn a_layout_nested_eight_splits_deep_resolves_without_panicking() {
        let mut node = Node::Panel(Panel::Stats);
        for depth in 0..MAX_DEPTH {
            let direction = if depth % 2 == 0 {
                Direction::Vertical
            } else {
                Direction::Horizontal
            };
            node = Node::Split {
                direction,
                children: vec![
                    Child::panel(1, Panel::TwitchChat),
                    Child { weight: 1, node },
                ],
            };
        }
        let layout = Layout { root: node };
        for size in 0..40u16 {
            let area = Rect::new(0, 0, size, size);
            let covered: usize = layout
                .resolve(area)
                .into_iter()
                .map(|(_, rect)| usize::from(rect.width) * usize::from(rect.height))
                .sum();
            assert_eq!(covered, usize::from(size) * usize::from(size));
        }
    }

    #[test]
    fn validate_accepts_every_preset() {
        for (name, _) in presets::NAMES {
            let layout = presets::by_name(name).unwrap_or_else(|| panic!("{name} is missing"));
            assert_eq!(layout.validate(), Ok(()), "{name}");
        }
    }

    #[test]
    fn validate_rejects_a_split_with_nothing_in_it() {
        let layout = Layout {
            root: Node::Split {
                direction: Direction::Vertical,
                children: Vec::new(),
            },
        };
        let message = layout.validate().unwrap_err();
        assert!(message.contains("no panels"), "{message}");
    }

    #[test]
    fn validate_rejects_a_split_whose_weights_are_all_zero() {
        let layout = Layout {
            root: Node::Split {
                direction: Direction::Vertical,
                children: vec![
                    Child::panel(0, Panel::Stats),
                    Child::panel(0, Panel::TwitchChat),
                ],
            },
        };
        let message = layout.validate().unwrap_err();
        assert!(message.contains("weight of 0"), "{message}");
    }

    #[test]
    fn validate_rejects_a_layout_nested_deeper_than_the_limit() {
        let mut node = Node::Panel(Panel::Stats);
        for _ in 0..=MAX_DEPTH {
            node = Node::Split {
                direction: Direction::Vertical,
                children: vec![Child { weight: 1, node }],
            };
        }
        let message = Layout { root: node }.validate().unwrap_err();
        assert!(message.contains("deep"), "{message}");
        assert!(message.contains("flatten"), "{message}");
    }

    #[test]
    fn the_default_layout_survives_a_round_trip_through_toml() {
        let layout = Layout::default();
        let file = layout.to_file().expect("the default fits the file format");
        let text = toml::to_string_pretty(&file).expect("serialises");
        let read_back: LayoutFile = toml::from_str(&text).expect("parses");
        assert_eq!(read_back, file, "the text did not read back as itself");
        assert_eq!(read_back.to_layout(), Ok(layout));
    }

    #[test]
    fn every_preset_that_fits_the_file_format_survives_a_round_trip_through_toml() {
        for (name, _) in presets::NAMES {
            let layout = presets::by_name(name).unwrap();
            let Some(file) = layout.to_file() else {
                continue;
            };
            let text = toml::to_string_pretty(&file).unwrap();
            let read_back: LayoutFile = toml::from_str(&text).unwrap();
            assert_eq!(read_back.to_layout(), Ok(layout), "{name}");
        }
    }

    #[test]
    fn a_hand_written_layout_section_parses_into_the_expected_tree() {
        let text = r#"
            direction = "vertical"

            [[rows]]
            weight = 1
            panels = [{ panel = "stream_info", weight = 1 }]

            [[rows]]
            weight = 3
            panels = [
                { panel = "twitch_chat", weight = 1 },
                { panel = "youtube_chat", weight = 1 },
            ]
        "#;
        let file: LayoutFile = toml::from_str(text).expect("parses");
        assert_eq!(file.to_layout(), Ok(Layout::default()));
    }

    #[test]
    fn a_layout_section_left_out_of_the_file_falls_back_to_the_default() {
        let file: LayoutFile = toml::from_str("").expect("an empty section parses");
        assert_eq!(file.to_layout(), Ok(Layout::default()));
    }

    #[test]
    fn a_row_with_no_panels_is_reported_with_the_row_number() {
        let file: LayoutFile = toml::from_str("[[rows]]\nweight = 1\npanels = []\n").unwrap();
        let message = file.to_layout().unwrap_err();
        assert!(message.contains("row 1"), "{message}");
    }

    #[test]
    fn a_misspelt_panel_name_in_the_file_is_reported_rather_than_ignored() {
        let error = toml::from_str::<LayoutFile>(
            "[[rows]]\npanels = [{ panel = \"twich_chat\", weight = 1 }]\n",
        )
        .unwrap_err();
        assert!(error.to_string().contains("unknown panel"), "{error}");
    }

    #[test]
    fn a_row_that_omits_its_weight_is_given_an_ordinary_share() {
        let file: LayoutFile =
            toml::from_str("[[rows]]\npanels = [{ panel = \"stats\" }]\n").unwrap();
        assert_eq!(file.rows[0].weight, 1);
        assert_eq!(file.rows[0].panels[0].weight, 1);
    }

    #[test]
    fn a_tree_too_deep_for_the_file_format_refuses_to_be_written_instead_of_losing_a_level() {
        let layout = Layout {
            root: Node::Split {
                direction: Direction::Vertical,
                children: vec![Child::split(
                    1,
                    Direction::Horizontal,
                    vec![Child::split(
                        1,
                        Direction::Vertical,
                        vec![Child::panel(1, Panel::Stats)],
                    )],
                )],
            },
        };
        assert_eq!(layout.to_file(), None);
    }

    #[test]
    fn a_layout_of_one_panel_writes_and_reads_back_as_one_row() {
        let layout = Layout {
            root: Node::Panel(Panel::ActivityLog),
        };
        let file = layout.to_file().expect("one panel always fits");
        assert_eq!(file.to_layout(), Ok(layout));
    }

    #[test]
    fn the_everything_preset_holds_all_eight_panels_exactly_once() {
        let mut panels = presets::everything().panels();
        assert_eq!(panels.len(), Panel::ALL.len());
        panels.sort();
        let mut all = Panel::ALL.to_vec();
        all.sort();
        assert_eq!(panels, all);
    }

    #[test]
    fn every_named_preset_can_be_looked_up_and_an_unknown_name_cannot() {
        for (name, description) in presets::NAMES {
            assert!(presets::by_name(name).is_some(), "{name}");
            assert!(!description.is_empty(), "{name} has no description");
        }
        assert!(presets::by_name("cinema").is_none());
    }
}
