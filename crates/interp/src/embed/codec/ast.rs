//! The syntax tree's encoding. Structs write their fields in order; enums write a tag for
//! the variant, then its fields.

use fennec_syntax::{
    Arg, BinOp, Case, CompKind, Expr, ExprKind, Extension, FStrPart, Field, FnDef, Generic, Import,
    Interface, Item, MatchArm, Module, Param, Pattern, Stmt, StmtKind, TestDef, TypeDef, TypeExpr, UnaryOp,
};

use super::{Decode, Encode, Reader, Writer};

/// Encodes and decodes a struct field by field.
macro_rules! fields {
    ($ty:ident { $($field:ident),* $(,)? }) => {
        impl Encode for $ty {
            fn encode(&self, to: &mut Writer) {
                $(self.$field.encode(to);)*
            }
        }

        impl Decode for $ty {
            fn decode(from: &mut Reader<'_>) -> Option<Self> {
                Some($ty { $($field: Decode::decode(from)?),* })
            }
        }
    };
}

fields!(Module { items });
fields!(FnDef { name, generics, params, ret, arrow, body, span });
fields!(Generic { name, bounds, span });
fields!(Param { name, ty, is_mut, is_self, span });
fields!(TypeExpr { name, args, optional, span });
fields!(TypeDef { name, generics, fields, cases, methods, span });
fields!(Field { name, ty, default, rule, span });
fields!(Case { name, fields, span });
fields!(Interface { name, methods, span });
fields!(Extension { name, generics, methods, span });
fields!(Import { module, names, aliases, alias, span });
fields!(TestDef { name, inputs, body, bench, span });
fields!(Stmt { kind, span });
fields!(Expr { kind, span });
fields!(MatchArm { pattern, guard, body, span });
fields!(Arg { name, is_mut, value });

/// Encodes and decodes an enum whose variants have no fields, by position.
macro_rules! units {
    ($ty:ident [$($variant:ident),* $(,)?]) => {
        impl Encode for $ty {
            fn encode(&self, to: &mut Writer) {
                const ALL: &[$ty] = &[$($ty::$variant),*];
                let at = ALL.iter().position(|known| known == self).unwrap_or(0);
                to.tag(u8::try_from(at).unwrap_or(0));
            }
        }

        impl Decode for $ty {
            fn decode(from: &mut Reader<'_>) -> Option<Self> {
                const ALL: &[$ty] = &[$($ty::$variant),*];
                ALL.get(usize::from(from.tag()?)).copied()
            }
        }
    };
}

units!(UnaryOp[Neg, Not]);
units!(CompKind[List, Generator, Set, Dict]);
units!(BinOp[
    Add, Sub, Mul, Div, FloorDiv, Mod, Pow, Eq, NotEq, Lt, LtEq, Gt, GtEq, And, Or, In, NotIn, Is, IsNot,
]);

impl Encode for Item {
    fn encode(&self, to: &mut Writer) {
        match self {
            Item::Function(def) => (0, def).encode_tagged(to),
            Item::Struct(def) => (1, def).encode_tagged(to),
            Item::Enum(def) => (2, def).encode_tagged(to),
            Item::Error(def) => (3, def).encode_tagged(to),
            Item::Interface(def) => (4, def).encode_tagged(to),
            Item::TypeAlias { name, ty, span } => {
                to.tag(5);
                name.encode(to);
                ty.encode(to);
                span.encode(to);
            }
            Item::Import(import) => (6, import).encode_tagged(to),
            Item::Test(test) => (7, test).encode_tagged(to),
            Item::Stmt(stmt) => (8, stmt).encode_tagged(to),
            Item::Extend(extension) => (9, extension).encode_tagged(to),
            Item::Const { name, ty, value, span } => {
                to.tag(10);
                name.encode(to);
                ty.encode(to);
                value.encode(to);
                span.encode(to);
            }
        }
    }
}

impl Decode for Item {
    fn decode(from: &mut Reader<'_>) -> Option<Self> {
        Some(match from.tag()? {
            0 => Item::Function(Decode::decode(from)?),
            1 => Item::Struct(Decode::decode(from)?),
            2 => Item::Enum(Decode::decode(from)?),
            3 => Item::Error(Decode::decode(from)?),
            4 => Item::Interface(Decode::decode(from)?),
            5 => Item::TypeAlias {
                name: Decode::decode(from)?,
                ty: Decode::decode(from)?,
                span: Decode::decode(from)?,
            },
            6 => Item::Import(Decode::decode(from)?),
            7 => Item::Test(Decode::decode(from)?),
            8 => Item::Stmt(Decode::decode(from)?),
            9 => Item::Extend(Decode::decode(from)?),
            10 => Item::Const {
                name: Decode::decode(from)?,
                ty: Decode::decode(from)?,
                value: Decode::decode(from)?,
                span: Decode::decode(from)?,
            },
            _ => return None,
        })
    }
}

/// A variant's tag, then its one field.
trait Tagged {
    fn encode_tagged(&self, to: &mut Writer);
}

impl<T: Encode> Tagged for (u8, &T) {
    fn encode_tagged(&self, to: &mut Writer) {
        to.tag(self.0);
        self.1.encode(to);
    }
}

impl Encode for StmtKind {
    fn encode(&self, to: &mut Writer) {
        match self {
            StmtKind::Expr(expr) => (0, expr).encode_tagged(to),
            StmtKind::Assign { target, op, value, declare_mut, ty } => {
                to.tag(1);
                target.encode(to);
                op.encode(to);
                value.encode(to);
                declare_mut.encode(to);
                ty.encode(to);
            }
            StmtKind::If { branches, otherwise } => {
                to.tag(2);
                branches.encode(to);
                otherwise.encode(to);
            }
            StmtKind::While { cond, body } => {
                to.tag(3);
                cond.encode(to);
                body.encode(to);
            }
            StmtKind::For { names, is_mut, iter, body } => {
                to.tag(4);
                names.encode(to);
                is_mut.encode(to);
                iter.encode(to);
                body.encode(to);
            }
            StmtKind::Return(value) => (5, value).encode_tagged(to),
            StmtKind::Fail(value) => (6, value).encode_tagged(to),
            StmtKind::Yield(value) => (7, value).encode_tagged(to),
            StmtKind::Assert(value) => (8, value).encode_tagged(to),
            StmtKind::With { value, name, is_mut, body } => {
                to.tag(9);
                value.encode(to);
                name.encode(to);
                is_mut.encode(to);
                body.encode(to);
            }
            StmtKind::Def(def) => (10, def).encode_tagged(to),
            StmtKind::Break => to.tag(11),
            StmtKind::Continue => to.tag(12),
            StmtKind::Pass => to.tag(13),
        }
    }
}

impl Decode for StmtKind {
    fn decode(from: &mut Reader<'_>) -> Option<Self> {
        let r = from;
        Some(match r.tag()? {
            0 => StmtKind::Expr(Decode::decode(r)?),
            1 => StmtKind::Assign {
                target: Decode::decode(r)?,
                op: Decode::decode(r)?,
                value: Decode::decode(r)?,
                declare_mut: Decode::decode(r)?,
                ty: Decode::decode(r)?,
            },
            2 => StmtKind::If { branches: Decode::decode(r)?, otherwise: Decode::decode(r)? },
            3 => StmtKind::While { cond: Decode::decode(r)?, body: Decode::decode(r)? },
            4 => StmtKind::For {
                names: Decode::decode(r)?,
                is_mut: Decode::decode(r)?,
                iter: Decode::decode(r)?,
                body: Decode::decode(r)?,
            },
            5 => StmtKind::Return(Decode::decode(r)?),
            6 => StmtKind::Fail(Decode::decode(r)?),
            7 => StmtKind::Yield(Decode::decode(r)?),
            8 => StmtKind::Assert(Decode::decode(r)?),
            9 => StmtKind::With {
                value: Decode::decode(r)?,
                name: Decode::decode(r)?,
                is_mut: Decode::decode(r)?,
                body: Decode::decode(r)?,
            },
            10 => StmtKind::Def(Decode::decode(r)?),
            11 => StmtKind::Break,
            12 => StmtKind::Continue,
            13 => StmtKind::Pass,
            _ => return None,
        })
    }
}

impl Encode for ExprKind {
    fn encode(&self, to: &mut Writer) {
        match self {
            ExprKind::Int(n) => (0, n).encode_tagged(to),
            ExprKind::Float(x) => (1, x).encode_tagged(to),
            ExprKind::Str(text) => (2, text).encode_tagged(to),
            ExprKind::FStr(parts) => (3, parts).encode_tagged(to),
            ExprKind::Bool(b) => (4, b).encode_tagged(to),
            ExprKind::None => to.tag(5),
            ExprKind::Name(name) => (6, name).encode_tagged(to),
            ExprKind::Variant(name) => (7, name).encode_tagged(to),
            ExprKind::List(items) => (8, items).encode_tagged(to),
            ExprKind::Tuple(items) => (9, items).encode_tagged(to),
            ExprKind::Dict(pairs) => (10, pairs).encode_tagged(to),
            ExprKind::Set(items) => (11, items).encode_tagged(to),
            ExprKind::Comprehension { kind, elem, value, names, iter, cond } => {
                to.tag(12);
                kind.encode(to);
                elem.encode(to);
                value.encode(to);
                names.encode(to);
                iter.encode(to);
                cond.encode(to);
            }
            ExprKind::Unary { op, expr } => {
                to.tag(13);
                op.encode(to);
                expr.encode(to);
            }
            ExprKind::Binary { op, lhs, rhs } => {
                to.tag(14);
                op.encode(to);
                lhs.encode(to);
                rhs.encode(to);
            }
            _ => encode_more(self, to),
        }
    }
}

/// The rest of `ExprKind`'s variants, split off to keep functions short.
fn encode_more(kind: &ExprKind, to: &mut Writer) {
    match kind {
        ExprKind::Cond { then, cond, otherwise } => {
            to.tag(15);
            then.encode(to);
            cond.encode(to);
            otherwise.encode(to);
        }
        ExprKind::Else { value, default } => {
            to.tag(16);
            value.encode(to);
            default.encode(to);
        }
        ExprKind::Try { value, context } => {
            to.tag(17);
            value.encode(to);
            context.encode(to);
        }
        ExprKind::Call { callee, args } => {
            to.tag(18);
            callee.encode(to);
            args.encode(to);
        }
        ExprKind::Attr { object, name, optional } => {
            to.tag(19);
            object.encode(to);
            name.encode(to);
            optional.encode(to);
        }
        ExprKind::Index { object, index } => {
            to.tag(20);
            object.encode(to);
            index.encode(to);
        }
        ExprKind::Slice { object, start, end } => {
            to.tag(21);
            object.encode(to);
            start.encode(to);
            end.encode(to);
        }
        ExprKind::Lambda { params, body } => {
            to.tag(22);
            params.encode(to);
            body.encode(to);
        }
        ExprKind::Match { subject, arms } => {
            to.tag(23);
            subject.encode(to);
            arms.encode(to);
        }
        _ => {}
    }
}

impl Decode for ExprKind {
    fn decode(from: &mut Reader<'_>) -> Option<Self> {
        let r = from;
        Some(match r.tag()? {
            0 => ExprKind::Int(Decode::decode(r)?),
            1 => ExprKind::Float(Decode::decode(r)?),
            2 => ExprKind::Str(Decode::decode(r)?),
            3 => ExprKind::FStr(Decode::decode(r)?),
            4 => ExprKind::Bool(Decode::decode(r)?),
            5 => ExprKind::None,
            6 => ExprKind::Name(Decode::decode(r)?),
            7 => ExprKind::Variant(Decode::decode(r)?),
            8 => ExprKind::List(Decode::decode(r)?),
            9 => ExprKind::Tuple(Decode::decode(r)?),
            10 => ExprKind::Dict(Decode::decode(r)?),
            11 => ExprKind::Set(Decode::decode(r)?),
            12 => ExprKind::Comprehension {
                kind: Decode::decode(r)?,
                elem: Decode::decode(r)?,
                value: Decode::decode(r)?,
                names: Decode::decode(r)?,
                iter: Decode::decode(r)?,
                cond: Decode::decode(r)?,
            },
            13 => ExprKind::Unary { op: Decode::decode(r)?, expr: Decode::decode(r)? },
            14 => {
                ExprKind::Binary { op: Decode::decode(r)?, lhs: Decode::decode(r)?, rhs: Decode::decode(r)? }
            }
            tag => return decode_more(tag, r),
        })
    }
}

fn decode_more(tag: u8, r: &mut Reader<'_>) -> Option<ExprKind> {
    Some(match tag {
        15 => ExprKind::Cond {
            then: Decode::decode(r)?,
            cond: Decode::decode(r)?,
            otherwise: Decode::decode(r)?,
        },
        16 => ExprKind::Else { value: Decode::decode(r)?, default: Decode::decode(r)? },
        17 => ExprKind::Try { value: Decode::decode(r)?, context: Decode::decode(r)? },
        18 => ExprKind::Call { callee: Decode::decode(r)?, args: Decode::decode(r)? },
        19 => ExprKind::Attr {
            object: Decode::decode(r)?,
            name: Decode::decode(r)?,
            optional: Decode::decode(r)?,
        },
        20 => ExprKind::Index { object: Decode::decode(r)?, index: Decode::decode(r)? },
        21 => {
            ExprKind::Slice { object: Decode::decode(r)?, start: Decode::decode(r)?, end: Decode::decode(r)? }
        }
        22 => ExprKind::Lambda { params: Decode::decode(r)?, body: Decode::decode(r)? },
        23 => ExprKind::Match { subject: Decode::decode(r)?, arms: Decode::decode(r)? },
        _ => return None,
    })
}

impl Encode for Pattern {
    fn encode(&self, to: &mut Writer) {
        match self {
            Pattern::Wildcard => to.tag(0),
            Pattern::Bind(name) => (1, name).encode_tagged(to),
            Pattern::Literal(expr) => (2, expr).encode_tagged(to),
            Pattern::Variant { name, dotted, args } => {
                to.tag(3);
                name.encode(to);
                dotted.encode(to);
                args.encode(to);
            }
            Pattern::Tuple(items) => (4, items).encode_tagged(to),
            Pattern::Or(alternatives) => (5, alternatives).encode_tagged(to),
        }
    }
}

impl Decode for Pattern {
    fn decode(from: &mut Reader<'_>) -> Option<Self> {
        Some(match from.tag()? {
            0 => Pattern::Wildcard,
            1 => Pattern::Bind(Decode::decode(from)?),
            2 => Pattern::Literal(Decode::decode(from)?),
            3 => Pattern::Variant {
                name: Decode::decode(from)?,
                dotted: Decode::decode(from)?,
                args: Decode::decode(from)?,
            },
            4 => Pattern::Tuple(Decode::decode(from)?),
            5 => Pattern::Or(Decode::decode(from)?),
            _ => return None,
        })
    }
}

impl Encode for FStrPart {
    fn encode(&self, to: &mut Writer) {
        match self {
            FStrPart::Text(text) => (0, text).encode_tagged(to),
            FStrPart::Expr { expr, spec } => {
                to.tag(1);
                expr.encode(to);
                spec.encode(to);
            }
        }
    }
}

impl Decode for FStrPart {
    fn decode(from: &mut Reader<'_>) -> Option<Self> {
        Some(match from.tag()? {
            0 => FStrPart::Text(Decode::decode(from)?),
            1 => FStrPart::Expr { expr: Decode::decode(from)?, spec: Decode::decode(from)? },
            _ => return None,
        })
    }
}
