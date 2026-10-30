mod common;

use common::{fixture, rbn, read, text};

#[test]
fn named_project_converts_against_cli_project() {
    let (_g, p) = fixture("named-project");

    let dev = p.join("../dev-out");
    let run = rbn()
        .current_dir(&p)
        .args(["dev.project.json", "--convert-to", dev.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(run.status.success(), "{}", text(&run));
    let story = read(&dev.join("src/stories/button.story.luau"));
    assert!(story.contains(r#"require("../mod/helpers")"#), "{story}");

    // default.project.json ignores stories, so they are not in its sourcemap and stay raw
    let default = p.join("../default-out");
    let run = rbn()
        .current_dir(&p)
        .args(["--convert-to", default.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(run.status.success(), "{}", text(&run));
    let story = read(&default.join("src/stories/button.story.luau"));
    assert!(story.contains(r#"require("@mod/helpers")"#), "{story}");
    let init = read(&default.join("src/init.luau"));
    assert!(init.contains(r#"require("@self/mod/helpers")"#), "{init}");
}

#[test]
fn convert_into_project_dir_is_refused() {
    let (_g, p) = fixture("basic-alias");
    let run = rbn()
        .current_dir(&p)
        .args(["--convert-to", "."])
        .output()
        .unwrap();
    assert_eq!(run.status.code(), Some(1), "{}", text(&run));
    assert!(p.join("default.project.json").is_file());
    assert!(p.join("src/init.luau").is_file());
}

#[test]
fn convert_inside_project_does_not_nest_previous_output() {
    let (_g, p) = fixture("basic-alias");

    for _ in 0..2 {
        let run = rbn()
            .current_dir(&p)
            .args(["--convert-to", "out"])
            .output()
            .unwrap();
        assert!(run.status.success(), "{}", text(&run));
    }

    assert!(p.join("out/src/init.luau").is_file());
    assert!(!p.join("out/out").exists());
    let init = read(&p.join("out/src/init.luau"));
    assert!(init.contains(r#"require("@self/mod/helpers")"#), "{init}");
    // the project itself is never rewritten
    assert!(read(&p.join("src/init.luau")).contains(r#"require("@mod/helpers")"#));
}

#[test]
fn watch_with_convert_is_warned_and_ignored() {
    let (_g, p) = fixture("basic-alias");
    let out = p.join("../watch-out");
    let run = rbn()
        .current_dir(&p)
        .args(["--watch", "--convert-to", out.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(run.status.success(), "{}", text(&run));
    assert!(text(&run).contains("WARN --watch is ignored with --convert-to"));
    assert!(out.join("src/init.luau").is_file());
}
