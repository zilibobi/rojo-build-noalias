mod common;

use common::{build, fixture, model, read, set_dir_mtime, text, tmp_of, workspaces_in};
use std::fs::File;
use std::path::Path;
use std::time::{Duration, SystemTime};

fn assert_ok(out: &std::process::Output) {
    assert!(out.status.success(), "build should succeed:\n{}", text(out));
}

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

// tests/init.luau:438-459
#[test]
fn normal_build_leaves_the_project_untouched() {
    let (_g, p) = fixture("normal-build");
    let before = read(&p.join("src/init.luau"));
    assert_ok(&build(&p, &["-o", "out.rbxm"]));

    assert!(p.join("pkg").is_dir());
    assert!(!p.join("src/pkg").exists());
    assert_eq!(read(&p.join("src/init.luau")), before);
    assert_eq!(
        read(&p.join("default.project.json")),
        read(
            &Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/normal-build/default.project.json")
        )
    );
}

#[test]
fn built_model_holds_dealiased_sources() {
    let (_g, p) = fixture("normal-build");
    let out = p.join("../out.rbxm");
    assert_ok(&build(&p, &["-o", out.to_str().unwrap()]));

    let roots = model::load(&out);
    let init = roots[0].source.as_deref().unwrap();
    assert!(
        init.contains(r#"require("@self/mod/helpers")"#)
            && init.contains(r#"require("@self/pkg/Promise")"#),
        "{init}"
    );

    let helpers = roots[0].find(&["mod", "helpers"]).unwrap();
    let helpers = helpers.source.as_deref().unwrap();
    assert!(
        helpers.contains(r#"require("../pkg/Promise")"#),
        "{helpers}"
    );
    assert!(
        model::sources(&roots)
            .iter()
            .all(|(_, s)| !s.contains("@mod/") && !s.contains("@pkg/"))
    );
}

#[test]
fn marker_is_printed_once_after_the_output_exists() {
    let (_g, p) = fixture("normal-build");
    let out = build(&p, &["-o", "out.rbxm"]);
    assert_ok(&out);

    let s = text(&out);
    let markers: Vec<_> = s
        .lines()
        .filter(|l| l.contains("Built project to"))
        .collect();
    assert_eq!(markers, vec!["OK   Built project to out.rbxm"], "{s}");
    assert!(p.join("out.rbxm").is_file());
}

#[test]
fn failed_build_exits_one_and_keeps_the_old_output() {
    let (_g, p) = fixture("normal-build");
    std::fs::write(p.join("out.rbxm"), "old").unwrap();
    std::fs::write(
        p.join("default.project.json"),
        r#"{"name": "X", "tree": {"$path": "src", "gone": {"$path": "missing"}}}"#,
    )
    .unwrap();

    let out = build(&p, &["-o", "out.rbxm"]);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out));
    assert_eq!(read(&p.join("out.rbxm")), "old");
    assert!(!text(&out).contains("Built project to"), "{}", text(&out));
    assert!(
        text(&out).contains("ERR  Rojo build failed"),
        "{}",
        text(&out)
    );
}

#[test]
fn missing_output_dir_fails_without_a_marker() {
    let (_g, p) = fixture("normal-build");
    let out = build(&p, &["-o", "nowhere/out.rbxm"]);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out));
    assert!(!text(&out).contains("Built project to"), "{}", text(&out));
    assert!(!p.join("nowhere").exists());
}

#[test]
fn workspace_is_removed_after_exit() {
    let (_g, p) = fixture("normal-build");
    assert_ok(&build(&p, &["-o", "out.rbxm"]));
    assert_eq!(workspaces_in(&tmp_of(&p)), Vec::<std::path::PathBuf>::new());

    // and after a failed build too
    std::fs::write(p.join("default.project.json"), "{ not json").unwrap();
    assert_eq!(build(&p, &["-o", "out.rbxm"]).status.code(), Some(1));
    assert_eq!(workspaces_in(&tmp_of(&p)), Vec::<std::path::PathBuf>::new());
}

#[test]
fn concurrent_runs_do_not_share_workspaces() {
    let (_g, p) = fixture("normal-build");

    let runs: Vec<_> = ["a.rbxm", "b.rbxm"]
        .into_iter()
        .map(|name| {
            let p = p.clone();
            std::thread::spawn(move || build(&p, &["-o", name]))
        })
        .collect();

    for run in runs {
        assert_ok(&run.join().unwrap());
    }

    assert_eq!(
        std::fs::read(p.join("a.rbxm")).unwrap(),
        std::fs::read(p.join("b.rbxm")).unwrap()
    );
    assert!(workspaces_in(&tmp_of(&p)).is_empty());
}

#[test]
fn stale_workspace_is_swept_live_one_kept() {
    let (_g, p) = fixture("normal-build");
    let tmp = tmp_of(&p);
    let dead = tmp.join("rojo-build-noalias.dead");
    let live = tmp.join("rojo-build-noalias.live");
    let v1 = tmp.join("rojo-build-noalias");

    for dir in [&dead, &live, &v1] {
        write(&dir.join("tree/x.luau"), "x");
        std::fs::write(dir.join("lock"), "").unwrap();
    }

    let held = File::open(live.join("lock")).unwrap();
    held.lock().unwrap();

    // both past the age a fresh run is spared for, so only the lock tells them apart
    let old = SystemTime::now() - Duration::from_secs(60);

    for dir in [&dead, &live, &v1] {
        set_dir_mtime(dir, old);
    }

    assert_ok(&build(&p, &["-o", "out.rbxm"]));

    assert!(!dead.exists());
    assert!(live.join("tree/x.luau").is_file());
    // v1.2.4's shared dir is not one of ours
    assert!(v1.join("tree/x.luau").is_file());
    assert_eq!(workspaces_in(&tmp), vec![live.clone()]);
    drop(held);
}

#[test]
fn named_project_build_uses_the_cli_project() {
    let (_g, p) = fixture("named-project");
    assert_ok(&build(&p, &["dev.project.json", "--output", "out.rbxm"]));

    let roots = model::load(&p.join("out.rbxm"));
    let story = roots[0].find(&["stories", "button.story"]).unwrap();
    let story = story.source.as_deref().unwrap();
    assert!(story.contains(r#"require("../mod/helpers")"#), "{story}");
}

#[test]
fn nested_luaurc_build_matches_startup_rules() {
    let (_g, p) = fixture("nested-luaurc");

    assert_ok(&build(&p, &["-o", "plain.rbxm"]));
    let roots = model::load(&p.join("plain.rbxm"));
    let job = roots[0].find(&["worker", "job"]).unwrap();
    let job = job.source.as_deref().unwrap();
    assert!(job.contains(r#"require("../pkg/vfx")"#), "{job}");
    assert!(job.contains(r#"require("../mod/helpers")"#), "{job}");

    assert_ok(&build(&p, &["--process-orphans", "-o", "orphans.rbxm"]));
    let roots = model::load(&p.join("orphans.rbxm"));
    let job = roots[0].find(&["worker", "job"]).unwrap();
    let job = job.source.as_deref().unwrap();
    assert!(
        job.contains(r#"require("../BundledAssets/ForgeVFX")"#),
        "{job}"
    );
    assert!(job.contains(r#"require("../mod/helpers")"#), "{job}");
}

#[test]
fn luaurc_outside_the_mounts_is_not_processed() {
    let (_g, p) = fixture("normal-build");
    write(&p.join("junk/.luaurc"), r#"{"aliases":{"x":"./x"}}"#);
    write(&p.join("junk/default.project.json"), "{ not json");

    let out = build(&p, &["-v", "-o", "out.rbxm"]);
    assert_ok(&out);
    assert!(
        text(&out).lines().all(|l| !l.contains("junk")),
        "{}",
        text(&out)
    );
}

#[test]
fn path_outside_the_project_is_an_error() {
    let (_g, p) = fixture("normal-build");
    write(&p.join("../outside/x.luau"), "return 1");
    std::fs::write(
        p.join("default.project.json"),
        r#"{"name": "X", "tree": {"$path": "src", "out": {"$path": "../outside"}}}"#,
    )
    .unwrap();

    let out = build(&p, &["-o", "out.rbxm"]);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out));
    assert!(
        text(&out).contains("outside the project directory"),
        "{}",
        text(&out)
    );
    assert!(text(&out).contains("../outside"), "{}", text(&out));
    assert!(!p.join("out.rbxm").exists());
}

#[test]
fn rbxmx_output_is_xml() {
    let (_g, p) = fixture("normal-build");
    assert_ok(&build(&p, &["-o", "out.rbxmx"]));
    assert!(read(&p.join("out.rbxmx")).starts_with("<roblox"));
}

#[test]
fn build_prints_the_table_only_with_verbose() {
    let (_g, p) = fixture("normal-build");

    let quiet = text(&build(&p, &["-o", "out.rbxm"]));
    assert!(!quiet.contains("Processed project:"), "{quiet}");
    assert!(!quiet.contains("Done in"), "{quiet}");
    assert!(!quiet.contains("@pkg/Promise"), "{quiet}");

    let loud = text(&build(&p, &["-v", "-o", "out.rbxm"]));
    assert!(loud.contains("OK   Processed project: "), "{loud}");
    assert!(loud.contains("INFO Done in "), "{loud}");
    assert!(
        loud.contains(
            r#"NormalBuild/mod/helpers require("@pkg/Promise") -> require("../pkg/Promise")"#
        ),
        "{loud}"
    );
}

#[test]
fn file_mount_reads_its_meta_file() {
    let (_g, p) = fixture("normal-build");
    write(
        &p.join("lib/run.server.luau"),
        "print(require(\"@mod/helpers\"))",
    );
    write(
        &p.join("lib/run.meta.json"),
        r#"{"properties": {"Disabled": true}}"#,
    );
    std::fs::write(
        p.join("default.project.json"),
        r#"{"name": "X", "tree": {"$path": "src", "pkg": {"$path": "pkg"}, "Runner": {"$path": "lib/run.server.luau"}}}"#,
    )
    .unwrap();

    assert_ok(&build(&p, &["-o", "out.rbxmx"]));
    let xml = read(&p.join("out.rbxmx"));
    assert!(
        xml.contains(r#"<bool name="Disabled">true</bool>"#),
        "{xml}"
    );
}

#[test]
fn skip_names_apply_only_below_a_mount_root() {
    let (_g, p) = fixture("normal-build");
    write(&p.join("node_modules/pkgs/Thing.luau"), "return 1");
    write(&p.join("src/node_modules/Hidden.luau"), "return 2");
    std::fs::write(
        p.join("default.project.json"),
        r#"{"name": "X", "tree": {"$path": "src", "pkg": {"$path": "pkg"}, "Packages": {"$path": "node_modules/pkgs"}}}"#,
    )
    .unwrap();

    assert_ok(&build(&p, &["-o", "out.rbxm"]));
    let roots = model::load(&p.join("out.rbxm"));
    assert!(roots[0].find(&["Packages", "Thing"]).is_some(), "{roots:?}");
    assert!(roots[0].find(&["node_modules"]).is_none(), "{roots:?}");
}

#[test]
fn rojo_runs_in_the_project_dir() {
    // run from outside the project: a rokit shim finds its manifest only from the project dir
    let (_g, p) = fixture("normal-build");
    let parent = p.parent().unwrap();
    let tmp = tmp_of(&p);
    let out = common::rbn()
        .current_dir(parent)
        .env("TMPDIR", &tmp)
        .env("TEMP", &tmp)
        .env("TMP", &tmp)
        .args(["--color", "never", "normal-build", "-o", "out.rbxm"])
        .output()
        .unwrap();

    assert_ok(&out);
    assert!(parent.join("out.rbxm").is_file());
    assert!(text(&out).contains("OK   Built project to out.rbxm"));
}

#[test]
fn missing_project_dir_is_an_error() {
    let (_g, p) = fixture("normal-build");
    let out = build(&p, &["gone", "-o", "out.rbxm"]);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out));
    assert!(text(&out).contains("does not exist"), "{}", text(&out));
    assert!(!p.join("out.rbxm").exists());
}

#[test]
fn nested_root_through_a_skip_name_is_mirrored() {
    // the outer root leaves src/node_modules out; the inner root names it itself
    let (_g, p) = fixture("normal-build");
    write(&p.join("src/node_modules/x/Thing.luau"), "return 1");
    std::fs::write(
        p.join("default.project.json"),
        r#"{"name": "X", "tree": {"$className": "Folder", "A": {"$path": "src"}, "B": {"$path": "src/node_modules/x"}}}"#,
    )
    .unwrap();

    assert_ok(&build(&p, &["-o", "out.rbxm"]));
    let roots = model::load(&p.join("out.rbxm"));
    assert!(roots[0].find(&["B", "Thing"]).is_some(), "{roots:?}");

    // the one mirror now holds src/node_modules/x, so A shows it too, as plain Rojo builds it
    let plain = common::rojo_build(&p, &p.join("default.project.json"), &p.join("plain.rbxm"));
    let shape = |roots: &[model::Inst]| roots.iter().map(model::Inst::shape).collect::<Vec<_>>();
    assert_eq!(shape(&roots), shape(&plain));
}
