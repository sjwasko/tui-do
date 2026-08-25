//! The effect runtime: the half of criax that is allowed to block.
//!
//! `criax-tui` describes what it wants as [`Effect`] values and learns what happened as
//! [`Msg`] values. This module is what turns one into the other — it owns the terminal,
//! the store, the API client and the sync timer, and it is the only place in criax where
//! anything is awaited.
//!
//! The loop is deliberately shaped so that no store read or network call sits between a
//! keypress and a frame: every effect is spawned onto its own task and answers by sending
//! a message back. The render loop's only await is on the message channel.

mod terminal;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use criax_api::Client;
use criax_core::models::ProjectId;
use criax_core::store::{LabelFilter, LabelSort, ProjectFilter, ProjectSort, LAST_PROJECT};
use criax_core::{Config, Store, Sync};
use criax_tui::model::landing_scope;
use criax_tui::theme::{ColorDepth, Theme};
use criax_tui::update::reload_everything;
use criax_tui::{update, view, Effect, Model, Msg};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

use terminal::TerminalGuard;

/// How often the clock is reported to the model.
///
/// One second is enough for "synced 2m ago" and for a toast to expire, and cheap enough
/// that a redraw per tick does not register.
const TICK: Duration = Duration::from_secs(1);

/// How long the input reader waits before checking whether it should stop.
const INPUT_POLL: Duration = Duration::from_millis(100);

/// Run the interface until the user quits.
///
/// # Errors
/// Anything that stops criax from starting: an unreadable store, an unusable terminal, a
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

    let mut model = Model::new(&config, scope, chrono::Utc::now(), size);
    model.theme = Theme::new(color_depth());
    if let Some(problem) = credential_problem {
        model.status.sync = criax_tui::model::SyncStatus::Failed {
            message: problem.clone(),
        };
        model.toast(criax_tui::model::Toast::error(problem));
    }

    let stop = Arc::new(AtomicBool::new(false));
    spawn_input(tx.clone(), Arc::clone(&stop));
    spawn_ticks(tx.clone(), Arc::clone(&stop));
    if let Some(sync) = sync.clone() {
        spawn_sync_timer(sync, config.sync.clone(), Arc::clone(&stop), tx.clone());
    }

    let result = event_loop(&mut model, &mut guard, rx, &tx, &store, sync.as_ref()).await;
    stop.store(true, Ordering::SeqCst);
    result
}

/// Read messages, update, draw. The only loop in criax.
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
                    spawn_sync(sync, tx, Pass::Push);
                }
            });
        }
        Effect::SyncNow => {
            if let Some(sync) = sync {
                spawn_sync(Arc::clone(sync), tx.clone(), Pass::Full);
            }
        }
        // The loop notices `model.running` rather than being killed from here, so the
        // terminal is restored on the way out of `run` in every case.
        Effect::Quit => {}
        // `Effect` is `non_exhaustive` so Phase 4 can add write effects without breaking
        // this crate. An effect this build does not know about is not silently dropped.
        other => tracing::warn!(?other, "unhandled effect"),
    }
}

/// Build the sync engine, if there is a credential to use.
///
/// Returns the engine and, when there is none, why. Neither case is an error: criax
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
        .credentials(criax_api::Credentials::api_token(token))
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
    /// Push, then pull. What the timer asks for.
    Full,
}

/// Run one sync pass, forwarding its events into the loop.
fn spawn_sync(sync: Arc<Sync>, tx: UnboundedSender<Msg>, pass: Pass) {
    /// One pass at a time. Two overlapping passes would push the same queue twice, and
    /// the queue is ordered — an entry sent twice is a task created twice.
    static RUNNING: AtomicBool = AtomicBool::new(false);
    /// An edit arrived while a pass was running. Without this, a change made during the
    /// startup pull — which takes half a minute against 3,877 tasks — sits in the queue
    /// until the five-minute timer comes round, and the user is told nothing.
    static AGAIN: AtomicBool = AtomicBool::new(false);

    if RUNNING.swap(true, Ordering::SeqCst) {
        if pass == Pass::Push {
            AGAIN.store(true, Ordering::SeqCst);
        }
        return;
    }
    tokio::spawn(async move {
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
        let outcome = match pass {
            Pass::Push => engine.push().await.map(|_| ()),
            Pass::Full => engine.once().await.map(|_| ()),
        };
        if let Err(error) = outcome {
            let _ = tx.send(Msg::Sync(criax_core::SyncEvent::Failed {
                phase: criax_core::sync::Phase::Pull,
                message: error.to_string(),
            }));
        }
        forward.abort();
        RUNNING.store(false, Ordering::SeqCst);
        // A push asked for while this pass was running still has to happen.
        if AGAIN.swap(false, Ordering::SeqCst) {
            spawn_sync(sync, tx, Pass::Push);
        }
    });
}

/// Read terminal events on a blocking thread.
///
/// crossterm's reader blocks, so it lives on its own thread rather than in the async
/// runtime, and polls so it can notice that criax is shutting down.
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
            if stop.load(Ordering::SeqCst) || tx.send(Msg::Tick(chrono::Utc::now())).is_err() {
                return;
            }
        }
    });
}

/// Sync on a timer, and once at startup.
fn spawn_sync_timer(
    sync: Arc<Sync>,
    settings: criax_core::config::SyncConfig,
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
            spawn_sync(Arc::clone(&sync), tx.clone(), Pass::Full);
        }
    });
}

/// How much colour this terminal can show.
///
/// Read once, here, rather than anywhere in `criax-tui`: the UI layer is a pure function
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
) -> anyhow::Result<()> {
    let store_path = Store::default_path().context("could not decide where to keep the store")?;
    let store = Store::open(&store_path)
        .await
        .with_context(|| format!("could not open the store at {}", store_path.display()))?;

    let parsed = criax_core::quickadd::parse(text, &chrono::Utc::now());
    if parsed.title.trim().is_empty() {
        anyhow::bail!("nothing to add");
    }

    let projects = store
        .projects(ProjectFilter::default(), ProjectSort::default())
        .await
        .context("could not read the project list")?;
    let labels = store
        .labels(LabelFilter::default(), LabelSort::default())
        .await
        .unwrap_or_default();

    let built =
        criax_tui::quickadd_task(&parsed, &projects, &labels, None).ok_or_else(|| match parsed
            .project
            .as_deref()
        {
            Some(name) => anyhow::anyhow!(
                "no project called \"{name}\". criax has {} cached — run criax once to sync",
                projects.len()
            ),
            None => anyhow::anyhow!("no project to add to; run criax once to sync the list"),
        })?;

    let title = built.task.title.clone();
    let project_id = built.task.project_id;
    let project = projects
        .iter()
        .find(|project| project.id == project_id)
        .map_or_else(|| project_id.to_string(), |project| project.title.clone());
    store
        .queue(criax_core::store::Mutation::CreateTask {
            task: Box::new(built.task),
        })
        .await
        .context("could not queue the task")?;

    // Named with its id when the name is ambiguous, because a task added to the wrong
    // Inbox is a task the user will not find.
    if built.ambiguous_project {
        println!(
            "Added \"{title}\" to {project} (#{project_id}) — more than one project has that name"
        );
    } else {
        println!("Added \"{title}\" to {project}");
    }
    if !built.unknown_labels.is_empty() {
        println!(
            "No label called {} — criax cannot create labels yet, so it was left off.",
            built.unknown_labels.join(", ")
        );
    }

    if offline {
        println!("Queued. The next run will send it.");
        return Ok(());
    }

    let (sync, problem) = build_sync(config, config_path, &store);
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
        // interface or `criax add` -- sends it. Failing here would throw away a task the
        // user has already been told was added.
        Ok(report) => println!("Queued — {} still waiting to be sent.", report.deferred),
        Err(error) => println!("Queued — could not reach the server: {error}"),
    }
    Ok(())
}
