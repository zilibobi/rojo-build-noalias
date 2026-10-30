use clap::{ArgAction, Parser, ValueEnum};
use regex::Regex;
use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum ColorChoice {
    Auto,
    Always,
    Never,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Options {
    pub process_orphans: bool,
    pub wax_compat: bool,
    pub verbose: u8,
    pub color: ColorChoice,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub dir: PathBuf,
    pub file_name: Option<String>,
}

impl Target {
    /// The project file or directory as seen from `root`.
    pub fn project_in(&self, root: &Path) -> PathBuf {
        match &self.file_name {
            Some(name) => root.join(name),
            None => root.to_path_buf(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OutputTarget {
    File(PathBuf),
    Plugin(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BuildArgs {
    pub target: Target,
    pub output: OutputTarget,
    pub watch: bool,
    pub opts: Options,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConvertArgs {
    pub target: Target,
    pub out_dir: PathBuf,
    pub opts: Options,
    pub watch_ignored: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Mode {
    Help,
    Version,
    Build(BuildArgs),
    Convert(ConvertArgs),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CliError {
    /// exit 2
    Usage(String),
    /// exit 1
    Refused(String),
}

#[derive(Parser)]
#[command(
    name = "rojo-build-noalias",
    disable_help_flag = true,
    disable_version_flag = true
)]
struct Raw {
    #[arg(value_name = "PROJECT")]
    project: Option<PathBuf>,
    #[arg(short = 'o', long = "output", value_name = "OUTPUT")]
    output: Option<PathBuf>,
    #[arg(
        short = 'p',
        long = "plugin",
        value_name = "PLUGIN",
        conflicts_with = "output"
    )]
    plugin: Option<String>,
    #[arg(long = "watch")]
    watch: bool,
    #[arg(short = 'v', long = "verbose", action = ArgAction::Count)]
    verbose: u8,
    #[arg(
        long = "color",
        value_enum,
        default_value = "auto",
        value_name = "COLOR"
    )]
    color: ColorChoice,
    #[arg(long = "convert-to", value_name = "DIR")]
    convert_to: Option<PathBuf>,
    #[arg(long = "process-orphans")]
    process_orphans: bool,
    #[arg(long = "wax-compat")]
    wax_compat: bool,
    #[arg(short = 'h', long = "help")]
    help: bool,
    #[arg(short = 'V', long = "version")]
    version: bool,
}

/// Lexical normalization: drops `.` and trailing separators, folds `..` against a preceding name.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();

    for comp in path.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                let pops = matches!(out.components().next_back(), Some(Component::Normal(_)));

                if pops {
                    out.pop();
                } else if !matches!(
                    out.components().next_back(),
                    Some(Component::RootDir | Component::Prefix(_))
                ) {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }

    out
}

pub fn resolve_target(arg: Option<&Path>, cwd: &Path) -> Target {
    let Some(arg) = arg else {
        return Target {
            dir: normalize(cwd),
            file_name: None,
        };
    };

    let full = normalize(&cwd.join(arg));

    if full.is_file() {
        let file_name = full.file_name().map(|n| n.to_string_lossy().into_owned());

        if let (Some(file_name), Some(parent)) = (file_name, full.parent()) {
            return Target {
                dir: parent.to_path_buf(),
                file_name: Some(file_name),
            };
        }
    }

    Target {
        dir: full,
        file_name: None,
    }
}

pub fn version_line() -> String {
    format!("rojo-build-noalias {}", env!("CARGO_PKG_VERSION"))
}

fn usage(msg: &str) -> CliError {
    CliError::Usage(msg.to_string())
}

pub fn parse(args: Vec<OsString>, cwd: &Path) -> Result<Mode, CliError> {
    let wants_help = args.iter().any(|a| a == "-h" || a == "--help");

    if args.is_empty() || wants_help {
        return Ok(Mode::Help);
    }

    let mut argv = vec![OsString::from("rojo-build-noalias")];
    argv.extend(args);
    let raw = Raw::try_parse_from(argv).map_err(|e| CliError::Usage(e.to_string()))?;

    if raw.version {
        return Ok(Mode::Version);
    }

    if raw.plugin.is_some() && cfg!(target_os = "linux") {
        return Err(CliError::Refused(
            "The '--plugin' option is not supported on Linux.".to_string(),
        ));
    }

    let opts = Options {
        process_orphans: raw.process_orphans,
        wax_compat: raw.wax_compat,
        verbose: raw.verbose,
        color: raw.color,
    };
    let target = resolve_target(raw.project.as_deref(), cwd);

    if let Some(dir) = raw.convert_to {
        let out_dir = normalize(&cwd.join(dir));

        if target.dir.starts_with(&out_dir) {
            return Err(CliError::Refused(
                "refusing to convert into the project directory or one of its parents".to_string(),
            ));
        }

        return Ok(Mode::Convert(ConvertArgs {
            target,
            out_dir,
            opts,
            watch_ignored: raw.watch,
        }));
    }

    let output = match (raw.output, raw.plugin) {
        (Some(file), _) => OutputTarget::File(file),
        (None, Some(plugin)) => OutputTarget::Plugin(plugin),
        (None, None) => {
            return Err(usage(
                "You must specify an output filename for the build with either '-o' or '--output' (or '-p' for a plugin)",
            ));
        }
    };

    Ok(Mode::Build(BuildArgs {
        target,
        output,
        watch: raw.watch,
        opts,
    }))
}

const HELP: &str = "\
A wrapper around the Rojo build command that replaces Luau alias requires with relative paths.

This tool automatically processes all .luaurc files in your project (including nested packages)
and replaces alias-based requires (e.g. @alias/path) with relative path requires.

USAGE:
    rojo-build-noalias [OPTIONS] [PROJECT]

ROJO ARGS:
    <PROJECT>   Path to the project to serve. Defaults to the current directory [default: ]

OPTIONS:
        --convert-to <DIR>      Convert project to directory with aliases replaced, then exit (no build)
        --process-orphans       Treat orphan .luaurc files (without project files) as direct text replacements
        --wax-compat            Emit init.luau paths with one extra `..` for wax's pre-RFC bundle shim

ROJO OPTIONS:
        --color <COLOR>         Set color behavior. Valid values are auto, always, and never [default: auto]
    -h, --help                  Print help information
    -o, --output <OUTPUT>       Where to output the result
    -p, --plugin <PLUGIN>       Output the result to the local Roblox Studio plugins folder (not supported on Linux)
    -v, --verbose               Sets verbosity level. Can be specified multiple times
    -V, --version               Print version information
        --watch                 Whether to automatically rebuild when any input files change
";

pub fn help_text(color: bool) -> String {
    let first = format!("rojo-build-noalias {}", env!("CARGO_PKG_VERSION"));

    if !color {
        return format!("{first}\n{HELP}");
    }

    let paint = |code: &str, text: &str| format!("\x1b[{code}m{text}\x1b[0m");

    // the same four passes as v1.2.4, in the same order
    let headers = Regex::new(r"[A-Z][A-Z ]*:").unwrap();
    let placeholders = Regex::new(r"<[A-Z]+>").unwrap();
    let short = Regex::new(r" (-[^- ]+),").unwrap();
    let long = Regex::new(r"--[A-Za-z?-]+").unwrap();

    let body = headers.replace_all(HELP, |c: &regex::Captures| paint("33", &c[0]));
    let body = placeholders.replace_all(&body, |c: &regex::Captures| paint("32", &c[0]));
    let body = short.replace_all(&body, |c: &regex::Captures| {
        format!(" {},", paint("32", &c[1]))
    });
    let body = long.replace_all(&body, |c: &regex::Captures| paint("32", &c[0]));

    format!(
        "{} {}\n{body}",
        paint("32", "rojo-build-noalias"),
        env!("CARGO_PKG_VERSION")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<OsString> {
        list.iter().map(OsString::from).collect()
    }

    fn build(list: &[&str], cwd: &Path) -> BuildArgs {
        match parse(args(list), cwd) {
            Ok(Mode::Build(b)) => b,
            other => panic!("expected a build, got {other:?}"),
        }
    }

    #[test]
    fn resolve_existing_project_file() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("dev.project.json"), "{}").unwrap();
        let t = resolve_target(Some(Path::new("dev.project.json")), tmp.path());
        assert_eq!(t.dir, tmp.path());
        assert_eq!(t.file_name.as_deref(), Some("dev.project.json"));
        assert_eq!(
            t.project_in(Path::new("/ws")),
            Path::new("/ws/dev.project.json")
        );
    }

    #[test]
    fn resolve_existing_dir_missing_path_and_none() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("proj")).unwrap();

        let t = resolve_target(Some(Path::new("proj")), tmp.path());
        assert_eq!(t.dir, tmp.path().join("proj"));
        assert_eq!(t.file_name, None);
        assert_eq!(t.project_in(Path::new("/ws")), Path::new("/ws"));

        let t = resolve_target(Some(Path::new("nope/x")), tmp.path());
        assert_eq!(t.dir, tmp.path().join("nope/x"));
        assert_eq!(t.file_name, None);

        assert_eq!(resolve_target(None, tmp.path()).dir, tmp.path());
        assert_eq!(
            resolve_target(Some(Path::new("proj///")), tmp.path()).dir,
            tmp.path().join("proj")
        );
    }

    #[test]
    fn flags_after_the_positional() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("dev.project.json"), "{}").unwrap();
        let b = build(&["dev.project.json", "--output", "x.rbxm"], tmp.path());
        assert_eq!(b.output, OutputTarget::File(PathBuf::from("x.rbxm")));
        assert_eq!(b.target.dir, tmp.path());
        assert_eq!(b.target.file_name.as_deref(), Some("dev.project.json"));
        assert!(!b.watch);
    }

    #[test]
    fn verbose_counts_and_color_parses() {
        let cwd = Path::new("/p");
        assert_eq!(build(&["-v", "-v", "-o", "x.rbxm"], cwd).opts.verbose, 2);
        assert_eq!(build(&["-vv", "-o", "x.rbxm"], cwd).opts.verbose, 2);
        let b = build(&["--color", "never", "-o", "x.rbxm", "--watch"], cwd);
        assert_eq!(b.opts.color, ColorChoice::Never);
        assert!(b.watch);
    }

    #[test]
    fn convert_to_resolves_and_refuses_the_project() {
        let tmp = tempfile::tempdir().unwrap();
        let cwd = tmp.path();

        match parse(args(&["--convert-to", "./out/"]), cwd) {
            Ok(Mode::Convert(c)) => {
                assert_eq!(c.out_dir, cwd.join("out"));
                assert!(!c.watch_ignored);
            }
            other => panic!("{other:?}"),
        }

        for dir in [".", ".."] {
            match parse(args(&["--convert-to", dir]), cwd) {
                Err(CliError::Refused(m)) => assert!(m.contains(
                    "refusing to convert into the project directory or one of its parents"
                )),
                other => panic!("{other:?}"),
            }
        }

        match parse(args(&["--convert-to", "out", "--watch"]), cwd) {
            Ok(Mode::Convert(c)) => assert!(c.watch_ignored),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn help_and_usage_modes() {
        let cwd = Path::new("/p");
        assert_eq!(parse(vec![], cwd), Ok(Mode::Help));
        assert_eq!(parse(args(&["-o", "-h"]), cwd), Ok(Mode::Help));
        assert_eq!(parse(args(&["-V"]), cwd), Ok(Mode::Version));
        assert!(matches!(
            parse(args(&["--watch"]), cwd),
            Err(CliError::Usage(_))
        ));
        assert!(matches!(parse(args(&["-o"]), cwd), Err(CliError::Usage(_))));
    }

    #[test]
    fn help_text_color_only_when_asked() {
        assert!(!help_text(false).contains('\x1b'));
        let c = help_text(true);
        assert!(c.contains("\x1b[33mUSAGE:\x1b[0m"));
        assert!(c.contains("\x1b[32m<PROJECT>\x1b[0m"));
        assert!(c.contains(" \x1b[32m-h\x1b[0m,"));
        assert!(c.contains("\x1b[32m--convert-to\x1b[0m"));
    }
}
