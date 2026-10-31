//! `engines-live` — the free live player: [`engines_live::app::run`] with no note hook. See
//! `app` for what it does and its flags.

use std::process::ExitCode;

fn main() -> ExitCode {
    engines_live::app::run(std::env::args_os(), &|_| None)
}
