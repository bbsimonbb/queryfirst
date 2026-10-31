//! Approximation of JS `String.prototype.localeCompare` (ICU root collation, default
//! strength) that is exact for identifier-like strings (ASCII letters, digits, punctuation).

use std::cmp::Ordering;

/// CLDR root order of ASCII punctuation/symbols (all sort before digits and letters).
const SYMBOLS: &str = " _-,;:!?.'\"()[]{}@*/\\&#%`^+<=>|~$";

/// Primary weight: symbols < digits < letters (case-insensitive) < everything else.
fn primary(c: char) -> u32 {
    if let Some(i) = SYMBOLS.find(c) {
        return i as u32;
    }
    match c {
        '0'..='9' => 100 + (c as u32 - '0' as u32),
        'a'..='z' => 200 + (c as u32 - 'a' as u32),
        'A'..='Z' => 200 + (c as u32 - 'A' as u32),
        _ => 1000 + c as u32,
    }
}

/// Tertiary weight: lowercase sorts before uppercase.
fn tertiary(c: char) -> u32 {
    u32::from(c.is_ascii_uppercase())
}

pub fn locale_compare(a: &str, b: &str) -> Ordering {
    a.chars()
        .map(primary)
        .cmp(b.chars().map(primary))
        .then_with(|| a.chars().map(tertiary).cmp(b.chars().map(tertiary)))
}
