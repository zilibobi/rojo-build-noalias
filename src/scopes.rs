use crate::cli::Options;
use crate::dealias::{self, OrphanRules, ProjectRules, Replacement};
use crate::rojo::Rojo;
use crate::{log, luaurc, paths, project, sourcemap};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub enum Rules {
    Project(ProjectRules),
    Orphan(OrphanRules),
}

impl Rules {
    /// Whether both rewrite every require the same way: the same resolved table (project) or the
    /// same aliases (orphan).
    pub fn same_table(&self, other: &Rules) -> bool {
        match (self, other) {
            (Rules::Project(a), Rules::Project(b)) => {
                a.root_name == b.root_name && a.targets == b.targets
            }
            (Rules::Orphan(a), Rules::Orphan(b)) => a.aliases == b.aliases,
            _ => false,
        }
    }
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
    maps: &mut Sourcemaps,
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

    let mapped = maps
        .remove(&project)
        .unwrap_or_else(|| rojo.sourcemap(&project));

    let node = match mapped {
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
    project: Option<PathBuf>,
    maps: &mut Sourcemaps,
    opts: &Options,
    rojo: &Rojo,
    strict: bool,
) -> anyhow::Result<Option<Scope>> {
    let verbose = log::verbose() > 0;
    let dir = luaurc.parent().unwrap_or(Path::new(""));

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

    project_scope(luaurc, dir, project, &aliases, maps, rojo, strict)
}

type Sourcemaps = BTreeMap<PathBuf, anyhow::Result<sourcemap::Node>>;

/// Whether the scope of `luaurc` will ask for the sourcemap of `project`: its aliases parse and
/// are not empty, and the project parses. A guess made ahead of time; a miss costs one more run.
fn maps_project(luaurc: &Path, project: &Path) -> bool {
    let aliases = std::fs::read_to_string(luaurc)
        .ok()
        .and_then(|text| luaurc::parse_aliases(&text).ok());

    aliases.is_some_and(|a| !a.is_empty()) && project::read(project).is_ok()
}

/// One `rojo sourcemap` per project, all running at once (spec 7.2).
fn sourcemaps(rojo: &Rojo, projects: &[&PathBuf]) -> Sourcemaps {
    std::thread::scope(|s| {
        let running: Vec<_> = projects
            .iter()
            .map(|p| (*p, s.spawn(|| rojo.sourcemap(p))))
            .collect();

        running
            .into_iter()
            .map(|(p, run)| {
                let node = run
                    .join()
                    .unwrap_or_else(|_| Err(anyhow::anyhow!("rojo sourcemap panicked")));
                (p.clone(), node)
            })
            .collect()
    })
}

/// Builds every scope, mapping the projects in parallel first; messages still print in scope
/// order. `strict` says per `.luaurc` whether a scope that cannot be built is an error.
fn build_with(
    root: &Path,
    luaurcs: &[PathBuf],
    lookup: &dyn Fn(&Path) -> Option<PathBuf>,
    opts: &Options,
    rojo: &Rojo,
    strict: &dyn Fn(&Path) -> bool,
) -> anyhow::Result<ScopeSet> {
    let plans: Vec<(&PathBuf, Option<PathBuf>)> = luaurcs.iter().map(|l| (l, lookup(l))).collect();
    let mapped: BTreeSet<&PathBuf> = plans
        .iter()
        .filter_map(|(l, p)| p.as_ref().filter(|p| maps_project(l, p)))
        .collect();
    let mapped: Vec<&PathBuf> = mapped.into_iter().collect();
    let mut maps = sourcemaps(rojo, &mapped);
    let mut scopes = Vec::new();

    for (luaurc, project) in plans {
        let strict = strict(luaurc);

        if let Some(scope) = build_one(luaurc, project, &mut maps, opts, rojo, strict)? {
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
        match build_with(root, luaurcs, lookup, opts, rojo, &|_| false) {
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
        build_with(root, luaurcs, lookup, opts, rojo, &|_| true)
    }

    /// A rebuild in watch mode: as [`ScopeSet::build_strict`] for every `.luaurc` that has a scope
    /// in `previous`, so a scope that worked fails the batch instead of vanishing, and as
    /// [`ScopeSet::build`] for the rest, which startup already skipped with a warning.
    pub fn rebuild(
        root: &Path,
        luaurcs: &[PathBuf],
        lookup: &dyn Fn(&Path) -> Option<PathBuf>,
        opts: &Options,
        rojo: &Rojo,
        previous: &ScopeSet,
    ) -> anyhow::Result<ScopeSet> {
        let had: BTreeSet<&Path> = previous.scopes.iter().map(|s| s.luaurc.as_path()).collect();
        build_with(root, luaurcs, lookup, opts, rojo, &|l| had.contains(l))
    }

    /// Whether `file` goes through the same scopes, with the same tables and the same instance
    /// path, in both sets, so that rewriting it again would give the same bytes.
    pub fn same_chain(&self, other: &ScopeSet, file: &Path) -> bool {
        let mine = self.chain(file);
        let theirs = other.chain(file);

        self.wax == other.wax
            && mine.len() == theirs.len()
            && mine.iter().zip(&theirs).all(|((x, a), (y, b))| {
                x.luaurc == y.luaurc && x.rules.same_table(&y.rules) && a == b
            })
    }

    /// The scopes covering `file` in order, each with the instance path it gives the file.
    fn chain(&self, file: &Path) -> Vec<(&Scope, Option<&Vec<String>>)> {
        self.scopes
            .iter()
            .filter(|s| s.covers(file))
            .map(|s| (s, s.files.get(file)))
            .collect()
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

    #[test]
    fn rebuild_is_strict_only_for_scopes_that_were_built() {
        let tmp = tempfile::tempdir().unwrap();
        let t = tmp.path();
        write(&t.join("a/.luaurc"), r#"{"aliases": {"x": "./x"}}"#);
        write(&t.join("b/.luaurc"), r#"{"aliases": {"y": "./y"},}"#);
        let luaurcs = [t.join("a/.luaurc"), t.join("b/.luaurc")];
        let o = opts(true);

        let first = ScopeSet::build(t, &luaurcs, &no_project, &o, &rojo(t));
        assert_eq!(first.scopes.len(), 1);

        // b never parsed: still skipped with a warning
        let again = ScopeSet::rebuild(t, &luaurcs, &no_project, &o, &rojo(t), &first).unwrap();
        assert_eq!(again.scopes.len(), 1);

        // a parsed: breaking it is an error naming it
        write(&t.join("a/.luaurc"), "{");
        let err = ScopeSet::rebuild(t, &luaurcs, &no_project, &o, &rojo(t), &first)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains(&t.join("a/.luaurc").display().to_string()),
            "{err}"
        );
    }

    #[test]
    fn same_chain_compares_tables_and_order() {
        let tmp = tempfile::tempdir().unwrap();
        let t = tmp.path();
        write(&t.join(".luaurc"), r#"{"aliases": {"a": "./a"}}"#);
        write(&t.join("src/.luaurc"), r#"{"aliases": {"b": "./b"}}"#);
        let mut luaurcs = discover_all(t, None);
        order(&mut luaurcs);
        let o = opts(true);
        let file = t.join("src/x.luau");
        let other = t.join("y.luau");

        let before = ScopeSet::build(t, &luaurcs, &no_project, &o, &rojo(t));
        let same = ScopeSet::build(t, &luaurcs, &no_project, &o, &rojo(t));
        assert!(before.same_chain(&same, &file));

        write(&t.join("src/.luaurc"), r#"{"aliases": {"b": "./c"}}"#);
        let after = ScopeSet::build(t, &luaurcs, &no_project, &o, &rojo(t));
        assert!(!before.same_chain(&after, &file));
        assert!(before.same_chain(&after, &other));

        let fewer = ScopeSet::build(t, &[t.join(".luaurc")], &no_project, &o, &rojo(t));
        assert!(!before.same_chain(&fewer, &file));
    }
}
