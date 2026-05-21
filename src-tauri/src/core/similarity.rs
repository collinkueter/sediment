//! Character-trigram name similarity (Phase 3 R4).
//!
//! When an extracted entity is not an exact match for anything in the graph,
//! the staging pipeline checks whether it is a *near* match — a typo or a
//! spelling variant of an entity already filed. The measure is the Dice
//! coefficient over the set of character 3-grams of the two names: cheap,
//! deterministic, and good at catching "Jon Smith" ≈ "John Smith" without an
//! embedding round-trip.

use std::collections::HashSet;

/// Dice similarity of two names in `[0.0, 1.0]`:
/// `2·|shared 3-grams| / (|a 3-grams| + |b 3-grams|)`. Case-insensitive and
/// whitespace-collapsed. Identical names score `1.0`; names sharing no 3-gram
/// score `0.0`. A name shorter than three characters contributes itself as a
/// single gram so very short names still compare.
pub fn trigram_similarity(a: &str, b: &str) -> f32 {
    let ga = trigrams(a);
    let gb = trigrams(b);
    if ga.is_empty() || gb.is_empty() {
        return 0.0;
    }
    let shared = ga.intersection(&gb).count();
    (2.0 * shared as f32) / (ga.len() + gb.len()) as f32
}

/// The set of lowercased character 3-grams of `s`, whitespace-collapsed.
fn trigrams(s: &str) -> HashSet<String> {
    let normalized = s
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    let chars: Vec<char> = normalized.chars().collect();
    let mut set = HashSet::new();
    if chars.is_empty() {
        return set;
    }
    if chars.len() < 3 {
        set.insert(chars.iter().collect::<String>());
        return set;
    }
    for window in chars.windows(3) {
        set.insert(window.iter().collect::<String>());
    }
    set
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_names_score_one() {
        assert_eq!(trigram_similarity("John Smith", "John Smith"), 1.0);
        // Case and surrounding whitespace are normalised away.
        assert_eq!(trigram_similarity("john smith", "  John   Smith "), 1.0);
    }

    #[test]
    fn near_matches_score_above_disjoint_ones() {
        // A one-letter spelling variant is clearly similar.
        let variant = trigram_similarity("Jon Smith", "John Smith");
        assert!(
            variant > 0.5,
            "Jon/John Smith should be a near match: {variant}"
        );
        let typo = trigram_similarity("Micrsoft", "Microsoft");
        assert!(
            typo > 0.5,
            "a single-typo name should be a near match: {typo}"
        );
        // Unrelated names share little.
        let unrelated = trigram_similarity("Jane Doe", "Microsoft");
        assert!(
            unrelated < 0.2,
            "unrelated names should score low: {unrelated}"
        );
    }

    #[test]
    fn empty_names_score_zero() {
        assert_eq!(trigram_similarity("", "John"), 0.0);
        assert_eq!(trigram_similarity("   ", ""), 0.0);
    }

    #[test]
    fn very_short_names_still_compare() {
        assert_eq!(trigram_similarity("Jo", "Jo"), 1.0);
        assert_eq!(trigram_similarity("Jo", "Al"), 0.0);
    }
}
