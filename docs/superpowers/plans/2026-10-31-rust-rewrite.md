# rojo-build-noalias 2.0 Rust Rewrite Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the Luau tool with a Rust binary that keeps v1.2.4's command line and one-shot models, and whose `--watch` rebuilds once per batch of saves within 400 ms, handles every kind of file change, never ships a raw alias and costs nothing idle.

**Architecture:** A lib crate (`rojo_build_noalias`) plus a thin bin. A per-run temp workspace mirrors only the files the project mounts; script files are de-aliased in memory through the same deepest-first scope chain at startup and in watch mode, then `rojo build` runs once per batch into the workspace and the result is renamed over the output. Watch mode turns native file events into path hints, closes a batch after a 200 ms global quiet window, diffs the hinted paths against a snapshot and applies only what changed.

**Tech Stack:** Rust 1.94.1 (edition 2024), clap 4.6, notify 8.2, regex 1.13, serde_json 1.0, jsonc-parser 0.34, tempfile 3.27, walkdir 2.5, ctrlc 3.5, anyhow 1.0, dunce 1.0; tests with assert_cmd 2.2, rbx_binary 3.0, rbx_dom_weak 4.2; Rojo 7.6.1 as an external binary.

**Spec:** `docs/superpowers/specs/2026-10-31-rust-rewrite-design.md` (read sections 4 to 7 before any task; section 6 is the behavior reference with v1.2.4 line numbers).

## Global Constraints

- ASCII only in code, comments, docs and commit messages: no em or en dashes, no arrows, no ellipsis characters.
- This repo's commits: `fix: ...`, `test: ...`, `docs: ...` where one fits, otherwise a plain lowercase imperative (`add an orphan processing mode`); `version 2.0.0` for the bump. A body explains why when it is not obvious. No trailers, never an issue or PR number. Stage explicit paths, never `git add -A` or `commit -a`. Never push, never tag: the owner tags `v2.0.0` and pushes.
- vfx-forge (Task 10 only): plain lowercase imperative subjects, no prefix, no trailers, never push. Before Task 10, vfx-forge is only ever read (Task 6 copies it to a snapshot); never run its `dev.sh`.
- Toolchain: `rust-toolchain.toml` channel `1.94.1`; `Cargo.toml` `edition = "2024"`, `rust-version = "1.89"`; `Cargo.lock` committed; Rojo `7.6.1` from `rokit.toml`.
- Crate versions exactly as the Tech Stack line; `serde_json` with `preserve_order`, `jsonc-parser` with `serde`, `ctrlc` with `termination`. No `notify-debouncer-*`, no `rojo` crate, no `libc`.
- Log prefixes: `INFO `, `WARN `, `OK   `, `ERR  `, `ROJO ` (the prefix word padded to 4, then one space). Everything prints to stdout.
- The marker: exactly one line `OK   Built project to <OUTPUT as given>` per landed build, printed after the output is in place; Rojo's own `Built project to` line is never forwarded.
- Exit codes: 0 success or signal in watch mode, 1 failed build or conversion, 2 usage error.
- Workspace: `tempfile` dir with prefix `rojo-build-noalias.` under `std::env::temp_dir()`, holding `tree/`, `out/`, `lock`; skip names `.git`, `node_modules`, `.luau-lsp`, `.vscode`.
- Watch: quiet window `200 ms`, max batch age `2 s`; no polling, no timeouts while no batch is open.
- Targets on vfx-forge `dev.project.json`: save-to-marker under 400 ms, startup under 3 s, idle CPU under 50 ms per minute.
- Tests read the version from `env!("CARGO_PKG_VERSION")`, never a literal; `Cargo.toml` carries `2.0.0-dev` until Task 9.
- Keep the Luau tool (`src/*.luau`, `tests/init.luau`) in place until Task 9 deletes it; the Rust tree lives beside it (`src/*.rs`).

## Review Focus

- Editor atomic saves (write `x.luau.tmp`, rename over `x.luau`; vim's `4913` probe file): one build with the new bytes, the temp name never in the model. Test: Task 8 `atomic_save_via_rename_lands_and_temp_never_ships`.
- A branch switch or formatter run rewriting hundreds of files for longer than 2 s: a bounded number of builds (at most one per 2 s of stream plus one), the final model equal to a fresh build, no crash. Test: Task 8 `checkout_sized_stream_builds_bounded_and_ends_right`.
- A mount root or an alias's target folder deleted and recreated: the delete reaches the model (the alias warns and stays raw), the recreated tree is watched again and later edits inside it land. Tests: Task 7 `mount_root_deleted_and_recreated`, Task 8 `mount_root_deleted_and_recreated_is_rewatched`.
- Two runs on one project at once (dev.sh's watcher, labs.toml's one-shot, build.sh): disjoint workspaces, neither wipes the other, the stale sweep spares a live run. Tests: Task 5 `concurrent_runs_do_not_share_workspaces`, `stale_workspace_is_swept_live_one_kept`; Task 8 `two_watchers_on_one_project_both_build`.
- The output file inside a watched mount (`-o src/out.rbxm`): no rebuild loop. Test: Task 8 `output_inside_a_mount_does_not_loop`.

---

## File Structure

| Path | Task | Responsibility |
|---|---|---|
| `Cargo.toml`, `Cargo.lock`, `rust-toolchain.toml` | 1 | crate, deps, toolchain |
| `src/main.rs` | 1, 3, 5, 8 | parse, dispatch, exit code |
| `src/lib.rs` | 1 | module list |
| `src/cli.rs` | 1 | clap args, help, target and DIR resolution, `Mode` |
| `src/log.rs` | 1 | prefixes, colors, EPIPE |
| `src/shutdown.rs` | 1, 8 | one shutdown request from signals or EPIPE |
| `src/paths.rs` | 2 | normalize, skip names, predicates, `/` display |
| `src/luaurc.rs` | 2 | strict-JSON aliases |
| `src/sourcemap.rs` | 2, 3 | node types, folder derivation, node index; `generate` in 3 |
| `src/dealias.rs` | 2 | matcher, context, relative path, `@self`, project and orphan rules |
| `src/workspace.rs` | 3, 5 | temp dir, lock, sweep, copy, mirror, snapshot, write-if-changed |
| `src/rojo.rs` | 3, 5 | sourcemap and build invocations, line forwarding |
| `src/project.rs` | 3, 4, 5 | lookup, JSONC read, root `$path`, flatten, mount closure |
| `src/scopes.rs` | 3 | discovery, order, scope chain |
| `src/report.rs` | 3 | the startup replacement table |
| `src/convert.rs` | 3, 4 | convert mode |
| `src/output.rs` | 5 | atomic swap |
| `src/pipeline.rs` | 5, 7 | build mode state, start, build, diff, apply |
| `src/diff.rs` | 7 | hint diff into a `ChangeSet` |
| `src/watch/mod.rs`, `src/watch/batcher.rs` | 8 | notify loop, quiet window |
| `tests/common/mod.rs`, `tests/common/model.rs`, `tests/common/watcher.rs` | 1, 4, 8 | fixture copies, binary runs, model reading, watcher harness |
| `tests/*.rs` | per task | integration tests |
| `tests/fixtures/nested-luaurc/`, `tests/fixtures/named-project/` | 3 | new fixtures |
| `examples/rbxm_diff.rs`, `scripts/snapshot-vfx-forge.sh`, `scripts/parity.sh`, `scripts/watch-latency.sh` | 6, 8 | parity and timing tools (dev only) |
| `.github/workflows/build.yml`, `README.md`, `rokit.toml` | 9 | release |

---

### Task 1: Crate skeleton and the v1 command line

**Files:**
- Create: `Cargo.toml`, `rust-toolchain.toml`, `src/main.rs`, `src/lib.rs`, `src/cli.rs`, `src/log.rs`, `src/shutdown.rs`, `tests/common/mod.rs`, `tests/cli.rs`
- Modify: `.gitignore` (add `/target`)

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `cli::ColorChoice { Auto, Always, Never }`
  - `cli::Options { process_orphans: bool, wax_compat: bool, verbose: u8, color: ColorChoice }`
  - `cli::Target { dir: PathBuf, file_name: Option<String> }` with `fn project_in(&self, root: &Path) -> PathBuf` (root joined with `file_name`, or root)
  - `cli::OutputTarget { File(PathBuf), Plugin(String) }`
  - `cli::BuildArgs { target: Target, output: OutputTarget, watch: bool, opts: Options }`
  - `cli::ConvertArgs { target: Target, out_dir: PathBuf, opts: Options, watch_ignored: bool }`
  - `cli::Mode { Help, Version, Build(BuildArgs), Convert(ConvertArgs) }`
  - `cli::CliError { Usage(String), Refused(String) }` (exit 2 and exit 1)
  - `cli::parse(args: Vec<OsString>, cwd: &Path) -> Result<Mode, CliError>`
  - `cli::resolve_target(arg: Option<&Path>, cwd: &Path) -> Target`
  - `cli::help_text(color: bool) -> String`
  - `log::init(choice: ColorChoice)`, `log::{info, warn, success, error, rojo}(msg: &str)`, `log::verbose() -> u8`, `log::set_verbose(n: u8)`
  - `shutdown::request()`, `shutdown::requested() -> bool`, `shutdown::on_request(f: impl Fn() + Send + Sync + 'static)`
  - tests: `common::rbn() -> assert_cmd::Command`, `common::fixture(name: &str) -> (tempfile::TempDir, PathBuf)` (copy of `tests/fixtures/<name>`, returns the guard and the project dir), `common::text(out: &std::process::Output) -> String` (stdout then stderr)

- [ ] **Step 1: Write the failing tests**

`tests/cli.rs`:

```rust
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
    let out = rbn().arg("--version").output().unwrap();
    assert_eq!(String::from_utf8(out.stdout).unwrap(), format!("rojo-build-noalias {}\n", env!("CARGO_PKG_VERSION")));
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
    assert_eq!(rbn().args(["--frobnicate", "-o", "x.rbxm"]).output().unwrap().status.code(), Some(2));
    assert_eq!(rbn().arg("--watch").output().unwrap().status.code(), Some(2)); // neither -o nor -p
}
```

Unit tests in `src/cli.rs` (`#[cfg(test)]`, against a temp dir as cwd):
- `resolve_target` with an existing `dev.project.json` file: `dir` = its parent, `file_name` = `Some("dev.project.json")`; an existing dir: that dir, `None`; a missing path: that path as `dir`; `None`: cwd; `"proj///"`: trailing slashes gone.
- `parse(["dev.project.json", "--output", "x.rbxm"])` gives `Build` with `OutputTarget::File("x.rbxm")` and the target above (flags after the positional, labs.toml's form).
- `parse(["-v", "-v", "-o", "x.rbxm"])` gives `verbose == 2`; `["--color", "never", ...]` gives `ColorChoice::Never`.
- `parse(["--convert-to", "./out/"])` gives `out_dir == cwd.join("out")`; `["--convert-to", "."]` and `["--convert-to", ".."]` give `CliError::Refused` containing `refusing to convert into the project directory or one of its parents`; `["--convert-to", "out", "--watch"]` gives `watch_ignored == true`.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test`
Expected: compile errors (`cli` module missing), then after empty stubs, FAIL on every test.

- [ ] **Step 3: Implement**

`Cargo.toml`: package `rojo-build-noalias`, version `2.0.0-dev`, lib name `rojo_build_noalias`, bin `rojo-build-noalias` at `src/main.rs`; deps and dev-deps per the Global Constraints (rbx crates wait for Task 4). `[profile.release]` `lto = true`, `codegen-units = 1`, `strip = true`.

`cli`: clap derive with `disable_help_flag = true`, `disable_version_flag = true`, explicit `-h/--help` and `-V/--version` bools, one optional positional `PROJECT`. Empty argv is `Mode::Help`. `-h` anywhere wins. `--plugin` on Linux is `Refused("The '--plugin' option is not supported on Linux.")`. Without `--convert-to`, one of `-o`/`-p` is required (`Usage`). DIR resolution: `cwd.join(dir)` then `paths`-style lexical normalization (write a private `normalize` here; Task 2 moves it to `paths`).

`help_text`: the v1.2.4 text from `src/init.luau:11-35` verbatim, dedented, plus `-V, --version   Print version information` under ROJO OPTIONS' sibling list, with the first line `rojo-build-noalias <version>`. Coloring rules from `util.luau:88-91`: section headers `[A-Z ]+:` yellow, `<UPPER>` placeholders green, short and long flags green, the name green.

`log`: a global `OnceLock` holding the color decision (`Auto` = stdout is a terminal). Each function writes `<prefix> <msg>\n` to stdout; prefix colors from `log.luau` (INFO blue, WARN yellow, OK green, ERR red, ROJO purple). A `BrokenPipe` write error calls `shutdown::request()` and is otherwise swallowed.

`shutdown`: an `AtomicBool` plus a `Mutex<Vec<Box<dyn Fn() + Send + Sync>>>` of callbacks run once on the first `request()`.

`main`: `Help` prints `help_text`, `Version` prints the line, `Refused` prints `ERR` and exits 1, `Usage` prints clap's message to stderr and exits 2; `Build` and `Convert` print `ERR  not implemented yet` and exit 1 (replaced in Tasks 3 and 5).

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test`
Expected: all tests PASS, `cargo clippy --all-targets -- -D warnings` clean.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml Cargo.lock rust-toolchain.toml .gitignore src/main.rs src/lib.rs src/cli.rs src/log.rs src/shutdown.rs tests/common/mod.rs tests/cli.rs
git commit -m "add the rust crate with the v1 command line"
```

---

### Task 2: The de-alias core

Pure functions only; no process, no file system beyond what a closure answers.

**Files:**
- Create: `src/paths.rs`, `src/luaurc.rs`, `src/sourcemap.rs`, `src/dealias.rs`
- Modify: `src/lib.rs`, `src/cli.rs` (use `paths::normalize`)

**Interfaces:**
- Consumes: nothing beyond Task 1's `log`.
- Produces:
  - `paths::SKIP_NAMES: [&str; 4]`, `paths::normalize(p: &Path) -> PathBuf` (lexical: drops `.`, folds `..`, no fs access), `paths::is_script(p: &Path) -> bool` (`.lua`/`.luau`), `paths::is_init(p: &Path) -> bool` (`init.lua`/`init.luau`), `paths::skipped(rel: &Path) -> bool` (any component in `SKIP_NAMES`), `paths::slash(p: &Path) -> String` (components joined by `/`)
  - `luaurc::parse_aliases(text: &str) -> anyhow::Result<BTreeMap<String, String>>` (strict `serde_json`; missing `aliases` is an empty map)
  - `sourcemap::Node { name: String, class_name: String, file_paths: Vec<PathBuf>, children: Vec<Node> }` (serde camelCase, both vectors default empty), `sourcemap::parse(json: &str) -> anyhow::Result<Node>`
  - `sourcemap::folder_path(node: &Node) -> Option<PathBuf>`
  - `sourcemap::NodeIndex { root_name: String, paths: BTreeMap<PathBuf, Vec<String>> }` (absolute path to instance path)
  - `sourcemap::index(sm: &Node, root_path: Option<&Path>, exists: &dyn Fn(&Path) -> bool) -> NodeIndex`
  - `dealias::Replacement { label: String, from: String, to: String }`
  - `dealias::matcher<'a>(aliases: impl IntoIterator<Item = &'a str>) -> Option<regex::bytes::Regex>`
  - `dealias::context_path(file: &Path, instance: &[String], wax: bool) -> Vec<String>`
  - `dealias::relative_path(from: &[String], to: &[String]) -> Vec<String>`
  - `dealias::to_self_path(rel: &str, root_name: &str) -> String`
  - `dealias::resolve_aliases(aliases: &BTreeMap<String, String>, scope_dir: &Path, index: &NodeIndex, luaurc: &Path) -> BTreeMap<String, Vec<String>>` (alias to target instance path; logs the two v1.2.4 warnings per miss)
  - `dealias::ProjectRules::new(root_name: String, targets: BTreeMap<String, Vec<String>>) -> ProjectRules`, `.rewrite(&self, src: &[u8], context: &[String], label: &str) -> (Vec<u8>, Vec<Replacement>)`
  - `dealias::OrphanRules::new(aliases: BTreeMap<String, String>) -> OrphanRules`, `.rewrite(&self, src: &[u8], label: &str) -> (Vec<u8>, Vec<Replacement>)`

- [ ] **Step 1: Write the failing unit tests**

In `src/dealias.rs`:

```rust
fn v(xs: &[&str]) -> Vec<String> { xs.iter().map(|s| s.to_string()).collect() }

#[test]
fn relative_paths() {
    assert_eq!(relative_path(&v(&["R", "mod", "helpers"]), &v(&["R", "utils", "config"])), v(&["..", "utils", "config"]));
    assert_eq!(relative_path(&v(&[]), &v(&["R"])), v(&[".", "R"]));
    assert_eq!(relative_path(&v(&["R", "a"]), &v(&["R", "a", "b"])), v(&[".", "b"]));
    assert_eq!(relative_path(&v(&["R", "lib", "Foo"]), &v(&["R", "pkg", "Logger"])), v(&["..", "pkg", "Logger"]));
    assert_eq!(relative_path(&v(&["R", "lib", "Foo", ""]), &v(&["R", "pkg", "Logger"])), v(&["..", "..", "pkg", "Logger"]));
}

#[test]
fn self_paths() {
    assert_eq!(to_self_path("./R/mod/helpers", "R"), "@self/mod/helpers");
    assert_eq!(to_self_path("../../R/x", "R"), "@self/x");
    assert_eq!(to_self_path("../R2/x", "R"), "../R2/x");
    assert_eq!(to_self_path("../mod/R/x", "R"), "../mod/R/x");
}

#[test]
fn context_paths() {
    let root = v(&["R"]);
    let nested = v(&["R", "lib", "Foo"]);
    assert_eq!(context_path(Path::new("/p/src/init.luau"), &root, false), v(&[]));
    assert_eq!(context_path(Path::new("/p/src/lib/Foo/init.luau"), &nested, false), nested);
    assert_eq!(context_path(Path::new("/p/src/lib/Foo/init.lua"), &nested, true), v(&["R", "lib", "Foo", ""]));
    assert_eq!(context_path(Path::new("/p/src/lib/Foo/Bar.luau"), &v(&["R", "lib", "Foo", "Bar"]), true), v(&["R", "lib", "Foo", "Bar"]));
}

#[test]
fn matcher_reach() {
    let m = matcher(["vfx", "vfx-pkg"]).unwrap();
    let alias = |s: &str| m.captures(s.as_bytes()).map(|c| String::from_utf8(c[1].to_vec()).unwrap());
    assert_eq!(alias(r#"require("@vfx-pkg/Z")"#).as_deref(), Some("vfx-pkg"));
    assert_eq!(alias(r#"require('@vfx")"#).as_deref(), Some("vfx"));
    assert_eq!(alias(r#"-- require("@vfx/x")"#).as_deref(), Some("vfx"));
    assert_eq!(alias(r#"require ("@vfx")"#), None);
    assert_eq!(alias(r#"require("@vfxx")"#), None);
    assert_eq!(alias(r#"require("@vfx/")"#), None);
    assert!(matcher(std::iter::empty::<&str>()).is_none());
}

#[test]
fn project_rewrite_matches_basic_alias_fixture() {
    let targets = BTreeMap::from([
        ("mod".to_string(), v(&["BasicAlias", "mod"])),
        ("utils".to_string(), v(&["BasicAlias", "utils"])),
    ]);
    let rules = ProjectRules::new("BasicAlias".into(), targets);
    let (root, rows) = rules.rewrite(b"local h = require(\"@mod/helpers\")\nlocal c = require('@utils/config')", &v(&[]), "BasicAlias");
    assert_eq!(String::from_utf8(root).unwrap(), "local h = require(\"@self/mod/helpers\")\nlocal c = require(\"@self/utils/config\")");
    assert_eq!(rows[0].from, r#"require("@mod/helpers")"#);
    let (h, _) = rules.rewrite(b"require(\"@utils/config\")", &v(&["BasicAlias", "mod", "helpers"]), "BasicAlias/mod/helpers");
    assert_eq!(h, b"require(\"../utils/config\")");
}

#[test]
fn orphan_rewrite_is_literal() {
    let rules = OrphanRules::new(BTreeMap::from([
        ("packages".to_string(), "./packages".to_string()),
        ("vfx".to_string(), "../BundledAssets/ForgeVFX".to_string()),
    ]));
    let (out, _) = rules.rewrite(b"require(\"@packages/Promise\") require('@vfx')", "src/main.luau");
    assert_eq!(out, b"require(\"./packages/Promise\") require(\"../BundledAssets/ForgeVFX\")");
}
```

In `src/sourcemap.rs`, `folder_path_cases` over inline JSON nodes (absolute paths under `/p`):
- folder `pkg` whose children are the nested project node `encoder` (`filePaths` `["/p/pkg/Encoder/src/init.luau", "/p/pkg/Encoder/default.project.json"]`) then `Promise` (`["/p/pkg/Promise.luau"]`): `Some("/p/pkg")`.
- folder `pkg` holding only that nested project node: `Some("/p/pkg")`.
- folder `lib` whose child `Foo` has `["/p/src/lib/Foo/init.luau"]`: `Some("/p/src/lib")`.
- folder `A` holding folder `B` holding `c` (`["/p/A/B/c.luau"]`): `Some("/p/A")`.
- folder with no children: `None`.

And `index_registers_roots_and_inits`: the nested-init sourcemap (copy the JSON measured in this plan's research: root `NestedInit` with `filePaths ["/p/src/init.luau", "/p/default.project.json"]`, `lib` folder, `Foo` module with child `Bar`, `pkg` folder with `Logger`) and `root_path = Some("/p/src")`, `exists` true only for `/p/src/init.luau`: `paths["/p/src"] == ["NestedInit"]`, `paths["/p/src/init.luau"] == ["NestedInit"]`, `paths["/p/src/lib"] == ["NestedInit", "lib"]`, `paths["/p/src/lib/Foo/init.luau"] == ["NestedInit", "lib", "Foo"]`, `paths["/p/src/pkg/Logger.luau"] == ["NestedInit", "pkg", "Logger"]`.

In `src/paths.rs`: `slash(Path::new("a").join("My Folder").join("b.luau")) == "a/My Folder/b.luau"`; `normalize("/p/./src/../src/mod/") == "/p/src/mod"`; `skipped("pkg/x/.git/HEAD")` true, `skipped("pkg/gitx/a")` false.

In `src/luaurc.rs`: valid map parses; `{}` gives empty; `{"aliases": {"a": "./a"},}` (trailing comma) is an error.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib`
Expected: FAIL (functions missing).

- [ ] **Step 3: Implement the four modules**

Port, do not redesign: `folder_path` is `util.tryGetFolderPath` (`util.luau:205-274`) line for line, with segments as path components and the same depth bookkeeping; `index` is `replaceAliases.luau:229-340` (first `filePaths` entry only; folder plus init file; then the root `$path` and its init with instance path `[root_name]`; a folder with no path logs `Folder '<name>' has no files, skipping`); `relative_path` is `util.getRelativePath`; `to_self_path` is `convertToSelfPath`; `context_path` is `replaceAliases.luau:401-412`.

`resolve_aliases`: for each alias, key = `normalize(scope_dir.join(value))` (divergence 6 of the spec), looked up as is, then `/init.luau`, then `/init.lua`. On a miss log `Alias '<a>' with path '<value>' in <luaurc> was not found in the sourcemap.` and `Looking for: <key>`.

The matcher, the one algorithm the tests leave open:

```rust
// longest first so an alias that prefixes another never shadows it
let mut names: Vec<&str> = aliases.into_iter().collect();
names.sort_by(|a, b| b.len().cmp(&a.len()).then(a.cmp(b)));
let alt = names.iter().map(|n| regex::escape(n)).collect::<Vec<_>>().join("|");
Regex::new(&format!(r#"require\(["']@({alt})(?:["']\)|/([^"']+)["']\))"#))
```

`ProjectRules::rewrite` uses `replace_all` with a closure: group 2 absent means `relative_path(context, target)` joined with `/`, else that joined with `/` plus the subpath; both through `to_self_path`; emitted as `require("<path>")`. Each match pushes a `Replacement { label, from: require("@a") or require("@a/sub"), to }`. Work on bytes (`regex::bytes`) so a non-UTF-8 file passes through untouched outside its matches.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib`
Expected: PASS; `cargo clippy --all-targets -- -D warnings` clean.

- [ ] **Step 5: Commit**

```bash
git add src/lib.rs src/cli.rs src/paths.rs src/luaurc.rs src/sourcemap.rs src/dealias.rs
git commit -m "port alias resolution and the relative require rules"
```

---

### Task 3: Convert mode end to end

**Files:**
- Create: `src/workspace.rs`, `src/rojo.rs`, `src/project.rs`, `src/scopes.rs`, `src/report.rs`, `src/convert.rs`, `tests/alias_resolution.rs`, `tests/orphans.rs`, `tests/convert_safety.rs`
- Create fixtures: `tests/fixtures/nested-luaurc/`, `tests/fixtures/named-project/` (contents below)
- Modify: `src/lib.rs`, `src/main.rs` (dispatch `Mode::Convert`), `src/sourcemap.rs` (add `generate`)

**Interfaces:**
- Consumes: Task 1 `cli::{ConvertArgs, Options, Target}`, `log`; Task 2 everything.
- Produces:
  - `workspace::Workspace::create() -> anyhow::Result<Workspace>`, `Workspace::create_in(temp_root: &Path) -> anyhow::Result<Workspace>`, `.root() -> &Path`, `.tree() -> PathBuf`, `.out() -> PathBuf`
  - `workspace::sweep_stale(temp_root: &Path) -> usize`
  - `workspace::copy_tree(from: &Path, to: &Path, exclude: Option<&Path>) -> anyhow::Result<()>`
  - `rojo::Rojo { cwd: PathBuf, color: ColorChoice, verbose: u8 }`, `Rojo::sourcemap(&self, project: &Path) -> anyhow::Result<sourcemap::Node>` (runs `rojo sourcemap --absolute <project>` in `cwd`; on failure the error carries Rojo's exit code and stderr, as `replaceAliases.luau:209-213`)
  - `project::find_for_luaurc(dir: &Path, preferred: Option<&str>) -> Option<PathBuf>`
  - `project::read(path: &Path) -> anyhow::Result<serde_json::Value>` (JSONC)
  - `project::root_path(project: &serde_json::Value) -> Option<String>` (`$path` string or `{"optional": s}`)
  - `scopes::Rules { Project(dealias::ProjectRules), Orphan(dealias::OrphanRules) }`
  - `scopes::Scope { luaurc: PathBuf, dir: PathBuf, project: Option<PathBuf>, rules: Rules, files: BTreeMap<PathBuf, Vec<String>> }` (project scopes: covered script path to instance path; orphan scopes: empty, coverage is "script under `dir`")
  - `scopes::discover_all(tree: &Path, exclude: Option<&Path>) -> Vec<PathBuf>`
  - `scopes::order(luaurcs: &mut [PathBuf])` (component count descending, then path ascending)
  - `scopes::ScopeSet { scopes: Vec<Scope>, wax: bool }`
  - `ScopeSet::build(luaurcs: &[PathBuf], lookup: &dyn Fn(&Path) -> Option<PathBuf>, opts: &Options, rojo: &Rojo) -> ScopeSet` (skips and warns per spec 6.1 and 6.2, never fails)
  - `ScopeSet::covers(&self, file: &Path) -> bool`
  - `ScopeSet::rewrite(&self, file: &Path, src: &[u8]) -> (Vec<u8>, Vec<(usize, dealias::Replacement)>)` (scope index, in chain order)
  - `report::ProjectLog { title: String, rows: Vec<dealias::Replacement> }`, `report::print(logs: &[ProjectLog], elapsed: std::time::Duration)`
  - `convert::run(args: &cli::ConvertArgs) -> anyhow::Result<()>`
  - tests: `common::read(p: &Path) -> String`

New fixtures (each also gets the same `rokit.toml` as the others, `[tools]` `rojo = "rojo-rbx/rojo@7.6.1"`):

`nested-luaurc` (modeled on vfx-forge's `src/mod/emitter/worker`):
- `.luaurc`: `{"aliases": {"mod": "./src/mod", "vfx": "./pkg/vfx"}}`
- `default.project.json`: `{"name": "NestedLuaurc", "tree": {"$path": "src", "pkg": {"$path": "pkg"}}}`
- `pkg/vfx/init.luau`: `return {}`
- `src/init.luau`: `local helpers = require("@mod/helpers")` newline `return {}`
- `src/mod/helpers.luau`: `return {}`
- `src/worker/.luaurc`: `{"aliases": {"vfx": "../BundledAssets/ForgeVFX"}}`
- `src/worker/job.luau`: `local vfx = require("@vfx")` newline `local helpers = require("@mod/helpers")` newline `return {}`

`named-project` (the f0a7e46 case):
- `.luaurc`: `{"aliases": {"mod": "./src/mod"}}`
- `default.project.json`: `{"name": "Named", "globIgnorePaths": ["**/*.story.luau"], "tree": {"$path": "src"}}`
- `dev.project.json`: `{"name": "Named", "tree": {"$path": "src"}}`
- `src/init.luau`: `local helpers = require("@mod/helpers")` newline `return {}`
- `src/mod/helpers.luau`: `return {}`
- `src/stories/button.story.luau`: `local helpers = require("@mod/helpers")` newline `return {}`

- [ ] **Step 1: Write the failing tests**

Port each Lua test from `tests/init.luau` with the same name in snake case, the same fixture and the same `assertContains`/`assertNotContains` strings. The run is `rbn().current_dir(&project).args(["--convert-to", out])` (plus `--wax-compat` or `--process-orphans` where the Lua test had it), asserting `status.success()`.

`tests/alias_resolution.rs`: `basic_alias_requires_become_relative` (`init.luau:155-174`), `wax_compat_adds_extra_dotdot_to_nested_init` (`:176-190`), `nested_init_follows_abstract_module_paths` (`:192-213`), `folder_alias_resolves_when_nested_project_is_first_child` (`:215-237`), `self_alias_resolves` (`:239-256`). Note `basic-alias` and the others root at `src` with no outside mounts, so flattening (Task 4) does not move their files.

`tests/orphans.rs`: `process_orphans_replaces_text` (`:400-416`), `orphans_skipped_without_flag` (`:418-430`), plus:

```rust
#[test]
fn nested_orphan_scope_falls_to_the_root_scope_without_the_flag() {
    let (_g, p) = fixture("nested-luaurc");
    let out = p.join("../out");
    assert!(rbn().current_dir(&p).args(["--convert-to", out.to_str().unwrap()]).output().unwrap().status.success());
    let job = read(&out.join("src/worker/job.luau"));
    assert!(job.contains(r#"require("../pkg/vfx")"#), "{job}");
    assert!(job.contains(r#"require("../mod/helpers")"#), "{job}");
}

#[test]
fn nested_orphan_scope_runs_first_with_the_flag() {
    // same, with "--process-orphans"
    // job contains require("../BundledAssets/ForgeVFX") and require("../mod/helpers")
}
```

(`nested-luaurc` mounts `pkg` outside `src`, so after Task 4 the converted `pkg` sits at `src/pkg`; these two tests read only `src/worker/job.luau`, which does not move.)

`tests/convert_safety.rs`:
- `named_project_converts_against_cli_project`: convert `dev.project.json`: `src/stories/button.story.luau` contains `require("../mod/helpers")`; convert with no project argument: it still contains `require("@mod/helpers")`.
- `convert_into_project_dir_is_refused`: `--convert-to .` exits 1 and `default.project.json` and `src/init.luau` still exist.
- `convert_inside_project_does_not_nest_previous_output`: run `--convert-to out` twice in the `basic-alias` copy: `out/src/init.luau` exists and `out/out` does not.

Unit tests in `src/workspace.rs`: `create_then_drop_removes_the_dir`; `sweep_removes_unlocked_and_keeps_locked` (two `rojo-build-noalias.*` dirs with a `lock` file, one held by `File::lock` in the test); `copy_tree_skips_names` (`.git`, `node_modules`, `.luau-lsp`, `.vscode` at depth 2 not copied); `copy_tree_survives_a_symlink_loop` (`#[cfg(unix)]`, `a/loop -> ..`, copy finishes). In `src/scopes.rs`: `order_is_deepest_first_then_by_path`; `find_for_luaurc` prefers `preferred`, then the four fixed names in order, then the first `*.project.json(c)` by name.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test`
Expected: the new integration tests FAIL (`ERR  not implemented yet`), unit tests fail to compile.

- [ ] **Step 3: Implement**

`Workspace::create_in`: `tempfile::Builder::new().prefix("rojo-build-noalias.").tempdir_in(dunce::canonicalize(temp_root)?)`, create `tree/` and `out/`, open `lock` and hold `try_lock()` for the struct's lifetime. `create()` calls `sweep_stale(temp_dir())` then `create_in(temp_dir())`. `sweep_stale` removes each `rojo-build-noalias.*` dir whose `lock` it can `try_lock`, skips the rest.

`copy_tree`: `walkdir` with `follow_links(true)`, `filter_entry` dropping `SKIP_NAMES` and `exclude`, loop errors and unreadable entries skipped (as `util.copyDir`).

`convert::run` follows `init.luau:201-388`: workspace dir is DIR itself when DIR is outside the project, else `ws.tree()` then copied to DIR; wipe DIR first; `copy_tree(project, work, exclude = DIR if inside)`; `discover_all(work, exclude)`; `order`; `ScopeSet::build` with `lookup = |l| project::find_for_luaurc(l.parent(), preferred)` where `preferred` is the CLI file name only for the `.luaurc` in the project root; the `Rojo` cwd is the original project dir. Then rewrite every covered script under `work` through `ScopeSet::rewrite` and write the ones that changed; collect rows per scope into `report::ProjectLog`s (title `Processed project: <original path>`, orphans `... (orphan)`), print the report, then `OK   Project converted successfully to <DIR>`. `watch_ignored` prints `WARN --watch is ignored with --convert-to`. `main` maps `Err` to `ERR  <error chain>` and exit 1.

`report::print` follows `init.luau:301-339` (common padding widths across all projects, rows sorted by `from` length, then label, then position), then `INFO Done in <ms>ms` and a blank line.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test`
Expected: PASS. Then compare against the Luau tool by hand on one fixture: `lune run src/init.luau -- --convert-to /tmp/rbn-lua-out` and `cargo run -- --convert-to /tmp/rbn-rs-out`, both in a copy of `tests/fixtures/self-alias`; `diff -r /tmp/rbn-lua-out /tmp/rbn-rs-out` prints nothing.

- [ ] **Step 5: Commit**

```bash
git add src/lib.rs src/main.rs src/sourcemap.rs src/workspace.rs src/rojo.rs src/project.rs src/scopes.rs src/report.rs src/convert.rs tests/common/mod.rs tests/alias_resolution.rs tests/orphans.rs tests/convert_safety.rs tests/fixtures/nested-luaurc tests/fixtures/named-project
git commit -m "convert projects with the rust pipeline"
```

---

### Task 4: Flattening and the converted structure

**Files:**
- Create: `tests/common/model.rs`, `tests/flattening.rs`, `tests/convert_structure.rs`, `tests/forge_vfx.rs`
- Modify: `src/project.rs` (add `flatten_tree_paths`), `src/convert.rs` (call it), `Cargo.toml` (dev-deps `rbx_binary = "3.0"`, `rbx_dom_weak = "4.2"`)

**Interfaces:**
- Consumes: Task 3 `convert::run`, `project::read`, `workspace::copy_tree`.
- Produces:
  - `project::flatten_tree_paths(work: &Path, project_path: &Path) -> anyhow::Result<bool>` (true when the file was rewritten)
  - tests: `model::Inst { name: String, class: String, source: Option<String>, children: Vec<Inst> }`, `model::load(path: &Path) -> Vec<Inst>` (the model's roots), `Inst::find(&self, path: &[&str]) -> Option<&Inst>`, `model::sources(roots: &[Inst]) -> Vec<(String, String)>` (`/`-joined instance path, Source)

- [ ] **Step 1: Write the failing tests**

Ported with the Lua names and assertions: `external_path_is_flattened_into_root` (`init.luau:264-291`), `multiple_external_paths_are_all_flattened` (`:293-318`), `child_path_inside_root_is_not_moved` (`:320-335`), `convert_to_copies_all_files_and_resolves_aliases` (`:343-392`, the Rojo build of the converted tree and the "one child named pkg" count go through `model::load`), `real_forge_vfx_project_converts` (`:467-499`, early return with `eprintln!("skipped: ...")` when `/home/zili/Projects/laptop/vfx-forge/pkg/forge-vfx` is absent). Project files are compared as `serde_json::Value`.

New:

```rust
#[test]
fn flattened_entry_with_more_than_a_path_is_kept_and_warned() {
    let (_g, p) = fixture("external-path");
    // rewrite default.project.json with "pkg": {"$path": "pkg", "$className": "Folder"}
    let out = /* convert */;
    assert!(text(&out).contains(r#""pkg" was moved into "src" but its project entry carries more than a $path"#));
    let project: serde_json::Value = /* read converted default.project.json */;
    assert_eq!(project["tree"]["pkg"]["$path"], "src/pkg");
}

#[test]
fn converted_project_keeps_key_order() {
    // convert external-path; the converted default.project.json text has "name" before "tree"
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --test flattening --test convert_structure --test forge_vfx`
Expected: FAIL (`pkg should be moved into src/`).

- [ ] **Step 3: Implement `flatten_tree_paths`**

Port `util.flattenTreePaths` (`util.luau:498-594`): root `$path` with trailing slashes stripped; each non-`$` child with a `$path` outside it moves into `<root>/<key>` (file: copy then remove; dir: remove the destination, `copy_tree`, remove the source); an entry holding only `$path` is removed, otherwise kept with the new path and the warning text exactly `"<key>" was moved into "<root>" but its project entry carries more than a $path, so it is kept and Rojo will build two instances named "<key>"`. Rewrite with `serde_json::to_string_pretty` (key order kept by `preserve_order`). `convert::run` calls it for each project scope after rewriting, before the final copy.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test`
Expected: PASS, including `real_forge_vfx_project_converts` on the owner's machine (not skipped).

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml Cargo.lock src/project.rs src/convert.rs tests/common/model.rs tests/flattening.rs tests/convert_structure.rs tests/forge_vfx.rs
git commit -m "flatten outside paths into the root when converting"
```

---

### Task 5: One-shot build mode

**Files:**
- Create: `src/output.rs`, `src/pipeline.rs`, `tests/build.rs`
- Modify: `src/project.rs` (mount closure), `src/workspace.rs` (mirror, snapshot, write-if-changed), `src/rojo.rs` (build), `src/main.rs` (dispatch `Mode::Build` without `--watch`), `src/lib.rs`

**Interfaces:**
- Consumes: Tasks 1 to 3.
- Produces:
  - `project::Mounts { roots: BTreeSet<PathBuf>, projects: BTreeSet<PathBuf>, luaurcs: BTreeSet<PathBuf> }` (all relative to the project dir; `roots` are mounted dirs or files)
  - `project::mount_closure(project_dir: &Path, cli_project: &Path, preferred: Option<&str>) -> anyhow::Result<Mounts>` (spec 6.5, including scope projects and ancestor `.luaurc` files; a `$path` resolving outside `project_dir` is an error whose message contains `outside the project directory`)
  - `Mounts::contains(&self, rel: &Path) -> bool`, `Mounts::watch_dirs(&self) -> (Vec<PathBuf>, Vec<PathBuf>)` (recursive dirs, shallow dirs; relative)
  - `workspace::Meta { dir: bool, len: u64, mtime: SystemTime }`, `workspace::Snapshot { entries: BTreeMap<PathBuf, Meta> }` (relative)
  - `workspace::mirror_mounts(src: &Path, tree: &Path, mounts: &Mounts) -> anyhow::Result<(Snapshot, BTreeMap<PathBuf, Vec<u8>>)>` (snapshot plus raw bytes of every script, keyed relative; scripts are written raw here and rewritten by the pipeline before any build)
  - `workspace::write_if_changed(path: &Path, bytes: &[u8]) -> anyhow::Result<bool>` (creates parents)
  - `rojo::BuildOut { File(PathBuf), Plugin(String) }`, `Rojo::build(&self, project: &Path, out: &BuildOut) -> anyhow::Result<()>` (forwards stdout and stderr lines with `log::rojo`, drops lines containing `Built project to`, `Err` on a non-zero exit)
  - `output::swap(built: &Path, dest: &Path) -> anyhow::Result<()>`
  - `pipeline::Pipeline` with `fn start(args: &cli::BuildArgs) -> anyhow::Result<Pipeline>` (workspace, mounts, mirror, scopes, rewrite all, report), `fn build(&mut self) -> anyhow::Result<()>` (rojo build into `out/build.<ext of OUTPUT>`, `output::swap`, marker), `fn tree(&self) -> PathBuf`, `fn project_dir(&self) -> &Path`, `fn mounts(&self) -> &Mounts`, `fn output(&self) -> Option<&Path>`
  - `pipeline::run_once(args: &cli::BuildArgs) -> anyhow::Result<()>`
  - tests: `common::build(dir: &Path, args: &[&str]) -> std::process::Output` (runs with `--color never` and `TMPDIR`, `TEMP`, `TMP` set to a test-owned dir), `common::workspaces_in(tmp: &Path) -> Vec<PathBuf>`

- [ ] **Step 1: Write the failing tests**

`tests/build.rs`:

```rust
#[test]
fn normal_build_leaves_the_project_untouched() {
    // ported from tests/init.luau:438-459: after a build, p/pkg exists and p/src/pkg does not
}

#[test]
fn built_model_holds_dealiased_sources() {
    let (_g, p) = fixture("normal-build");
    let out = p.join("../out.rbxm");
    assert!(build(&p, &["-o", out.to_str().unwrap()]).status.success());
    let roots = model::load(&out);
    let init = roots[0].source.as_deref().unwrap();
    assert!(init.contains(r#"require("@self/mod/helpers")"#) && init.contains(r#"require("@self/pkg/Promise")"#));
    let helpers = roots[0].find(&["mod", "helpers"]).unwrap().source.as_deref().unwrap();
    assert!(helpers.contains(r#"require("../pkg/Promise")"#));
    assert!(model::sources(&roots).iter().all(|(_, s)| !s.contains("@mod/") && !s.contains("@pkg/")));
}

#[test]
fn marker_is_printed_once_after_the_output_exists() {
    let (_g, p) = fixture("normal-build");
    let out = build(&p, &["-o", "out.rbxm"]);
    let s = text(&out);
    let markers: Vec<_> = s.lines().filter(|l| l.contains("Built project to")).collect();
    assert_eq!(markers, vec!["OK   Built project to out.rbxm"]);
    assert!(p.join("out.rbxm").is_file());
}

#[test]
fn failed_build_exits_one_and_keeps_the_old_output() {
    let (_g, p) = fixture("normal-build");
    std::fs::write(p.join("out.rbxm"), "old").unwrap();
    std::fs::write(p.join("default.project.json"), r#"{"name": "X", "tree": {"$path": "src", "gone": {"$path": "missing"}}}"#).unwrap();
    let out = build(&p, &["-o", "out.rbxm"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(std::fs::read_to_string(p.join("out.rbxm")).unwrap(), "old");
    assert!(!text(&out).contains("Built project to"));
}

#[test]
fn workspace_is_removed_after_exit() { /* after a build, workspaces_in(tmp) is empty */ }

#[test]
fn concurrent_runs_do_not_share_workspaces() {
    // two builds of one fixture started together in one TMPDIR, different -o: both exit 0, outputs byte-identical
}

#[test]
fn stale_workspace_is_swept_live_one_kept() {
    // tmp/rojo-build-noalias.dead/lock (unlocked) is gone after a build;
    // tmp/rojo-build-noalias.live/lock held with File::lock by the test is still there
}

#[test]
fn named_project_build_uses_the_cli_project() {
    // build "dev.project.json" with "--output out.rbxm" after the positional:
    // Named/stories/button.story Source contains require("../mod/helpers")
}

#[test]
fn nested_luaurc_build_matches_startup_rules() {
    // without the flag: NestedLuaurc/worker/job contains require("../pkg/vfx");
    // with --process-orphans: require("../BundledAssets/ForgeVFX"); both contain require("../mod/helpers")
}

#[test]
fn luaurc_outside_the_mounts_is_not_processed() {
    // add junk/.luaurc {"aliases":{"x":"./x"}} and junk/default.project.json "{ not json"
    // to a normal-build copy: the build exits 0 and no output line mentions "junk"
}

#[test]
fn path_outside_the_project_is_an_error() {
    // "$path": "../outside": exit 1, output contains "outside the project directory"
}

#[test]
fn rbxmx_output_is_xml() {
    // -o out.rbxmx: the file starts with "<roblox"
}
```

Unit tests: in `src/project.rs`, `mount_closure` on `normal-build` gives roots `{src, pkg}`, projects `{default.project.json}`, luaurcs `{.luaurc}`; on `nested-project-in-folder` the projects include `pkg/Encoder/default.project.json`; a temp tree mounting `vendor/shared` with a `vendor/.luaurc` lists it. In `src/output.rs`, `swap_replaces_and_leaves_no_temp_files` (dest content replaced; no `.rbn-*` entry left in the dir).

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --test build`
Expected: FAIL (`ERR  not implemented yet`).

- [ ] **Step 3: Implement**

`mount_closure`: start from the CLI project file; collect every `$path` in its tree recursively (string or `{"optional": s}`), resolved against the project file's dir and normalized; a `$path` naming a `*.project.json` file adds that file to `projects` and recurses into its tree; walk each mounted directory (skip names applied) for `default.project.json` files (nested projects) and `.luaurc` files, recursing into the trees of the former; add `.luaurc` files from the project dir and from every directory between it and each mount root; for each `.luaurc`, `find_for_luaurc` on the original tree gives its scope project, which joins `projects` and whose tree joins the closure. Repeat until nothing is added.

`mirror_mounts`: copy each root (files and dirs, the `copy_tree` rules) into `tree`, plus every file in `projects` and `luaurcs`; record a `Meta` per entry and the raw bytes of every script.

`Pipeline::start`: `Workspace::create`, `mount_closure`, `mirror_mounts`, scopes from `mounts.luaurcs` mapped into `tree` (lookup maps the original scope project into `tree`), rewrite every covered script from the raw map, `write_if_changed`, report. `build`: `Rojo::build(target.project_in(&tree), File(out/build.<ext>))` with cwd = the original project dir, then `output::swap`, then `log::success("Built project to <OUTPUT as given>")`; plugin mode passes `Plugin(name)` and prints `Built project to plugin <name>`.

`output::swap`: `NamedTempFile::with_prefix_in(".rbn-", dest.parent())`, copy the built bytes in, `persist(dest)`; on a persist error on Windows retry 10 times 50 ms apart, then `fs::copy` with `WARN could not replace <dest> atomically; copied over it`.

`main`: `Build` without `watch` calls `run_once`; `Err` prints `ERR  <error chain>` and exits 1.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test`
Expected: PASS for all suites.

- [ ] **Step 5: Commit**

```bash
git add src/lib.rs src/main.rs src/project.rs src/workspace.rs src/rojo.rs src/output.rs src/pipeline.rs tests/common/mod.rs tests/build.rs
git commit -m "build from a per-run mirror of the mounted files"
```

---

### Task 6: Prove one-shot parity with v1.2.4

**Files:**
- Create: `scripts/snapshot-vfx-forge.sh`, `scripts/parity.sh`, `examples/rbxm_diff.rs`

**Interfaces:**
- Consumes: the release binary from Tasks 1 to 5; the `rbx_binary` and `rbx_dom_weak` dev-dependencies from Task 4.
- Produces:
  - `scripts/snapshot-vfx-forge.sh <dest>`: `rsync -a --delete --exclude .git --exclude node_modules --exclude /.worktrees --exclude /.claude --exclude /dist /home/zili/Projects/laptop/vfx-forge/ <dest>/`, then copies `dist/staging`, `dist/empty` and `dist/link` if present; prints `snapshot <dest> <file count>`.
  - `scripts/parity.sh`: reads `OLD_BIN`, `NEW_BIN`, `SNAP`; prints one `PASS <case>` or `DIFF <case>` line per case and exits 1 on any `DIFF`; prints `time <case> old=<ms> new=<ms>`.
  - `cargo run --example rbxm_diff -- <a> <b>`: loads both with `rbx_binary::from_reader` and walks the two `rbx_dom_weak` trees in child order, printing up to 20 differences as `<instance path>: <what>` (missing child, class, property name, property value, every property compared); exit 0 when none.

- [ ] **Step 1: Write the scripts and the example**

`parity.sh` cases, each built with the old binary twice (`old-a`, `old-b`) and the new one once, in `SNAP` as cwd:
1. `dev`: `-o <tmp>/dev.<tool>.rbxm dev.project.json`
2. `default`: `-o <tmp>/default.<tool>.rbxm`
3. `bundle` (only if `SNAP/dist/staging` exists): `--process-orphans -o <tmp>/bundle.<tool>.rbxm bundle.project.json`
4. each `tests/fixtures/*` holding a `default.project.json`, as copies in `<tmp>`
5. `convert-forge-vfx`: `--convert-to <tmp>/cv.<tool>` in `SNAP/pkg/forge-vfx`; compared with `diff -r` after replacing each tree's `default.project.json` with its `jq -S .` form
A case passes when `old-a` equals `old-b` (else `DIFF <case> old tool unstable`) and `cmp` finds the new output equal to `old-a`; on a byte difference it runs `rbxm_diff` and prints its output under the `DIFF` line. Old outputs are checked for existence (v1.2.4 exits 0 even when Rojo fails).

- [ ] **Step 2: Fetch v1.2.4 and the snapshot**

```bash
T=$(mktemp -d)
gh release download v1.2.4 -R zilibobi/rojo-build-noalias -p 'rbn-1.2.4-linux-x86_64.zip' -D "$T"
unzip -q "$T/rbn-1.2.4-linux-x86_64.zip" -d "$T/old"
bash scripts/snapshot-vfx-forge.sh "$T/snap"
cargo build --release
```

Expected: `snapshot <T>/snap <n>` with n in the thousands, `$T/old/rojo-build-noalias` executable.

- [ ] **Step 3: Run parity**

```bash
ROJO_DIR=$(dirname "$(cd /home/zili/Projects/laptop/vfx-forge && mise which rojo)")
PATH="$ROJO_DIR:$PATH" OLD_BIN="$T/old/rojo-build-noalias" NEW_BIN="$PWD/target/release/rojo-build-noalias" SNAP="$T/snap" bash scripts/parity.sh
```

Expected: every line `PASS`, and `time dev old=<about 17000> new=<under 3000>`. The real rojo goes first on PATH because v1.2.4 runs Rojo inside its `/tmp` workspace, where a rokit shim finds no manifest.

- [ ] **Step 4: Resolve any DIFF before continuing**

A `DIFF` is a bug in Tasks 2 to 5 unless it is one of the spec's section 5 divergences; fix it in the owning module with a regression test there, rerun Step 3. A difference that is a listed divergence and still semantically equal under `rbxm_diff` is acceptable only if written into the commit body below. Then delete the snapshot (it holds `src/.env.luau`): `rm -rf "$T"`.

- [ ] **Step 5: Commit**

```bash
git add scripts/snapshot-vfx-forge.sh scripts/parity.sh examples/rbxm_diff.rs
git commit -m "test: check one-shot builds against v1.2.4 on vfx-forge"
```

Body: the case list with PASS, the two startup times, and any accepted difference with its cause.

---

### Task 7: Incremental apply

Everything watch mode does after an event, driven directly so it is testable without a watcher.

**Files:**
- Create: `src/diff.rs`, `tests/incremental.rs`
- Modify: `src/pipeline.rs`, `src/lib.rs`

**Interfaces:**
- Consumes: Task 5 `Pipeline`, `Mounts`, `Snapshot`, `mirror_mounts`, `write_if_changed`; Task 3 `ScopeSet`.
- Produces:
  - `diff::Hints { paths: BTreeSet<PathBuf>, full: bool }` (absolute paths under the project dir)
  - `diff::ChangeSet { added: BTreeSet<PathBuf>, removed: BTreeSet<PathBuf>, changed: BTreeSet<PathBuf>, config: bool }` (relative), `ChangeSet::is_empty(&self) -> bool`
  - `diff::diff(project_dir: &Path, mounts: &Mounts, snapshot: &Snapshot, raw: &BTreeMap<PathBuf, Vec<u8>>, hints: &Hints) -> anyhow::Result<ChangeSet>`
  - `Pipeline::diff(&self, hints: &Hints) -> anyhow::Result<ChangeSet>`
  - `Pipeline::apply(&mut self, changes: &ChangeSet) -> anyhow::Result<Applied>`
  - `pipeline::Applied { wrote: usize, removed: usize, rewatch: bool }`, `Applied::dirty(&self) -> bool` (`wrote + removed > 0`)
  - `Pipeline::relevant(&self, path: &Path) -> bool` (under a root, or a project file, or a `.luaurc`, or a direct child of a shallow watch dir named `.luaurc` or `*.project.json(c)`; never under a skip name; never the output or a `.rbn-*` file)
  - tests: `incremental::assert_mirror_equals_fresh(p: &Pipeline, args: &BuildArgs)` (a second `Pipeline::start` on the same project; the two `tree` dirs must hold the same relative paths with the same bytes)

- [ ] **Step 1: Write the failing tests**

Each test copies a fixture, starts a `Pipeline` (with `OutputTarget::File` in the temp dir), mutates the source, calls `p.apply(&p.diff(&hints)?)?`, then checks. `hints` names the touched paths, as notify would.

```rust
#[test]
fn content_edit_rewrites_only_that_file() {
    // normal-build: append a line to src/mod/helpers.luau
    // applied.wrote == 1; mirror helpers contains require("../pkg/Promise") and the new line
    // assert_mirror_equals_fresh
}

#[test]
fn second_save_with_the_same_length_and_mtime_is_seen() {
    // write "local a = 1" to src/mod/helpers.luau, apply; write "local a = 2",
    // set its mtime back to the first save's with File::set_modified, apply:
    // the mirror holds "local a = 2"
}
```

Also, each ending with `assert_mirror_equals_fresh`:
- `new_folder_with_module_is_mirrored_and_dealiased`: `src/newdir/thing.luau` with `require("@mod/helpers")`, hint `src/newdir`; the mirror file holds `require("../mod/helpers")`.
- `deleted_file_is_pruned`: remove `pkg/Promise.luau` (hint it): the mirror file is gone, `applied.removed == 1`.
- `renamed_file_and_folder_are_moved`: rename `src/mod/helpers.luau` to `src/mod/util.luau`, then create `src/newdir/a.luau` and rename `src/newdir` to `src/newdir2`; hints name both old and new paths.
- `touch_without_change_writes_nothing`: rewrite identical bytes: `!applied.dirty()`.
- `nested_luaurc_edit_matches_startup` (`nested-luaurc`): edit `src/worker/job.luau`; the mirror job holds `require("../pkg/vfx")`.
- `luaurc_alias_change_realiases_the_scope`: point `mod` at `./src/other` with a new `src/other/helpers.luau`; hint `.luaurc` and the new dir; `changes.config` is true.
- `broken_luaurc_keeps_previous_aliases`: write `{` to `.luaurc`: `apply` is `Err`, the mirror is unchanged; restore it: `Ok`, equals fresh.
- `project_edit_adding_a_mount_mirrors_it`: add `"extra": {"$path": "extra"}` and `extra/x.luau`: `applied.rewatch`, mirror has `extra/x.luau`.
- `unmounted_paths_are_ignored`: hint `junk/x.luau` (created): `changes.is_empty()`.
- `full_rescan_finds_everything`: three different edits, `Hints { full: true, paths: {} }`.
- `mount_root_deleted_and_recreated`: remove `pkg` (hint `pkg`): mirror has no `pkg`, the run logs the alias warning; recreate `pkg/Promise.luau`, hint `pkg`: equals fresh.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --test incremental`
Expected: FAIL (methods missing).

- [ ] **Step 3: Implement**

`diff::diff` per spec 7.2: `full` walks every root and compares with the whole snapshot; otherwise each hint is classified by `stat`: gone (remove it and its snapshot subtree), a dir (shallow compare of children, full walk of new subdirs), a file (script: bytes against `raw`; other: `len` or `mtime` differ). Hints outside the mounts or under skip names are dropped. `config` when any added, removed or changed path is a `.luaurc` or a project file in `mounts`.

`Pipeline::apply` in this order: update `raw` and `snapshot`; mirror creates and removes; if `config`: re-run `mount_closure` (errors here fail the batch), mirror new roots, prune dropped ones, rebuild the `ScopeSet`, set `rewatch`; a `.luaurc` that fails to parse after having parsed is an `Err` naming it, with the previous `ScopeSet` kept; if anything was added or removed or a `.json` file changed, regenerate every project scope's sourcemap (`std::thread::scope`, one `rojo sourcemap` each); compute dirty scripts (changed or added; context or chain changed; every script of a scope whose resolved targets changed); rewrite them from `raw` with `ScopeSet::rewrite`; `write_if_changed`. On any `Err` the next call must work from a consistent state: keep the old `ScopeSet`, and mark the pipeline so the next `diff` is forced `full`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/lib.rs src/pipeline.rs src/diff.rs tests/incremental.rs
git commit -m "apply added, removed and changed files to the mirror"
```

---

### Task 8: Watch mode

**Files:**
- Create: `src/watch/mod.rs`, `src/watch/batcher.rs`, `tests/common/watcher.rs`, `tests/watch.rs`, `scripts/watch-latency.sh`
- Modify: `src/main.rs` (dispatch `--watch`), `src/shutdown.rs` (install `ctrlc`), `src/rojo.rs` (kill the child on shutdown), `src/lib.rs`

**Interfaces:**
- Consumes: Task 7 `Pipeline::{diff, apply, relevant}`, `Hints`; Task 5 `Pipeline::{start, build, mounts}`, `Mounts::watch_dirs`.
- Produces:
  - `watch::batcher::QUIET: Duration` = 200 ms, `watch::batcher::MAX_WAIT: Duration` = 2 s
  - `watch::batcher::Batcher::new(quiet: Duration, max_wait: Duration) -> Batcher`, `.push(&mut self, now: Instant, paths: impl IntoIterator<Item = PathBuf>, full: bool)`, `.deadline(&self) -> Option<Instant>`, `.take(&mut self, now: Instant) -> Option<diff::Hints>`
  - `watch::Msg { Fs(notify::Result<notify::Event>), Shutdown }`
  - `watch::run(args: &cli::BuildArgs) -> anyhow::Result<()>`
  - tests: `watcher::Watcher::spawn(dir: &Path, extra: &[&str]) -> Watcher` (`--color never --watch -o <tmp>/out.rbxm`, own TMPDIR, stdout read by a thread into a channel), `Watcher::spawn_out(dir: &Path, out: &Path, extra: &[&str]) -> Watcher` (same with the given output), `.wait_built(&self, within: Duration) -> bool`, `.count_built(&self, during: Duration) -> usize`, `.saw(&self, needle: &str) -> bool`, `.model(&self) -> Vec<model::Inst>`, `.out_bytes(&self) -> Vec<u8>`, `.assert_matches_fresh_build(&self)` (600 ms with no marker, then a one-shot build of the same dir to another file and `assert_eq!` of the bytes), `.pid(&self) -> u32`, `.tmp(&self) -> &Path`, `.term(self)`, `.close_stdout(&mut self)`

- [ ] **Step 1: Write the failing batcher tests**

In `src/watch/batcher.rs`:

```rust
#[test]
fn five_quick_pushes_are_one_batch_after_quiet() {
    let t0 = Instant::now();
    let mut b = Batcher::new(QUIET, MAX_WAIT);
    for i in 0..5 { b.push(t0 + ms(40 * i), [PathBuf::from(format!("/p/f{i}"))], false); }
    let last = t0 + ms(160);
    assert!(b.take(last + ms(199)).is_none());
    let h = b.take(last + ms(200)).unwrap();
    assert_eq!(h.paths.len(), 5);
    assert!(b.deadline().is_none());
}

#[test]
fn a_stream_that_never_pauses_closes_at_max_wait() {
    let t0 = Instant::now();
    let mut b = Batcher::new(QUIET, MAX_WAIT);
    let mut closed = None;
    for i in 0..40 {
        let now = t0 + ms(100 * i);
        if let Some(h) = b.take(now) { closed = Some((i, h)); break; }
        b.push(now, [PathBuf::from("/p/x")], false);
    }
    assert_eq!(closed.unwrap().0, 20); // t0 + 2 s
}

#[test]
fn full_rescan_flag_survives_merging() { /* push(full = true) then push(full = false): take().full */ }
```

- [ ] **Step 2: Write the failing watch tests**

`tests/watch.rs`, each on a fresh fixture copy (`normal-build` unless named), starting with `assert!(w.wait_built(secs(10)))` for the initial build and ending with `w.assert_matches_fresh_build()`. Every test that leaves each alias's target folder with at least one file also checks that no Source in the final model holds the fixture's raw aliases (`@mod/`, `@pkg/`, `@vfx`); in `normal-build` each of `src/mod` and `pkg` holds one file, so a test that deletes one first adds a second.

```rust
#[test]
fn burst_of_five_files_is_one_build() {
    let (_g, p) = fixture("normal-build");
    let w = Watcher::spawn(&p, &[]);
    assert!(w.wait_built(secs(10)));
    for i in 0..5 { std::fs::write(p.join(format!("src/mod/b{i}.luau")), "return require(\"@pkg/Promise\")").unwrap(); }
    assert_eq!(w.count_built(secs(2)), 1);
    w.assert_matches_fresh_build();
}

#[test]
fn two_saves_1500ms_apart_both_land() {
    // write "return 1" to src/mod/helpers.luau, wait_built; sleep 1.5 s; write "return 2", wait_built;
    // Source of NormalBuild/mod/helpers == "return 2"
}

#[test]
fn broken_project_edit_keeps_last_good_build() {
    let (_g, p) = fixture("normal-build");
    let w = Watcher::spawn(&p, &[]);
    assert!(w.wait_built(secs(10)));
    let good = w.out_bytes();
    std::fs::write(p.join("default.project.json"), "{ not json").unwrap();
    assert_eq!(w.count_built(secs(2)), 0);
    assert!(w.saw("ERR ") && w.saw("keeping the last good build"));
    assert_eq!(w.out_bytes(), good);
    // restore the original project file: wait_built, then assert_matches_fresh_build
}
```

Also:
- `new_folder_with_module_builds_once_and_keeps_watching`: `src/newdir/thing.luau` requiring `@mod/helpers`: one build, `NormalBuild/newdir/thing` holds `require("../mod/helpers")`; then an edit inside `src/newdir` builds again (the watcher survived the new folder).
- `deleted_file_leaves_the_model`: add `pkg/Extra.luau`, wait for its build, remove it: `NormalBuild/pkg/Extra` absent, `NormalBuild/pkg/Promise` present.
- `renamed_file_and_folder_move_in_the_model`: `mv src/mod/helpers.luau src/mod/util.luau`, then create `src/newdir/a.luau` and `mv src/newdir src/newdir2`: old names absent, new present.
- `nested_luaurc_edit_matches_startup` (`nested-luaurc`): edit `src/worker/job.luau`: `NestedLuaurc/worker/job` holds `require("../pkg/vfx")`.
- `broken_luaurc_edit_keeps_last_good_build`: write `{` to `.luaurc`; as the project test.
- `atomic_save_via_rename_lands_and_temp_never_ships`: write `src/mod/helpers.luau.tmp1234` then rename it over `src/mod/helpers.luau`, and create and delete `src/mod/4913`: one build, no instance named `helpers.luau` or `4913`.
- `checkout_sized_stream_builds_bounded_and_ends_right`: 40 files written 75 ms apart (3 s of stream) in `src/mod`: `count_built(secs(5))` between 1 and 3.
- `mount_root_deleted_and_recreated_is_rewatched`: `rm -r pkg`: a build without `pkg`, output mentions `was not found in the sourcemap`; recreate `pkg/Promise.luau`: a build with it; then edit `pkg/Promise.luau`: another build carrying the edit.
- `churn_outside_mounts_builds_nothing`: 50 writes under `junk/` and `docs/`: `count_built(secs(2)) == 0`.
- `output_inside_a_mount_does_not_loop`: `Watcher::spawn_out(&p, &p.join("src/out.rbxm"), &[])`: after the initial build, `count_built(secs(3)) <= 1`.
- `two_watchers_on_one_project_both_build`: two `Watcher`s, one edit: each sees one build; each `tmp()` holds exactly its own workspace.
- `sigterm_removes_workspace` (`#[cfg(unix)]`): `kill -TERM <pid>`: exit within 2 s, status 0, `workspaces_in(w.tmp())` empty.
- `closed_stdout_exits_and_removes_workspace` (`#[cfg(unix)]`): `close_stdout`, then an edit: the process exits within 3 s, workspace gone.
- `idle_cpu_is_near_zero` (`#[cfg(target_os = "linux")]`): after the initial build sleep 1 s, read `utime + stime` from `/proc/<pid>/stat`, sleep 5 s, read again: the delta is at most 2 ticks.

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test --lib watch && cargo test --test watch`
Expected: FAIL (batcher missing; `--watch` prints `not implemented`).

- [ ] **Step 4: Implement**

`Batcher`: `push` sets `first` if unset, `last = now`, merges paths and `full`; `deadline = min(last + quiet, first + max_wait)`; `take(now)` returns the merged `Hints` and resets when `now >= deadline`.

`watch::run`: `Pipeline::start`; first `build` (an error is logged with `ERR` and the loop still starts); create an `std::sync::mpsc` channel, a `notify::recommended_watcher` sending `Msg::Fs`, `shutdown::on_request` sending `Msg::Shutdown`; install `ctrlc::set_handler(shutdown::request)`; watch `mounts().watch_dirs()` (recursive and non-recursive, absolute); push one `full` hint. Loop: `recv()` when no batch is open, `recv_timeout(deadline - now)` when one is; each `Fs` event pushes its `relevant` paths (an event with `need_rescan()` or an `Err` pushes `full`); on a closed batch run `diff`, `apply`, and `build` when `applied.dirty()` or the last build failed; `applied.rewatch` re-registers watches from the new mounts. Any `Err` (and a panic caught with `catch_unwind` around the batch) prints `ERR  <chain>` and `WARN keeping the last good build at <OUTPUT>` and forces the next batch `full`. `Shutdown` breaks the loop, killing a running rojo child; returning drops the workspace. In verbose mode each batch prints `INFO <c> changed, <a> added, <r> removed in <ms> ms` and its replacement rows.

`scripts/watch-latency.sh <snapshot>`: starts the release binary with `--watch -o <tmp>/out.rbxm dev.project.json` in the snapshot, waits for the first marker, then 5 times appends `-- latency probe <n>` to `src/utility.luau`, timestamps the write and the next marker line, prints `latency_ms=<n>` each; then idles 60 s and prints `idle_cpu_ms=<n>` from `/proc`; restores the file and kills the watcher.

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test`
Expected: PASS. Run `cargo test --test watch` three times in a row: PASS each time (no flake).

- [ ] **Step 6: Measure on the vfx-forge snapshot**

```bash
T=$(mktemp -d); bash scripts/snapshot-vfx-forge.sh "$T/snap"; cargo build --release
PATH="$(dirname "$(cd /home/zili/Projects/laptop/vfx-forge && mise which rojo)"):$PATH" bash scripts/watch-latency.sh "$T/snap"; rm -rf "$T"
```

Expected: five `latency_ms` values under 400, `idle_cpu_ms` under 50.

- [ ] **Step 7: Commit**

```bash
git add src/lib.rs src/main.rs src/shutdown.rs src/rojo.rs src/watch/mod.rs src/watch/batcher.rs tests/common/watcher.rs tests/watch.rs scripts/watch-latency.sh
git commit -m "watch native file events and rebuild once per batch"
```

Body: why a global quiet window instead of a debouncer crate (spec 7.2), and the measured latencies and idle CPU.

---

### Task 9: Release 2.0.0

**Files:**
- Modify: `.github/workflows/build.yml` (rewrite), `README.md`, `rokit.toml`, `Cargo.toml` (`version = "2.0.0"`), `Cargo.lock`
- Delete: `src/init.luau`, `src/mod/log.luau`, `src/mod/util.luau`, `src/mod/replaceAliases.luau`, `tests/init.luau`, `.darklua.json`, `stylua.toml`, `.luaurc`

**Interfaces:**
- Consumes: the finished crate.
- Produces: release assets `rbn-<version>-{linux-x86_64,linux-aarch64,windows-x86_64,windows-aarch64,macos-x86_64,macos-aarch64}.zip`, each holding `rojo-build-noalias` (`rojo-build-noalias.exe` on Windows) at the zip root.

- [ ] **Step 1: Write the workflow**

Triggers: `push` of tags `v*`, and `workflow_dispatch` (builds without releasing). Jobs:
- `test`: matrix `ubuntu-latest`, `macos-latest`, `windows-latest`; `actions/checkout@v4`, `CompeyDev/setup-rokit@v0.1.2` (Rojo 7.6.1 from `rokit.toml`), the toolchain from `rust-toolchain.toml` (rustup reads it), `cargo test --locked`.
- `build`: `needs: test`; matrix of `{name, os, target}`: `linux-x86_64` on `ubuntu-24.04` with `x86_64-unknown-linux-musl`; `linux-aarch64` on `ubuntu-24.04-arm` with `aarch64-unknown-linux-musl`; `windows-x86_64` on `windows-latest` with `x86_64-pc-windows-msvc`; `windows-aarch64` on `windows-11-arm` with `aarch64-pc-windows-msvc`; `macos-x86_64` on `macos-14` with `x86_64-apple-darwin`; `macos-aarch64` on `macos-14` with `aarch64-apple-darwin`. Steps: `sudo apt-get install -y musl-tools` on Linux, `rustup target add`, `cargo build --release --locked --target <target>`, zip the binary alone under its plain name (`zip -j` on Unix, `Compress-Archive` on Windows) as `rbn-${VERSION}-<name>.zip` where `VERSION` is the tag without `v`, `actions/upload-artifact@v4`.
- `release`: `needs: build`, only on tags; `actions/download-artifact@v4` with `merge-multiple: true`; `anton-yurchenko/git-release@v6` with the six zips (no `.luau` asset any more).

- [ ] **Step 2: Remove the Luau tool and update the docs**

Delete the files listed above. `rokit.toml` keeps only `rojo = "rojo-rbx/rojo@7.6.1"` (tests run it). README: installation unchanged (rokit and manual), add mise (`"ubi:zilibobi/rojo-build-noalias" = "2.0.0"`), a "Watch mode" section (what is watched, the 200 ms window, one build per batch, last good build kept, per-run workspace), the exit codes, the 2.0 breaking changes (spec section 5, items 1 to 4), and the existing limitations. `Cargo.toml` version `2.0.0`.

- [ ] **Step 3: Verify locally**

```bash
cargo test --locked
rustup target add x86_64-unknown-linux-musl
cargo build --release --locked --target x86_64-unknown-linux-musl
./target/x86_64-unknown-linux-musl/release/rojo-build-noalias --version
cd target/x86_64-unknown-linux-musl/release && zip -j /tmp/rbn-2.0.0-linux-x86_64.zip rojo-build-noalias && unzip -l /tmp/rbn-2.0.0-linux-x86_64.zip
python3 -c "import yaml,sys; yaml.safe_load(open('.github/workflows/build.yml'))"
```

Expected: tests PASS; `rojo-build-noalias 2.0.0`; `unzip -l` lists exactly one entry, `rojo-build-noalias`; the YAML loads.

- [ ] **Step 4: Commit**

```bash
git add .github/workflows/build.yml README.md rokit.toml Cargo.toml Cargo.lock
git rm src/init.luau src/mod/log.luau src/mod/util.luau src/mod/replaceAliases.luau tests/init.luau .darklua.json stylua.toml .luaurc
git commit -m "version 2.0.0"
```

Body: the rewrite in one paragraph, the breaking changes, and that assets keep their 1.x names for rokit and ubi.

- [ ] **Step 5: Hand the tag to the owner**

Do not tag or push. Give the owner: `git tag v2.0.0 && git push origin main v2.0.0`, then the checks once the workflow finishes: `gh release view v2.0.0 -R zilibobi/rojo-build-noalias --json assets -q '.assets[].name'` lists the six zips; in a temp dir `mise exec ubi:zilibobi/rojo-build-noalias@2.0.0 -- rojo-build-noalias --version` prints `rojo-build-noalias 2.0.0`; `rokit add zilibobi/rojo-build-noalias@2.0.0` succeeds in a temp dir with a `rokit.toml`.

---

### Task 10: vfx-forge on 2.0.0

Only after the owner has published `v2.0.0`. This task works in `/home/zili/Projects/laptop/vfx-forge` and follows its AGENTS.md (plain lowercase imperative commits, no prefix, no trailers, explicit paths, never push).

**Files:**
- Modify: `/home/zili/Projects/laptop/vfx-forge/mise.toml:21`, `/home/zili/Projects/laptop/vfx-forge/scripts/bash/dev.sh:24-26,62-63`, `/home/zili/Projects/laptop/vfx-forge/docs/references/engine-test-lane.md` (the sections "New directories under `src/` ARE missed", "The watcher may not be yours" paragraph on `/tmp/rojo-build-noalias/*`, "The build cache does not propagate a deletion", "When the watcher wedges")

**Interfaces:**
- Consumes: the published release.
- Produces: vfx-forge pinned to 2.0.0 with the simplified `dev.sh`.

- [ ] **Step 1: Bump the pin and simplify dev.sh**

`mise.toml`: `"ubi:zilibobi/rojo-build-noalias" = "2.0.0"`. `dev.sh`: delete `rm -rf /tmp/rojo-build-noalias/* 2>/dev/null || true`; replace the supervisor comment (lines 24-26) with one saying the loop restarts the watcher if it ever exits and that each run owns its own workspace; keep the `case *"Built project to"*` marker block, which now fires once per build. `engine-test-lane.md`: new folders, deletes and renames now reach the build; there is no shared cache to clear; the wedge recovery becomes "restart dev.sh".

- [ ] **Step 2: Verify the one-shot callers**

```bash
cd /home/zili/Projects/laptop/vfx-forge && mise install && mise exec -- rojo-build-noalias --version
mise exec -- rojo-build-noalias dev.project.json --output /tmp/rbn-check.rbxm; echo "exit $?"
```

Expected: `rojo-build-noalias 2.0.0`; one `OK   Built project to /tmp/rbn-check.rbxm`, `exit 0`.

- [ ] **Step 3: Verify the watcher live**

Start `bash scripts/bash/dev.sh` with the tool's own `run_in_background` (not `setsid ... &`, which the lane doc says gets reaped). Then: save `src/utility.luau` unchanged except for a comment: exactly one `[dev] rebuild` line; create `src/ui/_probe/probe.luau` returning `require("@lib/logger")`: one rebuild, and in Studio (ui-labs `execute_luau`) `shared.forge_dev.stamp` advances and the module's Source holds a relative require; rename that folder, then delete it: one rebuild each, and the module is gone from the plugin. Remove the probe, stop the watcher.

- [ ] **Step 4: Verify the release build**

Run: `bash scripts/bash/build.sh`
Expected: completes with exit 0 (a failing skeleton build would now stop it, divergence 1 of the spec).

- [ ] **Step 5: Commit**

```bash
git add mise.toml scripts/bash/dev.sh docs/references/engine-test-lane.md
git commit -m "move to rojo-build-noalias 2.0.0 and stop clearing its workspace"
```

Body: 2.0 watches only the mounted trees with native events, builds once per batch and keeps a per-run workspace, so dev.sh no longer clears `/tmp/rojo-build-noalias` (which also killed other sessions' runs) and prints the marker once per build.
