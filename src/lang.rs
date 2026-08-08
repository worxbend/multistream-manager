//! The language list used by the form's language autocomplete.
//!
//! Twitch's `broadcaster_language` and YouTube's `snippet.defaultLanguage` both
//! take an ISO 639-1 two-letter code. Nobody remembers that Ukrainian is `uk`
//! and not `ua`, so the form lets you type the language's name and picks the
//! code for you.
//!
//! This is the subset Twitch actually offers in its own stream manager, which is
//! the practical limit on what is worth listing.

/// `(code, English name, endonym)` — the endonym is the language's own name for
/// itself, which is often what someone is actually looking for.
pub const LANGUAGES: &[(&str, &str, &str)] = &[
    ("ar", "Arabic", "العربية"),
    ("bg", "Bulgarian", "български"),
    ("cs", "Czech", "čeština"),
    ("da", "Danish", "dansk"),
    ("de", "German", "Deutsch"),
    ("el", "Greek", "Ελληνικά"),
    ("en", "English", "English"),
    ("es", "Spanish", "español"),
    ("fi", "Finnish", "suomi"),
    ("fr", "French", "français"),
    ("he", "Hebrew", "עברית"),
    ("hi", "Hindi", "हिन्दी"),
    ("hu", "Hungarian", "magyar"),
    ("id", "Indonesian", "Bahasa Indonesia"),
    ("it", "Italian", "italiano"),
    ("ja", "Japanese", "日本語"),
    ("ko", "Korean", "한국어"),
    ("ms", "Malay", "Bahasa Melayu"),
    ("nl", "Dutch", "Nederlands"),
    ("no", "Norwegian", "norsk"),
    ("pl", "Polish", "polski"),
    ("pt", "Portuguese", "português"),
    ("ro", "Romanian", "română"),
    ("ru", "Russian", "русский"),
    ("sk", "Slovak", "slovenčina"),
    ("sv", "Swedish", "svenska"),
    ("th", "Thai", "ไทย"),
    ("tr", "Turkish", "Türkçe"),
    ("uk", "Ukrainian", "українська"),
    ("vi", "Vietnamese", "Tiếng Việt"),
    ("zh", "Chinese", "中文"),
];

/// The English name for a code, or the code itself if it is not in the list.
pub fn name_for(code: &str) -> String {
    LANGUAGES
        .iter()
        .find(|(c, _, _)| c.eq_ignore_ascii_case(code))
        .map(|(_, name, _)| (*name).to_string())
        .unwrap_or_else(|| code.to_string())
}

/// Search by code, English name or endonym, returning `(code, label)` pairs.
///
/// An exact code match is promoted to the front, so typing `no` offers
/// Norwegian first rather than burying it under every language whose name
/// happens to contain the letters "no".
pub fn search(query: &str) -> Vec<(String, String)> {
    let needle = query.trim().to_lowercase();

    let mut exact = Vec::new();
    let mut partial = Vec::new();

    for (code, name, endonym) in LANGUAGES {
        let label = format!("{name} ({code}) — {endonym}");

        if needle.is_empty() {
            partial.push((code.to_string(), label));
            continue;
        }

        if code.eq_ignore_ascii_case(&needle) {
            exact.push((code.to_string(), label));
        } else if name.to_lowercase().contains(&needle)
            || endonym.to_lowercase().contains(&needle)
            || code.starts_with(&needle)
        {
            partial.push((code.to_string(), label));
        }
    }

    exact.extend(partial);
    exact
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn searching_by_english_name_finds_the_code() {
        let results = search("polish");
        assert_eq!(results[0].0, "pl");
    }

    #[test]
    fn searching_by_endonym_works_too() {
        let results = search("polski");
        assert_eq!(results[0].0, "pl");
    }

    #[test]
    fn an_exact_code_match_is_listed_first() {
        // "no" is Norwegian's code but also appears inside several other names,
        // so the exact match has to win.
        let results = search("no");
        assert_eq!(
            results[0].0,
            "no",
            "got {:?}",
            &results[..3.min(results.len())]
        );
    }

    #[test]
    fn an_empty_query_returns_every_language() {
        assert_eq!(search("").len(), LANGUAGES.len());
    }

    #[test]
    fn an_unknown_query_returns_nothing() {
        assert!(search("klingon").is_empty());
    }

    #[test]
    fn codes_resolve_to_readable_names() {
        assert_eq!(name_for("uk"), "Ukrainian");
        assert_eq!(name_for("en"), "English");
    }

    #[test]
    fn an_unknown_code_is_returned_unchanged() {
        assert_eq!(name_for("xx"), "xx");
    }

    #[test]
    fn every_code_is_two_letters_and_unique() {
        let mut codes: Vec<&str> = LANGUAGES.iter().map(|(c, _, _)| *c).collect();
        let count = codes.len();
        codes.sort_unstable();
        codes.dedup();
        assert_eq!(codes.len(), count, "duplicate language code in the table");
        assert!(LANGUAGES.iter().all(|(c, _, _)| c.len() == 2));
    }
}
