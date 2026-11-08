//! `std.json`: `json.loads` and `json.dumps`. The reading and writing itself is in
//! `formats`, which `std.toml` shares.

use fennec_syntax::{Diagnostic, Span};

use crate::value::Value;


