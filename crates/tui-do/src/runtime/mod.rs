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

mod terminal;

use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use anyhow::Context;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tui_do_api::Client;
use tui_do_core::models::ProjectId;
use tui_do_core::store::{LabelFilter, LabelSort, ProjectFilter, ProjectSort, LAST_PROJECT};
use tui_do_core::sync::Reach;
use tui_do_core::{Config, Store, Sync};
use tui_do_ui::model::landing_scope;
use tui_do_ui::model::UrlAction;
use tui_do_ui::theme::{ColorDepth, Theme};
use tui_do_ui::update::reload_everything;
use tui_do_ui::{update, view, Effect, Model, Msg};

use terminal::TerminalGuard;

/// How often the clock is reported to the model.
///
/// One second is enough for "synced 2m ago" and for a toast to expire, and cheap enough
/// that a redraw per tick does not register.
const TICK: Duration = Duration::from_secs(1);

/// How long the input reader waits before checking whether it should stop.
const INPUT_POLL: Duration = Duration::from_millis(100);

/// The current time, carrying this machine's UTC offset.
///
/// The only clock in the program that the interface sees, and the offset is the point of
/// it. `due today` is resolved against this, and so is the decision to call a date
/// "Today" — both are claims about the calendar the user is living in, and both answer
/// wrongly if the instant arrives labelled UTC. Stamped as `Utc::now()`, `due today`
/// lands at 23:59 UTC, which west of Greenwich is that same evening: the task turns
/// overdue hours early, every day. That is the bug in the project tui-do replaces.
///
/// `fixed_offset` rather than `DateTime<Local>` so the value carries its offset with it
/// rather than depending on the reader's zone, which is what lets `tui-do-ui` stay pure
/// and its tests stay deterministic. Re-read on every tick, so a DST change is picked up
/// within a second without restarting.
fn now() -> chrono::DateTime<chrono::FixedOffset> {
    chrono::Local::now().fixed_offset()
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
    let store_path = Store::default_path().context("could not decide where to keep the store")?;
    let store = Store::open(&store_path)
        .await
        .with_context(|| format!("could not open the store at {}", store_path.display()))?;

    let (tx, rx) = mpsc::unbounded_channel::<Msg>();

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
    let size = guard
        .terminal()
        .size()
        .map_or((120, 40), |s| (s.width, s.height));

    let mut model = Model::new(&config, scope, now(), size);
    model.theme = Theme::new(color_depth());
    model.url_action = url_action();
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
        spawn_sync_timer(sync, config.sync.clone(), Arc::clone(&stop), tx.clone());
    }

    let result = event_loop(&mut model, &mut guard, rx, &tx, &store, sync.as_ref()).await;
    stop.store(true, Ordering::SeqCst);

    // The terminal goes back to the user before anything is printed to it.
    drop(guard);
    if let Some(sync) = &sync {
        flush_on_exit(&store, sync).await;
    }
    result
}

/// Read messages, update, draw. The only loop in tui-do.
async fn event_loop(
    model: &mut Model,
    guard: &mut TerminalGuard,
    mut rx: UnboundedReceiver<Msg>,
    tx: &UnboundedSender<Msg>,
    store: &Store,
    sync: Option<&Arc<Sync>>,
) -> anyhow::Result<()> {
    for effect in reload_everything(model) {
        perform(effect, store, sync, tx);
    }
    draw(model, guard)?;

    while let Some(msg) = rx.recv().await {
        let mut effects = update(model, msg);
        // Drain whatever else has already arrived before drawing. A sync pass finishing
        // sends four answers at once, and drawing between each of them is three wasted
        // frames.
        while let Ok(next) = rx.try_recv() {
            effects.extend(update(model, next));
        }
        for effect in effects {
            perform(effect, store, sync, tx);
        }
        if !model.running {
            break;
        }
        draw(model, guard)?;
    }
    Ok(())
}

fn draw(model: &Model, guard: &mut TerminalGuard) -> anyhow::Result<()> {
    guard
        .terminal()
        .draw(|frame| view(model, frame))
        .context("could not draw to the terminal")?;
    Ok(())
}

/// Execute one effect, off the render loop.
///
/// Every arm spawns and returns immediately. Nothing here awaits inline, because this
/// function is called from the loop that also draws.
fn perform(effect: Effect, store: &Store, sync: Option<&Arc<Sync>>, tx: &UnboundedSender<Msg>) {
    match effect {
        Effect::LoadTasks { id, filter, sort } => {
            let (store, tx) = (store.clone(), tx.clone());
            tokio::spawn(async move {
                let msg = match store.tasks(filter, sort).await {
                    Ok(tasks) => Msg::TasksLoaded { id, tasks },
                    Err(error) => Msg::StoreFailed(error.to_string()),
                };
                let _ = tx.send(msg);
            });
        }
        Effect::LoadProjects => {
            let (store, tx) = (store.clone(), tx.clone());
            tokio::spawn(async move {
                let msg = match store
                    .projects(ProjectFilter::default(), ProjectSort::default())
                    .await
                {
                    Ok(projects) => Msg::ProjectsLoaded(projects),
                    Err(error) => Msg::StoreFailed(error.to_string()),
                };
                let _ = tx.send(msg);
            });
        }
        Effect::LoadLabels => {
            let (store, tx) = (store.clone(), tx.clone());
            tokio::spawn(async move {
                let msg = match store
                    .labels(LabelFilter::default(), LabelSort::default())
                    .await
                {
                    Ok(labels) => Msg::LabelsLoaded(labels),
                    Err(error) => Msg::StoreFailed(error.to_string()),
                };
                let _ = tx.send(msg);
            });
        }
        Effect::LoadCounts => {
            let (store, tx) = (store.clone(), tx.clone());
            tokio::spawn(async move {
                let msg = match store.project_task_counts().await {
                    Ok(counts) => Msg::CountsLoaded(counts),
                    Err(error) => Msg::StoreFailed(error.to_string()),
                };
                let _ = tx.send(msg);
            });
        }
        Effect::LoadPending => {
            let (store, tx) = (store.clone(), tx.clone());
            tokio::spawn(async move {
                if let Ok(health) = store.queue_health().await {
                    let _ = tx.send(Msg::PendingLoaded(health));
                }
            });
        }
        Effect::RememberProject(project) => {
            let store = store.clone();
            tokio::spawn(async move {
                let value = project.map_or_else(String::new, |id| id.get().to_string());
                // A failure here costs the user their landing project next launch. It is
                // not worth a toast in front of the task they are reading.
                let _ = store.set_state(LAST_PROJECT, &value).await;
            });
        }
        Effect::Apply(mutation) => {
            let (store, tx) = (store.clone(), tx.clone());
            let sync = sync.cloned();
            tokio::spawn(async move {
                if let Err(error) = store.queue(mutation).await {
                    // The model has already shown the change. Saying the store refused it
                    // is the only honest thing to do; the reload below puts the list back
                    // to what was actually written.
                    let _ = tx.send(Msg::StoreFailed(error.to_string()));
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
            // tokio feature for one call. `xdg-open` hands the URL to a handler and exits
            // immediately, so this waits on a fork-exec and not on a browser.
            tokio::task::spawn_blocking(move || {
                let result = std::process::Command::new("xdg-open")
                    .arg(&url)
                    // Inherited handles would let the handler write over the alternate
                    // screen -- the interface owns this terminal.
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status();
                let failure = match result {
                    Ok(status) if status.success() => None,
                    Ok(status) => Some(format!("xdg-open exited with {status}")),
                    Err(error) => Some(format!("could not run xdg-open: {error}")),
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
    let token = match config.api_token(config_path) {
        Ok(Some(token)) => token,
        Ok(None) => {
            return (
                None,
                Some("No API token configured — showing the local cache only.".to_string()),
            )
        }
        Err(error) => return (None, Some(format!("Not syncing: {error}"))),
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
    /// Push, then pull only what the server says has changed. What `r` asks for.
    ///
    /// Between a push and a full pass in every sense: it costs a page rather than
    /// seventy-eight, and it sees everything a full pass does except deletions.
    Delta,
    /// Push, then pull everything, removing what the server no longer has. What startup,
    /// the timer, and `R` ask for.
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

fn spawn_sync(sync: Arc<Sync>, tx: UnboundedSender<Msg>, pass: Pass, trigger: Trigger) {
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
    let handle = tokio::spawn(async move {
        let (events_tx, mut events_rx) = mpsc::unbounded_channel();
        let forward = {
            let tx = tx.clone();
            tokio::spawn(async move {
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
        if let Err(error) = outcome {
            let _ = tx.send(Msg::Sync(tui_do_core::SyncEvent::Failed {
                phase: tui_do_core::sync::Phase::Pull,
                message: error.to_string(),
            }));
        }
        // Drop the engine so its sender closes, then let the forwarder finish draining.
        //
        // This used to `abort()` here, which raced the last event out of the channel --
        // and the last event is `Finished`, the only one that reloads. The pull updated
        // the store, the interface was never told, and the list stayed as it was until
        // the next launch. A task added by `tui-do add` was in the database and not on
        // the screen, which is exactly what it looked like from the outside.
        drop(engine);
        let _ = forward.await;
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
async fn flush_on_exit(store: &Store, sync: &Arc<Sync>) {
    let pending = store.pending_count().await.unwrap_or(0);
    if pending <= 0 {
        return;
    }
    if let Ok(mut slot) = in_flight().lock() {
        if let Some(handle) = slot.take() {
            handle.abort();
        }
    }

    let changes = if pending == 1 { "change" } else { "changes" };
    print!("Sending {pending} queued {changes}");
    let _ = io::stdout().flush();

    // A dot a second, rather than one message and a silent wait. On an unroutable
    // server the push cannot answer, and a still screen there reads as a hang —
    // which is the one thing this project is not allowed to look like. The dots
    // are also the clock: when the last one lands, the wait is over.
    let syncer = (*sync).clone();
    let push = syncer.push();
    tokio::pin!(push);
    let mut outcome = None;
    for _ in 0..FLUSH_DOTS {
        tokio::select! {
            answered = &mut push => { outcome = Some(answered); break }
            () = tokio::time::sleep(FLUSH_TICK) => {
                print!(".");
                let _ = io::stdout().flush();
            }
        }
    }
    println!();

    match outcome {
        Some(Ok(report)) if report.is_complete() => println!("Sent."),
        Some(Ok(report)) => println!(
            "{} still queued; the next run will send them.",
            report.deferred
        ),
        Some(Err(error)) => println!("Still queued — could not reach the server: {error}"),
        None => println!("Still queued — the server did not answer in time."),
    }
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
    found.next().is_none().then(|| first.title.clone())
}

/// Read terminal events on a blocking thread.
///
/// crossterm's reader blocks, so it lives on its own thread rather than in the async
/// runtime, and polls so it can notice that tui-do is shutting down.
fn spawn_input(tx: UnboundedSender<Msg>, stop: Arc<AtomicBool>) {
    std::thread::spawn(move || {
        while !stop.load(Ordering::SeqCst) {
            match crossterm::event::poll(INPUT_POLL) {
                Ok(true) => match crossterm::event::read() {
                    Ok(crossterm::event::Event::Key(key)) => {
                        if tx.send(Msg::Key(key)).is_err() {
                            return;
                        }
                    }
                    Ok(crossterm::event::Event::Resize(width, height)) => {
                        if tx.send(Msg::Resize(width, height)).is_err() {
                            return;
                        }
                    }
                    Ok(_) => {}
                    Err(_) => return,
                },
                Ok(false) => {}
                Err(_) => return,
            }
        }
    });
}

/// Report the time, so `update` never has to read a clock.
fn spawn_ticks(tx: UnboundedSender<Msg>, stop: Arc<AtomicBool>) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(TICK);
        loop {
            interval.tick().await;
            if stop.load(Ordering::SeqCst) || tx.send(Msg::Tick(now())).is_err() {
                return;
            }
        }
    });
}

/// Sync on a timer, and once at startup.
fn spawn_sync_timer(
    sync: Arc<Sync>,
    settings: tui_do_core::config::SyncConfig,
    stop: Arc<AtomicBool>,
    tx: UnboundedSender<Msg>,
) {
    if !settings.enabled {
        return;
    }
    tokio::spawn(async move {
        let mut interval =
            tokio::time::interval(Duration::from_secs(settings.interval_seconds.max(30)));
        loop {
            interval.tick().await;
            if stop.load(Ordering::SeqCst) {
                return;
            }
            spawn_sync(
                Arc::clone(&sync),
                tx.clone(),
                Pass::Full,
                Trigger::Scheduled,
            );
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
fn url_action() -> UrlAction {
    if std::env::var_os("SSH_CONNECTION").is_some() || std::env::var_os("SSH_TTY").is_some() {
        return UrlAction::Copy;
    }
    if std::env::var_os("WAYLAND_DISPLAY").is_none() && std::env::var_os("DISPLAY").is_none() {
        return UrlAction::Copy;
    }
    UrlAction::Open
}

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
    offline: bool,
    create_labels: bool,
) -> anyhow::Result<()> {
    let store_path = Store::default_path().context("could not decide where to keep the store")?;
    let store = Store::open(&store_path)
        .await
        .with_context(|| format!("could not open the store at {}", store_path.display()))?;

    let parsed = tui_do_core::quickadd::parse(text, &now());
    if parsed.title.trim().is_empty() {
        anyhow::bail!("nothing to add");
    }

    let (sync, problem) = build_sync(config, config_path, &store);

    let mut projects = store
        .projects(ProjectFilter::default(), ProjectSort::default())
        .await
        .context("could not read the project list")?;
    let mut labels = store
        .labels(LabelFilter::default(), LabelSort::default())
        .await
        .unwrap_or_default();

    let mut built = tui_do_ui::quickadd_task(
        &parsed,
        &projects,
        &labels,
        None,
        config.view.default_project.as_deref(),
    );

    // A name that matches nothing may mean the cache is empty or simply older than the
    // project it names. Both are worth one cheap request each to settle — this is the one
    // case where reading from the local store alone is worse than asking the server.
    if built.is_none() && !offline {
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
                    None,
                    config.view.default_project.as_deref(),
                );
            }
        }
    }

    let built = built.ok_or_else(|| match parsed.project.as_deref() {
        Some(name) => match longer_project(&projects, name) {
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
                describe(&projects)
            ),
        },
        None => anyhow::anyhow!(
            "no project to add to — tui-do knows of {}",
            describe(&projects)
        ),
    })?;

    let project_id = built.task.project_id;
    let project = projects
        .iter()
        .find(|project| project.id == project_id)
        .map_or_else(|| project_id.to_string(), |project| project.title.clone());
    let queued = queue_add(&store, built, create_labels).await?;

    // Named with its id when the name is ambiguous, because a task added to the wrong
    // Inbox is a task the user will not find.
    if queued.ambiguous_project {
        println!(
            "Added \"{}\" to {project} (#{project_id}) — more than one project has that name",
            queued.title
        );
    } else {
        println!("Added \"{}\" to {project}", queued.title);
    }
    if let Some(note) = queued.backdated {
        println!("Note: {note}.");
    }
    if !queued.created_labels.is_empty() {
        println!("Created label {}.", queued.created_labels.join(", "));
    }
    if !queued.unknown_labels.is_empty() {
        // Scoped to next time in the *text*, not only in the comment: the natural reading
        // of a CLI hint is "re-run me with this flag", and the task above is already
        // queued, so doing that would add a second task rather than fixing this one. The
        // hint used to say neither, and it said "create it" however many names it had
        // just listed.
        let (names, pronoun, object) = if queued.unknown_labels.len() == 1 {
            ("No label called", "it was", "it")
        } else {
            ("No labels called", "they were", "them")
        };
        println!(
            "{names} {} — {pronoun} left off. Pass --create-labels next time to have \
             tui-do create {object}; this task is already queued, so re-running now would \
             add a second one.",
            queued.unknown_labels.join(", ")
        );
    }

    if offline {
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
    match sync.push().await {
        Ok(report) if report.is_complete() => println!("Sent."),
        // Not an error: the local store has it, the outbox has it, and the next run --
        // interface or `tui-do add` -- sends it. Failing here would throw away a task the
        // user has already been told was added.
        Ok(report) => println!("Queued — {} still waiting to be sent.", report.deferred),
        Err(error) => println!("Queued — could not reach the server: {error}"),
    }
    Ok(())
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
    // Read before the task is moved into the mutation. Same rule as the interface: a date
    // already gone by is allowed, never confirmed quietly.
    let backdated = tui_do_ui::past_due_note(built.task.due_date.get(), now());

    store
        .queue(tui_do_core::store::Mutation::CreateTask {
            task: Box::new(built.task),
        })
        .await
        .context("could not queue the task")?;

    Ok(Queued {
        title,
        ambiguous_project,
        backdated,
        created_labels,
        unknown_labels,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use tokio::sync::mpsc;
    use tui_do_core::sync::{Phase, PullReport, PushReport, SyncReport};
    use tui_do_core::SyncEvent;

    use super::*;

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
        tui_do_ui::quickadd_task(&parsed, &projects, &[], None, None)
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
