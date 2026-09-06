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
# Osaka Jade, the Omarchy theme, transcribed from
# ~/.local/share/omarchy/themes/osaka-jade/colors.toml. VHS has no built-in for it, so the
# palette is given inline. Verified by rendering and sampling the background pixel: #101c18
# against the theme's #111c18, a one-step shift from GIF's 256-colour quantisation.
#
# One deliberate substitution: the theme's own `yellow` is #459451, which is a green. Using
# it as ANSI yellow would render an overdue date and a high priority in green, which is the
# opposite of what those colours are for -- so ANSI yellow takes the theme's `bright_yellow`
# instead, and brightYellow takes `bright_foreground`. Every value is still Osaka Jade's.
Set Theme { "name": "Osaka Jade", "background": "#111c18", "foreground": "#C1C497", "black": "#0c1512", "red": "#FF5345", "green": "#549e6a", "yellow": "#E5C736", "blue": "#509475", "magenta": "#D2689C", "cyan": "#2DD5B7", "white": "#C1C497", "brightBlack": "#53685B", "brightRed": "#db9f9c", "brightGreen": "#63b07a", "brightYellow": "#F7E8B2", "brightBlue": "#ACD4CF", "brightMagenta": "#75bbb3", "brightCyan": "#8CD3CB", "brightWhite": "#D6D5BC", "selection": "#32473B", "cursor": "#2DD5B7" }
Set TypingSpeed 60ms

# Two panes: the interface on the left, a shell on the right, so a task written from the
# command line can be seen arriving in the interface without cutting away.
#
# The panes deliberately use *different* stores. The right-hand shell writes to the server
# and the left-hand interface pulls it back with `r`, which is what actually happens when a
# second machine or an agent files something -- and it keeps two processes off one SQLite
# file.
Hide
Type "tmux kill-session -t demo 2>/dev/null; tmux new-session -d -s demo"   Enter
Type "tmux set -g status off"                                              Enter
Type "tmux split-window -h -t demo -l 42%"                                 Enter
Type "tmux send-keys -t demo:0.0 'export TUI_DO_CONFIG=~/.config/tui-do/demo.yaml TUI_DO_DB=~/.local/share/tui-do/demo.db; clear' Enter"  Enter
Type "tmux send-keys -t demo:0.1 'export TUI_DO_CONFIG=~/.config/tui-do/demo.yaml TUI_DO_DB=/tmp/tui-do-agent.db; clear' Enter"           Enter
Type "tmux select-pane -t demo:0.0"                                        Enter
Type "clear"                                                               Enter
Type "tmux attach -t demo"                                                 Enter
Sleep 1s
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

# --- the Agents project ------------------------------------------------------
Type "gp"
Sleep 1200ms
Type@120ms "Agents"
Sleep 800ms
Enter
Sleep 7s

# --- a task filed from the shell, arriving in the interface ------------------
Ctrl+b
Type "o"
Sleep 1200ms
Type@75ms "tui-do add 'Summarise the sync engine backoff policy +Agents *research !3 tomorrow'"
Sleep 1200ms
Enter
Sleep 3s

Ctrl+b
Type "o"
Sleep 1200ms
# `r` pulls what changed. The new task appears without the interface being restarted.
Type "r"
Sleep 4s

# --- back to All tasks, then the pane toggles -------------------------------
# `z s` and `z p` are worth seeing on a full list rather than on an empty project.
Tab
Sleep 700ms
Type "gg"
Sleep 700ms
Enter
Sleep 1500ms
Tab
Sleep 1s

Type "zp"
Sleep 2s
Type "zp"
Sleep 1500ms
Type "zs"
Sleep 2s
Type "zs"
Sleep 1500ms

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

Hide
Type "tmux kill-session -t demo"  Enter
Show
