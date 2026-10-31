mod common;

use common::{fixture, rbn, read, text};
use std::path::Path;
use std::process::Output;

fn convert(project: &Path, out: &Path, extra: &[&str]) -> Output {
    let out = rbn()
        .current_dir(project)
        .args(["--convert-to", out.to_str().unwrap()])
        .args(extra)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "convert should succeed:\n{}",
        text(&out)
    );
    out
}

// tests/init.luau:155-174
#[test]
fn basic_alias_requires_become_relative() {
    let (_g, p) = fixture("basic-alias");
    let out = p.join("../basic-alias-out");
    convert(&p, &out, &[]);

    let init = read(&out.join("src/init.luau"));
    assert!(init.contains(r#"require("@self/mod/helpers")"#), "{init}");
    assert!(init.contains(r#"require("@self/utils/config")"#), "{init}");
    assert!(!init.contains("@mod/"), "{init}");
    assert!(!init.contains("@utils/"), "{init}");

    let helpers = read(&out.join("src/mod/helpers.luau"));
    assert!(
        helpers.contains(r#"require("../utils/config")"#),
        "{helpers}"
    );
    assert!(!helpers.contains("@utils/"), "{helpers}");
}

// tests/init.luau:176-190
#[test]
fn wax_compat_adds_extra_dotdot_to_nested_init() {
    let (_g, p) = fixture("nested-init");
    let out = p.join("../nested-init-wax-out");
    convert(&p, &out, &["--wax-compat"]);

    let nested = read(&out.join("src/lib/Foo/init.luau"));
    assert!(
        nested.contains(r#"require("../../pkg/Logger")"#),
        "{nested}"
    );
    assert!(!nested.contains(r#"require("../pkg/Logger")"#), "{nested}");
}

// tests/init.luau:192-213
#[test]
fn nested_init_follows_abstract_module_paths() {
    let (_g, p) = fixture("nested-init");
    let out = p.join("../nested-init-out");
    convert(&p, &out, &[]);

    let root = read(&out.join("src/init.luau"));
    assert!(root.contains(r#"require("@self/lib/Foo")"#), "{root}");
    assert!(root.contains(r#"require("@self/pkg/Logger")"#), "{root}");

    let nested = read(&out.join("src/lib/Foo/init.luau"));
    assert!(nested.contains(r#"require("../pkg/Logger")"#), "{nested}");
    assert!(
        !nested.contains(r#"require("../../pkg/Logger")"#),
        "{nested}"
    );
}

// tests/init.luau:215-237
#[test]
fn folder_alias_resolves_when_nested_project_is_first_child() {
    let (_g, p) = fixture("nested-project-in-folder");
    let out = p.join("../nested-project-in-folder-out");
    let run = convert(&p, &out, &[]);
    let log = text(&run);
    assert!(!log.contains("was not found in the sourcemap"), "{log}");

    let user = read(&out.join("src/mod/user.luau"));
    assert!(user.contains(r#"require("../pkg/Promise")"#), "{user}");
    assert!(user.contains(r#"require("../pkg/Encoder")"#), "{user}");
    assert!(!user.contains("@pkg/"), "{user}");

    let init = read(&out.join("src/init.luau"));
    assert!(init.contains(r#"require("@self/pkg/Promise")"#), "{init}");
    assert!(!init.contains("@pkg/"), "{init}");
}

// tests/init.luau:239-256
#[test]
fn self_alias_resolves() {
    let (_g, p) = fixture("self-alias");
    let out = p.join("../self-alias-out");
    convert(&p, &out, &[]);

    let init = read(&out.join("src/init.luau"));
    assert!(init.contains(r#"require("@self/mod/helpers")"#), "{init}");
    assert!(
        init.contains(r#"require("@self/mod/deep/nested")"#),
        "{init}"
    );

    let helpers = read(&out.join("src/mod/helpers.luau"));
    assert!(
        helpers.contains(r#"require("../mod/deep/nested")"#),
        "{helpers}"
    );
}

// owner ruling: the startup table and its timing line print only with -v
#[test]
fn replacement_table_prints_only_with_verbose() {
    let (_g, p) = fixture("basic-alias");

    let quiet = text(&convert(&p, &p.join("../quiet-out"), &[]));
    assert!(!quiet.contains("Processed project:"), "{quiet}");
    assert!(!quiet.contains("Done in"), "{quiet}");
    assert!(!quiet.contains(r#"require("@mod/helpers")"#), "{quiet}");
    assert!(
        quiet.contains("OK   Project converted successfully to"),
        "{quiet}"
    );

    let loud = text(&convert(&p, &p.join("../loud-out"), &["-v"]));
    // the binary prints the resolved path (macOS /private/var, no \\?\ on Windows)
    let resolved = dunce::canonicalize(&p).unwrap();
    let title = format!(
        "OK   Processed project: {}",
        resolved.join("default.project.json").display()
    );
    assert!(loud.contains(&title), "{loud}");
    assert!(loud.contains("INFO Done in "), "{loud}");
    assert!(
        loud.contains(r#"INFO BasicAlias             require("@mod/helpers")  -> require("@self/mod/helpers")"#),
        "{loud}"
    );
}
