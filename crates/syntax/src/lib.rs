//! Fennec's front end: source text to a syntax tree, with diagnostics that point back at the source.

mod ast;
mod diagnostic;
mod lexer;
mod modules;
mod parser;
mod prelude;
mod span;
mod token;

pub use ast::{
    Arg, BinOp, Case, CompKind, Expr, ExprKind, FStrPart, Field, FnDef, Import, Interface, Item, MatchArm,
    Module, Param, Pattern, Stmt, StmtKind, TestDef, TypeDef, TypeExpr, UnaryOp,
};
pub use diagnostic::Diagnostic;
pub use modules::{Imported, importable_type, member as std_member, module_as_value, std_import};
pub use parser::{parse, parse_prelude, parse_repl};
pub use prelude::PRELUDE;
pub use span::Span;
