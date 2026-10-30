mod common;

use common::{convert, fixture, json, read, rojo_build, text};
use std::path::Path;

fn write(p: &Path, body: &str) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, body).unwrap();
}

// tests/init.luau:264-291
#[test]
fn external_path_is_flattened_into_root() {
    let (_g, p) = fixture("external-path");
    let out = p.join("../external-path-out");
    convert(&p, &out, &[]);

    assert!(
        out.join("src/pkg").is_dir(),
        "pkg should be moved into src/"
    );
    assert!(
        out.join("src/pkg/Promise.luau").is_file(),
        "Promise.luau should exist in src/pkg/"
    );
    assert!(
        out.join("src/pkg/Signal.luau").is_file(),
        "Signal.luau should exist in src/pkg/"
    );
    assert!(
        !out.join("pkg").exists(),
        "pkg/ should not exist at root level"
    );

    let helpers = read(&out.join("src/mod/helpers.luau"));
    assert!(
        helpers.contains(r#"require("../pkg/Promise")"#),
        "{helpers}"
    );

    let init = read(&out.join("src/init.luau"));
    assert!(init.contains(r#"require("@self/pkg/Promise")"#), "{init}");

    // the root $path mounts pkg now; an entry left behind would build a second pkg
    let project = json(&out.join("default.project.json"));
    assert_eq!(
        project["tree"].get("pkg"),
        None,
        "project.json should drop the pkg entry once pkg sits inside src"
    );
    assert_eq!(project["tree"]["$path"], "src");
}

// tests/init.luau:293-318
#[test]
fn multiple_external_paths_are_all_flattened() {
    let (_g, p) = fixture("multi-external-path");
    let out = p.join("../multi-external-path-out");
    convert(&p, &out, &[]);

    assert!(out.join("src/pkg").is_dir(), "pkg should be inside src/");
    assert!(
        out.join("src/vendor").is_dir(),
        "vendor should be inside src/"
    );
    assert!(out.join("src/pkg/Promise.luau").is_file());
    assert!(out.join("src/vendor/Lib.luau").is_file());
    assert!(!out.join("pkg").exists(), "pkg/ should not exist at root");
    assert!(
        !out.join("vendor").exists(),
        "vendor/ should not exist at root"
    );

    let utils = read(&out.join("src/mod/utils.luau"));
    assert!(utils.contains(r#"require("../pkg/Promise")"#), "{utils}");
    assert!(utils.contains(r#"require("../vendor/Lib")"#), "{utils}");

    let project = json(&out.join("default.project.json"));
    assert_eq!(project["tree"].get("pkg"), None, "pkg entry dropped");
    assert_eq!(project["tree"].get("vendor"), None, "vendor entry dropped");
}

// tests/init.luau:320-335
#[test]
fn child_path_inside_root_is_not_moved() {
    let (_g, p) = fixture("nested-inside-path");
    let out = p.join("../nested-inside-path-out");
    convert(&p, &out, &[]);

    assert!(
        out.join("src/lib/foo.luau").is_file(),
        "foo.luau should be at src/lib/"
    );
    assert!(!out.join("src/src").exists(), "should not create src/src/");

    let project = json(&out.join("default.project.json"));
    assert_eq!(project["tree"]["lib"]["$path"], "src/lib");
    assert_eq!(
        read(&out.join("default.project.json")),
        read(&p.join("default.project.json")),
        "a project with nothing to move is not rewritten"
    );
}

// v1.2.4 compared the raw strings, took "./src/lib" for a path outside "src/", deleted
// src/lib while moving it onto itself and dropped its entry
#[test]
fn inside_path_spelled_differently_is_not_moved() {
    let (_g, p) = fixture("nested-inside-path");
    let project =
        r#"{"name": "NestedInsidePath", "tree": {"$path": "src/", "lib": {"$path": "./src/lib"}}}"#;
    write(&p.join("default.project.json"), project);
    let out = p.join("../nested-inside-path-out");
    convert(&p, &out, &[]);

    assert!(out.join("src/lib/foo.luau").is_file());
    assert!(!out.join("src/src").exists());
    assert_eq!(read(&out.join("default.project.json")), project);
}

#[test]
fn flattened_entry_with_more_than_a_path_is_kept_and_warned() {
    let (_g, p) = fixture("external-path");
    write(
        &p.join("default.project.json"),
        r#"{"name": "ExternalPath", "tree": {"$path": "src", "pkg": {"$path": "pkg", "$className": "Folder"}}}"#,
    );
    let out = p.join("../external-path-out");
    let run = convert(&p, &out, &[]);

    assert!(
        text(&run).contains(r#""pkg" was moved into "src" but its project entry carries more than a $path, so it is kept and Rojo will build two instances named "pkg""#),
        "{}",
        text(&run)
    );
    let project = json(&out.join("default.project.json"));
    assert_eq!(project["tree"]["pkg"]["$path"], "src/pkg");
    assert_eq!(project["tree"]["pkg"]["$className"], "Folder");
    assert!(out.join("src/pkg/Promise.luau").is_file());
}

// v1.2.4 resolved a nested project's $paths against the workspace root, so flattening
// pkg/inner moved the root's pkg (holding pkg/inner itself) and then crashed writing
// pkg/inner/default.project.json (util.luau:592), as it does on vfx-forge
#[test]
fn nested_project_paths_resolve_against_its_own_dir() {
    let (_g, p) = fixture("nested-flatten");
    let out = p.join("../nested-flatten-out");
    convert(&p, &out, &[]);

    assert!(
        out.join("src/pkg/inner/src/pkg/Promise.luau").is_file(),
        "the inner pkg moves into the inner root, then pkg into the outer root"
    );
    assert!(!out.join("pkg").exists());
    assert!(!out.join("src/pkg/inner/pkg").exists());

    let inner = json(&out.join("src/pkg/inner/default.project.json"));
    assert_eq!(inner["tree"].get("pkg"), None, "{inner}");
    let outer = json(&out.join("default.project.json"));
    assert_eq!(outer["tree"].get("pkg"), None, "{outer}");

    let job = read(&out.join("src/pkg/inner/src/init.luau"));
    assert!(job.contains(r#"require("@self/pkg/Promise")"#), "{job}");

    // the converted tree builds the same instances as the original
    let before = rojo_build(&p, &p, &p.join("../before.rbxm"));
    let after = rojo_build(&out, &out, &p.join("../after.rbxm"));
    let shape = |roots: &[common::model::Inst]| roots.iter().map(|r| r.shape()).collect::<Vec<_>>();
    assert_eq!(shape(&before), shape(&after));
}

// v1.2.4 crashed on an optional child $path (it ran a string pattern over the table) and
// dropped the entry of a child whose directory was missing
#[test]
fn optional_and_missing_child_paths() {
    let (_g, p) = fixture("external-path");
    write(
        &p.join("default.project.json"),
        r#"{"name": "ExternalPath", "tree": {"$path": "src", "pkg": {"$path": {"optional": "pkg"}}, "gone": {"$path": {"optional": "gone"}}, "kept": {"$path": {"optional": "extra"}, "$className": "Folder"}}}"#,
    );
    write(&p.join("extra/x.luau"), "return 1\n");
    let out = p.join("../external-path-out");
    convert(&p, &out, &[]);

    let project = json(&out.join("default.project.json"));
    assert_eq!(project["tree"].get("pkg"), None, "{project}");
    assert!(out.join("src/pkg/Promise.luau").is_file());
    assert_eq!(
        project["tree"]["gone"]["$path"]["optional"], "gone",
        "a missing optional path stays as written"
    );
    assert_eq!(project["tree"]["kept"]["$path"]["optional"], "src/kept");
    assert!(out.join("src/kept/x.luau").is_file());
}

// v1.2.4 resolved "../outside" against DIR and moved a directory next to DIR into the output
#[test]
fn path_outside_the_converted_tree_is_left_alone() {
    let (_g, p) = fixture("external-path");
    write(
        &p.join("default.project.json"),
        r#"{"name": "ExternalPath", "tree": {"$path": "src", "pkg": {"$path": "pkg"}, "outside": {"$path": "../outside"}, "nested": {"$path": "other.project.json"}}}"#,
    );
    write(
        &p.join("other.project.json"),
        r#"{"name": "Other", "tree": {"$className": "Folder"}}"#,
    );
    write(&p.join("../outside/x.luau"), "return 1\n");
    let out = p.join("../external-path-out");
    let run = convert(&p, &out, &[]);
    let log = text(&run);

    assert!(p.join("../outside/x.luau").is_file(), "{log}");
    assert!(!out.join("src/outside").exists(), "{log}");
    assert!(
        log.contains(
            r#""outside" cannot be moved into "src", so its project entry is left as it is"#
        ),
        "{log}"
    );
    assert!(
        log.contains(
            r#""nested" cannot be moved into "src", so its project entry is left as it is"#
        ),
        "{log}"
    );
    assert!(out.join("other.project.json").is_file());

    let project = json(&out.join("default.project.json"));
    assert_eq!(project["tree"]["outside"]["$path"], "../outside");
    assert_eq!(project["tree"]["nested"]["$path"], "other.project.json");
    assert_eq!(project["tree"].get("pkg"), None, "{project}");
}

// v1.2.4 moved a file to "<root>/<key>" with no extension, which Rojo ignores, and dropped
// its entry, so the script vanished from the converted build
#[test]
fn file_child_keeps_its_suffix_and_meta() {
    let (_g, p) = fixture("external-path");
    write(
        &p.join("default.project.json"),
        r#"{"name": "ExternalPath", "tree": {"$path": "src", "pkg": {"$path": "pkg"}, "Runner": {"$path": "lib/run.server.luau"}}}"#,
    );
    write(&p.join("lib/run.server.luau"), "print(1)\n");
    write(
        &p.join("lib/run.meta.json"),
        r#"{"properties": {"Disabled": true}}"#,
    );
    let out = p.join("../external-path-out");
    convert(&p, &out, &[]);

    assert!(out.join("src/Runner.server.luau").is_file());
    assert!(out.join("src/Runner.meta.json").is_file());
    assert!(!out.join("lib/run.server.luau").exists());
    assert!(!out.join("lib/run.meta.json").exists());
    let project = json(&out.join("default.project.json"));
    assert_eq!(project["tree"].get("Runner"), None, "{project}");

    let before = rojo_build(&p, &p, &p.join("../before.rbxm"));
    let after = rojo_build(&out, &out, &p.join("../after.rbxm"));
    let runner = after[0].find(&["Runner"]).expect("Runner is built");
    assert_eq!(runner.class, "Script");
    let shape = |roots: &[common::model::Inst]| roots.iter().map(|r| r.shape()).collect::<Vec<_>>();
    assert_eq!(shape(&before), shape(&after));
}
