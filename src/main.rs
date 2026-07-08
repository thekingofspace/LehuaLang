mod bundle;
mod cli;
mod dll;
mod engine;
mod error;
mod headers;
mod libs;
mod manifest;
mod parallel;
mod portable;
mod provider;
mod resolver;
mod scaffold;
mod vpath;

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args_os()
        .skip(1)
        .map(|a| a.to_string_lossy().into_owned())
        .collect();

    match bundle::load_embedded() {
        Ok(Some(bundle)) => cli::run_embedded(bundle, args),
        Ok(None) => cli::main(),
        Err(e) => {
            eprintln!("lehua: this executable's embedded app is corrupt: {e}");
            ExitCode::FAILURE
        }
    }
}
