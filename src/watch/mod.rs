pub mod batcher;

use crate::cli::{BuildArgs, OutputTarget};
use crate::diff::Hints;
use crate::pipeline::Pipeline;
use crate::{log, paths, shutdown};
use batcher::{Batcher, MAX_WAIT, QUIET};
use notify::event::{AccessKind, AccessMode};
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher as _};
use std::collections::BTreeSet;
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

    let shown = match &args.output {
        OutputTarget::File(given) => given.display().to_string(),
        OutputTarget::Plugin(name) => format!("plugin {name}"),
    };

    // watching before the first build queues what changes during it
    let mut warned = Warned::default();
    let watches = watch_mounts(&tx, &pipeline, &mut warned);
    let mut watch = Loop {
        watches,
        pipeline,
        tx,
        batcher: Batcher::new(QUIET, MAX_WAIT),
        real: false,
        warned,
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
    watches: Watches,
    tx: Sender<Msg>,
    batcher: Batcher,
    /// a file event went into the open batch; a batch holding only the rescans the loop queued
    /// itself never pays an owed build
    real: bool,
    warned: Warned,
    /// OUTPUT as given, or `plugin <name>`
    shown: String,
    /// the last build or batch failed, so the next batch with a file event builds even when it
    /// wrote nothing
    owed: bool,
    /// the next batch is a full rescan
    full_next: bool,
    /// the watches are made again after the next batch that applies; set by a failed diff,
    /// apply or panic, never by a failed build
    rewatch_owed: bool,
}

/// How a batch failed: a failed build leaves the mirror and the watches as they should be.
enum Failure {
    Batch(anyhow::Error),
    Build(anyhow::Error),
}

impl From<anyhow::Error> for Failure {
    fn from(e: anyhow::Error) -> Failure {
        Failure::Batch(e)
    }
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
    /// (warned once per cause) or a rescan flag (a lost event) makes the batch a full rescan.
    fn event(&mut self, event: notify::Result<notify::Event>) {
        let now = Instant::now();

        let event = match event {
            Ok(event) if !event.need_rescan() => event,
            Ok(_) => {
                self.real = true;
                self.batcher.push(now, [], true);
                return;
            }
            Err(e) => {
                self.warned.warn(&e, None);
                self.real = true;
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
            .flat_map(|p| rebase(p, &self.watches.aliases))
            .filter(|p| self.pipeline.relevant(p))
            .collect();

        if !paths.is_empty() {
            self.real = true;
            self.batcher.push(now, paths, false);
        }
    }

    /// One closed batch. Any error, or a panic, keeps the last good build and makes the next
    /// batch a full rescan; a shutdown that interrupted the batch is not reported.
    fn batch(&mut self, mut hints: Hints) {
        hints.full |= std::mem::take(&mut self.full_next);
        let real = std::mem::take(&mut self.real);
        let result = catch_unwind(AssertUnwindSafe(|| self.land(&hints, real)));

        if shutdown::requested() {
            return;
        }

        let (message, rewatch) = match result {
            Ok(Ok(())) => return,
            Ok(Err(Failure::Build(e))) => (format!("{e:#}"), false),
            Ok(Err(Failure::Batch(e))) => (format!("{e:#}"), true),
            Err(panic) => (format!("panic: {}", panic_text(panic.as_ref())), true),
        };

        log::error(&message);
        log::warn(&format!("keeping the last good build at {}", self.shown));
        self.owed = true;
        self.full_next = true;
        self.rewatch_owed |= rewatch;
    }

    /// Diff, apply, rewatch when the mounts moved, and build when the mirror changed, or when a
    /// build is owed and a file event is in the batch (`real`). Never builds after a failed
    /// apply, and a failing build is retried only on the next file event, never on a rescan the
    /// loop queued itself.
    fn land(&mut self, hints: &Hints, real: bool) -> Result<(), Failure> {
        let started = Instant::now();
        let pays = self.owed && real;
        let changes = self.pipeline.diff(hints)?;

        if changes.is_empty() && !pays {
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

        if applied.dirty() || pays {
            self.pipeline.build().map_err(Failure::Build)?;
            self.owed = false;
        }

        Ok(())
    }

    /// Makes the watches again from the current mounts. A file written into a new root before
    /// its watch existed is caught by the full rescan this queues.
    fn rewatch(&mut self) {
        self.watches = Watches::default();
        self.watches = watch_mounts(&self.tx, &self.pipeline, &mut self.warned);
        self.rewatch_owed = false;
        self.batcher.push(Instant::now(), [], true);
    }
}

/// The watcher and how the watched paths are spelled once links are resolved.
#[derive(Default)]
struct Watches {
    /// held only to keep the watches alive
    _watcher: Option<RecommendedWatcher>,
    /// (resolved, as the pipeline spells it) for every watched path whose resolved spelling
    /// differs: FSEvents reports real paths, so a symlinked root or project dir reports its target
    aliases: Vec<(PathBuf, PathBuf)>,
}

/// A watcher on what the pipeline mounts: each directory root recursively, and shallowly the
/// directories above roots, projects and `.luaurc` files (spec 7.2). A root that does not exist
/// is not watched; the shallow watch on its parent sees it appear. No watcher when none can be
/// made at all, which is warned about: the loop then only reacts to a shutdown.
fn watch_mounts(tx: &Sender<Msg>, pipeline: &Pipeline, warned: &mut Warned) -> Watches {
    let tx = tx.clone();
    let made = notify::recommended_watcher(move |event| {
        let _ = tx.send(Msg::Fs(event));
    });

    let mut watcher = match made {
        Ok(watcher) => watcher,
        Err(e) => {
            log::warn(&format!("cannot watch for file changes: {e}"));
            return Watches::default();
        }
    };

    let dir = pipeline.project_dir();
    let (deep, shallow) = pipeline.mounts().watch_dirs();
    let deep = deep.iter().map(|r| (r, RecursiveMode::Recursive));
    let shallow = shallow.iter().map(|r| (r, RecursiveMode::NonRecursive));
    let mut aliases = Vec::new();

    for (rel, mode) in deep.chain(shallow) {
        let path = if rel.as_os_str().is_empty() {
            dir.to_path_buf()
        } else {
            dir.join(rel)
        };

        if let Err(e) = watcher.watch(&path, mode)
            && path.exists()
        {
            warned.warn(&e, Some(&path));
        }

        if let Ok(real) = dunce::canonicalize(&path)
            && real != path
        {
            aliases.push((real, path));
        }
    }

    Watches {
        _watcher: Some(watcher),
        aliases,
    }
}

/// `path` as the event gave it, plus each spelling it has under a watched path whose resolved
/// spelling it starts with. Every candidate is offered to `relevant()`.
fn rebase(path: &Path, aliases: &[(PathBuf, PathBuf)]) -> Vec<PathBuf> {
    let path = paths::normalize(path);
    let mut out = vec![path.clone()];

    for (real, given) in aliases {
        if let Ok(rel) = path.strip_prefix(real) {
            let spelled = if rel.as_os_str().is_empty() {
                given.clone()
            } else {
                given.join(rel)
            };

            if !out.contains(&spelled) {
                out.push(spelled);
            }
        }
    }

    out
}

/// Watcher errors already warned about, by cause, so a recurring one warns once.
#[derive(Default)]
struct Warned(BTreeSet<String>);

impl Warned {
    fn warn(&mut self, e: &notify::Error, path: Option<&Path>) {
        let cause = cause(e);

        if !self.0.insert(cause.clone()) {
            return;
        }

        let path = path.or(e.paths.first().map(PathBuf::as_path));

        match path {
            Some(path) => log::warn(&format!("cannot watch {}: {cause}", path.display())),
            None => log::warn(&format!("file watching failed: {cause}")),
        }
    }
}

/// What went wrong, without the paths, so one cause warns once.
fn cause(e: &notify::Error) -> String {
    match &e.kind {
        notify::ErrorKind::MaxFilesWatch => "the limit on watched folders is reached \
            (on Linux, fs.inotify.max_user_watches), so changes in some folders are missed; \
            raise it, for example with `sudo sysctl fs.inotify.max_user_watches=524288`, and \
            restart the watch"
            .to_string(),
        notify::ErrorKind::Generic(text) => text.clone(),
        notify::ErrorKind::Io(io) => io.to_string(),
        other => format!("{other:?}"),
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rebase_spells_a_resolved_path_as_the_pipeline_does() {
        let aliases = vec![
            (PathBuf::from("/private/var/p"), PathBuf::from("/var/p")),
            (
                PathBuf::from("/private/var/p/real_pkg"),
                PathBuf::from("/var/p/pkg"),
            ),
        ];

        assert_eq!(
            rebase(Path::new("/private/var/p/src/a.luau"), &aliases),
            [
                PathBuf::from("/private/var/p/src/a.luau"),
                PathBuf::from("/var/p/src/a.luau")
            ]
        );
        assert_eq!(
            rebase(Path::new("/private/var/p/real_pkg/x.luau"), &aliases),
            [
                PathBuf::from("/private/var/p/real_pkg/x.luau"),
                PathBuf::from("/var/p/real_pkg/x.luau"),
                PathBuf::from("/var/p/pkg/x.luau")
            ]
        );
        assert_eq!(
            rebase(Path::new("/private/var/p/real_pkg"), &aliases)[2],
            PathBuf::from("/var/p/pkg")
        );
        assert_eq!(
            rebase(Path::new("/var/p/src/./a.luau"), &aliases),
            [PathBuf::from("/var/p/src/a.luau")]
        );
    }

    #[test]
    fn a_watcher_error_warns_once_per_cause_and_names_the_watch_limit() {
        let limit = notify::Error::new(notify::ErrorKind::MaxFilesWatch);
        assert!(cause(&limit).contains("fs.inotify.max_user_watches"));

        let mut warned = Warned::default();
        warned.warn(&limit, Some(Path::new("/p/a")));
        warned.warn(&limit, Some(Path::new("/p/b")));
        warned.warn(&notify::Error::generic("boom"), None);
        assert_eq!(warned.0.len(), 2);
    }
}
