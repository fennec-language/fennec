//! Fennec's language server, which editors and AI agents talk to over the Language Server
//! Protocol. It checks a document's program as it's edited, unsaved text and all, and
//! answers with:
//!
//! - diagnostics, the checker's errors, published for every file of the program;
//! - each diagnostic's fix as a code action, the edit `fennec fix` would make;
//! - go to definition, for names, members, types and enum cases;
//! - hover, showing a declaration's signature as written and its `##` doc comment, or what
//!   a local name holds;
//! - formatting, with `fennec fmt`'s one style.
//!
//! `fennec lsp` runs it, in the build daemon when there is one, so every editor shares the
//! daemon's engine and cache.

mod analysis;
mod declaration;
mod definition;
mod findings;
mod json;
mod session;
mod text;
mod wire;

use std::io::{self, BufRead, Write};

pub use json::Json;
pub use session::Session;
pub use text::{path_of, uri_of};
pub use wire::{read, write};

/// Serves one editor over `input` and `output` until it says it's done or goes away.
/// Returns whether it shut down as the protocol asks, for the exit code.
pub fn serve(input: &mut dyn BufRead, output: &mut dyn Write) -> io::Result<bool> {
    let mut session = Session::new();
    while let Some(message) = read(input)? {
        for answer in session.handle(&message) {
            write(output, &answer)?;
        }
        if session.exited() {
            return Ok(session.shut_down());
        }
    }
    Ok(false)
}
