mod common;

use common::{convert, json, read, rojo_build};
use std::path::Path;

const FORGE_DIR: &str = "/home/zili/Projects/laptop/vfx-forge/pkg/forge-vfx";

// tests/init.luau:467-499; converts a copy, so nothing runs inside the checkout
#[test]
fn real_forge_vfx_project_converts() {
    let forge = Path::new(FORGE_DIR);

    if !forge.is_dir() {
        eprintln!("skipped: {FORGE_DIR} not found");
        return;
    }

    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("forge-vfx");
    rojo_build_noalias::workspace::copy_tree(forge, &p, None).unwrap();
    let out = tmp.path().join("forge-out");
    convert(&p, &out, &[]);

    assert!(out.join("src/pkg").is_dir(), "pkg should be inside src/");
    assert!(out.join("src/pkg/Promise.luau").is_file());
    assert!(out.join("src/pkg/Shake.luau").is_file());
    assert!(!out.join("pkg").exists(), "pkg/ should not exist at root");

    let effects = read(&out.join("src/services/effects.luau"));
    assert!(
        effects.contains(r#"require("../pkg/Promise")"#),
        "{effects}"
    );

    let project = json(&out.join("default.project.json"));
    assert_eq!(
        project["tree"].get("pkg"),
        None,
        "forge-vfx project drops the pkg entry"
    );

    let camshake = read(&out.join("src/effects/camera_shake.luau"));
    assert!(
        camshake.contains(r#"require("../pkg/Shake")"#),
        "{camshake}"
    );

    let types = read(&out.join("src/types.luau"));
    assert!(types.contains(r#"require("./pkg/Promise")"#), "{types}");

    // the converted tree builds the same instances as the original
    let before = rojo_build(&p, &p, &tmp.path().join("before.rbxm"));
    let after = rojo_build(&out, &out, &tmp.path().join("after.rbxm"));
    let shape = |roots: &[common::model::Inst]| roots.iter().map(|r| r.shape()).collect::<Vec<_>>();
    assert_eq!(shape(&before), shape(&after));
}
