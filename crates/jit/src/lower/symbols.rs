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

use cranelift_codegen::ir::{InstBuilder, MemFlagsData, Value, types};
use cranelift_frontend::FunctionBuilder;
use cranelift_module::{DataDescription, Linkage, Module};
use fennec_syntax::{Diagnostic, Span};

use super::{Lowerer, Shared, intern, internal};

/// Loads `value` through the data symbol `symbol`, declaring it the first time it's used.
pub(super) fn load(
    b: &mut FunctionBuilder<'_>,
    module: &mut dyn Module,
    shared: &mut Shared,
    symbol: String,
    value: i64,
) -> Result<Value, Diagnostic> {
    let id = if let Some((id, stored)) = shared.symbols.get_mut(&symbol) {
        *stored = value;
        *id
    } else {
        let id = module
            .declare_data(&symbol, Linkage::Local, false, false)
            .map_err(|err| internal(err, Span::default()))?;
        shared.symbols.insert(symbol, (id, value));
        id
    };
    let global = module.declare_data_in_func(id, b.func);
    let pointer = module.target_config().pointer_type();
    let address = b.ins().symbol_value(pointer, global);
    // The value never changes during the run, so the load can move out of loops.
    let flags = MemFlagsData::trusted().with_readonly().with_can_move();
    Ok(b.ins().load(types::I64, flags, address, 0))
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
        match load(&mut self.b, &mut *self.module, &mut *self.shared, symbol, value) {
            Ok(loaded) => loaded,
            // Declaring a symbol only fails on a clash, which names by content rule out;
            // the number itself is still right, only not reusable.
            Err(_) => self.int(value),
        }
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
    /// function's own start, which is read through a symbol, so a function that moves
    /// within its file keeps the same machine code.
    pub(super) fn span_args(&mut self, span: Span) -> [Value; 2] {
        let span = self.site(span);
        let [start, end] = [span.start, span.end].map(i64::from);
        if self.slots_base.is_some() {
            return [self.int(start), self.int(end)];
        }
        let origin = i64::from(self.function.def.span.start);
        let base = self.program_number(format!("fennec.origin.{}", digest(&self.function.key)), origin);
        [start, end].map(|at| self.b.ins().iadd_imm_s(base, at - origin))
    }

    /// The number of something this function hands the runtime, like an expression for the
    /// interpreter, read through a symbol named by the function and how many it handed over
    /// before. Inlined code counts as the function it's inlined into.
    pub(super) fn handed_number(&mut self, kind: &str, value: i64) -> Value {
        let symbol = format!("fennec.{kind}.{}.{}", digest(&self.function.key), self.handed);
        self.handed += 1;
        self.program_number(symbol, value)
    }
}
