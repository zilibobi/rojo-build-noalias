mod common;

use common::{convert, fixture, json, read, rojo_build};

// tests/init.luau:343-392
#[test]
fn convert_to_copies_all_files_and_resolves_aliases() {
    let (_g, p) = fixture("convert-to-structure");
    let out = p.join("../convert-to-structure-out");
    convert(&p, &out, &[]);

    assert!(out.join("src/init.luau").is_file());
    assert!(out.join("src/mod/helpers.luau").is_file());

    assert!(
        out.join("extra.txt").is_file(),
        "extra.txt should be copied"
    );
    assert!(read(&out.join("extra.txt")).contains("this file should be copied too"));

    assert!(out.join("default.project.json").is_file());
    assert!(
        out.join(".luaurc").is_file(),
        ".luaurc should exist in output"
    );
    assert!(
        out.join("rokit.toml").is_file(),
        "rokit.toml should exist in output"
    );

    let init = read(&out.join("src/init.luau"));
    assert!(!init.contains("@mod/"), "{init}");
    assert!(!init.contains("@pkg/"), "{init}");

    assert!(out.join("src/pkg").is_dir(), "pkg should be inside src/");
    assert!(!out.join("pkg").exists(), "pkg/ should not exist at root");

    // and the converted project builds exactly one instance named pkg
    let model = rojo_build(&out, &out, &out.join("built.rbxm"));
    assert_eq!(model.len(), 1, "one root instance");
    let named = model[0].children.iter().filter(|c| c.name == "pkg").count();
    assert_eq!(
        named, 1,
        "the built model holds one child named pkg, not two"
    );
}

// v1.2.4 rewrote the project through a Lua table: keys came out sorted and an empty array
// came out as an empty object, which Rojo rejects for globIgnorePaths
#[test]
fn converted_project_keeps_key_order() {
    let (_g, p) = fixture("external-path");
    std::fs::write(
        p.join("default.project.json"),
        r#"{"name": "ExternalPath", "globIgnorePaths": [], "tree": {"$path": "src", "pkg": {"$path": "pkg"}, "Zed": {"$className": "Folder"}, "Alpha": {"$className": "Folder"}}, "emitLegacyScripts": false}"#,
    )
    .unwrap();
    let out = p.join("../external-path-out");
    convert(&p, &out, &[]);

    let text = read(&out.join("default.project.json"));
    let at = |key: &str| {
        text.find(&format!("\"{key}\""))
            .unwrap_or_else(|| panic!("{key} missing:\n{text}"))
    };
    assert!(at("name") < at("globIgnorePaths"), "{text}");
    assert!(at("globIgnorePaths") < at("tree"), "{text}");
    assert!(at("tree") < at("emitLegacyScripts"), "{text}");
    assert!(at("Zed") < at("Alpha"), "{text}");
    assert!(text.contains(r#""globIgnorePaths": []"#), "{text}");

    let project = json(&out.join("default.project.json"));
    assert_eq!(project["tree"].get("pkg"), None, "{text}");
}
