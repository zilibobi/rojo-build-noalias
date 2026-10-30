use crate::cli::ConvertArgs;
use crate::report::{self, ProjectLog};
use crate::rojo::Rojo;
use crate::scopes::{self, ScopeSet};
use crate::workspace::{self, Workspace};
use crate::{log, paths, project};
use anyhow::Context;
use std::path::{Path, PathBuf};
use std::time::Instant;

/// `--convert-to` (v1.2.4 `init.luau:201-388`): a copy of the project with every alias require
/// rewritten, written to `args.out_dir`.
pub fn run(args: &ConvertArgs) -> anyhow::Result<()> {
    if args.watch_ignored {
        log::warn("--watch is ignored with --convert-to");
    }

    let project_dir = &args.target.dir;
    let out_dir = &args.out_dir;
    let verbose = log::verbose() > 0;

    if !project_dir.is_dir() {
        anyhow::bail!(
            "the project directory {} does not exist",
            project_dir.display()
        );
    }

    // the CLI's check is lexical; a symlinked spelling of DIR must not reach the project either
    let project_real = dunce::canonicalize(project_dir)
        .with_context(|| format!("cannot resolve {}", project_dir.display()))?;
    let out_real = resolve(out_dir)?;

    if project_real.starts_with(&out_real) {
        anyhow::bail!("refusing to convert into the project directory or one of its parents");
    }

    // DIR inside the project is converted in a workspace first and left out of the copy, so a
    // previous output is never converted into the next one (divergence 4)
    let inside = out_real.starts_with(&project_real);

    if out_real.is_dir() {
        std::fs::remove_dir_all(&out_real)
            .with_context(|| format!("cannot remove {}", out_dir.display()))?;
    } else if out_real.exists() {
        anyhow::bail!("{} exists and is not a directory", out_dir.display());
    }

    let ws = if inside {
        Some(Workspace::create()?)
    } else {
        None
    };
    let work = ws
        .as_ref()
        .map_or_else(|| out_real.clone(), Workspace::tree);

    if verbose {
        log::info(&format!(
            "Converting project from {} to {}...",
            project_dir.display(),
            work.display()
        ));
    }

    // DIR as the copy walks it: the project dir as given, then DIR's place inside it
    let rel = out_real.strip_prefix(&project_real).ok().filter(|_| inside);
    let exclude = rel.map(|rel| project_dir.join(rel));
    workspace::copy_tree(project_dir, &work, exclude.as_deref())?;

    let work_exclude = rel.map(|rel| work.join(rel));
    let mut luaurcs = scopes::discover_all(&work, work_exclude.as_deref());
    scopes::order(&mut luaurcs);

    if luaurcs.is_empty() {
        if verbose {
            log::warn("No .luaurc files found. Running standard Rojo build...");
        }
    } else {
        if verbose {
            log::info(&format!(
                "Found {} .luaurc file(s). Processing aliases...\n",
                luaurcs.len()
            ));
        }

        let started = Instant::now();
        let logs = dealias_tree(args, &work, &luaurcs, work_exclude.as_deref())?;
        report::print(&logs, started.elapsed());

        if verbose {
            log::blank();
        }
    }

    if ws.is_some() {
        if verbose {
            log::info(&format!(
                "Copying converted project to {}...",
                out_dir.display()
            ));
        }

        workspace::copy_tree(&work, &out_real, None)?;
    }

    log::success(&format!(
        "Project converted successfully to {}",
        out_dir.display()
    ));

    Ok(())
}

/// `path` with symlinks resolved: its nearest existing ancestor canonicalized, the missing
/// names after it joined back on.
fn resolve(path: &Path) -> anyhow::Result<PathBuf> {
    let mut base = path;
    let mut missing = Vec::new();

    loop {
        if let Ok(real) = dunce::canonicalize(base) {
            return Ok(missing.iter().rev().fold(real, |out, name| out.join(name)));
        }

        let (Some(name), Some(parent)) = (base.file_name(), base.parent()) else {
            anyhow::bail!("cannot resolve {}", path.display());
        };

        missing.push(name);
        base = parent;
    }
}

/// Rewrites every covered script under `work` in place through the scope chain and returns the
/// rows per scope, titled with the original paths.
fn dealias_tree(
    args: &ConvertArgs,
    work: &Path,
    luaurcs: &[PathBuf],
    exclude: Option<&Path>,
) -> anyhow::Result<Vec<ProjectLog>> {
    let project_dir = &args.target.dir;
    let preferred = args.target.file_name.as_deref();

    // the CLI's project file name counts only for the .luaurc in the project root
    let lookup = |luaurc: &Path| {
        let dir = luaurc.parent()?;
        let preferred = if dir == work { preferred } else { None };
        project::find_for_luaurc(dir, preferred)
    };

    let rojo = Rojo {
        cwd: project_dir.clone(),
        color: args.opts.color,
        verbose: args.opts.verbose,
    };
    let set = ScopeSet::build(work, luaurcs, &lookup, &args.opts, &rojo);

    let mut rows = vec![Vec::new(); set.scopes.len()];
    let mut checked = vec![0usize; set.scopes.len()];

    let scripts = walkdir::WalkDir::new(work)
        .sort_by_file_name()
        .into_iter()
        .filter_entry(|e| exclude != Some(e.path()))
        .flatten()
        .filter(|e| e.file_type().is_file() && paths::is_script(e.path()));

    for entry in scripts {
        let file = entry.path();

        for (i, scope) in set.scopes.iter().enumerate() {
            if scope.covers(file) {
                checked[i] += 1;
            }
        }

        if !set.covers(file) {
            continue;
        }

        let src = std::fs::read(file).with_context(|| format!("cannot read {}", file.display()))?;
        let (out, made) = set.rewrite(file, &src);

        if out != src {
            std::fs::write(file, &out)
                .with_context(|| format!("cannot write {}", file.display()))?;
        }

        for (i, row) in made {
            rows[i].push(row);
        }
    }

    let original = |path: &Path| match path.strip_prefix(work) {
        Ok(rel) => project_dir.join(rel),
        Err(_) => path.to_path_buf(),
    };

    let mut logs = Vec::new();

    for ((scope, rows), checked) in set.scopes.iter().zip(rows).zip(checked) {
        if rows.is_empty() {
            if log::verbose() > 0 {
                match &scope.project {
                    Some(project) => log::info(&format!(
                        "Processed {} - checked {checked} files, no alias requires found",
                        project.display()
                    )),
                    None => log::info(&format!(
                        "Processed {} (orphan mode) - checked {checked} files, no alias requires found",
                        scope.luaurc.display()
                    )),
                }
            }

            continue;
        }

        let title = match &scope.project {
            Some(project) => original(project).display().to_string(),
            None => format!("{} (orphan)", original(&scope.luaurc).display()),
        };

        logs.push(ProjectLog { title, rows });
    }

    Ok(logs)
}
