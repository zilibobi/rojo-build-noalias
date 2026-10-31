use crate::paths::{self, SKIP_NAMES};
use crate::project::{self, Mounts};
use crate::workspace::{self, Meta, Snapshot};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::Metadata;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// What a batch of file events named: absolute paths under the project directory, or `full` when
/// events may have been lost (a rescan flag, a watcher error, a failed batch). Hints, never truth:
/// the diff stats every path itself.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Hints {
    pub paths: BTreeSet<PathBuf>,
    pub full: bool,
}

/// How the mounted files differ from the snapshot, relative to the project directory. A path whose
/// kind changed (a file that is now a directory) is both removed and added; a removed directory
/// lists everything the snapshot held below it. `config` is set when a `.luaurc` or a project file
/// is among them, so the mount closure and the scopes are computed again.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ChangeSet {
    pub added: BTreeSet<PathBuf>,
    pub removed: BTreeSet<PathBuf>,
    pub changed: BTreeSet<PathBuf>,
    pub config: bool,
}

impl ChangeSet {
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty() && self.changed.is_empty() && !self.config
    }
}

/// Spec 7.2's diff. With `full`, every root is walked and compared with the whole snapshot.
/// Otherwise each hinted path is classified by a stat: gone (it and its snapshot subtree are
/// removed), a directory (its immediate children are compared with the snapshot, and any new
/// subdirectory is walked in full) or a file. A script is changed when its bytes differ from
/// `raw`; any other file when its length, mtime or content hash differs. A hint the build does not
/// read is dropped, unless it is a directory above a mount, whose mounts are then classified.
pub fn diff(
    project_dir: &Path,
    mounts: &Mounts,
    snapshot: &Snapshot,
    raw: &BTreeMap<PathBuf, Vec<u8>>,
    hints: &Hints,
) -> anyhow::Result<ChangeSet> {
    let dir = paths::normalize(project_dir);
    let mut d = Differ {
        dir: &dir,
        mounts,
        snapshot,
        raw,
        out: ChangeSet::default(),
    };

    if hints.full {
        d.full();
    } else {
        for path in &hints.paths {
            let path = paths::normalize(path);

            if let Ok(rel) = path.strip_prefix(&dir) {
                d.hint(rel);
            }
        }
    }

    let mut out = d.out;
    out.config = out
        .added
        .iter()
        .chain(&out.removed)
        .chain(&out.changed)
        .any(|rel| is_config_name(rel) || mounts.projects.contains(rel));

    Ok(out)
}

/// Whether the file name is one that can change the mount closure or a scope's project lookup.
pub fn is_config_name(rel: &Path) -> bool {
    let Some(name) = rel.file_name().and_then(|n| n.to_str()) else {
        return false;
    };

    name == ".luaurc"
        || name == "project.json"
        || name == "project.jsonc"
        || name.ends_with(".project.json")
        || name.ends_with(".project.jsonc")
}

/// Whether `rel` is a proper ancestor of a root, a project file or a `.luaurc` (`""` is the
/// project directory).
pub fn above_mount(mounts: &Mounts, rel: &Path) -> bool {
    mounts
        .roots
        .iter()
        .chain(&mounts.projects)
        .chain(&mounts.luaurcs)
        .any(|m| m != rel && m.starts_with(rel))
}

/// Whether `rel` is spelled by the project file rather than the disk: a mount root, or a
/// directory above a mount. Rojo takes `"$path": "vendor/Shared"` for `vendor/shared` on a
/// case-insensitive filesystem, so such a path is never held to the disk's spelling.
pub fn spelled_by_project(mounts: &Mounts, rel: &Path) -> bool {
    mounts.roots.iter().any(|r| r == rel) || above_mount(mounts, rel)
}

/// Whether the build reads `rel` (relative to `project_dir`): [`Mounts::contains`] (with the meta
/// file counted for a root that is a file now, not when the closure was made), or a file that
/// would join the closure: a `.luaurc` above a root, or the project file a mounted `.luaurc`'s
/// lookup would now pick.
pub fn wanted(project_dir: &Path, mounts: &Mounts, rel: &Path) -> bool {
    let under_root = mounts.roots.iter().any(|root| {
        rel.strip_prefix(root)
            .is_ok_and(|rest| !paths::skipped(rest))
    });

    if under_root || mounts.projects.contains(rel) || mounts.luaurcs.contains(rel) {
        return true;
    }

    let name = rel.file_name().unwrap_or_default().to_string_lossy();

    if name.ends_with(".meta.json") {
        let meta_of_file_root = mounts
            .roots
            .iter()
            .any(|r| project::meta_of(r) == rel && project_dir.join(r).is_file());

        if meta_of_file_root {
            return true;
        }
    }

    let Some(parent) = rel.parent() else {
        return false;
    };

    if !is_config_name(rel) || paths::skipped(rel) {
        return false;
    }

    if name == ".luaurc" {
        // the closure takes every .luaurc above a root
        return mounts
            .roots
            .iter()
            .any(|r| r != parent && r.starts_with(parent));
    }

    // a project file matters where the lookup of a mounted .luaurc would pick it; in the project
    // directory the CLI's project always wins
    !parent.as_os_str().is_empty()
        && mounts.luaurcs.contains(&parent.join(".luaurc"))
        && project::find_for_luaurc(&project_dir.join(parent), None) == Some(project_dir.join(rel))
}

struct Differ<'a> {
    dir: &'a Path,
    mounts: &'a Mounts,
    snapshot: &'a Snapshot,
    raw: &'a BTreeMap<PathBuf, Vec<u8>>,
    out: ChangeSet,
}

impl Differ<'_> {
    fn wanted(&self, rel: &Path) -> bool {
        wanted(self.dir, self.mounts, rel)
    }

    fn known(&self, rel: &Path) -> Option<&Meta> {
        self.snapshot.entries.get(rel)
    }

    /// The snapshot's entries at `rel` and below it.
    fn subtree(&self, rel: &Path) -> impl Iterator<Item = &PathBuf> {
        self.snapshot
            .entries
            .range(rel.to_path_buf()..)
            .map(|(k, _)| k)
            .take_while(move |k| k.starts_with(rel))
    }

    fn hint(&mut self, rel: &Path) {
        if self.wanted(rel) || self.known(rel).is_some() {
            self.classify(rel);

            // a root that changed kind gains or loses the meta file beside it
            if self.mounts.roots.contains(rel) {
                self.classify(&project::meta_of(rel));
            }

            return;
        }

        if !above_mount(self.mounts, rel) {
            return;
        }

        // a directory above a mount, renamed or recreated: its mounts may have moved with it
        let mounts = self.mounts;
        let under: Vec<PathBuf> = mounts
            .roots
            .iter()
            .chain(&mounts.projects)
            .chain(&mounts.luaurcs)
            .filter(|m| m.starts_with(rel))
            .cloned()
            .collect();

        for m in under {
            self.classify(&m);

            if mounts.roots.contains(&m) {
                self.classify(&project::meta_of(&m));
            }
        }

        for name in self.config_files_in(rel) {
            self.classify(&name);
        }
    }

    /// The `.luaurc` and project files directly in the directory `rel` that the build would read.
    fn config_files_in(&self, rel: &Path) -> Vec<PathBuf> {
        let Ok(entries) = std::fs::read_dir(self.dir.join(rel)) else {
            return Vec::new();
        };

        entries
            .flatten()
            .map(|e| rel.join(e.file_name()))
            .filter(|r| is_config_name(r) && self.wanted(r))
            .collect()
    }

    /// Compares one path with the snapshot, as a hint names it.
    fn classify(&mut self, rel: &Path) {
        let stat = std::fs::metadata(self.dir.join(rel));

        match stat {
            // another spelling of a case-only rename: this one is gone, and the directory's
            // listing names the one that is there now
            Ok(_)
                if !spelled_by_project(self.mounts, rel)
                    && !paths::named_exactly(&self.dir.join(rel)) =>
            {
                self.remove(rel);

                if let Some(parent) = rel.parent()
                    && self.known(parent).is_some_and(|m| m.dir)
                {
                    self.children(parent);
                }
            }
            Ok(meta) if self.wanted(rel) => {
                let was_dir = self.known(rel).map(|m| m.dir);
                self.visit(rel, &meta);

                // a directory that was one before: compare its children
                if meta.is_dir() && was_dir == Some(true) {
                    self.children(rel);
                }
            }
            _ => self.remove(rel),
        }
    }

    /// Records `rel` against the snapshot given its current metadata: added (with everything
    /// below it when it is a new directory), removed and added when its kind changed, or changed.
    fn visit(&mut self, rel: &Path, meta: &Metadata) {
        let Some(old) = self.known(rel).copied() else {
            self.add(rel, meta);
            return;
        };

        if old.dir != meta.is_dir() {
            self.remove(rel);
            self.add(rel, meta);
            return;
        }

        if !old.dir && self.file_changed(rel, &old, meta) {
            self.out.changed.insert(rel.to_path_buf());
        }
    }

    fn file_changed(&self, rel: &Path, old: &Meta, meta: &Metadata) -> bool {
        let path = self.dir.join(rel);

        if paths::is_script(rel) {
            // bytes only: mtime plays no part, so a second save in the same tick is seen
            return match std::fs::read(&path) {
                Ok(bytes) => self.raw.get(rel) != Some(&bytes),
                Err(_) => true,
            };
        }

        let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);

        if meta.len() != old.len || mtime != old.mtime {
            return true;
        }

        // only a file mirrored within its mtime resolution can change without a newer mtime
        let Some(hash) = old.hash else {
            return false;
        };

        std::fs::read(&path).map_or(true, |bytes| workspace::content_hash(&bytes) != hash)
    }

    /// `rel` is new: it, every directory above it the snapshot lacks, and when it is a directory,
    /// everything below it.
    fn add(&mut self, rel: &Path, meta: &Metadata) {
        let mut parent = rel.parent();

        while let Some(p) = parent {
            if p.as_os_str().is_empty() || !self.wanted(p) {
                break;
            }

            match self.known(p) {
                Some(m) if m.dir => break,
                Some(_) => self.remove(p),
                None => {}
            }

            self.out.added.insert(p.to_path_buf());
            parent = p.parent();
        }

        self.out.added.insert(rel.to_path_buf());

        if meta.is_dir() {
            self.walk(rel);
        }
    }

    /// Everything below the new directory `rel`, with the mirror's rules.
    fn walk(&mut self, rel: &Path) {
        let mut now = BTreeMap::new();
        self.scan(rel, &mut now);

        for (child, meta) in now {
            if child == rel {
                continue;
            }

            match self.known(&child).copied() {
                Some(old) if old.dir == meta.is_dir() => {
                    if !old.dir && self.file_changed(&child, &old, &meta) {
                        self.out.changed.insert(child);
                    }
                }
                Some(_) => {
                    self.remove(&child);
                    self.out.added.insert(child);
                }
                None => {
                    self.out.added.insert(child);
                }
            }
        }
    }

    /// The immediate children of the directory `rel` against the snapshot's: new ones are added
    /// (and walked), gone ones removed, and one whose kind changed is both.
    fn children(&mut self, rel: &Path) {
        let mut seen = BTreeSet::new();

        if let Ok(entries) = std::fs::read_dir(self.dir.join(rel)) {
            for entry in entries.flatten() {
                let child = rel.join(entry.file_name());

                if !self.wanted(&child) {
                    continue;
                }

                let Ok(meta) = std::fs::metadata(entry.path()) else {
                    continue;
                };

                seen.insert(child.clone());

                match self.known(&child).copied() {
                    None => self.add(&child, &meta),
                    Some(old) if old.dir != meta.is_dir() => {
                        self.remove(&child);
                        self.add(&child, &meta);
                    }
                    Some(_) => {}
                }
            }
        }

        let gone: Vec<PathBuf> = self
            .subtree(rel)
            .filter(|k| k.parent() == Some(rel) && !seen.contains(*k))
            .cloned()
            .collect();

        for child in gone {
            self.remove(&child);
        }
    }

    /// `rel` and everything the snapshot holds below it are gone.
    fn remove(&mut self, rel: &Path) {
        let gone: Vec<PathBuf> = self.subtree(rel).cloned().collect();
        self.out.removed.extend(gone);
    }

    /// Every path the build reads now, compared with the whole snapshot.
    fn full(&mut self) {
        let mut now: BTreeMap<PathBuf, Metadata> = BTreeMap::new();
        let mounts = self.mounts;

        for root in &mounts.roots {
            self.scan(root, &mut now);
            self.scan(&project::meta_of(root), &mut now);
        }

        for single in mounts.projects.iter().chain(&mounts.luaurcs) {
            self.scan(single, &mut now);
        }

        let mut above: BTreeSet<PathBuf> = BTreeSet::new();

        for m in mounts
            .roots
            .iter()
            .chain(&mounts.projects)
            .chain(&mounts.luaurcs)
        {
            above.extend(m.ancestors().skip(1).map(Path::to_path_buf));
        }

        for dir in above {
            for name in self.config_files_in(&dir) {
                self.scan(&name, &mut now);
            }
        }

        let gone: Vec<PathBuf> = self
            .snapshot
            .entries
            .keys()
            .filter(|k| !now.contains_key(*k))
            .cloned()
            .collect();
        self.out.removed.extend(gone);

        for (rel, meta) in &now {
            match self.known(rel).copied() {
                None => {
                    self.out.added.insert(rel.clone());
                }
                Some(old) if old.dir != meta.is_dir() => {
                    self.out.removed.insert(rel.clone());
                    self.out.added.insert(rel.clone());
                }
                Some(old) => {
                    if !old.dir && self.file_changed(rel, &old, meta) {
                        self.out.changed.insert(rel.clone());
                    }
                }
            }
        }
    }

    /// `rel` and, for a directory, everything below it the build reads, into `now`. Symlinks are
    /// followed, skip names are left out below `rel`, and unreadable entries are skipped, as the
    /// mirror does.
    fn scan(&self, rel: &Path, now: &mut BTreeMap<PathBuf, Metadata>) {
        let from = self.dir.join(rel);

        let walk = walkdir::WalkDir::new(&from)
            .follow_links(true)
            .sort_by_file_name()
            .into_iter()
            .filter_entry(|e| e.depth() == 0 || !SKIP_NAMES.iter().any(|n| e.file_name() == *n));

        for entry in walk.flatten() {
            let Ok(inner) = entry.path().strip_prefix(&from) else {
                continue;
            };

            let path = if inner.as_os_str().is_empty() {
                rel.to_path_buf()
            } else {
                rel.join(inner)
            };

            if !self.wanted(&path) {
                continue;
            }

            if let Ok(meta) = entry.metadata() {
                now.insert(path, meta);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn put(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn set(items: &[&str]) -> BTreeSet<PathBuf> {
        items.iter().map(PathBuf::from).collect()
    }

    type Mirrored = (
        tempfile::TempDir,
        PathBuf,
        Mounts,
        Snapshot,
        BTreeMap<PathBuf, Vec<u8>>,
    );

    /// A project mounting `src` with a `.luaurc`, mirrored into a sibling `tree`.
    fn project() -> Mirrored {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("p");
        put(
            &dir.join("default.project.json"),
            r#"{"name": "X", "tree": {"$path": "src"}}"#,
        );
        put(&dir.join(".luaurc"), r#"{"aliases": {"a": "./src"}}"#);
        put(&dir.join("src/init.luau"), "return 1");
        put(&dir.join("src/lib/a.luau"), "return 2");
        put(&dir.join("src/data.txt"), "abc");

        let mounts = project::mount_closure(&dir, Path::new("default.project.json"), None).unwrap();
        let tree = tmp.path().join("tree");
        let (snap, raw) = workspace::mirror_mounts(&dir, &tree, &mounts).unwrap();

        (tmp, dir, mounts, snap, raw)
    }

    fn hints(dir: &Path, rels: &[&str]) -> Hints {
        Hints {
            paths: rels.iter().map(|r| dir.join(r)).collect(),
            full: false,
        }
    }

    #[test]
    fn a_hinted_directory_compares_its_children_and_walks_new_ones() {
        let (_t, dir, mounts, snap, raw) = project();
        put(&dir.join("src/new/deep/x.luau"), "x");
        put(&dir.join("src/new/node_modules/y.luau"), "y");
        std::fs::remove_dir_all(dir.join("src/lib")).unwrap();
        // an edit inside a hinted directory is not the directory's change
        put(&dir.join("src/init.luau"), "return 3");

        let c = diff(&dir, &mounts, &snap, &raw, &hints(&dir, &["src"])).unwrap();

        assert_eq!(
            c.added,
            set(&["src/new", "src/new/deep", "src/new/deep/x.luau"])
        );
        assert_eq!(c.removed, set(&["src/lib", "src/lib/a.luau"]));
        assert!(c.changed.is_empty());
        assert!(!c.config);
    }

    #[test]
    fn a_file_with_parents_missing_from_the_snapshot_brings_them() {
        let (_t, dir, mounts, snap, raw) = project();
        put(&dir.join("src/n/m/x.luau"), "x");

        let c = diff(
            &dir,
            &mounts,
            &snap,
            &raw,
            &hints(&dir, &["src/n/m/x.luau"]),
        )
        .unwrap();

        assert_eq!(c.added, set(&["src/n", "src/n/m", "src/n/m/x.luau"]));
    }

    #[test]
    fn non_script_content_is_compared_whatever_the_mtime() {
        let (_t, dir, mounts, snap, raw) = project();
        let path = dir.join("src/data.txt");
        let mtime = std::fs::metadata(&path).unwrap().modified().unwrap();
        std::fs::write(&path, "abd").unwrap();
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(mtime)
            .unwrap();

        let c = diff(&dir, &mounts, &snap, &raw, &hints(&dir, &["src/data.txt"])).unwrap();
        assert_eq!(c.changed, set(&["src/data.txt"]));
    }

    #[test]
    fn skip_names_outside_paths_and_unchanged_files_give_nothing() {
        let (_t, dir, mounts, snap, raw) = project();
        put(&dir.join("src/.git/HEAD"), "x");
        put(&dir.join("other/x.luau"), "x");
        put(&dir.join("other/.luaurc"), "{}");
        std::fs::write(dir.join("src/init.luau"), "return 1").unwrap();

        let rels = [
            "src/.git/HEAD",
            "src/.git",
            "other/x.luau",
            "other/.luaurc",
            "src/init.luau",
        ];
        let c = diff(&dir, &mounts, &snap, &raw, &hints(&dir, &rels)).unwrap();
        assert!(c.is_empty(), "{c:?}");
    }

    #[test]
    fn config_files_that_can_change_the_build_are_seen() {
        let (_t, dir, mounts, snap, raw) = project();
        put(&dir.join(".luaurc"), r#"{"aliases": {"b": "./src"}}"#);
        // the CLI project wins the root dir, so another project file there changes nothing
        put(&dir.join("dev.project.json"), "{}");

        let rels = ["dev.project.json", ".luaurc"];
        let c = diff(&dir, &mounts, &snap, &raw, &hints(&dir, &rels)).unwrap();
        assert!(c.added.is_empty(), "{c:?}");
        assert_eq!(c.changed, set(&[".luaurc"]));
        assert!(c.config);
    }

    #[test]
    fn a_project_file_counts_where_it_becomes_a_scope_project() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("p");
        put(
            &dir.join("default.project.json"),
            r#"{"name": "X", "tree": {"$path": "src", "s": {"$path": "vendor/shared"}}}"#,
        );
        put(&dir.join("src/init.luau"), "");
        put(&dir.join("vendor/shared/x.luau"), "");
        put(
            &dir.join("vendor/.luaurc"),
            r#"{"aliases": {"a": "./shared"}}"#,
        );
        let mounts = project::mount_closure(&dir, Path::new("default.project.json"), None).unwrap();
        let tree = tmp.path().join("tree");
        let (snap, raw) = workspace::mirror_mounts(&dir, &tree, &mounts).unwrap();

        // an orphan scope until a project file appears beside its .luaurc
        put(&dir.join("vendor/b.project.json"), "{}");
        put(&dir.join("vendor/.luaurc.bak"), "{}");
        put(&dir.join("other/.luaurc"), "{}");
        let rels = [
            "vendor/b.project.json",
            "vendor/.luaurc.bak",
            "other/.luaurc",
        ];
        let c = diff(&dir, &mounts, &snap, &raw, &hints(&dir, &rels)).unwrap();
        assert_eq!(c.added, set(&["vendor/b.project.json"]));
        assert!(c.config);

        // a project file the lookup ranks below the one found first is not
        put(&dir.join("vendor/a.project.json"), "{}");
        let mounts = project::mount_closure(&dir, Path::new("default.project.json"), None).unwrap();
        let (snap, raw) = workspace::mirror_mounts(&dir, &tree, &mounts).unwrap();
        let c = diff(&dir, &mounts, &snap, &raw, &hints(&dir, &rels)).unwrap();
        assert!(c.is_empty(), "{c:?}");

        // a .luaurc above a root is read by the closure
        put(&dir.join("vendor/x/.luaurc"), "{}");
        put(&dir.join("src/.luaurc"), "{}");
        let rels = ["vendor/x/.luaurc", "src/.luaurc"];
        let c = diff(&dir, &mounts, &snap, &raw, &hints(&dir, &rels)).unwrap();
        assert_eq!(c.added, set(&["src/.luaurc"]));
    }

    #[test]
    fn a_renamed_ancestor_reclassifies_its_mounts() {
        let (_t, dir, mounts, snap, raw) = project();
        std::fs::rename(dir.join("src"), dir.join("moved")).unwrap();

        let c = diff(&dir, &mounts, &snap, &raw, &hints(&dir, &[""])).unwrap();
        assert!(c.removed.contains(Path::new("src")));
        assert!(c.removed.contains(Path::new("src/lib/a.luau")));
        assert!(!c.removed.contains(Path::new(".luaurc")));
        assert!(c.added.is_empty());
    }

    #[test]
    fn full_matches_the_hinted_result() {
        let (_t, dir, mounts, snap, raw) = project();
        put(&dir.join("src/new/x.luau"), "x");
        put(&dir.join("src/lib/a.luau"), "changed");
        std::fs::remove_file(dir.join("src/data.txt")).unwrap();

        let full = Hints {
            paths: BTreeSet::new(),
            full: true,
        };
        let c = diff(&dir, &mounts, &snap, &raw, &full).unwrap();
        assert_eq!(c.added, set(&["src/new", "src/new/x.luau"]));
        assert_eq!(c.removed, set(&["src/data.txt"]));
        assert_eq!(c.changed, set(&["src/lib/a.luau"]));

        let untouched = diff(&dir, &mounts, &snap, &raw, &Hints::default()).unwrap();
        assert!(untouched.is_empty());
    }

    #[test]
    fn a_file_that_became_a_directory_is_removed_and_added() {
        let (_t, dir, mounts, snap, raw) = project();
        std::fs::remove_file(dir.join("src/data.txt")).unwrap();
        put(&dir.join("src/data.txt/inner.luau"), "x");

        let c = diff(&dir, &mounts, &snap, &raw, &hints(&dir, &["src/data.txt"])).unwrap();
        assert_eq!(c.removed, set(&["src/data.txt"]));
        assert_eq!(c.added, set(&["src/data.txt", "src/data.txt/inner.luau"]));
    }
}
