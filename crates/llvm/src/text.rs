//! Printing and f-strings.

use fennec_interp::embed::layout::TEXT_CHARS;
use fennec_ir::{FormatPart, LocalId, Operand};
use fennec_syntax::{BinOp, Span};

use crate::emit::{Emit, LENGTH_ACCESS};
use crate::kinds::Kind;

impl Emit<'_, '_, '_> {
    /// The number of a string, read from its symbol, since it depends on the whole program.
    fn string_number(&mut self, text: &str) -> String {
        let name = format!("s_{}", fennec_blake3::hash(text.as_bytes()).to_hex());
        self.declare(&format!("@\"{name}\" = external dso_local global i64"));
        self.module.shared.strings.insert(name.clone(), text.to_owned());
        let temp = self.temp();
        self.line(&format!("{temp} = load i64, ptr @\"{name}\", !invariant.load !1"));
        temp
    }

    /// The address of a literal's block, filled in when the program starts.
    pub(crate) fn literal(&mut self, text: &str) -> String {
        let name = format!("l_{}", fennec_blake3::hash(text.as_bytes()).to_hex());
        self.declare(&format!("@\"{name}\" = external dso_local global ptr"));
        self.module.shared.literals.insert(name.clone(), text.to_owned());
        let temp = self.temp();
        self.line(&format!("{temp} = load ptr, ptr @\"{name}\", !invariant.load !1"));
        temp
    }

    fn print_text(&mut self, text: &str) {
        let number = self.string_number(text);
        self.declare("declare void @fennec_print_str(i64) nounwind");
        self.line(&format!("call void @fennec_print_str(i64 {number})"));
    }

    pub(crate) fn print(&mut self, args: &[Operand]) -> Result<(), String> {
        // Everything is worked out before anything prints.
        let mut values = Vec::with_capacity(args.len());
        for arg in args {
            values.push(match arg {
                Operand::Str(text) => (text.clone(), None),
                other => {
                    let (value, kind) = self.operand(other)?;
                    (value, Some(kind))
                }
            });
        }
        for (at, (value, kind)) in values.iter().enumerate() {
            if at > 0 {
                self.print_text(" ");
            }
            match kind {
                None => self.print_text(value),
                Some(Kind::Int) => {
                    self.declare("declare void @fennec_print_int(i64) nounwind");
                    self.line(&format!("call void @fennec_print_int(i64 {value})"));
                }
                Some(Kind::Float) => {
                    self.declare("declare void @fennec_print_float(double) nounwind");
                    self.line(&format!("call void @fennec_print_float(double {value})"));
                }
                Some(Kind::Bool) => {
                    self.declare("declare void @fennec_print_bool(i8) nounwind");
                    let wide = self.temp();
                    self.line(&format!("{wide} = zext i1 {value} to i8"));
                    self.line(&format!("call void @fennec_print_bool(i8 {wide})"));
                }
                Some(Kind::Str) => {
                    self.declare("declare void @fennec_str_print(ptr) nounwind");
                    self.line(&format!("call void @fennec_str_print(ptr {value})"));
                }
                Some(Kind::List(_) | Kind::Boxed(_) | Kind::Dict(..)) => {
                    self.declare("declare i64 @fennec_mark() nounwind");
                    self.declare("declare void @fennec_truncate(i64) nounwind");
                    self.declare("declare i64 @fennec_box_to_value(ptr) nounwind");
                    self.declare("declare void @fennec_print_value(i64) nounwind");
                    let (mark, handle) = (self.temp(), self.temp());
                    self.line(&format!("{mark} = call i64 @fennec_mark()"));
                    self.line(&format!("{handle} = call i64 @fennec_box_to_value(ptr {value})"));
                    self.line(&format!("call void @fennec_print_value(i64 {handle})"));
                    self.line(&format!("call void @fennec_truncate(i64 {mark})"));
                }
                Some(other) => {
                    return Err(format!("it prints a {other:?}, which this backend doesn't take yet"));
                }
            }
        }
        self.print_text("\n");
        Ok(())
    }

    /// A new string holding a value, by a spec if it has one.
    fn piece(&mut self, value: &Operand, spec: Option<&str>, span: Span) -> Result<String, String> {
        let (value, kind) = self.operand(value)?;
        let temp = self.temp();
        let Some(spec) = spec else {
            match kind {
                Kind::Int => {
                    self.declare("declare ptr @fennec_int_text(i64) nounwind");
                    self.line(&format!("{temp} = call ptr @fennec_int_text(i64 {value})"));
                }
                Kind::Float => {
                    self.declare("declare ptr @fennec_float_text(double) nounwind");
                    self.line(&format!("{temp} = call ptr @fennec_float_text(double {value})"));
                }
                Kind::Str => {
                    self.retain(&value);
                    return Ok(value);
                }
                other => return Err(format!("it formats a {other:?}, which this backend doesn't take yet")),
            }
            return Ok(temp);
        };
        // A spec goes through a temporary of the runtime's until it can take a number directly.
        let boxed = self.temp();
        match kind {
            Kind::Int => {
                self.declare("declare i64 @fennec_int(i64) nounwind");
                self.line(&format!("{boxed} = call i64 @fennec_int(i64 {value})"));
            }
            Kind::Float => {
                self.declare("declare i64 @fennec_float(double) nounwind");
                self.line(&format!("{boxed} = call i64 @fennec_float(double {value})"));
            }
            other => {
                return Err(format!("it formats a {other:?} by a spec, which this backend doesn't take yet"));
            }
        }
        let spec = self.literal(spec);
        self.declare("declare ptr @fennec_spec_text(i64, ptr, i64, i64) nounwind");
        let (from, to) = (i64::from(span.start), i64::from(span.end));
        self.line(&format!(
            "{temp} = call ptr @fennec_spec_text(i64 {boxed}, ptr {spec}, i64 {from}, i64 {to})"
        ));
        self.leave_if_failed();
        Ok(temp)
    }

    pub(crate) fn format(&mut self, dest: LocalId, parts: &[FormatPart], span: Span) -> Result<(), String> {
        self.declare("declare i64 @fennec_mark() nounwind");
        self.declare("declare void @fennec_truncate(i64) nounwind");
        let mark = self.temp();
        self.line(&format!("{mark} = call i64 @fennec_mark()"));
        let mut built: Option<String> = None;
        for part in parts {
            let (piece, owned) = match part {
                FormatPart::Text(text) => (self.literal(text), false),
                FormatPart::Value { value, spec } => (self.piece(value, spec.as_deref(), span)?, true),
            };
            built = Some(match built {
                None if owned => piece,
                None => {
                    self.retain(&piece);
                    piece
                }
                Some(so_far) => {
                    self.declare("declare ptr @fennec_str_append(ptr, ptr) nounwind");
                    let joined = self.temp();
                    self.line(&format!("{joined} = call ptr @fennec_str_append(ptr {so_far}, ptr {piece})"));
                    if owned {
                        self.release(&piece);
                    }
                    joined
                }
            });
        }
        let value = if let Some(value) = built {
            value
        } else {
            let empty = self.literal("");
            self.retain(&empty);
            empty
        };
        self.line(&format!("call void @fennec_truncate(i64 {mark})"));
        self.assign(dest, &value)
    }

    /// An operator on two strings.
    pub(crate) fn string_binary(&mut self, op: BinOp, (a, b): (&str, &str)) -> Result<String, String> {
        match op {
            BinOp::Add => {
                self.declare("declare ptr @fennec_str_concat(ptr, ptr) nounwind");
                let joined = self.temp();
                self.line(&format!("{joined} = call ptr @fennec_str_concat(ptr {a}, ptr {b})"));
                Ok(joined)
            }
            BinOp::Eq | BinOp::NotEq => Ok(self.same_blocks(op, (a, b))),
            BinOp::Lt | BinOp::LtEq | BinOp::Gt | BinOp::GtEq => {
                self.declare("declare i64 @fennec_str_compare(ptr, ptr) nounwind");
                let cc = match op {
                    BinOp::Lt => "slt",
                    BinOp::LtEq => "sle",
                    BinOp::Gt => "sgt",
                    _ => "sge",
                };
                let (ordering, result) = (self.temp(), self.temp());
                self.line(&format!("{ordering} = call i64 @fennec_str_compare(ptr {a}, ptr {b})"));
                self.line(&format!("{result} = icmp {cc} i64 {ordering}, 0"));
                Ok(result)
            }
            BinOp::In | BinOp::NotIn => {
                self.declare("declare i8 @fennec_str_contains(ptr, ptr) nounwind");
                let (found, bit) = (self.temp(), self.temp());
                self.line(&format!("{found} = call i8 @fennec_str_contains(ptr {b}, ptr {a})"));
                self.line(&format!("{bit} = trunc i8 {found} to i1"));
                if op == BinOp::In {
                    return Ok(bit);
                }
                let flipped = self.temp();
                self.line(&format!("{flipped} = xor i1 {bit}, true"));
                Ok(flipped)
            }
            other => Err(format!("it uses {other:?} on strings, which this backend doesn't take yet")),
        }
    }

    /// `==` on two blocks, as the runtime compares them.
    pub(crate) fn same_blocks(&mut self, op: BinOp, (a, b): (&str, &str)) -> String {
        self.declare("declare i8 @fennec_box_equal(ptr, ptr) nounwind");
        let (same, bit) = (self.temp(), self.temp());
        self.line(&format!("{same} = call i8 @fennec_box_equal(ptr {a}, ptr {b})"));
        self.line(&format!("{bit} = trunc i8 {same} to i1"));
        if op == BinOp::Eq {
            return bit;
        }
        let flipped = self.temp();
        self.line(&format!("{flipped} = xor i1 {bit}, true"));
        flipped
    }

    /// `text[index]`, which leaves the function if it's out of range.
    pub(crate) fn string_index(
        &mut self,
        dest: LocalId,
        text: &str,
        index: &Operand,
        span: Span,
    ) -> Result<(), String> {
        let (index, _) = self.operand(index)?;
        self.declare("declare ptr @fennec_str_index(ptr, i64, i64, i64) nounwind");
        let (from, to) = (i64::from(span.start), i64::from(span.end));
        let one = self.temp();
        self.line(&format!(
            "{one} = call ptr @fennec_str_index(ptr {text}, i64 {index}, i64 {from}, i64 {to})"
        ));
        self.leave_if_failed();
        self.assign(dest, &one)
    }

    /// Leaves the function if the runtime has recorded a failure.
    pub(crate) fn leave_if_failed(&mut self) {
        self.declare("@fennec_failed = external dso_local global ptr");
        let (flag_at, flag, failed) = (self.temp(), self.temp(), self.temp());
        self.line(&format!("{flag_at} = load ptr, ptr @fennec_failed, !invariant.load !1"));
        self.line(&format!("{flag} = load i8, ptr {flag_at}"));
        self.line(&format!("{failed} = icmp ne i8 {flag}, 0"));
        self.leave_if(&failed);
    }

    /// `str(value)`.
    pub(crate) fn text_of_value(&mut self, dest: LocalId, value: &Operand) -> Result<(), String> {
        let (held, kind) = self.operand(value)?;
        let temp = self.temp();
        match kind {
            Kind::Int => {
                self.declare("declare ptr @fennec_int_text(i64) nounwind");
                self.line(&format!("{temp} = call ptr @fennec_int_text(i64 {held})"));
            }
            Kind::Float => {
                self.declare("declare ptr @fennec_float_text(double) nounwind");
                self.line(&format!("{temp} = call ptr @fennec_float_text(double {held})"));
            }
            Kind::Str => {
                self.retain(&held);
                return self.assign(dest, &held);
            }
            other => {
                return Err(format!("it calls `str` on a {other:?}, which this backend doesn't take yet"));
            }
        }
        self.assign(dest, &temp)
    }

    /// No case of a `match` took the value. The runtime's message shows it where it can.
    pub(crate) fn no_match(&mut self, value: &Operand, span: Span) -> Result<(), String> {
        let (held, kind) = self.operand(value)?;
        let handle = self.temp();
        match kind {
            Kind::Int => {
                self.declare("declare i64 @fennec_int(i64) nounwind");
                self.line(&format!("{handle} = call i64 @fennec_int(i64 {held})"));
            }
            Kind::Float => {
                self.declare("declare i64 @fennec_float(double) nounwind");
                self.line(&format!("{handle} = call i64 @fennec_float(double {held})"));
            }
            Kind::Str | Kind::List(_) | Kind::Boxed(_) => {
                self.declare("declare i64 @fennec_box_to_value(ptr) nounwind");
                self.line(&format!("{handle} = call i64 @fennec_box_to_value(ptr {held})"));
            }
            // The checker has shown a `match` on an enum takes every case, so nothing's shown.
            Kind::Enum(_) => {
                self.declare("declare i64 @fennec_unit() nounwind");
                self.line(&format!("{handle} = call i64 @fennec_unit()"));
            }
            other => return Err(format!("it matches on a {other:?}, which this backend doesn't take yet")),
        }
        self.declare("declare void @fennec_no_match(i64, i64, i64) #1");
        let (from, to) = (i64::from(span.start), i64::from(span.end));
        self.line(&format!("call void @fennec_no_match(i64 {handle}, i64 {from}, i64 {to})"));
        let failed = self.failed_exit();
        self.line(&failed);
        Ok(())
    }

    /// A builtin on strings.
    pub(crate) fn text_builtin(
        &mut self,
        dest: Option<LocalId>,
        name: &str,
        args: &[Operand],
        span: Span,
    ) -> Result<(), String> {
        let first = args.first();
        match name {
            "str.len" => {
                let (text, _) = self.operand(first.ok_or("no argument")?)?;
                let (at, length) = (self.temp(), self.temp());
                self.line(&format!("{at} = getelementptr inbounds i8, ptr {text}, i64 {TEXT_CHARS}"));
                self.line(&format!("{length} = load i64, ptr {at}{LENGTH_ACCESS}"));
                self.assign(dest.ok_or("no result")?, &length)
            }
            "str" => self.text_of_value(dest.ok_or("no result")?, first.ok_or("no argument")?),
            "str.code" => {
                let (text, _) = self.operand(first.ok_or("no argument")?)?;
                self.declare("declare i64 @fennec_str_code(ptr, i64, i64) nounwind");
                let (from, to) = (i64::from(span.start), i64::from(span.end));
                let temp = self.temp();
                self.line(&format!("{temp} = call i64 @fennec_str_code(ptr {text}, i64 {from}, i64 {to})"));
                self.leave_if_failed();
                self.assign(dest.ok_or("no result")?, &temp)
            }
            "str.join" => {
                let (separator, _) = self.operand(first.ok_or("no argument")?)?;
                let (list, _) = self.operand(args.get(1).ok_or("no argument")?)?;
                self.declare("declare ptr @fennec_str_join(ptr, ptr) nounwind");
                let temp = self.temp();
                self.line(&format!("{temp} = call ptr @fennec_str_join(ptr {separator}, ptr {list})"));
                self.assign(dest.ok_or("no result")?, &temp)
            }
            other => Err(format!("it calls `{other}`, which this backend doesn't take yet")),
        }
    }
}
