//! Fennec's language server, which editors and AI agents talk to over the Language Server
//! Protocol. It checks a document's program as it's edited, unsaved text and all, and
//! answers with:
//!
//! - diagnostics, the checker's errors, published for every file of the program as soon as
//!   they're found, then again with what the linter finds and what may copy, which take longer;
//! - each diagnostic's fix as a code action, the edit `fennec fix` would make;
//! - go to definition, for names, members, types, enum cases, named arguments, the cases a
//!   pattern names and the modules of a kit;
//! - hover, showing a declaration's signature as written and its `##` doc comment, or what
//!   a local name holds;
//! - find references and rename, which know scopes and types, so a local of the same name
//!   elsewhere isn't touched;
//! - a document's outline;
//! - completion of names in scope, members after a dot and enum cases;
//! - signature help inside a call's parentheses;
//! - inlay hints: inferred types of local names, parameter names at calls, and copies in loops;
//! - formatting, with `fennec fmt`'s one style.
//!
//! `fennec lsp` runs it, in the build daemon when there is one, so every editor shares the
//! daemon's engine and cache.

mod analysis;
mod completion;
mod declaration;
mod definition;
mod findings;
mod inlay;
mod json;
mod library;
mod references;
mod session;
mod signature;
mod symbols;
mod text;
mod wire;

use std::io::{self, BufRead, Write};
use std::sync::mpsc::{self, TryRecvError};

pub use json::Json;
pub use session::Session;
pub use text::{path_of, uri_of};
pub use wire::{read, write};

/// Serves one editor over `input` and `output` until it says it's done or goes away.
/// Returns whether it shut down as the protocol asks, for the exit code.
///
/// A program's mistakes are published as soon as the checker has found them. Before working out
/// what the linter finds and what may copy, which take longer, it answers whatever the editor has
/// sent meanwhile, as the hovers and outlines an editor asks for on opening a file, from what the
/// checker found. So `input` is read on a thread of its own, which is left reading once the
/// session ends: whatever runs this closes `input`, or ends, to stop it.
pub fn serve(input: impl BufRead + Send + 'static, output: &mut dyn Write) -> io::Result<bool> {
    let (sender, received) = mpsc::channel();
    let mut input = input;
    std::thread::spawn(move || {
        loop {
            let message = read(&mut input);
            let more = matches!(message, Ok(Some(_)));
            if sender.send(message).is_err() || !more {
                return;
            }
        }
    });
    let mut session = Session::staged();
    loop {
        let message = match received.try_recv() {
            Ok(message) => message,
            Err(TryRecvError::Empty) if session.pending() => {
                for answer in session.finish() {
                    write(output, &answer)?;
                }
                continue;
            }
            Err(_) => received.recv().unwrap_or(Ok(None)),
        };
        let Some(message) = message? else { return Ok(false) };
        for answer in session.handle(&message) {
            write(output, &answer)?;
        }
        if session.exited() {
            return Ok(session.shut_down());
        }
    }
}
