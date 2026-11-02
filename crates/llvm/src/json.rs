//! `std.json`, by the runtime's own `json.dumps` and `json.loads`. What's written crosses as a
//! temporary and what's read comes back as one, so a value held in place, like a plain struct,
//! goes by way of a block of its layout's shape made for the crossing.

use fennec_check::Type;
use fennec_ir::{LocalId, Operand};
use fennec_syntax::Span;

use crate::emit::Emit;
use crate::kinds::{Kind, element_symbol};
use crate::lists::FIELDS_OFFSET;

impl Emit<'_, '_, '_> {
    pub(crate) fn json_builtin(
        &mut self,
        dest: Option<LocalId>,
        name: &str,
        args: &[Operand],
        span: Span,
    ) -> Result<(), String> {
        let dest = dest.ok_or("no result")?;
        let first = args.first().ok_or("no argument")?;
        let (from, to) = (i64::from(span.start), i64::from(span.end));
        self.declare("declare i64 @fennec_mark() nounwind");
        self.declare("declare void @fennec_truncate(i64) nounwind");
        let mark = self.temp();
        self.line(&format!("{mark} = call i64 @fennec_mark()"));
        match name {
            "json.dumps" => {
                let (value, kind) = self.operand(first)?;
                let (handle, crossed) = self.value_handle(&value, &kind)?;
                self.declare("declare ptr @fennec_json_dumps(i64, i64, i64) nounwind");
                let text = self.temp();
                self.line(&format!(
                    "{text} = call ptr @fennec_json_dumps(i64 {handle}, i64 {from}, i64 {to})"
                ));
                self.leave_if_failed();
                if let Some(block) = crossed {
                    self.release(&block);
                }
                self.line(&format!("call void @fennec_truncate(i64 {mark})"));
                self.assign(dest, &text)
            }
            "json.loads" => {
                let (text, _) = self.operand(first)?;
                // The type to read is the one the program wrote, here as a string.
                let local = self.function.locals.get(dest.0).ok_or("no such local")?;
                let Type::Result(wanted, _) = &local.ty else {
                    return Err("`json.loads` giving something other than a result".to_owned());
                };
                let written = self.literal(&wanted.to_string());
                self.declare("declare i64 @fennec_json_loads(ptr, ptr, i64, i64) nounwind");
                let handle = self.temp();
                self.line(&format!(
                    "{handle} = call i64 @fennec_json_loads(ptr {text}, ptr {written}, i64 {from}, i64 {to})"
                ));
                self.leave_if_failed();
                // `Ok` with what was read, or `Err` with the `ParseError`, made here.
                let kind = self.kind_of(dest)?.clone();
                let block = if let Kind::Boxed(id) = kind { Some(self.new_block(id)?) } else { None };
                let base = if let Some(block) = &block {
                    self.offset(block, FIELDS_OFFSET)
                } else {
                    let (slot, text) = (self.slot(dest), self.text_of(&kind));
                    self.line(&format!("store {text} zeroinitializer, ptr {slot}"));
                    slot
                };
                self.declare("declare i8 @fennec_is_err(i64) nounwind");
                self.declare("declare i64 @fennec_unwrap(i64) nounwind");
                let (failed, bad) = (self.temp(), self.temp());
                self.line(&format!("{failed} = call i8 @fennec_is_err(i64 {handle})"));
                self.line(&format!("{bad} = icmp ne i8 {failed}, 0"));
                let (err, ok, done) = (self.label("unread"), self.label("read"), self.label("loaded"));
                self.line(&format!("br i1 {bad}, label %{err}, label %{ok}"));
                self.start_block(&err);
                self.declare("declare i64 @fennec_error_of(i64) nounwind");
                let error = self.temp();
                self.line(&format!("{error} = call i64 @fennec_error_of(i64 {handle})"));
                self.write_case(&kind, &base, ("Err", &error), span)?;
                self.line(&format!("br label %{done}"));
                self.start_block(&ok);
                let inner = self.temp();
                self.line(&format!("{inner} = call i64 @fennec_unwrap(i64 {handle})"));
                self.write_case(&kind, &base, ("Ok", &inner), span)?;
                self.line(&format!("br label %{done}"));
                self.start_block(&done);
                self.line(&format!("call void @fennec_truncate(i64 {mark})"));
                if let Some(block) = block {
                    self.assign(dest, &block)?;
                }
                Ok(())
            }
            other => Err(format!("it calls `{other}`, which this backend doesn't take yet")),
        }
    }

    /// A temporary for a value, and the block made to carry it across, to let go of after.
    fn value_handle(&mut self, value: &str, kind: &Kind) -> Result<(String, Option<String>), String> {
        let handle = self.temp();
        match kind {
            Kind::Int => {
                self.declare("declare i64 @fennec_int(i64) nounwind");
                self.line(&format!("{handle} = call i64 @fennec_int(i64 {value})"));
            }
            Kind::Float => {
                self.declare("declare i64 @fennec_float(double) nounwind");
                self.line(&format!("{handle} = call i64 @fennec_float(double {value})"));
            }
            Kind::Bool => {
                self.declare("declare i64 @fennec_bool(i8) nounwind");
                let byte = self.temp();
                self.line(&format!("{byte} = zext i1 {value} to i8"));
                self.line(&format!("{handle} = call i64 @fennec_bool(i8 {byte})"));
            }
            Kind::Str | Kind::List(_) | Kind::Boxed(_) | Kind::Dict(..) => {
                self.declare("declare i64 @fennec_box_to_value(ptr) nounwind");
                self.line(&format!("{handle} = call i64 @fennec_box_to_value(ptr {value})"));
            }
            Kind::Struct(id) | Kind::Enum(id) => {
                if self.holds_blocks(*id)? {
                    return Err("it writes JSON of a value held in place that holds blocks".to_owned());
                }
                let block = self.new_block(*id)?;
                let fields = self.offset(&block, FIELDS_OFFSET);
                self.store_field(kind, value, &fields, "");
                self.declare("declare i64 @fennec_box_to_value(ptr) nounwind");
                self.line(&format!("{handle} = call i64 @fennec_box_to_value(ptr {block})"));
                return Ok((handle, Some(block)));
            }
            other => {
                return Err(format!("it writes JSON of a {other:?}, which this backend doesn't take yet"));
            }
        }
        Ok((handle, None))
    }

    /// Writes a case of a result at `base`, its one field read from a temporary.
    fn write_case(
        &mut self,
        kind: &Kind,
        base: &str,
        (case, handle): (&str, &str),
        span: Span,
    ) -> Result<(), String> {
        let (number, fields) = self.case_of(kind, case)?;
        self.line(&format!("store i64 {number}, ptr {base}"));
        let Some((_, offset, held)) = fields.first().cloned() else { return Ok(()) };
        let held = held.ok_or("a result holding something this backend doesn't take yet")?;
        let at = self.offset(base, offset);
        self.native_at(handle, &held, &at, span)
    }

    /// Writes what a temporary holds at `at`, as a value of this kind, with an owner of its own,
    /// failing as the interpreter does when it isn't one.
    fn native_at(&mut self, handle: &str, kind: &Kind, at: &str, span: Span) -> Result<(), String> {
        let (from, to) = (i64::from(span.start), i64::from(span.end));
        let value = self.temp();
        match kind {
            Kind::Int => {
                self.declare("declare i64 @fennec_to_int(i64, i64, i64) nounwind");
                self.line(&format!("{value} = call i64 @fennec_to_int(i64 {handle}, i64 {from}, i64 {to})"));
            }
            Kind::Float => {
                self.declare("declare double @fennec_to_float(i64, i64, i64) nounwind");
                self.line(&format!(
                    "{value} = call double @fennec_to_float(i64 {handle}, i64 {from}, i64 {to})"
                ));
            }
            Kind::Bool => {
                self.declare("declare i8 @fennec_to_bool(i64, i64, i64) nounwind");
                let byte = self.temp();
                self.line(&format!("{byte} = call i8 @fennec_to_bool(i64 {handle}, i64 {from}, i64 {to})"));
                self.line(&format!("{value} = icmp ne i8 {byte}, 0"));
            }
            Kind::Str => {
                self.declare("declare ptr @fennec_str_from_value(i64, i64, i64) nounwind");
                self.line(&format!(
                    "{value} = call ptr @fennec_str_from_value(i64 {handle}, i64 {from}, i64 {to})"
                ));
            }
            Kind::List(item) => {
                let symbol = element_symbol(self.module.layouts, *item);
                self.module.shared.elements.insert(symbol.clone(), *item);
                self.declare(&format!("@\"{symbol}\" = external dso_local global i64"));
                self.declare("declare ptr @fennec_list_from_value(i64, i64, i64, i64) nounwind");
                let element = self.temp();
                self.line(&format!("{element} = load i64, ptr @\"{symbol}\", !invariant.load !1"));
                self.line(&format!(
                    "{value} = call ptr @fennec_list_from_value(i64 {handle}, i64 {element}, i64 {from}, i64 {to})"
                ));
            }
            Kind::Boxed(id) => {
                let shape = self.shape_of(*id)?;
                self.declare("declare ptr @fennec_box_from_value(i64, i64, i64, i64) nounwind");
                self.line(&format!(
                    "{value} = call ptr @fennec_box_from_value(i64 {handle}, i64 {shape}, i64 {from}, i64 {to})"
                ));
            }
            Kind::Struct(id) | Kind::Enum(id) => {
                if self.holds_blocks(*id)? {
                    return Err("it reads JSON into a value held in place that holds blocks".to_owned());
                }
                let shape = self.shape_of(*id)?;
                self.declare("declare ptr @fennec_box_from_value(i64, i64, i64, i64) nounwind");
                self.line(&format!(
                    "{value} = call ptr @fennec_box_from_value(i64 {handle}, i64 {shape}, i64 {from}, i64 {to})"
                ));
                self.leave_if_failed();
                let fields = self.offset(&value, FIELDS_OFFSET);
                self.store_field(kind, &fields, at, "");
                self.release(&value);
                return Ok(());
            }
            other => {
                return Err(format!("it reads JSON into a {other:?}, which this backend doesn't take yet"));
            }
        }
        self.leave_if_failed();
        self.store_field(kind, &value, at, "");
        Ok(())
    }
}
