//! Printing and f-strings.

use fennec_ir::{FormatPart, LocalId, Operand};
use fennec_syntax::Span;

use crate::emit::Emit;
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
        self.declare("@fennec_failed = external dso_local global ptr");
        let (flag_at, flag, failed) = (self.temp(), self.temp(), self.temp());
        self.line(&format!("{flag_at} = load ptr, ptr @fennec_failed, !invariant.load !1"));
        self.line(&format!("{flag} = load i8, ptr {flag_at}"));
        self.line(&format!("{failed} = icmp ne i8 {flag}, 0"));
        self.leave_if(&failed);
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
}
