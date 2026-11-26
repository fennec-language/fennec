//! What `std.pack` writes beside a value, as `docs/pack.md` pins it: a type's shape, the 64-bit
//! FNV-1a hash of that text, and its layout. Written from the doc, not from the compiler's code,
//! so the two are held to each other. A type that writes its own `serialize` is shaped by what it
//! saves, as the doc says.

use fennec_check::Type;
use fennec_syntax::{Expr, ExprKind};

use super::{Interp, Run, unsupported};

impl Interp {
    /// `shape_hash[T]()`: the FNV-1a hash of `ty`'s shape, read as a signed 64-bit int.
    pub(crate) fn shape_hash(&self, ty: &Type) -> Run<i64> {
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in self.shape(ty)?.bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0100_0000_01b3);
        }
        Ok(hash.cast_signed())
    }

    /// A type's shape, as `docs/pack.md` writes it.
    pub(crate) fn shape(&self, ty: &Type) -> Run<String> {
        let mut out = String::new();
        self.shape_into(ty, &mut Vec::new(), &mut out)?;
        Ok(out)
    }

    fn shape_into(&self, ty: &Type, open: &mut Vec<String>, out: &mut String) -> Run<()> {
        match ty {
            Type::Int => out.push_str("int"),
            Type::Float => out.push_str("float"),
            Type::Sized(fennec_check::Num::F32) => out.push_str("f32"),
            Type::Bool => out.push_str("bool"),
            Type::Str => out.push_str("str"),
            Type::Bytes => out.push_str("bytes"),
            Type::List(item) => self.shapes_within(("list[", "]"), &[item], open, out)?,
            Type::Set(item) => self.shapes_within(("set[", "]"), &[item], open, out)?,
            Type::Table(item) => self.shapes_within(("Table[", "]"), &[item], open, out)?,
            Type::Id(item) => self.shapes_within(("Id[", "]"), &[item], open, out)?,
            Type::Dict(key, value) => self.shapes_within(("dict[", "]"), &[key, value], open, out)?,
            Type::Optional(item) => self.shapes_within(("", "?"), &[item], open, out)?,
            Type::Tuple(items) => {
                let items: Vec<&Type> = items.iter().collect();
                self.shapes_within(("(", ")"), &items, open, out)?;
            }
            Type::Union(members) => {
                out.push('(');
                for (at, member) in members.iter().enumerate() {
                    if at > 0 {
                        out.push_str(" | ");
                    }
                    self.shape_into(member, open, out)?;
                }
                out.push(')');
            }
            Type::Named(name) => self.named_shape(name, &[], open, out)?,
            Type::Generic(name, args) => self.named_shape(name, args, open, out)?,
            other => {
                return unsupported(format!("the pack shape of a {}", other.source().unwrap_or_default()));
            }
        }
        Ok(())
    }

    /// `items`' shapes between `opening` and `closing`, apart by `, `.
    fn shapes_within(
        &self,
        (opening, closing): (&str, &str),
        items: &[&Type],
        open: &mut Vec<String>,
        out: &mut String,
    ) -> Run<()> {
        out.push_str(opening);
        for (at, item) in items.iter().enumerate() {
            if at > 0 {
                out.push_str(", ");
            }
            self.shape_into(item, open, out)?;
        }
        out.push_str(closing);
        Ok(())
    }

    /// A struct's or an enum's shape: its name as the program writes it, its type arguments in
    /// full, then its fields or cases with the arguments filled in. Inside itself, the same type
    /// with the same arguments is its name and arguments alone.
    fn named_shape(&self, name: &str, args: &[Type], open: &mut Vec<String>, out: &mut String) -> Run<()> {
        // A type that writes its own `serialize` is shaped by what it saves: a standard library
        // type that saves as another by that type, and any other by its name and arguments alone.
        if let Some(saved_as) = Self::saved_as(name, args) {
            return self.shape_into(&saved_as, open, out);
        }
        let saves_itself = self.program.method(name, "serialize").is_some();
        let start = out.len();
        out.push_str(fennec_syntax::rename::shown_type_name(name));
        if !args.is_empty() {
            let args: Vec<&Type> = args.iter().collect();
            self.shapes_within(("[", "]"), &args, open, out)?;
        }
        let instance = out.get(start..).unwrap_or(name).to_owned();
        if saves_itself || open.contains(&instance) {
            return Ok(());
        }
        open.push(instance);
        let Some(declared) = self.program.types.get(name) else {
            return unsupported(format!("the pack shape of a {name}"));
        };
        if declared.def.cases.is_empty() {
            out.push('{');
            let saved = self.saved_fields(name, None, args)?;
            for (at, field) in saved.iter().filter(|field| !field.skipped).enumerate() {
                if at > 0 {
                    out.push_str(", ");
                }
                out.push_str(&field.name);
                out.push_str(": ");
                self.shape_into(&field.ty, open, out)?;
            }
            out.push('}');
        } else {
            out.push('<');
            for (at, case) in declared.def.cases.iter().enumerate() {
                if at > 0 {
                    out.push_str(", ");
                }
                out.push_str(&case.name);
                let fields = self.saved_fields(name, Some(&case.name), args)?;
                if !fields.is_empty() {
                    out.push('(');
                    for (at, field) in fields.iter().enumerate() {
                        if at > 0 {
                            out.push_str(", ");
                        }
                        out.push_str(&field.name);
                        out.push_str(": ");
                        self.shape_into(&field.ty, open, out)?;
                    }
                    out.push(')');
                }
            }
            out.push('>');
        }
        open.pop();
        Ok(())
    }

    /// What one of the standard library's types that saves as another saves as, as
    /// `docs/pack.md` lists them: a `Deque[T]` as a `list[T]`, a `Time` as a `str`.
    fn saved_as(name: &str, args: &[Type]) -> Option<Type> {
        let first = || Box::new(args.first().cloned().unwrap_or(Type::Unknown));
        Some(match name {
            "Deque" | "SortedSet" => Type::List(first()),
            "SortedMap" => Type::Dict(first(), Box::new(args.get(1).cloned().unwrap_or(Type::Unknown))),
            "Packed" | "Grid" => Type::List(Box::new(Type::List(first()))),
            "Uuid" | "Time" => Type::Str,
            "Duration" => Type::Int,
            _ => return None,
        })
    }

    /// `layout[T]()`: what a value of `ty` saves as, words apart by single spaces.
    pub(crate) fn layout(&self, ty: &Type) -> Run<String> {
        let mut words = Vec::new();
        self.layout_into(ty, &mut Vec::new(), &mut words)?;
        Ok(words.join(" "))
    }

    fn layout_into(&self, ty: &Type, open: &mut Vec<String>, words: &mut Vec<String>) -> Run<()> {
        let mut word = |text: &str| words.push(text.to_owned());
        match ty {
            Type::Int => word("i"),
            Type::Float => word("f"),
            Type::Sized(fennec_check::Num::F32) => word("g"),
            Type::Bool => word("b"),
            Type::Str => word("s"),
            Type::Bytes => word("y"),
            Type::List(item) | Type::Set(item) => {
                word("l");
                self.layout_into(item, open, words)?;
            }
            Type::Optional(item) => {
                word("o");
                self.layout_into(item, open, words)?;
            }
            Type::Dict(key, value) => {
                word("m");
                self.layout_into(key, open, words)?;
                self.layout_into(value, open, words)?;
            }
            Type::Tuple(items) => {
                word("t");
                word(&items.len().to_string());
                for item in items {
                    self.layout_into(item, open, words)?;
                }
            }
            Type::Union(members) => {
                word("u");
                word(&members.len().to_string());
                for member in members {
                    words.push(member.source().unwrap_or_default().replace(' ', ""));
                    self.layout_into(member, open, words)?;
                }
            }
            Type::Table(item) => {
                word("T");
                self.layout_into(item, open, words)?;
            }
            Type::Id(_) => word("I"),
            Type::Named(name) => self.named_layout(name, &[], open, words)?,
            Type::Generic(name, args) => self.named_layout(name, args, open, words)?,
            _ => word("?"),
        }
        Ok(())
    }

    /// A struct's or an enum's layout, a type inside itself by how far in it's open, or what one
    /// of the standard library's types saves as.
    fn named_layout(
        &self,
        name: &str,
        args: &[Type],
        open: &mut Vec<String>,
        words: &mut Vec<String>,
    ) -> Run<()> {
        let saved_as = Self::saved_as(name, args);
        if let Some(saved_as) = saved_as {
            return self.layout_into(&saved_as, open, words);
        }
        let instance = if args.is_empty() {
            name.to_owned()
        } else {
            Type::Generic(name.to_owned(), args.to_vec()).source().unwrap_or_else(|| name.to_owned())
        };
        if let Some(at) = open.iter().position(|seen| *seen == instance) {
            words.push("r".to_owned());
            words.push(at.to_string());
            return Ok(());
        }
        if self.program.method(name, "serialize").is_some() {
            words.push("O".to_owned());
            words.push(instance.replace(' ', ""));
            return Ok(());
        }
        let Some(declared) = self.program.types.get(name) else {
            words.push("?".to_owned());
            return Ok(());
        };
        open.push(instance);
        if declared.def.cases.is_empty() {
            let saved = self.saved_fields(name, None, args)?;
            let saved: Vec<_> = saved.iter().filter(|field| !field.skipped).collect();
            words.push("S".to_owned());
            words.push(saved.len().to_string());
            for field in saved {
                words.push(field.saved_name().to_owned());
                self.layout_into(&field.ty, open, words)?;
            }
        } else {
            words.push("E".to_owned());
            words.push(declared.def.cases.len().to_string());
            for case in &declared.def.cases {
                let fields = self.saved_fields(name, Some(&case.name), args)?;
                words.push(case.name.clone());
                words.push(fields.len().to_string());
                for field in fields {
                    words.push(field.name.clone());
                    self.layout_into(&field.ty, open, words)?;
                }
            }
        }
        open.pop();
        Ok(())
    }

    /// The type a call's type argument, `T` in `shape_hash[T]()`, stands for here: a type
    /// parameter of the running call, filled in, or a type written out.
    pub(crate) fn type_argument(&self, written: &Expr) -> Run<Type> {
        let ty = match &written.kind {
            ExprKind::Name(name) => match name.as_str() {
                "int" => Type::Int,
                "float" => Type::Float,
                "bool" => Type::Bool,
                "str" => Type::Str,
                "bytes" => Type::Bytes,
                _ => match self.resolved(&Type::Param(name.clone())) {
                    Type::Param(_) if self.program.types.contains_key(name.as_str()) => {
                        Type::Named(name.clone())
                    }
                    Type::Param(_) => return unsupported(format!("the type `{name}` a call's written with")),
                    found => found,
                },
            },
            ExprKind::Index { object, index } => {
                let ExprKind::Name(name) = &object.kind else {
                    return unsupported("a call's type argument that isn't a type");
                };
                let written: Vec<&Expr> = match &index.kind {
                    ExprKind::Tuple(items) => items.iter().collect(),
                    _ => vec![&**index],
                };
                let mut args = Vec::with_capacity(written.len());
                for arg in written {
                    args.push(self.type_argument(arg)?);
                }
                let one = |args: &[Type]| Box::new(args.first().cloned().unwrap_or(Type::Unknown));
                match (name.as_str(), args.len()) {
                    ("list", 1) => Type::List(one(&args)),
                    ("set", 1) => Type::Set(one(&args)),
                    ("Table", 1) => Type::Table(one(&args)),
                    ("Id", 1) => Type::Id(one(&args)),
                    ("dict", 2) => {
                        Type::Dict(one(&args), Box::new(args.get(1).cloned().unwrap_or(Type::Unknown)))
                    }
                    _ if self.program.types.contains_key(name.as_str()) => Type::Generic(name.clone(), args),
                    _ => return unsupported(format!("the type `{name}[...]` a call's written with")),
                }
            }
            _ => return unsupported("a call's type argument that isn't a name"),
        };
        if !super::typed::settled(&ty) {
            return unsupported("a call's type argument that isn't settled");
        }
        Ok(ty)
    }
}
