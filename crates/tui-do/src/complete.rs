//! Shell completion of quick-add names: `tui-do add Call the VA +Le<Tab>` offers `+Legal`.
//!
//! The shell runs `tui-do` itself on every Tab, with `TUI_DO_COMPLETE` naming the shell and
//! the words of the line after a `--` (clap_complete's `CompleteEnv`). What a word can
//! become is decided by `tui_do_ui::assist`, the same rules the `tui-do add` prompt and the
//! `a` prompt use; this module only turns its answer into whole shell words.
//!
//! The store is opened read-only, never created, migrated or synced, and nothing here
//! touches the network: a shell calls this on every Tab and must not wait on a server.
//! No store means no names, silently, because a completion has nowhere to say anything.

use std::ffi::{OsStr, OsString};
use std::path::Path;

use clap_complete::engine::{ArgValueCompleter, CompletionCandidate};
use clap_complete::env::CompleteEnv;
use tui_do_ui::assist::{self, Vocabulary};

/// The variable that turns a run into a completion request, and names the shell.
///
/// Not clap_complete's default `COMPLETE`: that name is generic enough to be set in
/// somebody's environment for another reason, and then every `tui-do` run would answer
/// as a completer instead of doing its job.
pub const VAR: &str = "TUI_DO_COMPLETE";

/// How many words before the cursor the completer reads. Enough for `due next w` and a
/// bracketed name a few words long; not so many that a description given with `-d`
/// earlier in the line can open a bracket the task's text never did.
const CONTEXT: usize = 6;

/// Answer a completion request and exit, if this run is one; otherwise return at once.
///
/// Called first in `main`, before anything else can write to stdout.
pub fn handle(command: impl Fn() -> clap::Command) {
    // Read before `CompleteEnv` runs: it removes the variable before building the command.
    let bash = std::env::var_os(VAR).is_some_and(|shell| shell == "bash");
    CompleteEnv::with_factory(|| with_completer(command(), previous_words(), bash))
        .var(VAR)
        .complete();
}

/// `command` with `tui-do add`'s text completing names from the local store.
fn with_completer(command: clap::Command, previous: Vec<String>, bash: bool) -> clap::Command {
    command.mut_subcommand("add", |add| {
        add.mut_arg("text", |text| {
            text.add(ArgValueCompleter::new(move |current: &OsStr| {
                store_names(&previous, current, bash)
            }))
        })
    })
}

/// `word` escaped for bash, which inserts a candidate into the line exactly as given.
///
/// zsh and fish quote a candidate themselves; bash does not, so `+[A  B]` went in as two
/// words, and `add`, joining its words with one space, filed the task in `A B` -- a
/// project that does not exist. Measured in bash 5.3 under a pty: the escaped form arrives
/// as the single argument `+[A  B]`.
///
/// What the user already typed is left as typed, because bash replaces the word with the
/// longest prefix the candidates share: escaping `*ur` to `\*ur…` turned `*ur` Tab into
/// `\*u`, losing the `r`. Everything after it that a shell would read specially is
/// backslashed.
fn for_bash(current: &str, word: &str) -> String {
    let typed = current
        .char_indices()
        .zip(word.chars())
        .find(|((_, a), b)| a != b)
        .map_or_else(|| current.len().min(word.len()), |((at, _), _)| at);
    let mut out = word[..typed].to_string();
    for c in word[typed..].chars() {
        if !(c.is_alphanumeric() || "+-_.,/@%^:=".contains(c)) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// The words of the line before the one being completed, as the shell handed them over.
///
/// clap's completer is told only the current word, and a date after `due ` needs the word
/// before it, so they are read here from the same arguments clap reads: everything after
/// `--`, up to the index bash and zsh pass in `_CLAP_COMPLETE_INDEX`, or all but the last
/// for fish, which passes the current word last.
fn previous_words() -> Vec<String> {
    let args: Vec<OsString> = std::env::args_os().collect();
    let index = std::env::var("_CLAP_COMPLETE_INDEX")
        .ok()
        .and_then(|index| index.parse().ok());
    words_before(&args, index)
}

/// [`previous_words`] from `args` (the whole command line, completer first) and the
/// shell's index of the current word, if it gave one.
fn words_before(args: &[OsString], index: Option<usize>) -> Vec<String> {
    let line = args
        .iter()
        .position(|arg| arg == "--")
        .map_or(&[][..], |at| &args[at + 1..]);
    let current = index.unwrap_or_else(|| line.len().saturating_sub(1));
    line.iter()
        .take(current)
        // The program's own name is not part of the task.
        .skip(1)
        .map(|word| word.to_string_lossy().into_owned())
        .collect()
}

/// The candidates for `current` from the store `TUI_DO_DB` names, or the default one.
fn store_names(previous: &[String], current: &OsStr, bash: bool) -> Vec<CompletionCandidate> {
    let Some(current) = current.to_str() else {
        return Vec::new();
    };
    let Ok(path) = crate::runtime::store_path() else {
        return Vec::new();
    };
    names_at(&path, previous, current)
        .into_iter()
        .map(|word| CompletionCandidate::new(if bash { for_bash(current, &word) } else { word }))
        .collect()
}

/// The whole words `current` can become, from the store at `store_path`, read-only.
///
/// No store, or one that cannot be read, gives the date words alone: they need no store.
pub fn names_at(store_path: &Path, previous: &[String], current: &str) -> Vec<String> {
    let vocabulary = crate::runtime::store_only_runtime().map_or_else(
        |_| Vocabulary::default(),
        |runtime| runtime.block_on(crate::runtime::vocabulary_at(store_path)),
    );
    words(previous, current, &vocabulary)
}

/// What `current` can become, as whole shell words, given the words before it.
///
/// `assist::candidates` answers for a line, and a shell replaces only the word under the
/// cursor, so each candidate is cut to that word: `+Le` becomes `+Legal`, and `w` after
/// `due next` becomes `week` -- the `next ` is already on the line. A candidate that does
/// not begin with what the earlier words already say is dropped rather than offered.
///
/// A name holding a control character is dropped too: it came from a server, possibly
/// from somebody else, and the shell would put it straight into the user's command line.
#[must_use]
pub fn words(previous: &[String], current: &str, vocabulary: &Vocabulary) -> Vec<String> {
    let context = &previous[previous.len().saturating_sub(CONTEXT)..];
    let mut before = context.join(" ");
    if !before.is_empty() {
        before.push(' ');
    }
    let word_start = before.len();
    before.push_str(current);
    let Some(found) = assist::candidates(&before, vocabulary) else {
        return Vec::new();
    };
    found
        .tokens
        .into_iter()
        .filter(|token| !token.chars().any(char::is_control))
        .filter_map(|token| {
            if found.start >= word_start {
                Some(format!("{}{token}", &current[..found.start - word_start]))
            } else {
                let already = &before[found.start..word_start];
                token
                    .get(..already.len())
                    .filter(|head| head.eq_ignore_ascii_case(already))
                    .map(|_| token[already.len()..].to_string())
            }
        })
        .filter(|word| !word.is_empty())
        .collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use tui_do_core::models::{Label, LabelId, Project, ProjectId};
    use tui_do_core::Store;

    use super::*;

    fn vocabulary() -> Vocabulary {
        Vocabulary {
            projects: vec!["Dinner Places".into(), "Legal".into(), "Life Admin".into()],
            labels: vec!["health".into(), "urgent".into()],
            users: vec!["admin".into()],
        }
    }

    fn previous(line: &str) -> Vec<String> {
        line.split_whitespace().map(String::from).collect()
    }

    #[test]
    fn a_sigil_word_completes_to_whole_names() {
        let names = vocabulary();
        assert_eq!(
            words(&previous("add Call the VA"), "+Le", &names),
            ["+Legal"]
        );
        assert_eq!(
            words(&previous("add"), "+L", &names),
            ["+Legal", "+[Life Admin]"]
        );
        assert_eq!(words(&previous("add x"), "*ur", &names), ["*urgent"]);
        assert_eq!(words(&previous("add x"), "@a", &names), ["@admin"]);
        assert_eq!(
            words(&previous("add x"), "+Di", &names),
            ["+[Dinner Places]"]
        );
    }

    #[test]
    fn a_plain_word_completes_to_nothing() {
        assert!(words(&previous("add Call the"), "V", &vocabulary()).is_empty());
        assert!(words(&previous("add"), "", &vocabulary()).is_empty());
    }

    #[test]
    fn a_date_completes_after_due_and_start() {
        let names = vocabulary();
        assert_eq!(
            words(&previous("add Call due"), "tom", &names),
            ["tomorrow"]
        );
        assert_eq!(
            words(&previous("add Call start"), "eo", &names),
            ["eow", "eom", "eoy"]
        );
        // The shell has already split `next` off; only the word under the cursor is
        // replaced, so the candidate is the rest of the phrase.
        assert_eq!(
            words(&previous("add Call due next"), "we", &names),
            ["week", "weekend", "wednesday"]
        );
    }

    #[test]
    fn a_bracket_opened_in_an_earlier_word_completes_the_rest() {
        assert_eq!(
            words(&previous("add Book +[Dinner"), "Pl", &vocabulary()),
            ["Places]"]
        );
    }

    #[test]
    fn bash_gets_a_spaced_name_as_one_escaped_word() {
        assert_eq!(for_bash("+A", "+[A  B]"), r"+\[A\ \ B\]");
        assert_eq!(for_bash("+Li", "+[Life Admin]"), r"+\[Life\ Admin\]");
        assert_eq!(for_bash("tom", "tomorrow"), "tomorrow");
        assert_eq!(for_bash("ne", "next week"), r"next\ week");
        // What was typed stays as typed, or bash's shared prefix would cut it short.
        assert_eq!(for_bash("*ur", "*urgent"), "*urgent");
        assert_eq!(for_bash("", "*urgent"), r"\*urgent");
        assert_eq!(for_bash("Pl", "Places]"), r"Places\]");
        assert_eq!(for_bash("+O", "+[O'Neil & Co]"), r"+\[O\'Neil\ \&\ Co\]");
    }

    #[test]
    fn a_name_carrying_a_control_character_is_not_offered() {
        let names = Vocabulary {
            projects: vec!["Work\u{1b}]52;c;cGF5\u{7}".into(), "Workshop".into()],
            ..Vocabulary::default()
        };
        assert_eq!(words(&[], "+Wo", &names), ["+Workshop"]);
    }

    #[test]
    fn the_words_before_are_read_from_after_the_separator() {
        let args: Vec<OsString> = ["/bin/tui-do", "--", "tui-do", "add", "x", "due", "to"]
            .map(OsString::from)
            .to_vec();
        // bash and zsh say which word is current, counting the program as 0.
        assert_eq!(words_before(&args, Some(4)), ["add", "x", "due"]);
        // fish passes the current word last.
        assert_eq!(words_before(&args, None), ["add", "x", "due"]);
    }

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("tui-do-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Seeded on a runtime of its own, then completed with none running, as a shell's
    /// call would be.
    #[test]
    fn names_come_from_a_seeded_store() {
        let dir = scratch("complete-seeded");
        let path = dir.join("tui-do.db");
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let store = Store::open(&path).await.unwrap();
            store
                .upsert_projects(vec![Project {
                    id: ProjectId(4),
                    title: "Legal".to_string(),
                    ..Project::default()
                }])
                .await
                .unwrap();
            store
                .upsert_labels(vec![Label {
                    id: LabelId(1),
                    title: "needs review".to_string(),
                    ..Label::default()
                }])
                .await
                .unwrap();
        });

        assert_eq!(
            names_at(&path, &previous("add Call the VA"), "+Le"),
            ["+Legal"]
        );
        assert_eq!(
            names_at(&path, &previous("add Call the VA"), "*ne"),
            ["*[needs review]"]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_store_gives_no_names_and_creates_none() {
        let dir = scratch("complete-missing");
        let path = dir.join("tui-do.db");
        assert!(names_at(&path, &previous("add x"), "+Le").is_empty());
        assert!(!path.exists(), "completing created a store");
        assert_eq!(
            std::fs::read_dir(&dir).unwrap().count(),
            0,
            "completing left a file behind"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
