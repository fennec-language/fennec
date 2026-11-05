//! Where a fixed-size array, `array[T, N]`, is kept. One that fits in [`IN_PLACE_BYTES`] is
//! kept where it is, like a struct's fields: on the stack, in a struct, or in a list's items, with
//! no block and no count. A bigger one would risk overflowing the stack, so it's kept as a list
//! of its N items instead, which nothing else in the program can tell apart: its length still
//! never changes, and copying it still gives a separate value.

use std::collections::{HashMap, HashSet};

use fennec_check::Type;

use crate::Program;

/// The most an array kept in place may take, in bytes. Its items are counted as a native backend
/// holds them, a word for a number and three for a string, so this is close but not exact.
pub const IN_PLACE_BYTES: u64 = 64 * 1024;

/// Turns each array too big to keep in place into a list of its items, everywhere the program
/// mentions its type. An array inside it is decided first, so an array of big arrays is a list
/// of lists, while an array of small ones may still fit.
pub fn spill_big_arrays(program: &mut Program) {
    let sizes = Sizes::of(program);
    let mut change = |ty: &mut Type| *ty = sizes.placed(ty);
    for def in program.structs.iter_mut().chain(&mut program.errors) {
        def.fields.iter_mut().for_each(|(_, ty)| change(ty));
    }
    for case in program.enums.iter_mut().flat_map(|def| &mut def.cases) {
        case.fields.iter_mut().for_each(|(_, ty)| change(ty));
    }
    for def in &mut program.generators {
        change(&mut def.item);
        def.slots.iter_mut().for_each(&mut change);
    }
    for function in program.functions.iter_mut().chain(&mut program.constants) {
        change(&mut function.returns);
        function.locals.iter_mut().for_each(|local| change(&mut local.ty));
    }
    for text in &mut program.own_texts {
        change(&mut text.ty);
    }
}

/// How big the program's own types are, as written before any array is turned into a list.
struct Sizes {
    structs: HashMap<String, Vec<Type>>,
    enums: HashMap<String, Vec<Vec<Type>>>,
    /// `shared` types, held by a block's address.
    cells: HashSet<String>,
}

impl Sizes {
    fn of(program: &Program) -> Sizes {
        let fields = |fields: &[(String, Type)]| fields.iter().map(|(_, ty)| ty.clone()).collect();
        let structs = program.structs.iter().chain(&program.errors);
        Sizes {
            structs: structs.clone().map(|def| (def.name.clone(), fields(&def.fields))).collect(),
            enums: program
                .enums
                .iter()
                .map(|def| (def.name.clone(), def.cases.iter().map(|case| fields(&case.fields)).collect()))
                .collect(),
            cells: structs.filter(|def| def.shared).map(|def| def.name.clone()).collect(),
        }
    }

    /// The type with each array too big to keep in place turned into a list, innermost first.
    fn placed(&self, ty: &Type) -> Type {
        let inner = |ty: &Type| Box::new(self.placed(ty));
        match ty {
            Type::Array(item, size) => {
                let item = inner(item);
                if self.bytes(&item, &mut Vec::new()).saturating_mul(*size) > IN_PLACE_BYTES {
                    Type::List(item)
                } else {
                    Type::Array(item, *size)
                }
            }
            Type::List(item) => Type::List(inner(item)),
            Type::Buffer(item) => Type::Buffer(inner(item)),
            Type::Set(item) => Type::Set(inner(item)),
            Type::Iterator(item) => Type::Iterator(inner(item)),
            Type::Optional(item) => Type::Optional(inner(item)),
            Type::Id(item) => Type::Id(inner(item)),
            Type::Table(item) => Type::Table(inner(item)),
            Type::Dict(key, value) => Type::Dict(inner(key), inner(value)),
            Type::Result(value, error) => Type::Result(inner(value), inner(error)),
            Type::Tuple(items) => Type::Tuple(items.iter().map(|item| self.placed(item)).collect()),
            Type::Generic(name, items) => {
                Type::Generic(name.clone(), items.iter().map(|item| self.placed(item)).collect())
            }
            Type::Arrow(params, returns) => {
                Type::Arrow(params.iter().map(|param| self.placed(param)).collect(), inner(returns))
            }
            other => other.clone(),
        }
    }

    /// About how many bytes a value of this type takes where it's kept in place. `within` are
    /// the named types being measured already: one inside itself is held by a block's address.
    fn bytes(&self, ty: &Type, within: &mut Vec<String>) -> u64 {
        const WORD: u64 = 8;
        match ty {
            Type::Unit | Type::None => 0,
            Type::Str => 3 * WORD,
            Type::Array(item, size) => self.bytes(item, within).saturating_mul(*size),
            Type::Tuple(items) => items.iter().map(|item| self.bytes(item, within)).sum(),
            Type::Optional(item) => WORD + self.bytes(item, within),
            Type::Result(value, error) => WORD + self.bytes(value, within).max(self.bytes(error, within)),
            Type::Named(name) if !within.contains(name) && !self.cells.contains(name) => {
                within.push(name.clone());
                let size = if let Some(fields) = self.structs.get(name) {
                    fields.iter().map(|field| self.bytes(field, within)).sum()
                } else if let Some(cases) = self.enums.get(name) {
                    let largest = cases.iter().map(|fields| fields.iter().map(|field| self.bytes(field, within)).sum());
                    WORD + largest.max().unwrap_or(0)
                } else {
                    WORD
                };
                within.pop();
                size
            }
            _ => WORD,
        }
    }
}

#[cfg(test)]
mod tests {
    use fennec_check::Type;

    use super::{IN_PLACE_BYTES, Sizes};

    fn array(item: Type, size: u64) -> Type {
        Type::Array(Box::new(item), size)
    }

    #[test]
    fn an_array_over_the_limit_is_kept_as_a_list() {
        let sizes = Sizes { structs: Default::default(), enums: Default::default(), cells: Default::default() };
        let fits = IN_PLACE_BYTES / 8;
        assert_eq!(sizes.placed(&array(Type::Int, fits)), array(Type::Int, fits));
        assert_eq!(sizes.placed(&array(Type::Int, fits + 1)), Type::List(Box::new(Type::Int)));
        // Each inner array is a list's address, so the outer one fits.
        let inner = array(Type::Float, fits + 1);
        assert_eq!(sizes.placed(&array(inner, 4)), array(Type::List(Box::new(Type::Float)), 4));
    }

    #[test]
    fn a_struct_counts_its_fields() {
        let mut sizes =
            Sizes { structs: Default::default(), enums: Default::default(), cells: Default::default() };
        sizes.structs.insert("Pair".to_owned(), vec![Type::Int, Type::Str]);
        let pair = Type::Named("Pair".to_owned());
        // 32 bytes each: 2048 fit, 2049 don't.
        assert_eq!(sizes.placed(&array(pair.clone(), 2048)), array(pair.clone(), 2048));
        assert_eq!(sizes.placed(&array(pair.clone(), 2049)), Type::List(Box::new(pair)));
    }
}
