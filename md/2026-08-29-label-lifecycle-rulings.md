# tui-do — the label lifecycle: decisions taken during the build, 2026-08-29

The feature is described in `md/2026-08-28-label-creation-design.md` and was built to
`md/2026-08-29-label-lifecycle-plan.md`, in twelve tasks with a review after each and a
whole-branch review at the end. This file is the part that would otherwise have been lost:
the decisions taken *while* building, when the plan turned out to be wrong or a review
found something the plan had not anticipated.

Nine of these correct the plan itself. That is the honest headline — the plan was written
carefully against the code and was still wrong in nine places, most of which only showed up
when someone tried to build from it.

Known limitations that survived the build are recorded in `md/MANUAL-CHECKS2.md` under
section F, not here.

---

**Ruling 1 (pre-flight): T4 also updates `Msg::Adopted` and its runtime translation.**
T4 changes `SyncEvent::Adopted`'s fields to `Subject`, and `tui-do-ui`'s `Msg::Adopted`
mirrors that event with the binary translating between them. Changing one without the
other leaves the workspace not compiling, so T4 must carry both; T7 then implements the
split by kind. Cost if wrong: T4's diff is a little wider than its brief says, and T7's is
narrower. Nothing is lost either way.

**Ruling 2 (pre-flight): `Subject` is re-exported from `tui_do_core::store`.**
T1's text defines it in `store/outbox.rs` and does not say how `tui-do-ui` reaches it.
Every other shared type there (`Mutation`, `OutboxEntry`) is re-exported from
`tui_do_core::store`, so `Subject` follows the same route. Cost if wrong: a one-line
export moves.

**Ruling 3: the sixth site is in scope for Task 1.** `store/tasks.rs:360`, the pending
check inside `upsert_tasks_from_server`, reads
`SELECT exists(SELECT 1 FROM outbox WHERE subject_id = ?1)` with no kind. Verified by
hand. It is the same defect the brief's five sites are, and it is the one that decides
whether a pull skips a task: a queued label with id 41 would make every pull skip task 41
and clamp the watermark to it. The brief listed five sites because I missed this one when
writing the plan; the finding is right and the fix belongs here, not in a later task where
it would sit latent behind a shipped commit. Cost if wrong: a one-line SQL change lands in
Task 1's commit rather than its own.

**Ruling 4: the v5 index keeps the order the plan gave it.** The reviewer is right that
every query is `subject_id = ?1 AND subject_kind = 'task'` and that
`(subject_id, subject_kind)` would subsume the existing `outbox_by_subject`. But both
predicates are equality, so a composite index serves them in either order, and the only
real cost is one redundant index on a table holding tens of queued rows. That is a smaller
price than editing a migration that is already committed — the append-only rule exists so
that nobody has to reason about which databases have run one, and reasoning about it is
exactly what changing this would require. If the redundancy ever matters, a v6 drops
`outbox_by_subject` cleanly. Cost if wrong: one superfluous index until someone bothers.

**Ruling 5: the interim `transmit` arm stands, and Task 4 replaces it.** It is unreachable
today — nothing outside a test can queue a `CreateLabel` until Task 8 wires the UI — and
the alternative, an arm that refuses or panics, is a worse thing to leave in a shipped
commit than one that is honest about being provisional. Its comment names both missing
halves. Carried into Task 4's dispatch as a *replace*, not an *add*, since Task 4's brief
was written assuming no arm exists. Cost if wrong: if anything queued a `CreateLabel`
before Task 4 lands, the server would get the label while the local row kept id -1 and the
entry was dropped. Nothing between here and there can queue one.
Task 2: review 1 — spec ✅, quality Approved. One Important finding, out of this task's
scope; four minors.

**Ruling 6: `retain_labels` needs the outbox exemption, and Task 3 owns it.** The reviewer
found that `store/labels.rs` deletes every label the server's listing did not name unless
it is attached to a task — with no `outbox` exemption, where `retain_tasks` has one. So a
label created offline and not yet attached is erased by the next `pull_lists`, which runs
on every pull including startup, before the create is ever sent; the outbox entry survives
and creates a server label the local store has no row for. Verified by hand. No brief in
the plan covers it, so it was on course to ship. It is not a defect in Task 2's diff —
nothing can queue a `CreateLabel` yet — so Task 2 is complete, and the fix goes to Task 3,
which is the task that owns a provisional label's survival in the store. Cost if wrong: the
guard lands one task later than ideal, in the same file group, still before anything can
reach it.

**Ruling 7: minors 1-3 are fixed now rather than deferred.** Minor 1 is the substantive
one: `settle_create_label`'s `UPDATE OR IGNORE task_labels SET label_id` — the single
statement whose ordering the method exists to protect — has no assertion over it. Deleting
it leaves the adoption test passing, because the following `DELETE FROM labels` cascades
the row away and nothing looks at `task_labels`. The user-visible cost is a locally
attached label falling off its task the moment the create settles. The reviewer noted the
task-side `settle_create` test has the identical hole, so both get the assertion. Fixed
here rather than deferred because Task 4 is about to wire the first caller, and a
regression window that opens as the caller lands is the worst time to have one. Cost if
wrong: three small test/comment edits in files nothing else touches for several tasks.
Task 3: fix round 1/5 (3 addressed, 0 open — task_labels re-point assertion on both the
label and task sides, incident comment tightened, OR IGNORE rationale; commit b31b8e6).
Re-reviewer independently traced `labels_for`'s INNER JOIN to confirm the new assertion can
genuinely fail when the re-point is removed, rather than taking the report's word.
Task 3: complete (commits 0b3bfff..b31b8e6, review clean)

**Ruling 1 was partly wrong, corrected here.** There is no `Msg::Adopted`: `tui-do-ui`
consumes `Msg::Sync(SyncEvent)` directly, so the event-shape change was a two-way change,
not three. The implementer adapted correctly. **This changes Task 7's brief**, whose test
snippet dispatches `Msg::Adopted { provisional, assigned }` — it must be
`Msg::Sync(SyncEvent::Adopted { .. })`. Carried into Task 7's dispatch.

**Ruling 8: minors 1-4 are fixed now; minor 5 is dropped.** 1, 3 and 4 are one-liners
(`is_failing()` instead of open-coding `attempts > 0`; a `warn!` and a comment correction
where the server holds two exact matches; the ASCII-only caveat on "case-insensitive").
Minor 2 is a real branch with no test — the retry whose read finds nothing and must fall
through to a create — and it is half the reason the read exists. Minor 5 (a paging test of
its own for `labels_named`) is dropped: rule 4 is satisfied structurally by going through
`Pager`, and `all_labels` already has a paging test. Cost if wrong: one uncovered paging
path on a method whose pager is shared with a covered one.

**Ruling 9: `labels_named` and `Client::label` get live coverage, in Task 5.** The reviewer
noted that `labels_named` has never run against dev, and that `Client::update_label`'s
`405` sat undiscovered in exactly that position — a client method with no live caller. Task
5 adds `Client::label`, a second such method, and is the last task touching the API crate,
so both get an assertion in `tests/live.rs` there. Cost if wrong: two client methods reach
Phase 5 proven only against a mock, which is the failure mode this project has already had
once.
Task 4: fix round 1/5 (4 addressed, 0 open — named the gate `is_failing()`, covered the
empty-read fall-through, logged duplicate titles and made the gate comment's limit
explicit, ASCII caveat; commit 5e4e273). Re-reviewer independently judged the new test's
bite claim sound: the id assertion catches the regression first, with the PUT mock's
`.expect(1)` as a backstop.
Task 4: complete (commits b31b8e6..5e4e273, review clean)

**Ruling 9 discharged: I ran the live suite against dev.** All nine tests pass, including
the new `Client::label` and `labels_named` assertions — the exact-title filter over
Vikunja's substring search is now proven against the real server. Dev left clean.

**Ruling 10: minor 6 is deferred to Task 9, not fixed here.** `SyncEvent::Overwrote` now
carries a `Subject` and the only consumer still discards it, so a label collision toasts
word-for-word like a task collision. Naming the subject is the payoff of the type change
and it is user-visible text, which belongs with the label editor in Task 9 where a human
will actually see it. Cost if wrong: one generic toast survives two more tasks on a path no
UI can reach yet.
Task 5: fix round 1/5 (2 Important + 4 minors addressed, 0 open; commit 1d361a1). The
implementer found my suggested fix for minor 4 unreachable (`Call` and `Pager::new` are
`pub(crate)`) and split the client method instead — `labels_matching` is the raw `s=`
search, `labels_named` is that plus the exact filter. Re-reviewer judged the split a
reasonable decomposition rather than gratuitous API widening: it is the only way to make
the filter's exactness observable from an integration test, and `labels_named`'s behaviour
is unchanged. Both guard-removal and load-bearing claims independently traced and sound.
Live suite re-run against dev after the fix: 9/9 pass.
Task 5: complete (commits 5e4e273..1d361a1, review clean)

**Ruling 11: Task 7's minor 1 is folded into Task 8, not fixed in its own round.** The
negative test `an_adoption_leaves_a_label_it_does_not_name_alone` pins the guard for
`model.data.labels`, `task.labels` and `query.scope` but has no modal on the stack, so the
three new modal blocks have no negative case. They all route through the same `swap`
closure, so the risk is low, and Task 8 is in that exact test file. Cost if wrong: the
cheaper half of a pair lands one task later.
Task 7: deferred minor: `undo_text`'s four label arms are unquoted where `UpdateTask` quotes
its title. Pre-existing, cosmetic. **For the final review to triage.**
Task 7: complete (commits 8e97e72..84f291a, review clean)

**Ruling 12: a rejected label create clears the form's `awaiting`, and the `Rejected` arm
reloads labels.** The reviewer found the new `awaiting` mechanism has an unanalysed path
that this task is what makes reachable. Two halves. A rejection landing *after* the naming
reload leaves a phantom label ticked in the form until the next pull — fixed by adding
`Effect::LoadLabels` to the `Rejected` arm, which already reloads tasks, counts and pending.
A rejection landing *before* any reload leaves the title in `awaiting` forever, so
`creatable()` returns `None` and `Ctrl-N` silently does nothing for the rest of the form's
life — a user whose account cannot create labels sees the key work once and then die with no
explanation. The form cannot map the event's `Subject::Label(id)` back to a title, because
it never learned the id, so the only bounded answer without new plumbing is to clear
`awaiting` on any label-subject rejection. Cost if wrong: with two creates in flight and one
rejected, the other loses its automatic tick — but the reload still brings it into
`model.data.labels`, so it is visible and `creatable()` is right about it. A lost tick is a
much smaller failure than a dead key.

**Ruling 13: minor 4 is documented, not fixed.** `Esc` then `l` inside the reload window
reopens the duplicate window that `awaiting` closes, because `awaiting` lives in
`LabelsState`. Closing it properly needs model-level state, which is a larger change than
the hole justifies — one keystroke, inside a window that is one round trip wide, producing
one duplicate label in a pool the user can see. Cost if wrong: a rare duplicate nobody has
reported. **For the final review to triage.**

**Ruling 14: `eq_ignore_ascii_case` stays ASCII-only, with a caveat comment.** Three sites
here treat `café`/`CAFÉ` as distinct. The whole codebase folds this way — `resolve_labels`,
`labels_named`, and Task 4 already documented the caveat there — so changing three sites in
isolation would make the interface disagree with itself about what a duplicate is. Cost if
wrong: non-ASCII case pairs are offered as creatable when they arguably should not be.
Task 8: fix round 1/5 (2 Important + 3 minors addressed, 0 open; commit 88c9ee0).
Re-reviewer confirmed the two rejection tests genuinely distinguish the two halves of the
finding, that the clear is gated to label subjects only with the cost written into the
comment, that the folding was not changed, and that the no-golden decision was reasonable
rather than an avoidance.
Task 8: deferred minor: `a_rejected_label_create_is_taken_off_the_screen_it_is_still_ticked_on`
asserts the effect is queued, not that the phantom tick is gone once the reload lands. Test
rigor, not a product defect. **For the final review to triage.**
Task 8: complete (commits fd55537..88c9ee0, review clean)

**Ruling 15: the submission carries the form's `Label` as `before`.** `EditedLabel` gains a
`before: Box<Label>`, and the pool lookup stays as the existence check it is already doing
well. Cost if wrong: the submission is one field wider.

**Ruling 16: minor 3 is fixed, not documented.** The `l` form's footer advertises `C-e` for
any row under the cursor, including a label only the task carries and not yet in the pool —
where `C-e` toasts "not synced here yet". The default render fixture is exactly that shape,
so the offer names an uneditable label the moment the user presses Down. `LabelsState`
cannot see the pool, so this needs that knowledge passed in — worth the plumbing, because
the doc comment claims each hint is "shown exactly when it would do something" and it is
not. Cost if wrong: one more field on a form state that already carries four.

**Ruling 16 corrected by the implementer, and its version is better.** I ruled that the
footer should hide `C-e` for a label not in the pool. It checked the underlying fact first
and found my premise wrong: a task's labels are read by *joining* the labels table
(`store/labels.rs`, INNER JOIN), so a label the `l` form shows always has a store row — the
pool lacking it is a `LabelsLoaded`/`TasksLoaded` ordering window in the snapshot, not a
structural gap. So rather than hiding the key it made the key work: `known_label` looks in
the pool and then in the tasks' labels. That makes "shown exactly when it would do
something" true in the stronger direction and drops a state field that would have needed
keeping in step across three methods, while keeping the refusal for the genuinely absent
case. Accepted. The lesson is the same one as the Important finding: I ruled from the
plan's model of the code rather than from the code.
Task 9: fix round verified — all 5 findings addressed, every mutation-check traced by hand
by the re-reviewer, no golden moved. One deferred observation: `labels_offer` adds the gap
width even when only one hint is present, a ~1-character over-subtraction from the
truncation budget. Harmless. **For the final review to triage.**
Task 9: complete (commits 88c9ee0..f216507, review clean)

**Ruling 17: minor 1 is fixable cleanly, contrary to the review's assumption.** It says
gating on the rejection's `kind` would mean "matching a display string, its own fragility".
It is not a display string: `SyncEvent::Rejected.kind` is `Mutation::kind()`, whose own doc
comment calls it "a short stable name" that exists so "a future retry policy [can] select by
kind without parsing every row" — `"create_label"`. Gating on it is using the field for
exactly its documented purpose. Cost if wrong: a rejected rename of an unrelated label stops
abandoning a create that is still in flight, which is the behaviour we want anyway.

**Ruling 18: minor 6 is parked, not fixed.** There is no route back to fix a typo — a user
who typed `*waitng` can only create it or drop it, then re-edit the task. The brief
specified two answers and the implementation is faithful to it. A third answer ("edit the
line") means restoring a popped prompt with its text, which is a bigger change than the
sharpness justifies at this point in the plan. **For the final review to triage.**
Task 10: fix round 1/5 (5 minors addressed, 0 open; commit 0ebf75a). Ruling 17 paid for
itself immediately: the kind gate exposed a pre-existing test asserting `"create label"`
with a space — a display string, not the store's `Mutation::kind()` — which the re-reviewer
confirmed from the diff. The implementer also narrowed Task 8's `awaiting` clear on the same
predicate, fixing the identical latent bug there (a rejected rename of some other label used
to clear `awaiting` for an unrelated create); the re-reviewer judged that out-of-scope change
correct and safe.
Task 10: deferred minor: no test exercises the narrowed kind gate through `Modal::Labels`,
only through `Modal::ConfirmLabels`; and the kind-coupling test hardcodes the literal rather
than referencing the private constant, so it pins against a store-side rename but not
against the constant itself drifting. Both disclosed by the implementer. **For the final
review to triage.**
Task 10: parked: no route back to fix a typo in an unknown `*label` — create it or drop it,
then re-edit the task. Faithful to the brief's two answers. **For the final review.**
Task 10: complete (commits f216507..0ebf75a, review clean)

**Ruling 19: the duplicate-label hole is fixed in both paths, and at the shared root.** The
reviewer found that repeated or case-varying unknown names queue one `CreateLabel` each, so
`tui-do add "x *waiting *Waiting" --create-labels` makes two server labels unattended — the
exact global-pool pollution the flag's own `--help` says it prevents. I checked the
interactive path and it has the same hole: `resolve_labels` folds ASCII case when *matching*
a known label, then pushes every miss unfiltered, and `create_labels` queues one create per
title. Two case-variants are therefore "the same label" for matching and "two new labels"
for creating, which is the interface disagreeing with itself. `resolve_labels` should treat
misses the way it treats matches. Cost if wrong: a user who genuinely wants both `waiting`
and `Waiting` gets one — which Vikunja's own global pool makes a bad idea anyway.
Task 11: fix round 1/5 (2 Important + 3 minors addressed; commit 17416cf). Deduped at the
shared root as ruled, fixing both paths. Verified the real store after its release-binary
smoke test: still schema v4, 0 outbox rows, 3889 tasks, one pre-existing label — the smoke
test used a scratch config, and a run against the real one would have migrated it to v5.
Task 11: fix round verified — all 5 findings addressed, real-store safety confirmed (every
new test uses `Store::in_memory()`; nothing in the diff touches the default path).
Task 11: deferred minor: `resolve_labels`'s new dedup compares the untrimmed stored name
against a trimmed candidate, so a label token carrying incidental whitespace could evade it.
Not live — quickadd's tokenizer does not produce such tokens — and it carries the same
asymmetry the pre-existing match already had. **For the final review to triage.**
Task 11: complete (commits 0ebf75a..17416cf, review clean)

**Ruling 20: no scoped re-review seat for a two-word prose fix.** The change is two
corrected phrases in documentation, each verified against source by the implementer before
editing, and the final whole-branch review reads this same diff. I verified both strings
myself. Cost if wrong: a wrong word in a docs file that the final review is about to read.
Task 12: complete (commits 17416cf..9488297, review clean)

**Ruling 21: minor 4 is not fixed.** `is_provisional_label` is public with no production
caller — but it mirrors `is_provisional`, which has the same property and predates this
branch. Adding a caller or removing the export are both larger decisions than this fix wave
should take. Cost if wrong: one unused predicate stays exported, consistent with its sibling.

**Ruling 22: the re-weighted test shape is fixed while we are there.** The reviewer asked me
to reconsider a deferred minor — a test asserting a `LoadLabels` effect was queued rather
than that the phantom tick is gone — noting it sits on the same seam as the Important
finding: verifying the right message was sent rather than that the wrong state cannot arise.
It is right. The new test for the Important finding asserts the request was never made, and
this one gets the same treatment.
