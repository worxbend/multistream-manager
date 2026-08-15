//! Built-in searchable emoji catalog for the composer's emoji picker.
//!
//! Ported behavior from `yc` (repo ../yc, commit
//! 9e67efd10c0790ec22df2c944bcee6be1bc37cf8, files
//! internal/emoji/catalog.go and internal/app/update.go —
//! `emojiPickerCandidates`). The point of the port survives intact: the
//! picker is compiled in and needs **no credentials and no network** —
//! YouTube's API supplies no emote imagery and Twitch emotes need a Helix
//! index, but Unicode emoji work everywhere, immediately.
//!
//! Documented deviation: yc embeds a larger curated Unicode table (a couple
//! hundred entries) plus a full cluster validator; this catalog is a tighter
//! curation of ~120 of the most commonly sent emoji, each with a lowercase
//! name and a few search keywords. The cluster validation lives in
//! `chat/render.rs` (`is_emoji_cluster`), not here, because rendering — not
//! the picker — is what must classify arbitrary incoming text.

/// One emoji in the built-in picker set (yc's `emoji.Entry`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EmojiEntry {
    /// The emoji itself — one whole grapheme cluster.
    pub emoji: &'static str,
    /// Lowercase human name, matched by search and shown in the picker row.
    pub name: &'static str,
    /// Extra lowercase search terms.
    pub keywords: &'static [&'static str],
}

/// The full built-in catalog, in curated display order: reactions first
/// (what a live chat sends most — yc's ordering rule), then gestures,
/// hearts, celebration, animals, food, objects, and symbols.
pub fn catalog() -> &'static [EmojiEntry] {
    CATALOG
}

/// Search the catalog: an empty (or blank) query returns the catalog head;
/// otherwise a case-insensitive substring match over name and keywords, with
/// name-*prefix* matches ranked before the rest (same shape as the roster's
/// prefix-beats-substring rule, so both pickers feel identical). Catalog
/// order breaks ties, keeping common emoji near the top. At most `max`
/// results.
pub fn search(query: &str, max: usize) -> Vec<&'static EmojiEntry> {
    if max == 0 {
        return Vec::new();
    }
    let needle = query.trim().to_lowercase();
    if needle.is_empty() {
        return CATALOG.iter().take(max).collect();
    }
    let mut prefix_hits: Vec<&'static EmojiEntry> = Vec::new();
    let mut substring_hits: Vec<&'static EmojiEntry> = Vec::new();
    for entry in CATALOG {
        if entry.name.starts_with(&needle) {
            prefix_hits.push(entry);
        } else if entry.name.contains(&needle) || entry.keywords.iter().any(|k| k.contains(&needle))
        {
            substring_hits.push(entry);
        }
    }
    prefix_hits.extend(substring_hits);
    prefix_hits.truncate(max);
    prefix_hits
}

macro_rules! e {
    ($emoji:literal, $name:literal, [$($kw:literal),*]) => {
        EmojiEntry { emoji: $emoji, name: $name, keywords: &[$($kw),*] }
    };
}

static CATALOG: &[EmojiEntry] = &[
    // --- smileys & reactions (the top of the list is what chat sends most) ---
    e!("😀", "grinning face", ["smile", "happy", "grin"]),
    e!("😃", "grinning face with big eyes", ["smile", "happy"]),
    e!("😄", "grinning face with smiling eyes", ["smile", "happy"]),
    e!("😁", "beaming face", ["grin", "smile"]),
    e!("😆", "grinning squinting face", ["laugh", "haha"]),
    e!(
        "😅",
        "grinning face with sweat",
        ["laugh", "relief", "phew"]
    ),
    e!(
        "🤣",
        "rolling on the floor laughing",
        ["rofl", "laugh", "lol"]
    ),
    e!("😂", "face with tears of joy", ["lol", "laugh", "cry"]),
    e!("🙂", "slightly smiling face", ["smile", "fine"]),
    e!("🙃", "upside-down face", ["silly", "irony"]),
    e!("😉", "winking face", ["wink", "flirt"]),
    e!("😊", "smiling face with smiling eyes", ["blush", "happy"]),
    e!("😇", "smiling face with halo", ["angel", "innocent"]),
    e!("🥰", "smiling face with hearts", ["love", "adore"]),
    e!("😍", "smiling face with heart-eyes", ["love", "crush"]),
    e!("😘", "face blowing a kiss", ["kiss", "love"]),
    e!("😋", "face savoring food", ["yum", "tasty"]),
    e!("😜", "winking face with tongue", ["silly", "joke"]),
    e!("🤪", "zany face", ["goofy", "wild"]),
    e!("🤨", "face with raised eyebrow", ["suspicious", "doubt"]),
    e!("🧐", "face with monocle", ["inspect", "hmm"]),
    e!("🤓", "nerd face", ["glasses", "geek"]),
    e!("😎", "smiling face with sunglasses", ["cool", "shades"]),
    e!("🥳", "partying face", ["party", "celebrate"]),
    e!("😏", "smirking face", ["smirk", "smug"]),
    e!("😒", "unamused face", ["meh", "unimpressed"]),
    e!("😔", "pensive face", ["sad", "thoughtful"]),
    e!("😴", "sleeping face", ["sleep", "zzz", "bored"]),
    e!("🤤", "drooling face", ["drool", "hungry"]),
    e!("🥵", "hot face", ["hot", "heat", "sweat"]),
    e!("🥶", "cold face", ["cold", "freezing"]),
    e!("🤯", "exploding head", ["mind blown", "shock", "wow"]),
    e!("🥺", "pleading face", ["please", "beg", "puppy eyes"]),
    e!("😢", "crying face", ["sad", "tear"]),
    e!("😭", "loudly crying face", ["sob", "sad", "cry"]),
    e!("😤", "face with steam from nose", ["angry", "determined"]),
    e!("😠", "angry face", ["mad", "angry"]),
    e!("😡", "enraged face", ["rage", "angry"]),
    e!("🤬", "face with symbols on mouth", ["swearing", "censored"]),
    e!("😱", "face screaming in fear", ["scream", "shock"]),
    e!("😳", "flushed face", ["blush", "surprised"]),
    e!("😬", "grimacing face", ["awkward", "yikes"]),
    e!("🙄", "face with rolling eyes", ["eyeroll", "annoyed"]),
    e!("🤔", "thinking face", ["think", "hmm", "consider"]),
    e!("🤫", "shushing face", ["quiet", "shh", "spoiler"]),
    e!("😶", "face without mouth", ["speechless", "silent"]),
    e!("😐", "neutral face", ["meh", "blank"]),
    e!("💀", "skull", ["dead", "lol", "skeleton"]),
    e!("🤡", "clown face", ["clown", "joke"]),
    e!("👻", "ghost", ["boo", "spooky"]),
    e!("🤖", "robot", ["bot", "machine"]),
    e!("😈", "smiling face with horns", ["devil", "evil"]),
    e!("🥲", "smiling face with tear", ["bittersweet", "grateful"]),
    e!("😮", "face with open mouth", ["wow", "surprised", "pog"]),
    // --- gestures & people ---
    e!("👋", "waving hand", ["wave", "hello", "bye"]),
    e!("👍", "thumbs up", ["like", "yes", "approve"]),
    e!("👎", "thumbs down", ["dislike", "no"]),
    e!("👏", "clapping hands", ["clap", "applause", "bravo"]),
    e!("🙌", "raising hands", ["hooray", "praise"]),
    e!("🙏", "folded hands", ["please", "thanks", "pray"]),
    e!("🤝", "handshake", ["deal", "agreement"]),
    e!("✌️", "victory hand", ["peace", "two"]),
    e!("🤞", "crossed fingers", ["luck", "hope"]),
    e!("🤟", "love-you gesture", ["love", "rock"]),
    e!("🤘", "sign of the horns", ["rock", "metal"]),
    e!("👌", "ok hand", ["ok", "perfect"]),
    e!("👉", "pointing right", ["point", "this"]),
    e!("👀", "eyes", ["look", "watching", "sus"]),
    e!("💪", "flexed biceps", ["strong", "muscle", "gym"]),
    e!("🧠", "brain", ["smart", "think"]),
    e!("🫡", "saluting face", ["salute", "respect", "o7"]),
    e!("🤦", "person facepalming", ["facepalm", "doh"]),
    e!("🤷", "person shrugging", ["shrug", "dunno"]),
    // --- hearts ---
    e!("❤️", "red heart", ["love", "heart"]),
    e!("🧡", "orange heart", ["love", "heart"]),
    e!("💛", "yellow heart", ["love", "heart"]),
    e!("💚", "green heart", ["love", "heart"]),
    e!("💙", "blue heart", ["love", "heart"]),
    e!("💜", "purple heart", ["love", "heart", "twitch"]),
    e!("🖤", "black heart", ["love", "dark"]),
    e!("🤍", "white heart", ["love", "pure"]),
    e!("💔", "broken heart", ["heartbreak", "sad"]),
    e!("💖", "sparkling heart", ["love", "sparkle"]),
    e!("💕", "two hearts", ["love", "affection"]),
    e!("💯", "hundred points", ["100", "perfect", "agree"]),
    // --- celebration ---
    e!("🎉", "party popper", ["party", "celebrate", "congrats"]),
    e!("🎊", "confetti ball", ["party", "confetti"]),
    e!("🎂", "birthday cake", ["birthday", "cake"]),
    e!("🎁", "wrapped gift", ["present", "gift"]),
    e!("🏆", "trophy", ["win", "champion", "first"]),
    e!("🥇", "gold medal", ["first", "winner"]),
    e!("✨", "sparkles", ["shiny", "magic", "new"]),
    e!("🔥", "fire", ["lit", "hot", "flame"]),
    e!("⚡", "high voltage", ["lightning", "fast", "zap"]),
    e!("💥", "collision", ["boom", "explosion"]),
    e!("🚀", "rocket", ["launch", "fast", "moon"]),
    e!("🎮", "video game", ["gaming", "controller", "play"]),
    e!("🎲", "game die", ["dice", "random", "luck"]),
    e!("🎯", "bullseye", ["target", "accurate"]),
    e!("🎵", "musical note", ["music", "song"]),
    // --- animals ---
    e!("🐶", "dog face", ["dog", "puppy", "pet"]),
    e!("🐱", "cat face", ["cat", "kitten", "pet"]),
    e!("🦊", "fox", ["fox", "sly"]),
    e!("🐻", "bear", ["bear", "grizzly"]),
    e!("🐼", "panda", ["panda", "cute"]),
    e!("🐸", "frog", ["frog", "toad"]),
    e!("🐵", "monkey face", ["monkey", "ape"]),
    e!("🦆", "duck", ["duck", "quack"]),
    e!("🐟", "fish", ["fish", "sea"]),
    e!("🦀", "crab", ["crab", "rust"]),
    e!("🐢", "turtle", ["turtle", "slow"]),
    e!("🦄", "unicorn", ["unicorn", "magic"]),
    // --- food & drink ---
    e!("🍕", "pizza", ["pizza", "food", "slice"]),
    e!("🍔", "hamburger", ["burger", "food"]),
    e!("🍟", "french fries", ["fries", "food"]),
    e!("🌮", "taco", ["taco", "food"]),
    e!("🍿", "popcorn", ["popcorn", "movie", "drama"]),
    e!("🍩", "doughnut", ["donut", "sweet"]),
    e!("🍪", "cookie", ["cookie", "sweet"]),
    e!("☕", "hot beverage", ["coffee", "tea", "mug"]),
    e!("🍺", "beer mug", ["beer", "cheers", "drink"]),
    e!("🧋", "bubble tea", ["boba", "drink"]),
    // --- objects & symbols ---
    e!("💰", "money bag", ["money", "cash", "rich"]),
    e!("💎", "gem stone", ["diamond", "gem", "value"]),
    e!("⏰", "alarm clock", ["time", "alarm", "late"]),
    e!("📈", "chart increasing", ["up", "growth", "stonks"]),
    e!("📉", "chart decreasing", ["down", "loss", "crash"]),
    e!("🔴", "red circle", ["live", "record", "red"]),
    e!("⭐", "star", ["star", "favorite", "rating"]),
    e!("🌙", "crescent moon", ["moon", "night", "sleep"]),
    e!("☀️", "sun", ["sun", "sunny", "day"]),
    e!("🌈", "rainbow", ["rainbow", "pride", "color"]),
    e!("❄️", "snowflake", ["snow", "cold", "winter"]),
    e!("✅", "check mark button", ["done", "yes", "correct"]),
    e!("❌", "cross mark", ["no", "wrong", "delete"]),
    e!("❓", "question mark", ["question", "what", "huh"]),
    e!("❗", "exclamation mark", ["important", "warning", "alert"]),
    e!("♻️", "recycling symbol", ["recycle", "repeat", "green"]),
    e!("🏳️", "white flag", ["surrender", "gg"]),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_is_reasonably_sized() {
        let n = catalog().len();
        assert!(
            (100..=160).contains(&n),
            "curated catalog should hold ~120 entries, has {n}"
        );
    }

    #[test]
    fn every_entry_is_well_formed() {
        for entry in catalog() {
            assert!(!entry.emoji.is_empty(), "empty emoji for {:?}", entry.name);
            assert!(!entry.name.is_empty(), "empty name for {:?}", entry.emoji);
            assert_eq!(
                entry.name,
                entry.name.to_lowercase(),
                "name not lowercase: {:?}",
                entry.name
            );
            for kw in entry.keywords {
                assert!(!kw.is_empty(), "empty keyword for {:?}", entry.name);
                assert_eq!(*kw, kw.to_lowercase(), "keyword not lowercase: {kw:?}");
            }
        }
    }

    #[test]
    fn search_by_name() {
        let got = search("thinking face", 5);
        assert_eq!(got[0].emoji, "🤔");
    }

    #[test]
    fn search_by_keyword() {
        let got = search("rofl", 5);
        assert!(got.iter().any(|e| e.emoji == "🤣"));
        let got = search("stonks", 5);
        assert_eq!(got[0].emoji, "📈");
    }

    #[test]
    fn name_prefix_matches_rank_first() {
        // "fire" is the exact name of 🔥 and a substring elsewhere
        // ("campfire"-style keywords, "french fries" doesn't match) — the
        // name-prefix hit must come first regardless of catalog position.
        let got = search("fire", 10);
        assert_eq!(got[0].emoji, "🔥");
        // "star" prefixes "star" (⭐) but only substrings "mustard"-like
        // keyword text; the prefix hit leads.
        let got = search("star", 10);
        assert_eq!(got[0].emoji, "⭐");
    }

    #[test]
    fn search_is_case_insensitive() {
        assert_eq!(search("FIRE", 5)[0].emoji, "🔥");
        assert_eq!(search("  Fire ", 5)[0].emoji, "🔥");
    }

    #[test]
    fn empty_query_returns_catalog_head() {
        let got = search("", 5);
        assert_eq!(got.len(), 5);
        for (a, b) in got.iter().zip(catalog().iter()) {
            assert_eq!(a.emoji, b.emoji);
        }
        assert_eq!(search("   ", 3).len(), 3);
    }

    #[test]
    fn max_is_respected() {
        assert_eq!(search("", 1).len(), 1);
        assert_eq!(search("face", 4).len(), 4);
        assert!(search("zzzznotfound", 10).is_empty());
        assert!(search("fire", 0).is_empty());
    }
}
