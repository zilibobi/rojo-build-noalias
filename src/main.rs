use rojo_build_noalias::cli::{self, CliError, Mode};
use rojo_build_noalias::{convert, log, pipeline, rojo, shutdown, watch};
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

            // before anything spawns rojo, which a terminal's Ctrl-C no longer reaches
            if let Err(e) = shutdown::install() {
                log::warn(&format!("cannot handle signals: {e}"));
            }

            rojo::kill_on_shutdown();

            let result = if args.watch {
                watch::run(&args)
            } else {
                pipeline::run_once(&args)
            };

            match result {
                Ok(()) => ExitCode::SUCCESS,
                Err(_) if shutdown::requested() && args.watch => ExitCode::SUCCESS,
                Err(_) if shutdown::requested() => {
                    log::error("interrupted");
                    ExitCode::from(1)
                }
                Err(e) => {
                    log::error(&format!("{e:#}"));
                    ExitCode::from(1)
                }
            }
        }
        Ok(Mode::Convert(args)) => {
            log::init(args.opts.color);
            log::set_verbose(args.opts.verbose);

            match convert::run(&args) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    log::error(&format!("{e:#}"));
                    ExitCode::from(1)
                }
            }
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
