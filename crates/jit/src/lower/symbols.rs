//! Numbers that depend on the whole program or on this run, read through data symbols named by
//! what they stand for, so a function's machine code depends only on the function.
//!
//! A string literal's address changes every run, and a name's number changes whenever the
//! program gains or loses a name earlier on. Written into the code as constants, they'd make
//! a function's machine code differ from run to run, and no compiled function could ever be
//! reused. So when compiling to run here, code loads them from data symbols instead: content
//! like a name or a literal names its own symbol, and what a function hands to the
//! interpreter is named by the function and how many it handed over before. Each run defines
//! the symbols with its own values. An executable is built whole and never reused, so it uses
//! the numbers themselves.

use cranelift_codegen::ir::{GlobalValue, InstBuilder, MemFlagsData, Value, types};
use cranelift_frontend::FunctionBuilder;
use cranelift_module::{DataDescription, DataId, Linkage, Module};
use fennec_syntax::{Diagnostic, FnDef, Span};

use std::collections::HashMap;

use super::{Lowerer, Shared, internal};

/// The data symbol `symbol`, declared the first time it's used, holding `value` this run.
fn data(
    module: &mut dyn Module,
    shared: &mut Shared,
    symbol: String,
    value: i64,
) -> Result<DataId, Diagnostic> {
    if let Some((id, stored)) = shared.symbols.get_mut(&symbol) {
        *stored = value;
        return Ok(*id);
    }
    let id = module
        .declare_data(&symbol, Linkage::Local, false, false)
        .map_err(|err| internal(err, Span::default()))?;
    shared.symbols.insert(symbol, (id, value));
    Ok(id)
}

/// Loads a symbol's value. It never changes during the run, so the load can move: out of
/// loops, and into the cold blocks that are its only uses.
fn load_from(b: &mut FunctionBuilder<'_>, pointer: types::Type, global: GlobalValue) -> Value {
    let address = b.ins().symbol_value(pointer, global);
    let flags = MemFlagsData::trusted().with_readonly().with_can_move();
    b.ins().load(types::I64, flags, address, 0)
}

/// Where a function starts in its file, which failures' positions are relative to, loaded in
/// its entry block. None in an executable, which uses positions as they are.
pub(super) fn origin(
    b: &mut FunctionBuilder<'_>,
    module: &mut dyn Module,
    shared: &mut Shared,
    function: &crate::types::Function<'_>,
    digest: &str,
) -> Result<Option<Value>, Diagnostic> {
    if shared.slots.is_some() {
        return Ok(None);
    }
    let start = i64::from(function.def.span.start);
    let id = data(module, shared, format!("fennec.origin.{digest}"), start)?;
    let global = module.declare_data_in_func(id, b.func);
    Ok(Some(load_from(b, module.target_config().pointer_type(), global)))
}

/// The failure flag native code checks after each call into the runtime. It's linked in by
/// name like the runtime's functions, so code computes its address directly: no load, and a
/// value the register allocator can recompute instead of keeping it in a register.
pub(super) const FAILED: &str = "fennec_failed";

/// The address of something in the runtime linked in by name, the same in every run's code.
pub(super) fn import(
    b: &mut FunctionBuilder<'_>,
    module: &mut dyn Module,
    name: &str,
) -> Result<Value, Diagnostic> {
    let id = module
        .declare_data(name, Linkage::Import, true, false)
        .map_err(|err| internal(err, Span::default()))?;
    let global = module.declare_data_in_func(id, b.func);
    let pointer = module.target_config().pointer_type();
    Ok(b.ins().symbol_value(pointer, global))
}

/// Gives every symbol this run's value, once all the functions are lowered.
pub(super) fn define(module: &mut dyn Module, shared: &Shared) -> Result<(), Diagnostic> {
    for (id, value) in shared.symbols.values() {
        let mut data = DataDescription::new();
        data.define(value.to_le_bytes().to_vec().into_boxed_slice());
        data.set_align(8);
        module.define_data(*id, &data).map_err(|err| internal(err, Span::default()))?;
    }
    Ok(())
}

/// A short, stable name for text that may hold anything.
pub(super) fn digest(text: &str) -> String {
    let hex = fennec_blake3::hash(text.as_bytes()).to_hex();
    hex.get(..16).unwrap_or(&hex).to_owned()
}

impl Lowerer<'_, '_> {
    /// A number the program decides. When compiling to run here it's read through `symbol`;
    /// in an executable, it's the number itself.
    pub(super) fn program_number(&mut self, symbol: String, value: i64) -> Value {
        if self.slots_base.is_some() {
            return self.int(value);
        }
        // Declaring a symbol only fails on a clash, which names by content rule out; the
        // number itself is still right, only not reusable.
        let Ok(id) = data(&mut *self.module, &mut *self.shared, symbol, value) else {
            return self.int(value);
        };
        let module = &mut *self.module;
        let func = &mut *self.b.func;
        let global = *self.globals.entry(id).or_insert_with(|| module.declare_data_in_func(id, func));
        let pointer = self.module.target_config().pointer_type();
        load_from(&mut self.b, pointer, global)
    }

    pub(super) fn name_id(&mut self, name: &str) -> Value {
        let id = intern(&mut self.shared.names, &mut self.shared.tables.names, name);
        self.program_number(format!("fennec.name.{}", digest(name)), id)
    }

    pub(super) fn string_id(&mut self, text: &str) -> Value {
        let id = intern(&mut self.shared.strings, &mut self.shared.tables.strings, text);
        self.program_number(format!("fennec.string.{}", digest(text)), id)
    }

    /// Where `span` is, for a failure to point at. Positions are written relative to the
    /// start of the function they're in, which is read through a symbol, so a function that
    /// moves within its file keeps the same machine code, and so does code it's inlined into.
    /// A position in an inlined standard library function is reported at the call to it.
    pub(super) fn span_args(&mut self, span: Span) -> [Value; 2] {
        let at = self.site(span);
        let [start, end] = [at.start, at.end].map(i64::from);
        let Some(own) = self.origin else { return [self.int(start), self.int(end)] };
        let (origin, from) = match self.inlined_origin {
            Some(inlined) if !span.in_prelude() => inlined,
            _ => (own, i64::from(self.function.def.span.start)),
        };
        [start, end].map(|at| self.b.ins().iadd_imm_s(origin, at - from))
    }

    /// The number of something this function hands the runtime, like an expression for the
    /// interpreter, read through a symbol named by the function and how many it handed over
    /// before. Inlined code counts as the function it's inlined into.
    /// Starts inlining the program function `key`, defined by `def`: positions in it are
    /// relative to its own start from here on. Gives back what to restore afterwards.
    pub(super) fn enter_origin(&mut self, key: &str, def: &FnDef) -> Option<(Value, i64)> {
        let outer = self.inlined_origin;
        if self.origin.is_some() && !def.span.in_prelude() {
            let start = i64::from(def.span.start);
            let value = self.program_number(format!("fennec.origin.{}", digest(key)), start);
            self.inlined_origin = Some((value, start));
        }
        outer
    }

    pub(super) fn handed_number(&mut self, kind: &str, value: i64) -> Value {
        let symbol = format!("fennec.{kind}.{}.{}", self.digest, self.handed);
        self.handed += 1;
        self.program_number(symbol, value)
    }
}

/// A string's number in `table`, giving it the next one the first time.
fn intern(map: &mut HashMap<String, i64>, table: &mut Vec<String>, text: &str) -> i64 {
    if let Some(id) = map.get(text) {
        return *id;
    }
    let id = i64::try_from(table.len()).unwrap_or(0);
    table.push(text.to_owned());
    map.insert(text.to_owned(), id);
    id
}
