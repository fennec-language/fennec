//! Which of the runtime's exports takes which capability methods and runtime functions. It's
//! here, not in `fennec-runtime`, so the compiler can read it too: that crate brings its own
//! allocator, which a program links and the compiler can't.

/// Which export runs a method or a function by its name, for compiled code to pick where the
/// call is written: a capability's method as `Fs.read_text`, a handle's as `File.read_line`, one
/// of the runtime's own functions as `compress.gzip`. Each of these exports reaches only its own
/// module, so a program links only the modules it calls into: one that reads a file carries no
/// SQLite and no sockets. Handed a name another row is for, an export stops the program as a bug.
///
/// A row is a name, or the start of names when it ends in a dot, and the first row that fits is
/// the one, so a name comes before the start that would fit it too. A name no row fits goes to
/// the export that runs them all, which [`module_entry`] says with `None`:
/// `fennec_native_capability`, `fennec_net_call` or `fennec_crypto_call`. So does every method
/// of a capability in a test build, whose fakes only that one asks.
pub const MODULE_ENTRIES: &[(&str, &str)] = &[
    // The two methods of an `Fs` that open a database, which bring SQLite with them.
    ("Fs._sqlite_open", "fennec_sqlite_capability"),
    ("Fs._sqlite_read", "fennec_sqlite_capability"),
    ("Fs.", "fennec_fs_capability"),
    ("Net.", "fennec_net_capability"),
    ("Process.", "fennec_process_capability"),
    // The small ones, together.
    ("Stdin.", "fennec_io_capability"),
    ("Stderr.", "fennec_io_capability"),
    ("Env.", "fennec_io_capability"),
    ("Entropy.", "fennec_io_capability"),
    ("Signals.", "fennec_io_capability"),
    // Handles.
    ("Database.", "fennec_database_call"),
    ("Transaction.", "fennec_database_call"),
    ("File.", "fennec_file_call"),
    ("Connection.", "fennec_socket_call"),
    ("Server.", "fennec_socket_call"),
    ("UdpSocket.", "fennec_socket_call"),
    // The runtime's own functions.
    ("compress.", "fennec_compress_call"),
    ("unicode.", "fennec_unicode_call"),
    ("crypto.", "fennec_crypto_alone_call"),
];

/// The export for the method or function called `name`, by [`MODULE_ENTRIES`], or `None` for one
/// only the export that runs every module's takes.
#[must_use]
pub fn module_entry(name: &str) -> Option<&'static str> {
    let fits = |row: &str| if row.ends_with('.') { name.starts_with(row) } else { name == row };
    MODULE_ENTRIES.iter().find(|(row, _)| fits(row)).map(|(_, export)| *export)
}
