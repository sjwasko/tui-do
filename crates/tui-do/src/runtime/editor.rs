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

/// The lines `--edit` puts under an empty first line, and removes again.
///
/// Only these exact lines are removed, not every line starting with `#`: a description
/// is often Markdown, and `# Heading` is the first thing many people would type.
fn header(title: &str, project: &str) -> Vec<String> {
    vec![
        format!("# Description for \"{title}\" in {project}."),
        "# These # lines are removed; Markdown headings you write are kept.".to_string(),
        "# Save an empty file to add no description. Exit the editor with an error".to_string(),
        "# (:cq in vim) to add nothing at all.".to_string(),
    ]
}

/// The file `--edit` opens: an empty line to type on, then the header.
fn template(title: &str, project: &str) -> String {
    let mut text = String::from("\n");
    for line in header(title, project) {
        text.push_str(&line);
        text.push('\n');
    }
    text
}

/// What the person wrote, with the header taken back out.
///
/// Leading and trailing blank lines go too, as `git commit` drops them: they are the
/// template's, not the person's -- the empty first line, and the gap left where the
/// header was. Everything between is kept exactly. CRLF and invalid UTF-8 are left to
/// `description::normalise`, which every way in shares.
fn strip_header(written: &str, title: &str, project: &str) -> String {
    let header = header(title, project);
    let lines: Vec<&str> = written
        .split('\n')
        .filter(|line| {
            !header
                .iter()
                .any(|ours| ours == line.trim_end_matches('\r'))
        })
        .collect();
    let blank = |line: &&str| line.trim().is_empty();
    let start = lines.iter().position(|line| !blank(line));
    let end = lines.iter().rposition(|line| !blank(line));
    match (start, end) {
        (Some(start), Some(end)) => lines[start..=end].join("\n"),
        _ => String::new(),
    }
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
pub(crate) fn compose(editor: &dyn Editor, title: &str, project: &str) -> anyhow::Result<String> {
    let scratch = Scratch::create(&template(title, project))?;
    editor.edit(&scratch.path)?;
    let written = std::fs::read(&scratch.path)
        .with_context(|| format!("could not read {}", scratch.path.display()))?;
    // Checked before the header comes out, so a cap or a bad byte is reported against
    // what the person actually wrote.
    let written = tui_do_core::description::normalise(written)?;
    Ok(strip_header(&written, title, project))
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

    #[test]
    fn the_header_comes_out_and_markdown_headings_stay() {
        let title = "Write the plan";
        let project = "Work";
        let mut written = String::from("# Goal\n\nShip it.\n\n## Steps\n- one\n");
        written.push_str(&template(title, project));
        assert_eq!(
            strip_header(&written, title, project),
            "# Goal\n\nShip it.\n\n## Steps\n- one"
        );

        // Typed on the empty first line, as the cursor invites.
        let typed = template(title, project).replacen('\n', "Just this.\n", 1);
        assert_eq!(strip_header(&typed, title, project), "Just this.");

        // Untouched, or emptied: no description.
        assert_eq!(strip_header(&template(title, project), title, project), "");
        assert_eq!(strip_header("", title, project), "");

        // A `#` line the person wrote that only looks like ours is theirs.
        let lookalike = "# Description for something else.\nbody";
        assert_eq!(strip_header(lookalike, title, project), lookalike);
    }

    #[test]
    fn the_header_names_the_task_and_explains_itself() {
        let text = template("Call the VA", "Legal");
        assert!(
            text.starts_with('\n'),
            "the cursor's line comes first: {text:?}"
        );
        assert!(text.contains("\"Call the VA\" in Legal"), "{text}");
        assert!(
            text.contains("Save an empty file to add no description"),
            "{text}"
        );
        assert!(text.contains(":cq"), "{text}");
    }

    #[test]
    fn compose_returns_what_the_editor_wrote_and_removes_the_file() {
        let editor =
            FakeEditor::writing("Para one.\r\n\r\nPara two.\n\n# Description for \"t\" in P.\n");
        let got = compose(&editor, "t", "P").unwrap();
        assert_eq!(got, "Para one.\n\nPara two.");

        let (path, template) = editor.saw.borrow().clone().unwrap();
        assert!(template.contains("\"t\" in P"), "{template}");
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
        compose(&editor, "t", "P").unwrap();
        assert_eq!(*editor.0.borrow() & 0o777, 0o600);
    }

    #[test]
    fn a_failed_editor_is_an_error_and_still_removes_the_file() {
        let editor = FakeEditor::failing();
        let error = compose(&editor, "t", "P").unwrap_err();
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
        assert_eq!(compose(&editor, "t", "P").unwrap(), "");

        struct Bytes;
        impl Editor for Bytes {
            fn edit(&self, path: &Path) -> anyhow::Result<()> {
                std::fs::write(path, b"caf\xe9")?;
                Ok(())
            }
        }
        let error = compose(&Bytes, "t", "P").unwrap_err();
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
        assert_eq!(compose(&editor, "t", "P").unwrap(), "flag=--wait");

        let failing = ShellEditor {
            command: "false".to_string(),
        };
        let error = compose(&failing, "t", "P").unwrap_err();
        assert!(
            error.to_string().contains("exited with status 1"),
            "{error}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
