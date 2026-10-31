use crate::cli::{BuildArgs, OutputTarget};
use crate::diff::{self, ChangeSet, Hints};
use crate::project::{self, Mounts};
use crate::rojo::{BuildOut, Rojo};
use crate::scopes::{self, ScopeSet};
use crate::workspace::{self, Meta, Snapshot, Workspace};
use crate::{log, output, paths, report, shutdown};
use anyhow::Context;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Instant;

/// Build mode's state (spec 6.5): a per-run workspace whose `tree/` mirrors what the project
/// mounts, every script in it de-aliased, and what the mirror was made from.
pub struct Pipeline {
    args: BuildArgs,
    project_dir: PathBuf,
    /// the output file, absolute; `None` in plugin mode
    output: Option<PathBuf>,
    /// the file a symlinked output points to, resolved at start and after every apply
    output_target: Option<PathBuf>,
    rojo: Rojo,
    mounts: Mounts,
    snapshot: Snapshot,
    /// every mirrored script's source bytes, keyed relative to the project directory
    raw: BTreeMap<PathBuf, Vec<u8>>,
    scopes: ScopeSet,
    ws: Workspace,
    /// set while an apply runs and left set when it fails: the next diff is a full rescan and the
    /// next apply rewrites every script, so a half-applied batch cannot linger
    recover: bool,
}

/// What [`Pipeline::apply`] did to the mirror.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Applied {
    /// mirror files written and directories created
    pub wrote: usize,
    /// mirror entries removed
    pub removed: usize,
    /// the directories to watch changed, or a root appeared or vanished
    pub rewatch: bool,
}

impl Applied {
    /// Whether the mirror changed, so a build would give a different model.
    pub fn dirty(&self) -> bool {
        self.wrote + self.removed > 0
    }
}

impl Pipeline {
    /// Creates the workspace, mirrors the mount closure into it and de-aliases every covered
    /// script, printing the replacement table with `-v`. Nothing is built yet.
    pub fn start(args: &BuildArgs) -> anyhow::Result<Pipeline> {
        // one spelling for every comparison and message: the one events arrive in (macOS reports
        // /private/var for /var) and the long one on Windows (no 8.3 short names)
        let project_dir = paths::resolved(&args.target.dir);
        let verbose = log::verbose() > 0;

        if !project_dir.is_dir() {
            anyhow::bail!(
                "the project directory {} does not exist",
                project_dir.display()
            );
        }

        let output = match &args.output {
            OutputTarget::File(given) => {
                let cwd = std::env::current_dir().context("cannot read the current directory")?;
                Some(paths::resolved_parent(&paths::normalize(&cwd.join(given))))
            }
            OutputTarget::Plugin(_) => None,
        };

        let ws = Workspace::create()?;
        let tree = ws.tree();

        if verbose {
            log::info(&format!(
                "Copying project from {} to temporary workspace...",
                project_dir.display()
            ));
        }

        // Rojo takes default.project.json from a directory
        let cli_project = args
            .target
            .file_name
            .as_deref()
            .unwrap_or("default.project.json");
        let mounts = project::mount_closure(
            &project_dir,
            Path::new(cli_project),
            args.target.file_name.as_deref(),
        )?;
        let (snapshot, raw) = workspace::mirror_mounts(&project_dir, &tree, &mounts)?;

        let rojo = Rojo {
            cwd: project_dir.clone(),
            color: args.opts.color,
            verbose: args.opts.verbose,
        };

        let mut pipeline = Pipeline {
            args: args.clone(),
            project_dir,
            output_target: output.as_deref().and_then(link_target),
            output,
            rojo,
            mounts,
            snapshot,
            raw,
            scopes: ScopeSet {
                root: tree,
                scopes: Vec::new(),
                wax: args.opts.wax_compat,
            },
            ws,
            recover: false,
        };

        for rel in pipeline.output_rels() {
            if pipeline.snapshot.entries.contains_key(&rel) {
                pipeline.unmirror(&rel)?;
            }
        }

        pipeline.dealias_all()?;

        Ok(pipeline)
    }

    /// Builds the scopes of every mirrored `.luaurc`, rewrites every covered script from its raw
    /// bytes and prints the table (`init.luau:240-339`).
    fn dealias_all(&mut self) -> anyhow::Result<()> {
        let verbose = log::verbose() > 0;
        let tree = self.tree();
        let luaurcs = self.luaurcs();

        if luaurcs.is_empty() {
            if verbose {
                log::warn("No .luaurc files found. Running standard Rojo build...");
            }

            return Ok(());
        }

        if verbose {
            log::info(&format!(
                "Found {} .luaurc file(s). Processing aliases...\n",
                luaurcs.len()
            ));
        }

        let started = Instant::now();
        let project_dir = &self.project_dir;
        let set = self.build_scopes(None)?;
        let mut rows = vec![Vec::new(); set.scopes.len()];
        let mut checked = vec![0usize; set.scopes.len()];

        for (rel, src) in &self.raw {
            let file = tree.join(rel);

            for (i, scope) in set.scopes.iter().enumerate() {
                if scope.covers(&file) {
                    checked[i] += 1;
                }
            }

            if !set.covers(&file) {
                continue;
            }

            let (out, made) = set.rewrite(&file, src);

            if out != *src {
                workspace::write_if_changed(&file, &out)?;
            }

            for (i, row) in made {
                rows[i].push(row);
            }
        }

        let original = |path: &Path| match path.strip_prefix(&tree) {
            Ok(rel) => project_dir.join(rel),
            Err(_) => path.to_path_buf(),
        };

        let logs = report::logs(&set, rows, &checked, &original);
        report::print(&logs, started.elapsed());

        if verbose {
            log::blank();
        }

        self.scopes = set;

        Ok(())
    }

    /// Every mirrored `.luaurc` in the tree, in scope order.
    fn luaurcs(&self) -> Vec<PathBuf> {
        let tree = self.tree();
        let mut luaurcs: Vec<PathBuf> = self.mounts.luaurcs.iter().map(|r| tree.join(r)).collect();
        scopes::order(&mut luaurcs);
        luaurcs
    }

    /// The scopes of the mirror: as startup builds them (`previous` is `None`, never fails), or
    /// rebuilt in watch mode, where a scope `previous` had that can no longer be built is an error
    /// naming the original file.
    fn build_scopes(&self, previous: Option<&ScopeSet>) -> anyhow::Result<ScopeSet> {
        let tree = self.tree();
        let luaurcs = self.luaurcs();
        let project_dir = &self.project_dir;
        let file_name = self.args.target.file_name.as_deref();

        // the scope project is looked up in the project, which holds files the mirror does not
        let lookup = |luaurc: &Path| -> Option<PathBuf> {
            let dir = luaurc.parent()?.strip_prefix(&tree).ok()?;
            let preferred = file_name.filter(|_| dir.as_os_str().is_empty());
            let found = project::find_for_luaurc(&project_dir.join(dir), preferred)?;
            Some(tree.join(found.strip_prefix(project_dir).ok()?))
        };

        let (opts, rojo) = (&self.args.opts, &self.rojo);

        let Some(previous) = previous else {
            return Ok(ScopeSet::build(&tree, &luaurcs, &lookup, opts, rojo));
        };

        ScopeSet::rebuild(&tree, &luaurcs, &lookup, opts, rojo, previous).map_err(|e| {
            let text = format!("{e:#}");
            let shown = tree.display().to_string();
            anyhow::anyhow!(
                "{}",
                text.replace(&shown, &project_dir.display().to_string())
            )
        })
    }

    /// One `rojo build` of the mirror into `out/build.<ext of OUTPUT>`, swapped into place, then
    /// the marker. In plugin mode Rojo writes the plugin itself. Refused while the last apply
    /// failed, since the mirror may then hold a half-applied batch.
    pub fn build(&mut self) -> anyhow::Result<()> {
        if self.recover {
            anyhow::bail!(
                "the last batch failed to apply, so the mirror is not built until one applies"
            );
        }

        let project = self.args.target.project_in(&self.tree());

        if log::verbose() > 0 {
            log::info("Running Rojo build...");
        }

        match (&self.args.output, &self.output) {
            (OutputTarget::File(given), Some(dest)) => {
                let ext = given
                    .extension()
                    .map(|e| format!(".{}", e.to_string_lossy()))
                    .unwrap_or_default();
                let built = self.ws.out().join(format!("build{ext}"));

                self.rojo.build(&project, &BuildOut::File(built.clone()))?;
                output::swap(&built, dest)?;
                log::success(&format!("Built project to {}", given.display()));
            }
            (OutputTarget::Plugin(name), _) => {
                self.rojo.build(&project, &BuildOut::Plugin(name.clone()))?;
                log::success(&format!("Built project to plugin {name}"));
            }
            (OutputTarget::File(_), None) => unreachable!("a file output always has a path"),
        }

        Ok(())
    }

    /// The mirror Rojo builds.
    pub fn tree(&self) -> PathBuf {
        self.ws.tree()
    }

    pub fn project_dir(&self) -> &Path {
        &self.project_dir
    }

    pub fn mounts(&self) -> &Mounts {
        &self.mounts
    }

    /// The output file as an absolute path; `None` in plugin mode.
    pub fn output(&self) -> Option<&Path> {
        self.output.as_deref()
    }

    pub fn snapshot(&self) -> &Snapshot {
        &self.snapshot
    }

    pub fn raw(&self) -> &BTreeMap<PathBuf, Vec<u8>> {
        &self.raw
    }

    pub fn scopes(&self) -> &ScopeSet {
        &self.scopes
    }

    /// The diff of `hints` against the snapshot (spec 7.2). After a failed apply it is a full
    /// rescan with `config` set, so it is never empty and the next apply redoes everything.
    pub fn diff(&self, hints: &Hints) -> anyhow::Result<ChangeSet> {
        let full = Hints {
            paths: BTreeSet::new(),
            full: true,
        };
        let hints = if self.recover { &full } else { hints };

        let mut changes = diff::diff(
            &self.project_dir,
            &self.mounts,
            &self.snapshot,
            &self.raw,
            hints,
        )?;
        changes.config |= self.recover;

        for set in [
            &mut changes.added,
            &mut changes.removed,
            &mut changes.changed,
        ] {
            set.retain(|rel| !self.is_output(rel));
        }

        Ok(changes)
    }

    /// The output, and the file a symlinked output points to, relative to the project directory
    /// when they lie inside it.
    fn output_rels(&self) -> Vec<PathBuf> {
        self.output
            .iter()
            .chain(&self.output_target)
            .filter_map(|p| p.strip_prefix(&self.project_dir).ok())
            .map(Path::to_path_buf)
            .collect()
    }

    /// Whether `rel` is the output or a `.rbn-*` temp file beside it. An output inside a mount
    /// is never mirrored, so a build never holds the build before it.
    fn is_output(&self, rel: &Path) -> bool {
        let temp = rel
            .file_name()
            .is_some_and(|n| n.to_string_lossy().starts_with(".rbn-"));

        self.output_rels()
            .iter()
            .any(|out| out == rel || (temp && out.parent() == rel.parent()))
    }

    /// Whether an event on `path` can concern the build: a path the build reads (under a root, a
    /// project file, a `.luaurc`, the meta file of a file root), a `.luaurc` or project file in a
    /// directory above a mount, or such a directory itself (a renamed or recreated parent of a
    /// root). Never the output, the file a symlinked output points to, or a `.rbn-*` temp file.
    pub fn relevant(&self, path: &Path) -> bool {
        let temp = path
            .file_name()
            .is_some_and(|n| n.to_string_lossy().starts_with(".rbn-"));

        if temp {
            return false;
        }

        let path = paths::normalize(path);

        if self.output.as_ref() == Some(&path) || self.output_target.as_ref() == Some(&path) {
            return false;
        }

        let Ok(rel) = path.strip_prefix(&self.project_dir) else {
            return false;
        };

        diff::wanted(&self.project_dir, &self.mounts, rel) || diff::above_mount(&self.mounts, rel)
    }

    /// Applies one batch's changes to the mirror (spec 7.2): the raw cache, the snapshot and the
    /// mirror entries; for a config change the mount closure, newly mounted paths and pruned ones;
    /// the scopes when entries came or went or a `.json` changed; then every dirty script is
    /// de-aliased again from its raw bytes and written only when its bytes differ. On an error the
    /// previous scopes stay, and the next diff and apply start over from a full rescan.
    pub fn apply(&mut self, changes: &ChangeSet) -> anyhow::Result<Applied> {
        let recovering = self.recover;
        self.recover = true;
        self.output_target = self.output.as_deref().and_then(link_target);
        let applied = self.apply_changes(changes, recovering)?;
        self.recover = false;

        Ok(applied)
    }

    fn apply_changes(&mut self, changes: &ChangeSet, recovering: bool) -> anyhow::Result<Applied> {
        let started = Instant::now();
        let config = changes.config || recovering;
        let watched = self.mounts.watch_dirs();
        let mut batch = Batch::default();

        // the closure first: a project that does not parse fails the batch before anything moves
        let mut mounts = if config {
            self.closure()?
        } else {
            self.mounts.clone()
        };
        mounts.files = mounts
            .roots
            .iter()
            .filter(|r| self.project_dir.join(r).is_file())
            .cloned()
            .collect();

        for rel in changes.removed.iter().rev() {
            batch.removed += self.unmirror(rel)?;
        }

        let touched: BTreeSet<&PathBuf> = changes.added.iter().chain(&changes.changed).collect();

        for rel in touched {
            // a project file beside a mount that the new closure does not read
            if !mounts.contains(rel) {
                continue;
            }

            self.mirror_entry(rel, &mut batch)?;
        }

        if config {
            self.reconcile(&mounts, &mut batch)?;
        }

        self.mounts = mounts;

        let rescope = config
            || batch.removed > 0
            || !changes.added.is_empty()
            || !changes.removed.is_empty()
            || !batch.added_scripts.is_empty()
            || changes
                .changed
                .iter()
                .any(|r| r.extension().is_some_and(|e| e == "json"));

        let tree = self.tree();
        let mut dirty: BTreeSet<PathBuf> = changes
            .changed
            .iter()
            .chain(&changes.added)
            .filter(|r| self.raw.contains_key(*r))
            .cloned()
            .collect();
        dirty.extend(batch.added_scripts.iter().cloned());

        if rescope {
            let set = self.build_scopes(Some(&self.scopes))?;

            if recovering {
                dirty.extend(self.raw.keys().cloned());
            } else {
                let moved = self
                    .raw
                    .keys()
                    .filter(|r| !set.same_chain(&self.scopes, &tree.join(r)));
                dirty.extend(moved.cloned());
            }

            self.scopes = set;
        }

        let mut rows = vec![Vec::new(); self.scopes.scopes.len()];

        for rel in &dirty {
            let Some(src) = self.raw.get(rel) else {
                continue;
            };

            let file = tree.join(rel);
            let (out, made) = self.scopes.rewrite(&file, src);

            if workspace::write_if_changed(&file, &out)? {
                batch.wrote.insert(rel.clone());
            }

            for (i, row) in made {
                rows[i].push(row);
            }
        }

        self.print_rows(rows, started.elapsed());

        let roots_moved = self
            .mounts
            .roots
            .iter()
            .any(|r| changes.added.contains(r) || changes.removed.contains(r));

        Ok(Applied {
            wrote: batch.wrote.len(),
            removed: batch.removed,
            rewatch: roots_moved || self.mounts.watch_dirs() != watched,
        })
    }

    /// The mount closure of the CLI project, read from the project directory.
    fn closure(&self) -> anyhow::Result<Mounts> {
        let cli_project = self
            .args
            .target
            .file_name
            .as_deref()
            .unwrap_or("default.project.json");

        project::mount_closure(
            &self.project_dir,
            Path::new(cli_project),
            self.args.target.file_name.as_deref(),
        )
    }

    /// Mirrors the added or changed entry `rel` from the project. A script that is new is created
    /// empty, so that the sourcemaps see it without raw aliases ever sitting in the mirror, and is
    /// written once its scopes are known; a changed one is only rewritten then. An entry that has
    /// gone since the diff is removed.
    fn mirror_entry(&mut self, rel: &Path, batch: &mut Batch) -> anyhow::Result<()> {
        let src = self.project_dir.join(rel);
        let dest = self.tree().join(rel);

        let Ok(meta) = std::fs::metadata(&src) else {
            batch.removed += self.unmirror(rel)?;
            return Ok(());
        };

        let mtime = meta.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH);

        if meta.is_dir() {
            if dest.is_file() {
                batch.removed += self.unmirror(rel)?;
            }

            if !dest.is_dir() {
                std::fs::create_dir_all(&dest)
                    .with_context(|| format!("cannot create {}", dest.display()))?;
                batch.wrote.insert(rel.to_path_buf());
            }

            self.snapshot
                .entries
                .insert(rel.to_path_buf(), Meta::dir(mtime));
            return Ok(());
        }

        let Ok(bytes) = std::fs::read(&src) else {
            batch.removed += self.unmirror(rel)?;
            return Ok(());
        };

        if dest.is_dir() {
            batch.removed += self.unmirror(rel)?;
        }

        let meta = Meta::file(rel, &bytes, mtime);

        if paths::is_script(rel) {
            if !dest.is_file() {
                workspace::place_script(&dest)?;
                batch.wrote.insert(rel.to_path_buf());
                batch.added_scripts.insert(rel.to_path_buf());
            }

            self.raw.insert(rel.to_path_buf(), bytes);
        } else if workspace::write_if_changed(&dest, &bytes)? {
            batch.wrote.insert(rel.to_path_buf());
        }

        self.snapshot.entries.insert(rel.to_path_buf(), meta);

        Ok(())
    }

    /// Removes `rel` and everything below it from the mirror, the snapshot and the raw cache, then
    /// every directory above it that only held it. Returns how many snapshot entries went.
    fn unmirror(&mut self, rel: &Path) -> anyhow::Result<usize> {
        let dest = self.tree().join(rel);

        match std::fs::symlink_metadata(&dest) {
            Ok(m) if m.is_dir() => std::fs::remove_dir_all(&dest),
            Ok(_) => std::fs::remove_file(&dest),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
        .with_context(|| format!("cannot remove {}", dest.display()))?;

        let gone: Vec<PathBuf> = self
            .snapshot
            .entries
            .range(rel.to_path_buf()..)
            .map(|(k, _)| k)
            .take_while(|k| k.starts_with(rel))
            .cloned()
            .collect();

        for k in &gone {
            self.snapshot.entries.remove(k);
            self.raw.remove(k);
        }

        self.remove_empty_parents(rel);

        Ok(gone.len())
    }

    /// The directories above `rel` that the snapshot does not hold and that are empty now: the
    /// mirror created them only as parents.
    fn remove_empty_parents(&self, rel: &Path) {
        let tree = self.tree();

        for dir in rel.ancestors().skip(1) {
            if dir.as_os_str().is_empty() || self.snapshot.entries.contains_key(dir) {
                return;
            }

            if std::fs::remove_dir(tree.join(dir)).is_err() {
                return;
            }
        }
    }

    /// After a config change: prunes what the new closure no longer reads (a directory that still
    /// holds a mounted path stays as its parent) and mirrors every root, meta file, project file
    /// and `.luaurc` the mirror lacks.
    fn reconcile(&mut self, mounts: &Mounts, batch: &mut Batch) -> anyhow::Result<()> {
        let unread: Vec<PathBuf> = self
            .snapshot
            .entries
            .keys()
            .filter(|k| !mounts.contains(k))
            .cloned()
            .collect();

        // deepest first, so a directory is looked at after what it held
        for rel in unread.iter().rev() {
            let holds_mounted = self
                .snapshot
                .entries
                .range(rel.clone()..)
                .skip(1)
                .take_while(|(k, _)| k.starts_with(rel))
                .next()
                .is_some();

            // the mirror keeps it as a parent, as a fresh mirror would make it
            if holds_mounted {
                self.snapshot.entries.remove(rel);
                continue;
            }

            batch.removed += self.unmirror(rel)?;
        }

        let mut wanted: Vec<PathBuf> = Vec::new();

        for root in &mounts.roots {
            wanted.push(root.clone());

            if mounts.files.contains(root) {
                wanted.push(project::meta_of(root));
            }
        }

        wanted.extend(mounts.projects.iter().cloned());
        wanted.extend(mounts.luaurcs.iter().cloned());

        let tree = self.tree();

        for rel in wanted {
            if self.snapshot.entries.contains_key(&rel) {
                continue;
            }

            let touched = workspace::mirror_path(
                &self.project_dir,
                &tree,
                &rel,
                &mut self.snapshot,
                &mut self.raw,
            )?;

            for rel in touched {
                if self.raw.contains_key(&rel) {
                    batch.added_scripts.insert(rel.clone());
                }

                batch.wrote.insert(rel);
            }
        }

        Ok(())
    }

    /// One batch's replacement rows, with `-v` only (divergence 10).
    fn print_rows(
        &self,
        rows: Vec<Vec<crate::dealias::Replacement>>,
        elapsed: std::time::Duration,
    ) {
        if log::verbose() == 0 || rows.iter().all(Vec::is_empty) {
            return;
        }

        let tree = self.tree();
        let original = |path: &Path| match path.strip_prefix(&tree) {
            Ok(rel) => self.project_dir.join(rel),
            Err(_) => path.to_path_buf(),
        };

        let logs: Vec<report::ProjectLog> = self
            .scopes
            .scopes
            .iter()
            .zip(rows)
            .filter(|(_, rows)| !rows.is_empty())
            .map(|(scope, rows)| report::ProjectLog {
                title: report::title(scope, &original),
                rows,
            })
            .collect();

        report::print(&logs, elapsed);
    }
}

/// The file `output` points to when it is a symlink.
fn link_target(output: &Path) -> Option<PathBuf> {
    let target = paths::resolved_parent(&paths::normalize(&output::through_links(output).ok()?));
    (target != output).then_some(target)
}

/// What one apply has done so far.
#[derive(Default)]
struct Batch {
    /// mirror paths written or created
    wrote: BTreeSet<PathBuf>,
    removed: usize,
    /// scripts that are new in the mirror, written raw until they are rewritten
    added_scripts: BTreeSet<PathBuf>,
}

/// A one-shot build: start, build once, and remove the workspace. A shutdown request between the
/// two builds nothing.
pub fn run_once(args: &BuildArgs) -> anyhow::Result<()> {
    let mut pipeline = Pipeline::start(args)?;

    if shutdown::requested() {
        anyhow::bail!("interrupted");
    }

    pipeline.build()
}
