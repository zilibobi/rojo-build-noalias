pub mod batcher;

use crate::cli::{BuildArgs, OutputTarget};
use crate::diff::Hints;
use crate::pipeline::Pipeline;
use crate::{log, paths, shutdown};
use batcher::{Batcher, MAX_WAIT, QUIET};
use notify::event::{AccessKind, AccessMode};
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher as _};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::Instant;

/// What the loop waits on: a file event, or a shutdown request (a signal, or EPIPE on stdout).
pub enum Msg {
    Fs(notify::Result<notify::Event>),
    Shutdown,
}

/// Watch mode (spec 7.2): start, build, then one diff, apply and at most one build per batch of
/// file events, until a shutdown request. A failed batch keeps the last good build and the loop
/// keeps watching; only a failed start is an error.
pub fn run(args: &BuildArgs) -> anyhow::Result<()> {
    let (tx, rx) = mpsc::channel();
    let stop = tx.clone();
    shutdown::on_request(move || {
        let _ = stop.send(Msg::Shutdown);
    });

    let pipeline = Pipeline::start(args)?;

    if shutdown::requested() {
        return Ok(());
    }

    let project_dir = pipeline.project_dir().to_path_buf();
    let canonical = dunce::canonicalize(&project_dir)
        .ok()
        .filter(|c| *c != project_dir);
    let shown = match &args.output {
        OutputTarget::File(given) => given.display().to_string(),
        OutputTarget::Plugin(name) => format!("plugin {name}"),
    };

    // watching before the first build queues what changes during it
    let mut watch = Loop {
        watcher: watch_mounts(&tx, &pipeline),
        pipeline,
        tx,
        batcher: Batcher::new(QUIET, MAX_WAIT),
        project_dir,
        canonical,
        shown,
        owed: false,
        full_next: false,
        rewatch_owed: false,
    };

    if log::verbose() > 0 {
        log::info("Watch mode enabled. Watching for file changes...\n");
    }

    if let Err(e) = watch.pipeline.build() {
        if shutdown::requested() {
            return Ok(());
        }

        log::error(&format!("{e:#}"));
        watch.owed = true;
    }

    // anything that changed while starting
    watch.batcher.push(Instant::now(), [], true);
    watch.run(&rx);

    Ok(())
}

struct Loop {
    pipeline: Pipeline,
    /// dropped and made again on a rewatch, so no stale watch survives a root that came back
    watcher: Option<RecommendedWatcher>,
    tx: Sender<Msg>,
    batcher: Batcher,
    project_dir: PathBuf,
    /// the project dir with links resolved, when that differs: FSEvents reports real paths
    canonical: Option<PathBuf>,
    /// OUTPUT as given, or `plugin <name>`
    shown: String,
    /// the last build or batch failed, so the next batch that applies builds even when it wrote
    /// nothing
    owed: bool,
    /// the next batch is a full rescan
    full_next: bool,
    /// the watches are made again after the next batch that applies
    rewatch_owed: bool,
}

impl Loop {
    fn run(&mut self, rx: &Receiver<Msg>) {
        loop {
            // no batch open: block with no timeout, so an idle watch costs nothing
            let msg = match self.batcher.deadline() {
                None => match rx.recv() {
                    Ok(msg) => Some(msg),
                    Err(_) => return,
                },
                Some(deadline) => {
                    match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                        Ok(msg) => Some(msg),
                        Err(RecvTimeoutError::Timeout) => None,
                        Err(RecvTimeoutError::Disconnected) => return,
                    }
                }
            };

            match msg {
                Some(Msg::Shutdown) => return,
                Some(Msg::Fs(event)) => self.event(event),
                None => {}
            }

            if shutdown::requested() {
                return;
            }

            if let Some(hints) = self.batcher.take(Instant::now()) {
                self.batch(hints);
            }

            if shutdown::requested() {
                return;
            }
        }
    }

    /// Adds the paths of one event that can concern the build to the open batch. A watcher error
    /// or a rescan flag (a lost event) makes the batch a full rescan.
    fn event(&mut self, event: notify::Result<notify::Event>) {
        let now = Instant::now();

        let event = match event {
            Ok(event) if !event.need_rescan() => event,
            _ => {
                self.batcher.push(now, [], true);
                return;
            }
        };

        // opens and reads, the diff's own included; a finished write still counts
        match event.kind {
            EventKind::Access(AccessKind::Close(AccessMode::Write)) => {}
            EventKind::Access(_) => return,
            _ => {}
        }

        let paths: Vec<PathBuf> = event
            .paths
            .iter()
            .map(|p| self.localize(p))
            .filter(|p| self.pipeline.relevant(p))
            .collect();

        if !paths.is_empty() {
            self.batcher.push(now, paths, false);
        }
    }

    /// An event path spelled under the project dir as the pipeline knows it.
    fn localize(&self, path: &Path) -> PathBuf {
        let path = paths::normalize(path);

        if path.starts_with(&self.project_dir) {
            return path;
        }

        match &self.canonical {
            Some(real) => match path.strip_prefix(real) {
                Ok(rel) => self.project_dir.join(rel),
                Err(_) => path,
            },
            None => path,
        }
    }

    /// One closed batch. Any error, or a panic, keeps the last good build and makes the next
    /// batch a full rescan; a shutdown that interrupted the batch is not reported.
    fn batch(&mut self, mut hints: Hints) {
        hints.full |= std::mem::take(&mut self.full_next);
        let result = catch_unwind(AssertUnwindSafe(|| self.land(&hints)));

        if shutdown::requested() {
            return;
        }

        let message = match result {
            Ok(Ok(())) => return,
            Ok(Err(e)) => format!("{e:#}"),
            Err(panic) => format!("panic: {}", panic_text(panic.as_ref())),
        };

        log::error(&message);
        log::warn(&format!("keeping the last good build at {}", self.shown));
        self.owed = true;
        self.full_next = true;
        self.rewatch_owed = true;
    }

    /// Diff, apply, rewatch when the mounts moved, and build when the mirror changed or a build
    /// is owed. Never builds after a failed apply.
    fn land(&mut self, hints: &Hints) -> anyhow::Result<()> {
        let started = Instant::now();
        let changes = self.pipeline.diff(hints)?;

        if changes.is_empty() && !self.owed {
            return Ok(());
        }

        let applied = self.pipeline.apply(&changes)?;

        if log::verbose() > 0 && !changes.is_empty() {
            log::info(&format!(
                "{} changed, {} added, {} removed in {} ms",
                changes.changed.len(),
                changes.added.len(),
                changes.removed.len(),
                started.elapsed().as_millis()
            ));
        }

        if applied.rewatch || self.rewatch_owed {
            self.rewatch();
        }

        if shutdown::requested() {
            return Ok(());
        }

        if applied.dirty() || self.owed {
            self.pipeline.build()?;
            self.owed = false;
        }

        Ok(())
    }

    /// Makes the watches again from the current mounts. A file written into a new root before
    /// its watch existed is caught by the full rescan this queues.
    fn rewatch(&mut self) {
        self.watcher = None;
        self.watcher = watch_mounts(&self.tx, &self.pipeline);
        self.rewatch_owed = false;
        self.batcher.push(Instant::now(), [], true);
    }
}

/// A watcher on what the pipeline mounts: each directory root recursively, and shallowly the
/// directories above roots, projects and `.luaurc` files (spec 7.2). A root that does not exist
/// is not watched; the shallow watch on its parent sees it appear. `None` when no watcher can be
/// made at all, which is warned about: the loop then only reacts to a shutdown.
fn watch_mounts(tx: &Sender<Msg>, pipeline: &Pipeline) -> Option<RecommendedWatcher> {
    let tx = tx.clone();
    let made = notify::recommended_watcher(move |event| {
        let _ = tx.send(Msg::Fs(event));
    });

    let mut watcher = match made {
        Ok(watcher) => watcher,
        Err(e) => {
            log::warn(&format!("cannot watch for file changes: {e}"));
            return None;
        }
    };

    let dir = pipeline.project_dir();
    let (deep, shallow) = pipeline.mounts().watch_dirs();
    let deep = deep.iter().map(|r| (r, RecursiveMode::Recursive));
    let shallow = shallow.iter().map(|r| (r, RecursiveMode::NonRecursive));

    for (rel, mode) in deep.chain(shallow) {
        let path = if rel.as_os_str().is_empty() {
            dir.to_path_buf()
        } else {
            dir.join(rel)
        };

        if let Err(e) = watcher.watch(&path, mode)
            && path.exists()
        {
            log::warn(&format!("cannot watch {}: {e}", path.display()));
        }
    }

    Some(watcher)
}

fn panic_text(panic: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = panic.downcast_ref::<&str>() {
        return s.to_string();
    }

    if let Some(s) = panic.downcast_ref::<String>() {
        return s.clone();
    }

    "unknown cause".to_string()
}
