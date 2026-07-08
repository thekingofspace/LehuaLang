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

#[cfg(windows)]
fn enable_ansi() {
    use windows_sys::Win32::System::Console::{
        GetConsoleMode, GetStdHandle, SetConsoleMode, ENABLE_VIRTUAL_TERMINAL_PROCESSING,
        STD_ERROR_HANDLE, STD_OUTPUT_HANDLE,
    };
    unsafe {
        for which in [STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
            let handle = GetStdHandle(which);
            let mut mode = 0u32;
            if GetConsoleMode(handle, &mut mode) != 0 {
                SetConsoleMode(handle, mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING);
            }
        }
    }
}

#[cfg(not(windows))]
fn enable_ansi() {}

fn main() -> ExitCode {
    enable_ansi();
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
