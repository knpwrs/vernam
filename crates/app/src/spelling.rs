//! Spell checking the composer, as Desktop's (`app/spell_check.main.ts`,
//! Chromium's checker): misspelled words are underlined, and right-clicking
//! one offers its suggestions. Here macOS's `NSSpellChecker` does the
//! checking, off the main thread once typing pauses (it may be used from
//! any thread). Words in mentions and links aren't checked, and the word
//! being typed isn't marked until it's done.

use std::ops::Range;

/// The misspelled words in a text, as byte ranges in order: the system's
/// checker (none off macOS).
pub fn check(text: &str) -> Vec<Range<usize>> {
    #[cfg(target_os = "macos")]
    return macos::check(text);
    #[cfg(target_os = "linux")]
    return linux::check(text);
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = text;
        Vec::new()
    }
}

/// The system's suggestions.
pub fn guesses(text: &str, range: Range<usize>) -> Vec<String> {
    #[cfg(target_os = "macos")]
    return macos::guesses(text, range);
    #[cfg(target_os = "linux")]
    return linux::guesses(text, range);
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = (text, range);
        Vec::new()
    }
}

/// UTF-16 offset of a byte offset (NSString counts UTF-16 units).
#[cfg(target_os = "macos")]
fn utf16_at(text: &str, byte: usize) -> usize {
    text[..byte.min(text.len())].encode_utf16().count()
}

/// Byte offset of a UTF-16 offset, if it falls on a character boundary.
#[cfg(target_os = "macos")]
fn byte_at(text: &str, utf16: usize) -> Option<usize> {
    let mut units = 0;
    for (byte, c) in text.char_indices() {
        if units == utf16 {
            return Some(byte);
        }
        units += c.len_utf16();
    }
    (units == utf16).then_some(text.len())
}

#[cfg(target_os = "macos")]
mod macos {
    use std::ops::Range;

    use objc2_app_kit::NSSpellChecker;
    use objc2_foundation::{NSNotFound, NSRange, NSString};

    pub fn check(text: &str) -> Vec<Range<usize>> {
        let string = NSString::from_str(text);
        let checker = NSSpellChecker::sharedSpellChecker();
        let total = string.length();
        let mut found = Vec::new();
        let mut from = 0usize;
        while from < total {
            let range: NSRange = unsafe {
                checker.checkSpellingOfString_startingAt_language_wrap_inSpellDocumentWithTag_wordCount(
                    &string,
                    from as isize,
                    None,
                    false,
                    0,
                    std::ptr::null_mut(),
                )
            };
            if range.location == NSNotFound as usize || range.length == 0 || range.location < from {
                break;
            }
            if let (Some(start), Some(end)) = (super::byte_at(text, range.location), super::byte_at(text, range.location + range.length)) {
                found.push(start..end);
            }
            from = range.location + range.length;
        }
        found
    }

    pub fn guesses(text: &str, range: Range<usize>) -> Vec<String> {
        let string = NSString::from_str(text);
        let start = super::utf16_at(text, range.start);
        let range = NSRange::new(start, super::utf16_at(text, range.end) - start);
        let checker = NSSpellChecker::sharedSpellChecker();
        checker
            .guessesForWordRange_inString_language_inSpellDocumentWithTag(range, &string, None, 0)
            .map(|list| list.iter().map(|s| s.to_string()).collect())
            .unwrap_or_default()
    }
}

/// Hunspell, the checker under Desktop's Electron and Chromium's bundled
/// dictionaries: spellbook parses the system's dictionary and checks.
#[cfg(target_os = "linux")]
mod linux {
    use std::ops::Range;
    use std::path::PathBuf;
    use std::sync::OnceLock;

    use spellbook::Dictionary;

    /// What a context menu shows.
    const MAX_GUESSES: usize = 5;

    pub fn check(text: &str) -> Vec<Range<usize>> {
        let Some(dictionary) = dictionary() else { return Vec::new() };
        words(text)
            .into_iter()
            // `2nd`, `5pm`, `3x`: a dictionary rejects them all (its words
            // are letters), so they'd read as misspelled.
            .filter(|(_, word)| !word.chars().any(|c| c.is_numeric()))
            .filter(|(_, word)| !dictionary.check(word))
            .map(|(range, _)| range)
            .collect()
    }

    pub fn guesses(text: &str, range: Range<usize>) -> Vec<String> {
        let Some(dictionary) = dictionary() else { return Vec::new() };
        let mut guesses = Vec::new();
        dictionary.suggest(&text[range], &mut guesses);
        guesses.truncate(MAX_GUESSES);
        guesses
    }

    /// The user's language's dictionary, loaded once (None: none is
    /// installed, `hunspell-en_US` and friends). Without one, nothing is
    /// marked, as off macOS with no spellchecker.
    fn dictionary() -> Option<&'static Dictionary> {
        static DICTIONARY: OnceLock<Option<Dictionary>> = OnceLock::new();
        DICTIONARY
            .get_or_init(|| {
                let languages = languages();
                for directory in directories() {
                    for language in &languages {
                        let (Ok(aff), Ok(dic)) = (
                            std::fs::read_to_string(directory.join(format!("{language}.aff"))),
                            std::fs::read_to_string(directory.join(format!("{language}.dic"))),
                        ) else {
                            continue;
                        };
                        match Dictionary::new(&aff, &dic) {
                            Ok(dictionary) => return Some(dictionary),
                            Err(error) => tracing::warn!("couldn't parse {language}'s dictionary: {error}"),
                        }
                    }
                }
                tracing::info!("no hunspell dictionary for any of {languages:?}, spellcheck is off");
                None
            })
            .as_ref()
    }

    /// The user's languages, longest first: `en_US.UTF-8` in `LC_ALL`
    /// (glibc's first pick), `LC_MESSAGES`, or `LANG` is `en_US`, then its
    /// bare language `en`.
    fn languages() -> Vec<String> {
        let raw = std::env::var("LC_ALL")
            .or_else(|_| std::env::var("LC_MESSAGES"))
            .or_else(|_| std::env::var("LANG"))
            .unwrap_or_default();
        let base = raw.split('.').next().unwrap_or_default().to_string();
        let mut languages = vec![base.clone()];
        if let Some((language, _)) = base.split_once('_') {
            languages.push(language.to_string());
        }
        languages.retain(|language| !language.is_empty() && language != "C" && language != "POSIX");
        languages
    }

    /// Where hunspell dictionaries live: the user's first (a user-installed
    /// dictionary should win over the system's), then the system's, which
    /// Debian's `hunspell` and Fedora's `myspell` name differently.
    fn directories() -> Vec<PathBuf> {
        let mut directories = Vec::new();
        if let Some(data_home) = std::env::var_os("XDG_DATA_HOME") {
            directories.push(PathBuf::from(data_home).join("hunspell"));
        } else if let Some(home) = std::env::var_os("HOME") {
            directories.push(PathBuf::from(home).join(".local/share/hunspell"));
        }
        directories.push(PathBuf::from("/usr/share/hunspell"));
        directories.push(PathBuf::from("/usr/share/myspell/dicts"));
        directories.push(PathBuf::from("/usr/local/share/hunspell"));
        directories
    }

    /// The words in a text, as (byte range, word): runs of alphanumeric
    /// characters and internal apostrophes (`don't`, `isn’t`), with ones
    /// hanging off an edge trimmed (`'tis` is `tis`). Spellbook leaves
    /// tokenizing to its caller.
    pub(crate) fn words(text: &str) -> Vec<(Range<usize>, &str)> {
        let apostrophe = |c: char| c == '\'' || c == '’';
        let mut words = Vec::new();
        let mut start: Option<usize> = None;
        for (byte, c) in text.char_indices().chain([(text.len(), '\0')]) {
            if c.is_alphanumeric() || (start.is_some() && apostrophe(c)) {
                if start.is_none() {
                    start = Some(byte);
                }
                continue;
            }
            if let Some(from) = start.take() {
                let body = &text[from..byte];
                let inner = body.trim_start_matches(apostrophe).trim_end_matches(apostrophe);
                let at = from + (body.len() - inner.len());
                if !inner.is_empty() {
                    words.push((at..at + inner.len(), inner));
                }
            }
        }
        words
    }
}

/// The misspellings to mark: not in a mention or a link (`skip`), and not
/// the word the caret is at the end of while it's still being typed.
pub fn marked(found: &[Range<usize>], skip: &[Range<usize>], caret: Option<usize>) -> Vec<Range<usize>> {
    found
        .iter()
        .filter(|r| !skip.iter().any(|s| s.start < r.end && r.start < s.end))
        .filter(|r| caret != Some(r.end))
        .cloned()
        .collect()
}

/// Misspellings found in `checked`, carried over to `now` (typed since):
/// those before the change stay, those after it move with the text, and
/// those it touched are dropped until the next check.
pub fn carry_over(checked: &str, found: &[Range<usize>], now: &str) -> Vec<Range<usize>> {
    if checked == now {
        return found.to_vec();
    }
    let prefix = checked.bytes().zip(now.bytes()).take_while(|(a, b)| a == b).count();
    let max_suffix = checked.len().min(now.len()) - prefix;
    let suffix = checked.bytes().rev().zip(now.bytes().rev()).take(max_suffix).take_while(|(a, b)| a == b).count();
    let changed_end = checked.len() - suffix;
    let shift = now.len() as isize - checked.len() as isize;
    // A word still ends (or starts) where it did when the text beside it
    // isn't part of a word.
    let word_char = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '\'' || c == '’');
    found
        .iter()
        .filter_map(|r| {
            if r.end < prefix || (r.end == prefix && !word_char(now[prefix..].chars().next())) {
                Some(r.clone())
            } else if r.start > changed_end
                || (r.start == changed_end
                    && !word_char(now[..(r.start as isize + shift).max(0) as usize].chars().next_back()))
            {
                let start = r.start.checked_add_signed(shift)?;
                let end = r.end.checked_add_signed(shift)?;
                (now.is_char_boundary(start) && now.is_char_boundary(end)).then_some(start..end)
            } else {
                None
            }
        })
        .collect()
}

/// The misspelling a right-click at byte `at` (the caret offset nearest
/// it) lands on. Its end counts, as Chromium's `FirstMarkerAroundPosition`
/// takes the word around the position: just past a word, or on the left
/// half of a comma after it, finds the word.
pub fn at(marked: &[Range<usize>], at: usize) -> Option<Range<usize>> {
    marked.iter().find(|r| r.start <= at && at <= r.end).cloned()
}

#[cfg(test)]
// Ranges of bytes, not lists of them.
#[allow(clippy::single_range_in_vec_init)]
mod tests {
    use super::*;

    #[cfg(target_os = "macos")]
    #[test]
    fn utf16_offsets_map_to_bytes() {
        let text = "é😀a";
        assert_eq!(utf16_at(text, 2), 1);
        assert_eq!(utf16_at(text, 6), 3);
        assert_eq!(byte_at(text, 3), Some(6));
        assert_eq!(byte_at(text, 2), None, "inside the emoji");
        assert_eq!(byte_at(text, 4), Some(7));
    }

    #[test]
    fn mentions_links_and_the_word_being_typed_arent_marked() {
        let found = [0..4, 10..14, 20..25];
        assert_eq!(marked(&found, &[9..15], None), [0..4, 20..25]);
        assert_eq!(marked(&found, &[], Some(25)), [0..4, 10..14], "still typing it");
        assert_eq!(marked(&found, &[], Some(26)), found);
    }

    #[test]
    fn misspellings_follow_the_text_as_its_typed() {
        let checked = "helo wrld bye";
        let found = [0..4, 5..9];
        assert_eq!(carry_over(checked, &found, checked), found);
        // Typed at the end: both stay.
        assert_eq!(carry_over(checked, &found, "helo wrld bye now"), found);
        // Typed at the start: both move.
        assert_eq!(carry_over(checked, &found, "oh helo wrld bye"), [3..7, 8..12]);
        // The second word fixed: only the first stays.
        assert_eq!(carry_over(checked, &found, "helo world bye"), [0..4]);
        // Typed onto a word: it's checked again.
        assert_eq!(carry_over(checked, &found, "xhelo wrld bye"), [6..10]);
        assert_eq!(carry_over(checked, &found, "helox wrld bye"), [6..10]);
        assert_eq!(at(&[0..4, 8..12], 10), Some(8..12));
        assert_eq!(at(&[0..4], 6), None);
        assert_eq!(at(&[0..4], 4), Some(0..4), "just past it");
    }

    /// macOS's checker, headless: obvious misspellings are found, and a
    /// word it knows isn't. (Nothing depends on the dictionary beyond that.)
    #[cfg(target_os = "macos")]
    #[test]
    fn the_system_checker_finds_obvious_misspellings() {
        let text = "Thiss is a tesst of the checker";
        let found = check(text);
        let words: Vec<&str> = found.iter().map(|r| &text[r.clone()]).collect();
        assert!(words.contains(&"Thiss") && words.contains(&"tesst"), "{words:?}");
        assert!(!words.contains(&"checker"));
        let range = found.iter().find(|r| &text[(*r).clone()] == "tesst").unwrap().clone();
        assert!(!guesses(text, range).is_empty());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn words_keep_internal_apostrophes_and_drop_edge_ones() {
        let text = "don't 'tis o’’kay hello, isn’t — a’";
        let found: Vec<&str> = linux::words(text).iter().map(|(_, w)| *w).collect();
        assert_eq!(found, ["don't", "tis", "o’’kay", "hello", "isn’t", "a"]);
    }

    /// The system's dictionary, if one is installed: obvious misspellings
    /// are found, and a word it knows isn't.
    #[cfg(target_os = "linux")]
    #[test]
    fn the_system_checker_finds_obvious_misspellings() {
        // Safety: fine in a test before any thread reads the environment.
        unsafe { std::env::set_var("LC_MESSAGES", "en_US.UTF-8") };
        let text = "Thiss is a tesst of the checker 2nd 5pm";
        let found = check(text);
        if found.is_empty() {
            eprintln!("skipping: no hunspell dictionary installed");
            return;
        }
        let words: Vec<&str> = found.iter().map(|r| &text[r.clone()]).collect();
        assert!(words.contains(&"Thiss") && words.contains(&"tesst"), "{words:?}");
        assert!(!words.contains(&"checker"), "{words:?}");
        // Numbers are letters to a dictionary, so they aren't misspellings.
        assert!(!words.contains(&"2nd") && !words.contains(&"5pm"), "{words:?}");
        assert!(!guesses(text, found.iter().find(|r| &text[(*r).clone()] == "tesst").unwrap().clone()).is_empty());
    }
}
