//! `std.json` and `std.toml`, which native code reads and writes through std's own code in Fennec:
//! what can't be is refused here. And results made from what the runtime gives back.

use fennec_ir::LocalId;
use fennec_syntax::Span;

use crate::emit::Emit;
use crate::kinds::{Kind, element_symbol};
use crate::lists::FIELDS_OFFSET;

/// Why `json.dumps`, `json.loads`, `toml.dumps` or `toml.loads`, `name`, of what has no `def
/// serialize` or `def deserialize`, isn't built: std's readers and writers in Fennec can't take
/// it, and native code has no other way to write or read it. `name` may have named arguments on
/// its end, as in `json.dumps.indent`.
pub(crate) fn not_saved_natively(name: &str) -> String {
    let function = name.split('.').take(2).collect::<Vec<_>>().join(".");
    format!("its `{function}` is of a type that can't be saved natively yet")
}

impl Emit<'_, '_, '_> {
    /// A result made here from a temporary holding `Ok` or `Err`, written into `dest`'s slot, or
    /// into a block given back for `dest` to take once the temporaries are let go of.
    pub(crate) fn result_from(
        &mut self,
        dest: LocalId,
        handle: &str,
        span: Span,
    ) -> Result<Option<String>, String> {
        let kind = self.kind_of(dest)?.clone();
        let block = if let Kind::Boxed(id) = kind { Some(self.new_block(id)?) } else { None };
        let base = if let Some(block) = &block {
            self.offset(block, FIELDS_OFFSET)
        } else {
            self.release_held(dest)?;
            let slot = self.slot(dest);
            let zero = self.zero_line(&kind, &slot);
            self.line(&zero);
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
        Ok(block)
    }

    /// A temporary for a value, and the block made to carry it across, to let go of after.
    pub(crate) fn value_handle(
        &mut self,
        value: &str,
        kind: &Kind,
    ) -> Result<(String, Option<String>), String> {
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
            // A header crosses as a block of its own, let go of after.
            Kind::Str if self.headers() => {
                self.declare("declare i64 @fennec_box_to_value(ptr) nounwind");
                let block = self.owned_text_block(value);
                self.line(&format!("{handle} = call i64 @fennec_box_to_value(ptr {block})"));
                return Ok((handle, Some(block)));
            }
            Kind::Str | Kind::List(_) | Kind::Boxed(_) | Kind::Bytes | Kind::Dict(..) => {
                self.declare("declare i64 @fennec_box_to_value(ptr) nounwind");
                self.line(&format!("{handle} = call i64 @fennec_box_to_value(ptr {value})"));
            }
            Kind::Struct(id) | Kind::Enum(id) => {
                let block = self.new_block(*id)?;
                let fields = self.offset(&block, FIELDS_OFFSET);
                self.store_field(kind, value, &fields, "");
                // The block owns what its copy holds, and lets go of it when it goes.
                self.retain_inline(kind, &fields);
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
    /// failing when it isn't one.
    pub(crate) fn native_at(
        &mut self,
        handle: &str,
        kind: &Kind,
        at: &str,
        span: Span,
    ) -> Result<(), String> {
        let (from, to) = self.place_in_source(span);
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
                if self.headers() {
                    self.leave_if_failed();
                    let header = self.text_from_block(&value);
                    self.store_field(kind, &header, at, "");
                    return Ok(());
                }
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
            Kind::Bytes => {
                let shape = fennec_rt::embed::heap::BYTES_SHAPE;
                self.declare("declare ptr @fennec_box_from_value(i64, i64, i64, i64) nounwind");
                self.line(&format!(
                    "{value} = call ptr @fennec_box_from_value(i64 {handle}, i64 {shape}, i64 {from}, i64 {to})"
                ));
            }
            // The runtime writes a dict of the shape for its keys and values.
            Kind::Dict(key, held) => {
                let shape = self.dict_shape(*key, *held);
                self.declare("declare ptr @fennec_box_from_value(i64, i64, i64, i64) nounwind");
                self.line(&format!(
                    "{value} = call ptr @fennec_box_from_value(i64 {handle}, i64 {shape}, i64 {from}, i64 {to})"
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
                let shape = self.shape_of(*id)?;
                self.declare("declare ptr @fennec_box_from_value(i64, i64, i64, i64) nounwind");
                self.line(&format!(
                    "{value} = call ptr @fennec_box_from_value(i64 {handle}, i64 {shape}, i64 {from}, i64 {to})"
                ));
                self.leave_if_failed();
                let fields = self.offset(&value, FIELDS_OFFSET);
                self.store_field(kind, &fields, at, "");
                // What it holds outlives the block it came in.
                self.retain_inline(kind, at);
                self.release(&value);
                return Ok(());
            }
            // An optional string: all zeros for `None`, otherwise as a string is held.
            Kind::MaybeText => {
                self.declare("declare i8 @fennec_is_none(i64) nounwind");
                let (byte, none) = (self.temp(), self.temp());
                self.line(&format!("{byte} = call i8 @fennec_is_none(i64 {handle})"));
                self.line(&format!("{none} = icmp ne i8 {byte}, 0"));
                let (missing, holds, joined) = (self.label("none"), self.label("holds"), self.label("read"));
                self.line(&format!("br i1 {none}, label %{missing}, label %{holds}"));
                self.start_block(&missing);
                self.store_field(kind, "zeroinitializer", at, "");
                self.line(&format!("br label %{joined}"));
                self.start_block(&holds);
                self.native_at(handle, &Kind::Str, at, span)?;
                self.line(&format!("br label %{joined}"));
                self.start_block(&joined);
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
