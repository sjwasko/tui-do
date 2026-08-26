"""Does `r` surface a task added by `criax add` into the project on screen?

Scope is removed as a variable: the TUI is landed on a uniquely-named project and
the task is added to that same project by name.
"""

import fcntl
import os
import select
import sqlite3
import struct
import subprocess
import sys
import termios
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from vt import Screen

DB = os.path.expanduser("~/.local/share/criax/criax.db")
BIN = "./target/release/criax"
PROJECT = "Mileage"
PROJECT_ID = 30
COLS, ROWS = 120, 40
PROBE = f"REFRESH PROBE {int(time.time())}"


def db(query, args=()):
    con = sqlite3.connect(f"file:{DB}?mode=ro", uri=True, timeout=5)
    try:
        return con.execute(query, args).fetchall()
    finally:
        con.close()


class Tui:
    def __init__(self):
        self.master, slave = os.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLS, 0, 0))
        self.proc = subprocess.Popen(
            [BIN], stdin=slave, stdout=slave, stderr=slave, close_fds=True,
            env={**os.environ, "TERM": "xterm-256color"},
        )
        os.close(slave)
        self.screen = Screen(COLS, ROWS)

    def pump(self, seconds):
        end = time.time() + seconds
        while time.time() < end:
            r, _, _ = select.select([self.master], [], [], 0.2)
            if r:
                try:
                    data = os.read(self.master, 65536)
                except OSError:
                    return
                if not data:
                    return
                self.screen.feed(data.decode("utf-8", "replace"))

    def send(self, keys):
        os.write(self.master, keys.encode())

    def wait_loaded(self, limit=120):
        end = time.time() + limit
        while time.time() < end:
            self.pump(1)
            text = self.screen.text()
            if "Loading" not in text and "tasks" in text:
                return True
        return False

    def close(self):
        self.send("q")
        self.pump(10)
        try:
            self.proc.wait(timeout=15)
        except subprocess.TimeoutExpired:
            self.proc.kill()


def report(name, screen_text):
    lines = [l for l in screen_text.split("\n")]
    print(f"\n----- {name} -----")
    print(f"  breadcrumb: {lines[0].strip()[:90]}")
    body = [l for l in lines if l.strip()]
    for line in body[:14]:
        print(f"  |{line[:110]}")
    print(f"  probe on screen: {PROBE in screen_text}")
    return PROBE in screen_text


print(f"landing the TUI on {PROJECT} (#{PROJECT_ID}); probe = {PROBE!r}")
tui = Tui()
if not tui.wait_loaded():
    print("the list never loaded"); tui.close(); sys.exit(2)
tui.pump(3)
before = tui.screen.text()
report("BEFORE the add", before)

print(f"\nadding to +{PROJECT} from a shell, while the TUI runs")
out = subprocess.run(
    [BIN, "add", f"{PROBE} +{PROJECT}"], capture_output=True, text=True
)
print("  " + out.stdout.strip().replace("\n", "\n  "))
rows = db("select id, project_id, title from tasks where title like ?", (f"{PROBE}%",))
print(f"  in the store: {rows}")

last_pull_before = db("select value from sync_state where key='last_pull'")
print(f"\npressing r; last_pull was {last_pull_before}")
tui.send("r")
end = time.time() + 180
while time.time() < end:
    tui.pump(2)
    if db("select value from sync_state where key='last_pull'") != last_pull_before:
        print("  the pull finished")
        break
tui.pump(6)
# the new task sorts to the end of the list, so go there before looking
tui.send("G")
tui.pump(3)
after = tui.screen.text()
seen_after_r = report("AFTER r", after)

tui.close()
print("\nrelaunching")
tui2 = Tui()
tui2.wait_loaded()
tui2.pump(3)
tui2.send("G")
tui2.pump(3)
seen_after_restart = report("AFTER restart", tui2.screen.text())
tui2.close()

print("\n=======================================")
print(f"visible after r:       {seen_after_r}")
print(f"visible after restart: {seen_after_restart}")
if seen_after_restart and not seen_after_r:
    print("REPRODUCED: a restart shows what r does not.")
elif seen_after_r:
    print("r did surface it.")
