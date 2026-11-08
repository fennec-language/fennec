//! The standard library's collections in their natural shape: a `SortedMap` is an object, a
//! `SortedSet` or a `Deque` is an array, and a `Grid` is an array of rows. Each collection
//! says what it holds with its own method and is built again with its own `of`, so the shapes
//! live in the standard library, and this only knows which types have one. A program's own
//! type of the same name is written field by field, like any other.

use std::rc::Rc;

use fennec_syntax::{Diagnostic, Field, Span, TypeExpr};

use super::Names;
use crate::value::{Instance, Value};

/// Each collection, the method that says what it holds, and the type that's written as.
const SHAPES: [(&str, &str); 4] =
    [("SortedMap", "items"), ("SortedSet", "items"), ("Deque", "items"), ("Grid", "rows")];

/// The standard library's types that save as one plain value, as their own `serialize` writes
/// them for native code: a `Time` as its RFC 3339 text, a `Uuid` as its text, and a `Duration`
/// as its nanoseconds.
const PLAIN: [&str; 3] = ["Time", "Uuid", "Duration"];

