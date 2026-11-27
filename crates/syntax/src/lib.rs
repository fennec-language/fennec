//! Fennec's front end: source text to a syntax tree, with diagnostics that point back at the source.

mod ast;
pub mod codec;
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
pub mod sight;
mod sources;
mod span;
mod stdlib;
mod token;
mod trivia;
pub mod visit;

pub use ast::{
    Annotation, Arg, BinOp, Blob, Case, CompKind, Expr, ExprKind, Extension, FStrPart, Field, FnDef, Generic,
    HOLDABLE, Holds, Import, Interface, Item, MatchArm, Module, OPTIONAL, Param, Pattern, REF, Stmt,
    StmtKind, TestDef, TypeDef, TypeExpr, UnaryOp, WithName,
};
pub use diagnostic::{Also, Diagnostic, Edit, HELP_MARK, Hint, KIT_MARK, apply_fixes, without_kits};
pub use doc_examples::doc_examples;
pub use modules::{
    Imported, dotted, exported as std_exports, importable_type, let_programs_import_std_private_names,
    member as std_member, module_as_value, package_member, python_module, shared_declaration, short_module,
    std_import, std_module, written_type,
};
pub use parser::{NESTING_LIMIT, parse, parse_at, parse_prelude, parse_written};
pub use sources::{Sources, Trace};
pub use span::{Copied, Span};
pub use stdlib::{
    EngineDeclaration, KeptPrelude, core_to_run, engine_declarations, given, keep_prelude_in,
    module_providing, module_to_run, prelude, prelude_file_at, prelude_files, prelude_modules,
    prelude_to_run,
};
pub use trivia::{Lexed, SourceToken, TokenKind, lex_source};
