use crate::cli::ColorChoice;
use crate::shutdown;
use std::io::{ErrorKind, IsTerminal, Write};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU8, Ordering};

static COLOR: OnceLock<bool> = OnceLock::new();
static VERBOSE: AtomicU8 = AtomicU8::new(0);

const RESET: &str = "\x1b[0m";
const RED: &str = "\x1b[31m";
const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const BLUE: &str = "\x1b[34m";
const PURPLE: &str = "\x1b[35m";

pub fn wants_color(choice: ColorChoice) -> bool {
    match choice {
        ColorChoice::Always => true,
        ColorChoice::Never => false,
        ColorChoice::Auto => std::io::stdout().is_terminal(),
    }
}

/// Decide once whether prefixes are colored; later calls are ignored.
pub fn init(choice: ColorChoice) {
    let _ = COLOR.set(wants_color(choice));
}

pub fn verbose() -> u8 {
    VERBOSE.load(Ordering::Relaxed)
}

pub fn set_verbose(n: u8) {
    VERBOSE.store(n, Ordering::Relaxed);
}

fn color_on() -> bool {
    *COLOR.get_or_init(|| wants_color(ColorChoice::Auto))
}

fn line(prefix: &str, color: &str, msg: &str) -> String {
    if color_on() {
        format!("{color}{prefix}{RESET} {msg}\n")
    } else {
        format!("{prefix} {msg}\n")
    }
}

fn emit(text: &str) {
    let mut out = std::io::stdout().lock();
    let res = out.write_all(text.as_bytes()).and_then(|()| out.flush());

    if let Err(e) = res
        && e.kind() == ErrorKind::BrokenPipe
    {
        shutdown::request();
    }
}

pub fn info(msg: &str) {
    emit(&line("INFO", BLUE, msg));
}

pub fn warn(msg: &str) {
    emit(&line("WARN", YELLOW, msg));
}

pub fn success(msg: &str) {
    emit(&line("OK  ", GREEN, msg));
}

pub fn error(msg: &str) {
    emit(&line("ERR ", RED, msg));
}

pub fn rojo(msg: &str) {
    emit(&line("ROJO", PURPLE, msg));
}

/// An empty line, as v1.2.4's `print("")`.
pub fn blank() {
    emit("\n");
}
