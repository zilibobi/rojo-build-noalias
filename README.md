## rojo-build-noalias

A wrapper around the [Rojo](https://github.com/rojo-rbx/rojo) *build* command, written in Rust.

## Motivation

Luau LSP does not currently support string requires that map based on the `sourcemap.json` file.

A workaround is to use aliases, but they are not currently supported in Roblox.

This tool replaces aliases with relative string requires based on your project's sourcemap.

## Showcase

add video here

## Installation

### With [Rokit](https://github.com/rojo-rbx/rokit)

```sh
rokit add zilibobi/rojo-build-noalias
```

### With [mise](https://mise.jdx.dev)

```toml
[tools]
"ubi:zilibobi/rojo-build-noalias" = "2.0.1"
```

### Manual

1. Download the latest build from [releases](https://github.com/zilibobi/rojo-build-noalias/releases/latest)
2. Add it to your PATH

## Watch mode

`--watch` builds once, then rebuilds when the files the build reads change. It needs `-o` or `-p`.

- What is watched: every directory your project mounts, the project files, and the `.luaurc` files that apply to them. Changes under `.git`, `node_modules`, `.luau-lsp` and `.vscode` are ignored, and so are changes to the output file itself.
- A batch of changes closes after 200 ms without a new change, or 2 s after its first change if changes never stop (a branch checkout, a formatter run). One batch is one build, however many files it holds.
- A save that changes no bytes builds nothing.
- The last good build is kept. If a build fails (a broken project file, a failing Rojo, an output directory that is gone), the output stays as it was, you get an `ERR` line and a `WARN` that says the last good build was kept, and watching continues. A failing build is retried only when a file really changes again, never on a timer.
- Each run works in its own directory under the system temp directory, so two watchers and a one-shot build can run side by side. Orderly exits remove it.
- On Linux, every watched folder takes an inotify watch. When the limit is reached, a `WARN` names it and how to raise it, for example `sudo sysctl fs.inotify.max_user_watches=524288`. Restart the watch afterwards.

Every build prints `Built project to <OUTPUT>` once, after the output is in place. The output is replaced atomically, so Studio never reads a half-written file.

## Exit codes

| Code | Meaning |
|---|---|
| 0 | Success. In watch mode also SIGINT, SIGTERM and SIGHUP (Ctrl-C and closing the console on Windows), which stop the watch and remove its workspace. |
| 1 | A one-shot build or a conversion failed, or a signal interrupted a one-shot build. |
| 2 | Usage error, such as an unknown flag or a missing value. |
| 130 | A second signal arrived while the first was being handled. The process exits at once and leaves its workspace behind; the next run removes it. |

Watch mode never exits because of a build error.

## 2.0 breaking changes

- A failed one-shot build exits 1 and leaves the previous output as it was. 1.x exited 0 whatever Rojo did, so a script under `set -e` carried on with a stale file. The same holds when the output cannot be written or the project directory does not exist.
- Unknown flags are a usage error (exit 2). 1.x forwarded them to `rojo build`. A missing value after `-o` also exits 2.
- Rojo builds to a temporary file with the output's own extension, so `-o x.rbxmx` receives XML, not binary bytes.
- `--convert-to` refuses a directory that is the project directory or one of its ancestors, and leaves the directory out of the copy when it lies inside the project. 1.x deleted the project in the first case and nested the previous output one level deeper on every run in the second.

## Output

Warnings, errors and the final status line always print. The table of replaced requires (one row per rewritten require, grouped by project) and the time it took print only with `-v`.

## Limitations

- Doesn't work with `rojo serve`
- The rojo `--plugin` option is not supported on Linux
- Only works with projects that build to a model
- Only supports models with 1 root instance
- Every `$path` in a project Rojo reads must stay inside the project directory
