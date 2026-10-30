use crate::cli::{BuildArgs, OutputTarget};
use crate::project::{self, Mounts};
use crate::rojo::{BuildOut, Rojo};
use crate::scopes::{self, ScopeSet};
use crate::workspace::{self, Snapshot, Workspace};
use crate::{log, output, paths, report};
use anyhow::Context;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

/// Build mode's state (spec 6.5): a per-run workspace whose `tree/` mirrors what the project
/// mounts, every script in it de-aliased, and what the mirror was made from.
pub struct Pipeline {
    args: BuildArgs,
    project_dir: PathBuf,
    /// the output file, absolute; `None` in plugin mode
    output: Option<PathBuf>,
    rojo: Rojo,
    mounts: Mounts,
    snapshot: Snapshot,
    /// every mirrored script's source bytes, keyed relative to the project directory
    raw: BTreeMap<PathBuf, Vec<u8>>,
    scopes: ScopeSet,
    ws: Workspace,
}

impl Pipeline {
    /// Creates the workspace, mirrors the mount closure into it and de-aliases every covered
    /// script, printing the replacement table with `-v`. Nothing is built yet.
    pub fn start(args: &BuildArgs) -> anyhow::Result<Pipeline> {
        let project_dir = args.target.dir.clone();
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
                Some(paths::normalize(&cwd.join(given)))
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
        };

        pipeline.dealias_all()?;

        Ok(pipeline)
    }

    /// Builds the scopes of every mirrored `.luaurc`, rewrites every covered script from its raw
    /// bytes and prints the table (`init.luau:240-339`).
    fn dealias_all(&mut self) -> anyhow::Result<()> {
        let verbose = log::verbose() > 0;
        let tree = self.tree();
        let mut luaurcs: Vec<PathBuf> = self.mounts.luaurcs.iter().map(|r| tree.join(r)).collect();
        scopes::order(&mut luaurcs);

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
        let file_name = self.args.target.file_name.as_deref();

        // the scope project is looked up in the project, which holds files the mirror does not
        let lookup = |luaurc: &Path| -> Option<PathBuf> {
            let dir = luaurc.parent()?.strip_prefix(&tree).ok()?;
            let preferred = file_name.filter(|_| dir.as_os_str().is_empty());
            let found = project::find_for_luaurc(&project_dir.join(dir), preferred)?;
            Some(tree.join(found.strip_prefix(project_dir).ok()?))
        };

        let set = ScopeSet::build(&tree, &luaurcs, &lookup, &self.args.opts, &self.rojo);
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

    /// One `rojo build` of the mirror into `out/build.<ext of OUTPUT>`, swapped into place, then
    /// the marker. In plugin mode Rojo writes the plugin itself.
    pub fn build(&mut self) -> anyhow::Result<()> {
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
}

/// A one-shot build: start, build once, and remove the workspace.
pub fn run_once(args: &BuildArgs) -> anyhow::Result<()> {
    let mut pipeline = Pipeline::start(args)?;
    pipeline.build()
}
