//! Everything a key can be bound to.
//!
//! An *action* is a thing the program does, named independently of whatever
//! key happens to run it. That separation is what makes the keys
//! configurable at all: the config file says `"<Leader>os" = "obs.stream"`,
//! and nothing in the program needs to know which key that was.
//!
//! Names are `group.verb`, and the group is the same word used in the
//! which-key popup — so `obs.stream` appears under the OBS group and reads as
//! "OBS → stream" wherever it is shown.

use std::fmt;

/// Where a binding applies.
///
/// The same key means different things in different places: `j` moves down a
/// scene list on the OBS tab and scrolls chat on the Chat tab. Rather than one
/// flat map with the ambiguity that implies, each binding belongs to a
/// context, and a lookup tries the active context before falling back to
/// [`Context::Global`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Context {
    /// Everywhere.
    Global,
    /// The Stream Info tab.
    StreamInfo,
    /// The chat panes, on either the Chat or the Combined tab.
    Chat,
    /// The OBS tab.
    Obs,
}

impl Context {
    pub const ALL: [Context; 4] = [
        Context::Global,
        Context::StreamInfo,
        Context::Chat,
        Context::Obs,
    ];

    /// The name used in the config file: `[keys]`, `[keys.chat]`, and so on.
    pub fn name(self) -> &'static str {
        match self {
            Context::Global => "global",
            Context::StreamInfo => "stream_info",
            Context::Chat => "chat",
            Context::Obs => "obs",
        }
    }
}

/// Something the program can be asked to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Action {
    // --- getting around ---------------------------------------------------
    TabStreamInfo,
    TabChat,
    TabCombined,
    TabObs,
    TabNext,
    TabPrevious,
    /// Swap which half of the Combined tab has the keyboard.
    CombinedSwapFocus,

    // --- the program itself -----------------------------------------------
    Quit,
    CommandPalette,
    MessageHistory,
    WhichKey,
    ThemePicker,
    CycleAnimations,
    ToggleTelemetry,

    // --- streaming --------------------------------------------------------
    GoLive,
    EditStreamInfo,
    RefreshStats,
    CopyTwitchKey,
    CopyYouTubeKey,
    OpenWatchPage,

    // --- chat -------------------------------------------------------------
    ChatCompose,
    ChatSearch,
    ChatSearchNext,
    ChatSearchPrevious,
    ChatJoin,
    ChatReconnect,
    ChatNextChat,
    ChatPreviousChat,
    ChatNextAccount,
    ChatPreviousAccount,
    ChatScrollUp,
    ChatScrollDown,
    ChatPageUp,
    ChatPageDown,
    ChatToTop,
    ChatToBottom,
    ChatFocusNextPane,
    ChatFocusPreviousPane,
    ChatWiden,
    ChatNarrow,
    ChatResetPanes,
    ChatToggleActivity,
    ChatToggleInspect,
    ChatEmojiPicker,
    ChatReply,
    ChatClearFilters,

    // --- OBS --------------------------------------------------------------
    ObsUp,
    ObsDown,
    ObsSwapPane,
    ObsActivate,
    ObsToggleMute,
    ObsMuteAll,
    ObsVolumeUp,
    ObsVolumeDown,
    ObsToggleStream,
    ObsToggleRecord,
    ObsPauseRecording,
    ObsNextProfile,
    ObsNextCollection,
    ObsReconnect,
    ObsRefresh,
}

impl Action {
    /// Every action, so the config parser can name the valid ones and the
    /// which-key popup can list them.
    pub const ALL: &'static [Action] = &[
        Action::TabStreamInfo,
        Action::TabChat,
        Action::TabCombined,
        Action::TabObs,
        Action::TabNext,
        Action::TabPrevious,
        Action::CombinedSwapFocus,
        Action::Quit,
        Action::CommandPalette,
        Action::MessageHistory,
        Action::WhichKey,
        Action::ThemePicker,
        Action::CycleAnimations,
        Action::ToggleTelemetry,
        Action::GoLive,
        Action::EditStreamInfo,
        Action::RefreshStats,
        Action::CopyTwitchKey,
        Action::CopyYouTubeKey,
        Action::OpenWatchPage,
        Action::ChatCompose,
        Action::ChatSearch,
        Action::ChatSearchNext,
        Action::ChatSearchPrevious,
        Action::ChatJoin,
        Action::ChatReconnect,
        Action::ChatNextChat,
        Action::ChatPreviousChat,
        Action::ChatNextAccount,
        Action::ChatPreviousAccount,
        Action::ChatScrollUp,
        Action::ChatScrollDown,
        Action::ChatPageUp,
        Action::ChatPageDown,
        Action::ChatToTop,
        Action::ChatToBottom,
        Action::ChatFocusNextPane,
        Action::ChatFocusPreviousPane,
        Action::ChatWiden,
        Action::ChatNarrow,
        Action::ChatResetPanes,
        Action::ChatToggleActivity,
        Action::ChatToggleInspect,
        Action::ChatEmojiPicker,
        Action::ChatReply,
        Action::ChatClearFilters,
        Action::ObsUp,
        Action::ObsDown,
        Action::ObsSwapPane,
        Action::ObsActivate,
        Action::ObsToggleMute,
        Action::ObsMuteAll,
        Action::ObsVolumeUp,
        Action::ObsVolumeDown,
        Action::ObsToggleStream,
        Action::ObsToggleRecord,
        Action::ObsPauseRecording,
        Action::ObsNextProfile,
        Action::ObsNextCollection,
        Action::ObsReconnect,
        Action::ObsRefresh,
    ];

    /// The name used in the config file.
    pub fn name(self) -> &'static str {
        match self {
            Action::TabStreamInfo => "tab.stream_info",
            Action::TabChat => "tab.chat",
            Action::TabCombined => "tab.combined",
            Action::TabObs => "tab.obs",
            Action::TabNext => "tab.next",
            Action::TabPrevious => "tab.previous",
            Action::CombinedSwapFocus => "tab.swap_focus",

            Action::Quit => "app.quit",
            Action::CommandPalette => "app.command_palette",
            Action::MessageHistory => "app.messages",
            Action::WhichKey => "app.which_key",
            Action::ThemePicker => "ui.theme",
            Action::CycleAnimations => "ui.animations",
            Action::ToggleTelemetry => "ui.telemetry",

            Action::GoLive => "stream.go_live",
            Action::EditStreamInfo => "stream.edit",
            Action::RefreshStats => "stream.refresh",
            Action::CopyTwitchKey => "stream.copy_twitch_key",
            Action::CopyYouTubeKey => "stream.copy_youtube_key",
            Action::OpenWatchPage => "stream.open_watch_page",

            Action::ChatCompose => "chat.compose",
            Action::ChatSearch => "chat.search",
            Action::ChatSearchNext => "chat.search_next",
            Action::ChatSearchPrevious => "chat.search_previous",
            Action::ChatJoin => "chat.join",
            Action::ChatReconnect => "chat.reconnect",
            Action::ChatNextChat => "chat.next",
            Action::ChatPreviousChat => "chat.previous",
            Action::ChatNextAccount => "chat.next_account",
            Action::ChatPreviousAccount => "chat.previous_account",
            Action::ChatScrollUp => "chat.scroll_up",
            Action::ChatScrollDown => "chat.scroll_down",
            Action::ChatPageUp => "chat.page_up",
            Action::ChatPageDown => "chat.page_down",
            Action::ChatToTop => "chat.oldest",
            Action::ChatToBottom => "chat.newest",
            Action::ChatFocusNextPane => "chat.next_pane",
            Action::ChatFocusPreviousPane => "chat.previous_pane",
            Action::ChatWiden => "chat.widen",
            Action::ChatNarrow => "chat.narrow",
            Action::ChatResetPanes => "chat.reset_panes",
            Action::ChatToggleActivity => "chat.activity",
            Action::ChatToggleInspect => "chat.inspect",
            Action::ChatEmojiPicker => "chat.emoji",
            Action::ChatReply => "chat.reply",
            Action::ChatClearFilters => "chat.clear_filters",

            Action::ObsUp => "obs.up",
            Action::ObsDown => "obs.down",
            Action::ObsSwapPane => "obs.swap_pane",
            Action::ObsActivate => "obs.activate",
            Action::ObsToggleMute => "obs.mute",
            Action::ObsMuteAll => "obs.mute_all",
            Action::ObsVolumeUp => "obs.volume_up",
            Action::ObsVolumeDown => "obs.volume_down",
            Action::ObsToggleStream => "obs.stream",
            Action::ObsToggleRecord => "obs.record",
            Action::ObsPauseRecording => "obs.pause_recording",
            Action::ObsNextProfile => "obs.next_profile",
            Action::ObsNextCollection => "obs.next_collection",
            Action::ObsReconnect => "obs.reconnect",
            Action::ObsRefresh => "obs.refresh",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        let name = name.trim();
        Action::ALL
            .iter()
            .copied()
            .find(|action| action.name() == name)
    }

    /// A short description, for the which-key popup and the help.
    pub fn describe(self) -> &'static str {
        match self {
            Action::TabStreamInfo => "Stream Info tab",
            Action::TabChat => "Chat tab",
            Action::TabCombined => "Combined tab",
            Action::TabObs => "OBS tab",
            Action::TabNext => "Next tab",
            Action::TabPrevious => "Previous tab",
            Action::CombinedSwapFocus => "Swap combined halves",

            Action::Quit => "Quit",
            Action::CommandPalette => "Command palette",
            Action::MessageHistory => "Message history",
            Action::WhichKey => "Show every binding",
            Action::ThemePicker => "Choose a theme",
            Action::CycleAnimations => "Cycle animations",
            Action::ToggleTelemetry => "Toggle telemetry",

            Action::GoLive => "Go live",
            Action::EditStreamInfo => "Edit stream info",
            Action::RefreshStats => "Refresh statistics",
            Action::CopyTwitchKey => "Copy Twitch stream key",
            Action::CopyYouTubeKey => "Copy YouTube stream key",
            Action::OpenWatchPage => "Open watch page",

            Action::ChatCompose => "Write a message",
            Action::ChatSearch => "Search chat",
            Action::ChatSearchNext => "Next match",
            Action::ChatSearchPrevious => "Previous match",
            Action::ChatJoin => "Join a channel",
            Action::ChatReconnect => "Reconnect chat",
            Action::ChatNextChat => "Next chat",
            Action::ChatPreviousChat => "Previous chat",
            Action::ChatNextAccount => "Next account",
            Action::ChatPreviousAccount => "Previous account",
            Action::ChatScrollUp => "Scroll back",
            Action::ChatScrollDown => "Scroll forward",
            Action::ChatPageUp => "Page back",
            Action::ChatPageDown => "Page forward",
            Action::ChatToTop => "Oldest message",
            Action::ChatToBottom => "Newest message",
            Action::ChatFocusNextPane => "Next pane",
            Action::ChatFocusPreviousPane => "Previous pane",
            Action::ChatWiden => "Widen left pane",
            Action::ChatNarrow => "Narrow left pane",
            Action::ChatResetPanes => "Reset pane sizes",
            Action::ChatToggleActivity => "Toggle activity view",
            Action::ChatToggleInspect => "Toggle inspect panel",
            Action::ChatEmojiPicker => "Emoji picker",
            Action::ChatReply => "Reply to selection",
            Action::ChatClearFilters => "Clear message filters",

            Action::ObsUp => "Move up",
            Action::ObsDown => "Move down",
            Action::ObsSwapPane => "Swap scenes/audio",
            Action::ObsActivate => "Switch scene or toggle mute",
            Action::ObsToggleMute => "Toggle mute",
            Action::ObsMuteAll => "Mute everything",
            Action::ObsVolumeUp => "Volume up",
            Action::ObsVolumeDown => "Volume down",
            Action::ObsToggleStream => "Start/stop streaming",
            Action::ObsToggleRecord => "Start/stop recording",
            Action::ObsPauseRecording => "Pause/resume recording",
            Action::ObsNextProfile => "Next profile",
            Action::ObsNextCollection => "Next scene collection",
            Action::ObsReconnect => "Reconnect to OBS",
            Action::ObsRefresh => "Refresh from OBS",
        }
    }

    /// The group this belongs to, which is the part of the name before the
    /// dot. Used to head the sections of the which-key popup.
    pub fn group(self) -> &'static str {
        self.name()
            .split_once('.')
            .map(|(group, _)| group)
            .unwrap_or("other")
    }
}

impl fmt::Display for Action {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    /// A name is what the config file says, so two actions sharing one would
    /// make a binding ambiguous.
    #[test]
    fn every_action_has_its_own_name() {
        let names: BTreeSet<&str> = Action::ALL.iter().map(|action| action.name()).collect();
        assert_eq!(names.len(), Action::ALL.len(), "two actions share a name");
    }

    /// `ALL` is written by hand, so it is worth checking nothing was left out
    /// of it — an action missing from the list could never be bound.
    #[test]
    fn every_action_is_listed_and_parseable() {
        for action in Action::ALL {
            assert_eq!(
                Action::parse(action.name()),
                Some(*action),
                "{} does not round-trip",
                action.name()
            );
            assert!(!action.describe().is_empty());
            assert!(!action.group().is_empty());
        }
    }

    #[test]
    fn an_unknown_action_name_is_rejected() {
        assert_eq!(Action::parse("obs.explode"), None);
        assert_eq!(Action::parse(""), None);
    }

    #[test]
    fn names_are_grouped_by_their_prefix() {
        assert_eq!(Action::ObsToggleMute.group(), "obs");
        assert_eq!(Action::ChatCompose.group(), "chat");
        assert_eq!(Action::GoLive.group(), "stream");
    }

    /// The names appear in the config file as table headings, so two
    /// contexts sharing one would make `[keys.chat]` ambiguous.
    #[test]
    fn every_context_has_its_own_name() {
        let names: BTreeSet<&str> = Context::ALL.iter().map(|context| context.name()).collect();
        assert_eq!(names.len(), Context::ALL.len());
    }
}
