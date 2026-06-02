//! `marv` — the command-line front end for the marv toolchain.
//!
//! Subcommands mirror the agent protocol (`spec/03-compiler-protocol.md`):
//!
//! - `fmt`    — canonicalize source (wired to `marv-syntax::format`).
//! - `check`  — type / effect / capability checking (milestone M2).
//! - `build`  — compile a target (milestone M4).
//! - `verify` — discharge contracts via SMT (milestone M6).
//!
//! Only `fmt` does real work today; the rest are wired for argument parsing and
//! report the milestone that will implement them. Argument parsing is
//! hand-rolled to keep the workspace dependency-free for now.

use std::io::{self, Read, Write};
use std::process::ExitCode;

const USAGE: &str = "\
marv — the marv language toolchain

USAGE:
    marv <command> [args]

COMMANDS:
    fmt [--write|--check] [files...]
                               Canonicalize marv source. With no files, reads
                               stdin and writes canonical form to stdout. With
                               files and no flag, prints each file's canonical
                               form to stdout. With --write, rewrites each file
                               in place. With --check, writes nothing and exits
                               non-zero if any input is not already canonical.
    check [files...]           Type / effect / capability check (milestone M2).
    build [target]             Compile a target (milestone M4).
    verify [files...]          Discharge contracts via SMT (milestone M6).

    -h, --help                 Print this help.
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();

    let Some(command) = args.first() else {
        eprint!("{USAGE}");
        return ExitCode::FAILURE;
    };

    let rest = &args[1..];

    match command.as_str() {
        "-h" | "--help" | "help" => {
            print!("{USAGE}");
            ExitCode::SUCCESS
        }
        "fmt" => cmd_fmt(rest),
        "check" => not_yet_implemented("check", "M2"),
        "build" => not_yet_implemented("build", "M4"),
        "verify" => not_yet_implemented("verify", "M6"),
        other => {
            eprintln!("marv: unknown command `{other}`\n");
            eprint!("{USAGE}");
            ExitCode::FAILURE
        }
    }
}

/// Report a subcommand that is parsed but not yet built, naming its milestone.
fn not_yet_implemented(command: &str, milestone: &str) -> ExitCode {
    eprintln!("marv {command}: not yet implemented (milestone {milestone})");
    ExitCode::FAILURE
}

/// `marv fmt` — canonicalize source via `marv-syntax::format`.
fn cmd_fmt(args: &[String]) -> ExitCode {
    let check_only = args.iter().any(|a| a == "--check");
    let write = args.iter().any(|a| a == "--write");
    let files: Vec<&String> = args.iter().filter(|a| !a.starts_with("--")).collect();

    if check_only && write {
        eprintln!("marv fmt: --check and --write are mutually exclusive");
        return ExitCode::FAILURE;
    }

    // No files: act as a stdin -> stdout filter.
    if files.is_empty() {
        let mut src = String::new();
        if let Err(e) = io::stdin().read_to_string(&mut src) {
            eprintln!("marv fmt: reading stdin: {e}");
            return ExitCode::FAILURE;
        }
        let formatted = marv_syntax::format(&src);
        if check_only {
            return if formatted == src {
                ExitCode::SUCCESS
            } else {
                eprintln!("marv fmt: <stdin> is not in canonical form");
                ExitCode::FAILURE
            };
        }
        if let Err(e) = io::stdout().write_all(formatted.as_bytes()) {
            eprintln!("marv fmt: writing stdout: {e}");
            return ExitCode::FAILURE;
        }
        return ExitCode::SUCCESS;
    }

    // Files: print to stdout by default, rewrite in place with --write, or
    // report non-canonical files with --check.
    let mut had_error = false;
    let mut needs_formatting = false;

    for path in files {
        let src = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("marv fmt: {path}: {e}");
                had_error = true;
                continue;
            }
        };
        let formatted = marv_syntax::format(&src);

        if check_only {
            if formatted != src {
                eprintln!("marv fmt: {path} is not in canonical form");
                needs_formatting = true;
            }
            continue;
        }

        if write {
            if formatted != src {
                if let Err(e) = std::fs::write(path, &formatted) {
                    eprintln!("marv fmt: {path}: {e}");
                    had_error = true;
                }
            }
        } else if let Err(e) = io::stdout().write_all(formatted.as_bytes()) {
            eprintln!("marv fmt: writing stdout: {e}");
            return ExitCode::FAILURE;
        }
    }

    if had_error || needs_formatting {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}
