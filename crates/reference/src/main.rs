//! `fennec-reference`: writes the language reference, for the website's build.
#![expect(clippy::print_stderr, reason = "it reports what's wrong")]

use std::process::ExitCode;

fn main() -> ExitCode {
    match fennec_reference::write(false) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::FAILURE
        }
    }
}
