# tui-do — scripted demo recording.
#
#   vhs docs/demo/tui-do.tape
#
# Records against the *demo* profile: a dev-instance account holding synthetic data
# only, and its own store. Neither the real store nor real task data is reachable from
# here, which is the point -- a screenshot of a real backlog publishes it permanently.
#
# The script is committed so the recording can be regenerated when the interface
# changes. A GIF cannot tell you it has gone stale; a tape re-run can.

Output docs/demo/tui-do.gif

Set Shell bash
Set FontSize 15
Set Width 1440
Set Height 810
Set Padding 18
Set Theme "Catppuccin Mocha"
Set TypingSpeed 60ms

# The demo profile. TUI_DO_DB keeps this away from the real store.
Hide
Type "export TUI_DO_CONFIG=~/.config/tui-do/demo.yaml"  Enter
Type "export TUI_DO_DB=~/.local/share/tui-do/demo.db"   Enter
Type "clear"                                            Enter
Show

Type "tui-do"  Sleep 500ms  Enter

# Let the first sync land before anything is filmed.
Sleep 4s

# --- the list ---------------------------------------------------------------
Down@400ms 4
Sleep 1s
Up@400ms 2
Sleep 1s

# --- open a task, show the rendered description -----------------------------
Enter
Sleep 2500ms
Escape
Sleep 800ms

# --- quick-add: one line sets project, label, priority and a date -----------
Type "a"
Sleep 700ms
Type@70ms "Draft the quarterly summary +Work *urgent !4 friday"
Sleep 1500ms
Enter
Sleep 2500ms

# --- complete something, then undo it ---------------------------------------
Down@400ms 2
Sleep 700ms
Type "d"
Sleep 1500ms
Type "u"
Sleep 1500ms

# --- search ------------------------------------------------------------------
Type "/"
Sleep 500ms
Type@80ms "release"
Sleep 1800ms
Escape
Sleep 700ms

# --- jump to a project -------------------------------------------------------
Type "gp"
Sleep 1200ms
Down@400ms 2
Enter
Sleep 2500ms

# --- column layouts ----------------------------------------------------------
Type "L"
Sleep 1800ms
Type "L"
Sleep 1800ms

# --- the keymap --------------------------------------------------------------
Type "?"
Sleep 3s
Escape
Sleep 800ms

Type "q"
Sleep 1s
