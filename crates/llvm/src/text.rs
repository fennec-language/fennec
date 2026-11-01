//! Printing and f-strings.

use fennec_interp::embed::layout::{TEXT, TEXT_BYTES, TEXT_CHARS};
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
                Some(kind) => self.print_one(value, kind)?,
            }
        }
        self.print_text("\n");
        Ok(())
    }

    /// Prints one value: an optional as what it holds, or `None`.
    fn print_one(&mut self, value: &str, kind: &Kind) -> Result<(), String> {
        if let Kind::Maybe(inner) = kind {
            let none = self.temp();
            self.line(&format!("{none} = icmp eq ptr {value}, null"));
            let (holds, missing, joined) = (self.label("holds"), self.label("none"), self.label("printed"));
            self.line(&format!("br i1 {none}, label %{missing}, label %{holds}"));
            self.start_block(&holds);
            self.print_one(value, inner)?;
            self.line(&format!("br label %{joined}"));
            self.start_block(&missing);
            self.print_text("None");
            self.line(&format!("br label %{joined}"));
            self.start_block(&joined);
            return Ok(());
        }
        if self.is_optional(kind) {
            let (number, fields) = self.case_of(kind, "Some")?;
            let (_, offset, held) = fields.first().cloned().ok_or("an optional with nothing in it")?;
            let held = held.ok_or("an optional of something this backend doesn't take yet")?;
            let base = self.fields_at(value, kind);
            let (tag, some) = (self.temp(), self.temp());
            self.line(&format!("{tag} = load i64, ptr {base}"));
            self.line(&format!("{some} = icmp eq i64 {tag}, {number}"));
            let (holds, none, joined) = (self.label("holds"), self.label("none"), self.label("printed"));
            self.line(&format!("br i1 {some}, label %{holds}, label %{none}"));
            self.start_block(&holds);
            let at = self.offset(&base, offset);
            let inner = if held.in_memory() { at } else { self.load_field(&held, &at, "") };
            self.print_one(&inner, &held)?;
            self.line(&format!("br label %{joined}"));
            self.start_block(&none);
            self.print_text("None");
            self.line(&format!("br label %{joined}"));
            self.start_block(&joined);
            return Ok(());
        }
        match kind {
            Kind::Int => {
                self.declare("declare void @fennec_print_int(i64) nounwind");
                self.line(&format!("call void @fennec_print_int(i64 {value})"));
            }
            Kind::Float => {
                self.declare("declare void @fennec_print_float(double) nounwind");
                self.line(&format!("call void @fennec_print_float(double {value})"));
            }
            Kind::Bool => {
                self.declare("declare void @fennec_print_bool(i8) nounwind");
                let wide = self.temp();
                self.line(&format!("{wide} = zext i1 {value} to i8"));
                self.line(&format!("call void @fennec_print_bool(i8 {wide})"));
            }
            Kind::Str => {
                self.declare("declare void @fennec_str_print(ptr) nounwind");
                self.line(&format!("call void @fennec_str_print(ptr {value})"));
            }
            Kind::List(_) | Kind::Boxed(_) | Kind::Dict(..) => {
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
            other => {
                return Err(format!("it prints a {other:?}, which this backend doesn't take yet"));
            }
        }
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
                Kind::List(_) | Kind::Boxed(_) | Kind::Dict(..) => {
                    self.declare("declare i64 @fennec_mark() nounwind");
                    self.declare("declare void @fennec_truncate(i64) nounwind");
                    self.declare("declare i64 @fennec_box_to_value(ptr) nounwind");
                    self.declare("declare ptr @fennec_value_text(i64) nounwind");
                    let (mark, handle) = (self.temp(), self.temp());
                    self.line(&format!("{mark} = call i64 @fennec_mark()"));
                    self.line(&format!("{handle} = call i64 @fennec_box_to_value(ptr {value})"));
                    self.line(&format!("{temp} = call ptr @fennec_value_text(i64 {handle})"));
                    self.line(&format!("call void @fennec_truncate(i64 {mark})"));
                }
                other => return Err(format!("it formats a {other:?}, which this backend doesn't take yet")),
            }
            return Ok(temp);
        };
        let spec = self.literal(spec);
        let (from, to) = (i64::from(span.start), i64::from(span.end));
        match kind {
            Kind::Int => {
                self.declare("declare ptr @fennec_int_spec_text(i64, ptr, i64, i64) nounwind");
                self.line(&format!(
                    "{temp} = call ptr @fennec_int_spec_text(i64 {value}, ptr {spec}, i64 {from}, i64 {to})"
                ));
            }
            Kind::Float => {
                self.declare("declare ptr @fennec_float_spec_text(double, ptr, i64, i64) nounwind");
                self.line(&format!(
                    "{temp} = call ptr @fennec_float_spec_text(double {value}, ptr {spec}, i64 {from}, i64 {to})"
                ));
            }
            other => {
                return Err(format!("it formats a {other:?} by a spec, which this backend doesn't take yet"));
            }
        }
        self.leave_if_failed();
        Ok(temp)
    }

    pub(crate) fn format(&mut self, dest: LocalId, parts: &[FormatPart], span: Span) -> Result<(), String> {
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
            BinOp::Eq | BinOp::NotEq => Ok(self.same_text(op, (a, b))),
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

    /// `==` on two strings: the same block, different lengths and short ones decide it here.
    fn same_text(&mut self, op: BinOp, (a, b): (&str, &str)) -> String {
        self.declare("declare i8 @fennec_box_equal(ptr, ptr) nounwind");
        self.module.add_helper(
            "fen_same_text",
            format!(
                "define internal i1 @fen_same_text(ptr %a, ptr %b) #0 {{\nentry:\n  %same = icmp eq ptr %a, %b\n  br i1 %same, label %yes, label %lengths\nlengths:\n  %a_at = getelementptr inbounds i8, ptr %a, i64 {TEXT_BYTES}\n  %b_at = getelementptr inbounds i8, ptr %b, i64 {TEXT_BYTES}\n  %a_bytes = load i64, ptr %a_at\n  %b_bytes = load i64, ptr %b_at\n  %alike = icmp eq i64 %a_bytes, %b_bytes\n  br i1 %alike, label %size, label %no\nsize:\n  %short = icmp ule i64 %a_bytes, 8\n  br i1 %short, label %word, label %slow\nword:\n  %a_text = getelementptr inbounds i8, ptr %a, i64 {TEXT}\n  %b_text = getelementptr inbounds i8, ptr %b, i64 {TEXT}\n  %a_word = load i64, ptr %a_text\n  %b_word = load i64, ptr %b_text\n  %equal_word = icmp eq i64 %a_word, %b_word\n  ret i1 %equal_word\nslow:\n  %equal = call i8 @fennec_box_equal(ptr %a, ptr %b)\n  %is = icmp ne i8 %equal, 0\n  ret i1 %is\nyes:\n  ret i1 true\nno:\n  ret i1 false\n}}\n\n"
            ),
        );
        let bit = self.temp();
        self.line(&format!("{bit} = call i1 @fen_same_text(ptr {a}, ptr {b})"));
        if op == BinOp::Eq {
            return bit;
        }
        let flipped = self.temp();
        self.line(&format!("{flipped} = xor i1 {bit}, true"));
        flipped
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
        // In an ASCII string a character's place is its byte's, so one inside it comes from the
        // table, and only the rest go to the runtime, which counts characters from the start.
        let (bytes_at, chars_at) = (self.offset(text, 16), self.offset(text, 24));
        let (bytes, chars, ascii, inside, quick) =
            (self.temp(), self.temp(), self.temp(), self.temp(), self.temp());
        self.line(&format!("{bytes} = load i64, ptr {bytes_at}"));
        self.line(&format!("{chars} = load i64, ptr {chars_at}"));
        self.line(&format!("{ascii} = icmp eq i64 {bytes}, {chars}"));
        self.line(&format!("{inside} = icmp ult i64 {index}, {bytes}"));
        self.line(&format!("{quick} = and i1 {ascii}, {inside}"));
        let (table_path, runtime_path, joined) =
            (self.label("table"), self.label("count"), self.label("indexed"));
        self.line(&format!("br i1 {quick}, label %{table_path}, label %{runtime_path}, !prof !8"));
        self.start_block(&table_path);
        self.declare("@fennec_ascii = external dso_local global ptr");
        let start = self.offset(text, crate::emit::TEXT_START);
        let (lead_at, lead, wide, table, slot, from_table) =
            (self.temp(), self.temp(), self.temp(), self.temp(), self.temp(), self.temp());
        self.line(&format!("{lead_at} = getelementptr inbounds i8, ptr {start}, i64 {index}"));
        self.line(&format!("{lead} = load i8, ptr {lead_at}"));
        self.line(&format!("{wide} = zext i8 {lead} to i64"));
        self.line(&format!("{table} = load ptr, ptr @fennec_ascii, !invariant.load !1"));
        self.line(&format!("{slot} = getelementptr inbounds i64, ptr {table}, i64 {wide}"));
        self.line(&format!("{from_table} = load ptr, ptr {slot}"));
        self.retain(&from_table);
        let table_end = self.current.clone();
        self.line(&format!("br label %{joined}"));
        self.start_block(&runtime_path);
        self.declare("declare ptr @fennec_str_index(ptr, i64, i64, i64) nounwind");
        let (from, to) = (i64::from(span.start), i64::from(span.end));
        let counted = self.temp();
        self.line(&format!(
            "{counted} = call ptr @fennec_str_index(ptr {text}, i64 {index}, i64 {from}, i64 {to})"
        ));
        self.leave_if_failed();
        let runtime_end = self.current.clone();
        self.line(&format!("br label %{joined}"));
        self.start_block(&joined);
        let one = self.temp();
        self.line(&format!("{one} = phi ptr [ {from_table}, %{table_end} ], [ {counted}, %{runtime_end} ]"));
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
            "str.contains" => {
                let (text, _) = self.operand(first.ok_or("no argument")?)?;
                let (part, _) = self.operand(args.get(1).ok_or("no argument")?)?;
                let found = self.string_binary(BinOp::In, (&part, &text))?;
                self.assign(dest.ok_or("no result")?, &found)
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
