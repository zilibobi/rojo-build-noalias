mod common;

use common::read;
use rojo_build_noalias::cli::{BuildArgs, ColorChoice, Options, OutputTarget, Target};
use rojo_build_noalias::diff::{ChangeSet, Hints};
use rojo_build_noalias::pipeline::{Applied, Pipeline};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::path::{Path, PathBuf};

/// The fixture with its project dir resolved: the pipeline and the watcher's events spell it that
/// way (macOS reports /private/var for a temp dir under /var), so hints built from it are too.
fn fixture(name: &str) -> (tempfile::TempDir, PathBuf) {
    let (guard, dir) = common::fixture(name);
    let dir = dunce::canonicalize(&dir).unwrap();
    (guard, dir)
}

fn args_for(dir: &Path, out: &Path) -> BuildArgs {
    BuildArgs {
        target: Target {
            dir: dir.to_path_buf(),
            file_name: None,
        },
        output: OutputTarget::File(out.to_path_buf()),
        watch: false,
        opts: Options {
            process_orphans: false,
            wax_compat: false,
            verbose: 0,
            color: ColorChoice::Never,
        },
    }
}

struct Run {
    _guard: tempfile::TempDir,
    dir: PathBuf,
    args: BuildArgs,
    p: Pipeline,
}

fn start(name: &str) -> Run {
    let (guard, dir) = fixture(name);
    let args = args_for(&dir, &guard.path().join("out.rbxm"));
    let p = Pipeline::start(&args).unwrap();

    Run {
        _guard: guard,
        dir,
        args,
        p,
    }
}

impl Run {
    fn hints(&self, rels: &[&str]) -> Hints {
        Hints {
            paths: rels.iter().map(|r| self.dir.join(r)).collect(),
            full: false,
        }
    }

    fn diff(&self, rels: &[&str]) -> ChangeSet {
        self.p.diff(&self.hints(rels)).unwrap()
    }

    /// One batch: the diff of the hinted paths, applied.
    fn step(&mut self, rels: &[&str]) -> Applied {
        let changes = self.diff(rels);
        self.p.apply(&changes).unwrap()
    }

    fn try_step(&mut self, rels: &[&str]) -> anyhow::Result<Applied> {
        let changes = self.p.diff(&self.hints(rels))?;
        self.p.apply(&changes)
    }

    fn put(&self, rel: &str, text: &str) {
        let path = self.dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn mirror(&self, rel: &str) -> String {
        read(&self.p.tree().join(rel))
    }

    fn args_output(&self) -> PathBuf {
        match &self.args.output {
            OutputTarget::File(path) => path.clone(),
            OutputTarget::Plugin(_) => unreachable!("the tests build to a file"),
        }
    }

    fn fresh(&self) {
        assert_mirror_equals_fresh(&self.p, &self.args);
    }
}

/// Every entry under `root`, relative, with a file's bytes (`None` for a directory).
fn listing(root: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
    walkdir::WalkDir::new(root)
        .min_depth(1)
        .into_iter()
        .map(|e| {
            let e = e.unwrap();
            let rel = e.path().strip_prefix(root).unwrap().to_path_buf();
            let bytes = e
                .file_type()
                .is_file()
                .then(|| std::fs::read(e.path()).unwrap());
            (rel, bytes)
        })
        .collect()
}

/// The mirror after any number of applies equals the mirror a second `Pipeline::start` makes from
/// the same project: the same relative paths, with the same bytes.
fn assert_mirror_equals_fresh(p: &Pipeline, args: &BuildArgs) {
    let fresh = Pipeline::start(args).unwrap();
    let got = listing(&p.tree());
    let want = listing(&fresh.tree());

    let got_keys: BTreeSet<&PathBuf> = got.keys().collect();
    let want_keys: BTreeSet<&PathBuf> = want.keys().collect();
    assert_eq!(got_keys, want_keys, "the mirror holds other paths");

    for (rel, bytes) in &want {
        assert_eq!(
            got[rel].as_deref().map(String::from_utf8_lossy),
            bytes.as_deref().map(String::from_utf8_lossy),
            "{} differs from a fresh build",
            rel.display()
        );
    }
}

#[test]
fn content_edit_rewrites_only_that_file() {
    let mut r = start("normal-build");
    let helpers = read(&r.dir.join("src/mod/helpers.luau"));
    r.put(
        "src/mod/helpers.luau",
        &format!("{helpers}\nlocal added = 1\n"),
    );

    let applied = r.step(&["src/mod/helpers.luau"]);

    assert_eq!(applied.wrote, 1);
    assert_eq!(applied.removed, 0);
    let mirror = r.mirror("src/mod/helpers.luau");
    assert!(mirror.contains(r#"require("../pkg/Promise")"#), "{mirror}");
    assert!(mirror.contains("local added = 1"), "{mirror}");
    r.fresh();
}

#[test]
fn second_save_with_the_same_length_and_mtime_is_seen() {
    let mut r = start("normal-build");
    let path = r.dir.join("src/mod/helpers.luau");

    r.put("src/mod/helpers.luau", "local a = 1");
    r.step(&["src/mod/helpers.luau"]);
    let first = std::fs::metadata(&path).unwrap().modified().unwrap();

    r.put("src/mod/helpers.luau", "local a = 2");
    File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_modified(first)
        .unwrap();
    assert_eq!(std::fs::metadata(&path).unwrap().modified().unwrap(), first);

    let applied = r.step(&["src/mod/helpers.luau"]);

    assert_eq!(applied.wrote, 1);
    assert_eq!(r.mirror("src/mod/helpers.luau"), "local a = 2");
    r.fresh();
}

#[test]
fn new_folder_with_module_is_mirrored_and_dealiased() {
    let mut r = start("normal-build");
    r.put(
        "src/newdir/thing.luau",
        "local helpers = require(\"@mod/helpers\")\nreturn {}\n",
    );

    let applied = r.step(&["src/newdir"]);

    assert!(applied.dirty());
    let thing = r.mirror("src/newdir/thing.luau");
    assert!(thing.contains(r#"require("../mod/helpers")"#), "{thing}");
    r.fresh();
}

#[test]
fn deleted_file_is_pruned() {
    let mut r = start("normal-build");
    std::fs::remove_file(r.dir.join("pkg/Promise.luau")).unwrap();

    let changes = r.diff(&["pkg/Promise.luau"]);
    assert_eq!(
        changes.removed,
        BTreeSet::from([PathBuf::from("pkg/Promise.luau")])
    );
    let applied = r.p.apply(&changes).unwrap();

    assert_eq!(applied.removed, 1);
    assert!(!r.p.tree().join("pkg/Promise.luau").exists());
    assert!(r.p.tree().join("pkg").is_dir());
    r.fresh();
}

/// The names in the mirror directory `rel`, exactly as it lists them.
fn mirror_names(r: &Run, rel: &str) -> BTreeSet<String> {
    std::fs::read_dir(r.p.tree().join(rel))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect()
}

// on a case-insensitive filesystem (the macOS and Windows CI legs) the old spelling still stats
// after the rename, and the mirror kept it while the sourcemap and the rewrite looked for the new
// one, so the script shipped raw
#[test]
fn case_only_rename_is_mirrored_by_its_new_name() {
    let mut r = start("normal-build");
    r.put("src/mod/Thing.luau", "return require(\"@pkg/Promise\")");
    r.step(&["src/mod/Thing.luau"]);
    assert!(mirror_names(&r, "src/mod").contains("Thing.luau"));

    std::fs::rename(
        r.dir.join("src/mod/Thing.luau"),
        r.dir.join("src/mod/thing.luau"),
    )
    .unwrap();
    // the filesystem is case-insensitive here, so the test cannot pass for want of the bug
    #[cfg(any(target_os = "macos", windows))]
    assert!(r.dir.join("src/mod/Thing.luau").exists());
    r.step(&["src/mod/Thing.luau", "src/mod/thing.luau"]);

    let names = mirror_names(&r, "src/mod");
    assert!(names.contains("thing.luau"), "{names:?}");
    assert!(!names.contains("Thing.luau"), "{names:?}");
    let thing = r.mirror("src/mod/thing.luau");
    assert_eq!(thing, r#"return require("../pkg/Promise")"#);
    r.fresh();

    r.put("src/Newdir/a.luau", "return require(\"@mod/helpers\")");
    r.step(&["src/Newdir"]);
    std::fs::rename(r.dir.join("src/Newdir"), r.dir.join("src/newdir")).unwrap();
    #[cfg(any(target_os = "macos", windows))]
    assert!(r.dir.join("src/Newdir").exists());
    r.step(&["src/Newdir", "src/newdir"]);

    let names = mirror_names(&r, "src");
    assert!(
        names.contains("newdir") && !names.contains("Newdir"),
        "{names:?}"
    );
    let a = r.mirror("src/newdir/a.luau");
    assert_eq!(a, r#"return require("../mod/helpers")"#);
    r.fresh();
}

// a case-insensitive filesystem only: Rojo cannot find vendor/Shared on Linux
#[cfg(any(target_os = "macos", windows))]
#[test]
fn a_mount_spelled_in_another_case_keeps_its_files_on_a_hint() {
    let (guard, dir) = fixture("normal-build");
    std::fs::create_dir_all(dir.join("vendor/shared")).unwrap();
    std::fs::write(
        dir.join("vendor/shared/x.luau"),
        "return require(\"@pkg/Promise\")",
    )
    .unwrap();
    std::fs::write(
        dir.join("default.project.json"),
        r#"{"name": "NormalBuild", "tree": {"$path": "src", "pkg": {"$path": "pkg"}, "vendor": {"$path": "vendor/Shared"}}}"#,
    )
    .unwrap();
    let args = args_for(&dir, &guard.path().join("out.rbxm"));
    let mut r = Run {
        p: Pipeline::start(&args).unwrap(),
        _guard: guard,
        dir,
        args,
    };
    let x = "vendor/Shared/x.luau";
    assert!(r.p.tree().join(x).is_file());

    // the root itself, the directory above it, and the project dir above both
    for hint in ["vendor/Shared", "vendor", ""] {
        let applied = r.step(&[hint]);
        assert_eq!(applied.removed, 0, "a hint on {hint:?} removed the mount");
        assert!(r.p.tree().join(x).is_file(), "a hint on {hint:?} lost {x}");
    }

    assert_eq!(r.mirror(x), r#"return require("../pkg/Promise")"#);
    r.fresh();
}

#[test]
fn renamed_file_and_folder_are_moved() {
    let mut r = start("normal-build");
    std::fs::rename(
        r.dir.join("src/mod/helpers.luau"),
        r.dir.join("src/mod/util.luau"),
    )
    .unwrap();

    let applied = r.step(&["src/mod/helpers.luau", "src/mod/util.luau"]);

    assert_eq!(applied.removed, 1);
    assert!(!r.p.tree().join("src/mod/helpers.luau").exists());
    let util = r.mirror("src/mod/util.luau");
    assert!(util.contains(r#"require("../pkg/Promise")"#), "{util}");
    r.fresh();

    r.put("src/newdir/a.luau", "return require(\"@pkg/Promise\")");
    r.step(&["src/newdir"]);
    r.fresh();

    std::fs::rename(r.dir.join("src/newdir"), r.dir.join("src/newdir2")).unwrap();
    let applied = r.step(&["src/newdir", "src/newdir2"]);

    assert!(applied.removed >= 2, "{}", applied.removed);
    assert!(!r.p.tree().join("src/newdir").exists());
    let a = r.mirror("src/newdir2/a.luau");
    assert!(a.contains(r#"require("../pkg/Promise")"#), "{a}");
    r.fresh();
}

#[test]
fn touch_without_change_writes_nothing() {
    let mut r = start("normal-build");

    for rel in ["src/mod/helpers.luau", "default.project.json", ".luaurc"] {
        let bytes = std::fs::read(r.dir.join(rel)).unwrap();
        std::fs::write(r.dir.join(rel), bytes).unwrap();
    }

    let applied = r.step(&["src/mod/helpers.luau", "default.project.json", ".luaurc"]);

    assert!(!applied.dirty(), "{applied:?}");
    assert!(!applied.rewatch);
    r.fresh();
}

#[test]
fn nested_luaurc_edit_matches_startup() {
    let mut r = start("nested-luaurc");
    let job = read(&r.dir.join("src/worker/job.luau"));
    r.put("src/worker/job.luau", &format!("{job}-- edited\n"));

    let applied = r.step(&["src/worker/job.luau"]);

    assert_eq!(applied.wrote, 1);
    let job = r.mirror("src/worker/job.luau");
    assert!(job.contains(r#"require("../pkg/vfx")"#), "{job}");
    assert!(job.contains("-- edited"), "{job}");
    r.fresh();
}

#[test]
fn luaurc_alias_change_realiases_the_scope() {
    let mut r = start("normal-build");
    r.put("src/other/helpers.luau", "return {}");
    r.put(
        ".luaurc",
        r#"{"aliases": {"mod": "./src/other", "pkg": "./pkg"}}"#,
    );

    let changes = r.diff(&[".luaurc", "src/other"]);
    assert!(changes.config);
    let applied = r.p.apply(&changes).unwrap();

    assert!(applied.dirty());
    let init = r.mirror("src/init.luau");
    assert!(init.contains(r#"require("@self/other/helpers")"#), "{init}");
    r.fresh();
}

#[test]
fn broken_luaurc_keeps_previous_aliases() {
    let mut r = start("normal-build");
    let good = read(&r.dir.join(".luaurc"));
    let scripts = |r: &Run| -> BTreeMap<PathBuf, Option<Vec<u8>>> {
        listing(&r.p.tree())
            .into_iter()
            .filter(|(rel, _)| rel != Path::new(".luaurc"))
            .collect()
    };
    let before = scripts(&r);

    r.put(".luaurc", "{");
    let err = r.try_step(&[".luaurc"]).unwrap_err();
    assert!(format!("{err:#}").contains(".luaurc"), "{err:#}");
    assert_eq!(scripts(&r), before);

    // while it stays broken a script edit fails too, and lands once it parses again
    let helpers = read(&r.dir.join("src/mod/helpers.luau"));
    r.put("src/mod/helpers.luau", &format!("{helpers}-- more\n"));
    assert!(r.try_step(&["src/mod/helpers.luau"]).is_err());

    r.put(".luaurc", &good);
    r.try_step(&[".luaurc"]).unwrap();
    let helpers = r.mirror("src/mod/helpers.luau");
    assert!(helpers.contains("-- more"), "{helpers}");
    assert!(
        helpers.contains(r#"require("../pkg/Promise")"#),
        "{helpers}"
    );
    r.fresh();
}

#[test]
fn project_edit_adding_a_mount_mirrors_it() {
    let mut r = start("normal-build");
    r.put("extra/x.luau", "return require(\"@pkg/Promise\")");
    r.put(
        "default.project.json",
        r#"{"name": "NormalBuild", "tree": {"$path": "src", "pkg": {"$path": "pkg"}, "extra": {"$path": "extra"}}}"#,
    );

    let applied = r.step(&["default.project.json", "extra", "extra/x.luau"]);

    assert!(applied.rewatch);
    assert!(r.p.mounts().roots.contains(Path::new("extra")));
    let x = r.mirror("extra/x.luau");
    assert!(x.contains(r#"require("../pkg/Promise")"#), "{x}");
    r.fresh();
}

#[test]
fn project_edit_dropping_a_mount_prunes_it() {
    let mut r = start("normal-build");
    r.put(
        "default.project.json",
        r#"{"name": "NormalBuild", "tree": {"$path": "src"}}"#,
    );

    let applied = r.step(&["default.project.json"]);

    assert!(applied.rewatch);
    assert!(applied.removed >= 2, "{applied:?}");
    assert!(!r.p.tree().join("pkg").exists());
    r.fresh();
}

#[test]
fn unmounted_paths_are_ignored() {
    let r = start("normal-build");
    r.put("junk/x.luau", "return 1");
    r.put("notes.txt", "x");

    assert!(r.diff(&["junk/x.luau", "junk", "notes.txt"]).is_empty());
    assert!(!r.p.relevant(&r.dir.join("junk/x.luau")));
    r.fresh();
}

#[test]
fn full_rescan_finds_everything() {
    let mut r = start("normal-build");
    r.put("src/mod/helpers.luau", "return require(\"@pkg/Promise\")");
    r.put("src/new.luau", "return require(\"@mod/helpers\")");
    std::fs::remove_file(r.dir.join("pkg/Promise.luau")).unwrap();
    r.put("pkg/Other.luau", "return 2");

    let changes =
        r.p.diff(&Hints {
            paths: BTreeSet::new(),
            full: true,
        })
        .unwrap();
    assert_eq!(
        changes.changed,
        BTreeSet::from([PathBuf::from("src/mod/helpers.luau")])
    );
    assert_eq!(
        changes.added,
        BTreeSet::from([
            PathBuf::from("pkg/Other.luau"),
            PathBuf::from("src/new.luau")
        ])
    );
    assert_eq!(
        changes.removed,
        BTreeSet::from([PathBuf::from("pkg/Promise.luau")])
    );

    let applied = r.p.apply(&changes).unwrap();
    assert!(applied.dirty());
    let new = r.mirror("src/new.luau");
    assert!(new.contains(r#"require("./mod/helpers")"#), "{new}");
    r.fresh();
}

#[test]
fn mount_root_deleted_and_recreated() {
    // a required $path that is gone fails the batch (Rojo cannot build or map it)
    let mut r = start("normal-build");
    let promise = read(&r.dir.join("pkg/Promise.luau"));
    std::fs::remove_dir_all(r.dir.join("pkg")).unwrap();

    let err = r.try_step(&["pkg"]).unwrap_err();
    assert!(format!("{err:#}").contains("sourcemap"), "{err:#}");
    assert!(!r.p.tree().join("pkg").exists());

    r.put("pkg/Promise.luau", &promise);
    let applied = r.try_step(&["pkg"]).unwrap();

    assert!(applied.dirty());
    assert!(applied.rewatch);
    let helpers = r.mirror("src/mod/helpers.luau");
    assert!(
        helpers.contains(r#"require("../pkg/Promise")"#),
        "{helpers}"
    );
    r.fresh();
}

#[test]
fn optional_mount_deleted_leaves_its_alias_raw() {
    let (guard, dir) = fixture("normal-build");
    std::fs::write(
        dir.join("default.project.json"),
        r#"{"name": "NormalBuild", "tree": {"$path": "src", "pkg": {"$path": {"optional": "pkg"}}}}"#,
    )
    .unwrap();
    let args = args_for(&dir, &guard.path().join("out.rbxm"));
    let mut r = Run {
        p: Pipeline::start(&args).unwrap(),
        _guard: guard,
        dir,
        args,
    };
    let promise = read(&r.dir.join("pkg/Promise.luau"));

    std::fs::remove_dir_all(r.dir.join("pkg")).unwrap();
    let applied = r.step(&["pkg"]);

    assert!(applied.dirty());
    assert!(applied.rewatch);
    assert!(!r.p.tree().join("pkg").exists());
    let helpers = r.mirror("src/mod/helpers.luau");
    assert!(helpers.contains(r#"require("@pkg/Promise")"#), "{helpers}");
    r.fresh();

    r.put("pkg/Promise.luau", &promise);
    r.step(&["pkg"]);
    let helpers = r.mirror("src/mod/helpers.luau");
    assert!(
        helpers.contains(r#"require("../pkg/Promise")"#),
        "{helpers}"
    );
    r.fresh();
}

#[test]
fn new_nested_luaurc_in_a_new_folder_is_a_config_change() {
    let mut r = start("normal-build");
    r.put("src/w/.luaurc", r#"{"aliases": {"pkg": "./nowhere"}}"#);
    r.put("src/w/job.luau", "return require(\"@mod/helpers\")");

    let changes = r.diff(&["src/w"]);
    assert!(changes.config);
    r.p.apply(&changes).unwrap();

    assert!(r.p.mounts().luaurcs.contains(Path::new("src/w/.luaurc")));
    let job = r.mirror("src/w/job.luau");
    assert!(job.contains(r#"require("../mod/helpers")"#), "{job}");
    r.fresh();
}

#[test]
fn file_root_that_becomes_a_dir_is_remirrored() {
    let (guard, dir) = fixture("normal-build");
    std::fs::write(
        dir.join("default.project.json"),
        r#"{"name": "NormalBuild", "tree": {"$path": "src", "pkg": {"$path": "pkg"}, "run": {"$path": "lib/run.server.luau"}}}"#,
    )
    .unwrap();
    std::fs::create_dir_all(dir.join("lib")).unwrap();
    std::fs::write(dir.join("lib/run.server.luau"), "require(\"@mod/helpers\")").unwrap();
    std::fs::write(dir.join("lib/run.meta.json"), r#"{"properties": {}}"#).unwrap();
    let args = args_for(&dir, &guard.path().join("out.rbxm"));
    let mut r = Run {
        p: Pipeline::start(&args).unwrap(),
        _guard: guard,
        dir,
        args,
    };
    assert!(
        r.p.mounts()
            .files
            .contains(Path::new("lib/run.server.luau"))
    );

    std::fs::remove_file(r.dir.join("lib/run.server.luau")).unwrap();
    r.put("lib/run.server.luau/init.luau", "return 1");
    let applied = r.step(&["lib/run.server.luau"]);

    assert!(applied.rewatch);
    assert!(
        !r.p.mounts()
            .files
            .contains(Path::new("lib/run.server.luau"))
    );
    assert!(!r.p.tree().join("lib/run.meta.json").exists());
    r.fresh();
}

#[test]
fn relevant_drops_the_output_and_temp_files() {
    let r = start("normal-build");
    let out = r.p.output().unwrap().to_path_buf();

    assert!(r.p.relevant(&r.dir.join("src/mod/helpers.luau")));
    assert!(r.p.relevant(&r.dir.join("src/brand/new.luau")));
    assert!(r.p.relevant(&r.dir.join("default.project.json")));
    assert!(r.p.relevant(&r.dir.join(".luaurc")));
    assert!(!r.p.relevant(&r.dir.join("other.project.json")));
    assert!(r.p.relevant(&r.dir.join("src/w/default.project.json")));
    assert!(r.p.relevant(&r.dir.join("src")));
    assert!(!r.p.relevant(&r.dir.join("src/.git/HEAD")));
    assert!(!r.p.relevant(&r.dir.join("src/node_modules/x.luau")));
    assert!(!r.p.relevant(&r.dir.join("src/.rbn-abc123")));
    assert!(!r.p.relevant(&r.dir.join("rokit.toml")));
    assert!(!r.p.relevant(&r.dir.join("junk/.luaurc")));
    assert!(!r.p.relevant(&out));
    assert!(!r.p.relevant(Path::new("/elsewhere/x.luau")));
}

#[cfg(unix)]
#[test]
fn relevant_drops_a_symlinked_outputs_target() {
    let (guard, dir) = fixture("normal-build");
    let target = dir.join("src/real.rbxm");
    let link = guard.path().join("out.rbxm");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    let args = args_for(&dir, &link);
    let p = Pipeline::start(&args).unwrap();

    assert!(!p.relevant(&link));
    assert!(!p.relevant(&target));
    assert!(p.relevant(&dir.join("src/other.rbxm")));
}

#[test]
fn a_new_root_around_a_mounted_one_keeps_it_dealiased() {
    let (guard, dir) = fixture("normal-build");
    std::fs::write(
        dir.join("default.project.json"),
        r#"{"name": "NormalBuild", "tree": {"$path": "src", "pkg": {"$path": "pkg"}, "part": {"$path": "lib/part"}}}"#,
    )
    .unwrap();
    std::fs::create_dir_all(dir.join("lib/part")).unwrap();
    std::fs::write(
        dir.join("lib/part/x.luau"),
        "return require(\"@pkg/Promise\")",
    )
    .unwrap();
    let args = args_for(&dir, &guard.path().join("out.rbxm"));
    let mut r = Run {
        p: Pipeline::start(&args).unwrap(),
        _guard: guard,
        dir,
        args,
    };
    assert!(r.mirror("lib/part/x.luau").contains("../pkg/Promise"));

    r.put(
        "default.project.json",
        r#"{"name": "NormalBuild", "tree": {"$path": "src", "pkg": {"$path": "pkg"}, "part": {"$path": "lib/part"}, "all": {"$path": "lib"}}}"#,
    );
    r.step(&["default.project.json"]);

    let x = r.mirror("lib/part/x.luau");
    assert!(!x.contains("@pkg"), "{x}");
    r.fresh();
}

#[test]
fn after_a_failed_apply_the_next_diff_is_a_full_rescan() {
    let mut r = start("normal-build");
    let good = read(&r.dir.join(".luaurc"));
    r.put(".luaurc", "{");
    assert!(r.try_step(&[".luaurc"]).is_err());
    assert!(r.diff(&[]).config);

    // the fix arrives with no hint at all, as after a lost event
    r.put(".luaurc", &good);
    let changes = r.diff(&[]);
    assert!(!changes.is_empty());
    assert!(changes.changed.contains(Path::new(".luaurc")));
    r.p.apply(&changes).unwrap();

    assert!(r.diff(&[]).is_empty());
    r.fresh();
}

#[test]
fn new_nested_project_is_mounted() {
    let mut r = start("normal-build");
    r.put(
        "src/sub/default.project.json",
        r#"{"name": "Sub", "tree": {"$path": "code"}}"#,
    );
    r.put("src/sub/code/x.luau", "return require(\"@mod/helpers\")");
    r.put("src/sub/ignored.luau", "return 1");

    let changes = r.diff(&["src/sub"]);
    assert!(changes.config);
    let applied = r.p.apply(&changes).unwrap();

    assert!(applied.rewatch);
    assert!(
        r.p.mounts()
            .projects
            .contains(Path::new("src/sub/default.project.json"))
    );
    r.fresh();
}

/// Every mirrored script that still holds an alias require.
fn raw_requires(p: &Pipeline) -> Vec<PathBuf> {
    listing(&p.tree())
        .into_iter()
        .filter(|(rel, bytes)| {
            let script = rel.extension().is_some_and(|e| e == "luau" || e == "lua");
            let text = bytes.as_deref().map(String::from_utf8_lossy);
            script
                && text
                    .is_some_and(|t| t.contains("require(\"@mod") || t.contains("require(\"@pkg"))
        })
        .map(|(rel, _)| rel)
        .collect()
}

#[test]
fn a_failed_apply_leaves_no_raw_script_and_refuses_to_build() {
    let mut r = start("normal-build");
    let good = read(&r.dir.join(".luaurc"));
    r.put(".luaurc", "{");
    r.put("src/new.luau", "return require(\"@mod/helpers\")");

    assert!(r.try_step(&[".luaurc", "src/new.luau"]).is_err());

    assert!(raw_requires(&r.p).is_empty(), "{:?}", raw_requires(&r.p));
    let err = r.p.build().unwrap_err();
    assert!(format!("{err:#}").contains("failed"), "{err:#}");
    assert!(!r.args_output().exists());

    r.put(".luaurc", &good);
    r.try_step(&[".luaurc"]).unwrap();
    assert!(raw_requires(&r.p).is_empty());
    r.p.build().unwrap();
    assert!(r.args_output().is_file());
    r.fresh();
}

#[test]
fn a_failed_config_apply_leaves_no_raw_script_in_a_new_mount() {
    let mut r = start("normal-build");
    r.put("extra/x.luau", "return require(\"@pkg/Promise\")");
    r.put(
        "default.project.json",
        r#"{"name": "NormalBuild", "tree": {"$path": "src", "pkg": {"$path": "pkg"}, "extra": {"$path": "extra"}}}"#,
    );
    r.put(".luaurc", "{");

    assert!(
        r.try_step(&["default.project.json", "extra", ".luaurc"])
            .is_err()
    );

    assert!(raw_requires(&r.p).is_empty(), "{:?}", raw_requires(&r.p));
    assert!(r.p.build().is_err());
}
