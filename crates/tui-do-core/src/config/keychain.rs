//! The OS keychain as a credential source, and what "not there" has to mean precisely.
//!
//! This is the platform seam `CLAUDE.md` describes, in the shape the macOS port proved
//! works: a `cfg`-selected pair of functions rather than a trait. Phase 5's
//! `MarkdownRenderer` trait was dropped because rendering turned out to need no platform
//! knowledge, and `URL_OPENER` is two constants and a `cfg` — so a trait here would be a
//! second abstraction for one caller and one implementation. If a third platform ever
//! wants in, that is the moment to reach for one.
//!
//! **Nothing here reads a keychain yet.** Both arms answer [`Lookup::Unavailable`], which
//! is truthful on Linux today and a stub on macOS. Landing the seam and the outcomes
//! separately from the Security.framework calls is deliberate: this half compiles and is
//! testable on the Linux workstation the project is developed on, and the half that cannot
//! be is reduced to one function with a known signature.
//!
//! ## The four outcomes, which are not one outcome
//!
//! `md/2026-09-07-credential-storage-design.md` works out why "skipped when it is not
//! there" has to distinguish four cases, and the middle two are the trap:
//!
//! - **No service** — a headless box with no keychain at all. Fall through silently.
//! - **Present but locked** — fall through, *and say so once*. A locked ring that falls
//!   through silently to a file that does not exist produces "no config" when the true
//!   answer is "your keyring is locked", which the user cannot reach from the message.
//! - **Present, no entry** — a first run, or someone who keeps their token in a file.
//!   Fall through silently.
//! - **Present, entry, unreadable** — a real error. Report it and **do not** fall through:
//!   quietly substituting a different credential is how somebody ends up authenticated as
//!   the wrong account without noticing.

/// What asking the keychain for a token produced.
///
/// Deliberately not `Result<Option<String>>`: that shape has two states where this needs
/// five, and it was collapsing exactly these distinctions that the design note warns about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lookup {
    /// There is no keychain service to ask — the ordinary case on a headless box, and the
    /// only case on Linux today. Falls through silently.
    Unavailable,

    /// There is a keychain and it holds nothing for this account. Falls through silently.
    Absent,

    /// There is a keychain and it is locked. Falls through, and the caller says so once.
    Locked,

    /// A token.
    Found(String),

    /// There is an entry and it could not be read. **Not** a fall-through.
    Failed(String),
}

/// The service name every tui-do keychain entry is filed under.
pub const SERVICE: &str = "tui-do";

/// Read the token for `account` — the server URL — from the OS keychain.
///
/// **Keyed by server URL, not by a fixed name.** A person with a production server and a
/// development one has two tokens and needs both; a single `tui-do` entry would make
/// switching servers mean re-authenticating, and would silently hand the wrong token to
/// whichever config was loaded. The config already names exactly one server, so the URL is
/// the key that is always available and never ambiguous.
#[cfg(target_os = "macos")]
#[must_use]
pub fn read(_service: &str, _account: &str) -> Lookup {
    // The leaf the macOS session fills in: `keyring`, or Security.framework directly, and
    // the mapping from its errors onto the four outcomes above. Until then macOS behaves
    // exactly as Linux does and the file path still works, which is why this is a safe
    // thing to have landed unfinished.
    Lookup::Unavailable
}

/// No keychain on Linux, and that is a decision rather than an omission.
///
/// Measured 2026-09-08 and recorded in `md/2026-09-07-credential-storage-design.md`: the
/// pure-Rust Secret Service backend costs **53 net-new crates** against the present 412 and
/// pulls a *second* async runtime into a to-do client, because `zbus` keeps its `async-io`
/// defaults even with keyring's `tokio` feature on. Against that, the Secret Service is
/// frequently absent on the boxes tui-do actually runs on — every headless machine in the
/// fleet — and usually *locked* when present, since unlocking conventionally happens at
/// graphical login.
///
/// So the Linux half may well never be built, and that would be a fine outcome rather than
/// a gap. `token_file` is not a legacy path here; it is the ordinary one.
#[cfg(not(target_os = "macos"))]
#[must_use]
pub fn read(_service: &str, _account: &str) -> Lookup {
    Lookup::Unavailable
}

impl Lookup {
    /// Whether this outcome stops the search rather than falling through to the next source.
    ///
    /// Only [`Lookup::Failed`] does. A found token stops it by being an answer; the other
    /// three are all "ask the next source", and conflating `Failed` with them is the
    /// specific mistake the design note calls out.
    #[must_use]
    pub const fn is_fatal(&self) -> bool {
        matches!(*self, Self::Failed(_))
    }

    /// What to tell the user once, if anything.
    ///
    /// [`Lookup::Locked`] is the only outcome that is worth a line while still falling
    /// through: the others are either an answer, a real error the caller raises, or a
    /// silence that is correct.
    #[must_use]
    pub fn note(&self) -> Option<String> {
        match *self {
            Self::Locked => Some(
                "the OS keychain is locked, so it was skipped — unlock it, or set \
                 TUI_DO_API_TOKEN or server.token_file"
                    .to_string(),
            ),
            _ => None,
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    /// Nothing reads a keychain yet on either platform, and a test that says so is what
    /// stops "the keychain is where the credential lives" quietly becoming an assumption
    /// something depends on before it is true.
    #[test]
    fn no_platform_reads_a_keychain_yet() {
        assert_eq!(
            read(SERVICE, "https://vikunja.example.com"),
            Lookup::Unavailable
        );
    }

    /// The whole point of the enum: only an unreadable entry stops the search.
    #[test]
    fn only_an_unreadable_entry_stops_the_search() {
        assert!(Lookup::Failed("denied".to_string()).is_fatal());
        assert!(!Lookup::Unavailable.is_fatal());
        assert!(!Lookup::Absent.is_fatal());
        assert!(!Lookup::Locked.is_fatal());
        assert!(!Lookup::Found("tk_x".to_string()).is_fatal());
    }

    /// A locked ring falling through *silently* is the failure the design note names: the
    /// user is told "no config" when the answer is "unlock your keyring".
    #[test]
    fn a_locked_keychain_is_the_one_that_says_so() {
        assert!(Lookup::Locked.note().is_some());
        assert!(Lookup::Unavailable.note().is_none());
        assert!(Lookup::Absent.note().is_none());
        assert!(Lookup::Found("tk_x".to_string()).note().is_none());
    }
}
