//! Splitting quick-add input into words that remember where they came from.
//!
//! Every later stage works on these rather than on the raw string, so that removing a
//! recognised token from the title is a matter of deleting byte ranges rather than
//! string-replacing text that might occur twice.

use std::ops::Range;

/// One whitespace-delimited run of the input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Word {
    /// The text exactly as the user typed it.
    pub text: String,

    /// The same text lowercased, for matching keywords.
    pub lower: String,

    /// Where it sits in the input, in bytes.
    pub range: Range<usize>,
}

impl Word {
    /// Whether this word matches a keyword, ignoring case.
    #[must_use]
    pub fn is(&self, keyword: &str) -> bool {
        self.lower == keyword
    }

    /// The word with any trailing punctuation removed, for matching keywords that end a
    /// sentence: `due tomorrow.` should still find `tomorrow`.
    #[must_use]
    pub fn trimmed(&self) -> &str {
        self.lower.trim_end_matches(['.', ',', ';', ':', '!', '?'])
    }
}

/// Split input into words, recording byte ranges.
///
/// Whitespace separates words, with one exception: a magic prefix followed by an opening
/// quote or bracket takes everything to the matching close, so `*"needs review"` and
/// `+[Home Renovation]` stay whole.
///
/// The exception is deliberately narrow. Treating `\'` as a quote everywhere would make
/// "don\'t forget the milk" a single word up to the next apostrophe, which is a worse
/// bug than the one it fixes.
#[must_use]
pub fn split(input: &str) -> Vec<Word> {
    let mut words = Vec::new();
    let mut cursor = 0;

    while cursor < input.len() {
        // Skip whitespace.
        match input[cursor..].find(|c: char| !c.is_whitespace()) {
            Some(offset) => cursor += offset,
            None => break,
        }

        let start = cursor;
        let end = quoted_end(input, start).unwrap_or_else(|| {
            input[start..]
                .find(char::is_whitespace)
                .map_or(input.len(), |offset| start + offset)
        });

        words.push(word(input, start..end));
        cursor = end;
    }

    words
}

/// If a word at `start` is a magic prefix followed by a bracketed value, where it ends.
fn quoted_end(input: &str, start: usize) -> Option<usize> {
    let mut chars = input[start..].char_indices();
    let (_, prefix) = chars.next()?;
    if !matches!(prefix, '*' | '@' | '+' | '!') {
        return None;
    }

    let (open_offset, open) = chars.next()?;
    let close = match open {
        '"' => '"',
        '\'' => '\'',
        '[' => ']',
        '(' => ')',
        _ => return None,
    };

    let value_start = start + open_offset + open.len_utf8();
    let close_offset = input[value_start..].find(close)?;
    Some(value_start + close_offset + close.len_utf8())
}

fn word(input: &str, range: Range<usize>) -> Word {
    let text = input[range.clone()].to_string();
    Word {
        lower: text.to_lowercase(),
        text,
        range,
    }
}

/// Remove byte ranges from `input` and tidy the whitespace they leave behind.
///
/// Deleting ranges rather than replacing substrings is what keeps `Buy *milk milk` from
/// losing the wrong word.
#[must_use]
pub fn remove_ranges(input: &str, ranges: &[Range<usize>]) -> String {
    let mut sorted: Vec<Range<usize>> = ranges.to_vec();
    sorted.sort_by_key(|r| r.start);

    let mut out = String::with_capacity(input.len());
    let mut cursor = 0;
    for range in sorted {
        if range.start >= cursor {
            out.push_str(&input[cursor..range.start]);
            cursor = range.end;
        } else if range.end > cursor {
            // Overlapping ranges: skip the part already consumed.
            cursor = range.end;
        }
    }
    if cursor < input.len() {
        out.push_str(&input[cursor..]);
    }

    collapse_whitespace(&out)
}

/// Squeeze runs of whitespace and trim, so a removed token leaves no double space.
fn collapse_whitespace(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last_was_space = true;
    for ch in text.chars() {
        if ch.is_whitespace() {
            if !last_was_space {
                out.push(' ');
            }
            last_was_space = true;
        } else {
            out.push(ch);
            last_was_space = false;
        }
    }
    while out.ends_with(' ') {
        out.pop();
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn words_know_where_they_came_from() {
        let words = split("Buy  milk today");
        assert_eq!(words.len(), 3);
        assert_eq!(words[0].text, "Buy");
        assert_eq!(words[0].range, 0..3);
        assert_eq!(words[2].text, "today");
        assert_eq!(&"Buy  milk today"[words[2].range.clone()], "today");
    }

    #[test]
    fn leading_and_trailing_space_does_not_produce_empty_words() {
        let words = split("   spaced   ");
        assert_eq!(words.len(), 1);
        assert_eq!(words[0].text, "spaced");
    }

    #[test]
    fn empty_input_has_no_words() {
        assert!(split("").is_empty());
        assert!(split("    ").is_empty());
    }

    #[test]
    fn ranges_are_byte_correct_for_multibyte_input() {
        let input = "café *naïve today";
        let words = split(input);
        assert_eq!(&input[words[1].range.clone()], "*naïve");
    }

    #[test]
    fn removing_a_range_takes_the_right_occurrence() {
        // The reason ranges exist: a naive string replace would delete the first "milk".
        let input = "Buy milk *milk";
        let words = split(input);
        assert_eq!(remove_ranges(input, &[words[2].range.clone()]), "Buy milk");
    }

    #[test]
    fn removal_leaves_no_double_spaces() {
        let input = "Call Bob *urgent about the invoice";
        let words = split(input);
        assert_eq!(
            remove_ranges(input, &[words[2].range.clone()]),
            "Call Bob about the invoice"
        );
    }

    #[test]
    fn several_ranges_come_out_in_any_order() {
        let input = "a *b c +d e";
        let words = split(input);
        let ranges = vec![words[3].range.clone(), words[1].range.clone()];
        assert_eq!(remove_ranges(input, &ranges), "a c e");
    }

    #[test]
    fn a_quoted_value_after_a_prefix_stays_one_word() {
        let input = r#"Ship it *"needs review" +[Home Renovation] @'Ada Lovelace'"#;
        let words = split(input);
        let texts: Vec<&str> = words.iter().map(|w| w.text.as_str()).collect();
        assert_eq!(
            texts,
            vec![
                "Ship",
                "it",
                "*\"needs review\"",
                "+[Home Renovation]",
                "@'Ada Lovelace'",
            ]
        );
    }

    #[test]
    fn an_apostrophe_in_ordinary_text_is_not_a_quote() {
        // The narrow rule earns its keep here: a general quote-aware splitter would
        // swallow the rest of this line at "don't".
        let words = split("don't forget the milk");
        assert_eq!(words.len(), 4);
        assert_eq!(words[0].text, "don't");
    }

    #[test]
    fn an_unclosed_quote_falls_back_to_whitespace_splitting() {
        // Half-typed input is the normal state of a quick-add box.
        let words = split(r#"Ship *"needs review"#);
        assert_eq!(words.len(), 3);
        assert_eq!(words[1].text, "*\"needs");
    }

    #[test]
    fn trailing_punctuation_does_not_hide_a_keyword() {
        let words = split("due tomorrow.");
        assert_eq!(words[1].trimmed(), "tomorrow");
    }
}
