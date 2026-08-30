//! The layers wired together, for the checks no single crate can host.
//!
//! `tui-do-ui` is pure and knows nothing about a store; `tui-do-core` knows nothing about
//! a keystroke. That separation is rule 1 and is not negotiable — but it means the most
//! valuable assertions in the project have nowhere to live. The UI tests fake the store's
//! answers, the store tests fake the user, and *between* them is a seam where a message
//! can be produced with arguments nobody agreed on and every test still passes.
//!
//! `md/MANUAL-CHECKS2.md` F3 is the standing example, and the reason this crate exists.
//! A label created from the `l` form carries a provisional negative id until the server
//! names it. `crates/tui-do-ui/tests/update.rs` proves the open form reacts correctly to
//! `SyncEvent::Adopted { -1 → 41 }` — *given* that message. `crates/tui-do-core/tests/
//! sync.rs` proves a push emits an adoption. Neither proves the id in the second is the
//! id the first is waiting for, and the failure when they disagree is silence: the form
//! resolves its ticks against a renumbered list, matches nothing, queues no mutation and
//! toasts "Nothing changed".
//!
//! So this crate depends on both and drives a keystroke all the way to a request. It is
//! `publish = false`, has no binary, and nothing ships from it.
//!
//! # What the harness is, and what it is not
//!
//! [`Harness`] is a faithful-but-small reimplementation of the effect runtime in
//! `crates/tui-do/src/runtime/mod.rs`: same store calls, same messages back, same
//! push-after-apply. It is *not* that runtime — the real one owns a terminal and a tokio
//! task per effect, and neither can be asked for headlessly. So a break in the runtime's
//! own wiring is the one thing in the chain these tests cannot see; everything on either
//! side of it they can. Each arm of [`Harness::run`] names the arm it mirrors, so the two
//! can be read side by side when one changes.
//!
//! Effects are executed to completion in the order they were returned rather than
//! spawned, which makes the tests deterministic and is the one deliberate difference from
//! the runtime. The races it therefore cannot reproduce are covered where they belong, in
//! `tui-do-core`'s own tests.

// This crate is test scaffolding: `publish = false`, no binary, nothing ships from it.
// A harness reports a broken invariant the only way a test can, by panicking, and it does
// so with a message naming what it was doing -- which is strictly more use than threading
// a `Result` out to a `#[tokio::test]` that would `unwrap` it anyway. The workspace's
// denial stands everywhere it means something, which is code a user runs.
#![allow(clippy::expect_used, clippy::panic)]

use std::collections::VecDeque;

use chrono::{DateTime, FixedOffset};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use tui_do_api::{Client, Credentials};
use tui_do_core::config::Config;
use tui_do_core::store::{LabelFilter, LabelSort, ProjectFilter, ProjectSort, Store};
use tui_do_core::sync::Sync;
use tui_do_core::SyncEvent;
use tui_do_ui::query::Scope;
use tui_do_ui::update::{reload_everything, update};
use tui_do_ui::{Effect, Model, Msg};

/// How many messages one keystroke may produce before the harness calls it a loop.
///
/// A real cycle here would hang a test rather than fail it, and a hung test in CI is a
/// twenty-minute timeout with no output. The number is far above anything a keystroke
/// legitimately fans out to (a create is about a dozen) and far below forever.
const STEP_LIMIT: usize = 500;

/// A model, a store and a sync engine, wired the way the binary wires them.
#[derive(Debug)]
pub struct Harness {
    /// The interface's state, exactly as `update` leaves it.
    pub model: Model,
    /// The store both the interface and the engine read.
    pub store: Store,
    /// The API client, rebuilt into an engine per push the way `spawn_sync` does.
    client: Client,
    /// Whether a push is owed, from an [`Effect::Apply`] or a sync effect.
    ///
    /// Deferred rather than run where the effect was executed, because the *order* is
    /// what these tests are about. The runtime sends `Msg::Reload` and spawns the push
    /// side by side, and the reload wins every time in practice -- it is a local SQLite
    /// read against an HTTP round trip. Running the push first instead made the store
    /// answer that reload with the label already renumbered, so the form picked the
    /// server's id up through `absorb_created` and the adoption event was never needed.
    /// The test still passed with the event deleted, which is the precise failure it was
    /// written to catch.
    pending_push: bool,
    /// Every sync event the pushes have emitted, in order, for assertions about what the
    /// interface was told.
    pub events: Vec<SyncEvent>,
}

impl Harness {
    /// A harness against `uri`, with an empty in-memory store.
    ///
    /// # Errors
    ///
    /// If the store cannot be opened or the client cannot be built.
    pub fn new(uri: &str, now: DateTime<FixedOffset>) -> Result<Self, Box<dyn std::error::Error>> {
        let client = Client::builder(uri.to_string())
            .credentials(Credentials::api_token("tk_test"))
            .build()?;
        Ok(Self {
            model: Model::new(&Config::example(), Scope::All, now, (160, 40)),
            store: Store::in_memory()?,
            client,
            pending_push: false,
            events: Vec::new(),
        })
    }

    /// Load the interface from the store, the way startup does.
    pub async fn start(&mut self) {
        let effects = reload_everything(&mut self.model);
        self.settle(VecDeque::new(), effects).await;
    }

    /// Press a plain key.
    pub async fn press(&mut self, code: KeyCode) {
        self.send(Msg::Key(KeyEvent::new(code, KeyModifiers::NONE)))
            .await;
    }

    /// Type a run of characters, one keystroke each.
    pub async fn type_text(&mut self, text: &str) {
        for c in text.chars() {
            self.press(KeyCode::Char(c)).await;
        }
    }

    /// Press a Ctrl chord.
    pub async fn press_ctrl(&mut self, c: char) {
        self.send(Msg::Key(KeyEvent::new(
            KeyCode::Char(c),
            KeyModifiers::CONTROL,
        )))
        .await;
    }

    /// Feed one message in and run everything it causes to a standstill.
    ///
    /// # Panics
    ///
    /// If the messages do not run out, which is a cycle rather than a slow test.
    pub async fn send(&mut self, msg: Msg) {
        self.settle(VecDeque::from([msg]), Vec::new()).await;
    }

    /// Run messages and effects until neither is left and no push is owed.
    ///
    /// The two loops are the shape of the runtime, not an implementation detail. Local
    /// answers go round the inner one until the interface has stopped reacting; only then
    /// does the push that was owed actually go out, and whatever it has to say goes round
    /// the inner loop again. A push executed inline would answer a reload that has not
    /// happened yet, which is not an order the real program can produce.
    ///
    /// # Panics
    ///
    /// If the messages do not run out, which is a cycle rather than a slow test.
    async fn settle(&mut self, mut queue: VecDeque<Msg>, effects: Vec<Effect>) {
        for effect in effects {
            for reply in self.run(effect).await {
                queue.push_back(reply);
            }
        }
        let mut steps = 0usize;
        loop {
            while let Some(msg) = queue.pop_front() {
                steps += 1;
                assert!(steps <= STEP_LIMIT, "messages never settled: {msg:?}");
                for effect in update(&mut self.model, msg) {
                    for reply in self.run(effect).await {
                        queue.push_back(reply);
                    }
                }
            }
            if !std::mem::take(&mut self.pending_push) {
                return;
            }
            steps += 1;
            assert!(steps <= STEP_LIMIT, "pushes never settled");
            queue.extend(self.push().await);
        }
    }

    /// Execute one effect, answering with what the runtime would send back.
    ///
    /// Mirrors `runtime::execute`. An effect this harness does not know is a hole in a
    /// test rather than a warning to nobody, so it panics.
    ///
    /// # Panics
    ///
    /// On a store error, or an effect with no arm here.
    async fn run(&mut self, effect: Effect) -> Vec<Msg> {
        match effect {
            Effect::LoadTasks { id, filter, sort } => {
                let tasks = self.store.tasks(filter, sort).await.expect("read tasks");
                vec![Msg::TasksLoaded { id, tasks }]
            }
            Effect::LoadProjects => {
                let projects = self
                    .store
                    .projects(ProjectFilter::default(), ProjectSort::default())
                    .await
                    .expect("read projects");
                vec![Msg::ProjectsLoaded(projects)]
            }
            Effect::LoadLabels => {
                let labels = self
                    .store
                    .labels(LabelFilter::default(), LabelSort::default())
                    .await
                    .expect("read labels");
                vec![Msg::LabelsLoaded(labels)]
            }
            Effect::LoadCounts => {
                let counts = self
                    .store
                    .project_task_counts()
                    .await
                    .expect("read the counts");
                vec![Msg::CountsLoaded(counts)]
            }
            Effect::LoadPending => {
                let health = self.store.queue_health().await.expect("read the queue");
                vec![Msg::PendingLoaded(health)]
            }
            // The runtime queues, says `Reload`, and spawns a push -- in that order, and
            // the order is the point. The reload is a local read and lands first; the
            // push is a round trip, and what it learns arrives afterwards. See
            // `pending_push`.
            Effect::Apply(mutation) => {
                self.store.queue(mutation).await.expect("queue the change");
                self.pending_push = true;
                vec![Msg::Reload]
            }
            // Both are a push here. The pull half needs a mounted listing and would make
            // every test mount one; the tests that care about a pull live in
            // `tui-do-core`, which can ask for one directly.
            Effect::SyncNow | Effect::SyncFull => {
                self.pending_push = true;
                Vec::new()
            }
            Effect::RememberProject(_) | Effect::Quit => Vec::new(),
            other => panic!("the harness has no arm for {other:?}"),
        }
    }

    /// One push, with its events forwarded the way `spawn_sync` forwards them.
    async fn push(&mut self) -> Vec<Msg> {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let engine = Sync::new(self.client.clone(), self.store.clone()).with_events(tx);
        let outcome = engine.push().await;
        drop(engine);

        let mut msgs = Vec::new();
        while let Ok(event) = rx.try_recv() {
            self.events.push(event.clone());
            msgs.push(Msg::Sync(event));
        }
        if let Err(error) = outcome {
            msgs.push(Msg::Sync(SyncEvent::Failed {
                phase: tui_do_core::sync::Phase::Push,
                message: error.to_string(),
            }));
        }
        msgs
    }
}
