//! Self-introduction detection (ADR-0017 §6) — auto-*suggest* a speaker's name.
//!
//! When someone says their own name on a call ("hi, I'm Sarah", "this is John
//! Smith"), the capture worker offers that name as a one-tap rename for the current
//! (still-unnamed) speaker. It is **suggested, never asserted** — a false positive
//! costs one dismissal — which is why a lightweight, dependency-free heuristic is the
//! right tool here, not a model.
//!
//! Two facts shape the parsing:
//!   - the streaming ASR emits **lowercase, unpunctuated** text, so detection is
//!     case-insensitive and the result is Title-cased (we cannot lean on capitals);
//!   - high-frequency cues ("I'm …", "this is …") are followed by ordinary words far
//!     more often than by names, so a candidate is rejected when it is a common word
//!     ([`STOPWORDS`]). The conservative bias keeps the suggestion from crying wolf.

// Only reached on the capture path (the `audio` feature); unused in a headless
// `--no-default-features` build, so allow dead_code rather than gate the module.
#![allow(dead_code)]

/// The longest name we will lift from a cue — a first name, or first + last.
const MAX_NAME_WORDS: usize = 2;

/// Cue phrases that are *followed by* the speaker's name. Lowercase, apostrophes
/// included and a no-apostrophe twin ("im") because the ASR sometimes drops it.
/// Ordered longest-first so "my name is" wins over a bare "is".
const LEADING_CUES: &[&str] = &[
    "you are speaking with",
    "you're speaking with",
    "my name is",
    "i am called",
    "i'm called",
    "im called",
    "this is",
    "call me",
    "i am",
    "i'm",
    "im",
];

/// Words that are never a name in the slot right after a cue — articles, pronouns,
/// fillers, and the most common adjectives/verbs/adverbs that follow "I'm …" /
/// "this is …". Not exhaustive (it can't be); it kills the loudest false positives.
const STOPWORDS: &[&str] = &[
    "a", "an", "the", "and", "or", "but", "so", "to", "of", "in", "on", "at", "for", "with",
    "is", "was", "are", "am", "be", "been", "not", "no", "yes", "just", "really", "very",
    "here", "there", "now", "then", "going", "gonna", "trying", "about", "like", "good", "great",
    "fine", "okay", "ok", "sorry", "sure", "done", "ready", "happy", "glad", "afraid", "able",
    "this", "that", "these", "those", "it", "its", "he", "she", "they", "we", "you", "i", "me",
    "my", "your", "our", "his", "her", "their", "one", "two", "all", "right", "left", "back",
    "still", "also", "only", "well", "thinking", "looking", "talking", "calling", "saying",
    "wondering", "hoping", "pretty", "quite", "kind", "sort", "from", "what", "who", "how", "why",
];

/// Detect a self-introduction in `text` and return the speaker's Title-cased name,
/// or `None`. Tries each cue in order; the first that yields a valid 1–2 word name
/// wins. Also recognises the trailing "&lt;name&gt; here" form ("sarah here") at the
/// start of a segment. Returns at most a first + last name.
pub fn detect_self_introduction(text: &str) -> Option<String> {
    let lower = text.to_lowercase();

    for cue in LEADING_CUES {
        if let Some(rest) = after_cue(&lower, cue) {
            if let Some(name) = take_name(rest) {
                return Some(name);
            }
        }
    }

    // Trailing form: "<name> here" / "<name> speaking", only when it opens the
    // segment (so "over here", "right here" mid-sentence don't trip it).
    let mut words = lower.split_whitespace();
    if let (Some(w1), Some(w2)) = (words.next(), words.next()) {
        if matches!(w2.trim_matches(is_punct), "here" | "speaking") {
            if let Some(name) = clean_name_word(w1) {
                return Some(title_case(&name));
            }
        }
    }
    None
}

/// The substring of `haystack` immediately after the first occurrence of `cue` when
/// `cue` sits on a word boundary (so "is" inside "this" never matches). `None` when
/// the cue is absent.
fn after_cue<'a>(haystack: &'a str, cue: &str) -> Option<&'a str> {
    let mut from = 0;
    while let Some(rel) = haystack[from..].find(cue) {
        let start = from + rel;
        let end = start + cue.len();
        let before_ok = start == 0 || haystack.as_bytes()[start - 1] == b' ';
        let after_ok = end == haystack.len() || haystack.as_bytes()[end] == b' ';
        if before_ok && after_ok {
            return Some(haystack[end..].trim_start());
        }
        from = end;
    }
    None
}

/// Pull up to [`MAX_NAME_WORDS`] consecutive name-like words off the front of `rest`
/// and Title-case them. The first word must be a valid name word; trailing words are
/// taken only while they keep qualifying. `None` when the first word doesn't qualify.
fn take_name(rest: &str) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    for word in rest.split_whitespace() {
        match clean_name_word(word) {
            Some(w) => parts.push(title_case(&w)),
            None => break,
        }
        if parts.len() == MAX_NAME_WORDS {
            break;
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" "))
    }
}

/// Normalise a token to a candidate name word: strip surrounding punctuation, then
/// accept only a 2–15 char all-alphabetic token that isn't a [`STOPWORDS`] entry.
fn clean_name_word(word: &str) -> Option<String> {
    let w = word.trim_matches(is_punct);
    if w.len() < 2 || w.len() > 15 {
        return None;
    }
    if !w.chars().all(|c| c.is_ascii_alphabetic()) {
        return None;
    }
    if STOPWORDS.contains(&w) {
        return None;
    }
    Some(w.to_string())
}

fn is_punct(c: char) -> bool {
    !c.is_ascii_alphanumeric()
}

/// `"sarah"` → `"Sarah"`, `"mcadam"` stays `"Mcadam"` (good enough for a suggestion).
fn title_case(w: &str) -> String {
    let mut chars = w.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + &chars.as_str().to_lowercase(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_common_introductions() {
        assert_eq!(detect_self_introduction("hi i'm sarah"), Some("Sarah".into()));
        assert_eq!(detect_self_introduction("im john"), Some("John".into()));
        assert_eq!(
            detect_self_introduction("my name is john smith and i lead sales"),
            Some("John Smith".into())
        );
        assert_eq!(
            detect_self_introduction("this is mary"),
            Some("Mary".into())
        );
        assert_eq!(detect_self_introduction("call me alex"), Some("Alex".into()));
        assert_eq!(
            detect_self_introduction("you're speaking with priya patel today"),
            Some("Priya Patel".into())
        );
        assert_eq!(detect_self_introduction("sarah here"), Some("Sarah".into()));
        assert_eq!(detect_self_introduction("i am dana"), Some("Dana".into()));
    }

    #[test]
    fn rejects_non_introductions() {
        assert_eq!(detect_self_introduction("i'm not sure about that"), None);
        assert_eq!(detect_self_introduction("i'm going to the store"), None);
        assert_eq!(detect_self_introduction("this is great work everyone"), None);
        assert_eq!(detect_self_introduction("this is the plan"), None);
        assert_eq!(detect_self_introduction("i am done"), None);
        assert_eq!(detect_self_introduction("let's move over here"), None);
        assert_eq!(detect_self_introduction("so what do you think"), None);
        assert_eq!(detect_self_introduction(""), None);
    }

    #[test]
    fn word_boundary_avoids_substring_false_hit() {
        // "is" inside "this"/"crisis" must not be read as a cue.
        assert_eq!(detect_self_introduction("the crisis deepened"), None);
    }

    #[test]
    fn stops_at_two_words() {
        assert_eq!(
            detect_self_introduction("my name is anna maria gonzalez ramirez"),
            Some("Anna Maria".into())
        );
    }
}
