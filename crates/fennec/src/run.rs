//! `fennec run`: runs a program's `main`.

use std::io::{BufWriter, Write};
use std::process::ExitCode;

pub(crate) fn run(path: &str) -> ExitCode {
    let Some((source, module)) = crate::load(path) else { return ExitCode::FAILURE };
    let mut out = BufWriter::new(std::io::stdout().lock());
    let result = fennec_interp::run(&module, &mut out);
    let flushed = out.flush();
    match (result, flushed) {
        (Ok(()), Ok(())) => ExitCode::SUCCESS,
        (Err(diagnostic), _) => {
            eprint!("{}", diagnostic.render(path, &source));
            ExitCode::FAILURE
        }
        (Ok(()), Err(err)) => {
            eprintln!("error: couldn't write output: {err}");
            ExitCode::FAILURE
        }
    }
}
