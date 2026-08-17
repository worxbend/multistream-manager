//! Deciding which chat events deserve a desktop notification.
//!
//! The delivery half lives in [`crate::notify`]; this module only answers the
//! question "is this worth interrupting somebody for, and what should the
//! pop-up say?".
//!
//! The original selection was ported from `yc`
//! (`internal/app/notify.go`, `notificationFromMessage`) and covered YouTube's
//! money and membership events only. It has since been widened to the events a
//! Twitch stream actually turns on — raids first among them — because those
//! arrive as chat traffic too, and a raid you notice ten minutes late is a
//! raid you did not get to greet.
//!
//! What is *not* notified matters as much as what is. Ordinary messages, mode
//! changes and moderation never produce a pop-up: a notification for every
//! line is a notification for none. Neither does backlog (the priming page a
//! chat delivers when it opens is history, not news), our own sends, or a
//! message that has since been deleted.

use crate::chat::{ChatMessage, MembershipKind, MessageKind, PlatformMeta};
use crate::config::NotificationsConfig;
use crate::notify::{Notification, Urgency};

/// Decides whether a message is worth a desktop notification, and builds it
/// when it is.
///
/// `config` is consulted per event class, so somebody who wants raids and
/// nothing else can have exactly that.
pub fn high_signal(msg: &ChatMessage, config: &NotificationsConfig) -> Option<Notification> {
    if msg.historical || msg.local_echo || msg.deleted {
        return None;
    }

    // Twitch's stream events (raid, sub, gift, cheer) arrive as chat traffic,
    // so they are recognised here rather than anywhere more official-looking.
    if let Some(PlatformMeta::Twitch(meta)) = &msg.meta {
        if let Some(event) = twitch_event(&meta.system_event) {
            if !event.wanted(config) {
                return None;
            }
            return Some(Notification::new(
                event.title(),
                body_for(msg),
                event.urgency(),
            ));
        }
        // A cheer is an ordinary message with money attached — the bits tag,
        // not a system event — so it is matched separately.
        if meta.bits > 0 && msg.kind == MessageKind::Chat {
            if !config.cheers {
                return None;
            }
            return Some(Notification::new(
                format!("Cheer · {} bits", meta.bits),
                body_for(msg),
                Urgency::Normal,
            ));
        }
    }

    let (title, urgency) = match msg.kind {
        MessageKind::Paid if config.paid => (paid_title(msg), Urgency::Normal),
        MessageKind::Membership if config.memberships => (membership_title(msg), Urgency::Normal),
        _ => return None,
    };
    Some(Notification::new(title, body_for(msg), urgency))
}

/// The Twitch system events worth a pop-up.
///
/// Twitch labels every USERNOTICE with a `msg-id` — the short event name that
/// rides in [`crate::chat::TwitchMeta::system_event`]. Only the ones below are
/// notified; the rest (`ritual`, `unraid`, mod announcements, and anything
/// Twitch adds later) stay in the chat pane where they belong.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TwitchEvent {
    /// Another streamer just sent their whole audience to your channel. The
    /// one event with a deadline attached: you have seconds to say hello.
    Raid,
    /// A new subscription, a renewal, or a Prime/tier upgrade.
    Subscription,
    /// Somebody bought subscriptions for other people.
    GiftedSubscription,
    /// The viewer crossed a bits-badge threshold — a cheering milestone.
    BitsMilestone,
}

impl TwitchEvent {
    fn wanted(self, config: &NotificationsConfig) -> bool {
        match self {
            TwitchEvent::Raid => config.raids,
            TwitchEvent::Subscription | TwitchEvent::GiftedSubscription => config.subscriptions,
            TwitchEvent::BitsMilestone => config.cheers,
        }
    }

    fn title(self) -> &'static str {
        match self {
            TwitchEvent::Raid => "Raid",
            TwitchEvent::Subscription => "Subscription",
            TwitchEvent::GiftedSubscription => "Gifted subscriptions",
            TwitchEvent::BitsMilestone => "Bits milestone",
        }
    }

    /// A raid is critical, meaning most desktops will show it even with
    /// do-not-disturb on. That is the right call for the one event you cannot
    /// respond to late; everything else is ordinary news.
    fn urgency(self) -> Urgency {
        match self {
            TwitchEvent::Raid => Urgency::Critical,
            _ => Urgency::Normal,
        }
    }
}

/// Maps a Twitch `msg-id` to an event class. Case-insensitive because the tag
/// is documented lowercase but nothing on the wire enforces it.
fn twitch_event(system_event: &str) -> Option<TwitchEvent> {
    match system_event.trim().to_ascii_lowercase().as_str() {
        "raid" => Some(TwitchEvent::Raid),
        // `sub` is new, `resub` a renewal, and the `*paidupgrade` family is
        // somebody moving from a gift or from Prime to a paid tier.
        "sub" | "resub" | "primepaidupgrade" | "giftpaidupgrade" | "anongiftpaidupgrade" => {
            Some(TwitchEvent::Subscription)
        }
        // `submysterygift` is the "N subs to the community" announcement;
        // `subgift`/`anonsubgift` are the individual recipients that follow.
        "subgift"
        | "anonsubgift"
        | "submysterygift"
        | "standardpayforward"
        | "communitypayforward" => Some(TwitchEvent::GiftedSubscription),
        "bitsbadgetier" => Some(TwitchEvent::BitsMilestone),
        _ => None,
    }
}

/// Title for a paid event: the event name plus the platform's pre-localized
/// amount display when one exists (yc: notificationTitleForMessage).
fn paid_title(msg: &ChatMessage) -> String {
    let (name, display) = match &msg.meta {
        Some(PlatformMeta::YouTube(meta)) => {
            // The wire type distinguishes a Super Sticker from a Super Chat;
            // both are `Paid` in the normalized model.
            let name = if meta.raw_type.to_ascii_lowercase().contains("sticker") {
                "Super Sticker"
            } else {
                "Super Chat"
            };
            let display = meta.paid.as_ref().map(|p| p.display.as_str()).unwrap_or("");
            (name, display)
        }
        _ => ("Super Chat", ""),
    };
    if display.is_empty() {
        name.to_string()
    } else {
        format!("{name} {display}")
    }
}

/// Title for a membership event (yc: notificationTitleForMessage).
fn membership_title(msg: &ChatMessage) -> String {
    let kind = match &msg.meta {
        Some(PlatformMeta::YouTube(meta)) => meta.membership.as_ref().map(|m| m.kind),
        _ => None,
    };
    match kind {
        Some(MembershipKind::New) => "New member",
        // The reference has no distinct upgrade string; the closest label.
        Some(MembershipKind::Upgrade) => "New member",
        Some(MembershipKind::Milestone) => "Member milestone",
        Some(MembershipKind::Gifting) => "Gifted memberships",
        Some(MembershipKind::GiftReceived) => "Gift membership received",
        // A Membership row with no details (e.g. a Twitch sub normalized
        // without meta) still deserves its generic title.
        None => "New member",
    }
    .to_string()
}

/// Body: author, membership context when present, then the message text
/// (yc: notificationBodyForMessage).
///
/// For a Twitch system event the text is already Twitch's own system message
/// ("430 raiders from iamelisabete have joined!"), which reads better than
/// anything this program could assemble, so it is passed through unchanged.
fn body_for(msg: &ChatMessage) -> String {
    let mut parts: Vec<String> = Vec::new();
    let author = msg.author.display_name.trim();
    if !author.is_empty() {
        parts.push(author.to_string());
    }
    if let Some(PlatformMeta::YouTube(meta)) = &msg.meta {
        if let Some(membership) = &meta.membership {
            let level = membership.level.trim();
            if !level.is_empty() {
                parts.push(level.to_string());
            }
            // Zero means "the platform did not say", which must render as
            // nothing rather than "0 months".
            if membership.months > 0 {
                parts.push(format!("{} months", membership.months));
            }
            if membership.gift_count > 0 {
                parts.push(format!("{} gifts", membership.gift_count));
            }
        }
    }
    let head = parts.join(" · ");
    let text = msg.text.trim();
    match (head.is_empty(), text.is_empty()) {
        (_, true) => head,
        (true, false) => text.to_string(),
        (false, false) => format!("{head}: {text}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::{ChatAuthor, MembershipDetails, PaidAmount, TwitchMeta, YouTubeMeta};

    fn config() -> NotificationsConfig {
        NotificationsConfig::default()
    }

    fn message(kind: MessageKind) -> ChatMessage {
        ChatMessage {
            id: "m1".into(),
            timestamp: None,
            author: ChatAuthor {
                display_name: "Alice".into(),
                ..Default::default()
            },
            text: "hello".into(),
            kind,
            deleted: false,
            historical: false,
            local_echo: false,
            meta: None,
        }
    }

    fn twitch_system(event: &str, text: &str) -> ChatMessage {
        let mut msg = message(MessageKind::Notice);
        msg.text = text.into();
        msg.meta = Some(PlatformMeta::Twitch(TwitchMeta {
            system_event: event.into(),
            ..TwitchMeta::default()
        }));
        msg
    }

    fn paid_message() -> ChatMessage {
        let mut msg = message(MessageKind::Paid);
        msg.meta = Some(PlatformMeta::YouTube(YouTubeMeta {
            raw_type: "superChatEvent".into(),
            paid: Some(PaidAmount {
                micros: 5_000_000,
                currency: "USD".into(),
                display: "$5.00".into(),
                tier: 2,
            }),
            membership: None,
        }));
        msg
    }

    #[test]
    fn a_raid_notifies_critically_with_twitchs_own_wording() {
        let msg = twitch_system("raid", "430 raiders from iamelisabete have joined!");
        let notification = high_signal(&msg, &config()).expect("a raid must notify");
        assert_eq!(notification.title, "Raid");
        assert_eq!(
            notification.body,
            "Alice: 430 raiders from iamelisabete have joined!"
        );
        assert_eq!(notification.urgency, Urgency::Critical);
    }

    #[test]
    fn subscriptions_and_gifts_notify_under_their_own_titles() {
        for event in ["sub", "resub", "primepaidupgrade"] {
            let notification = high_signal(&twitch_system(event, "subscribed"), &config())
                .unwrap_or_else(|| panic!("{event} must notify"));
            assert_eq!(notification.title, "Subscription");
            assert_eq!(notification.urgency, Urgency::Normal);
        }
        for event in ["subgift", "submysterygift", "anonsubgift"] {
            let notification = high_signal(&twitch_system(event, "gifted"), &config())
                .unwrap_or_else(|| panic!("{event} must notify"));
            assert_eq!(notification.title, "Gifted subscriptions");
        }
    }

    #[test]
    fn a_msg_id_in_unexpected_case_is_still_recognised() {
        // Documented lowercase, but nothing on the wire enforces it, and
        // missing a raid over letter case would be an expensive bug.
        assert!(high_signal(&twitch_system("RAID", "joined"), &config()).is_some());
    }

    #[test]
    fn twitch_notices_that_are_not_stream_events_stay_quiet() {
        for event in ["ritual", "unraid", "announcement", ""] {
            assert!(
                high_signal(&twitch_system(event, "something"), &config()).is_none(),
                "{event} must not notify"
            );
        }
    }

    #[test]
    fn a_cheer_notifies_with_its_bit_count() {
        let mut msg = message(MessageKind::Chat);
        msg.text = "have some bits".into();
        msg.meta = Some(PlatformMeta::Twitch(TwitchMeta {
            bits: 500,
            ..TwitchMeta::default()
        }));
        let notification = high_signal(&msg, &config()).expect("a cheer must notify");
        assert_eq!(notification.title, "Cheer · 500 bits");
        assert_eq!(notification.body, "Alice: have some bits");
    }

    #[test]
    fn an_ordinary_twitch_message_never_notifies() {
        let mut msg = message(MessageKind::Chat);
        msg.meta = Some(PlatformMeta::Twitch(TwitchMeta::default()));
        assert!(high_signal(&msg, &config()).is_none());
    }

    #[test]
    fn each_event_class_can_be_switched_off_on_its_own() {
        let raids_only = NotificationsConfig {
            subscriptions: false,
            cheers: false,
            paid: false,
            memberships: false,
            ..NotificationsConfig::default()
        };
        assert!(high_signal(&twitch_system("raid", "joined"), &raids_only).is_some());
        assert!(high_signal(&twitch_system("sub", "subscribed"), &raids_only).is_none());
        assert!(high_signal(&paid_message(), &raids_only).is_none());

        let no_raids = NotificationsConfig {
            raids: false,
            ..NotificationsConfig::default()
        };
        assert!(high_signal(&twitch_system("raid", "joined"), &no_raids).is_none());
    }

    #[test]
    fn high_signal_notifies_for_paid() {
        let notification = high_signal(&paid_message(), &config()).expect("paid must notify");
        assert_eq!(notification.title, "Super Chat $5.00");
        assert_eq!(notification.body, "Alice: hello");
    }

    #[test]
    fn high_signal_distinguishes_super_sticker() {
        let mut msg = paid_message();
        if let Some(PlatformMeta::YouTube(meta)) = &mut msg.meta {
            meta.raw_type = "superStickerEvent".into();
        }
        let notification = high_signal(&msg, &config()).expect("sticker must notify");
        assert_eq!(notification.title, "Super Sticker $5.00");
    }

    #[test]
    fn high_signal_notifies_for_membership_with_context() {
        let mut msg = message(MessageKind::Membership);
        msg.text = String::new();
        msg.meta = Some(PlatformMeta::YouTube(YouTubeMeta {
            raw_type: "memberMilestoneChatEvent".into(),
            paid: None,
            membership: Some(MembershipDetails {
                kind: MembershipKind::Milestone,
                level: "Gold".into(),
                months: 12,
                gift_count: 0,
            }),
        }));
        let notification = high_signal(&msg, &config()).expect("membership must notify");
        assert_eq!(notification.title, "Member milestone");
        assert_eq!(notification.body, "Alice · Gold · 12 months");
    }

    #[test]
    fn high_signal_ignores_plain_chat() {
        for kind in [
            MessageKind::Chat,
            MessageKind::Action,
            MessageKind::Notice,
            MessageKind::Unknown,
        ] {
            assert!(high_signal(&message(kind), &config()).is_none());
        }
    }

    #[test]
    fn high_signal_ignores_historical_local_echo_and_deleted() {
        // The same three exclusions apply to a raid, which is the case that
        // would be most annoying to get wrong: reconnecting to a chat must not
        // re-announce a raid from an hour ago.
        for make in [paid_message, || twitch_system("raid", "joined")] {
            let mut historical = make();
            historical.historical = true;
            assert!(high_signal(&historical, &config()).is_none());

            let mut echo = make();
            echo.local_echo = true;
            assert!(high_signal(&echo, &config()).is_none());

            let mut deleted = make();
            deleted.deleted = true;
            assert!(high_signal(&deleted, &config()).is_none());
        }
    }
}
