//! Fennec's front end: source text to a syntax tree, with diagnostics that point back at the source.

mod ast;
mod diagnostic;
mod doc_examples;
#[doc(hidden)]
pub mod dump;
pub mod fixed_sum;
pub mod fuse;
mod lexer;
mod modules;
mod parser;
pub mod read;
pub mod rename;
mod sources;
mod span;
mod stdlib;
mod token;
mod trivia;
pub mod visit;

pub use ast::{
    Annotation, Arg, BinOp, Blob, Case, CompKind, Expr, ExprKind, Extension, FStrPart, Field, FnDef,
    GRANTABLE, Generic, Grant, Import, Interface, Item, MatchArm, Module, Param, Pattern, REF, Stmt,
    StmtKind, TestDef, TypeDef, TypeExpr, UnaryOp, WithName,
};
pub use diagnostic::{Also, Diagnostic, Edit, HELP_MARK, Hint, KIT_MARK, apply_fixes, without_kits};
pub use doc_examples::doc_examples;
pub use modules::{
    Imported, dotted, importable_type, member as std_member, module_as_value, package_member,
    shared_declaration, short_module, std_import, written_type,
};
pub use parser::{parse, parse_at, parse_prelude};
pub use sources::{Sources, Trace};
pub use span::Span;
pub use stdlib::{
    KeptPrelude, core_to_run, given, keep_prelude_in, module_providing, module_to_run, prelude,
    prelude_file_at, prelude_files, prelude_modules, prelude_to_run,
};
pub use trivia::{Lexed, SourceToken, TokenKind, lex_source};
