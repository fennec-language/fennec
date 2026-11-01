//! `==` when a type has its own `def ==`. Comparing then looks inside values for one: a struct
//! compares its fields, and a list its items, each with their type's own `==` if it has one.

use fennec_syntax::{BinOp, Diagnostic, Span};

use crate::interp::Interp;
use crate::value::Value;

impl Interp<'_> {
    /// `==`, `!=`, `in` and `not in` once some type has its own `def ==`, or `None` for
    /// anything else.
    pub(crate) fn compare_own(
        &mut self,
        op: BinOp,
        left: &Value,
        right: &Value,
        span: Span,
    ) -> Result<Option<Value>, Diagnostic> {
        let found = match (op, right) {
            (BinOp::Eq, _) => self.equal(left, right, span)?,
            (BinOp::NotEq, _) => !self.equal(left, right, span)?,
            (BinOp::In | BinOp::NotIn, Value::List(items) | Value::Tuple(items)) => {
                (op == BinOp::NotIn) != self.position(items, left, span)?.is_some()
            }
            _ => return Ok(None),
        };
        Ok(Some(Value::Bool(found)))
    }

    /// Where `item` first is among `items`.
    pub(crate) fn position(
        &mut self,
        items: &[Value],
        item: &Value,
        span: Span,
    ) -> Result<Option<usize>, Diagnostic> {
        for (at, each) in items.iter().enumerate() {
            if self.equal(each, item, span)? {
                return Ok(Some(at));
            }
        }
        Ok(None)
    }

    /// Whether two values are equal, calling a type's own `def ==` wherever there is one.
    pub(crate) fn equal(&mut self, a: &Value, b: &Value, span: Span) -> Result<bool, Diagnostic> {
        if !self.own_equality {
            return Ok(a == b);
        }
        match (a, b) {
            (Value::Instance(one), Value::Instance(other)) if one.type_name == other.type_name => {
                let own = self.type_info(&one.type_name).and_then(|info| info.methods.get("==").cloned());
                if let Some(own) = own {
                    return Ok(
                        self.call_with_values(&own, vec![a.clone(), b.clone()], span)? == Value::Bool(true)
                    );
                }
                if one.case != other.case || one.fields.len() != other.fields.len() {
                    return Ok(false);
                }
                for ((_, mine), (_, theirs)) in one.fields.iter().zip(&other.fields) {
                    if !self.equal(mine, theirs, span)? {
                        return Ok(false);
                    }
                }
                Ok(true)
            }
            (Value::List(one), Value::List(other)) | (Value::Tuple(one), Value::Tuple(other)) => {
                if one.len() != other.len() {
                    return Ok(false);
                }
                for (mine, theirs) in one.iter().zip(other.iter()) {
                    if !self.equal(mine, theirs, span)? {
                        return Ok(false);
                    }
                }
                Ok(true)
            }
            (Value::Ok(one), Value::Ok(other)) | (Value::Err(one), Value::Err(other)) => {
                self.equal(one, other, span)
            }
            _ => Ok(a == b),
        }
    }
}
