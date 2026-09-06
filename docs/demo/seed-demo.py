#!/usr/bin/env python3
"""Reset the demo account to a known state, so a recording is reproducible.

Every VHS run mutates the server: the tape's quick-add really creates a task and its
`d` really completes one. Without this, the second take differs from the first and the
tape stops being the deterministic thing it exists to be. Run this before recording.

Destructive: it deletes every project, label and task the account can see. Point it at a
throwaway account holding synthetic data, never at anyone's real task list.

    export TUI_DO_DEMO_URL=https://vikunja.example.com
    export TUI_DO_DEMO_TOKEN_FILE=~/.config/tui-do/demo-token
    python3 docs/demo/seed-demo.py

No hostname is baked in on purpose; this file is public and the server is not.
"""
import datetime
import json
import os
import sys
import urllib.error
import urllib.request

URL = os.environ.get("TUI_DO_DEMO_URL", "").rstrip("/")
TOKEN_FILE = os.path.expanduser(os.environ.get("TUI_DO_DEMO_TOKEN_FILE", ""))
if not URL or not TOKEN_FILE:
    sys.exit("set TUI_DO_DEMO_URL and TUI_DO_DEMO_TOKEN_FILE; see the docstring")
BASE = URL if URL.endswith("/api/v1") else URL + "/api/v1"
TOKEN = open(TOKEN_FILE).read().strip()


def call(method, path, body=None, tolerate=()):
    """Call the API. `tolerate` lists Vikunja error codes to return None for."""
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(
        BASE + path, data=data, method=method,
        headers={"Authorization": "Bearer " + TOKEN, "Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(req) as r:
            raw = r.read()
    except urllib.error.HTTPError as e:
        detail = e.read().decode()
        try:
            code = json.loads(detail).get("code")
        except ValueError:
            code = None
        if code in tolerate:
            return None
        sys.exit(f"{method} {path} -> {e.code}: {detail[:200]}")
    return json.loads(raw) if raw else None


def day(offset):
    d = datetime.datetime.now(datetime.timezone.utc).replace(
        hour=17, minute=0, second=0, microsecond=0)
    return (d + datetime.timedelta(days=offset)).strftime("%Y-%m-%dT%H:%M:%SZ")


# --- wipe -------------------------------------------------------------------
# Deleting a project takes its tasks with it. Pseudo-projects (Favorites and friends)
# carry negative ids and reject writes, so they are skipped rather than attempted.
# 3012 is "this is the user's default project", which Vikunja refuses to delete. That one
# is emptied by the task sweep below instead of being removed.
for project in call("GET", "/projects") or []:
    if project["id"] > 0:
        call("DELETE", f"/projects/{project['id']}", tolerate=(3012,))
for task in call("GET", "/tasks?per_page=200") or []:
    call("DELETE", f"/tasks/{task['id']}")
for label in call("GET", "/labels") or []:
    call("DELETE", f"/labels/{label['id']}")

# --- rebuild ----------------------------------------------------------------
projects = {t: call("PUT", "/projects", {"title": t})["id"]
            for t in ["Work", "Home", "Reading", "Errands", "Agents"]}
labels = {t: call("PUT", "/labels", {"title": t, "hex_color": c})["id"]
          for t, c in [("urgent", "e74c3c"), ("waiting", "f39c12"),
                       ("quick", "27ae60"), ("research", "3498db")]}

RENDER_DEMO = """## Acceptance criteria

- Renders **bold**, *italic* and `inline code`
- Keeps a single newline as a line break
- Leaves `Vec<String>` alone instead of eating the angle brackets

See the [design notes](https://example.com/design) for the reasoning."""

AGENT_RULES = """Runs unattended, so the output has to be checkable.

- Post the summary as a comment, do not edit the task body
- Stop and ask if more than **five** items would change
- Never create labels: a typo becomes a permanent global entry"""

TASKS = [
    ("Work", "Write the migration guide for the new API", 3, 2, False, ["research"], RENDER_DEMO),
    ("Work", "Review the pagination fix", 4, 0, False, ["urgent"], "Check the header handling on page 2 and beyond."),
    ("Work", "Reply to the vendor about licensing", 2, -1, False, ["waiting"], ""),
    ("Work", "Rotate the staging credentials", 5, -3, False, ["urgent"], "Overdue on purpose, to show how that reads."),
    ("Work", "Draft the release notes", 3, 5, False, [], ""),
    ("Work", "Add a regression test for the date parser", 3, 7, False, [], "Single newlines matter here.\nThis line should stay on its own."),
    ("Work", "Update the dependency audit", 2, 14, False, ["research"], ""),
    ("Work", "Close out the old feature branch", 1, None, True, [], ""),
    ("Work", "Ship the changelog", 2, None, True, [], ""),
    ("Agents", "Triage the overnight CI failures and summarise the top three", 4, 0, False, ["urgent"], AGENT_RULES),
    ("Agents", "Draft release notes from the merged PRs since the last tag", 3, 1, False, [], ""),
    ("Agents", "Cross-check the OpenAPI spec against the client's endpoint table", 3, 2, False, ["research"], "Any path the client builds that the spec does not describe is a bug in one of them."),
    ("Agents", "Re-run the flaky integration test 20x and report the failure rate", 4, 0, False, ["urgent"], ""),
    ("Agents", "Audit dependencies for advisories and open one issue per finding", 2, 3, False, ["research"], ""),
    ("Agents", "Propose labels for the untriaged issues, do not apply them", 2, 5, False, ["waiting"], "Proposals only. Applying them is a human decision."),
    ("Agents", "Summarise this week's open issues into a digest", 1, 4, False, ["quick"], ""),
    ("Agents", "Convert the design doc's decisions into tracked tasks", 2, 7, False, [], ""),
    ("Agents", "Sweep the changelog for entries missing a ticket reference", 1, None, True, ["quick"], ""),
    ("Home", "Change the furnace filter", 2, 3, False, ["quick"], ""),
    ("Home", "Book the annual boiler service", 3, 10, False, ["waiting"], ""),
    ("Home", "Sort the garage shelves", 1, 21, False, [], ""),
    ("Home", "Water the plants", 1, 1, False, ["quick"], "Every three days is plenty."),
    ("Home", "Replace the hallway bulb", 1, None, True, ["quick"], ""),
    ("Reading", "Finish the chapter on consistency models", 2, 6, False, ["research"], ""),
    ("Reading", "Read the postmortem at https://example.com/postmortem", 2, 4, False, ["research"], "Linked in the title, so `o` picks it up from there."),
    ("Reading", "Skim the release notes for the new toolchain", 1, 9, False, [], ""),
    ("Reading", "Start the book on distributed systems", 2, 30, False, [], ""),
    ("Errands", "Renew the parking permit", 4, 1, False, ["urgent"], ""),
    ("Errands", "Return the delivery box", 1, 0, False, ["quick"], ""),
    ("Errands", "Pick up the dry cleaning", 2, 2, False, ["quick"], ""),
    ("Errands", "Take the recycling out", 1, None, True, ["quick"], ""),
]

for proj, title, priority, due, done, labs, description in TASKS:
    pid = projects[proj]
    # The path value goes into the body too: Vikunja binds the body second, so a body
    # field that shadows a path parameter silently overwrites it.
    body = {"title": title, "project_id": pid, "priority": priority, "done": done}
    if description:
        body["description"] = description
    if due is not None:
        body["due_date"] = day(due)
    task = call("PUT", f"/projects/{pid}/tasks", body)
    for lab in labs:
        call("PUT", f"/tasks/{task['id']}/labels", {"label_id": labels[lab]})

print(f"reset: {len(TASKS)} tasks across {len(projects)} projects, {len(labels)} labels")
