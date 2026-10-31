mod common;

use common::watcher::Watcher;
use common::{fixture, read, workspaces_in};
use std::path::Path;
use std::time::{Duration, Instant};

fn secs(n: u64) -> Duration {
    Duration::from_secs(n)
}

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/// A watcher on `p` that has finished its first build.
fn started(p: &Path) -> Watcher {
    let w = Watcher::spawn(p, &[]);
    assert!(w.wait_built(secs(10)), "no first build:\n{}", w.log());
    w
}

/// Exactly one build lands: one marker within 10 s, then none for a second. Generous for a slow
/// runner, and a batch split in two still shows, since a batch closes 200 ms after its last event.
fn one_build(w: &Watcher) {
    assert!(w.wait_built(secs(10)), "no build:\n{}", w.log());
    assert_eq!(
        w.count_built(secs(1)),
        0,
        "more than one build:\n{}",
        w.log()
    );
}

/// The batch fails: its WARN arrives within 10 s, and no marker since the last counted one, or
/// for a second after the WARN.
fn failed_batch(w: &Watcher) {
    assert!(
        w.wait_saw("WARN keeping the last good build at ", secs(10)),
        "no failed batch:\n{}",
        w.log()
    );
    assert!(w.saw("ERR "), "{}", w.log());
    assert_eq!(w.count_built(secs(1)), 0, "{}", w.log());
}

/// The `-v` batch summaries since `mark`.
fn summaries(w: &Watcher, mark: usize) -> Vec<String> {
    w.lines_since(mark, "INFO ")
        .into_iter()
        .filter(|l| l.contains(" changed, ") && l.contains(" removed in "))
        .map(|l| l[..l.find(" in ").unwrap()].to_string())
        .collect()
}

fn source(w: &Watcher, path: &[&str]) -> String {
    w.find(path).source.unwrap_or_default()
}

#[test]
fn burst_of_five_files_is_one_build() {
    let (_g, p) = fixture("normal-build");
    let w = started(&p);

    // 10 ms apart, which a per-path or a shorter window splits, well inside the 200 ms one
    for i in 0..5 {
        write(
            &p.join(format!("src/mod/b{i}.luau")),
            "return require(\"@pkg/Promise\")",
        );
        std::thread::sleep(ms(10));
    }

    one_build(&w);
    assert_eq!(
        source(&w, &["mod", "b4"]),
        "return require(\"../pkg/Promise\")"
    );
    w.assert_no_raw_aliases();
    w.assert_matches_fresh_build();
}

#[test]
fn two_saves_1500ms_apart_both_land() {
    let (_g, p) = fixture("normal-build");
    let w = started(&p);
    let helpers = p.join("src/mod/helpers.luau");

    std::fs::write(&helpers, "return 1").unwrap();
    assert!(w.wait_built(secs(10)), "{}", w.log());
    std::thread::sleep(ms(1500));
    std::fs::write(&helpers, "return 2").unwrap();
    assert!(w.wait_built(secs(10)), "{}", w.log());

    assert_eq!(source(&w, &["mod", "helpers"]), "return 2");
    w.assert_matches_fresh_build();
}

#[test]
fn same_length_save_within_a_second_lands() {
    // v1 compared whole-second mtimes, so a second save of the same size was lost
    let (_g, p) = fixture("normal-build");
    let w = started(&p);
    let helpers = p.join("src/mod/helpers.luau");

    std::fs::write(&helpers, "return 1").unwrap();
    assert!(w.wait_built(secs(10)), "{}", w.log());
    std::fs::write(&helpers, "return 2").unwrap();
    assert!(w.wait_built(secs(10)), "{}", w.log());

    assert_eq!(source(&w, &["mod", "helpers"]), "return 2");
    w.assert_matches_fresh_build();
}

#[test]
fn broken_project_edit_keeps_last_good_build() {
    let (_g, p) = fixture("normal-build");
    let w = started(&p);
    let good = w.out_bytes();
    let project = p.join("default.project.json");
    let original = read(&project);

    std::fs::write(&project, "{ not json").unwrap();
    failed_batch(&w);
    assert_eq!(w.out_bytes(), good);

    std::fs::write(&project, original).unwrap();
    assert!(w.wait_built(secs(10)), "{}", w.log());
    w.assert_no_raw_aliases();
    w.assert_matches_fresh_build();
}

#[test]
fn new_folder_with_module_builds_once_and_keeps_watching() {
    let (_g, p) = fixture("normal-build");
    let w = started(&p);

    write(
        &p.join("src/newdir/thing.luau"),
        "return require(\"@mod/helpers\")",
    );
    one_build(&w);
    assert_eq!(
        source(&w, &["newdir", "thing"]),
        "return require(\"../mod/helpers\")"
    );

    // the new folder is watched: an edit inside it builds again
    write(
        &p.join("src/newdir/thing.luau"),
        "return require(\"@pkg/Promise\")",
    );
    assert!(w.wait_built(secs(10)), "{}", w.log());
    assert_eq!(
        source(&w, &["newdir", "thing"]),
        "return require(\"../pkg/Promise\")"
    );

    write(&p.join("src/newdir/deeper/more.luau"), "return 3");
    assert!(w.wait_built(secs(10)), "{}", w.log());
    assert!(w.has(&["newdir", "deeper", "more"]));
    w.assert_no_raw_aliases();
    w.assert_matches_fresh_build();
}

#[test]
fn deleted_file_leaves_the_model() {
    let (_g, p) = fixture("normal-build");
    let w = started(&p);

    write(&p.join("pkg/Extra.luau"), "return 1");
    assert!(w.wait_built(secs(10)), "{}", w.log());
    assert!(w.has(&["pkg", "Extra"]));

    std::fs::remove_file(p.join("pkg/Extra.luau")).unwrap();
    assert!(w.wait_built(secs(10)), "{}", w.log());
    assert!(!w.has(&["pkg", "Extra"]));
    assert!(w.has(&["pkg", "Promise"]));
    w.assert_no_raw_aliases();
    w.assert_matches_fresh_build();
}

#[test]
fn renamed_file_and_folder_move_in_the_model() {
    let (_g, p) = fixture("normal-build");
    let w = started(&p);

    // a second file keeps src/mod holding one, so the alias still resolves
    write(&p.join("src/mod/keep.luau"), "return 0");
    assert!(w.wait_built(secs(10)), "{}", w.log());

    std::fs::rename(p.join("src/mod/helpers.luau"), p.join("src/mod/util.luau")).unwrap();
    assert!(w.wait_built(secs(10)), "{}", w.log());
    assert!(!w.has(&["mod", "helpers"]));
    assert!(w.has(&["mod", "util"]));

    write(&p.join("src/newdir/a.luau"), "return 1");
    assert!(w.wait_built(secs(10)), "{}", w.log());
    std::fs::rename(p.join("src/newdir"), p.join("src/newdir2")).unwrap();
    assert!(w.wait_built(secs(10)), "{}", w.log());
    assert!(!w.has(&["newdir"]));
    assert!(w.has(&["newdir2", "a"]));

    // the renamed folder is still watched
    write(&p.join("src/newdir2/b.luau"), "return 2");
    assert!(w.wait_built(secs(10)), "{}", w.log());
    assert!(w.has(&["newdir2", "b"]));
    w.assert_matches_fresh_build();
}

#[test]
fn nested_luaurc_edit_matches_startup() {
    let (_g, p) = fixture("nested-luaurc");
    let w = started(&p);
    let job = p.join("src/worker/job.luau");
    let text = read(&job);

    std::fs::write(&job, format!("{text}-- edited\n")).unwrap();
    assert!(w.wait_built(secs(10)), "{}", w.log());

    let job = source(&w, &["worker", "job"]);
    assert!(job.contains("require(\"../pkg/vfx\")"), "{job}");
    assert!(job.contains("-- edited"), "{job}");
    w.assert_no_raw_aliases();
    w.assert_matches_fresh_build();
}

#[test]
fn broken_luaurc_edit_keeps_last_good_build() {
    let (_g, p) = fixture("normal-build");
    let w = started(&p);
    let good = w.out_bytes();
    let luaurc = p.join(".luaurc");
    let original = read(&luaurc);

    std::fs::write(&luaurc, "{").unwrap();
    failed_batch(&w);
    assert_eq!(w.out_bytes(), good);

    std::fs::write(&luaurc, original).unwrap();
    assert!(w.wait_built(secs(10)), "{}", w.log());
    w.assert_no_raw_aliases();
    w.assert_matches_fresh_build();
}

#[test]
fn atomic_save_via_rename_lands_and_temp_never_ships() {
    let (_g, p) = fixture("normal-build");
    let w = started(&p);
    let helpers = p.join("src/mod/helpers.luau");
    let temp = p.join("src/mod/helpers.luau.tmp1234");
    let probe = p.join("src/mod/4913");

    // vim's write probe, then an editor's write-and-rename
    std::fs::write(&probe, "").unwrap();
    std::fs::remove_file(&probe).unwrap();
    std::fs::write(&temp, "return require(\"@pkg/Promise\") -- saved").unwrap();
    std::fs::rename(&temp, &helpers).unwrap();

    one_build(&w);
    assert_eq!(
        source(&w, &["mod", "helpers"]),
        "return require(\"../pkg/Promise\") -- saved"
    );

    let names = format!("{:?}", w.model()[0].shape());
    assert!(!names.contains("helpers.luau"), "{names}");
    assert!(!names.contains("4913"), "{names}");
    w.assert_no_raw_aliases();
    w.assert_matches_fresh_build();
}

#[test]
fn checkout_sized_stream_builds_bounded_and_ends_right() {
    let (_g, p) = fixture("normal-build");
    let w = started(&p);

    for i in 0..40 {
        write(
            &p.join(format!("src/mod/s{i:02}.luau")),
            &format!("return require(\"@pkg/Promise\") -- {i}"),
        );
        std::thread::sleep(ms(75));
    }

    let builds = w.count_built(secs(5));
    assert!((1..=3).contains(&builds), "{builds} builds:\n{}", w.log());
    assert!(w.has(&["mod", "s39"]));
    w.assert_no_raw_aliases();
    w.assert_matches_fresh_build();
}

#[test]
fn mount_root_deleted_and_recreated_is_rewatched() {
    let (_g, p) = fixture("normal-build");
    let w = started(&p);
    let good = w.out_bytes();
    let promise = read(&p.join("pkg/Promise.luau"));

    // a required $path mount: the scope's sourcemap and Rojo both fail, so the last build stays
    std::fs::remove_dir_all(p.join("pkg")).unwrap();
    failed_batch(&w);
    assert_eq!(w.out_bytes(), good);

    write(&p.join("pkg/Promise.luau"), &promise);
    assert!(w.wait_built(secs(10)), "{}", w.log());
    assert!(w.has(&["pkg", "Promise"]));

    write(&p.join("pkg/Promise.luau"), "return { edited = true }");
    assert!(w.wait_built(secs(10)), "{}", w.log());
    assert_eq!(source(&w, &["pkg", "Promise"]), "return { edited = true }");
    w.assert_no_raw_aliases();
    w.assert_matches_fresh_build();
}

#[test]
fn churn_outside_mounts_builds_nothing() {
    let (_g, p) = fixture("normal-build");
    let w = Watcher::spawn(&p, &["-v"]);
    assert!(w.wait_built(secs(10)), "{}", w.log());
    let mark = w.mark();

    for i in 0..25 {
        write(&p.join(format!("junk/f{i}.luau")), "return 1");
        write(&p.join(format!("docs/n{i}.md")), "notes");
    }

    // then one real edit: its build is the only one, and its batch holds only the edit, however
    // late a slow runner delivers the churn
    std::thread::sleep(secs(1));
    write(&p.join("src/mod/helpers.luau"), "return 5");
    one_build(&w);
    assert_eq!(
        summaries(&w, mark),
        ["INFO 1 changed, 0 added, 0 removed"],
        "{}",
        w.log()
    );
    w.assert_matches_fresh_build();
}

#[test]
fn touch_without_a_byte_change_builds_nothing() {
    let (_g, p) = fixture("normal-build");
    let w = Watcher::spawn(&p, &["-v"]);
    assert!(w.wait_built(secs(10)), "{}", w.log());
    let mark = w.mark();
    let helpers = p.join("src/mod/helpers.luau");

    std::fs::write(&helpers, read(&helpers)).unwrap();

    // then one real edit elsewhere: a build for the touch would come first
    std::thread::sleep(secs(1));
    write(&p.join("pkg/Promise.luau"), "return 5");
    one_build(&w);
    assert_eq!(
        summaries(&w, mark),
        ["INFO 1 changed, 0 added, 0 removed"],
        "{}",
        w.log()
    );
    w.assert_matches_fresh_build();
}

#[test]
fn output_inside_a_mount_does_not_loop() {
    let (_g, p) = fixture("normal-build");
    let w = Watcher::spawn_out(&p, &p.join("src/out.rbxm"), &[]);
    assert!(w.wait_built(secs(10)), "{}", w.log());

    assert!(w.count_built(secs(3)) <= 1, "{}", w.log());
    assert!(!w.has(&["out"]), "the output was built into itself");
    w.assert_matches_fresh_build();
}

#[test]
fn two_watchers_on_one_project_both_build() {
    let (_g, p) = fixture("normal-build");
    let a = started(&p);
    let b = Watcher::spawn_out(&p, &a.tmp().join("out-b.rbxm"), &[]);
    assert!(b.wait_built(secs(10)), "{}", b.log());

    assert_eq!(a.tmp(), b.tmp());
    assert_eq!(workspaces_in(a.tmp()).len(), 2, "both workspaces coexist");

    write(&p.join("src/mod/helpers.luau"), "return 7");
    one_build(&a);
    one_build(&b);
    assert_eq!(source(&a, &["mod", "helpers"]), "return 7");
    assert_eq!(source(&b, &["mod", "helpers"]), "return 7");
    assert_eq!(workspaces_in(a.tmp()).len(), 2);
    a.assert_matches_fresh_build();
    b.assert_matches_fresh_build();
}

#[test]
fn verbose_batches_report_their_counts() {
    let (_g, p) = fixture("normal-build");
    let w = Watcher::spawn(&p, &["-v"]);
    assert!(w.wait_built(secs(10)), "{}", w.log());

    write(
        &p.join("src/mod/v.luau"),
        "return require(\"@pkg/Promise\")",
    );
    assert!(w.wait_built(secs(10)), "{}", w.log());
    assert!(
        w.saw("INFO 0 changed, 1 added, 0 removed in "),
        "{}",
        w.log()
    );
    w.assert_matches_fresh_build();
}

#[cfg(unix)]
#[test]
fn sigterm_removes_workspace() {
    let (_g, p) = fixture("normal-build");
    let mut w = started(&p);
    assert_eq!(workspaces_in(w.tmp()).len(), 1);

    let sent = Instant::now();
    let status = w.term();
    assert!(sent.elapsed() < secs(2), "took {:?}", sent.elapsed());
    assert_eq!(status.code(), Some(0));
    assert!(workspaces_in(w.tmp()).is_empty());
}

#[cfg(unix)]
#[test]
fn closed_stdout_exits_and_removes_workspace() {
    let (_g, p) = fixture("normal-build");
    let mut w = started(&p);
    w.close_stdout();

    let edited = Instant::now();
    write(&p.join("src/mod/helpers.luau"), "return 9");
    let status = w.wait_exit(secs(3));
    assert!(
        status.is_some(),
        "still running {:?} after the edit",
        edited.elapsed()
    );
    assert!(workspaces_in(w.tmp()).is_empty());
}

#[cfg(target_os = "linux")]
#[test]
fn idle_cpu_is_near_zero() {
    let (_g, p) = fixture("normal-build");
    let w = started(&p);

    let ticks = || {
        let stat = read(Path::new(&format!("/proc/{}/stat", w.pid())));
        // the fields after the parenthesized name; utime and stime are fields 14 and 15
        let rest = &stat[stat.rfind(')').unwrap() + 2..];
        let fields: Vec<&str> = rest.split_whitespace().collect();
        fields[11].parse::<u64>().unwrap() + fields[12].parse::<u64>().unwrap()
    };

    // a loop that wakes on its own reads (5 per second) costs too few ticks to see, but not too
    // few wakeups
    let wakeups = || {
        let mut total = 0u64;

        for task in std::fs::read_dir(format!("/proc/{}/task", w.pid())).unwrap() {
            let status =
                std::fs::read_to_string(task.unwrap().path().join("status")).unwrap_or_default();

            for line in status.lines() {
                if let Some(n) = line.strip_prefix("voluntary_ctxt_switches:") {
                    total += n.trim().parse::<u64>().unwrap();
                }
            }
        }

        total
    };

    std::thread::sleep(secs(1));
    let (before, woke) = (ticks(), wakeups());
    std::thread::sleep(secs(5));
    let (after, awake) = (ticks(), wakeups());
    assert!(after - before <= 2, "{} ticks in 5 s idle", after - before);
    assert!(awake - woke <= 5, "{} wakeups in 5 s idle", awake - woke);
}

/// A `rojo` whose `build` hangs (a shell running `sleep` as its child, as a rokit shim runs the
/// real Rojo) and whose other commands run the real one. Returns the bin dir to put first on
/// PATH and the file the hanging build writes its pids to.
#[cfg(unix)]
fn hanging_rojo(dir: &Path) -> (std::path::PathBuf, std::path::PathBuf) {
    let pids = dir.join("rojo-pids");
    let build = format!(
        "if [ \"$1\" = build ]; then\n  sleep 60 &\n  echo \"$$ $!\" > '{}'\n  wait\n  exit 1\nfi\n",
        pids.display()
    );

    (fake_rojo(dir, &build), pids)
}

/// A `rojo` that appends its subcommand to the returned file, then runs the real one.
#[cfg(unix)]
fn counting_rojo(dir: &Path) -> (std::path::PathBuf, std::path::PathBuf) {
    let calls = dir.join("rojo-calls");
    let log = format!("echo \"$1\" >> '{}'\n", calls.display());

    (fake_rojo(dir, &log), calls)
}

/// A bin dir holding a `rojo` script that runs `prelude`, then execs the real rojo.
#[cfg(unix)]
fn fake_rojo(dir: &Path, prelude: &str) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;

    let real = std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|d| d.join("rojo"))
        .find(|f| f.is_file())
        .expect("rojo on PATH");
    let bin = dir.join("fakebin");
    std::fs::create_dir_all(&bin).unwrap();

    let script = format!("#!/bin/sh\n{prelude}exec '{}' \"$@\"\n", real.display());
    let fake = bin.join("rojo");
    std::fs::write(&fake, script).unwrap();
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();

    bin
}

#[cfg(unix)]
fn path_with(bin: &Path) -> String {
    let mut dirs = vec![bin.to_path_buf()];
    dirs.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
    std::env::join_paths(dirs)
        .unwrap()
        .to_string_lossy()
        .into_owned()
}

/// The pids the hanging build wrote, once it has written them.
#[cfg(unix)]
fn wait_pids(file: &Path) -> Vec<String> {
    let started = Instant::now();

    loop {
        if let Ok(text) = std::fs::read_to_string(file)
            && text.ends_with('\n')
        {
            return text.split_whitespace().map(String::from).collect();
        }

        assert!(started.elapsed() < secs(10), "the fake rojo never ran");
        std::thread::sleep(ms(20));
    }
}

#[cfg(unix)]
fn assert_gone(pids: &[String]) {
    let started = Instant::now();
    let alive = |pid: &String| {
        std::process::Command::new("kill")
            .arg("-0")
            .arg(pid)
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap()
            .success()
    };

    while pids.iter().any(alive) {
        assert!(started.elapsed() < secs(10), "rojo survived: {pids:?}");
        std::thread::sleep(ms(20));
    }
}

#[cfg(unix)]
#[test]
fn sigterm_during_a_build_kills_rojo() {
    let (g, p) = fixture("normal-build");
    let (bin, pid_file) = hanging_rojo(g.path());
    let path = path_with(&bin);
    let out = common::tmp_of(&p).join("out.rbxm");
    let mut w = Watcher::spawn_with(&p, &out, &[], &[("PATH", &path)]);

    let pids = wait_pids(&pid_file);
    let sent = Instant::now();
    let status = w.term();
    assert!(sent.elapsed() < secs(2), "took {:?}", sent.elapsed());
    assert_eq!(status.code(), Some(0));
    assert_gone(&pids);
    assert!(workspaces_in(w.tmp()).is_empty());
    assert!(!w.saw("Built project to"), "{}", w.log());
}

#[cfg(unix)]
#[test]
fn sigint_during_a_one_shot_build_kills_rojo() {
    // rojo runs in a process group of its own, so a terminal's Ctrl-C reaches only this process,
    // which has to kill rojo itself
    let (g, p) = fixture("normal-build");
    let (bin, pid_file) = hanging_rojo(g.path());
    let tmp = common::tmp_of(&p);
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_rojo-build-noalias"))
        .current_dir(&p)
        .env("PATH", path_with(&bin))
        .env("TMPDIR", &tmp)
        .args(["--color", "never", "-o"])
        .arg(tmp.join("out.rbxm"))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();

    let pids = wait_pids(&pid_file);
    common::watcher::signal(child.id(), "-INT");

    let sent = Instant::now();

    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }

        assert!(sent.elapsed() < secs(5), "the build did not stop on SIGINT");
        std::thread::sleep(ms(20));
    };

    assert_eq!(status.code(), Some(1));
    assert_gone(&pids);
    assert!(workspaces_in(&tmp).is_empty());
    assert!(!tmp.join("out.rbxm").exists());

    let mut text = String::new();
    std::io::Read::read_to_string(&mut child.stdout.take().unwrap(), &mut text).unwrap();
    assert!(text.contains("ERR  interrupted"), "{text}");
    assert!(!text.contains("Built project to"), "{text}");
}

/// The `rojo build` runs the counting rojo has made so far.
#[cfg(unix)]
fn builds_in(calls: &Path) -> usize {
    std::fs::read_to_string(calls)
        .unwrap_or_default()
        .lines()
        .filter(|l| *l == "build")
        .count()
}

#[cfg(unix)]
#[test]
fn a_build_that_keeps_failing_is_retried_only_on_an_edit() {
    // the output dir goes away, so every build fails after diff and apply succeed; v1 and the
    // first cut of this loop rebuilt every 200 ms until it came back
    let (g, p) = fixture("normal-build");
    let (bin, calls) = counting_rojo(g.path());
    let path = path_with(&bin);
    let out_dir = common::tmp_of(&p).join("outdir");
    std::fs::create_dir_all(&out_dir).unwrap();
    let w = Watcher::spawn_with(&p, &out_dir.join("out.rbxm"), &[], &[("PATH", &path)]);
    assert!(w.wait_built(secs(10)), "{}", w.log());

    std::fs::remove_dir_all(&out_dir).unwrap();
    let mark = w.mark();
    let before = builds_in(&calls);

    write(&p.join("src/mod/helpers.luau"), "return 1");
    std::thread::sleep(ms(1500));
    write(&p.join("src/mod/helpers.luau"), "return 2");
    std::thread::sleep(ms(1500));

    let errors = w.lines_since(mark, "ERR ");
    let runs = builds_in(&calls) - before;
    assert!((1..=2).contains(&errors.len()), "{errors:?}\n{}", w.log());
    assert!((1..=2).contains(&runs), "{runs} rojo builds:\n{}", w.log());
    assert_eq!(w.count_built(ms(0)), 0, "{}", w.log());

    // a failed apply owes a rewatch; the rescan that rewatch queues must not retry the build
    let luaurc = p.join(".luaurc");
    let original = read(&luaurc);
    std::fs::write(&luaurc, "{").unwrap();
    assert!(
        w.wait_saw("keeping the last good build", secs(10)),
        "{}",
        w.log()
    );
    std::thread::sleep(ms(500));
    let mark = w.mark();
    let before = builds_in(&calls);
    std::fs::write(&luaurc, original).unwrap();
    std::thread::sleep(secs(3));

    let errors = w.lines_since(mark, "ERR ");
    assert_eq!(errors.len(), 1, "{errors:?}\n{}", w.log());
    assert_eq!(builds_in(&calls) - before, 1, "{}", w.log());

    // still owed: the next edit after the dir comes back lands
    std::fs::create_dir_all(&out_dir).unwrap();
    write(&p.join("src/mod/helpers.luau"), "return 3");
    one_build(&w);
    assert_eq!(source(&w, &["mod", "helpers"]), "return 3");
    w.assert_matches_fresh_build();
}

#[cfg(unix)]
#[test]
fn symlinked_mount_root_is_watched() {
    let (_g, p) = fixture("normal-build");
    std::fs::rename(p.join("pkg"), p.join("real_pkg")).unwrap();
    std::os::unix::fs::symlink("real_pkg", p.join("pkg")).unwrap();
    let w = started(&p);
    assert!(w.has(&["pkg", "Promise"]));

    write(
        &p.join("real_pkg/Promise.luau"),
        "return { through = true }",
    );
    one_build(&w);
    assert_eq!(source(&w, &["pkg", "Promise"]), "return { through = true }");

    write(&p.join("pkg/Added.luau"), "return 1");
    one_build(&w);
    assert!(w.has(&["pkg", "Added"]));
    w.assert_no_raw_aliases();
    w.assert_matches_fresh_build();
}
