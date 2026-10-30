use rojo_build_noalias::cli::{self, CliError, Mode};
use rojo_build_noalias::log;
use std::process::ExitCode;

fn main() -> ExitCode {
    let cwd = match std::env::current_dir() {
        Ok(cwd) => cwd,
        Err(e) => {
            log::error(&format!("cannot read the current directory: {e}"));
            return ExitCode::from(1);
        }
    };

    let args: Vec<_> = std::env::args_os().skip(1).collect();

    match cli::parse(args, &cwd) {
        Ok(Mode::Help) => {
            print!(
                "{}",
                cli::help_text(log::wants_color(cli::ColorChoice::Auto))
            );
            ExitCode::SUCCESS
        }
        Ok(Mode::Version) => {
            println!("{}", cli::version_line());
            ExitCode::SUCCESS
        }
        Ok(Mode::Build(args)) => {
            log::init(args.opts.color);
            log::set_verbose(args.opts.verbose);
            log::error("not implemented yet");
            ExitCode::from(1)
        }
        Ok(Mode::Convert(args)) => {
            log::init(args.opts.color);
            log::set_verbose(args.opts.verbose);
            log::error("not implemented yet");
            ExitCode::from(1)
        }
        Err(CliError::Refused(msg)) => {
            log::error(&msg);
            ExitCode::from(1)
        }
        Err(CliError::Usage(msg)) => {
            eprintln!("{msg}");
            ExitCode::from(2)
        }
    }
}
