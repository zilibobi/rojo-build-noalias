mod common;

use common::{fixture, rbn, read, text};
use std::path::Path;

fn convert(project: &Path, out: &Path, extra: &[&str]) {
    let run = rbn()
        .current_dir(project)
        .args(["--convert-to", out.to_str().unwrap()])
        .args(extra)
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "convert should succeed:\n{}",
        text(&run)
    );
}

// tests/init.luau:400-416
#[test]
fn process_orphans_replaces_text() {
    let (_g, p) = fixture("orphan-mode");
    let out = p.join("../orphan-mode-out");
    convert(&p, &out, &["--process-orphans"]);

    let main = read(&out.join("src/main.luau"));
    assert!(main.contains(r#"require("./packages/Promise")"#), "{main}");
    assert!(main.contains(r#"require("./lib/utils")"#), "{main}");

    let nested = read(&out.join("src/deep/nested.luau"));
    assert!(
        nested.contains(r#"require("./packages/Promise")"#),
        "{nested}"
    );
}

// tests/init.luau:418-430
#[test]
fn orphans_skipped_without_flag() {
    let (_g, p) = fixture("orphan-mode");
    let out = p.join("../orphan-mode-no-flag-out");
    convert(&p, &out, &[]);

    let main = read(&out.join("src/main.luau"));
    assert!(main.contains("@packages/Promise"), "{main}");
    assert!(main.contains("@lib/utils"), "{main}");
}

#[test]
fn nested_orphan_scope_falls_to_the_root_scope_without_the_flag() {
    let (_g, p) = fixture("nested-luaurc");
    let out = p.join("../out");
    convert(&p, &out, &[]);

    let job = read(&out.join("src/worker/job.luau"));
    assert!(job.contains(r#"require("../pkg/vfx")"#), "{job}");
    assert!(job.contains(r#"require("../mod/helpers")"#), "{job}");
}

#[test]
fn nested_orphan_scope_runs_first_with_the_flag() {
    let (_g, p) = fixture("nested-luaurc");
    let out = p.join("../out");
    convert(&p, &out, &["--process-orphans"]);

    let job = read(&out.join("src/worker/job.luau"));
    assert!(
        job.contains(r#"require("../BundledAssets/ForgeVFX")"#),
        "{job}"
    );
    assert!(job.contains(r#"require("../mod/helpers")"#), "{job}");
}
