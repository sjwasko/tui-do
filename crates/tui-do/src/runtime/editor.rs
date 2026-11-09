//! `tui-do add --edit`: write a description in the user's own editor, the way
//! `git commit` does.
//!
//! Launching an editor is platform-varying -- which variable names it, how its command
//! line is split, how it gets the terminal -- so it sits behind [`Editor`], with
//! [`ShellEditor`] as the Unix implementation the macOS port can keep or replace.
//! Everything around the launch (the temporary file, its header, reading it back) is
//! the same everywhere and lives here once.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use anyhow::Context as _;

/// Something that lets a person edit a file and says whether they meant it.
pub(crate) trait Editor {
    /// Edit `path` in place, returning once the person is done.
    ///
    /// # Errors
    /// The editor could not be started, or exited non-zero -- which is how a person
    /// says "cancel" to `git commit` (`:cq` in vim), and means the same here.
    fn edit(&self, path: &Path) -> anyhow::Result<()>;
}

/// The editor named by `$VISUAL`, else `$EDITOR`, else `vi`, run through `sh`.
///
/// Through `sh` because the variable is a command line, not a program name:
/// `EDITOR='code --wait'` is ordinary, and so is a quoted path with a space in it.
/// This is what git does -- `sh -c '$EDITOR "$@"' $EDITOR <file>` -- so whatever
/// works for `git commit` works here, and the file name is passed as an argument
/// rather than pasted into the command, so it is never re-parsed by the shell.
#[derive(Debug, Clone)]
pub(crate) struct ShellEditor {
    command: String,
}

impl ShellEditor {
    /// The editor this environment names.
    pub(crate) fn from_env() -> Self {
        Self {
            command: editor_command(std::env::var("VISUAL").ok(), std::env::var("EDITOR").ok()),
        }
    }
}

/// `$VISUAL`, else `$EDITOR`, else `vi`. An empty variable counts as unset, as git
/// treats it.
fn editor_command(visual: Option<String>, editor: Option<String>) -> String {
    [visual, editor]
        .into_iter()
        .flatten()
        .find(|command| !command.trim().is_empty())
        .unwrap_or_else(|| "vi".to_string())
}

impl Editor for ShellEditor {
    fn edit(&self, path: &Path) -> anyhow::Result<()> {
        // stdin, stdout and stderr are inherited: the editor gets the terminal itself.
        let status = std::process::Command::new("sh")
            .arg("-c")
            .arg(format!("{} \"$@\"", self.command))
            .arg(&self.command)
            .arg(path)
            .status()
            .with_context(|| format!("could not start the editor `{}`", self.command))?;
        if !status.success() {
            anyhow::bail!(
                "the editor `{}` {}, so nothing was added",
                self.command,
                match status.code() {
                    Some(code) => format!("exited with status {code}"),
                    None => "was stopped by a signal".to_string(),
                }
            );
        }
        Ok(())
    }
}

/// git's scissors line. Everything from it down belongs to the template, whatever the
/// person did to it, and everything above it is theirs.
///
/// A line rather than a marker on each help line, so a description can hold anything --
/// `# Heading` included, which is the first thing many people type in Markdown -- and an
/// edited help line cannot leak into it.
pub(crate) const SCISSORS: &str = "# ------------------------ >8 ------------------------";

/// What the editor's template says about the task.
pub(crate) struct Header {
    title: String,
    project: String,
}

impl Header {
    /// Made printable here, so no caller can forget: a title carrying a newline would
    /// otherwise end its line early, and could forge a scissors line of its own.
    pub(crate) fn new(title: &str, project: &str) -> Self {
        Self {
            title: super::printable(title),
            project: super::printable(project),
        }
    }

    /// The file `--edit` opens: an empty line to type on, then the scissors and the help.
    fn template(&self) -> String {
        format!(
            "\n{SCISSORS}\n\
             # Do not modify or remove the line above.\n\
             # Everything below it will be ignored.\n\
             #\n\
             # Description for \"{}\" in {}.\n\
             # Leave it empty to add no description. Exit the editor with an error\n\
             # (:cq in vim) to add nothing at all.\n",
            self.title, self.project
        )
    }
}

/// What `--edit` got back.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Composed {
    /// The description, checked like any other.
    pub(crate) text: String,
    /// The scissors line was gone, so the whole file was kept, help lines and all. The
    /// caller says so: keeping it is the safe direction, since cutting at a guess could
    /// throw away what the person wrote.
    pub(crate) scissors_missing: bool,
}

/// Everything above the scissors line, or the whole file if there is none.
///
/// Above it, blank lines at the start and blank lines just above the scissors go, as
/// `git commit` drops them: they are the template's empty first line and the gap a
/// person leaves before the help. Everything between is kept exactly. Without the
/// scissors nothing is trimmed, because there is no telling what is the template's.
fn cut(written: &str) -> (String, bool) {
    let lines: Vec<&str> = written.split('\n').collect();
    let Some(at) = lines.iter().position(|line| line.trim_end() == SCISSORS) else {
        return (written.to_string(), false);
    };
    let above = &lines[..at];
    let blank = |line: &&str| line.trim().is_empty();
    let start = above.iter().position(|line| !blank(line));
    let end = above.iter().rposition(|line| !blank(line));
    let text = match (start, end) {
        (Some(start), Some(end)) => above[start..=end].join("\n"),
        _ => String::new(),
    };
    (text, true)
}

/// A file only its owner can read, removed when this is dropped -- on success, on an
/// error, and on a panic that unwinds.
struct Scratch {
    path: PathBuf,
}

impl Scratch {
    /// Create a new, empty, owner-only file in the platform's temporary directory.
    fn create(contents: &str) -> anyhow::Result<Self> {
        let dir = std::env::temp_dir();
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos());
        let path = dir.join(format!(
            "tui-do-description-{}-{stamp}.md",
            std::process::id()
        ));
        let mut options = std::fs::OpenOptions::new();
        // `create_new` so a file someone else planted at that name is refused rather
        // than written through, and 0600 so the description is never readable by
        // another account, even for the minutes the editor is open.
        options.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let mut file = options
            .open(&path)
            .with_context(|| format!("could not create {}", path.display()))?;
        let scratch = Self { path };
        file.write_all(contents.as_bytes())
            .with_context(|| format!("could not write {}", scratch.path.display()))?;
        Ok(scratch)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Open `editor` on a template naming the task, and return the description written.
///
/// An empty string for an empty file: the person said "no description", which is not
/// a reason to refuse the task.
///
/// # Errors
/// The temporary file could not be made or read, the editor failed or exited non-zero,
/// or what was written is not a description `description::normalise` accepts.
pub(crate) fn compose(editor: &dyn Editor, header: &Header) -> anyhow::Result<Composed> {
    use tui_do_core::description::{normalise, DescriptionError};
    let scratch = Scratch::create(&header.template())?;
    editor.edit(&scratch.path)?;
    let written = std::fs::read(&scratch.path)
        .with_context(|| format!("could not read {}", scratch.path.display()))?;
    let written = String::from_utf8(written)
        .map_err(|error| DescriptionError::NotUtf8(error.utf8_error().valid_up_to()))?
        .replace("\r\n", "\n");
    let (body, found) = cut(&written);
    // The body, not the file, goes through the checks: the help below the scissors is
    // the template's, and must not count towards the cap.
    Ok(Composed {
        text: normalise(body.into_bytes())?,
        scissors_missing: !found,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
pub(crate) mod tests {
    use super::*;
    use std::cell::RefCell;

    /// An editor that does what a test says, and remembers the file it was given.
    pub(crate) struct FakeEditor {
        /// What to replace the file with, or `None` to leave it as the template.
        pub(crate) writes: Option<String>,
        /// Exit as `:cq` does.
        pub(crate) fails: bool,
        /// The path and the template it saw.
        pub(crate) saw: RefCell<Option<(PathBuf, String)>>,
    }

    impl FakeEditor {
        pub(crate) fn writing(text: &str) -> Self {
            Self {
                writes: Some(text.to_string()),
                fails: false,
                saw: RefCell::new(None),
            }
        }

        pub(crate) fn failing() -> Self {
            Self {
                writes: None,
                fails: true,
                saw: RefCell::new(None),
            }
        }
    }

    impl Editor for FakeEditor {
        fn edit(&self, path: &Path) -> anyhow::Result<()> {
            let template = std::fs::read_to_string(path).unwrap();
            *self.saw.borrow_mut() = Some((path.to_path_buf(), template));
            if self.fails {
                anyhow::bail!("the editor `fake` exited with status 1, so nothing was added");
            }
            if let Some(text) = &self.writes {
                std::fs::write(path, text).unwrap();
            }
            Ok(())
        }
    }

    #[test]
    fn visual_wins_then_editor_then_vi() {
        let some = |s: &str| Some(s.to_string());
        assert_eq!(
            editor_command(some("code --wait"), some("nano")),
            "code --wait"
        );
        assert_eq!(editor_command(None, some("nano")), "nano");
        assert_eq!(editor_command(some(""), some("nano")), "nano");
        assert_eq!(editor_command(some("  "), None), "vi");
        assert_eq!(editor_command(None, None), "vi");
    }

    fn header() -> Header {
        Header::new("Write the plan", "Work")
    }

    #[test]
    fn the_scissors_cut_and_markdown_headings_stay() {
        let template = header().template();
        let written = format!("# Goal\n\nShip it.\n\n\n## Steps\n- one\n  \n{template}");
        assert_eq!(
            cut(&written),
            ("# Goal\n\nShip it.\n\n\n## Steps\n- one".to_string(), true)
        );

        // Typed on the empty first line, as the cursor invites.
        let typed = template.replacen('\n', "Just this.\n", 1);
        assert_eq!(cut(&typed), ("Just this.".to_string(), true));

        // Leading blank lines go; leading spaces on a line with text stay.
        let indented = format!("\n \n\n    code\n{template}");
        assert_eq!(cut(&indented), ("    code".to_string(), true));

        // Untouched: no description.
        assert_eq!(cut(&template), (String::new(), true));
    }

    #[test]
    fn an_edited_header_below_the_scissors_does_not_leak() {
        let written = format!(
            "Body.\n{SCISSORS}\n# Description for \"something I changed\"\nI typed here too\n\n# more\n"
        );
        assert_eq!(cut(&written), ("Body.".to_string(), true));
        // Trailing spaces on the scissors line itself still count as the line.
        let spaced = format!("Body.\n{SCISSORS}   \nnot this");
        assert_eq!(cut(&spaced), ("Body.".to_string(), true));
    }

    #[test]
    fn a_deleted_scissors_line_keeps_the_whole_file() {
        let editor = FakeEditor::writing(
            "\nBody.\n# Do not modify or remove the line above.\n# Everything below it will be ignored.\n",
        );
        let got = compose(&editor, &header()).unwrap();
        assert_eq!(
            got,
            Composed {
                text: "\nBody.\n# Do not modify or remove the line above.\n# Everything below it will be ignored.".to_string(),
                scissors_missing: true,
            }
        );
    }

    #[test]
    fn a_title_with_a_newline_cannot_break_the_template() {
        let forged = Header::new(&format!("evil\n{SCISSORS}\nmore"), "Work\nX");
        let template = forged.template();
        assert_eq!(
            template.lines().filter(|line| *line == SCISSORS).count(),
            1,
            "{template}"
        );
        assert!(
            template.contains(&format!(
                "\"evil\u{fffd}{SCISSORS}\u{fffd}more\" in Work\u{fffd}X."
            )),
            "{template}"
        );
        // Left untouched it is still no description, and nothing of the title leaks.
        let untouched = FakeEditor {
            writes: None,
            fails: false,
            saw: RefCell::new(None),
        };
        assert_eq!(compose(&untouched, &forged).unwrap().text, "");
    }

    #[test]
    fn the_cap_applies_to_the_body_not_the_help() {
        let max = tui_do_core::description::MAX_BYTES;
        let template = header().template();
        let at_cap = FakeEditor::writing(&format!("{}{template}", "a".repeat(max)));
        assert_eq!(compose(&at_cap, &header()).unwrap().text.len(), max);
        let over = FakeEditor::writing(&format!("{}{template}", "a".repeat(max + 1)));
        let error = compose(&over, &header()).unwrap_err();
        assert!(error.to_string().contains("64 KiB"), "{error}");
    }

    #[test]
    fn the_header_names_the_task_and_explains_itself() {
        let text = Header::new("Call the VA", "Legal").template();
        assert!(
            text.starts_with(&format!("\n{SCISSORS}\n")),
            "the cursor's line comes first, then the scissors: {text:?}"
        );
        assert!(
            text.contains("Do not modify or remove the line above."),
            "{text}"
        );
        assert!(
            text.contains("Everything below it will be ignored."),
            "{text}"
        );
        assert!(text.contains("\"Call the VA\" in Legal"), "{text}");
        assert!(
            text.contains("Leave it empty to add no description"),
            "{text}"
        );
        assert!(text.contains(":cq"), "{text}");
    }

    #[test]
    fn compose_returns_what_the_editor_wrote_and_removes_the_file() {
        let editor = FakeEditor::writing(&format!(
            "Para one.\r\n\r\nPara two.\r\n\r\n{SCISSORS}\r\n# junk\r\n"
        ));
        let got = compose(&editor, &header()).unwrap();
        assert_eq!(
            got,
            Composed {
                text: "Para one.\n\nPara two.".to_string(),
                scissors_missing: false,
            }
        );

        let (path, template) = editor.saw.borrow().clone().unwrap();
        assert!(
            template.contains("\"Write the plan\" in Work"),
            "{template}"
        );
        assert!(path.starts_with(std::env::temp_dir()), "{}", path.display());
        assert!(!path.exists(), "the temporary file was left behind");
    }

    #[cfg(unix)]
    #[test]
    fn the_temporary_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt as _;
        struct Mode(RefCell<u32>);
        impl Editor for Mode {
            fn edit(&self, path: &Path) -> anyhow::Result<()> {
                *self.0.borrow_mut() = std::fs::metadata(path)?.permissions().mode();
                Ok(())
            }
        }
        let editor = Mode(RefCell::new(0));
        compose(&editor, &header()).unwrap();
        assert_eq!(*editor.0.borrow() & 0o777, 0o600);
    }

    #[test]
    fn a_failed_editor_is_an_error_and_still_removes_the_file() {
        let editor = FakeEditor::failing();
        let error = compose(&editor, &header()).unwrap_err();
        assert!(error.to_string().contains("nothing was added"), "{error}");
        let (path, _) = editor.saw.borrow().clone().unwrap();
        assert!(!path.exists(), "the temporary file was left behind");
    }

    #[test]
    fn what_the_editor_writes_is_checked_like_any_description() {
        let editor = FakeEditor {
            writes: None,
            fails: false,
            saw: RefCell::new(None),
        };
        // Left as the template: no description, not an error.
        assert_eq!(compose(&editor, &header()).unwrap().text, "");

        struct Bytes;
        impl Editor for Bytes {
            fn edit(&self, path: &Path) -> anyhow::Result<()> {
                std::fs::write(path, b"caf\xe9")?;
                Ok(())
            }
        }
        let error = compose(&Bytes, &header()).unwrap_err();
        assert!(error.to_string().contains("not UTF-8"), "{error}");
    }

    /// The real launch, through `sh`, with an `$EDITOR` that carries an argument. No
    /// terminal is needed for this part: the script never reads one.
    #[cfg(unix)]
    #[test]
    fn the_shell_editor_splits_the_command_as_git_does() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = std::env::temp_dir().join(format!("tui-do-editor-sh-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("my editor");
        std::fs::write(
            &script,
            "#!/bin/sh\n# $1 is the flag, $2 the file.\nprintf 'flag=%s\\n' \"$1\" > \"$2\"\n",
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let editor = ShellEditor {
            command: format!("'{}' --wait", script.display()),
        };
        // The script replaced the whole file, scissors and all, so all of it is kept.
        assert_eq!(
            compose(&editor, &header()).unwrap(),
            Composed {
                text: "flag=--wait".to_string(),
                scissors_missing: true,
            }
        );

        let failing = ShellEditor {
            command: "false".to_string(),
        };
        let error = compose(&failing, &header()).unwrap_err();
        assert!(
            error.to_string().contains("exited with status 1"),
            "{error}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
