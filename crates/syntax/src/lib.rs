//! Fennec's front end: source text to a syntax tree, with diagnostics that point back at the source.

mod ast;
mod diagnostic;
mod lexer;
mod parser;
mod span;
mod token;

pub use ast::{
    Arg, BinOp, Expr, ExprKind, FStrPart, FnDef, Module, Param, Stmt, StmtKind, TypeExpr, UnaryOp,
};
pub use diagnostic::Diagnostic;
pub use parser::parse;
pub use span::Span;
