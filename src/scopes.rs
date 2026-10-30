use crate::cli::Options;
use crate::dealias::{self, OrphanRules, ProjectRules, Replacement};
use crate::rojo::Rojo;
use crate::{log, luaurc, paths, project, sourcemap};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub enum Rules {
    Project(ProjectRules),
    Orphan(OrphanRules),
}

/// One `.luaurc` and what it rewrites.
#[derive(Clone, Debug)]
pub struct Scope {
    pub luaurc: PathBuf,
    pub dir: PathBuf,
    pub project: Option<PathBuf>,
    pub rules: Rules,
    /// project scopes: covered script path to instance path; orphan scopes: empty, they cover
    /// every script under `dir`
    pub files: BTreeMap<PathBuf, Vec<String>>,
}

impl Scope {
    pub fn covers(&self, file: &Path) -> bool {
        match self.rules {
            Rules::Project(_) => self.files.contains_key(file),
            Rules::Orphan(_) => {
                paths::is_script(file)
                    && file
                        .strip_prefix(&self.dir)
                        .is_ok_and(|rel| !paths::skipped(rel))
            }
        }
    }
}

/// Every `.luaurc` under `tree`, skipping [`paths::SKIP_NAMES`] and the `exclude` path.
pub fn discover_all(tree: &Path, exclude: Option<&Path>) -> Vec<PathBuf> {
    walkdir::WalkDir::new(tree)
        .sort_by_file_name()
        .into_iter()
        .filter_entry(|e| {
            let skipped = e.depth() > 0 && paths::SKIP_NAMES.iter().any(|n| e.file_name() == *n);
            !skipped && exclude != Some(e.path())
        })
        .flatten()
        .filter(|e| e.file_type().is_file() && e.file_name() == ".luaurc")
        .map(walkdir::DirEntry::into_path)
        .collect()
}

/// Deepest first (most components), then by path.
pub fn order(luaurcs: &mut [PathBuf]) {
    luaurcs.sort_by(|a, b| {
        let depth = |p: &PathBuf| p.components().count();
        depth(b).cmp(&depth(a)).then_with(|| a.cmp(b))
    });
}

/// The first line of an error, as v1.2.4 printed it after `with error:`.
fn first_line(e: &anyhow::Error) -> String {
    e.root_cause()
        .to_string()
        .lines()
        .next()
        .unwrap_or_default()
        .to_string()
}

/// A scope that cannot be built: an error when strict, else logged by `report` and skipped.
fn skip(strict: bool, e: anyhow::Error, report: impl FnOnce(&anyhow::Error)) -> anyhow::Result<()> {
    if strict {
        return Err(e);
    }

    report(&e);
    Ok(())
}

/// The `.luaurc`'s aliases, `None` when the scope is skipped (`replaceAliases.luau:12-34`).
fn read_aliases(luaurc: &Path, strict: bool) -> anyhow::Result<Option<BTreeMap<String, String>>> {
    let parsed = std::fs::read_to_string(luaurc)
        .map_err(anyhow::Error::from)
        .and_then(|text| luaurc::parse_aliases(&text));

    let aliases = match parsed {
        Ok(aliases) => aliases,
        Err(e) => {
            let e = anyhow::anyhow!(
                "Could not open {} with error: {}",
                luaurc.display(),
                first_line(&e)
            );
            skip(strict, e, |e| log::warn(&e.to_string()))?;
            return Ok(None);
        }
    };

    if aliases.is_empty() {
        if log::verbose() > 0 {
            log::info(&format!(
                "Skipping {} - no aliases defined",
                luaurc.display()
            ));
        }

        return Ok(None);
    }

    Ok(Some(aliases))
}

/// A project scope (`replaceAliases.luau:156-369` up to the rewrite loop).
fn project_scope(
    luaurc: &Path,
    dir: &Path,
    project: PathBuf,
    aliases: &BTreeMap<String, String>,
    rojo: &Rojo,
    strict: bool,
) -> anyhow::Result<Option<Scope>> {
    let value = match project::read(&project) {
        Ok(value) => value,
        Err(e) => {
            let e = anyhow::anyhow!(
                "Could not open {} with error: {}",
                project.display(),
                first_line(&e)
            );
            skip(strict, e, |e| log::error(&e.to_string()))?;
            return Ok(None);
        }
    };

    let node = match rojo.sourcemap(&project) {
        Ok(node) => node,
        Err(e) => {
            skip(strict, e, |e| {
                let text = format!("{e:#}");
                let (head, rest) = text.split_once('\n').unwrap_or((&text, ""));
                log::error(head);

                if !rest.is_empty() {
                    log::error(rest);
                }
            })?;
            return Ok(None);
        }
    };

    let project_dir = project.parent().unwrap_or(dir);
    let root = project::root_path(&value).map(|p| project_dir.join(p));
    let index = sourcemap::index(&node, root.as_deref(), &|p: &Path| p.is_file());
    let targets = dealias::resolve_aliases(aliases, project_dir, &index, luaurc);

    if targets.is_empty() {
        log::warn(&format!("No valid aliases found for {}", project.display()));
        return Ok(None);
    }

    let mut files = BTreeMap::new();

    for (path, instance) in &index.paths {
        if !paths::is_script(path) {
            continue;
        }

        if !path.is_file() {
            log::warn(&format!(
                "File in sourcemap doesn't exist: {}",
                path.display()
            ));
            continue;
        }

        files.insert(path.clone(), instance.clone());
    }

    Ok(Some(Scope {
        luaurc: luaurc.to_path_buf(),
        dir: dir.to_path_buf(),
        project: Some(project),
        rules: Rules::Project(ProjectRules::new(index.root_name, targets)),
        files,
    }))
}

/// One `.luaurc`'s scope, `None` when it is skipped (`init.luau:262-296`).
fn build_one(
    luaurc: &Path,
    lookup: &dyn Fn(&Path) -> Option<PathBuf>,
    opts: &Options,
    rojo: &Rojo,
    strict: bool,
) -> anyhow::Result<Option<Scope>> {
    let verbose = log::verbose() > 0;
    let dir = luaurc.parent().unwrap_or(Path::new(""));
    let project = lookup(luaurc);

    if project.is_none() {
        if !opts.process_orphans {
            if verbose {
                log::warn(&format!(
                    "No project file found for {}. Skipping...",
                    luaurc.display()
                ));
            }

            return Ok(None);
        }

        if verbose {
            log::info(&format!(
                "No project file found for {}. Using direct replacement mode...",
                luaurc.display()
            ));
        }
    }

    let Some(aliases) = read_aliases(luaurc, strict)? else {
        return Ok(None);
    };

    let Some(project) = project else {
        return Ok(Some(Scope {
            luaurc: luaurc.to_path_buf(),
            dir: dir.to_path_buf(),
            project: None,
            rules: Rules::Orphan(OrphanRules::new(aliases)),
            files: BTreeMap::new(),
        }));
    };

    project_scope(luaurc, dir, project, &aliases, rojo, strict)
}

fn build_with(
    root: &Path,
    luaurcs: &[PathBuf],
    lookup: &dyn Fn(&Path) -> Option<PathBuf>,
    opts: &Options,
    rojo: &Rojo,
    strict: bool,
) -> anyhow::Result<ScopeSet> {
    let mut scopes = Vec::new();

    for luaurc in luaurcs {
        if let Some(scope) = build_one(luaurc, lookup, opts, rojo, strict)? {
            scopes.push(scope);
        }
    }

    Ok(ScopeSet {
        root: root.to_path_buf(),
        scopes,
        wax: opts.wax_compat,
    })
}

/// The scopes of one tree in chain order: a file is rewritten by every scope that covers it,
/// deepest first, each one's output the next one's input.
#[derive(Clone, Debug)]
pub struct ScopeSet {
    /// the tree the scopes live in; orphan rows are labeled relative to it
    pub root: PathBuf,
    pub scopes: Vec<Scope>,
    pub wax: bool,
}

impl ScopeSet {
    /// Builds a scope per `.luaurc`, in the order given. Never fails: a scope that cannot be
    /// built is skipped with v1.2.4's message (spec 6.1, 6.2).
    pub fn build(
        root: &Path,
        luaurcs: &[PathBuf],
        lookup: &dyn Fn(&Path) -> Option<PathBuf>,
        opts: &Options,
        rojo: &Rojo,
    ) -> ScopeSet {
        match build_with(root, luaurcs, lookup, opts, rojo, false) {
            Ok(set) => set,
            Err(_) => unreachable!("a lenient build never fails"),
        }
    }

    /// As [`ScopeSet::build`], but a `.luaurc` or project file that does not parse, or a failed
    /// sourcemap, is an error instead of a skipped scope (watch mode keeps the last good scopes).
    pub fn build_strict(
        root: &Path,
        luaurcs: &[PathBuf],
        lookup: &dyn Fn(&Path) -> Option<PathBuf>,
        opts: &Options,
        rojo: &Rojo,
    ) -> anyhow::Result<ScopeSet> {
        build_with(root, luaurcs, lookup, opts, rojo, true)
    }

    pub fn covers(&self, file: &Path) -> bool {
        self.scopes.iter().any(|s| s.covers(file))
    }

    /// `src` rewritten by every scope covering `file`, in chain order, with each row tagged by
    /// the index of the scope that made it.
    pub fn rewrite(&self, file: &Path, src: &[u8]) -> (Vec<u8>, Vec<(usize, Replacement)>) {
        let mut text = src.to_vec();
        let mut rows = Vec::new();

        for (i, scope) in self.scopes.iter().enumerate() {
            if !scope.covers(file) {
                continue;
            }

            let (out, made) = match &scope.rules {
                Rules::Project(rules) => {
                    let instance = &scope.files[file];
                    let context = dealias::context_path(file, instance, self.wax);
                    rules.rewrite(&text, &context, &instance.join("/"))
                }
                Rules::Orphan(rules) => {
                    let rel = file.strip_prefix(&self.root).unwrap_or(file);
                    rules.rewrite(&text, &paths::slash(rel))
                }
            };

            text = out;
            rows.extend(made.into_iter().map(|r| (i, r)));
        }

        (text, rows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::ColorChoice;

    fn opts(process_orphans: bool) -> Options {
        Options {
            process_orphans,
            wax_compat: false,
            verbose: 0,
            color: ColorChoice::Never,
        }
    }

    fn rojo(cwd: &Path) -> Rojo {
        Rojo {
            cwd: cwd.to_path_buf(),
            color: ColorChoice::Never,
            verbose: 0,
        }
    }

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn no_project(_: &Path) -> Option<PathBuf> {
        None
    }

    #[test]
    fn order_is_deepest_first_then_by_path() {
        let mut list: Vec<PathBuf> = [
            "/p/.luaurc",
            "/p/src/b/.luaurc",
            "/p/pkg/x/y/.luaurc",
            "/p/src/a/.luaurc",
            "/p/z/.luaurc",
        ]
        .iter()
        .map(PathBuf::from)
        .collect();

        order(&mut list);

        let got: Vec<&str> = list.iter().map(|p| p.to_str().unwrap()).collect();
        assert_eq!(
            got,
            [
                "/p/pkg/x/y/.luaurc",
                "/p/src/a/.luaurc",
                "/p/src/b/.luaurc",
                "/p/z/.luaurc",
                "/p/.luaurc",
            ]
        );
    }

    #[test]
    fn discover_skips_names_and_the_excluded_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let t = tmp.path();
        for rel in [
            ".luaurc",
            "src/worker/.luaurc",
            "node_modules/x/.luaurc",
            "pkg/.git/.luaurc",
            "out/.luaurc",
        ] {
            write(&t.join(rel), "{}");
        }
        std::fs::create_dir_all(t.join("dir.luaurc/.luaurc")).unwrap();

        let mut found = discover_all(t, Some(&t.join("out")));
        found.sort();
        assert_eq!(found, vec![t.join(".luaurc"), t.join("src/worker/.luaurc")]);
    }

    #[test]
    fn orphan_chain_runs_deepest_first_and_labels_relative_paths() {
        let tmp = tempfile::tempdir().unwrap();
        let t = tmp.path();
        write(
            &t.join(".luaurc"),
            r#"{"aliases": {"a": "./root-a", "b": "./b"}}"#,
        );
        write(&t.join("src/.luaurc"), r#"{"aliases": {"a": "./inner-a"}}"#);
        write(&t.join("src/x.luau"), "");

        let mut luaurcs = discover_all(t, None);
        order(&mut luaurcs);
        let set = ScopeSet::build(t, &luaurcs, &no_project, &opts(true), &rojo(t));

        assert_eq!(set.scopes.len(), 2);
        assert_eq!(set.scopes[0].dir, t.join("src"));
        assert!(set.covers(&t.join("src/x.luau")));
        assert!(set.covers(&t.join("y.lua")));
        assert!(!set.covers(&t.join("src/x.json")));
        assert!(!set.covers(&t.join("node_modules/m.luau")));
        assert!(!set.scopes[0].covers(&t.join("y.lua")));

        let src = br#"require("@a/m") require('@b')"#;
        let (out, rows) = set.rewrite(&t.join("src/x.luau"), src);
        assert_eq!(out, br#"require("./inner-a/m") require("./b")"#);

        let tags: Vec<(usize, &str, &str)> = rows
            .iter()
            .map(|(i, r)| (*i, r.label.as_str(), r.from.as_str()))
            .collect();
        assert_eq!(
            tags,
            [
                (0, "src/x.luau", r#"require("@a/m")"#),
                (1, "src/x.luau", r#"require("@b")"#),
            ]
        );

        let (out, rows) = set.rewrite(&t.join("src/x.json"), src);
        assert_eq!(out, src);
        assert!(rows.is_empty());
    }

    #[test]
    fn orphans_are_skipped_without_the_flag() {
        let tmp = tempfile::tempdir().unwrap();
        let t = tmp.path();
        write(&t.join(".luaurc"), r#"{"aliases": {"a": "./a"}}"#);

        let set = ScopeSet::build(t, &[t.join(".luaurc")], &no_project, &opts(false), &rojo(t));
        assert!(set.scopes.is_empty());
        assert!(!set.covers(&t.join("x.luau")));
    }

    #[test]
    fn broken_or_empty_luaurc_skips_leniently_and_fails_strictly() {
        let tmp = tempfile::tempdir().unwrap();
        let t = tmp.path();
        write(&t.join("bad/.luaurc"), r#"{"aliases": {"a": "./a"},}"#);
        write(&t.join("empty/.luaurc"), r#"{"aliases": {}}"#);
        write(&t.join("good/.luaurc"), r#"{"aliases": {"a": "./a"}}"#);
        let mut luaurcs = discover_all(t, None);
        order(&mut luaurcs);

        let set = ScopeSet::build(t, &luaurcs, &no_project, &opts(true), &rojo(t));
        let dirs: Vec<&Path> = set.scopes.iter().map(|s| s.dir.as_path()).collect();
        assert_eq!(dirs, [t.join("good")]);

        let err = ScopeSet::build_strict(t, &luaurcs, &no_project, &opts(true), &rojo(t))
            .unwrap_err()
            .to_string();
        assert!(err.contains("bad"), "{err}");

        let fine = [t.join("empty/.luaurc"), t.join("good/.luaurc")];
        let set = ScopeSet::build_strict(t, &fine, &no_project, &opts(true), &rojo(t)).unwrap();
        assert_eq!(set.scopes.len(), 1);
    }

    #[test]
    fn unparseable_project_skips_leniently_and_fails_strictly() {
        let tmp = tempfile::tempdir().unwrap();
        let t = tmp.path();
        write(&t.join(".luaurc"), r#"{"aliases": {"a": "./a"}}"#);
        write(&t.join("default.project.json"), "{ not json");
        let lookup = |l: &Path| project::find_for_luaurc(l.parent().unwrap(), None);
        let luaurcs = [t.join(".luaurc")];

        let set = ScopeSet::build(t, &luaurcs, &lookup, &opts(false), &rojo(t));
        assert!(set.scopes.is_empty());
        assert!(ScopeSet::build_strict(t, &luaurcs, &lookup, &opts(false), &rojo(t)).is_err());
    }
}
