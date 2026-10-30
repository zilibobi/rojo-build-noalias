# rojo-build-noalias 2.0: a Rust rewrite

Date: 2026-10-31. Status: approved direction (owner decisions below are binding), design for review.

## 1. Why

v1.2.4 (tag `v1.2.4` = `0e0afb9`, Luau under lune) is correct for one-shot builds and broken in `--watch`, which is how vfx-forge's `scripts/bash/dev.sh` runs it all day. Measured on vfx-forge on 2026-10-30 (report: `scratchpad/watcher/report.md` of the measuring session; the numbers that matter are copied here):

- The poll walks the whole project root every 0.5 s (12,226 Luau files incl. `.worktrees`, `.claude`, `dist`, `docs`): ~450 ms per pass, 51% CPU while idle. Walking only the mounted trees takes 64-84 ms for 1,618 files.
- Detection takes 0.58-0.68 s. De-aliasing one changed file takes 2.9-3.1 s, because it re-runs the whole project (sourcemap 27 ms, reads 10 ms, 26 aliases x 2 `gsub` over 1,420 files = 3.36 s; one combined pattern = 51 ms). `rojo build` itself is 20-45 ms.
- A save produces a broken build at +0.68 s (raw `@fusion` requires) and the right one at +3.2 s. A burst of 5 files takes 14 s and 10 builds. Startup is 17 s (copies 491 MB, de-aliases 17 projects: every worktree plus `dist/staging`).

Root causes in v1.2.4, by line:

| Where | Defect |
|---|---|
| `src/init.luau:440` vs `:470` | the raw file is copied into Rojo's watched workspace before it is de-aliased, so Rojo builds the raw file first |
| `src/init.luau:440`, `:410`, `:498` | the copy has no mkdir and no pcall: a new folder kills the polling task while the rojo child keeps the process alive, so it looks idle forever |
| watch loop as a whole | no delete path: deleted and renamed files ship forever |
| `src/init.luau:437`, `src/mod/util.luau:186` | mtime compared against the workspace copy, at whole-second resolution, and the copy is rewritten ~3 s later: a second save 1.5 s after the first is lost |
| `src/init.luau:455-474` | watch mode takes only the nearest `.luaurc`; a nested orphan scope (`src/mod/emitter/worker/.luaurc`, no project file) leaves raw `@vfx` where startup had resolved it through the root scope |
| `src/init.luau:415-481` | the poll covers the whole root, so `build.sh` restaging `dist/staging` (553 files) stalls the watcher ~9 min |
| vfx-forge `dev.sh:63`, `:67-74` | `rm -rf /tmp/rojo-build-noalias/*` wipes other runs' workspaces; the marker is printed twice per build (the tool forwards Rojo's `Built project to` line and prints its own) |

Lune 0.10.4 and 0.11 have no file watching and only whole-second mtimes, so updating lune fixes none of this.

## 2. Owner decisions (binding)

1. Full rewrite in Rust, not a Rust watcher wrapping the Luau tool.
2. Same CLI and outputs, so `dev.sh`, `darklua-bundle.sh` (via `build.sh`), `labs.toml` and every doc command keep working. One-shot builds produce byte-identical (fallback: semantically identical) models to v1.2.4 on the vfx-forge project. Section 9 says how that is proven.
3. Watch mode fixes every root cause above: native file events with a 200 ms quiet window; watch only what the project mounts (plus `.luaurc` and project files); added, removed, changed and renamed files and folders all handled (new folders never crash, deletes and renames are pruned); de-alias in memory with one combined matcher on changed files only; Rojo never sees a raw file; one `rojo build` per batch to a temp file, then an atomic rename over the output; the last good build survives any error and the watcher keeps watching; nested `.luaurc` scopes behave in watch mode exactly as at startup; a per-run workspace cleaned on exit, never a shared `/tmp` directory another run can wipe; near-zero idle CPU.
4. Keep calling the `rojo` binary. No librojo vendoring (section 6.6 argues it stays that way).
5. Release a major version, `2.0.0` (tag `v2.0.0`), with cargo-built assets under the same names, so `rokit add zilibobi/rojo-build-noalias` and mise's `ubi:zilibobi/rojo-build-noalias` keep installing it on every platform the current workflow ships. Then vfx-forge bumps its pin and simplifies `dev.sh`.
6. The existing tests are ported to `cargo test` on the same fixtures, plus a test for every watch-mode failure above.

## 3. Goals and non-goals

Goals: the decisions above; per-save latency (last write to the marker) under 400 ms on vfx-forge `dev.project.json` (200 ms window + under 200 ms of work); startup under 3 s on the same project (v1.2.4: 17 s); idle CPU indistinguishable from zero (under 50 ms of CPU time per minute idle).

Non-goals: `rojo serve` support (still a README limitation); place builds with more than one root (still a limitation; Rojo itself handles them, we just do not promise anything); changing the de-alias semantics (relative paths, `@self`, init rules, wax compat) beyond the divergences listed in section 5; a config file; a library API for other crates (the lib target exists for tests only).

## 4. The command line (kept)

```
rojo-build-noalias [OPTIONS] [PROJECT]
```

| Flag | Meaning (as v1.2.4) |
|---|---|
| `PROJECT` | a directory, a project file (any name, e.g. `dev.project.json`, `default.minified.project.json`) or a path that does not exist (treated as a directory). Default: the current directory. Build mode takes the last positional, convert mode the first; there is only ever one, so 2.0 takes exactly one. |
| `-o, --output <OUTPUT>` | where the model lands, relative to the cwd. |
| `-p, --plugin <PLUGIN>` | Rojo writes into the Studio plugins folder. Refused on Linux with `The '--plugin' option is not supported on Linux.`, exit 1. |
| `--watch` | rebuild on changes (owned by this tool now; never passed to Rojo). |
| `-v, --verbose` | repeatable; sets our verbose logging and is passed through to Rojo as that many `-v`. |
| `--color <auto\|always\|never>` | passed through to Rojo; now also governs our own prefixes. |
| `--convert-to <DIR>` | write a converted copy of the project to DIR and exit (no build). |
| `--process-orphans` | `.luaurc` files with no project file beside them become literal text replacements. |
| `--wax-compat` | nested `init.luau` requires get one extra `..` for wax's pre-RFC bundle shim. |
| `-h, --help` | the v1.2.4 help text (same wording and coloring rules, version line `rojo-build-noalias 2.0.0`), exit 0. Also printed, exit 0, when no arguments at all are given (v1.2.4 `init.luau:10`). |
| `-V, --version` | new: `rojo-build-noalias 2.0.0`. |

Callers in vfx-forge that must keep working unchanged: `rojo-build-noalias --watch -o "$PLUGIN_OUT" dev.project.json` (dev.sh), `rojo-build-noalias --process-orphans -o "$SKELETON" bundle.project.json` (darklua-bundle.sh, via build.sh), `rojo-build-noalias dev.project.json --output forge-stories.rbxm` (labs.toml, flags after the positional), `rojo-build-noalias -o output.rbxm` (AGENTS.md).

Output contract. Everything we print goes to stdout (as v1.2.4, which printed errors with `print` too), with the v1.2.4 prefixes `INFO`, `WARN`, `OK  `, `ERR ` and `ROJO` followed by one space. Rojo's own stdout and stderr lines are forwarded with the `ROJO` prefix, except Rojo's `Built project to ...` line, which is swallowed. After the output file is in place we print exactly one line containing `Built project to <OUTPUT as given>` (`OK   Built project to <OUTPUT>`). That line is the marker `dev.sh` greps, and it is printed once per landed build, never for a failed one. In plugin mode the marker names the plugin (`OK   Built project to plugin <PLUGIN>`).

Exit codes: 0 on success (and on SIGINT/SIGTERM/SIGHUP in watch mode); 1 when a one-shot build or a conversion fails; 2 on a usage error. Watch mode never exits on a build error.

## 5. Deliberate divergences from v1.2.4

Each is either a bug fix or invisible on vfx-forge; each has a test.

1. A failed one-shot build exits 1. v1.2.4 exited 0 whatever Rojo did (it never forwarded Rojo's status), so `build.sh` under `set -euo pipefail` carried on with a stale skeleton. This is the main reason for the major bump.
2. Unknown flags are a usage error (exit 2). v1.2.4 forwarded them to `rojo build`, which has no flags beyond the ones in section 4, so no working invocation changes. A missing value after `-o` is also exit 2 (v1.2.4 printed an error and exited 0).
3. Rojo builds to a temp file with the output's own extension. v1.2.4 always built `build.rbxm` and copied it to whatever `-o` named, so `-o x.rbxmx` received binary bytes.
4. `--convert-to` refuses a DIR equal to the project directory or an ancestor of it (v1.2.4 deleted the project), and excludes DIR from the copy when DIR is inside the project (v1.2.4 copied the previous output into the new one, nesting it one level deeper on every run). Both checks compare paths after resolving symlinks (the project dir, and DIR or its nearest existing ancestor), so a symlinked spelling of DIR cannot reach the project; v1.2.4 compared string prefixes, which also took `proj-out` for a dir inside `proj`. Tests: `convert_safety::convert_into_project_dir_is_refused`, `convert_inside_project_does_not_nest_previous_output`, `symlink_onto_the_project_is_refused`, `symlink_into_the_project_is_treated_as_inside`.
5. Alias names match literally. v1.2.4 escaped only `- + * [ ] ( )` for Lua patterns, so a `.` in an alias name matched any character. No vfx-forge alias contains one.
6. Alias paths are normalized lexically before lookup (`./src/` and `./src/./mod` resolve). v1.2.4 stripped only a leading `./`, so such aliases warned "not found" and stayed raw.
7. Project files are parsed as JSONC (comments, trailing commas), as Rojo parses them. A JSONC project made v1.2.4 log an error and skip that scope. `.luaurc` stays strict JSON, warn-and-skip on a parse error, as in v1.2.4.
8. Where v1.2.4 depended on Lua table order (alias order, `readDir` order, unstable `table.sort` of scopes and of the replacement log), 2.0 sorts: aliases by name, directory entries by name, scopes by depth descending then path ascending, log rows by original-require length, then label (the instance path for a project row, the file path for an orphan row), then position. With `-v`, every scope's set-up messages (sourcemap scan, alias lines, skips) print before any rewrite message, where v1.2.4 interleaved them scope by scope; a sourcemap never depends on script contents, so building all scopes first changes nothing else. No vfx-forge output depends on any of these orders (section 6.3 shows why for aliases).
9. The workspace is a per-run directory holding only what the build reads (section 6.4), not a full copy of the project directory. `.luaurc` files outside it (worktrees, `dist/staging` when not mounted) are no longer processed; they could not change the model.
10. The replacement table at startup and its `INFO Done in <ms>ms` line print only with `-v`, in convert and build mode alike (owner ruling 2026-10-31; v1.2.4 always printed them, thousands of lines on vfx-forge). Warnings print as before. In watch mode, per-batch replacement rows also print only with `-v`. Test: `alias_resolution::replacement_table_prints_only_with_verbose`.
11. `--watch` together with `--convert-to` prints a `WARN` that watch is ignored (v1.2.4 ignored it silently).
12. A project `$path` that resolves outside the project directory is an error naming the path. v1.2.4's workspace held only the project directory, so Rojo failed on it there too, less clearly.
13. Folder derivation recognizes an init file by its exact name (`init.lua`, `init.luau`). v1.2.4 used the unanchored Lua pattern `init.luau$`, whose `.` matches any character, so a folder whose first file child was `reinit.luau` was derived one level too high (`/p/src` instead of `/p/src/lib`). Test: `sourcemap::tests::folder_path_init_match_is_exact`.
14. Convert mode fails (`ERR`, exit 1) instead of crashing or carrying on: when the project directory does not exist, when DIR exists and is a file (v1.2.4 crashed on both), and when a write fails while copying the project (v1.2.4 skipped any file it could not copy, so a full disk ended in a silently partial output; unreadable sources are still skipped). Tests: `convert_safety::missing_project_dir_is_an_error`, `convert_safety::dir_that_is_a_file_is_an_error`, `workspace::tests::copy_tree_fails_on_a_write_error`.
15. Flattening resolves each child `$path` against its own project file's directory, compares paths after lexical normalization (v1.2.4 compared raw strings, so `"./src/lib"` under a root `"src/"` counted as outside and was deleted while being moved onto itself), and never moves anything outside the converted tree. v1.2.4 resolved every project's child paths against the root of the copy, so a nested project with an outside mount (vfx-forge's `pkg/forge-vfx`, mounting `pkg`) moved the outer project's `pkg`, which held the nested project itself, and then crashed writing the nested project file (`util.luau:592`); on vfx-forge v1.2.4's convert never finished. A child that cannot be moved is left as it is with `WARN "<key>" cannot be moved into "<root>", so its project entry is left as it is`: its path leaves the converted tree (v1.2.4 resolved `../outside` against DIR and moved the directory beside DIR into the output), its key is not a single name, it contains the root, or it names a project file. A child whose path does not exist is left as written (v1.2.4 dropped its entry); an `{"optional": <path>}` child is flattened like a string one (v1.2.4 crashed on it). A root `$path` that is not a directory in the copy flattens nothing (v1.2.4 crashed on a file root). A move inside the tree that takes a directory another scope still needs is kept, as v1.2.4 kept it (refusing could break a legitimate sibling mount), and warned about: `WARN "<moved>" was moved into "<dest>", so the project of "<.luaurc>" is not flattened` when a later scope's `.luaurc` went with it, and `WARN "<moved>" was moved into "<dest>" but "<project file>" still mounts it` when a scope's project still names a `$path` at or under it. Paths are relative to the converted tree; project files that belong to no scope are not checked. Tests: `flattening::nested_project_paths_resolve_against_its_own_dir`, `flattening::inside_path_spelled_differently_is_not_moved`, `flattening::optional_and_missing_child_paths`, `flattening::path_outside_the_converted_tree_is_left_alone`, `flattening::moving_a_dir_another_scope_needs_is_warned`, `flattening::ordinary_flatten_warns_about_nothing`.
16. A flattened file mount keeps the suffix Rojo reads its class from, and its meta file: `"Runner": {"$path": "lib/run.server.luau"}` moves to `<root>/Runner.server.luau` and `lib/run.meta.json` to `<root>/Runner.meta.json`. The kept suffixes are `.server`, `.client`, `.plugin` and `.model` before the extension (the ones Rojo 7.6.1 strips), else the last extension alone. v1.2.4 moved the file to `<root>/Runner`, which Rojo ignores, and dropped the entry, so the script vanished from the converted build. Test: `flattening::file_child_keeps_its_suffix_and_meta`.
17. The rewritten project file keeps its key order, its empty arrays, and the difference between an integer and a float: `5` stays `5` and `1.0` stays `1.0`. A float is written in its shortest form, so `1.50` comes out as `1.5` and `1e3` as `1000.0`. v1.2.4 round-tripped the file through a Lua table, which sorted the keys, turned `[]` into `{}` (which Rojo rejects for `globIgnorePaths`) and wrote `1.0` as `1`. Both write pretty JSON with two-space indents and no trailing newline, so a project whose keys were already sorted comes out byte-identical; a JSONC project loses its comments (v1.2.4 could not parse it). Tests: `convert_structure::converted_project_keeps_key_order`, `convert_structure::converted_project_keeps_integers_and_floats_apart`.

## 6. Behavior kept, precisely

This section is the port's reference. Line numbers are v1.2.4.

### 6.1 Scope discovery and order

- A scope is one `.luaurc`. Its project file comes from `findProjectFileForLuaurc` (`util.luau:448`): in the `.luaurc`'s own directory, the CLI's project file name when the `.luaurc` sits in the project root, else `default.project.json`, `default.project.jsonc`, `project.json`, `project.jsonc`, else the first `*.project.json(c)` (2.0: first by name).
- A scope with a project file is a project scope. Without one it is an orphan scope, processed only under `--process-orphans` and otherwise skipped (a `WARN` under `-v`).
- A `.luaurc` that fails to parse or has no `aliases` (or an empty map) skips its scope with the v1.2.4 messages (`replaceAliases.luau:21-34`, `:172-185`).
- Scopes run deepest first (`init.luau:246`); the output of a deeper scope is the input of a shallower one. A file is rewritten by every scope that covers it, in that order. This is what makes `pkg/forge-vfx`'s own `@vfx` (= `./src/effects`) win over the root's `@vfx` (= `./pkg/forge-vfx/src`), and what makes the root resolve `src/mod/emitter/worker`'s `@vfx` when orphans are off.
- Which files a scope covers: a project scope covers the script files in its node index (6.2); an orphan scope covers every `.lua`/`.luau` file under its directory (`replaceAliases.luau:40-59`).

### 6.2 The node index of a project scope (`replaceAliases.inFiles`, `:156-369`)

- Run `rojo sourcemap --absolute <project>` (2.0 passes `--absolute`, present since Rojo 7.4, measured on 7.6.1). Relative paths in v1.2.4 were relative to the project file's directory; absolute paths remove that question.
- Walk every node below the root, carrying the chain of ancestor names. A node with `filePaths` registers `filePaths[0]` (the first entry only). A node without registers its derived folder path (`tryGetFolderPath`, `util.luau:205-274`, ported exactly except for divergence 13, including the preference for a plain file child over a nested project child and the depth bookkeeping), plus `<folder>/init.luau` or else `<folder>/init.lua` if that file exists. A folder with no derivable path logs `WARN Folder '<name>' has no files, skipping`.
- Every registered path maps to the node's instance path: root name, ancestors, the node itself.
- Then the project's root `$path` registers itself (if not already) with instance path `[root name]`, and so does `<root>/init.luau` (else `init.lua`) if it exists. A root without `$path` registers nothing (v1.2.4 crashed).
- Each alias resolves by looking up `<scope project dir>/<alias path>`, then `<that>/init.luau`, then `<that>/init.lua`. A miss logs the two v1.2.4 warnings (`was not found in the sourcemap.` and `Looking for: <path>`) and the alias stays raw. If no alias resolves: `WARN No valid aliases found for <project>`, scope skipped.

### 6.3 Rewriting one file

- Matcher: one regex per scope, `require\(["']@(<aliases>)(?:["']\)|/([^"']+)["']\))`, alternatives escaped with `regex::escape` and sorted longest first. Same reach as v1.2.4's two patterns per alias: no whitespace inside `require(`, either quote on either side, matches inside comments and strings too. One pass per scope; a replacement is never rescanned within the same scope. v1.2.4 applied aliases one after another, so a replacement could be rescanned by an alias that came later in table order. A project replacement starts with `.` or `@self`, and an `@self/rest` result arises only from the root init, whose context path is empty. When the scope's `self` alias targets the project root (the self-alias fixture, the usual case), rewriting `@self/rest` from that empty context gives `@self/rest` again, a fixed point, so the single pass gives the same text. When `self` targets anything else, or an orphan value itself starts with `@` and names another alias of the same scope, v1.2.4 rewrote the require a second time (or not, depending on table order) and 2.0 rewrites it exactly once: a fix, not a parity break.
- The replacement is always written with double quotes: `require("<path>")`.
- Context path: the file's instance path. For an init file (`init.lua`, `init.luau`): the root init drops its last element (empty context); a nested init under `--wax-compat` appends an empty sentinel element; otherwise it is unchanged (`replaceAliases.luau:401-412`).
- Relative path (`util.getRelativePath`, `:309-341`): common prefix `c`; ups = `len(from) - c - 1`, floored at 0; zero ups emits `.`, otherwise that many `..`; then the target's remaining names. Subpath appended with `/`.
- `@self` conversion (`convertToSelfPath`, `replaceAliases.luau:116-154`): `./<Root>/rest` becomes `@self/rest`; `../../<Root>/rest` (only `..` before the root name) becomes `@self/rest`. `<Root>` is the scope sourcemap's root name.
- Orphan scope: literal, `require("@a")` becomes `require("<luaurc value>")` and `require("@a/x")` becomes `require("<luaurc value>/x")`, with the value exactly as written in the `.luaurc`.
- Log rows: `<instance path joined by />` (orphans: the workspace-relative file path), original require, new require. Printed at startup, with `-v` only (divergence 10), under `OK   Processed project: <original project path>` (orphans suffixed ` (orphan)`), padded to common widths, then `INFO Done in <ms>ms`.

### 6.4 Convert mode (`init.luau:201-388`, `util.flattenTreePaths:498-594`)

- The output directory is wiped and the whole project directory is copied into it (or into the workspace first when DIR is inside the project, then copied over). Skipped by name at any depth: `.git`, `node_modules`, `.luau-lsp`, `.vscode`. Symlinks are followed (2.0 guards cycles); unreadable entries are skipped.
- Every `.luaurc` in the copy (except under DIR) is processed as 6.1 to 6.3, in place.
- Then, for each scope's project, deepest first, child `$path`s outside the root `$path` move into `<root>/<key>` (resolved against that project file's directory, divergence 15; a file keeps its suffix and meta file, divergence 16); an entry that held only `$path` is dropped, one with more is kept with the new path and the v1.2.4 warning (`"<key>" was moved into "<root>" but its project entry carries more than a $path, so it is kept and Rojo will build two instances named "<key>"`). The project file is rewritten as pretty JSON only when something moved (2.0 keeps key order, divergence 17).
- `OK   Project converted successfully to <DIR>`, exit 0.

### 6.5 Build mode workspace (new design, same model)

v1.2.4 copied the whole project dir. 2.0 mirrors only what Rojo and the scopes read:

- Mount closure: the CLI project file, every `$path` in its tree (string or `{ "optional": path }`), and recursively the trees of any project file reached: a `$path` naming a `*.project.json`, and every `default.project.json` found inside a mounted directory (Rojo mounts those as nested projects). Plus every scope project file and its own closure. Iterated to a fixpoint.
- Scope discovery (6.1) in build mode covers `.luaurc` files inside mounted directories and in every directory from the project dir down to each mount root (so `vendor/.luaurc` counts for a `vendor/shared` mount).
- Mirror: `<ws>/tree/<rel>` for every file under the closure, same skip names and symlink rules as 6.4; script files are written de-aliased, everything else is copied byte for byte.
- Rojo runs as `rojo build <ws>/tree/<project file> -o <ws>/out/build.<ext>` (`--color` and `-v` passed on), with the cwd set to the original project directory, so a rokit shim or mise finds its manifest. Sourcemaps run with the same cwd.

Why this gives the same model: Rojo reads only the project file and the files under its closure; every scope whose processing could change one of those files has its `.luaurc` in the project dir, in an ancestor of a mount, or inside a mount, and is still processed; and all other scopes only ever modified files Rojo never read. Section 9 checks it on vfx-forge.

### 6.6 Rojo stays a binary

`rojo build` is 20-45 ms of a save's budget. The `rojo` crate (7.7.1 on crates.io) exposes no stable library API, would pin the user to the Rojo version we compile in (vfx-forge pins Rojo through mise, separately from this tool), and would put Rojo's whole dependency tree in our build. No route is clearly better, so the binary stays.

## 7. Architecture

A lib crate (`rojo_build_noalias`, so integration tests can drive the pipeline) and a thin bin `rojo-build-noalias`.

| Module | Responsibility |
|---|---|
| `cli` | clap definition, help text, project and DIR resolution, `Mode` |
| `log` | prefixes, color choice, broken-pipe-safe writes |
| `shutdown` | SIGINT/SIGTERM/SIGHUP and EPIPE into one shutdown request |
| `paths` | lexical normalize, skip names, script and init predicates, `/`-joined display |
| `luaurc` | strict-JSON alias map |
| `project` | project lookup, JSONC read, root `$path`, mount closure, flatten |
| `sourcemap` | parse `rojo sourcemap --absolute` output, node index, folder derivation |
| `dealias` | matcher, context path, relative path, `@self`, project and orphan rewrite |
| `scopes` | discovery, ordering, the per-file scope chain |
| `workspace` | per-run temp dir, lock, stale sweep, mirror, write-if-changed, prune |
| `rojo` | invoking rojo (`sourcemap` and `build`), forwarding lines, swallowing its marker |
| `output` | temp file beside the output, atomic rename, retry on Windows |
| `report` | the startup replacement table |
| `convert` | convert mode |
| `pipeline` | build mode state: mounts, snapshot, raw source cache, scopes; start, apply, build |
| `watch` | notify watcher, batcher, hint diff, the loop |

### 7.1 Workspace

`tempfile::Builder::new().prefix("rojo-build-noalias.").tempdir()` under the system temp dir (canonicalized with `dunce`), so v1.2.4's `rm -rf /tmp/rojo-build-noalias/*` cannot reach it. Layout: `tree/`, `out/`, `lock`. The run holds an exclusive `File::try_lock` on `lock` for its lifetime. At startup each `rojo-build-noalias.*` directory at least 10 s old whose lock can be taken, or that has no lock file at all (a run that died between creating its dir and its lock), belongs to a dead run and is removed (SIGKILL leaves one behind; the next run collects it); a locked one belongs to a live run and is left alone, and a younger one may belong to a run that has not taken its lock yet. The `TempDir` drop removes ours on every orderly exit.

### 7.2 Watch mode

- Watcher: `notify` 8.2 `RecommendedWatcher` (inotify, FSEvents, ReadDirectoryChangesW). Recursive watches on each mounted directory; non-recursive watches on the parents of mounted files, of project files and of `.luaurc` files, and on every directory from the project dir to each mount root (catches a new `.luaurc`, a project edit, a mount root that is renamed or recreated). Events under a skipped name, outside the closure, or on the output file and our temp files are dropped at the source.
- Events are hints, never truth. They carry paths into a `Batcher`: a batch closes when 200 ms pass with no new relevant event, or 2 s after its first event if events never stop (a checkout, a formatter run). One batch is one diff, one apply and at most one build. Notify's rescan flag (inotify queue overflow, FSEvents `MustScanSubDirs`) or a watcher error turns the batch into a full rescan.
- Why no debouncer crate: `notify-debouncer-full` and `-mini` time each path separately, so a 5-file burst spread over 100 ms can close as two or three batches, and their rename pairing and file-id cache duplicate what the diff below already gets right. The batch rule is a global quiet window, about 40 lines, tested with a fake clock.
- Diff (`ChangeSet { added, removed, changed, config }`): for each hinted path, stat it. Gone: remove it, and its subtree if it was a directory. A directory: compare its immediate children with the snapshot (added and removed), and walk any added subdirectory in full. A file: a script is changed when its bytes differ from the cached raw source (mtime plays no part, which fixes the lost second save); any other file is changed when length or full-precision mtime differ. `config` is set when a `.luaurc` or any project file in the closure is hit.
- Apply, in order: update the raw cache and the snapshot; create and remove mirror entries (directories with `create_dir_all`; a new folder never crashes); if `config`, recompute the mount closure and scopes, re-register watches, and mirror newly mounted paths and prune unmounted ones; if any entry was added or removed or any `.json` file changed, regenerate the project scopes' sourcemaps (parallel, one `rojo sourcemap` per scope); mark dirty every changed or added script, every script whose context path or scope chain changed, and every script of a scope whose resolved alias table changed; de-alias the dirty set from the raw cache with the same scope chain as startup; write only outputs whose bytes differ.
- Raw bytes can sit in the mirror only inside apply (a new script exists raw while its scopes' sourcemaps are regenerated); `rojo build` runs only after apply returns, so Rojo never builds a raw file.
- Build: when apply changed any mirror bytes (or the previous build failed), run one `rojo build` to `<ws>/out/build.<ext>`, then `output::swap` into place and print the marker. A touch that changes no bytes builds nothing.
- Errors: any error in diff, apply or build (unparseable project, `rojo` failure, I/O error, a panic caught at the batch boundary) prints `ERR <message>` and `WARN keeping the last good build at <OUTPUT>`, leaves the output untouched and waits for the next batch, which runs a full rescan. A `.luaurc` that parsed before and fails now is such an error in watch mode: its previous aliases stay in force until it parses again (startup keeps v1.2.4's warn-and-skip).
- Startup: start the pipeline (mirror, scopes, de-alias, first build), register the watches, then run one full-rescan batch, which catches anything that changed during startup and is otherwise a no-op. An initial build failure keeps watching.
- Shutdown: `ctrlc` with `termination` (SIGINT, SIGTERM, SIGHUP; Ctrl-C and close on Windows) and an EPIPE on stdout (dev.sh's pipeline died) both request shutdown through the event channel; the loop kills a running rojo child, drops the workspace and exits 0.
- Idle: the loop blocks on the channel with no timeout while no batch is open. No polling anywhere.

### 7.3 Output swap

`tempfile::NamedTempFile::with_prefix_in(".rbn-", <output dir>)`, copy the built file in, `persist(output)` (a rename on the same filesystem). Studio loads only `.rbxm`, `.rbxmx`, `.lua` and `.luau` from its plugins folder, so the extensionless temp name is never loaded. On Windows a rename onto a file Studio holds open can fail: retry 10 times 50 ms apart, then copy over the output with a `WARN`. In plugin mode Rojo writes the plugin itself (not atomic; unchanged from v1.2.4).

## 8. Crates

| Crate | Version | Why |
|---|---|---|
| `clap` (derive) | 4.6 | the CLI; custom help keeps v1.2.4's text |
| `notify` | 8.2 | native events on all three OSes; 9.0 is still rc |
| `regex` | 1.13 | the combined matcher; its literal prefilter on `require(` already uses aho-corasick, so that crate is not a direct dependency |
| `serde`, `serde_json` (`preserve_order`) | 1, 1.0 | sourcemap and `.luaurc`; key order kept when convert rewrites a project file |
| `jsonc-parser` (`serde`) | 0.34 | project files as Rojo reads them |
| `tempfile` | 3.27 | per-run workspace, output temp file and persist |
| `walkdir` | 2.5 | mirror walks with `follow_links`, loop detection and `filter_entry` for skip names |
| `ctrlc` (`termination`) | 3.5 | orderly shutdown so the workspace is removed |
| `anyhow` | 1.0 | error context in messages |
| `dunce` | 1.0 | canonical paths without `\\?\` on Windows, which Rojo would otherwise receive |
| `anstream`, `anstyle` | as resolved by clap 4.6 | colored prefixes honoring `--color`; already in the tree |
| dev: `assert_cmd` | 2.2 | run the binary in tests |
| dev: `rbx_binary`, `rbx_dom_weak` | 3.0, 4.2 | read built models in tests and the parity diff; never in the shipped binary |

Not used: `notify-debouncer-full`/`-mini` (7.2), `aho-corasick` directly (above), `rojo`/librojo (6.6), `libc` (the lock replaces pid probing). Toolchain: edition 2024, `rust-version = "1.89"` (`File::try_lock`), `rust-toolchain.toml` pins 1.94.1, `Cargo.lock` committed.

## 9. Proving parity with v1.2.4

1. Freeze a snapshot of vfx-forge (`rsync -a` excluding `.git` and `node_modules` anywhere and `/.worktrees`, `/.claude`, `/dist` at the top; `/dist/staging`, `/dist/empty` and `/dist/link` copied when present). Both tools build the same frozen bytes; vfx-forge itself is only read. The snapshot holds `src/.env.luau`, so it lives in a temp dir that is deleted afterwards.
2. Download v1.2.4's `rbn-1.2.4-linux-x86_64.zip` with `gh release download`. Put the real rojo 7.6.1 on PATH (`mise which rojo` in vfx-forge), because v1.2.4 runs Rojo inside its `/tmp` workspace where a rokit shim finds no manifest.
3. Build v1.2.4 twice and check its two outputs are identical (Rojo was measured deterministic on the fixtures; this confirms it at scale). Then build with 2.0 and `cmp`. Cases: `dev.project.json`; `default.project.json`; `bundle.project.json --process-orphans` when the snapshot has `dist/staging`; every fixture under `tests/fixtures` with a project.
4. `--convert-to` on `pkg/forge-vfx` and on the vfx-forge snapshot: `diff -r` of the two output trees, project files compared after `jq -S`. v1.2.4 crashes in flatten on the vfx-forge snapshot (divergence 15), so there the comparison is per script: each of v1.2.4's scripts against 2.0's at the path flattening gives it, and the Rojo builds of the original snapshot and of 2.0's converted tree must hold the same instances (names and classes).
5. If a model differs in bytes, `cargo run --example rbxm_diff -- old new` (rbx_binary) lists the first differing instances and properties. Semantic identity (same tree, names, classes and properties) is the fallback bar, and the cause of any byte difference is written into the parity commit's body.
6. The same script times startup for both tools (the 3 s goal).

## 10. Tests

- Unit tests in each module for the pure parts: relative path, `@self` conversion, context path, matcher edge cases (`vfx` against `vfx-pkg`, mixed quotes, whitespace not matched, comments matched), folder derivation on the fixtures' sourcemaps, the batcher on a fake clock, the diff on temp trees.
- Ported integration tests (tests/init.luau, one Rust test per Lua test, same fixtures and assertions): alias resolution (5), flattening (3), convert structure (with a Rojo build and an rbx_binary child count), orphans (2), normal build, the forge-vfx canary (skipped when the checkout is absent).
- New fixtures: `nested-luaurc` (an orphan `.luaurc` inside the root project, modeled on `src/mod/emitter/worker`) and `named-project` (a `dev.project.json` that mounts a stories folder `default.project.json` does not, the f0a7e46 case).
- Watch tests, each spawning the binary with `--watch` on a fixture copy: a new folder with a module, a delete, a file rename, a folder rename, two saves 1.5 s apart, a burst of 5 files giving exactly one build, a nested-`.luaurc` edit, a broken project edit and a broken `.luaurc` edit each keeping the last good build and recovering, an editor-style atomic save, a checkout-sized stream, a mount root deleted and recreated, churn outside the mounts building nothing, the output inside a mount not looping, SIGTERM and a closed stdout each removing the workspace, idle CPU (Linux). Every watch test ends with the oracle: the watch output equals, byte for byte, a fresh one-shot build of the same tree. Every landed build is checked for raw aliases.

## 11. Release

- `.github/workflows/build.yml` on `v*` tags: a test job (`cargo test --locked` on ubuntu, macos and windows, Rojo from `rokit.toml` through `CompeyDev/setup-rokit`), a build matrix, and the existing `anton-yurchenko/git-release@v6` step.
- Assets keep their names: `rbn-<version>-<os>-<arch>.zip` for linux-x86_64, linux-aarch64, windows-x86_64, windows-aarch64, macos-x86_64, macos-aarch64, each holding `rojo-build-noalias` (`.exe` on Windows) at the zip root, which is what rokit's and ubi's asset matching and exe lookup found in 1.2.x. Linux builds target musl (static, no glibc floor). The `rojo-build-noalias.luau` asset is dropped.
- The repo loses the Luau tool (`src/*.luau`, `tests/init.luau`, `.darklua.json`, `stylua.toml`, `.luaurc`) and keeps `rokit.toml` with only `rojo` (tests run it). README gains the watch-mode guarantees.
- Commit `version 2.0.0`. Tagging `v2.0.0` and pushing are the owner's steps.

## 12. vfx-forge after the release

- `mise.toml`: `"ubi:zilibobi/rojo-build-noalias" = "2.0.0"`.
- `scripts/bash/dev.sh`: drop `rm -rf /tmp/rojo-build-noalias/*` and its comment; the marker stays one `case` on `Built project to`, which now fires once per build; the supervisor loop stays (cheap insurance) with its comment updated.
- `docs/references/engine-test-lane.md`: the sections on new directories being missed, the shared `/tmp` cache, deletions not propagating and the wedge recovery are rewritten for 2.0.
- Verified live: one `[dev] rebuild` per save; new folder, delete and rename reach Studio (`shared.forge_dev.stamp` advances); a full `build.sh` passes.

## 13. Risks

- FSEvents and Windows event delivery differ from inotify; the hint-plus-diff design and the full rescan on overflow make missed details recoverable, and CI runs the watch tests on all three OSes.
- A parity difference on vfx-forge would block the release; the parity task runs before any watch work lands, so it is found early.
- Rojo output determinism at vfx-forge scale is assumed until step 3 of section 9 confirms it.

## 14. Open for the owner

1. The own 200 ms global-quiet batcher instead of a `notify-debouncer-*` crate (7.2). Swapping in `notify-debouncer-full` is a contained change if preferred, at the cost of split bursts.
2. Whether `pkg/forge-vfx/rokit.toml` (pins `rojo-build-noalias@1.2.4`) moves to 2.0.0 too. It is a separate repo whose `origin` only the owner pushes.
3. Plugin mode (`-p`) stays non-atomic because Rojo writes the plugin itself. Making it atomic means locating the plugins folder ourselves (the `roblox_install` crate Rojo uses).
4. Divergence 1 (exit 1 on a failed build) will make a `build.sh` run stop where it used to continue silently. That is intended; say so if anything relies on the old behavior.
5. Settled 2026-10-31: the startup replacement table (thousands of lines on vfx-forge) moved behind `-v` (divergence 10).
