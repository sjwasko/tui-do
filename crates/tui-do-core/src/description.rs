//! A task description given from outside the interface: checked, normalised, and
//! summarised for a report that never prints it.
//!
//! `tui-do add` takes one inline, from a file or from stdin, and stores it exactly as
//! given -- no Markdown conversion, no trimming beyond one trailing newline. A
//! description on the wire is Markdown, plain text or TipTap HTML with nothing saying
//! which (`markdown::looks_like_html` decides when it is read), so guessing here would
//! only add a fourth shape. Here rather than in the binary so the agent surface can use
//! the same rules.

use std::fmt;

/// The largest description accepted, in bytes: 64 KiB.
///
/// Vikunja sets no limit we can find: the OpenAPI document gives `description` no
/// `maxLength` (where `title` has a `minLength`), and the server stores it as long text.
/// So this one is ours, and generous -- a long design note or a full stack trace is a few
/// KiB. It exists for the file that was named by mistake (a log, a binary), because every
/// write re-sends the whole task and a description that size would ride along on every
/// later edit of it.
pub const MAX_BYTES: usize = 64 * 1024;

/// Why a description was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DescriptionError {
    /// Larger than [`MAX_BYTES`]. Carries the size seen, which may be only a lower bound
    /// when the reader stopped early.
    TooLarge(usize),
    /// Not UTF-8; carries the offset of the first bad byte.
    NotUtf8(usize),
    /// Contains a NUL byte, at this offset. A text column on PostgreSQL rejects one, and
    /// no text a person means to file has one: it is a binary file named by mistake.
    Nul(usize),
}

impl fmt::Display for DescriptionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge(size) => write!(
                f,
                "the description is {} bytes or more, and the limit is {} (64 KiB). \
                 Attach anything that large in the web interface, or link to it",
                thousands(*size),
                thousands(MAX_BYTES)
            ),
            Self::NotUtf8(at) => write!(
                f,
                "the description is not UTF-8 text: byte {} is not valid. Convert it \
                 first, for example with iconv",
                thousands(*at)
            ),
            Self::Nul(at) => write!(
                f,
                "the description contains a NUL byte at byte {}, so it is not text; \
                 check the file is the one you meant",
                thousands(*at)
            ),
        }
    }
}

impl std::error::Error for DescriptionError {}

/// Check and normalise a description as it arrived.
///
/// Stored exactly as given, except that CRLF becomes LF and one trailing newline is
/// dropped -- a file or a heredoc always ends in one, and nobody means it as content.
/// Only one, so a description that ends in a blank line on purpose keeps it.
///
/// # Errors
/// Larger than [`MAX_BYTES`], not UTF-8, or carrying a NUL.
pub fn normalise(bytes: Vec<u8>) -> Result<String, DescriptionError> {
    if bytes.len() > MAX_BYTES {
        return Err(DescriptionError::TooLarge(bytes.len()));
    }
    let text = String::from_utf8(bytes)
        .map_err(|error| DescriptionError::NotUtf8(error.utf8_error().valid_up_to()))?;
    if let Some(at) = text.find('\0') {
        return Err(DescriptionError::Nul(at));
    }
    let mut text = text.replace("\r\n", "\n");
    if text.ends_with('\n') {
        text.pop();
    }
    Ok(text)
}

/// How big a description is, in the words `tui-do add` reports it with.
///
/// `3 paragraphs, 1,240 characters`. A paragraph is a run of lines with text on them,
/// ended by a blank line or the end; characters are Unicode scalar values, not bytes,
/// because that is what a person counts.
#[must_use]
pub fn size(text: &str) -> String {
    let paragraphs = paragraphs(text);
    let characters = text.chars().count();
    format!(
        "{} {}, {} {}",
        thousands(paragraphs),
        if paragraphs == 1 {
            "paragraph"
        } else {
            "paragraphs"
        },
        thousands(characters),
        if characters == 1 {
            "character"
        } else {
            "characters"
        },
    )
}

/// The number of paragraphs in `text`.
#[must_use]
pub fn paragraphs(text: &str) -> usize {
    let mut count = 0;
    let mut inside = false;
    for line in text.lines() {
        let blank = line.trim().is_empty();
        if !blank && !inside {
            count += 1;
        }
        inside = !blank;
    }
    count
}

/// The first line with text on it, cut to `width` characters with an ellipsis.
///
/// For a dry run, which shows enough to tell which description it read without
/// printing the description. The caller still makes it printable: this is the user's
/// own text, but a control character in it is no better on their terminal for that.
#[must_use]
pub fn first_line(text: &str, width: usize) -> String {
    let line = text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default();
    if line.chars().count() <= width {
        return line.to_string();
    }
    let mut cut: String = line.chars().take(width.saturating_sub(1)).collect();
    cut.truncate(cut.trim_end().len());
    cut.push('…');
    cut
}

/// `1240` as `1,240`.
fn thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, digit) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn a_description_is_kept_as_given_but_for_one_trailing_newline() {
        let given = "# Plan\n\n  - indented, *Markdown* <b>and</b> +tokens tomorrow\n\n";
        assert_eq!(
            normalise(given.as_bytes().to_vec()).unwrap(),
            "# Plan\n\n  - indented, *Markdown* <b>and</b> +tokens tomorrow\n"
        );
        assert_eq!(normalise(b"one line\n".to_vec()).unwrap(), "one line");
        assert_eq!(normalise(b"no newline".to_vec()).unwrap(), "no newline");
        assert_eq!(
            normalise(b"  leading space".to_vec()).unwrap(),
            "  leading space"
        );
        assert_eq!(normalise(Vec::new()).unwrap(), "");
    }

    #[test]
    fn crlf_becomes_lf() {
        assert_eq!(
            normalise(b"one\r\ntwo\r\n\r\nthree\r\n".to_vec()).unwrap(),
            "one\ntwo\n\nthree"
        );
        // A lone CR is content, not a line ending, and is left alone.
        assert_eq!(normalise(b"a\rb".to_vec()).unwrap(), "a\rb");
    }

    #[test]
    fn invalid_utf8_is_refused_with_where() {
        let error = normalise(b"caf\xe9 au lait".to_vec()).unwrap_err();
        assert_eq!(error, DescriptionError::NotUtf8(3));
        assert!(error.to_string().contains("not UTF-8"), "{error}");
        assert!(error.to_string().contains("byte 3"), "{error}");
    }

    #[test]
    fn a_nul_is_refused() {
        let error = normalise(b"ELF\0\x01".to_vec()).unwrap_err();
        assert_eq!(error, DescriptionError::Nul(3));
        assert!(error.to_string().contains("NUL"), "{error}");
    }

    #[test]
    fn the_size_cap_is_exact() {
        assert!(normalise(vec![b'a'; MAX_BYTES]).is_ok());
        let error = normalise(vec![b'a'; MAX_BYTES + 1]).unwrap_err();
        assert_eq!(error, DescriptionError::TooLarge(MAX_BYTES + 1));
        assert!(error.to_string().contains("65,537 bytes"), "{error}");
        assert!(error.to_string().contains("65,536 (64 KiB)"), "{error}");
    }

    #[test]
    fn the_size_counts_paragraphs_and_characters() {
        assert_eq!(size("one"), "1 paragraph, 3 characters");
        assert_eq!(size("a"), "1 paragraph, 1 character");
        assert_eq!(
            size("first\nstill first\n\n\nsecond\n  \nthird é"),
            "3 paragraphs, 37 characters"
        );
        assert_eq!(size(&"x".repeat(1240)), "1 paragraph, 1,240 characters");
        assert_eq!(paragraphs("\n\n  \n"), 0);
    }

    #[test]
    fn the_first_line_is_cut_to_width() {
        assert_eq!(first_line("\n  \n  Title here  \nmore", 40), "Title here");
        assert_eq!(first_line("abcdefghij", 10), "abcdefghij");
        assert_eq!(first_line("abcd efghijk", 8), "abcd ef…");
        assert_eq!(first_line("abcd efgh", 6), "abcd…");
        assert_eq!(first_line("ééééééé", 4), "ééé…");
        assert_eq!(first_line("", 10), "");
    }

    #[test]
    fn thousands_are_grouped() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1000), "1,000");
        assert_eq!(thousands(1_234_567), "1,234,567");
    }
}
