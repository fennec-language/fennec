//! The hashes a build uses a unit again by (edit builds, stage 1): of a function's IR, with its
//! places from where it starts; of what each read a unit made gives; and of the settings every
//! unit is written with. Each is the same whenever what it hashes is, in any process: sets and
//! maps are hashed in order, and a float by its bits.

use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};

use fennec_ir::{Block, Function, Inst, Terminator};
use fennec_syntax::Span;

use crate::share::{Place, anchor, place};

/// A key: a hash of everything something is made from.
pub(crate) type Key = [u8; 32];

/// A `Hasher` that's blake3, the same in every process and on every run.
pub(crate) struct Stable(fennec_blake3::Hasher);

impl Stable {
    pub(crate) fn new(context: &str) -> Self {
        Stable(fennec_blake3::Hasher::new_derive_key(context))
    }

    pub(crate) fn key(&self) -> Key {
        *self.0.finalize().as_bytes()
    }
}

impl Hasher for Stable {
    fn write(&mut self, bytes: &[u8]) {
        self.0.update(bytes);
    }

    fn finish(&self) -> u64 {
        let key = self.key();
        u64::from_le_bytes(*key.first_chunk::<8>().unwrap_or(&[0; 8]))
    }
}

/// The key of anything that hashes, under `context`.
pub(crate) fn key_of(context: &str, value: &impl Hash) -> Key {
    let mut state = Stable::new(context);
    value.hash(&mut state);
    state.key()
}

/// A set, hashed in its items' order.
pub(crate) fn hash_set<T: Ord + Hash>(set: &HashSet<T>, state: &mut Stable) {
    let mut items: Vec<&T> = set.iter().collect();
    items.sort_unstable();
    items.hash(state);
}

/// A map, hashed in its keys' order.
pub(crate) fn hash_map<K: Ord + Hash, V: Hash>(map: &HashMap<K, V>, state: &mut Stable) {
    let mut entries: Vec<(&K, &V)> = map.iter().collect();
    entries.sort_unstable_by(|first, second| first.0.cmp(second.0));
    entries.hash(state);
}

/// A function's IR, with every place in the program's own files from where the function starts
/// (see `share::anchor`): text added above it leaves the key as it was, and a place outside it
/// that moves apart from it changes the key.
pub(crate) fn ir_key(function: &Function) -> Key {
    let start = anchor(function);
    let relative =
        |span: Span| -> (Place, u32) { (place(span.start, start), span.end.wrapping_sub(span.start)) };
    let mut state = Stable::new("fennec unit ir 2026-11-17");
    let Function { name, params, mut_params, returns, locals, blocks, counts, places } = function;
    (name, params, mut_params, returns, locals, counts, places).hash(&mut state);
    blocks.len().hash(&mut state);
    for Block { insts, end } in blocks {
        insts.len().hash(&mut state);
        for Inst { kind, span } in insts {
            kind.hash(&mut state);
            relative(*span).hash(&mut state);
        }
        std::mem::discriminant(end).hash(&mut state);
        match end {
            Terminator::NoMatch { value, span } => (value, relative(*span)).hash(&mut state),
            Terminator::Stop { message, span } => (message, relative(*span)).hash(&mut state),
            other => other.hash(&mut state),
        }
    }
    state.key()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lowered(source: &str) -> Result<fennec_ir::Program, String> {
        let mut module = fennec_syntax::parse(source).map_err(|err| format!("{err:?}"))?;
        let checked = fennec_check::check_and_resolve(&mut module).map_err(|err| format!("{err:?}"))?;
        Ok(fennec_ir::lower(&module, &checked))
    }

    fn keys(source: &str) -> Result<HashMap<String, Key>, String> {
        let program = lowered(source)?;
        Ok(program.functions.iter().map(|function| (function.name.clone(), ir_key(function))).collect())
    }

    /// A function's key stays as it was when lines are added above it, and changes with its body,
    /// and with a float only its bits tell apart.
    #[test]
    fn a_functions_key_moves_with_it_and_changes_with_its_body() -> Result<(), String> {
        let source = "def picked(items: list[int], index: int) -> float:\n    return float(items[index]) * 0.5\n\ndef main(stdout: Stdout):\n    stdout.print(picked([3, 5], 1))\n    return\n";
        let before = keys(source)?;
        let moved = keys(&format!("# A line above.\n\n{source}"))?;
        let edited = keys(&source.replace("0.5", "0.25"))?;
        assert_eq!(before.get("picked"), moved.get("picked"));
        assert_ne!(before.get("picked"), edited.get("picked"));
        assert_eq!(before.get("main"), edited.get("main"));
        let floats = |value: f64| key_of("test", &fennec_ir::Operand::Float(value));
        assert_ne!(floats(f64::NAN), floats(-f64::NAN));
        assert_ne!(floats(0.0), floats(-0.0));
        Ok(())
    }
}
