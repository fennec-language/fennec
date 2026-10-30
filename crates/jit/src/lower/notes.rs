//! Notes on where compiled code isn't native: each place it hands work to the interpreter
//! or the runtime's objects, with what the work is. `fennec dev fallbacks` prints them, to
//! find what keeps a hot loop from running at native speed.

use fennec_syntax::{ExprKind, Span, StmtKind};

use super::Lowerer;

/// One place compiled code isn't native.
#[derive(Clone, Debug)]
pub struct Note {
    /// The function or method it's in: `name`, or `Type.name`.
    pub function: String,
    pub span: Span,
    /// What runs outside native code, and how.
    pub what: String,
    /// How many loops it's inside, in its function.
    pub loops: usize,
}

impl Lowerer<'_, '_> {
    /// Notes that the code at `span` isn't native, and why.
    pub(super) fn note(&mut self, span: Span, what: impl Into<String>) {
        let note = Note {
            function: self.current_key().to_owned(),
            span,
            what: what.into(),
            loops: self.loops.len(),
        };
        self.shared.notes.push(note);
    }
}

/// What an expression the interpreter runs does, in a few words.
pub(super) fn expression(kind: &ExprKind) -> String {
    let what = match kind {
        ExprKind::Call { callee, .. } => match &callee.kind {
            ExprKind::Attr { name, .. } => return format!("the interpreter calls the method `{name}`"),
            ExprKind::Name(name) => return format!("the interpreter calls `{name}`"),
            _ => "a call",
        },
        ExprKind::FStr(_) => "an f-string",
        ExprKind::List(_) => "a list",
        ExprKind::Tuple(_) => "a tuple",
        ExprKind::Dict(_) => "a dict",
        ExprKind::Set(_) => "a set",
        ExprKind::Comprehension { .. } => "a comprehension",
        ExprKind::Lambda { .. } => "an arrow function",
        ExprKind::Index { .. } => "indexing",
        ExprKind::Slice { .. } => "a slice",
        ExprKind::Attr { name, .. } => return format!("the interpreter reads the field `{name}`"),
        ExprKind::Binary { op, .. } => return format!("the interpreter runs `{}`", op.symbol()),
        ExprKind::Unary { .. } => "a unary operator",
        ExprKind::Name(name) => return format!("the interpreter reads `{name}`"),
        ExprKind::Variant(name) => return format!("the interpreter makes `.{name}`"),
        _ => "an expression",
    };
    format!("the interpreter runs {what}")
}

/// What a statement the interpreter runs does, in a few words.
pub(super) fn statement(kind: &StmtKind) -> String {
    let what = match kind {
        StmtKind::Assign { op: Some(op), .. } => return format!("the interpreter runs `{}=`", op.symbol()),
        StmtKind::Assign { .. } => "an assignment",
        StmtKind::For { .. } => "a `for` loop",
        StmtKind::With { .. } => "a `with` block",
        StmtKind::Assert(_) => "an `assert`",
        _ => "a statement",
    };
    format!("the interpreter runs {what}")
}
