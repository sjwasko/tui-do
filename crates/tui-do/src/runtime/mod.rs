//! The effect runtime: the half of tui-do that is allowed to block.
//!
//! `tui-do-ui` describes what it wants as [`Effect`] values and learns what happened as
//! [`Msg`] values. This module is what turns one into the other — it owns the terminal,
//! the store, the API client and the sync timer, and it is the only place in tui-do where
//! anything is awaited.
//!
//! The loop is deliberately shaped so that no store read or network call sits between a
//! keypress and a frame: every effect is spawned onto its own task and answers by sending
//! a message back. The render loop's only await is on the message channel.

mod coalesce;
mod editor;
mod prompt;
mod signals;
mod terminal;

use std::future::Future;
use std::io::{self, Write};
use std::panic::AssertUnwindSafe;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::task::{Context as TaskContext, Poll};
use std::time::Duration;

use anyhow::Context;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tui_do_api::Client;
use tui_do_core::models::ProjectId;
use tui_do_core::store::{
    LabelFilter, LabelSort, Mutation, ProjectFilter, ProjectSort, Subject, KEYS_NOTICE_DISMISSED,
    LAST_PROJECT, LAST_PULL,
};
use tui_do_core::sync::Reach;
use tui_do_core::{Config, Store, Sync, SyncEvent};
use tui_do_ui::modal::KeysChangedState;
use tui_do_ui::model::landing_scope;
use tui_do_ui::model::UrlAction;
use tui_do_ui::theme::{ColorDepth, Theme};
use tui_do_ui::update::reload_everything;
use tui_do_ui::{update, view, Effect, Model, Msg};

use signals::Signal;
use terminal::TerminalGuard;

/// How often the clock is reported to the model.
///
/// One second is enough for "synced 2m ago" and for a toast to expire, and cheap enough
/// that a redraw per tick does not register.
const TICK: Duration = Duration::from_secs(1);

/// How long the input reader waits before checking whether it should stop.
const INPUT_POLL: Duration = Duration::from_millis(100);

/// The current time, in this machine's zone.
///
/// The only clock in the program that the interface sees, and the zone is the point of
/// it. `due today` is resolved against this, and so is the decision to call a date
/// "Today" — both are claims about the calendar the user is living in, and both answer
/// wrongly if the instant arrives labelled UTC. Stamped as `Utc::now()`, `due today`
/// lands at 23:59 UTC, which west of Greenwich is that same evening: the task turns
/// overdue hours early, every day. That is the bug in the project tui-do replaces.
///
/// `Zone::Local` rather than today's fixed offset, because a date typed for the far side
/// of a daylight-saving change must be resolved at the offset in force on that date
/// (BUG-20). Tests pin `Zone::Fixed` instead, so they do not depend on the machine.
fn now() -> tui_do_core::ZonedTime {
    chrono::Utc::now().with_timezone(&tui_do_core::Zone::Local)
}

/// How long the flush on exit waits between dots.
const FLUSH_TICK: Duration = Duration::from_secs(1);

/// How many dots the flush on exit prints before giving up, one a second.
///
/// This is the whole budget for the last push, so it is a deliberate trade: long
/// enough that a slow-but-reachable server still lands the change, short enough
/// that quitting offline does not feel like a hang. The change is not lost either
/// way -- it stays in the outbox and goes out on the next run.
const FLUSH_DOTS: u32 = 5;

/// Run the interface until the user quits.
///
/// # Errors
/// Anything that stops tui-do from starting: an unreadable store, an unusable terminal, a
/// config that names a server it cannot parse. Once the loop is running, failures become
/// messages rather than errors — that is the point of the split.
pub async fn run(config: Config, config_path: std::path::PathBuf) -> anyhow::Result<()> {
    let store = open_store().await?;

    let (tx, mut rx) = LoopTx::channel();
    // Before the terminal is taken, so there is no moment when it is raw and a signal
    // would still end the process without giving it back (BUG-31).
    if let Some(listener) = signals::listen(tx.signals.clone()) {
        spawn_effect(tx.clone(), listener);
    }

    // The interface starts against the cache, whether or not a server is reachable. A
    // missing or unreadable credential costs the user sync, not the application: an
    // offline start that renders the cached list is the whole thesis of the rewrite.
    let (sync, credential_problem) = build_sync(&config, &config_path, &store);

    let scope = {
        let projects = store
            .projects(ProjectFilter::default(), ProjectSort::default())
            .await
            .unwrap_or_default();
        let remembered = store
            .state(LAST_PROJECT)
            .await
            .ok()
            .flatten()
            .and_then(|value| value.parse::<i64>().ok())
            .map(ProjectId);
        landing_scope(&config.view, remembered, &projects)
    };

    let mut guard = TerminalGuard::take()?;
    let size = guard.size().unwrap_or((120, 40));

    let mut model = Model::new(&config, scope, now(), size);
    model.theme = Theme::new(color_depth());
    model.url_action = url_action();
    if keys_notice_owed(&store).await {
        model
            .modals
            .push(tui_do_ui::Modal::KeysChanged(KeysChangedState::new(
                release_notes_url(),
            )));
    }
    if let Some(problem) = credential_problem {
        model.status.sync = tui_do_ui::model::SyncStatus::Failed {
            message: problem.clone(),
        };
        model.toast(tui_do_ui::model::Toast::error(problem));
    } else if let Some(exposed) = config.exposed_credential_files(&config_path).first() {
        // Second in line: a token readable by other accounts is worth saying, but not at
        // the cost of hiding a credential that does not work at all.
        model.toast(tui_do_ui::model::Toast::warning(exposed.clone()));
    }

    let stop = Arc::new(AtomicBool::new(false));
    spawn_input(tx.clone(), Arc::clone(&stop));
    spawn_ticks(tx.clone(), Arc::clone(&stop));
    if let Some(sync) = sync.clone() {
        spawn_sync_timer(
            sync,
            store.clone(),
            config.sync.clone(),
            Arc::clone(&stop),
            tx.clone(),
        );
    }

    let result = event_loop(&mut model, &mut guard, &tx, &mut rx, &store, sync.as_ref()).await;
    stop.store(true, Ordering::SeqCst);

    // The terminal goes back to the user before anything is printed to it. After a
    // panic the hook has done this already, and the drop is a no-op. After SIGHUP the
    // terminal has probably gone, so the restore is tried and not waited on, and the
    // flush below says nothing: there is nobody to read it, and a write could block.
    let signalled = match &result {
        Ok(Exit::Signalled(caught)) => Some(*caught),
        _ => None,
    };
    let hung_up = signalled.is_some_and(Signal::hung_up);
    guard.release_for(signalled);
    // A panicked task, lost input or a signal ends the session here too, rather than in
    // the panic hook, the input thread or the signal's default action, so the last push
    // still gets its chance: the edit made just before is usually the one the user most
    // wants to have landed.
    if let Some(sync) = &sync {
        let mut out: Box<dyn Write + Send> = if hung_up {
            Box::new(io::sink())
        } else {
            Box::new(io::stdout())
        };
        if let Some(again) = flush_unless_signalled(&store, sync, &mut rx, &mut *out).await {
            // A signal while the flush waits -- the second, or the first after `q`: the
            // user wants it gone now. The terminal is already back, so leave; what is
            // queued stays queued, and a push cut off mid-request may be sent again next
            // run (see `flush_unless_signalled`).
            std::process::exit(again.exit_code());
        }
    }
    match result? {
        Exit::Quit => Ok(()),
        // Returned rather than printed, so `main` reports it on stderr and exits
        // non-zero, the way every other failure to keep running does.
        Exit::Fatal(why) => Err(anyhow::anyhow!("tui-do stopped because {why}")),
        // As a shell reports a process the signal killed, so a script or a service
        // manager sees what it would have seen before tui-do caught the signal.
        Exit::Signalled(caught) => std::process::exit(caught.exit_code()),
    }
}

/// [`flush_on_exit`], unless another signal arrives first, in which case that signal.
///
/// The flush can wait five seconds on an unreachable server, and a signal during that
/// wait means "now" (BUG-31). That is a *second* signal when a signal ended the session,
/// and the *first* when `q`, a panic or lost input did -- the user quit, the flush is
/// still waiting, and they kill it.
///
/// Either way, a push in flight at that moment is abandoned mid-request, exactly as the
/// signal's default action abandoned it before tui-do caught signals, and as `kill -9`
/// still does. That is the BUG-3 class: the entry stays queued, and if the request had
/// already reached the server the next launch sends it again, which for a `CreateTask`
/// means a second task (BUG-33). The push being abandoned may be the flush's own or the
/// running pass's, which the flush is giving its grace. The third way a push is
/// abandoned is `tui-do add`'s, in `signals::listen`.
async fn flush_unless_signalled(
    store: &Store,
    sync: &Arc<Sync>,
    rx: &mut LoopRx,
    out: &mut (dyn Write + Send),
) -> Option<Signal> {
    tokio::select! {
        biased;
        Some(again) = rx.signals.recv() => Some(again),
        () = flush_on_exit(store, sync, out) => None,
    }
}

/// How the session ended.
#[derive(Debug, PartialEq, Eq)]
enum Exit {
    /// The user quit, or every sender went away.
    Quit,
    /// Something happened that the session cannot carry on from.
    Fatal(Fatal),
    /// SIGTERM, SIGHUP or SIGQUIT: the same quit as `q`, then the signal's exit code.
    Signalled(Signal),
}

/// Why the session cannot carry on.
#[derive(Debug, PartialEq, Eq)]
enum Fatal {
    /// A spawned task or thread panicked. Carries what it said.
    Panicked(String),
    /// The terminal stopped delivering input. Carries the error it gave. An interface
    /// that cannot be typed at cannot be quit either, so this ends it.
    InputLost(String),
}

impl std::fmt::Display for Fatal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Panicked(why) => write!(f, "a background task panicked: {why}"),
            Self::InputLost(why) => write!(f, "input from the terminal was lost: {why}"),
        }
    }
}

/// How anything off the render loop talks back to it: a [`Msg`] for `update`, or the
/// news that the session has to end.
///
/// The fatal side goes on its own channel rather than as a `Msg` variant. `Msg` belongs to
/// `tui-do-ui`, which is pure, and there is nothing for `update` to decide here: the
/// process is no longer in a state anyone can reason about, the panic hook has already
/// handed the terminal back, and the only correct answer is to stop -- which is the
/// runtime's job, as drawing is. A variant would put a message into the interface's
/// vocabulary that the interface must never act on.
#[derive(Debug, Clone)]
struct LoopTx {
    msg: UnboundedSender<Msg>,
    fatal: UnboundedSender<Fatal>,
    /// A signal that ends the session (BUG-31). Its own channel for the same reason as
    /// `fatal`: `update` has nothing to decide about it.
    signals: UnboundedSender<Signal>,
    /// This runtime's read gates (BUG-23). Made with the channel, so two loops in one
    /// process never share a queued request, its store or its sender.
    reads: Arc<coalesce::Reads>,
}

/// The loop's end of [`LoopTx`].
struct LoopRx {
    msg: UnboundedReceiver<Msg>,
    fatal: UnboundedReceiver<Fatal>,
    signals: UnboundedReceiver<Signal>,
}

impl LoopTx {
    fn channel() -> (Self, LoopRx) {
        let (msg, msg_rx) = mpsc::unbounded_channel();
        let (fatal, fatal_rx) = mpsc::unbounded_channel();
        let (signals, signals_rx) = mpsc::unbounded_channel();
        (
            Self {
                msg,
                fatal,
                signals,
                reads: Arc::default(),
            },
            LoopRx {
                msg: msg_rx,
                fatal: fatal_rx,
                signals: signals_rx,
            },
        )
    }

    /// Send `update` a message. Fails only once the loop has gone.
    fn send(&self, msg: Msg) -> Result<(), mpsc::error::SendError<Msg>> {
        self.msg.send(msg)
    }

    /// Tell the loop the session has to end. A loop that has already gone has nothing
    /// left to stop, so a failed send is dropped.
    fn fatal(&self, why: Fatal) {
        let _ = self.fatal.send(why);
    }

    /// Run `work`, and if it panics, tell the loop to stop and answer `None`.
    ///
    /// The one place a panic is caught. `AssertUnwindSafe` is sound because nothing that
    /// `work` may have left half-updated is looked at again: a caught panic ends the task
    /// or thread it happened on -- a future that panicked is never polled again -- and the
    /// payload is only turned into text. The session then ends on the loop's next turn.
    fn guarded<T>(&self, work: impl FnOnce() -> T) -> Option<T> {
        match std::panic::catch_unwind(AssertUnwindSafe(work)) {
            Ok(value) => Some(value),
            Err(payload) => {
                self.fatal(Fatal::Panicked(describe_panic(payload.as_ref())));
                None
            }
        }
    }
}

/// What a panic said, where it said anything readable.
///
/// `panic!("literal")` carries a `&str` and `panic!("{x}")` a `String`; anything else
/// came from `panic_any` and cannot be printed.
fn describe_panic(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(text) = payload.downcast_ref::<&str>() {
        (*text).to_string()
    } else if let Some(text) = payload.downcast_ref::<String>() {
        text.clone()
    } else {
        "a panic that carried no message".to_string()
    }
}

/// A future run under [`LoopTx::guarded`], one poll at a time. A panicking poll finishes
/// it, so it is never polled again. Boxed so it is `Unpin` and needs no pin projection.
struct Guarded<F> {
    tx: LoopTx,
    future: Pin<Box<F>>,
}

impl<F: Future<Output = ()>> Future for Guarded<F> {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<()> {
        let this = self.get_mut();
        this.tx
            .guarded(|| this.future.as_mut().poll(cx))
            .unwrap_or(Poll::Ready(()))
    }
}

/// Spawn a task whose panic ends the session instead of vanishing.
///
/// A plain `tokio::spawn` with its `JoinHandle` dropped is what BUG-7 was: tokio catches
/// the panic at the task boundary, the panic hook has already restored the terminal, and
/// the loop goes on drawing over the user's shell. Every task the runtime spawns goes
/// through here or through [`spawn_blocking_effect`].
///
/// The panic is caught inside the task rather than by a second task awaiting this one's
/// `JoinHandle`, so the handle returned is the task itself. `flush_on_exit` aborts or
/// awaits the running sync pass through that handle, and aborting a watcher would only
/// have detached the pass from it.
fn spawn_effect<F>(tx: LoopTx, future: F) -> tokio::task::JoinHandle<()>
where
    F: Future<Output = ()> + Send + 'static,
{
    tokio::spawn(Guarded {
        tx,
        future: Box::pin(future),
    })
}

/// [`spawn_effect`] for work that blocks a thread.
fn spawn_blocking_effect<F>(tx: LoopTx, work: F)
where
    F: FnOnce() + Send + 'static,
{
    tokio::task::spawn_blocking(move || tx.guarded(work));
}

/// Read messages, update, draw. The only loop in tui-do.
///
/// A signal, then a panicked task, is checked for first, every time round, and ends the
/// loop the same way `q` does -- by returning -- so everything after the loop in [`run`]
/// still runs. The receiver is borrowed so [`run`] can go on hearing signals afterwards.
async fn event_loop<B: ratatui::backend::Backend>(
    model: &mut Model,
    guard: &mut TerminalGuard<B>,
    tx: &LoopTx,
    rx: &mut LoopRx,
    store: &Store,
    sync: Option<&Arc<Sync>>,
) -> anyhow::Result<Exit> {
    for effect in reload_everything(model) {
        perform(effect, store, sync, tx);
    }
    guard.draw(|frame| view(model, frame))?;

    loop {
        let msg = tokio::select! {
            biased;
            // Ahead of the fatal report, so a hangup that also kills the input thread's
            // reads is still known for a hangup, and not written to as a live terminal.
            Some(caught) = rx.signals.recv() => return Ok(Exit::Signalled(caught)),
            Some(why) = rx.fatal.recv() => return Ok(Exit::Fatal(why)),
            msg = rx.msg.recv() => msg,
        };
        let Some(msg) = msg else { break };
        let mut effects = update(model, msg);
        // Drain whatever else has already arrived before drawing. A sync pass finishing
        // sends four answers at once, and drawing between each of them is three wasted
        // frames.
        while let Ok(next) = rx.msg.try_recv() {
            effects.extend(update(model, next));
        }
        for effect in effects {
            perform(effect, store, sync, tx);
        }
        if !model.running {
            break;
        }
        guard.draw(|frame| view(model, frame))?;
    }
    Ok(Exit::Quit)
}

/// Execute one effect, off the render loop.
///
/// Every arm spawns and returns immediately. Nothing here awaits inline, because this
/// function is called from the loop that also draws.
fn perform(effect: Effect, store: &Store, sync: Option<&Arc<Sync>>, tx: &LoopTx) {
    match effect {
        // The five reads `Msg::Reload` asks for go through `coalesce`: one of each in
        // flight and the newest request waiting behind it (BUG-23). A load that is
        // already running answers under its own id; only the one that starts later
        // carries a newer one.
        Effect::LoadTasks { id, filter, sort } => {
            let (store, answer) = (store.clone(), tx.clone());
            coalesce::coalesced(&tx.reads.tasks, tx, (id, filter, sort), move |request| {
                let (store, tx) = (store.clone(), answer.clone());
                async move {
                    let (id, filter, sort) = request;
                    let msg = match store.tasks(filter, sort).await {
                        Ok(tasks) => Msg::TasksLoaded { id, tasks },
                        Err(error) => Msg::StoreFailed(error.to_string()),
                    };
                    let _ = tx.send(msg);
                }
            });
        }
        Effect::LoadProjects => {
            let (store, answer) = (store.clone(), tx.clone());
            coalesce::coalesced(&tx.reads.projects, tx, (), move |()| {
                let (store, tx) = (store.clone(), answer.clone());
                async move {
                    let msg = match store
                        .projects(ProjectFilter::default(), ProjectSort::default())
                        .await
                    {
                        Ok(projects) => Msg::ProjectsLoaded(projects),
                        Err(error) => Msg::StoreFailed(error.to_string()),
                    };
                    let _ = tx.send(msg);
                }
            });
        }
        Effect::LoadLabels => {
            let (store, answer) = (store.clone(), tx.clone());
            coalesce::coalesced(&tx.reads.labels, tx, (), move |()| {
                let (store, tx) = (store.clone(), answer.clone());
                async move {
                    let msg = match store
                        .labels(LabelFilter::default(), LabelSort::default())
                        .await
                    {
                        Ok(labels) => Msg::LabelsLoaded(labels),
                        Err(error) => Msg::StoreFailed(error.to_string()),
                    };
                    let _ = tx.send(msg);
                }
            });
        }
        Effect::LoadUsers => {
            let (store, answer) = (store.clone(), tx.clone());
            coalesce::coalesced(&tx.reads.users, tx, (), move |()| {
                let (store, tx) = (store.clone(), answer.clone());
                async move {
                    let msg = match store.users().await {
                        Ok(users) => Msg::UsersLoaded(users),
                        Err(error) => Msg::StoreFailed(error.to_string()),
                    };
                    let _ = tx.send(msg);
                }
            });
        }
        Effect::LoadCounts => {
            let (store, answer) = (store.clone(), tx.clone());
            coalesce::coalesced(&tx.reads.counts, tx, (), move |()| {
                let (store, tx) = (store.clone(), answer.clone());
                async move {
                    let msg = match store.project_task_counts().await {
                        Ok(counts) => Msg::CountsLoaded(counts),
                        Err(error) => Msg::StoreFailed(error.to_string()),
                    };
                    let _ = tx.send(msg);
                }
            });
        }
        Effect::LoadPending => {
            let (store, answer) = (store.clone(), tx.clone());
            coalesce::coalesced(&tx.reads.pending, tx, (), move |()| {
                let (store, tx) = (store.clone(), answer.clone());
                async move {
                    if let Ok(health) = store.queue_health().await {
                        let _ = tx.send(Msg::PendingLoaded(health));
                    }
                }
            });
        }
        Effect::RememberProject(project) => {
            let store = store.clone();
            spawn_effect(tx.clone(), async move {
                let value = project.map_or_else(String::new, |id| id.get().to_string());
                // A failure here costs the user their landing project next launch. It is
                // not worth a toast in front of the task they are reading.
                let _ = store.set_state(LAST_PROJECT, &value).await;
            });
        }
        Effect::DismissKeysNotice => {
            let store = store.clone();
            spawn_effect(tx.clone(), async move {
                // A failure costs one more sight of the notice next launch, which is
                // not worth a toast over the list.
                let _ = store.set_state(KEYS_NOTICE_DISMISSED, "1").await;
            });
        }
        Effect::Apply { write, mutation } => {
            let (store, tx) = (store.clone(), tx.clone());
            let sync = sync.cloned();
            spawn_effect(tx.clone(), async move {
                // Answered by number either way, so the model knows which of the edits it
                // drew the store now holds, and stops laying it over loads issued after
                // this -- see `tui_do_ui::Overlay`.
                match store.queue(mutation).await {
                    // A create is the one mutation whose id the interface did not know
                    // when it drew the row: `Store::queue` allocates the provisional id
                    // inside its own transaction. Told before the reload, the selection
                    // names a row that exists by the time the list comes back.
                    Ok(entry) => {
                        if let Mutation::CreateTask { task } = &entry.mutation {
                            let _ = tx.send(Msg::TaskCreated { write, id: task.id });
                        }
                        let _ = tx.send(Msg::Written(write));
                    }
                    // The model has already shown the change. Saying the store refused it
                    // is the only honest thing to do; the reload below puts the list back
                    // to what was actually written.
                    Err(error) => {
                        let _ = tx.send(Msg::WriteFailed {
                            write,
                            message: error.to_string(),
                        });
                    }
                }
                // Reload from the store rather than trusting the optimistic copy, then
                // send it on its way. `spawn_sync` coalesces, so a burst of edits is one
                // push rather than one each.
                let _ = tx.send(Msg::Reload);
                if let Some(sync) = sync {
                    spawn_sync(sync, tx, Pass::Push, Trigger::Scheduled);
                }
            });
        }
        Effect::SyncNow => {
            if let Some(sync) = sync {
                spawn_sync(Arc::clone(sync), tx.clone(), Pass::Delta, Trigger::Asked);
            }
        }
        Effect::SyncFull => {
            if let Some(sync) = sync {
                spawn_sync(Arc::clone(sync), tx.clone(), Pass::Full, Trigger::Asked);
            }
        }
        // The loop notices `model.running` rather than being killed from here, so the
        // terminal is restored on the way out of `run` in every case.
        Effect::OpenUrl(url) => {
            let tx = tx.clone();
            // `spawn_blocking` rather than `tokio::process`, which would need another
            // tokio feature for one call. The opener hands the URL to a handler and exits
            // immediately, so this waits on a fork-exec and not on a browser.
            spawn_blocking_effect(tx.clone(), move || {
                let result = std::process::Command::new(URL_OPENER)
                    .arg(&url)
                    // Inherited handles would let the handler write over the alternate
                    // screen -- the interface owns this terminal.
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status();
                let failure = match result {
                    Ok(status) if status.success() => None,
                    Ok(status) => Some(format!("{URL_OPENER} exited with {status}")),
                    Err(error) => Some(format!("could not run {URL_OPENER}: {error}")),
                };
                if let Some(message) = failure {
                    let _ = tx.send(Msg::EffectFailed(message));
                }
            });
        }
        Effect::CopyToClipboard(text) => {
            if let Err(error) = osc52(&text) {
                let _ = tx.send(Msg::EffectFailed(format!("could not copy: {error}")));
            }
        }
        Effect::Quit => {}
        // `Effect` is `non_exhaustive` so Phase 4 can add write effects without breaking
        // this crate. An effect this build does not know about is not silently dropped.
        other => tracing::warn!(?other, "unhandled effect"),
    }
}

/// Build the sync engine, if there is a credential to use.
///
/// Returns the engine and, when there is none, why. Neither case is an error: tui-do
/// without a token is a perfectly good read-only session against the cache, and refusing
/// to start would be the freeze-on-network behaviour this project exists to remove.
fn build_sync(
    config: &Config,
    config_path: &std::path::Path,
    store: &Store,
) -> (Option<Arc<Sync>>, Option<String>) {
    let lookup = match config.resolve_token(config_path) {
        Ok(lookup) => lookup,
        Err(error) => return (None, Some(format!("Not syncing: {error}"))),
    };
    // A locked keychain falls through to the file, and saying so is the whole reason the
    // outcome is distinguished from "there is no keychain": without this line the user is
    // told "no API token configured" when the true answer is "unlock your keyring", which
    // is a diagnosis they cannot reach from the message.
    let token = match lookup.token {
        Some(token) => token,
        None => {
            let reason = lookup.note.map_or_else(
                || "No API token configured".to_string(),
                |note| format!("No API token: {note}"),
            );
            return (
                None,
                Some(format!("{reason} — showing the local cache only.")),
            );
        }
    };
    match Client::builder(&config.server.url)
        .credentials(tui_do_api::Credentials::api_token(token))
        .build()
    {
        Ok(client) => (Some(Arc::new(Sync::new(client, store.clone()))), None),
        Err(error) => (
            None,
            Some(format!(
                "Not syncing: {} is unusable: {error}",
                config.server.url
            )),
        ),
    }
}

/// Which halves of a sync to run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pass {
    /// Send queued changes and nothing else. What an edit asks for: it is one request per
    /// entry, where a pull of this instance is seventy-eight pages.
    Push,
    /// Push, then pull only what the server says has changed. What `r` asks for, and what
    /// the timer runs between hourly full passes (see [`timer_pass`]).
    ///
    /// Between a push and a full pass in every sense: it costs a page rather than
    /// seventy-eight, and it sees everything a full pass does except deletions.
    Delta,
    /// Push, then pull everything, removing what the server no longer has. What startup
    /// and `R` ask for, and what the timer runs once [`FULL_PULL_EVERY`] has passed
    /// without one.
    Full,
}

/// Run one sync pass, forwarding its events into the loop.
/// Nothing was asked for while a pass was running.
const NOTHING_AGAIN: u8 = 0;
/// A push was asked for while a pass was running.
const PUSH_AGAIN: u8 = 1;
/// An incremental pass was asked for while a pass was running.
const DELTA_AGAIN: u8 = 2;
/// A full pass was asked for while a pass was running. Ordered above the other two on
/// purpose: `fetch_max` then keeps the most thorough of what was asked for, and each
/// does everything the one below it does.
const FULL_AGAIN: u8 = 3;

/// Who asked for a pass, which decides whether it waits out a failed entry's backoff.
///
/// Not folded into [`Pass`] because the two are independent: `R` and the timer both ask
/// for a full pass and only one of them is a person saying "try now". Folding them would
/// make five variants where the fifth is the one that matters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Trigger {
    /// Startup, the timer, and the push that follows a write. Backoff stands.
    Scheduled,
    /// A keystroke. The user knows something the queue does not.
    Asked,
}

const fn again_code(pass: Pass) -> u8 {
    match pass {
        Pass::Push => PUSH_AGAIN,
        Pass::Delta => DELTA_AGAIN,
        Pass::Full => FULL_AGAIN,
    }
}

/// Set once the quit flush has begun, after which no pass is started.
///
/// The flush pushes on its own, outside `spawn_sync`'s `RUNNING` guard, so a pass that
/// started beside it -- a running pass's `AGAIN` tail, a late `Effect::Apply`, the timer
/// -- would be a second drain of the same queue, and two drains can each read a create
/// before either marks it sent. Never cleared: the process is on its way out.
static SYNC_STOPPED: AtomicBool = AtomicBool::new(false);

fn spawn_sync(sync: Arc<Sync>, tx: LoopTx, pass: Pass, trigger: Trigger) {
    if SYNC_STOPPED.load(Ordering::SeqCst) {
        return;
    }
    /// One pass at a time. Two overlapping passes would push the same queue twice, and
    /// the queue is ordered — an entry sent twice is a task created twice.
    static RUNNING: AtomicBool = AtomicBool::new(false);
    /// What was asked for while a pass was running: nothing, a push, or a full pass.
    ///
    /// Without this, a change made during the startup pull — half a minute against 3,877
    /// tasks — sits in the queue until the five-minute timer comes round. A full pass
    /// used to be dropped here outright, so `r` during a pass did nothing at all while
    /// the status line said "starting".
    static AGAIN: AtomicU8 = AtomicU8::new(NOTHING_AGAIN);
    /// Whether any of what was asked for while a pass was running was asked for by a
    /// person. Kept beside `AGAIN` rather than encoded into it because reach and trigger
    /// are independent: `fetch_max` over a single code would let a scheduled *full* pass
    /// outrank an asked-for *delta* and quietly drop the force, which is the bug this
    /// whole change is about, reached one pass later.
    static AGAIN_ASKED: AtomicBool = AtomicBool::new(false);

    if RUNNING.swap(true, Ordering::SeqCst) {
        // A full pass supersedes a queued push, because it does one anyway.
        AGAIN.fetch_max(again_code(pass), Ordering::SeqCst);
        if trigger == Trigger::Asked {
            AGAIN_ASKED.store(true, Ordering::SeqCst);
        }
        return;
    }
    let handle = spawn_effect(tx.clone(), async move {
        let (events_tx, mut events_rx) = mpsc::unbounded_channel();
        let forward = {
            let tx = tx.clone();
            spawn_effect(tx.clone(), async move {
                while let Some(event) = events_rx.recv().await {
                    let _ = tx.send(Msg::Sync(event));
                }
            })
        };

        let engine = (*sync).clone().with_events(events_tx);
        let outcome = match (pass, trigger) {
            // A write's own push is not a person asking, so an entry that just failed is
            // left to its schedule rather than retried on the back of an unrelated edit.
            (Pass::Push, _) => engine.push().await.map(|_| ()),
            (Pass::Delta, Trigger::Scheduled) => engine.delta().await.map(|_| ()),
            (Pass::Full, Trigger::Scheduled) => engine.once().await.map(|_| ()),
            (Pass::Delta, Trigger::Asked) => engine.asked(Reach::Incremental).await.map(|_| ()),
            (Pass::Full, Trigger::Asked) => engine.asked(Reach::Full).await.map(|_| ()),
        };
        // Drop the engine so its sender closes, then let the forwarder finish draining.
        //
        // This used to `abort()` here, which raced the last event out of the channel --
        // and the last event is `Finished`, the only one that reloads. The pull updated
        // the store, the interface was never told, and the list stayed as it was until
        // the next launch. A task added by `tui-do add` was in the database and not on
        // the screen, which is exactly what it looked like from the outside.
        drop(engine);
        let _ = forward.await;
        // Only after the forwarder has finished: sent before it, a `Sending` still in
        // the channel lands after `Failed` and leaves "sending" on screen for good.
        if let Err(error) = outcome {
            let _ = tx.send(Msg::Sync(tui_do_core::SyncEvent::Failed {
                phase: tui_do_core::sync::Phase::Pull,
                message: error.to_string(),
            }));
        }
        RUNNING.store(false, Ordering::SeqCst);
        // Whatever was asked for while this pass was running still has to happen.
        let again_trigger = if AGAIN_ASKED.swap(false, Ordering::SeqCst) {
            Trigger::Asked
        } else {
            Trigger::Scheduled
        };
        match AGAIN.swap(NOTHING_AGAIN, Ordering::SeqCst) {
            PUSH_AGAIN => spawn_sync(sync, tx, Pass::Push, again_trigger),
            // `r` during the startup pull landed here and was dropped: the model had
            // already been told a sync was starting, so the status line said "Syncing"
            // and the toast promised changed tasks, and then nothing ran until the
            // five-minute timer. Every code `again_code` can produce needs an arm.
            DELTA_AGAIN => spawn_sync(sync, tx, Pass::Delta, again_trigger),
            FULL_AGAIN => spawn_sync(sync, tx, Pass::Full, again_trigger),
            _ => {}
        }
    });
    if let Ok(mut slot) = in_flight().lock() {
        *slot = Some(handle);
    }
}

/// The pass currently running, so quitting can stop waiting for it.
fn in_flight() -> &'static Mutex<Option<tokio::task::JoinHandle<()>>> {
    static IN_FLIGHT: OnceLock<Mutex<Option<tokio::task::JoinHandle<()>>>> = OnceLock::new();
    IN_FLIGHT.get_or_init(|| Mutex::new(None))
}

/// Send whatever is still queued before the process ends.
///
/// Without this, an edit made in the first half-minute waits behind the startup pull —
/// seventy-eight pages against this instance — and quitting leaves it queued until the
/// next launch. Nothing is lost either way, but a task marked done that the server has
/// not heard about is a bad thing to walk away from.
///
/// The running pass is aborted first rather than waited for. A pull that stops early has
/// applied whole pages and not yet run the retain that removes what the server dropped,
/// so the worst it leaves behind is a store that is a little stale — which the next pull
/// corrects. A push that raced that pull could have its new task deleted by the retain
/// and would have to wait for the next pull to reappear.
///
/// What it says goes to `out`, which is stdout but after SIGHUP, when it is a sink: the
/// terminal has gone, and `println!` panics on a write that fails (BUG-31). Every write
/// ignores its error for the same reason.
async fn flush_on_exit(store: &Store, sync: &Arc<Sync>, out: &mut (dyn Write + Send)) {
    // Before anything else, so whatever is in flight below is the last pass there is.
    SYNC_STOPPED.store(true, Ordering::SeqCst);
    let pending = store.pending_count().await.unwrap_or(0);
    if pending <= 0 {
        return;
    }
    // A pass mid-*pull* is aborted, as before: whole pages are applied and the retain has
    // not run, so the store is a little stale and the next pull fixes it.
    //
    // A pass mid-*push* is not. The request may already have reached the server, and
    // pushing again below would send the same outbox entry a second time -- which for a
    // `CreateTask` means a second task, because Vikunja has no idempotency key and
    // `CreateTask` has no read-before-retry the way `CreateLabel` does. Reproduced by hand
    // on 2026-08-31 with the dev container paused: ids 3889 and 3891, both
    // `Bug #3 - pause container test`.
    //
    // So a running push is given the same grace the flush itself gets. If it finishes, its
    // entries are settled and the push below has nothing left to duplicate. If it does not,
    // the entry stays queued for the next launch -- which is the right trade: a change that
    // arrives late is better than a task that exists twice.
    //
    // A push that *panicked* is the same case, reached without the flag: unwinding cleared
    // it, but the request may still have gone out. There is nothing left to wait for, so
    // it goes straight to walking away.
    if sync.push_interrupted() {
        let _ = writeln!(
            out,
            "A send stopped partway; leaving the rest queued for next time."
        );
        return;
    }
    if sync.is_pushing() {
        // The same budget the dots below get: five seconds, one per tick.
        let deadline = FLUSH_TICK * FLUSH_DOTS;
        let handle = in_flight().lock().ok().and_then(|mut slot| slot.take());
        if let Some(handle) = handle {
            if tokio::time::timeout(deadline, handle).await.is_err() {
                // Still pushing when the grace ran out. Walk away without pushing: what is
                // queued is safe, and re-sending it is what creates the duplicate.
                //
                // BUG-23: this always said "Still sending", including the time it was
                // printed after five seconds with no request on the wire at all. Say
                // which it is.
                if sync.is_requesting() {
                    let _ = writeln!(out, "Still sending; leaving the rest queued for next time.");
                } else {
                    let _ = writeln!(
                        out,
                        "Waiting to send; leaving the rest queued for next time."
                    );
                }
                return;
            }
        }
    } else {
        let handle = in_flight().lock().ok().and_then(|mut slot| slot.take());
        if let Some(handle) = handle {
            handle.abort();
            // `abort` only asks. A pass being polled on another worker runs on to its next
            // await, so it is waited out -- briefly, since cancelling is all that is left
            // -- before the push below starts beside it.
            let _ = tokio::time::timeout(FLUSH_TICK, handle).await;
        }
    }
    // The slot holds the pass spawned last, which is not always the one running: a
    // pass's `AGAIN` tail can store its successor before the pass that spawned it has
    // stored itself. Whatever is still pushing now is a second drain, so walk away.
    if sync.is_pushing() {
        let _ = writeln!(out, "Still sending; leaving the rest queued for next time.");
        return;
    }

    let changes = if pending == 1 { "change" } else { "changes" };
    let _ = write!(out, "Sending {pending} queued {changes}");
    let _ = out.flush();

    // A dot a second, rather than one message and a silent wait. On an unroutable
    // server the push cannot answer, and a still screen there reads as a hang —
    // which is the one thing this project is not allowed to look like. The dots
    // are also the clock: when the last one lands, the wait is over.
    // Listened to for rejections, as `tui-do add` does (BUG-35): a refused entry is
    // rolled back rather than deferred, so the report alone cannot say why.
    let (events_tx, mut events) = mpsc::unbounded_channel();
    let syncer = Sync::clone(sync).with_events(events_tx);
    let push = syncer.push();
    tokio::pin!(push);
    let mut outcome = None;
    for _ in 0..FLUSH_DOTS {
        tokio::select! {
            answered = &mut push => { outcome = Some(answered); break }
            () = tokio::time::sleep(FLUSH_TICK) => {
                let _ = write!(out, ".");
                let _ = out.flush();
            }
        }
    }
    let _ = writeln!(out);

    let mut heard = Vec::new();
    while let Ok(event) = events.try_recv() {
        heard.push(event);
    }
    for line in flush_said(outcome, &heard) {
        let _ = writeln!(out, "{line}");
    }
}

/// What the quit flush says once its push has answered, or has not.
///
/// A refusal is said first and with the server's reason. `PushReport::is_complete` is
/// `deferred == 0` and a refused entry is rolled back rather than deferred, so reading
/// the report alone said "Sent." for a change the server had turned down -- the flush's
/// half of what BUG-35 was for `tui-do add`.
fn flush_said(
    outcome: Option<tui_do_core::Result<tui_do_core::sync::PushReport>>,
    events: &[SyncEvent],
) -> Vec<String> {
    let report = match outcome {
        Some(Ok(report)) => report,
        Some(Err(error)) => {
            return vec![format!(
                "Still queued — could not reach the server: {error}"
            )]
        }
        None => return vec!["Still queued — the server did not answer in time.".to_string()],
    };
    let mut lines = Vec::new();
    if report.rejected > 0 {
        let reasons: Vec<String> = events
            .iter()
            .filter_map(|event| match event {
                SyncEvent::Rejected { message, .. } => Some(printable(message)),
                _ => None,
            })
            .collect();
        let changes = if report.rejected == 1 {
            "change"
        } else {
            "changes"
        };
        lines.push(if reasons.is_empty() {
            format!("The server refused {} {changes}.", report.rejected)
        } else {
            format!(
                "The server refused {} {changes}: {}",
                report.rejected,
                reasons.join("; ")
            )
        });
    }
    if report.deferred > 0 {
        lines.push(format!(
            "{} still queued; the next run will send them.",
            report.deferred
        ));
    } else if report.rejected == 0 {
        lines.push("Sent.".to_string());
    } else if report.sent > 0 {
        lines.push("The rest were sent.".to_string());
    }
    lines
}

/// Server-written text, made safe to print on one line of a terminal.
///
/// `tui-do add` prints project titles straight to stdout, and that terminal is in
/// normal mode -- no raw mode, no alternate screen, and none of ratatui's filtering,
/// which is what protects these same strings inside the interface. A title is written
/// on the server and on a shared instance it is written by someone else, so it arrives
/// as arbitrary bytes: an ESC opens OSC 52 and writes the sender's text into the
/// reader's clipboard, ready for their next paste into a shell.
///
/// Every control character goes, `\n` and `\t` included, because every caller
/// interpolates into the middle of a sentence where neither is legitimate -- and a
/// newline forges a line of output that reads as tui-do's own.
fn printable(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { '\u{fffd}' } else { c })
        .collect()
}

/// Name the project a task landed in.
///
/// By its id when the store has never heard of it, which is what `+#12` naming a
/// project this box has not pulled yet leaves behind.
fn project_label(
    projects: &[tui_do_core::models::Project],
    id: tui_do_core::models::ProjectId,
) -> String {
    projects
        .iter()
        .find(|project| project.id == id)
        .map_or_else(|| id.to_string(), |project| printable(&project.title))
}

/// The dates a quick-add set, as the tail of `tui-do add`'s confirmation line.
///
/// `, due Fri 27 Aug 2027` — empty when neither was set. A parsed date used to be
/// silent: the only sign of one was an echoed title shorter than what was typed, which
/// is how `llvm 1.6 GB` came to be due in June 2027 (BUG-24). The time of day is shown
/// only when it is not the field's default (`DateField::default_time`, the same one the
/// parser fills in), so a plain date stays short. The words are `DateField::describe`'s,
/// which `Parsed::summary` uses too, so the two cannot name a date differently.
fn dates_suffix<Tz: chrono::TimeZone>(
    due: Option<chrono::DateTime<chrono::Utc>>,
    start: Option<chrono::DateTime<chrono::Utc>>,
    zone: &Tz,
) -> String
where
    Tz::Offset: std::fmt::Display,
{
    use tui_do_core::quickadd::DateField;
    let mut suffix = String::new();
    if let Some(at) = due {
        suffix.push_str(", ");
        suffix.push_str(&DateField::Due.describe(at, zone));
    }
    if let Some(at) = start {
        suffix.push_str(", ");
        suffix.push_str(&DateField::Start.describe(at, zone));
    }
    suffix
}

/// A project whose title begins with `name` and carries on, if exactly one does.
///
/// Exactly one, because two would make the suggestion a guess; and the first *word* has
/// to match rather than merely the prefix, so `Din` does not offer `Dinner Places` when
/// the user simply mistyped a project that does not exist.
fn longer_project(projects: &[tui_do_core::models::Project], name: &str) -> Option<String> {
    let mut found = projects.iter().filter(|project| {
        project.id.get() > 0
            && !project.is_archived
            && project
                .title
                .split_whitespace()
                .next()
                .is_some_and(|first| first.eq_ignore_ascii_case(name))
            && !project.title.eq_ignore_ascii_case(name)
    });
    let first = found.next()?;
    found.next().is_none().then(|| printable(&first.title))
}

/// Read terminal events on a blocking thread.
///
/// crossterm's reader blocks, so it lives on its own thread rather than in the async
/// runtime, and polls so it can notice that tui-do is shutting down.
///
/// A panic on a plain thread is as silent as one in a task: the thread ends, the hook
/// restores the terminal, and the interface carries on with no keyboard, so `q` can no
/// longer reach it. So this thread reports its panic the way a task does, and a terminal
/// that stops answering ends the session through the same exit.
fn spawn_input(tx: LoopTx, stop: Arc<AtomicBool>) {
    std::thread::spawn(move || tx.guarded(|| read_input(&tx, &stop, next_event)));
}

/// The next terminal event, or `None` if none arrived within `timeout`.
fn next_event(timeout: Duration) -> io::Result<Option<crossterm::event::Event>> {
    if crossterm::event::poll(timeout)? {
        crossterm::event::read().map(Some)
    } else {
        Ok(None)
    }
}

/// The input thread's body: forward keys and resizes until told to stop.
///
/// `events` is [`next_event`] everywhere but the tests. crossterm's reader is global and
/// reads the process's real terminal, so without it the error path could only be reached
/// by breaking the terminal under the test runner.
fn read_input(
    tx: &LoopTx,
    stop: &AtomicBool,
    mut events: impl FnMut(Duration) -> io::Result<Option<crossterm::event::Event>>,
) {
    while !stop.load(Ordering::SeqCst) {
        let msg = match events(INPUT_POLL) {
            Ok(Some(crossterm::event::Event::Key(key))) => Msg::Key(key),
            Ok(Some(crossterm::event::Event::Resize(width, height))) => Msg::Resize(width, height),
            Ok(_) => continue,
            // Returning quietly here was BUG-25: input gone for good, and the interface
            // drawing on with no way to quit it.
            Err(error) => {
                tx.fatal(Fatal::InputLost(error.to_string()));
                return;
            }
        };
        // The loop has gone, so there is nobody left to type at.
        if tx.send(msg).is_err() {
            return;
        }
    }
}

/// Report the time, so `update` never has to read a clock.
fn spawn_ticks(tx: LoopTx, stop: Arc<AtomicBool>) {
    spawn_effect(tx.clone(), async move {
        let mut interval = tokio::time::interval(TICK);
        loop {
            interval.tick().await;
            if stop.load(Ordering::SeqCst) || tx.send(Msg::Tick(now())).is_err() {
                return;
            }
        }
    });
}

/// How long the timer goes without a full pull before it does one.
///
/// A full pull is the only kind that notices a task deleted in another client, and against
/// the author's store it is ~79 pages, mostly done tasks. Every five minutes that was 288 of
/// them a day; hourly is 24, and the cost is that a deletion made elsewhere can take up to
/// an hour to disappear here unless `R` is pressed. Decided 2026-11-10 (`md/TODO.md`, lead 1).
const FULL_PULL_EVERY: chrono::TimeDelta = chrono::TimeDelta::hours(1);

/// Which pass a timer tick runs.
///
/// The first tick is startup and always pulls in full. After that, full only when no full
/// pull -- the timer's or `R`'s -- has reconciled within [`FULL_PULL_EVERY`]; a store that
/// cannot say when one last did gets a full one, because that is the pass that is never
/// wrong, only slow. A stamp in the future means the clock has gone back since it was
/// written, and counts as stale: read as fresh, it would hold off every full pull until
/// the clock caught up.
fn timer_pass(
    first: bool,
    last_reconcile: Option<chrono::DateTime<chrono::Utc>>,
    now: chrono::DateTime<chrono::Utc>,
) -> Pass {
    match last_reconcile {
        Some(at) if !first && at <= now && now - at < FULL_PULL_EVERY => Pass::Delta,
        _ => Pass::Full,
    }
}

/// Sync on a timer, and once at startup.
fn spawn_sync_timer(
    sync: Arc<Sync>,
    store: Store,
    settings: tui_do_core::config::SyncConfig,
    stop: Arc<AtomicBool>,
    tx: LoopTx,
) {
    if !settings.enabled {
        return;
    }
    spawn_effect(tx.clone(), async move {
        let mut interval =
            tokio::time::interval(Duration::from_secs(settings.interval_seconds.max(30)));
        let mut first = true;
        loop {
            interval.tick().await;
            if stop.load(Ordering::SeqCst) {
                return;
            }
            let last_reconcile = store.last_reconcile().await.ok().flatten();
            let pass = timer_pass(first, last_reconcile, chrono::Utc::now());
            first = false;
            spawn_sync(Arc::clone(&sync), tx.clone(), pass, Trigger::Scheduled);
        }
    });
}

/// What `o` can usefully do with a link on this box.
///
/// Read once, here, for the same reason [`color_depth`] is: `tui-do-ui` is a pure function
/// of what it is told, and "is there a browser to open onto" is something only the runtime
/// can ask.
///
/// `xdg-open` over SSH is worse than useless -- it either fails or opens a browser on the
/// machine at the far end, which is not where the person is. tui-do is used across a fleet
/// of boxes over Tailscale, so that is the ordinary case here, not the exotic one.
/// Whether to greet this launch with the notice that the keys moved onto Vikunja's.
///
/// Only a store that has synced before has a user who learned the old keys; a fresh
/// install has nothing to unlearn. Read errors answer "no": a notice is not worth
/// guessing about.
async fn keys_notice_owed(store: &Store) -> bool {
    let dismissed = store.state(KEYS_NOTICE_DISMISSED).await.ok().flatten();
    let synced = store.state(LAST_PULL).await.ok().flatten();
    dismissed.is_none() && synced.is_some()
}

/// The release that moved the keys. Pinned rather than read from `CARGO_PKG_VERSION`,
/// because someone skipping a release would otherwise be sent to notes that never mention
/// the change. **It must match the tag the change ships under** -- see `md/TODO.md`.
const KEYS_CHANGED_IN: &str = "v1.2.0";

/// The release notes that explain the key change.
fn release_notes_url() -> String {
    format!(
        "{}/releases/tag/{KEYS_CHANGED_IN}",
        env!("CARGO_PKG_REPOSITORY")
    )
}

pub(crate) fn url_action() -> UrlAction {
    if std::env::var_os("SSH_CONNECTION").is_some() || std::env::var_os("SSH_TTY").is_some() {
        return UrlAction::Copy;
    }
    // A local macOS session always has a window server to open onto, and sets neither of
    // these -- checking for them here would send every Mac down the copy path and never
    // open anything. The `SSH_*` check above is the one that still matters there.
    #[cfg(not(target_os = "macos"))]
    if std::env::var_os("WAYLAND_DISPLAY").is_none() && std::env::var_os("DISPLAY").is_none() {
        return UrlAction::Copy;
    }
    UrlAction::Open
}

/// The program that hands a URL to the desktop.
///
/// `xdg-open` is the freedesktop entry point, and does not exist on macOS, which has `open`
/// for the same job. Named once so the command and the error messages that quote it cannot
/// drift apart.
#[cfg(target_os = "macos")]
pub(crate) const URL_OPENER: &str = "open";
#[cfg(not(target_os = "macos"))]
pub(crate) const URL_OPENER: &str = "xdg-open";

/// Put text in the clipboard with OSC 52.
///
/// Chosen over `wl-copy`/`xclip` precisely because it is *not* a local subprocess: the
/// escape sequence travels back through the SSH connection and the terminal emulator at
/// the user's end puts it on **their** clipboard. That is the whole point on a fleet.
///
/// Not universally supported, and some terminals ship with it off. A terminal that ignores
/// it leaves the clipboard untouched with nothing to report -- the sequence is consumed
/// either way -- which is why the toast says what was copied rather than merely "copied".
fn osc52(text: &str) -> std::io::Result<()> {
    use base64::Engine as _;
    use std::io::Write as _;

    let encoded = base64::engine::general_purpose::STANDARD.encode(text.as_bytes());
    let mut out = std::io::stdout();
    // `]52;c;<base64>`. Written outside the draw cycle, which is safe: OSC 52
    // moves no cursor and paints no cell, so it cannot disturb the frame on screen.
    write!(out, "\x1b]52;c;{encoded}\x07")?;
    out.flush()
}

/// How much colour this terminal can show.
///
/// Read once, here, rather than anywhere in `tui-do-ui`: the UI layer is a pure function
/// of what it is told, and "what the terminal can do" is something the runtime knows.
fn color_depth() -> ColorDepth {
    if std::env::var_os("NO_COLOR").is_some() {
        return ColorDepth::None;
    }
    let colorterm = std::env::var("COLORTERM").unwrap_or_default();
    if colorterm.contains("truecolor") || colorterm.contains("24bit") {
        return ColorDepth::TrueColor;
    }
    let term = std::env::var("TERM").unwrap_or_default();
    if term.contains("256") || term.contains("kitty") || term.contains("alacritty") {
        return ColorDepth::Ansi256;
    }
    if term.is_empty() || term == "dumb" {
        return ColorDepth::None;
    }
    ColorDepth::Ansi16
}

/// What tui-do has cached, for an error message that says what to do next.
fn describe(projects: &[tui_do_core::models::Project]) -> String {
    let real = projects
        .iter()
        .filter(|project| project.id.get() > 0)
        .count();
    if real == 0 {
        "no projects at all; run tui-do once to sync, or check the server is reachable".to_string()
    } else {
        format!("{real} projects; run tui-do to refresh the list")
    }
}

/// Open the store where it lives: `TUI_DO_DB`, or the platform's data directory.
///
/// # Errors
/// No data directory to put it in, or a store that cannot be opened or migrated.
pub async fn open_store() -> anyhow::Result<Store> {
    let store_path = store_path()?;
    Store::open(&store_path)
        .await
        .with_context(|| format!("could not open the store at {}", store_path.display()))
}

/// Open the store read-only, without creating or migrating it: for a dry run, and for the
/// names the prompt completes from, neither of which may leave a store behind.
///
/// # Errors
/// [`tui_do_core::CoreError::NotReady`] as it stands, since it already says what to run;
/// anything else with the path it was opening.
pub async fn open_existing_store() -> anyhow::Result<Store> {
    open_existing_at(&store_path()?).await
}

/// [`open_existing_store`], at `store_path`.
async fn open_existing_at(store_path: &std::path::Path) -> anyhow::Result<Store> {
    match Store::open_existing_read_only(store_path).await {
        Ok(store) => Ok(store),
        Err(error @ tui_do_core::CoreError::NotReady(_)) => Err(error.into()),
        Err(error) => Err(anyhow::Error::new(error).context(format!(
            "could not open the store at {}",
            store_path.display()
        ))),
    }
}

/// A runtime on this thread, with no I/O or timer driver, for a command that reads the
/// local store and exits: shell completion and `tui-do add --tokens`. The store's reads
/// are `spawn_blocking`, which needs neither driver, and nothing here may touch a network.
///
/// # Errors
/// The runtime could not be built.
pub fn store_only_runtime() -> anyhow::Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .context("could not start the async runtime")
}

pub(crate) fn store_path() -> anyhow::Result<std::path::PathBuf> {
    Store::default_path().context("could not decide where to keep the store")
}

/// What `tui-do add`'s prompt completes from: the projects, labels and users in the
/// local store. Read here, by the runtime, because nothing in `tui-do-ui` may (Rule 1).
///
/// Opened read-only, so a prompt that is cancelled, or ends in a dry run, leaves no
/// store behind where there was none. No store, or an unreadable list, is only an empty
/// vocabulary: the prompt still works, without completion and with the static example.
pub async fn vocabulary() -> tui_do_ui::assist::Vocabulary {
    match store_path() {
        Ok(path) => vocabulary_at(&path).await,
        Err(_) => tui_do_ui::assist::Vocabulary::default(),
    }
}

/// [`vocabulary`], from the store at `store_path`.
pub(crate) async fn vocabulary_at(store_path: &std::path::Path) -> tui_do_ui::assist::Vocabulary {
    let Ok(store) = open_existing_at(store_path).await else {
        return tui_do_ui::assist::Vocabulary::default();
    };
    vocabulary_of(&store).await.unwrap_or_default()
}

/// The names in `store` a quick-add line can use.
async fn vocabulary_of(store: &Store) -> anyhow::Result<tui_do_ui::assist::Vocabulary> {
    let projects = store
        .projects(ProjectFilter::default(), ProjectSort::default())
        .await?;
    let labels = store
        .labels(LabelFilter::default(), LabelSort::default())
        .await?;
    let users = store.usernames().await?;
    Ok(tui_do_ui::assist::Vocabulary::new(
        &projects, &labels, users,
    ))
}

/// `tui-do add --tokens`: every name a quick-add line may use, from the local store, and
/// the token syntax. Read-only and offline, like `--dry-run`.
///
/// # Errors
/// No store yet, with the message that says to run tui-do once; or a store that cannot
/// be read.
pub async fn tokens() -> anyhow::Result<String> {
    let store = open_existing_store().await?;
    Ok(token_listing(&vocabulary_of(&store).await?))
}

/// The `--tokens` listing for `vocabulary`: projects, labels and users, one name to a
/// line and each written exactly as the canonical line writes it, then the syntax.
///
/// Every name goes through [`printable`]: it was written on a server, possibly by
/// somebody else, and a newline in it would forge a line of this listing.
fn token_listing(vocabulary: &tui_do_ui::assist::Vocabulary) -> String {
    use std::fmt::Write as _;
    use tui_do_core::quickadd::name_token;
    let mut out = String::new();
    for (heading, sigil, names) in [
        ("projects", '+', &vocabulary.projects),
        ("labels", '*', &vocabulary.labels),
        ("users", '@', &vocabulary.users),
    ] {
        let _ = writeln!(out, "{heading}:");
        if names.is_empty() {
            let _ = writeln!(out, "(none in the local store)");
        }
        for name in names {
            let _ = writeln!(out, "{}", printable(&name_token(sigil, name)));
        }
        out.push('\n');
    }
    out.push_str("syntax:\n");
    for line in tui_do_ui::assist::TOKEN_SYNTAX {
        let _ = writeln!(out, "  {line}");
    }
    out.push_str(
        "\nbest form: title +project *label @user !N due YYYY-MM-DD start YYYY-MM-DD every <n> <unit>\n\
         check a line first: tui-do add --dry-run '<line>'\n",
    );
    out
}

/// Ask for a task on a few lines at the bottom of the terminal, for `tui-do add` run
/// with no text. `None` when the user cancelled.
///
/// Synchronous, and called outside the async runtime: there is nothing to await while a
/// person types. The terminal goes back through `TerminalGuard`'s `Drop` and panic hook,
/// the same ones the interface uses, on every way out -- and a SIGTERM, SIGHUP or
/// SIGQUIT is one of those ways, ending the process with `128 + signal` (BUG-31).
/// `runtime` is the one `tui-do add` goes on to run on: the signal handlers belong to it,
/// and its worker threads deliver a signal while this thread waits for keys.
///
/// # Errors
/// A terminal that refuses raw mode or cannot be drawn on.
pub fn ask_for_task(
    runtime: &tokio::runtime::Handle,
    vocabulary: tui_do_ui::assist::Vocabulary,
    dry_run: bool,
) -> anyhow::Result<Option<String>> {
    let mut prompt = prompt::Prompt::new(vocabulary, now(), Theme::new(color_depth()), dry_run);
    // Before raw mode, as in `run` (BUG-31). Spawned plainly rather than through
    // `spawn_effect`, which reports to an event loop the prompt does not have; the
    // listener does nothing that could panic. Its receiver goes when this returns, and a
    // signal after that ends the process with its code as the default action would --
    // the terminal is no longer raw by then, but `add`'s push may be mid-request
    // (`signals::listen` says what that costs).
    let (to, mut caught) = mpsc::unbounded_channel();
    let listener = {
        let _context = runtime.enter();
        signals::listen(to)
    };
    if let Some(listener) = listener {
        runtime.spawn(listener);
    }
    let mut guard = TerminalGuard::inline(prompt::HEIGHT)?;
    let answer = prompt::ask(guard.terminal(), &mut prompt, next_event, || {
        caught.try_recv().ok()
    });
    guard.release_for(match &answer {
        Ok(prompt::Answer::Signalled(signal)) => Some(*signal),
        _ => None,
    });
    match answer? {
        prompt::Answer::Add(text) => {
            // Left in the scrollback where the prompt was, so the line that was added
            // sits above what `add` says about it.
            println!("{}{}", prompt.prefix(), printable(&text));
            Ok(Some(text))
        }
        prompt::Answer::Cancelled => Ok(None),
        // Nothing was typed that could be added, nothing is queued, and the terminal is
        // back: leave with the signal's code, as from the interface.
        prompt::Answer::Signalled(signal) => std::process::exit(signal.exit_code()),
    }
}

/// Add a task from the command line.
///
/// Shares everything that matters with the interface: the same parser, the same
/// `Store::queue`, the same outbox. The only difference is that there is no screen to
/// show it on, so it says what it did.
///
/// # Errors
/// An unreadable store or config. A server that cannot be reached is **not** an error —
/// the task is queued and the next run sends it, which is the whole point.
pub async fn add(
    config: &Config,
    config_path: &std::path::Path,
    text: &str,
    mode: AddMode,
) -> anyhow::Result<()> {
    let clock = now();
    let parsed = tui_do_core::quickadd::parse(text, &clock);
    // A line that is only an unknown label or user keeps those words as its title, so
    // only one that names nothing at all is empty here; `left_off` judges the rest.
    if parsed.title.trim().is_empty() && parsed.labels.is_empty() && parsed.assignees.is_empty() {
        anyhow::bail!("nothing to add");
    }
    // Before either branch, so a dry run refuses what the real add would (BUG-35).
    if let Some(refusal) = parsed.server_refusal() {
        anyhow::bail!("not added: {refusal}");
    }

    // Before `Store::open`, which creates and migrates, and before `build_sync`, so a dry
    // run never so much as builds a client.
    if mode.dry_run {
        let store = open_existing_store().await?;
        return rehearse(config, &store, text, parsed, &clock, &mode).await;
    }
    let store = open_store().await?;

    let (sync, problem) = build_sync(config, config_path, &store);

    let mut projects = store
        .projects(ProjectFilter::default(), ProjectSort::default())
        .await
        .context("could not read the project list")?;
    let mut labels = store
        .labels(LabelFilter::default(), LabelSort::default())
        .await
        .unwrap_or_default();
    let users = store.users().await.unwrap_or_default();

    let mut built = tui_do_ui::quickadd_task(
        &parsed,
        &projects,
        &labels,
        &users,
        None,
        config.view.default_project.as_deref(),
    );

    // A name that matches nothing may mean the cache is empty or simply older than the
    // project it names. Both are worth one cheap request each to settle — this is the one
    // case where reading from the local store alone is worse than asking the server.
    if built.is_none() && !mode.offline {
        if let Some(sync) = &sync {
            if sync.pull_lists().await.is_ok() {
                projects = store
                    .projects(ProjectFilter::default(), ProjectSort::default())
                    .await
                    .unwrap_or(projects);
                labels = store
                    .labels(LabelFilter::default(), LabelSort::default())
                    .await
                    .unwrap_or(labels);
                built = tui_do_ui::quickadd_task(
                    &parsed,
                    &projects,
                    &labels,
                    &users,
                    None,
                    config.view.default_project.as_deref(),
                );
            }
        }
    }

    let mut built = built.ok_or_else(|| no_project(&parsed, &projects))?;
    let parsed = left_off(&mut built, text, parsed, &clock, &mode)?;

    let project_id = built.task.project_id;
    let project = project_label(&projects, project_id);
    let (queued, description) = describe_and_queue(
        &store,
        built,
        &mode,
        &editor::ShellEditor::from_env(),
        &project,
    )
    .await?;

    let outcome = Outcome {
        title: &queued.title,
        project,
        project_id,
        ambiguous_project: queued.ambiguous_project,
        // In the zone the parser resolved them in, so a date across a daylight-saving
        // change is echoed on the day it was typed for (BUG-20).
        dates: dates_suffix(parsed.due_date, parsed.start_date, &clock.timezone()),
        backdated: queued.backdated.as_deref(),
        created_labels: &queued.created_labels,
        unknown_labels: &queued.unknown_labels,
        unknown_users: &queued.unknown_users,
        description: &description,
    };
    let mut lines = outcome.lines(Tense::Did).into_iter();
    if let Some(first) = lines.next() {
        println!("{first}");
    }
    // Second, straight under the line it rewrites.
    let kept = tui_do_core::quickadd::Kept {
        labels: &queued.unknown_labels,
        users: &queued.unknown_users,
    };
    if let Some(best) = better_phrasing(text, &parsed, &clock, kept) {
        println!("{}", best_line(&best));
    }
    for line in lines {
        println!("{line}");
    }

    if mode.offline {
        // `--offline` says "do not send it now", not "do not tell me the sending is
        // broken". A config that cannot authenticate -- an unreadable token file, or one
        // that is a directory -- makes "the next run will send it" a false promise, and
        // the same config without `--offline` reports the problem plainly. Saying it here
        // too costs one line and is the difference between a task that is waiting and a
        // task that will never go.
        match problem {
            Some(problem) => println!("Queued, but not syncing later either: {problem}"),
            None => println!("Queued. The next run will send it."),
        }
        return Ok(());
    }

    let Some(sync) = sync else {
        println!(
            "Queued. {}",
            problem.unwrap_or_else(|| "Not syncing.".to_string())
        );
        return Ok(());
    };
    // Listened to for rejections (BUG-35). A refused entry is rolled back and dropped,
    // not deferred, so `is_complete` holds for a push whose only entry the server turned
    // down -- and "Sent." for a task that exists nowhere is the worst answer a script
    // can be given. Adoptions too, because an `AttachLabel` queued behind this add's
    // create is retargeted to the server's id before it is sent.
    let (events_tx, mut events) = mpsc::unbounded_channel();
    let pushed = (*sync).clone().with_events(events_tx).push().await;
    let mut heard = Vec::new();
    while let Ok(event) = events.try_recv() {
        heard.push(event);
    }
    let said = push_report(pushed, &heard, &queued.subjects);
    for line in &said.lines {
        println!("{line}");
    }
    match said.refused {
        Some(message) => anyhow::bail!("the server refused it, so it was not added: {message}"),
        None => Ok(()),
    }
}

/// What `tui-do add` says once its push has answered.
struct PushSaid {
    /// Printed in order.
    lines: Vec<String>,
    /// The server's reason, when it refused this add's task itself, which makes the add
    /// an error: nothing was added.
    refused: Option<String>,
}

/// Word a push for `tui-do add`, from its report and the events it emitted in order.
///
/// `ours` are the subjects this add queued: its task and any labels `--create-labels`
/// queued with it. An adoption of one of them makes the server's id ours too, so an
/// `AttachLabel` retargeted to the created task is still recognised as this add's. A
/// refusal of the task's create means nothing was added. A refusal of anything else of
/// ours -- a label, or attaching it -- leaves the task created without it, which is said
/// but is not an error: a script that retried on it would add the task twice. A refusal
/// of anything else is an older queued entry this push sent on its way past.
fn push_report(
    pushed: tui_do_core::Result<tui_do_core::sync::PushReport>,
    events: &[SyncEvent],
    ours: &[Subject],
) -> PushSaid {
    let mut mine = ours.to_vec();
    let mut lines = Vec::new();
    let mut refused = None;
    for event in events {
        match event {
            SyncEvent::Adopted {
                provisional,
                assigned,
            } if mine.contains(provisional) => mine.push(*assigned),
            SyncEvent::Rejected {
                subject,
                kind,
                message,
            } => {
                let message = printable(message);
                if !mine.contains(subject) {
                    lines.push(format!(
                        "An earlier queued change was refused by the server: {message}"
                    ));
                } else if kind == "create_task" {
                    refused = Some(message);
                } else {
                    let what = match kind.as_str() {
                        "create_label" => "creating its label",
                        "attach_label" => "attaching its label",
                        _ => "part of it",
                    };
                    lines.push(format!(
                        "The task was added, but the server refused {what}: {message}"
                    ));
                }
            }
            _ => {}
        }
    }
    if refused.is_none() {
        lines.push(match pushed {
            Ok(report) if report.is_complete() => "Sent.".to_string(),
            // Not an error: the local store has it, the outbox has it, and the next run --
            // interface or `tui-do add` -- sends it. Failing here would throw away a task
            // the user has already been told was added.
            Ok(report) => format!("Queued — {} still waiting to be sent.", report.deferred),
            Err(error) => format!("Queued — could not reach the server: {error}"),
        });
    }
    PushSaid { lines, refused }
}

/// Why a quick-add found no project to land in, worded the same for a real add and a
/// dry run.
fn no_project(
    parsed: &tui_do_core::quickadd::Parsed,
    projects: &[tui_do_core::models::Project],
) -> anyhow::Error {
    match parsed
        .project
        .as_deref()
        .map(|typed| (typed, printable(typed)))
    {
        Some((typed, name)) => match longer_project(projects, typed) {
            // The commonest way to get here by far: `+Dinner Places` names the project
            // `Dinner` and leaves `Places` in the title, because a token ends at the
            // first space. Saying so here is the moment the bracket form is worth
            // learning -- the alternative is an error that looks like the project is
            // missing when it is only mis-typed.
            Some(full) => anyhow::anyhow!(
                "no project called \"{name}\" — did you mean +[{full}]? \
                 A name with a space in it has to be bracketed or quoted, \
                 or only its first word is read. See `tui-do add --help`."
            ),
            None => anyhow::anyhow!(
                "no project called \"{name}\" — tui-do knows of {}",
                describe(projects)
            ),
        },
        None => anyhow::anyhow!(
            "no project to add to — tui-do knows of {}",
            describe(projects)
        ),
    }
}

/// `tui-do add --dry-run`: everything a real add decides, and none of what it does.
///
/// Reads the projects and labels from the local store and resolves them exactly as
/// [`add`] does, so a line that would fail fails here with the same words. It never
/// reaches `Store::queue`, builds no `Sync` and so cannot push or pull: a project created
/// on the server since the last sync is unknown here, where a real add would ask the
/// server once before giving up.
///
/// # Errors
/// What [`add`] would answer for the same line: no project to land in, or an unreadable
/// project list.
async fn rehearse(
    config: &Config,
    store: &Store,
    text: &str,
    parsed: tui_do_core::quickadd::Parsed,
    clock: &tui_do_core::ZonedTime,
    mode: &AddMode,
) -> anyhow::Result<()> {
    let projects = store
        .projects(ProjectFilter::default(), ProjectSort::default())
        .await
        .context("could not read the project list")?;
    let labels = store
        .labels(LabelFilter::default(), LabelSort::default())
        .await
        .unwrap_or_default();
    let users = store.users().await.unwrap_or_default();
    let mut built = tui_do_ui::quickadd_task(
        &parsed,
        &projects,
        &labels,
        &users,
        None,
        config.view.default_project.as_deref(),
    )
    .ok_or_else(|| no_project(&parsed, &projects))?;
    let parsed = &left_off(&mut built, text, parsed, clock, mode)?;

    // What `queue_add` would do with the labels, without queuing anything.
    let (created_labels, unknown_labels) = if mode.create_labels {
        (built.unknown_labels.clone(), Vec::new())
    } else {
        (Vec::new(), built.unknown_labels.clone())
    };
    // `--dry-run` and `--edit` conflict in clap, so this is only reached by a caller
    // that skipped it; refuse rather than open an editor whose work is thrown away.
    let DescriptionSource::Given(description) = &mode.description else {
        anyhow::bail!("--dry-run cannot be combined with --edit: what you wrote would be lost");
    };
    let backdated = tui_do_ui::past_due_note(built.task.due_date.get(), *clock);
    let outcome = Outcome {
        title: &built.task.title,
        project: project_label(&projects, built.task.project_id),
        project_id: built.task.project_id,
        ambiguous_project: built.ambiguous_project,
        dates: dates_suffix(parsed.due_date, parsed.start_date, &clock.timezone()),
        backdated: backdated.as_deref(),
        created_labels: &created_labels,
        unknown_labels: &unknown_labels,
        unknown_users: &built.unknown_users,
        description,
    };
    for line in outcome.lines(Tense::Would) {
        println!("{line}");
    }
    println!("Parsed: {}", printable(&parsed.summary(&clock.timezone())));
    let kept = tui_do_core::quickadd::Kept {
        labels: &unknown_labels,
        users: &built.unknown_users,
    };
    if let Some(best) = checked_canonical(parsed, clock, kept) {
        println!("{}", best_line(&best));
    }
    println!("Dry run: nothing was queued or sent.");
    Ok(())
}

/// The line as it will be added, once the labels `--create-labels` will not make and the
/// users nobody here has seen are known to be left off: their words go back in the title,
/// as typed.
///
/// `parsed` unchanged when every label resolves or is to be created. Otherwise re-read
/// with the unknown names kept as text, which is what the `Parsed:` line and the
/// `as typed best:` check then describe, and checked against the server's limits again,
/// since the title is longer now.
///
/// # Errors
/// The longer title is past the server's limit.
fn left_off(
    built: &mut tui_do_ui::QuickAdd,
    text: &str,
    parsed: tui_do_core::quickadd::Parsed,
    clock: &tui_do_core::ZonedTime,
    mode: &AddMode,
) -> anyhow::Result<tui_do_core::quickadd::Parsed> {
    if built.left_off(mode.create_labels).is_empty() {
        if built.task.title.trim().is_empty() {
            anyhow::bail!("nothing to add");
        }
        return Ok(parsed);
    }
    let kept = built.keep_left_off(text, clock, mode.create_labels);
    if built.task.title.trim().is_empty() {
        anyhow::bail!("nothing to add");
    }
    if let Some(refusal) = kept.server_refusal() {
        anyhow::bail!("not added: {refusal}");
    }
    Ok(kept)
}

/// How `tui-do add` was asked to run.
#[derive(Debug, Clone, Default)]
pub struct AddMode {
    /// Queue without trying to send.
    pub offline: bool,
    /// Create any label the line names that does not exist yet.
    pub create_labels: bool,
    /// Say what would be added, and add nothing.
    pub dry_run: bool,
    /// Where the task's description comes from.
    pub description: DescriptionSource,
}

/// Where a task's description comes from.
///
/// Only a source: nothing reports or stores one of these. [`describe_and_queue`] settles
/// it into the text itself before the task is built and the outcome worded, so a
/// description still to be written can never be mistaken for an empty one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DescriptionSource {
    /// Given inline, from a file or from stdin, already through
    /// `description::normalise`. Empty when none was given.
    Given(String),
    /// To be written in the user's editor, once the title and project are known so the
    /// file can name them. The caller has checked there is a terminal for it.
    Edit,
}

impl Default for DescriptionSource {
    fn default() -> Self {
        Self::Given(String::new())
    }
}

/// Settle the description, then queue the task carrying it.
///
/// The editor runs first, before anything is queued -- a label `--create-labels` would
/// make included -- so an editor that exits non-zero leaves the outbox exactly as it
/// was. Returns the description beside what was queued, for the report.
///
/// The description goes on the task itself, so it travels the ordinary write path: the
/// store row, the outbox payload and the create's body all carry the whole `Task`. It is
/// body text, never quick-add: only the title line was parsed.
///
/// # Errors
/// The editor failed or was cancelled, what it wrote is not a description, or
/// [`queue_add`] failed.
async fn describe_and_queue(
    store: &Store,
    mut built: tui_do_ui::QuickAdd,
    mode: &AddMode,
    editor: &dyn editor::Editor,
    project: &str,
) -> anyhow::Result<(Queued, String)> {
    let description = match &mode.description {
        DescriptionSource::Given(text) => text.clone(),
        // Blocking, on purpose: nothing else is running yet, and the editor owns the
        // terminal until it exits.
        DescriptionSource::Edit => {
            let composed =
                editor::compose(editor, &editor::Header::new(&built.task.title, project))?;
            if composed.scissors_missing {
                eprintln!(
                    "warning: the editor's \">8\" scissors line was removed, so the whole \
                     file was kept as the description, help lines included."
                );
            }
            composed.text
        }
    };
    built.task.description.clone_from(&description);
    let queued = queue_add(store, built, mode.create_labels).await?;
    Ok((queued, description))
}

/// How wide a dry run's glimpse of the description is, in characters.
const DESCRIPTION_GLIMPSE: usize = 60;

/// Whether [`Outcome::lines`] reports an add that happened or one that would.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tense {
    /// `tui-do add`: "Added …".
    Did,
    /// `tui-do add --dry-run`: "Would add …".
    Would,
}

/// What an add did, or would do, for [`Outcome::lines`] to word.
///
/// One type for both, so a dry run's messages cannot drift from a real add's.
struct Outcome<'a> {
    /// The task's title.
    title: &'a str,
    /// The project it lands in, as printed.
    project: String,
    /// The same project's id, named when the name is ambiguous.
    project_id: tui_do_core::models::ProjectId,
    /// Whether more than one project answers to the name that was used.
    ambiguous_project: bool,
    /// `, due …, starts …`, from [`dates_suffix`].
    dates: String,
    /// A note if the due date has already passed.
    backdated: Option<&'a str>,
    /// Labels created because `--create-labels` said to.
    created_labels: &'a [String],
    /// Labels named that do not exist, and are left off.
    unknown_labels: &'a [String],
    /// Users named that the store has never seen, and are left off (BUG-34).
    unknown_users: &'a [String],
    /// The description, or `""`. Never printed whole: it is long by nature, so a real
    /// add reports its size, and a dry run its first line as well.
    description: &'a str,
}

impl Outcome<'_> {
    /// The report, a line at a time. The first line is the one people and the skill doc
    /// read, `Added "…" to <project>, due …`, and its shape is relied on.
    fn lines(&self, tense: Tense) -> Vec<String> {
        let Self {
            title,
            project,
            project_id,
            ambiguous_project,
            dates,
            backdated,
            created_labels,
            unknown_labels,
            unknown_users,
            description,
        } = self;
        let verb = match tense {
            Tense::Did => "Added",
            Tense::Would => "Would add",
        };
        // Typed by the user, or read from a file or stdin: as untrusted on a terminal as a
        // name from the server, so it goes through the same filter the `Parsed` line uses.
        let title = printable(title);
        let mut lines = Vec::new();
        // On the first line, after the dates, so the line's opening stays the shape
        // people and the skill doc read.
        let described = if description.is_empty() {
            String::new()
        } else {
            format!(
                ", with a description ({})",
                tui_do_core::description::size(description)
            )
        };
        // Named with its id when the name is ambiguous, because a task added to the wrong
        // Inbox is a task the user will not find.
        if *ambiguous_project {
            lines.push(format!(
                "{verb} \"{title}\" to {project} (#{project_id}){dates}{described} — more than one project has that name"
            ));
        } else {
            lines.push(format!("{verb} \"{title}\" to {project}{dates}{described}"));
        }
        // A dry run shows enough to tell which description it read, and no more.
        if tense == Tense::Would && !description.is_empty() {
            lines.push(format!(
                "Description begins: \"{}\"",
                printable(&tui_do_core::description::first_line(
                    description,
                    DESCRIPTION_GLIMPSE
                ))
            ));
        }
        if let Some(note) = backdated {
            lines.push(format!("Note: {note}."));
        }
        if !created_labels.is_empty() {
            let verb = match tense {
                Tense::Did => "Created",
                Tense::Would => "Would create",
            };
            lines.push(format!(
                "{verb} label {}.",
                printable(&created_labels.join(", "))
            ));
        }
        if !unknown_labels.is_empty() {
            let one = unknown_labels.len() == 1;
            let names = if one {
                "No label called"
            } else {
                "No labels called"
            };
            let object = if one { "it" } else { "them" };
            let listed = printable(&unknown_labels.join(", "));
            lines.push(match tense {
                // Scoped to next time in the *text*, not only in the comment: the natural
                // reading of a CLI hint is "re-run me with this flag", and the task above
                // is already queued, so doing that would add a second task rather than
                // fixing this one. The hint used to say neither, and it said "create it"
                // however many names it had just listed.
                Tense::Did => format!(
                    "{names} {listed} — {} left off. Pass --create-labels next time to have \
                     tui-do create {object}; this task is already queued, so re-running now \
                     would add a second one.",
                    if one { "it was" } else { "they were" }
                ),
                Tense::Would => format!(
                    "{names} {listed} — {} left off. Pass --create-labels to have tui-do \
                     create {object}.",
                    if one { "it would be" } else { "they would be" }
                ),
            });
        }
        if !unknown_users.is_empty() {
            let one = unknown_users.len() == 1;
            let left = match (tense, one) {
                (Tense::Did, true) => "it was",
                (Tense::Did, false) => "they were",
                (Tense::Would, true) => "it would be",
                (Tense::Would, false) => "they would be",
            };
            lines.push(format!(
                "No {} called {} — {left} left off. tui-do can assign only users it has \
                 seen on a task; `tui-do add --tokens` lists them.",
                if one { "user" } else { "users" },
                printable(&unknown_users.join(", "))
            ));
        }
        lines
    }
}

/// The canonical line, when it differs from what was typed and parses back to the same
/// task.
///
/// `None` for a line already phrased that way, so a well-written add stays quiet.
/// Whitespace is collapsed before comparing, so spacing alone never triggers it.
fn better_phrasing(
    text: &str,
    parsed: &tui_do_core::quickadd::Parsed,
    clock: &tui_do_core::ZonedTime,
    kept: tui_do_core::quickadd::Kept<'_>,
) -> Option<String> {
    let typed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    checked_canonical(parsed, clock, kept).filter(|best| *best != printable(&typed))
}

/// The `as typed best:` line, written so it can be pasted back after `tui-do add`.
///
/// A best form starting with `-` is shown after `-- `: pasted back bare, `--dry-run x`
/// is read as the flag, and `--` is what tells the command line that the rest is the
/// task. The `--` is not part of the task, so it goes outside any quotes.
fn best_line(best: &str) -> String {
    let guard = if best.starts_with('-') { "-- " } else { "" };
    format!("as typed best: {guard}{best}")
}

/// The line in its canonical form, if that parses back to the same task.
///
/// `None` otherwise, with a debug line saying so: a recommendation that changes the task
/// is worse than none, and a suppression nobody can see is hard to debug.
///
/// Read back with the same names `kept` as text that `parsed` was, or a left-off
/// `*label` kept in the title turns back into a label and the line is never offered.
fn checked_canonical(
    parsed: &tui_do_core::quickadd::Parsed,
    clock: &tui_do_core::ZonedTime,
    kept: tui_do_core::quickadd::Kept<'_>,
) -> Option<String> {
    let best = parsed.canonical(&clock.timezone());
    if tui_do_core::quickadd::parse_kept(&best, clock, kept).same_fields(parsed) {
        Some(printable(&best))
    } else {
        tracing::debug!(
            canonical = %best,
            "the canonical line does not parse back to the same task, so it is not shown"
        );
        None
    }
}

/// What [`queue_add`] queued, for the caller to report.
struct Queued {
    /// The task's title, read before it moved into the mutation.
    title: String,
    /// Whether more than one project answers to the name that was used.
    ambiguous_project: bool,
    /// A note if the due date has already passed, or `None`.
    backdated: Option<String>,
    /// Labels that were created because `--create-labels` said to.
    created_labels: Vec<String>,
    /// Labels that were named and do not exist, and were left off because
    /// `--create-labels` was not passed.
    unknown_labels: Vec<String>,
    /// Users named that the store has never seen, and were left off (BUG-34).
    unknown_users: Vec<String>,
    /// Everything this add queued, so a refusal of it can be told from a refusal of an
    /// older entry the same push happened to send.
    subjects: Vec<Subject>,
}

/// Queue a task built by quick-add — and, if `create_labels` says to, a label for every
/// name it used that does not exist yet.
///
/// Pulled out of [`add`] so a test can assert the queue's contents without also
/// exercising the printing and the push: `run_add`'s job is to talk to a terminal and a
/// server, and neither belongs in a test.
///
/// Each `CreateLabel` is queued **before** the `CreateTask` that names it, and the
/// server-bound id it will get is threaded onto the task first. This is the one thing
/// the CLI can do that the interface cannot: `Store::queue` allocates a label's
/// provisional id inside its own transaction and returns the [`OutboxEntry`] it wrote,
/// so the id is available here immediately rather than needing a reload to see it. Once
/// the label is on `built.task.labels`, `Mutation::decompose` puts an `AttachLabel`
/// behind the `CreateTask` for it, and adoption retargets that entry when the server
/// answers with the label's real id.
///
/// # Errors
/// [`tui_do_core::CoreError`] wrapped by `anyhow`, from either `Store::queue` call.
async fn queue_add(
    store: &Store,
    mut built: tui_do_ui::QuickAdd,
    create_labels: bool,
) -> anyhow::Result<Queued> {
    let mut created_labels = Vec::new();
    let mut subjects = Vec::new();
    if create_labels {
        for title in &built.unknown_labels {
            let entry = store
                .queue(tui_do_core::store::Mutation::CreateLabel {
                    label: Box::new(tui_do_api::models::Label {
                        title: title.clone(),
                        ..tui_do_api::models::Label::default()
                    }),
                })
                .await
                .context("could not queue the label")?;
            subjects.push(entry.mutation.subject());
            let tui_do_core::store::Mutation::CreateLabel { label } = entry.mutation else {
                anyhow::bail!("queuing a label returned a {} entry", entry.mutation.kind());
            };
            built.task.labels.push(*label);
            created_labels.push(title.clone());
        }
        built.unknown_labels.clear();
    }

    let title = built.task.title.clone();
    let ambiguous_project = built.ambiguous_project;
    let unknown_labels = std::mem::take(&mut built.unknown_labels);
    let unknown_users = std::mem::take(&mut built.unknown_users);
    // Read before the task is moved into the mutation. Same rule as the interface: a date
    // already gone by is allowed, never confirmed quietly.
    let backdated = tui_do_ui::past_due_note(built.task.due_date.get(), now());

    let entry = store
        .queue(tui_do_core::store::Mutation::CreateTask {
            task: Box::new(built.task),
        })
        .await
        .context("could not queue the task")?;
    subjects.push(entry.mutation.subject());

    Ok(Queued {
        title,
        ambiguous_project,
        backdated,
        created_labels,
        unknown_labels,
        unknown_users,
        subjects,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use ratatui::backend::TestBackend;
    use tokio::sync::mpsc;
    use tui_do_core::sync::{Phase, PullReport, PushReport, SyncReport};
    use tui_do_core::SyncEvent;

    use super::*;

    /// The key-change notice is for upgraders only, and `d` turns it off for good.
    #[tokio::test]
    async fn the_keys_notice_is_owed_to_a_synced_store_until_dismissed() {
        let dir = std::env::temp_dir().join(format!("tui-do-keys-notice-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::open(dir.join("tui-do.db")).await.unwrap();

        assert!(
            !keys_notice_owed(&store).await,
            "a fresh install has nothing to unlearn"
        );
        store
            .set_state(LAST_PULL, "2026-11-01T00:00:00Z")
            .await
            .unwrap();
        assert!(
            keys_notice_owed(&store).await,
            "someone who has synced is told"
        );
        store.set_state(KEYS_NOTICE_DISMISSED, "1").await.unwrap();
        assert!(
            !keys_notice_owed(&store).await,
            "and told only until they say stop"
        );

        drop(store);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_release_notes_link_names_the_tag() {
        assert_eq!(
            release_notes_url(),
            format!("https://github.com/sjwasko/tui-do/releases/tag/{KEYS_CHANGED_IN}")
        );
    }

    fn refused(subject: Subject, kind: &str, message: &str) -> SyncEvent {
        SyncEvent::Rejected {
            subject,
            kind: kind.to_string(),
            message: message.to_string(),
        }
    }

    fn sent() -> tui_do_core::Result<PushReport> {
        Ok(PushReport {
            sent: 1,
            rejected: 1,
            deferred: 0,
        })
    }

    #[test]
    fn the_quit_flush_names_a_refusal_instead_of_saying_sent() {
        // `is_complete` is `deferred == 0`, and a refused entry is rolled back rather than
        // deferred, so a flush whose only change the server turned down said "Sent.".
        let task = Subject::Task(tui_do_core::models::TaskId(4));
        let report = PushReport {
            sent: 0,
            rejected: 1,
            deferred: 0,
        };
        let said = flush_said(
            Some(Ok(report)),
            &[refused(
                task,
                "update_task",
                "The task title cannot be empty.",
            )],
        );
        assert_eq!(
            said,
            vec!["The server refused 1 change: The task title cannot be empty.".to_string()]
        );
    }

    #[test]
    fn the_quit_flush_says_what_went_beside_what_was_refused() {
        let task = Subject::Task(tui_do_core::models::TaskId(4));
        let report = PushReport {
            sent: 2,
            rejected: 2,
            deferred: 1,
        };
        let said = flush_said(
            Some(Ok(report)),
            &[refused(task, "update_task", "no\x1b]52;c;x\x07")],
        );
        assert_eq!(
            said,
            vec![
                "The server refused 2 changes: no\u{fffd}]52;c;x\u{fffd}".to_string(),
                "1 still queued; the next run will send them.".to_string(),
            ]
        );
        let clean = PushReport {
            sent: 3,
            rejected: 0,
            deferred: 0,
        };
        assert_eq!(flush_said(Some(Ok(clean)), &[]), vec!["Sent.".to_string()]);
    }

    #[test]
    fn a_refused_create_is_not_added() {
        let task = Subject::Task(tui_do_core::models::TaskId(-1));
        let said = push_report(sent(), &[refused(task, "create_task", "too long")], &[task]);
        assert_eq!(said.refused.as_deref(), Some("too long"));
        assert!(
            said.lines.is_empty(),
            "no Sent. under a refusal: {:?}",
            said.lines
        );
    }

    #[test]
    fn a_refused_label_leaves_the_task_added() {
        // The label's entry is ours, but the task went through: an error here would have
        // a script retry and add the task twice.
        let task = Subject::Task(tui_do_core::models::TaskId(-1));
        let label = Subject::Label(tui_do_core::models::LabelId(-1));
        let said = push_report(
            sent(),
            &[refused(label, "create_label", "bad title")],
            &[label, task],
        );
        assert_eq!(said.refused, None);
        assert_eq!(
            said.lines,
            [
                "The task was added, but the server refused creating its label: bad title",
                "Sent."
            ]
        );
    }

    #[test]
    fn an_attach_retargeted_by_adoption_is_still_ours() {
        let provisional = Subject::Task(tui_do_core::models::TaskId(-1));
        let assigned = Subject::Task(tui_do_core::models::TaskId(41));
        let events = [
            SyncEvent::Adopted {
                provisional,
                assigned,
            },
            refused(assigned, "attach_label", "forbidden"),
        ];
        let said = push_report(sent(), &events, &[provisional]);
        assert_eq!(said.refused, None);
        assert_eq!(
            said.lines[0],
            "The task was added, but the server refused attaching its label: forbidden"
        );
    }

    #[test]
    fn an_older_entry_refused_on_the_way_past_is_said_and_not_ours() {
        let ours = Subject::Task(tui_do_core::models::TaskId(-2));
        let older = Subject::Task(tui_do_core::models::TaskId(-1));
        let said = push_report(sent(), &[refused(older, "create_task", "gone")], &[ours]);
        assert_eq!(said.refused, None);
        assert_eq!(
            said.lines,
            [
                "An earlier queued change was refused by the server: gone",
                "Sent."
            ]
        );
    }

    /// The prompt's names come from a read-only open, so asking for a task -- and then
    /// cancelling it, or dry-running it -- never creates a store that was not there.
    #[tokio::test]
    async fn the_prompts_names_never_create_a_store() {
        let dir = std::env::temp_dir().join(format!("tui-do-vocabulary-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tui-do.db");

        assert_eq!(
            vocabulary_at(&path).await,
            tui_do_ui::assist::Vocabulary::default()
        );
        assert!(!path.exists(), "reading the names created a store");

        let store = Store::open(&path).await.unwrap();
        store
            .upsert_projects(vec![a_project(4, "Legal")])
            .await
            .unwrap();
        drop(store);
        assert_eq!(vocabulary_at(&path).await.projects, vec!["Legal"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn a_project(id: i64, title: &str) -> tui_do_core::models::Project {
        tui_do_core::models::Project {
            id: tui_do_core::models::ProjectId(id),
            title: title.to_string(),
            ..tui_do_core::models::Project::default()
        }
    }

    #[test]
    fn a_truncated_project_name_offers_the_whole_one() {
        // `tui-do add +Dinner Places ...` names the project `Dinner` and leaves `Places`
        // in the title, because a token ends at the first space. The error is the moment
        // the bracket form is worth learning.
        let projects = vec![
            a_project(1, "Dinner Places"),
            a_project(2, "Legal"),
            a_project(3, "Life Admin"),
        ];
        assert_eq!(
            longer_project(&projects, "Dinner").as_deref(),
            Some("Dinner Places")
        );
        assert_eq!(
            longer_project(&projects, "dinner").as_deref(),
            Some("Dinner Places")
        );
        // A project that exists under exactly that name is not a truncation.
        assert_eq!(longer_project(&projects, "Legal"), None);
        // Nor is a name that simply does not exist -- suggesting `Life Admin` for `Lif`
        // would be a guess dressed up as an answer.
        assert_eq!(longer_project(&projects, "Lif"), None);
    }

    /// A project title is written on the server, and on a shared instance it may be
    /// written by someone else. `tui-do add` prints it to a terminal in normal mode --
    /// no raw mode, no alternate screen, and none of ratatui's filtering, which is what
    /// protects the same string inside the interface.
    #[test]
    fn a_suggested_project_title_cannot_carry_an_escape_sequence() {
        let projects = vec![a_project(1, "Work \u{1b}]52;c;cGF5bG9hZA==\u{7}Notes")];
        let offered = longer_project(&projects, "Work").expect("the title is a truncation");
        assert!(
            !offered.chars().any(char::is_control),
            "the suggestion carried a raw escape to the terminal: {offered:?}"
        );
    }

    /// The other half of the same hazard: the line that reports where the task landed
    /// names a project the user never typed. `+Name` matches case-insensitively and
    /// exactly, so that form only ever echoes the user's own text back -- but a bare
    /// `tui-do add "buy milk"` has no name to match, falls through to the first real
    /// project in the store, and prints whatever that one is called.
    #[test]
    fn the_project_a_task_landed_in_is_named_without_its_control_characters() {
        let projects = vec![a_project(7, "Inbox\u{1b}[2K\u{1b}[GSent.")];
        assert_eq!(
            project_label(&projects, tui_do_core::models::ProjectId(7)),
            "Inbox\u{fffd}[2K\u{fffd}[GSent."
        );
        // A project the store has never heard of is still named by its id.
        assert_eq!(
            project_label(&projects, tui_do_core::models::ProjectId(9)),
            "9"
        );
    }

    /// An editor that exits non-zero queues nothing at all -- not the task, and not a
    /// label `--create-labels` would have made for it.
    #[tokio::test]
    async fn a_cancelled_editor_queues_nothing() {
        let store = Store::in_memory().unwrap();
        let built = tui_do_ui::QuickAdd {
            task: tui_do_core::models::Task {
                title: "write it".to_string(),
                project_id: tui_do_core::models::ProjectId(1),
                ..tui_do_core::models::Task::default()
            },
            unknown_labels: vec!["brand-new".to_string()],
            unknown_users: Vec::new(),
            ambiguous_project: false,
        };
        let mode = AddMode {
            create_labels: true,
            description: DescriptionSource::Edit,
            ..AddMode::default()
        };
        let error = describe_and_queue(
            &store,
            built.clone(),
            &mode,
            &editor::tests::FakeEditor::failing(),
            "Work",
        )
        .await
        .err()
        .expect("a cancelled editor is an error");
        assert!(error.to_string().contains("nothing was added"), "{error}");
        assert_eq!(store.pending_count().await.unwrap(), 0);

        // And one that writes queues the task with exactly that description.
        let editor = editor::tests::FakeEditor::writing(&format!(
            "Line one.\n\nLine two.\n\n{}\n# help\n",
            editor::SCISSORS
        ));
        let (_, description) = describe_and_queue(&store, built, &mode, &editor, "Work")
            .await
            .unwrap();
        assert_eq!(description, "Line one.\n\nLine two.");
        let (_, template) = editor.saw.borrow().clone().unwrap();
        assert!(template.contains("\"write it\" in Work"), "{template}");
        let queued = store.pending(None).await.unwrap();
        let task = queued
            .iter()
            .find_map(|entry| match &entry.mutation {
                tui_do_core::store::Mutation::CreateTask { task } => Some(task),
                _ => None,
            })
            .expect("the task was queued");
        assert_eq!(task.description, "Line one.\n\nLine two.");
    }

    /// A dry run and a real add word the same outcome the same way, bar the tense.
    #[test]
    fn a_dry_run_reports_in_the_same_words_as_an_add() {
        let created = vec!["new".to_string()];
        let unknown = vec!["a".to_string(), "b".to_string()];
        let outcome = Outcome {
            title: "call mom",
            project: "Inbox".to_string(),
            project_id: tui_do_core::models::ProjectId(7),
            ambiguous_project: true,
            dates: ", due Fri 27 Aug 2027".to_string(),
            backdated: Some("that date has passed"),
            created_labels: &created,
            unknown_labels: &unknown,
            unknown_users: &[],
            description: "",
        };
        assert_eq!(
            outcome.lines(Tense::Did),
            vec![
                "Added \"call mom\" to Inbox (#7), due Fri 27 Aug 2027 — more than one project \
                 has that name",
                "Note: that date has passed.",
                "Created label new.",
                "No labels called a, b — they were left off. Pass --create-labels next time \
                 to have tui-do create them; this task is already queued, so re-running now \
                 would add a second one.",
            ]
        );
        assert_eq!(
            outcome.lines(Tense::Would),
            vec![
                "Would add \"call mom\" to Inbox (#7), due Fri 27 Aug 2027 — more than one \
                 project has that name",
                "Note: that date has passed.",
                "Would create label new.",
                "No labels called a, b — they would be left off. Pass --create-labels to have \
                 tui-do create them.",
            ]
        );
    }

    /// Every name the user typed goes through `printable`, as the `Parsed` line already
    /// did: found by the 2026-11-09 smoke test, where ESC and BEL in a title reached the
    /// terminal from the `Added` and `Would add` lines.
    #[test]
    fn the_report_prints_no_control_characters() {
        let created = vec!["new\u{1b}]52;c;x\u{7}".to_string()];
        let unknown = vec!["gone\u{1b}[2J".to_string()];
        let outcome = Outcome {
            title: "call \u{1b}]52;c;evil\u{7}mom",
            project: "Inbox".to_string(),
            project_id: tui_do_core::models::ProjectId(7),
            ambiguous_project: false,
            dates: String::new(),
            backdated: None,
            created_labels: &created,
            unknown_labels: &unknown,
            unknown_users: &[],
            description: "",
        };
        for tense in [Tense::Did, Tense::Would] {
            let lines = outcome.lines(tense);
            for line in &lines {
                assert!(!line.chars().any(char::is_control), "{line:?}");
            }
            assert!(
                lines[0].ends_with("\"call \u{fffd}]52;c;evil\u{fffd}mom\" to Inbox"),
                "{:?}",
                lines[0]
            );
        }

        // And the error for a project that is not there, which names what was typed.
        let parsed = tui_do_core::quickadd::parse("x +\u{1b}]52;c;x\u{7}", &now());
        let said = no_project(&parsed, &[]).to_string();
        assert!(!said.chars().any(char::is_control), "{said:?}");
    }

    /// A description is counted on the first line and never printed; a dry run shows a
    /// printable glimpse of its first line as well.
    #[test]
    fn a_description_is_reported_by_size() {
        let description = "\u{1b}]52;c;evil\u{7} first line\n\nsecond paragraph";
        let outcome = Outcome {
            title: "write it",
            project: "Work".to_string(),
            project_id: tui_do_core::models::ProjectId(3),
            ambiguous_project: false,
            dates: ", due Fri 27 Aug 2027".to_string(),
            backdated: None,
            created_labels: &[],
            unknown_labels: &[],
            unknown_users: &[],
            description,
        };
        assert_eq!(
            outcome.lines(Tense::Did),
            vec![
                "Added \"write it\" to Work, due Fri 27 Aug 2027, with a description \
                 (2 paragraphs, 41 characters)"
            ]
        );
        let would = outcome.lines(Tense::Would);
        assert_eq!(would.len(), 2, "{would:?}");
        assert_eq!(
            would[1],
            "Description begins: \"\u{fffd}]52;c;evil\u{fffd} first line\""
        );
    }

    #[test]
    fn the_add_line_says_which_dates_were_set() {
        use chrono::{FixedOffset, TimeZone as _, Utc};
        // Five hours behind UTC, so a date shown in UTC would be visibly a day out.
        let zone = FixedOffset::west_opt(5 * 3600).expect("a valid offset");
        let local = |y, m, d, h, min| {
            zone.with_ymd_and_hms(y, m, d, h, min, 0)
                .single()
                .expect("an unambiguous instant")
                .with_timezone(&Utc)
        };
        assert_eq!(dates_suffix(None, None, &zone), "");
        assert_eq!(
            dates_suffix(Some(local(2027, 8, 27, 23, 59)), None, &zone),
            ", due Fri 27 Aug 2027"
        );
        assert_eq!(
            dates_suffix(Some(local(2026, 8, 27, 9, 0)), None, &zone),
            ", due Thu 27 Aug 2026 at 09:00"
        );
        assert_eq!(
            dates_suffix(
                Some(local(2026, 8, 28, 23, 59)),
                Some(local(2026, 8, 24, 0, 0)),
                &zone
            ),
            ", due Fri 28 Aug 2026, starts Mon 24 Aug 2026"
        );
    }

    /// Parse `text` against `clock` and echo it in the clock's zone, the way `add` does.
    fn echo(text: &str, clock: &tui_do_core::ZonedTime) -> String {
        let parsed = tui_do_core::quickadd::parse(text, clock);
        dates_suffix(parsed.due_date, parsed.start_date, &clock.timezone())
    }

    #[test]
    fn the_best_form_is_offered_for_a_line_with_a_label_left_off() {
        // Found by the 2026-11-10 review: the kept `*bills` in the title read back as a
        // label, the round trip failed, and the hint vanished for every such add.
        use chrono::TimeZone as _;
        let clock = tui_do_core::zone::pinned(
            chrono::FixedOffset::west_opt(5 * 3600)
                .and_then(|zone| zone.with_ymd_and_hms(2026, 8, 26, 10, 0, 0).single())
                .expect("a fixed now"),
        );
        let labels = vec!["bills".to_string()];
        let kept = tui_do_core::quickadd::Kept {
            labels: &labels,
            users: &[],
        };
        let text = "pay rent *bills due friday";
        let parsed = tui_do_core::quickadd::parse_kept(text, &clock, kept);
        assert_eq!(parsed.title, "pay rent *bills");
        let best = better_phrasing(text, &parsed, &clock, kept).expect("a best form");
        assert!(best.starts_with("pay rent *bills"), "{best}");
    }

    #[test]
    fn the_add_line_reports_what_the_parser_took() {
        // Through the parser: `due 27.8` is a date, and `1.6 GB` is not.
        use chrono::TimeZone as _;
        let clock = tui_do_core::zone::pinned(
            chrono::FixedOffset::west_opt(5 * 3600)
                .and_then(|zone| zone.with_ymd_and_hms(2026, 8, 26, 10, 0, 0).single())
                .expect("a fixed now"),
        );
        assert_eq!(echo("call mom due 27.8", &clock), ", due Thu 27 Aug 2026");
        assert_eq!(echo("llvm 1.6 GB", &clock), "");
    }

    #[test]
    fn a_date_across_daylight_saving_is_echoed_on_its_own_day() {
        // Typed in January in New York (-05:00) for a day in July (-04:00). The parser
        // resolves 23:59 and 00:00 at July's offset, and the echo must read them back by
        // the same rules. In January's fixed offset both are an hour earlier: the due
        // date gains a time nobody typed, and the start date moves to the day before.
        use chrono::TimeZone as _;
        use chrono_tz::America::New_York;
        let january = New_York
            .with_ymd_and_hms(2027, 1, 15, 10, 0, 0)
            .single()
            .expect("an unambiguous instant");
        let text = "fireworks start 2027-07-04 due 2027-07-04";
        let parsed = tui_do_core::quickadd::parse(text, &january);
        assert_eq!(
            dates_suffix(parsed.due_date, parsed.start_date, &New_York),
            ", due Sun 4 Jul 2027, starts Sun 4 Jul 2027"
        );

        // The same instants echoed in a frozen January offset -- what a clock pinned to
        // today's offset would print, and what `add` must not.
        let frozen = tui_do_core::zone::pinned(january.fixed_offset());
        assert_eq!(
            dates_suffix(parsed.due_date, parsed.start_date, &frozen.timezone()),
            ", due Sun 4 Jul 2027 at 22:59, starts Sat 3 Jul 2027 at 23:00"
        );
    }

    #[test]
    fn a_title_keeps_everything_that_is_not_a_control_character() {
        // The filter is C0, DEL and C1 -- not "anything unusual". Mangling a legitimate
        // title would be its own bug, and titles carry emoji and accents routinely.
        assert_eq!(printable("Café ☕ — 日本語"), "Café ☕ — 日本語");
        // C1: 0x9b is CSI on its own, and reaches here as a single char.
        assert_eq!(printable("a\u{9b}2Kb"), "a\u{fffd}2Kb");
    }

    #[test]
    fn two_candidates_make_the_suggestion_a_guess_so_none_is_offered() {
        let projects = vec![a_project(1, "Dinner Places"), a_project(2, "Dinner Ideas")];
        assert_eq!(longer_project(&projects, "Dinner"), None);
    }

    #[test]
    fn a_pseudo_project_is_never_suggested() {
        // The server invents `-1 Favorites` and friends into `/projects`, and every one
        // of them rejects a write.
        let projects = vec![a_project(-1, "Favorites Everything")];
        assert_eq!(longer_project(&projects, "Favorites"), None);
    }

    /// The shape the sync pass runs in: a forwarder relaying the engine's events, and an
    /// engine that emits `Finished` as its very last act.
    ///
    /// Awaiting the forwarder after dropping the sender is load-bearing. Aborting it
    /// instead raced the last event out of the channel -- and the last event is the only
    /// one that reloads, so a pull would update the store and leave the screen showing
    /// what it showed before, until the next launch.
    #[tokio::test]
    async fn the_last_event_of_a_pass_survives_the_end_of_it() {
        let (msg_tx, mut msg_rx) = mpsc::unbounded_channel::<Msg>();
        let (events_tx, mut events_rx) = mpsc::unbounded_channel::<SyncEvent>();

        let forward = tokio::spawn(async move {
            while let Some(event) = events_rx.recv().await {
                let _ = msg_tx.send(Msg::Sync(event));
            }
        });

        events_tx.send(SyncEvent::Started(Phase::Pull)).unwrap();
        events_tx
            .send(SyncEvent::Finished(SyncReport {
                push: PushReport::default(),
                pull: PullReport::default(),
            }))
            .unwrap();

        drop(events_tx);
        forward.await.unwrap();

        let mut seen = Vec::new();
        while let Ok(msg) = msg_rx.try_recv() {
            if let Msg::Sync(event) = msg {
                seen.push(event);
            }
        }
        assert!(
            matches!(seen.last(), Some(SyncEvent::Finished(_))),
            "the reload event was lost when the pass ended; saw {seen:?}"
        );
    }

    #[test]
    fn the_timer_pulls_in_full_at_startup_and_hourly_and_otherwise_only_changes() {
        let now = chrono::Utc::now();
        let recent = Some(now - chrono::TimeDelta::minutes(5));
        let stale = Some(now - chrono::TimeDelta::minutes(61));

        assert_eq!(timer_pass(true, recent, now), Pass::Full, "startup");
        assert_eq!(timer_pass(false, recent, now), Pass::Delta);
        assert_eq!(
            timer_pass(false, stale, now),
            Pass::Full,
            "an hour has passed"
        );
        assert_eq!(timer_pass(false, None, now), Pass::Full, "never reconciled");
        let ahead = Some(now + chrono::TimeDelta::hours(5));
        assert_eq!(
            timer_pass(false, ahead, now),
            Pass::Full,
            "the clock went back"
        );
    }

    #[test]
    fn a_full_pass_asked_for_mid_pass_outranks_a_push() {
        // Both are remembered, but a full pass does a push anyway, so it wins.
        assert!(again_code(Pass::Full) > again_code(Pass::Delta));
        assert!(again_code(Pass::Delta) > again_code(Pass::Push));
        assert!(again_code(Pass::Push) > NOTHING_AGAIN);
    }

    #[test]
    fn every_pass_asked_for_mid_pass_is_one_the_re_dispatch_knows() {
        // `Pass::Delta` -- what `r` sends -- had a code and no arm, so pressing `r`
        // during the startup pull set the status line to "Syncing", toasted, and then
        // ran nothing. Comparing the codes' *order* did not catch it, because the order
        // was right; what was missing was a branch. Assert against the arms instead.
        for pass in [Pass::Push, Pass::Delta, Pass::Full] {
            let code = again_code(pass);
            assert!(
                matches!(code, PUSH_AGAIN | DELTA_AGAIN | FULL_AGAIN),
                "{pass:?} encodes to {code}, which the re-dispatch drops on the floor"
            );
        }
    }

    /// How long a test waits for a panic to be reported before calling it lost. Generous:
    /// on the unfixed code it is never reported at all, so this only bounds the failure.
    const REPORTED_WITHIN: Duration = Duration::from_secs(5);

    #[tokio::test]
    async fn a_task_that_panics_reports_it_to_the_loop() {
        // What `perform` used to do was `tokio::spawn` and drop the handle, and tokio
        // swallowed the panic at the task boundary: nothing reached the loop at all.
        let (tx, mut rx) = LoopTx::channel();
        spawn_effect(tx.clone(), async {
            panic!("an effect fell over: {}", 7);
        });
        let why = tokio::time::timeout(REPORTED_WITHIN, rx.fatal.recv())
            .await
            .expect("a panicking task must report something")
            .expect("the fatal channel is still open");
        assert_eq!(why, Fatal::Panicked("an effect fell over: 7".to_string()));

        // A literal payload, from the blocking side.
        spawn_blocking_effect(tx, || panic!("blocking work fell over"));
        let why = tokio::time::timeout(REPORTED_WITHIN, rx.fatal.recv())
            .await
            .expect("a panicking blocking task must report something")
            .expect("the fatal channel is still open");
        assert_eq!(why, Fatal::Panicked("blocking work fell over".to_string()));
    }

    #[tokio::test]
    async fn a_task_that_finishes_reports_nothing_and_keeps_its_handle() {
        // The handle is the task's own, so `flush_on_exit` can still await it.
        let (tx, mut rx) = LoopTx::channel();
        spawn_effect(tx, async {}).await.unwrap();
        assert!(
            rx.fatal.try_recv().is_err(),
            "a clean task reported a panic"
        );
    }

    #[test]
    fn a_panic_payload_is_described_whatever_it_carries() {
        assert_eq!(describe_panic(&"literal"), "literal");
        assert_eq!(describe_panic(&"formatted".to_string()), "formatted");
        assert_eq!(describe_panic(&42_u8), "a panic that carried no message");
    }

    /// The other half of BUG-7: once a task has panicked, the loop must leave the way `q`
    /// leaves -- by returning, so `run` drops the guard and flushes the outbox -- and must
    /// not draw again on the way, because the panic hook has already given the terminal
    /// back. On the unfixed code the loop never returned at all.
    #[tokio::test]
    // The lock only keeps the other guard-holding test off `RAW`; nothing else in this
    // runtime wants it, so holding it across the loop's awaits cannot deadlock.
    #[allow(clippy::await_holding_lock)]
    async fn a_panicked_task_ends_the_loop_through_the_normal_exit() {
        let _terminal = terminal::lock_for_test();
        let store = scratch_store();
        let config = Config::example();
        let mut model = Model::new(&config, tui_do_ui::Scope::All, now(), (120, 40));
        let (tx, mut rx) = LoopTx::channel();
        let mut guard = TerminalGuard::over(TestBackend::new(40, 5)).unwrap();

        // The task panics, and the panic hook gives the terminal back -- `over` installs
        // no hook, so this does what it would. A message already queued behind the panic
        // must not be drawn: the fatal report is read first.
        spawn_effect(tx.clone(), async { panic!("the sync pass fell over") });
        tokio::time::sleep(Duration::from_millis(50)).await;
        terminal::restore_to(&mut io::sink());
        tx.send(Msg::Tick(now())).unwrap();

        let exit = tokio::time::timeout(
            REPORTED_WITHIN,
            event_loop(&mut model, &mut guard, &tx, &mut rx, &store, None),
        )
        .await
        .expect("the loop kept running after a task panicked")
        .unwrap();

        assert_eq!(
            exit,
            Exit::Fatal(Fatal::Panicked("the sync pass fell over".to_string()))
        );
        let blank = ratatui::buffer::Buffer::empty(ratatui::layout::Rect::new(0, 0, 40, 5));
        assert_eq!(
            guard.backend().buffer(),
            &blank,
            "a frame was drawn after the panic hook gave the terminal back"
        );
        assert!(
            model.running,
            "the loop left on the fatal report, not because update was told to quit"
        );
    }

    /// BUG-25's second half: a failed read used to end the input thread with a bare
    /// `return`, telling nobody, and the loop went on drawing on ticks for an interface
    /// nobody could type at or quit. It must end the session the way a panic does.
    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn an_input_error_ends_the_loop_through_the_normal_exit() {
        let _terminal = terminal::lock_for_test();
        let store = scratch_store();
        let config = Config::example();
        let mut model = Model::new(&config, tui_do_ui::Scope::All, now(), (120, 40));
        let (tx, mut rx) = LoopTx::channel();
        let mut guard = TerminalGuard::over(TestBackend::new(40, 5)).unwrap();

        // One key, then the terminal fails. The key still arrives, and the reader stops
        // asking: a second call would mean it went round again on a dead terminal.
        let mut calls = 0;
        let stop = AtomicBool::new(false);
        read_input(&tx, &stop, |_| {
            calls += 1;
            match calls {
                1 => Ok(Some(crossterm::event::Event::Key(
                    crossterm::event::KeyCode::Down.into(),
                ))),
                2 => Err(io::Error::other("the tty went away")),
                _ => panic!("read again after the terminal failed"),
            }
        });
        // A tick the loop would otherwise go on drawing for.
        tx.send(Msg::Tick(now())).unwrap();

        let exit = tokio::time::timeout(
            REPORTED_WITHIN,
            event_loop(&mut model, &mut guard, &tx, &mut rx, &store, None),
        )
        .await
        .expect("the loop kept running after input was lost")
        .unwrap();

        let lost = Fatal::InputLost("the tty went away".to_string());
        assert_eq!(
            lost.to_string(),
            "input from the terminal was lost: the tty went away"
        );
        assert_eq!(exit, Exit::Fatal(lost));
        assert!(model.running, "the loop left on the fatal report");
    }

    #[test]
    fn a_closed_loop_ends_the_input_thread_quietly() {
        // Nobody left to send to is not a failure worth reporting -- the session is
        // already over.
        // On its own thread, so a reader that ignored the failed send fails this test
        // rather than hanging the run.
        let (done, finished) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let (tx, rx) = LoopTx::channel();
            drop(rx);
            let stop = AtomicBool::new(false);
            read_input(&tx, &stop, |_| {
                Ok(Some(crossterm::event::Event::Key(
                    crossterm::event::KeyCode::Down.into(),
                )))
            });
            let _ = done.send(());
        });
        finished
            .recv_timeout(REPORTED_WITHIN)
            .expect("the input thread kept reading after the loop had gone");
    }

    /// A store with one task queued for creation, and a sync engine pointed at `url`.
    async fn one_queued_create(url: &str) -> (Store, Arc<Sync>) {
        let store = scratch_store();
        store
            .upsert_projects(vec![a_project(1, "Inbox")])
            .await
            .unwrap();
        store
            .queue(Mutation::CreateTask {
                task: Box::new(tui_do_core::models::Task {
                    project_id: ProjectId(1),
                    title: "queued before the signal".to_string(),
                    ..tui_do_core::models::Task::default()
                }),
            })
            .await
            .unwrap();
        let client = Client::builder(url)
            .credentials(tui_do_api::Credentials::api_token("tk_test"))
            .build()
            .unwrap();
        let sync = Arc::new(Sync::new(client, store.clone()));
        (store, sync)
    }

    /// BUG-31: a signal is the same quit as `q`. It ends the loop by returning, so `run`
    /// still gives the terminal back and still flushes -- which the signal's default
    /// action never allowed. The server is a closed port, so the push answers at once
    /// and the flush says what became of the change.
    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn a_signal_ends_the_loop_through_the_normal_exit_and_still_flushes() {
        let _terminal = terminal::lock_for_test();
        let (store, sync) = one_queued_create("http://127.0.0.1:9").await;
        let config = Config::example();
        let mut model = Model::new(&config, tui_do_ui::Scope::All, now(), (120, 40));
        let (tx, mut rx) = LoopTx::channel();
        let mut guard = TerminalGuard::over(TestBackend::new(40, 5)).unwrap();

        tx.signals.send(Signal::Terminate).unwrap();
        // A key the loop would otherwise act on.
        tx.send(Msg::Tick(now())).unwrap();
        let exit = tokio::time::timeout(
            REPORTED_WITHIN,
            event_loop(&mut model, &mut guard, &tx, &mut rx, &store, None),
        )
        .await
        .expect("the loop kept running after SIGTERM")
        .unwrap();
        assert_eq!(exit, Exit::Signalled(Signal::Terminate));
        assert!(model.running, "the loop left on the signal, not on a quit");
        drop(guard);

        let mut said = Vec::new();
        let again = tokio::time::timeout(
            REPORTED_WITHIN,
            flush_unless_signalled(&store, &sync, &mut rx, &mut said),
        )
        .await
        .expect("the flush outran its own budget");
        assert_eq!(again, None, "no second signal was sent");
        let said = String::from_utf8(said).unwrap();
        assert!(said.contains("Sending 1 queued change"), "{said}");
        // The push answered (with the entry deferred) rather than being cut off.
        assert!(said.contains("still queued; the next run"), "{said}");
    }

    /// A second signal while the flush waits on a server means "now": it stops waiting at
    /// once instead of after the five-second grace, and the change stays queued. The
    /// server accepts the connection and never answers, so without the signal the flush
    /// would print its dots for the whole budget.
    #[tokio::test]
    async fn a_second_signal_stops_the_flush_waiting() {
        let silent = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", silent.local_addr().unwrap());
        let (store, sync) = one_queued_create(&url).await;
        let (tx, mut rx) = LoopTx::channel();

        let again = tx.signals.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            let _ = again.send(Signal::Quit);
        });
        let started = std::time::Instant::now();
        let mut said = Vec::new();
        let caught = tokio::time::timeout(
            FLUSH_TICK * 2,
            flush_unless_signalled(&store, &sync, &mut rx, &mut said),
        )
        .await
        .expect("the flush went on waiting after a second signal");
        assert_eq!(caught, Some(Signal::Quit));
        assert!(started.elapsed() < FLUSH_TICK * 2);
        let said = String::from_utf8(said).unwrap();
        assert!(said.contains("Sending 1 queued change"), "{said}");
        assert!(
            !said.contains("Still queued") && !said.contains("Sent."),
            "{said}"
        );
        assert_eq!(
            store.pending(None).await.unwrap().len(),
            1,
            "it stays queued"
        );
        drop(silent);
    }

    /// A push that panics unwinds through `Pushing`'s `Drop`, so `is_pushing` reads false
    /// afterwards as if it had finished. `flush_on_exit` then pushed the queue again, and
    /// if the panic came after the request reached the server that is the BUG-3 duplicate
    /// -- which BUG-7's fix made happen on every panicked push rather than never.
    ///
    /// The panic is a real one, through the real `push`: polled with no tokio runtime,
    /// the push's first store read calls `spawn_blocking` and panics, with the push
    /// already marked as running. The server is a closed loopback port, so a push that
    /// should not have happened shows up as a failed attempt on the entry rather than as
    /// a request to anything.
    #[test]
    fn a_push_that_panicked_is_not_sent_again_on_the_way_out() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let store = scratch_store();
        runtime.block_on(async {
            store
                .upsert_projects(vec![a_project(1, "Inbox")])
                .await
                .unwrap();
            store
                .queue(Mutation::CreateTask {
                    task: Box::new(tui_do_core::models::Task {
                        project_id: ProjectId(1),
                        title: "sent, then panicked".to_string(),
                        ..tui_do_core::models::Task::default()
                    }),
                })
                .await
                .unwrap();
        });
        let client = Client::builder("http://127.0.0.1:9")
            .credentials(tui_do_api::Credentials::api_token("tk_test"))
            .build()
            .unwrap();
        let sync = Arc::new(Sync::new(client, store.clone()));

        let panicked = std::panic::catch_unwind(AssertUnwindSafe(|| {
            let mut push = Box::pin(sync.push());
            let _ = push
                .as_mut()
                .poll(&mut TaskContext::from_waker(std::task::Waker::noop()));
        }));
        assert!(panicked.is_err(), "the push was meant to panic");
        assert!(!sync.is_pushing(), "unwinding clears the running flag");

        runtime.block_on(flush_on_exit(&store, &sync, &mut io::sink()));

        let queued = runtime.block_on(store.pending(None)).unwrap();
        assert_eq!(queued.len(), 1, "the entry stays queued for the next run");
        assert_eq!(
            queued[0].attempts, 0,
            "flush_on_exit pushed again after a push had panicked partway"
        );
    }

    /// The flush pushes outside `spawn_sync`'s one-at-a-time guard, so anything that could
    /// still start a pass -- a pass's `AGAIN` tail, a late `Effect::Apply`, the timer --
    /// would push alongside it, and two drains can read the same create before either
    /// marks it sent. Once the flush has begun, nothing may start another.
    ///
    /// The server is a closed loopback port, so a pass that did start shows up as a
    /// failed attempt on the entry and as events on the loop's channel.
    #[tokio::test]
    async fn no_pass_starts_once_the_quit_flush_has_begun() {
        let store = scratch_store();
        let client = Client::builder("http://127.0.0.1:9")
            .credentials(tui_do_api::Credentials::api_token("tk_test"))
            .build()
            .unwrap();
        let sync = Arc::new(Sync::new(client, store.clone()));
        // Nothing queued yet, so the flush itself sends nothing.
        flush_on_exit(&store, &sync, &mut io::sink()).await;

        store
            .upsert_tasks(vec![tui_do_core::models::Task {
                id: tui_do_core::models::TaskId(4),
                project_id: ProjectId(1),
                title: "late".to_string(),
                ..tui_do_core::models::Task::default()
            }])
            .await
            .unwrap();
        store
            .queue(Mutation::DeleteTask {
                before: Box::new(
                    store
                        .task(tui_do_core::models::TaskId(4))
                        .await
                        .unwrap()
                        .unwrap(),
                ),
            })
            .await
            .unwrap();
        let (tx, mut rx) = LoopTx::channel();
        spawn_sync(Arc::clone(&sync), tx, Pass::Push, Trigger::Scheduled);
        tokio::time::sleep(Duration::from_millis(500)).await;

        assert!(
            rx.msg.try_recv().is_err(),
            "a pass started after the flush began"
        );
        let queued = store.pending(None).await.unwrap();
        assert_eq!(queued.len(), 1);
        assert_eq!(queued[0].attempts, 0, "a pass pushed after the flush began");
    }

    /// A store with nothing in it, for a test to queue against without touching
    /// `~/.local/share/tui-do`.
    fn scratch_store() -> Store {
        Store::in_memory().expect("an in-memory store opens")
    }

    /// Build a task the way `tui-do add` does, against a default project so a bare
    /// `+project` is never required just to exercise quick-add syntax.
    fn build_task(text: &str) -> tui_do_ui::QuickAdd {
        let projects = [a_project(1, "Inbox")];
        let parsed = tui_do_core::quickadd::parse(text, &now());
        tui_do_ui::quickadd_task(&parsed, &projects, &[], &[], None, None)
            .expect("the default project resolves")
    }

    #[tokio::test]
    async fn an_unknown_label_is_left_off_unless_the_flag_says_otherwise() {
        let store = scratch_store();
        let built = build_task("Call the VA *waiting");
        assert_eq!(built.unknown_labels, vec!["waiting".to_string()]);

        queue_add(&store, built.clone(), false).await.unwrap();
        let queued = store.pending(None).await.unwrap();
        assert_eq!(
            queued.iter().map(|e| e.mutation.kind()).collect::<Vec<_>>(),
            vec!["create_task"]
        );
    }

    #[tokio::test]
    async fn the_flag_queues_the_label_before_the_task_that_carries_it() {
        // Order is the contract: the create is queued first so the attach `decompose`
        // puts behind the task can be retargeted when the server names the label.
        let store = scratch_store();
        let built = build_task("Call the VA *waiting");
        queue_add(&store, built, true).await.unwrap();
        let queued = store.pending(None).await.unwrap();
        assert_eq!(
            queued.iter().map(|e| e.mutation.kind()).collect::<Vec<_>>(),
            vec!["create_label", "create_task", "attach_label"]
        );

        // Kinds alone would pass a version that attached a fresh `Label::default()`
        // (id 0) instead of the one `Store::queue` actually allocated -- `decompose`
        // emits an `AttachLabel` for any non-empty `task.labels` regardless of what id
        // is on it. What this task exists to establish is that the attach names the
        // *same* label the create did, and that the id is provisional (negative) until
        // the server answers.
        let tui_do_core::store::Mutation::CreateLabel { label: created } = &queued[0].mutation
        else {
            panic!("expected a create_label first: {:?}", queued[0].mutation);
        };
        assert!(
            created.id.get() < 0,
            "a label not yet sent should carry a provisional (negative) id: {:?}",
            created.id
        );
        let tui_do_core::store::Mutation::AttachLabel {
            label: attached, ..
        } = &queued[2].mutation
        else {
            panic!("expected an attach_label third: {:?}", queued[2].mutation);
        };
        assert_eq!(
            attached.id, created.id,
            "the attach must name the label the create allocated"
        );
    }

    #[tokio::test]
    async fn a_repeated_unknown_label_queues_one_create_not_two() {
        // A title is not unique to Vikunja -- two `CreateLabel`s for two spellings of
        // one typo both answer `201`, per CLAUDE.md's replay table, and the pool gets a
        // permanent duplicate. `resolve_labels` (the shared root `quickadd_task` calls
        // into) dedupes case-insensitively, so this is really a test that `queue_add`
        // trusts what it is handed rather than re-splitting it.
        let store = scratch_store();
        let built = build_task("Call the VA *waiting *Waiting *WAITING");
        assert_eq!(built.unknown_labels, vec!["waiting".to_string()]);

        queue_add(&store, built, true).await.unwrap();
        let queued = store.pending(None).await.unwrap();
        assert_eq!(
            queued
                .iter()
                .filter(|e| e.mutation.kind() == "create_label")
                .count(),
            1,
            "one name, one label: {queued:?}"
        );
    }
}
