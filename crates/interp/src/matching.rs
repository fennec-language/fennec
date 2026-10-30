//! Matching values against `case` patterns. A plain name never matches an error, so a
//! forgotten error can't slip into the success case.

use fennec_syntax::{Diagnostic, ExprKind, Pattern, Span};

use crate::interp::Interp;
use crate::value::{Instance, Value};

type Bindings = Vec<(String, Value)>;

impl Interp<'_> {
    pub(crate) fn matches(
        &self,
        pattern: &Pattern,
        value: &Value,
        span: Span,
    ) -> Result<Option<Bindings>, Diagnostic> {
        // Success-side patterns look through `Ok`.
        let success = match value {
            Value::Ok(inner) => inner.as_ref(),
            Value::Err(_) => value,
            other => other,
        };
        match pattern {
            Pattern::Wildcard => Ok(Some(Vec::new())),
            Pattern::Bind(name) => Ok((!value.is_failure()).then(|| vec![(name.clone(), success.clone())])),
            Pattern::Literal(expr) => Ok((literal(&expr.kind) == *success).then(Vec::new)),
            Pattern::Tuple(patterns) => match success {
                Value::Tuple(items) if items.len() == patterns.len() => self.all_match(patterns, items, span),
                _ => Ok(None),
            },
            Pattern::Variant { name, dotted: false, args } if name == "Ok" => match value {
                Value::Err(_) => Ok(None),
                _ => self.single(args, success, span),
            },
            Pattern::Variant { name, dotted: false, args } if name == "Err" => match value {
                Value::Err(error) => self.single(args, error, span),
                _ => Ok(None),
            },
            Pattern::Variant { name, dotted, args } => {
                let target = match (value, dotted) {
                    (Value::Err(error), false) => error.as_ref(),
                    (Value::Err(_), true) => return Ok(None),
                    _ => success,
                };
                let Value::Instance(instance) = target else { return Ok(None) };
                let named = match &instance.case {
                    Some(case) => case.as_ref() == name,
                    None => !dotted && instance.type_name.as_ref() == name,
                };
                if named { self.fields_match(args, instance, span) } else { Ok(None) }
            }
        }
    }

    fn single(&self, args: &[Pattern], value: &Value, span: Span) -> Result<Option<Bindings>, Diagnostic> {
        match args {
            [] => Ok(Some(Vec::new())),
            [pattern] => self.matches(pattern, value, span),
            _ => Err(Diagnostic::new("`Ok` and `Err` hold one value", span)),
        }
    }

    fn fields_match(
        &self,
        args: &[Pattern],
        instance: &Instance,
        span: Span,
    ) -> Result<Option<Bindings>, Diagnostic> {
        if args.len() > instance.fields.len() {
            return Err(Diagnostic::new(
                format!(
                    "this pattern has {} parts, but the value has {} fields",
                    args.len(),
                    instance.fields.len()
                ),
                span,
            ));
        }
        let values: Vec<Value> = instance.fields.iter().map(|(_, value)| value.clone()).collect();
        self.all_match(args, values.get(..args.len()).unwrap_or(&[]), span)
    }

    fn all_match(
        &self,
        patterns: &[Pattern],
        values: &[Value],
        span: Span,
    ) -> Result<Option<Bindings>, Diagnostic> {
        let mut bindings = Vec::new();
        for (pattern, value) in patterns.iter().zip(values) {
            match self.matches(pattern, value, span)? {
                Some(found) => bindings.extend(found),
                None => return Ok(None),
            }
        }
        Ok(Some(bindings))
    }
}

fn literal(kind: &ExprKind) -> Value {
    match kind {
        ExprKind::Int(n) => Value::Int(*n),
        ExprKind::Float(x) => Value::Float(*x),
        ExprKind::Str(text) => Value::str(text),
        ExprKind::Bool(b) => Value::Bool(*b),
        _ => Value::None,
    }
}
