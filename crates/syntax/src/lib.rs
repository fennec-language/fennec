//! Fennec's front end: source text to a syntax tree, with diagnostics that point back at the source.

mod ast;
mod diagnostic;
#[doc(hidden)]
pub mod dump;
mod lexer;
mod modules;
mod parser;
mod sources;
mod span;
mod stdlib;
mod token;
mod trivia;
pub mod visit;

pub use ast::{
    Arg, BinOp, Case, CompKind, Expr, ExprKind, Extension, FStrPart, Field, FnDef, Generic, Import,
    Interface, Item, MatchArm, Module, Param, Pattern, Stmt, StmtKind, TestDef, TypeDef, TypeExpr, UnaryOp,
};
pub use diagnostic::{Diagnostic, Edit, apply_fixes};
pub use modules::{
    Imported, dotted, importable_type, member as std_member, module_as_value, package_member, short_module,
    std_import,
};
pub use parser::{parse, parse_at, parse_prelude, parse_repl};
pub use sources::Sources;
pub use span::Span;
pub use stdlib::{given, prelude, prelude_modules};
pub use trivia::{Lexed, SourceToken, TokenKind, lex_source};
