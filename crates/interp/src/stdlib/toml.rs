//! `std.toml`: `toml.loads` and `toml.dumps`. The reading and writing itself is in
//! `formats`, which `std.json` shares.

use fennec_syntax::{Diagnostic, Span};

use crate::value::Value;


