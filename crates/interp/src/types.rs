//! Building values of user-defined types: structs and errors from their fields and
//! defaults, enum cases by name, and `where` rules checked as each value is made.

use std::rc::Rc;

use fennec_syntax::{Diagnostic, Field, Span};

use crate::call::CallArg;
use crate::format;
use crate::interp::{Interp, TypeKind};
use crate::value::{Instance, Value};

/// What's being built: a type, or one case of an enum.
struct Shape<'a> {
    type_name: &'a str,
    case: Option<&'a str>,
    fields: &'a [Field],
    is_error: bool,
}

impl Interp<'_> {
    /// Calls a type as a constructor: `Enemy(position=p)`, `NotFound(path)`, `IdMap[Enemy]()`.
    pub(crate) fn construct(
        &mut self,
        type_name: &str,
        args: Vec<CallArg>,
        span: Span,
    ) -> Result<Value, Diagnostic> {
        match (type_name, args.as_slice()) {
            ("IdMap", []) => return Ok(Value::IdMap(Rc::default())),
            ("SoaList", []) => return Ok(Value::list(Vec::new())),
            ("Path", [CallArg { value: Value::Str(path), .. }]) => return Ok(Value::Str(Rc::clone(path))),
            ("IdMap" | "SoaList" | "Path" | "Id", _) => {
                return Err(Diagnostic::new(
                    format!("`{type_name}` was called with the wrong arguments"),
                    span,
                ));
            }
            _ => {}
        }
        let info = self.type_info(type_name).ok_or_else(|| crate::eval::unknown_name(type_name, span))?;
        if info.kind == TypeKind::Enum {
            let cases: Vec<_> =
                info.def.cases.iter().map(|case| format!("{type_name}.{}", case.name)).collect();
            return Err(Diagnostic::new(format!("`{type_name}` is an enum, so pick one of its cases"), span)
                .with_help(format!("write one of {}", cases.join(", "))));
        }
        let shape =
            Shape { type_name, case: None, fields: &info.def.fields, is_error: info.kind == TypeKind::Error };
        self.build(&shape, args, span)
    }

    /// Builds an enum case, like `.Circle(radius=1.0)` or `Shape.Circle(1.0)`. Without a type,
    /// the case's name has to belong to exactly one enum.
    pub(crate) fn construct_variant(
        &mut self,
        enum_name: Option<&str>,
        case: &str,
        args: Vec<CallArg>,
        span: Span,
    ) -> Result<Value, Diagnostic> {
        let owners: Vec<String> = if let Some(name) = enum_name {
            vec![name.to_owned()]
        } else {
            let mut owners: Vec<String> = self
                .types
                .values()
                .filter(|info| info.kind == TypeKind::Enum && info.def.cases.iter().any(|c| c.name == case))
                .map(|info| info.def.name.clone())
                .collect();
            owners.sort();
            owners
        };
        let owner = match owners.as_slice() {
            [owner] => owner.clone(),
            [] => return Err(Diagnostic::new(format!("no enum has a case called `{case}`"), span)),
            several => {
                return Err(Diagnostic::new(format!("more than one enum has a case called `{case}`"), span)
                    .with_help(format!(
                        "say which one: {}",
                        several
                            .iter()
                            .map(|owner| format!("`{owner}.{case}`"))
                            .collect::<Vec<_>>()
                            .join(" or ")
                    )));
            }
        };
        let info = self.type_info(&owner).ok_or_else(|| crate::eval::unknown_name(&owner, span))?;
        let fields =
            info.def.cases.iter().find(|c| c.name == case).map(|c| c.fields.clone()).unwrap_or_default();
        let shape = Shape { type_name: &owner, case: Some(case), fields: &fields, is_error: false };
        self.build(&shape, args, span)
    }

    /// Builds a value of a built-in error type, like the `IoError` that `fs` methods fail with.
    pub(crate) fn make_error(
        &mut self,
        type_name: &str,
        fields: Vec<(&str, Value)>,
        span: Span,
    ) -> Result<Value, Diagnostic> {
        let args = fields.into_iter().map(|(name, value)| CallArg::named(name, value, span)).collect();
        self.construct(type_name, args, span)
    }

    fn build(&mut self, shape: &Shape<'_>, args: Vec<CallArg>, span: Span) -> Result<Value, Diagnostic> {
        let all_literal = args.iter().all(|arg| arg.literal);
        let values = self.field_values(shape, args, span)?;
        let instance = Value::Instance(Rc::new(Instance {
            type_name: Rc::from(shape.type_name),
            case: shape.case.map(Rc::from),
            fields: values.iter().map(|(name, value)| (Rc::from(name.as_str()), value.clone())).collect(),
            is_error: shape.is_error,
            cause: None,
        }));
        if shape.fields.iter().all(|field| field.rule.is_none()) {
            return Ok(instance);
        }
        match self.broken_rule(shape, &values)? {
            None if all_literal => Ok(instance),
            None => Ok(Value::Ok(Rc::new(instance))),
            Some(field) if all_literal => Err(Diagnostic::new(
                format!("this breaks the `where` rule on `{}.{field}`", shape.type_name),
                span,
            )
            .with_help("`where` rules are checked whenever a value is made")),
            Some(field) => {
                let message = Value::str(&format!("{}.{field} breaks its `where` rule", shape.type_name));
                let error = self.make_error("InvalidValue", vec![("message", message)], span)?;
                Ok(Value::Err(Rc::new(error)))
            }
        }
    }

    /// Matches arguments to fields, positionally and by name, filling in defaults.
    fn field_values(
        &mut self,
        shape: &Shape<'_>,
        args: Vec<CallArg>,
        span: Span,
    ) -> Result<Vec<(String, Value)>, Diagnostic> {
        let display = shape
            .case
            .map_or_else(|| shape.type_name.to_owned(), |case| format!("{}.{case}", shape.type_name));
        let mut given: Vec<Option<Value>> = shape.fields.iter().map(|_| None).collect();
        for (position, arg) in args.into_iter().enumerate() {
            if arg.mut_place.is_some() {
                return Err(Diagnostic::new(
                    "building a value doesn't change its arguments, so don't pass `mut`",
                    arg.span,
                ));
            }
            let at = match &arg.name {
                Some(name) => shape.fields.iter().position(|field| &field.name == name).ok_or_else(|| {
                    Diagnostic::new(format!("`{display}` has no field called `{name}`"), arg.span)
                })?,
                None => position,
            };
            let slot = given.get_mut(at).ok_or_else(|| {
                Diagnostic::new(format!("`{display}` takes {} value(s)", shape.fields.len()), arg.span)
            })?;
            if slot.is_some() {
                return Err(Diagnostic::new("this field is given twice", arg.span));
            }
            *slot = Some(arg.value);
        }
        let mut values = Vec::with_capacity(shape.fields.len());
        for (field, value) in shape.fields.iter().zip(given) {
            let value = match (value, &field.default) {
                (Some(value), _) => value,
                (None, Some(default)) => self.eval(default)?,
                (None, None) => {
                    return Err(Diagnostic::new(
                        format!("`{display}` needs a value for `{}`", field.name),
                        span,
                    )
                    .with_help(format!("pass it, like `{display}({}=...)`", field.name)));
                }
            };
            values.push((field.name.clone(), value));
        }
        Ok(values)
    }

    /// The first field whose `where` rule the values break, if any.
    fn broken_rule(
        &mut self,
        shape: &Shape<'_>,
        values: &[(String, Value)],
    ) -> Result<Option<String>, Diagnostic> {
        self.frame.push();
        for (name, value) in values {
            self.frame.declare(name, value.clone(), false);
        }
        let mut broken = None;
        let mut failure = None;
        for field in shape.fields {
            let Some(rule) = &field.rule else { continue };
            match self.truth(rule, "where") {
                Ok(true) => {}
                Ok(false) => {
                    broken = Some(field.name.clone());
                    break;
                }
                Err(err) => {
                    failure = Some(err);
                    break;
                }
            }
        }
        self.frame.pop();
        failure.map_or(Ok(broken), Err)
    }

    /// `replace(value, field=new, ...)`: a copy with some fields changed.
    pub(crate) fn replace(args: Vec<CallArg>, span: Span) -> Result<Value, Diagnostic> {
        let mut args = args.into_iter();
        let Some(CallArg { value: Value::Instance(instance), name: None, .. }) = args.next() else {
            return Err(Diagnostic::new("`replace` takes a struct value, then the fields to change", span)
                .with_help("like `replace(enemy, health=50)`"));
        };
        let mut changed = instance.as_ref().clone();
        for arg in args {
            let name = arg
                .name
                .ok_or_else(|| Diagnostic::new("name the field to change, like `health=50`", arg.span))?;
            let field = changed.field_mut(&name).ok_or_else(|| {
                Diagnostic::new(
                    format!(
                        "`{}` has no field called `{name}`",
                        format::repr(&Value::Type(Rc::clone(&instance.type_name)))
                    ),
                    arg.span,
                )
            })?;
            *field = arg.value;
        }
        Ok(Value::Instance(Rc::new(changed)))
    }
}
