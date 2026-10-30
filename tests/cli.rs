mod common;

use common::{rbn, text};

#[test]
fn no_args_prints_help_and_exits_zero() {
    let out = rbn().output().unwrap();
    assert!(out.status.success());
    let s = text(&out);
    assert!(s.contains(&format!("rojo-build-noalias {}", env!("CARGO_PKG_VERSION"))));
    assert!(s.contains("USAGE:"));
    assert!(s.contains("--convert-to <DIR>"));
    assert!(s.contains("--process-orphans"));
    assert!(s.contains("--wax-compat"));
}

#[test]
fn help_wins_over_other_flags() {
    for flag in ["-h", "--help"] {
        let out = rbn().args(["-o", "x.rbxm", flag]).output().unwrap();
        assert!(out.status.success());
        assert!(text(&out).contains("USAGE:"));
    }
}

#[test]
fn version_flag() {
    for flag in ["--version", "-V"] {
        let out = rbn().arg(flag).output().unwrap();
        assert_eq!(
            String::from_utf8(out.stdout).unwrap(),
            format!("rojo-build-noalias {}\n", env!("CARGO_PKG_VERSION"))
        );
    }
}

#[cfg(target_os = "linux")]
#[test]
fn plugin_refused_on_linux() {
    let out = rbn().args(["-p", "X.rbxm"]).output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(text(&out).contains("The '--plugin' option is not supported on Linux."));
}

#[test]
fn usage_errors_exit_two() {
    assert_eq!(rbn().arg("-o").output().unwrap().status.code(), Some(2));
    assert_eq!(
        rbn()
            .args(["--frobnicate", "-o", "x.rbxm"])
            .output()
            .unwrap()
            .status
            .code(),
        Some(2)
    );
    // neither -o nor -p
    assert_eq!(
        rbn().arg("--watch").output().unwrap().status.code(),
        Some(2)
    );
}
