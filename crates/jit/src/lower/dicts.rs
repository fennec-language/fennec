//! Dicts natively. A dict is the address of a block the runtime owns (see the runtime's
//! `dicts`): a hash table whose slots point at entries kept in the order their keys were
//! added. Native code hashes keys, looks them up, changes values and adds keys itself while
//! the dict has one owner and room. Copying a shared dict and growing a full one go to the
//! runtime, as does anything else, through the interpreter's dicts.

use cranelift_codegen::ir::condcodes::IntCC;
use cranelift_codegen::ir::{Block, InstBuilder, MemFlagsData, Value, types};
use cranelift_frontend::Variable;
use fennec_interp::embed::dicts::{DICT_HEADER_WORDS, ENTRY_WORDS, shape};
use fennec_syntax::{Arg, BinOp, Diagnostic, Expr, ExprKind, Span, Stmt};

use super::expr::Lowered;
use super::{Lowerer, internal};
use crate::helpers::Helper;
use crate::types::{Ty, Word};

/// Where a dict's count of keys, entries used, slots less one, and slots are.
const KEYS: i32 = 16;
const USED: i32 = 24;
const MASK: i32 = 32;
const SLOTS: i64 = 40;

/// Where a string's length in bytes and its text are.
const TEXT_BYTES: i32 = 16;
const TEXT: i32 = 40;

const MIX: i64 = 0x9E37_79B9_7F4A_7C15_u64.cast_signed();

/// How a dict's keys or values are held natively.
pub(super) fn word_ty(word: Word) -> Ty {
    match word {
        Word::Int => Ty::Int,
        Word::Float => Ty::Float,
        Word::Bool => Ty::Bool,
        Word::Str => Ty::Str,
    }
}

/// What a `for` loop over a dict gives each time round.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Walk {
    Keys,
    Values,
    Items,
}

/// What a lookup that doesn't find its key does: fail as reading `dict[key]` does, or as
/// `dict[key] += value` does.
#[derive(Clone, Copy)]
pub(super) enum Missing {
    Read,
    Augmented,
}

impl Lowerer<'_, '_> {
    /// The variable and kinds of a dict a name owns, for `name[key] = value`.
    pub(super) fn dict_target(&self, object: &Expr) -> Option<(Variable, (Word, Word))> {
        let ExprKind::Name(name) = &object.kind else { return None };
        let local = self.local(name)?;
        match (local.ty, local.var) {
            // A `mut` parameter is handed over for the call, so it can change in place.
            (Ty::Dict(key, value), Some(var)) if local.is_mut => Some((var, (key, value))),
            _ => None,
        }
    }

    /// `name[key] = value`, or `name[key] += value` and the like, for a dict a name owns. The
    /// key is worked out first, then the value, as the interpreter does.
    pub(super) fn assign_dict(
        &mut self,
        (var, key_word, value_word): (Variable, Word, Word),
        target: &Expr,
        index: &Expr,
        op: Option<BinOp>,
        value: &Expr,
    ) -> Result<(), Diagnostic> {
        let (key, key_ty) = self.expr(index)?;
        let key = self.convert(key, key_ty, word_ty(key_word), index.span)?;
        let new = self.expr(value)?;
        let (new, new_ty) = match op {
            None => new,
            Some(op) => {
                let dict = self.b.use_var(var);
                let current =
                    self.dict_get((dict, key_word, value_word), key, Missing::Augmented, target.span)?;
                self.operate(op, current, new, target.span.to(value.span))?
            }
        };
        let new = self.convert(new, new_ty, word_ty(value_word), value.span)?;
        let key = self.own(key, word_ty(key_word));
        let new = self.own(new, word_ty(value_word));
        self.dict_set(var, (key_word, value_word), key, new)
    }

    /// `dict[key]`, natively.
    pub(super) fn read_dict(
        &mut self,
        dict: Value,
        (key_word, value_word): (Word, Word),
        index: &Expr,
        span: Span,
    ) -> Result<Lowered, Diagnostic> {
        let (key, key_ty) = self.expr(index)?;
        let key = self.convert(key, key_ty, word_ty(key_word), index.span)?;
        let got = self.dict_get((dict, key_word, value_word), key, Missing::Read, span)?;
        self.consume(Some(key), word_ty(key_word))?;
        self.consume(Some(dict), Ty::Dict(key_word, value_word))?;
        Ok(got)
    }

    /// `key in dict` or `key not in dict`, natively, or `None` when it isn't one of those.
    pub(super) fn dict_binary(
        &mut self,
        op: BinOp,
        (key, key_ty): Lowered,
        (dict, dict_ty): Lowered,
        span: Span,
    ) -> Result<Option<Lowered>, Diagnostic> {
        let (BinOp::In | BinOp::NotIn, Some(dict), Ty::Dict(key_word, _)) = (op, dict, dict_ty) else {
            return Ok(None);
        };
        let key = self.convert(key, key_ty, word_ty(key_word), span)?;
        let found = self.dict_contains(dict, key_word, key)?;
        let found = if op == BinOp::NotIn { self.b.ins().bxor_imm_u(found, 1) } else { found };
        self.consume(Some(key), word_ty(key_word))?;
        self.consume(Some(dict), dict_ty)?;
        Ok(Some((Some(found), Ty::Bool)))
    }

    /// `dict.get(key, default)` natively, or `None` for another method, which the runtime
    /// then runs.
    pub(super) fn call_dict_method(
        &mut self,
        object: &Expr,
        (key_word, value_word): (Word, Word),
        name: &str,
        args: &[Arg],
    ) -> Result<Option<Lowered>, Diagnostic> {
        let ("get", [key, default]) = (name, args) else { return Ok(None) };
        if key.name.is_some() || default.name.is_some() || key.is_mut || default.is_mut {
            return Ok(None);
        }
        let (dict, dict_ty) = self.expr(object)?;
        let dict = dict.ok_or_else(|| internal("a missing dict", object.span))?;
        let (key_value, key_ty) = self.expr(&key.value)?;
        let key_value = self.convert(key_value, key_ty, word_ty(key_word), key.value.span)?;
        let (fallback, fallback_ty) = self.expr(&default.value)?;
        let fallback = self.convert(fallback, fallback_ty, word_ty(value_word), default.value.span)?;
        let fallback = self.own(fallback, word_ty(value_word));
        let got = self.dict_get_or((dict, key_word, value_word), key_value, fallback)?;
        self.consume(Some(key_value), word_ty(key_word))?;
        self.consume(Some(dict), dict_ty)?;
        Ok(Some((Some(got), word_ty(value_word))))
    }

    /// The dict a `for` loop goes over natively, and what it gives each time round: `dict`
    /// or `dict.keys()` its keys, `dict.values()` its values, `dict.items()` both.
    pub(super) fn dict_walk<'e>(&self, iter: &'e Expr, names: &[String]) -> Option<(&'e Expr, Walk)> {
        let (object, walk) = match &iter.kind {
            ExprKind::Call { callee, args } if args.is_empty() => match &callee.kind {
                ExprKind::Attr { object, name, optional: false } => match name.as_str() {
                    "keys" => (&**object, Walk::Keys),
                    "values" => (&**object, Walk::Values),
                    "items" => (&**object, Walk::Items),
                    _ => return None,
                },
                _ => return None,
            },
            _ => (iter, Walk::Keys),
        };
        let fits = names.len() == if walk == Walk::Items { 2 } else { 1 };
        (fits && matches!(self.static_ty(object), Some(Ty::Dict(..)))).then_some((object, walk))
    }

    /// `for name in dict` and the like, walking its entries in order. The loop keeps an
    /// owner of the dict, so changing it inside the loop copies it, and the loop goes over
    /// the dict as it was when the loop started, as the interpreter's does.
    pub(super) fn for_dict(
        &mut self,
        names: &[String],
        (dict, key_word, value_word): (Value, Word, Word),
        walk: Walk,
        span: Span,
        body: &[Stmt],
    ) -> Result<(), Diagnostic> {
        let flags = MemFlagsData::trusted();
        let dict = self.own(dict, Ty::Dict(key_word, value_word));
        self.drop_temps()?;
        self.open_scope();
        self.bind_native(&format!("\0for{}", span.start), Ty::Dict(key_word, value_word), Some(dict), false);
        let used = self.b.ins().load(types::I64, flags, dict, USED);
        let mask = self.b.ins().load(types::I64, flags, dict, MASK);
        let size = self.b.ins().iadd_imm_s(mask, 1);
        let size = self.b.ins().ishl_imm_s(size, 3);
        let entries = self.b.ins().iadd_imm_s(size, SLOTS);
        let entries = self.b.ins().iadd(dict, entries);
        let at = self.b.declare_var(types::I64);
        let zero = self.int(0);
        self.b.def_var(at, zero);
        let [live, inside, advance, exit] = [(); 4].map(|()| self.b.create_block());
        // The test comes first, then again at the bottom, so going round takes one jump.
        let more = self.b.ins().icmp(IntCC::UnsignedLessThan, zero, used);
        self.b.ins().brif(more, live, &[], exit, &[]);
        self.b.switch_to_block(live);
        let here = self.b.use_var(at);
        let offset = self.b.ins().imul_imm_s(here, i64::try_from(ENTRY_WORDS * 8).unwrap_or(0));
        let entry = self.b.ins().iadd(entries, offset);
        let hash = self.b.ins().load(types::I64, flags, entry, 0);
        let removed = self.b.ins().icmp_imm_s(IntCC::Equal, hash, -1);
        self.b.ins().brif(removed, advance, &[], inside, &[]);
        self.b.switch_to_block(inside);
        let depth = self.scopes.len();
        self.open_scope();
        let wanted: &[(i32, Word)] = match walk {
            Walk::Keys => &[(8, key_word)],
            Walk::Values => &[(16, value_word)],
            Walk::Items => &[(8, key_word), (16, value_word)],
        };
        for (name, (offset, word)) in names.iter().zip(wanted) {
            let bits = self.b.ins().load(types::I64, flags, entry, *offset);
            if *word == Word::Str {
                self.retain(bits);
            }
            let value = self.loaded(bits, *word);
            self.bind_native(name, word_ty(*word), Some(value), false);
        }
        self.loops.push(super::Loop { next: advance, done: exit, depth, put_back: None });
        let lowered = self.statements(body);
        self.loops.pop();
        lowered?;
        self.close_scope()?;
        if !self.is_dead() {
            self.b.ins().jump(advance, &[]);
        }
        self.b.switch_to_block(advance);
        let here = self.b.use_var(at);
        let after = self.b.ins().iadd_imm_s(here, 1);
        self.b.def_var(at, after);
        let more = self.b.ins().icmp(IntCC::UnsignedLessThan, after, used);
        self.b.ins().brif(more, live, &[], exit, &[]);
        self.b.switch_to_block(exit);
        self.close_scope()
    }

    /// A dict literal made natively, when the name it's for holds a native dict, or `None`.
    /// Each key is worked out, then its value, in order, as the interpreter does.
    pub(super) fn literal_for(&mut self, name: &str, value: &Expr) -> Result<Option<Lowered>, Diagnostic> {
        let (ExprKind::Dict(pairs), Some(ty @ Ty::Dict(key_word, value_word))) =
            (&value.kind, self.checked_ty(name))
        else {
            return Ok(None);
        };
        let shape = shape(self.shared.tables.str_shape, key_word, value_word)
            .ok_or_else(|| internal("a dict's shape", value.span))?;
        let shape = self.handed_number("shape", shape.cast_signed());
        let room = self.int(i64::try_from(pairs.len()).unwrap_or(0));
        let dict = self.help(Helper::DictNew, &[shape, room])?;
        let var = self.b.declare_var(types::I64);
        self.b.def_var(var, dict);
        for (key, item) in pairs {
            let (key_value, key_ty) = self.expr(key)?;
            let key_value = self.convert(key_value, key_ty, word_ty(key_word), key.span)?;
            let (item_value, item_ty) = self.expr(item)?;
            let item_value = self.convert(item_value, item_ty, word_ty(value_word), item.span)?;
            let key_value = self.own(key_value, word_ty(key_word));
            let item_value = self.own(item_value, word_ty(value_word));
            self.dict_set(var, (key_word, value_word), key_value, item_value)?;
        }
        Ok(Some((Some(self.b.use_var(var)), ty)))
    }

    /// A runtime object as an owned native dict, failing like the interpreter when it isn't
    /// one.
    pub(super) fn handle_to_dict(
        &mut self,
        handle: Value,
        key: Word,
        value: Word,
        span: Span,
    ) -> Result<Value, Diagnostic> {
        let shape = shape(self.shared.tables.str_shape, key, value)
            .ok_or_else(|| internal("a dict's shape", span))?;
        let shape = self.handed_number("shape", shape.cast_signed());
        let [start, end] = self.span_args(span);
        self.help(Helper::BoxFromValue, &[handle, shape, start, end])
    }

    /// A key's hash, worked out as the runtime does.
    fn dict_hash(&mut self, key: Word, value: Value) -> Result<Value, Diagnostic> {
        let mixed =
            if key == Word::Str { self.text_hash(value)? } else { self.b.ins().imul_imm_s(value, MIX) };
        let high = self.b.ins().ushr_imm_s(mixed, 29);
        let folded = self.b.ins().bxor(mixed, high);
        Ok(self.b.ins().ushr_imm_s(folded, 1))
    }

    /// A string's hash before it's finished: its length, then each word of its text mixed in.
    fn text_hash(&mut self, text: Value) -> Result<Value, Diagnostic> {
        let flags = MemFlagsData::trusted();
        let length = self.b.ins().load(types::I64, flags, text, TEXT_BYTES);
        let start = self.b.ins().imul_imm_s(length, MIX);
        let words = self.b.ins().iadd_imm_s(length, 7);
        let words = self.b.ins().ushr_imm_s(words, 3);
        let [head, body, done] = [(); 3].map(|()| self.b.create_block());
        for param in [types::I64, types::I64] {
            self.b.append_block_param(head, param);
        }
        self.b.append_block_param(done, types::I64);
        let zero = self.int(0);
        self.b.ins().jump(head, &[start.into(), zero.into()]);
        self.b.switch_to_block(head);
        let [hash, at] = self.params(head)?;
        let more = self.b.ins().icmp(IntCC::UnsignedLessThan, at, words);
        self.b.ins().brif(more, body, &[], done, &[hash.into()]);
        self.b.switch_to_block(body);
        let offset = self.b.ins().ishl_imm_s(at, 3);
        let place = self.b.ins().iadd(text, offset);
        let word = self.b.ins().load(types::I64, flags, place, TEXT);
        let turned = self.b.ins().rotl_imm_s(hash, 23);
        let mixed = self.b.ins().bxor(turned, word);
        let mixed = self.b.ins().imul_imm_s(mixed, MIX);
        let next = self.b.ins().iadd_imm_s(at, 1);
        self.b.ins().jump(head, &[mixed.into(), next.into()]);
        self.b.switch_to_block(done);
        self.param(done)
    }

    /// Looks a key up, going on to `found` with the address of its entry, or to `missing`
    /// with the address of the empty slot where it would go.
    fn probe(
        &mut self,
        dict: Value,
        key_word: Word,
        key: Value,
        hash: Value,
        [found, missing]: [Block; 2],
    ) -> Result<(), Diagnostic> {
        let flags = MemFlagsData::trusted();
        let mask = self.b.ins().load(types::I64, flags, dict, MASK);
        let slots = self.b.ins().iadd_imm_s(dict, SLOTS);
        let size = self.b.ins().iadd_imm_s(mask, 1);
        let size = self.b.ins().ishl_imm_s(size, 3);
        let entries = self.b.ins().iadd(slots, size);
        let first = self.b.ins().band(hash, mask);
        let [head, check, compare, next] = [(); 4].map(|()| self.b.create_block());
        self.b.append_block_param(head, types::I64);
        self.b.ins().jump(head, &[first.into()]);
        self.b.switch_to_block(head);
        let at = self.param(head)?;
        let offset = self.b.ins().ishl_imm_s(at, 3);
        let slot = self.b.ins().iadd(slots, offset);
        let taken = self.b.ins().load(types::I64, flags, slot, 0);
        self.b.ins().brif(taken, check, &[], missing, &[slot.into()]);
        self.b.switch_to_block(check);
        let entry = self.b.ins().iadd_imm_s(taken, -1);
        let entry = self.b.ins().imul_imm_s(entry, i64::try_from(ENTRY_WORDS * 8).unwrap_or(0));
        let entry = self.b.ins().iadd(entries, entry);
        let theirs = self.b.ins().load(types::I64, flags, entry, 0);
        let same_hash = self.b.ins().icmp(IntCC::Equal, theirs, hash);
        self.b.ins().brif(same_hash, compare, &[], next, &[]);
        self.b.switch_to_block(compare);
        let other = self.b.ins().load(types::I64, flags, entry, 8);
        let same_key = self.b.ins().icmp(IntCC::Equal, other, key);
        if key_word == Word::Str {
            // Equal strings in different blocks: compare their text.
            let text = self.b.create_block();
            self.b.ins().brif(same_key, found, &[entry.into()], text, &[]);
            self.b.switch_to_block(text);
            let equal = self.help(Helper::BoxEqual, &[other, key])?;
            self.b.ins().brif(equal, found, &[entry.into()], next, &[]);
        } else {
            self.b.ins().brif(same_key, found, &[entry.into()], next, &[]);
        }
        self.b.switch_to_block(next);
        let after = self.b.ins().iadd_imm_s(at, 1);
        let after = self.b.ins().band(after, mask);
        self.b.ins().jump(head, &[after.into()]);
        Ok(())
    }

    /// Blocks a probe goes on to, each with an address.
    fn probe_blocks(&mut self) -> [Block; 2] {
        [(); 2].map(|()| {
            let block = self.b.create_block();
            self.b.append_block_param(block, types::I64);
            block
        })
    }

    fn param(&self, block: Block) -> Result<Value, Diagnostic> {
        self.b
            .block_params(block)
            .first()
            .copied()
            .ok_or_else(|| internal("lost a block's value", Span::default()))
    }

    fn params(&self, block: Block) -> Result<[Value; 2], Diagnostic> {
        match self.b.block_params(block) {
            [first, second] => Ok([*first, *second]),
            _ => Err(internal("lost a block's values", Span::default())),
        }
    }

    /// `dict[key]`: an owned copy of its value, failing like the interpreter when the key
    /// isn't there. The dict and the key are only borrowed.
    pub(super) fn dict_get(
        &mut self,
        (dict, key_word, value_word): (Value, Word, Word),
        key: Value,
        missing: Missing,
        span: Span,
    ) -> Result<Lowered, Diagnostic> {
        let hash = self.dict_hash(key_word, key)?;
        let [found, absent] = self.probe_blocks();
        self.b.set_cold_block(absent);
        let done = self.b.create_block();
        self.b.append_block_param(done, types::I64);
        self.probe(dict, key_word, key, hash, [found, absent])?;
        self.b.switch_to_block(found);
        let entry = self.param(found)?;
        let value = self.b.ins().load(types::I64, MemFlagsData::trusted(), entry, 16);
        if value_word == Word::Str {
            self.retain(value);
        }
        self.b.ins().jump(done, &[value.into()]);
        self.b.switch_to_block(absent);
        let augmented = self.b.ins().iconst(types::I8, i64::from(matches!(missing, Missing::Augmented)));
        let [start, end] = self.span_args(span);
        self.call_helper(Helper::DictMissing, &[dict, key, augmented, start, end])?;
        let zero = self.int(0);
        self.b.ins().jump(done, &[zero.into()]);
        self.b.switch_to_block(done);
        let bits = self.param(done)?;
        Ok((Some(self.loaded(bits, value_word)), word_ty(value_word)))
    }

    /// `dict.get(key, default)`: an owned copy of the key's value, or the default. The dict
    /// and the key are only borrowed; the default is taken over.
    pub(super) fn dict_get_or(
        &mut self,
        (dict, key_word, value_word): (Value, Word, Word),
        key: Value,
        default: Value,
    ) -> Result<Value, Diagnostic> {
        let default = self.stored(default, value_word);
        let hash = self.dict_hash(key_word, key)?;
        let [found, absent] = self.probe_blocks();
        let done = self.b.create_block();
        self.b.append_block_param(done, types::I64);
        self.probe(dict, key_word, key, hash, [found, absent])?;
        self.b.switch_to_block(found);
        let entry = self.param(found)?;
        let value = self.b.ins().load(types::I64, MemFlagsData::trusted(), entry, 16);
        if value_word == Word::Str {
            self.retain(value);
            self.release(default)?;
        }
        self.b.ins().jump(done, &[value.into()]);
        self.b.switch_to_block(absent);
        self.b.ins().jump(done, &[default.into()]);
        self.b.switch_to_block(done);
        let bits = self.param(done)?;
        Ok(self.loaded(bits, value_word))
    }

    /// `key in dict`. The dict and the key are only borrowed.
    pub(super) fn dict_contains(
        &mut self,
        dict: Value,
        key_word: Word,
        key: Value,
    ) -> Result<Value, Diagnostic> {
        let hash = self.dict_hash(key_word, key)?;
        let [found, absent] = self.probe_blocks();
        let done = self.b.create_block();
        self.b.append_block_param(done, types::I8);
        self.probe(dict, key_word, key, hash, [found, absent])?;
        self.b.switch_to_block(found);
        let yes = self.b.ins().iconst(types::I8, 1);
        self.b.ins().jump(done, &[yes.into()]);
        self.b.switch_to_block(absent);
        let no = self.b.ins().iconst(types::I8, 0);
        self.b.ins().jump(done, &[no.into()]);
        self.b.switch_to_block(done);
        self.param(done)
    }

    /// `dict.len()`.
    pub(super) fn dict_length(&mut self, dict: Value) -> Value {
        self.load_length(dict, KEYS)
    }

    /// `name[key] = value` for a dict a name owns: changes or adds the key in place while
    /// the dict has one owner and room, and otherwise has the runtime copy or grow it. The
    /// key and value are taken over.
    pub(super) fn dict_set(
        &mut self,
        var: Variable,
        (key_word, value_word): (Word, Word),
        key: Value,
        value: Value,
    ) -> Result<(), Diagnostic> {
        let flags = MemFlagsData::trusted();
        let value = self.stored(value, value_word);
        let dict = self.b.use_var(var);
        let hash = self.dict_hash(key_word, key)?;
        let [found, absent] = self.probe_blocks();
        let [lookup, add, slow, done] = [(); 4].map(|()| self.b.create_block());
        self.b.set_cold_block(slow);
        self.b.append_block_param(done, types::I64);
        let count = self.b.ins().load(types::I64, flags, dict, 0);
        let alone = self.b.ins().icmp_imm_s(IntCC::Equal, count, 1);
        self.b.ins().brif(alone, lookup, &[], slow, &[]);
        self.b.switch_to_block(lookup);
        self.probe(dict, key_word, key, hash, [found, absent])?;
        self.b.switch_to_block(found);
        let entry = self.param(found)?;
        let old = self.b.ins().load(types::I64, flags, entry, 16);
        self.b.ins().store(flags, value, entry, 16);
        if value_word == Word::Str {
            self.release(old)?;
        }
        if key_word == Word::Str {
            self.release(key)?;
        }
        self.b.ins().jump(done, &[dict.into()]);
        self.b.switch_to_block(absent);
        let empty = self.param(absent)?;
        let used = self.b.ins().load(types::I64, flags, dict, USED);
        let mask = self.b.ins().load(types::I64, flags, dict, MASK);
        let slots = self.b.ins().iadd_imm_s(mask, 1);
        let quarter = self.b.ins().ushr_imm_s(slots, 2);
        let room = self.b.ins().isub(slots, quarter);
        let fits = self.b.ins().icmp(IntCC::UnsignedLessThan, used, room);
        self.b.ins().brif(fits, add, &[], slow, &[]);
        self.b.switch_to_block(add);
        let number = self.b.ins().iadd_imm_s(used, 1);
        self.b.ins().store(flags, number, empty, 0);
        let header = i64::try_from(DICT_HEADER_WORDS * 8).unwrap_or(0);
        let size = self.b.ins().ishl_imm_s(slots, 3);
        let entries = self.b.ins().iadd_imm_s(size, header);
        let entries = self.b.ins().iadd(dict, entries);
        let offset = self.b.ins().imul_imm_s(used, i64::try_from(ENTRY_WORDS * 8).unwrap_or(0));
        let entry = self.b.ins().iadd(entries, offset);
        for (at, word) in [hash, key, value].into_iter().enumerate() {
            self.b.ins().store(flags, word, entry, i32::try_from(at * 8).unwrap_or(0));
        }
        self.b.ins().store(flags, number, dict, USED);
        let keys = self.b.ins().load(types::I64, flags, dict, KEYS);
        let keys = self.b.ins().iadd_imm_s(keys, 1);
        self.b.ins().store(flags, keys, dict, KEYS);
        self.b.ins().jump(done, &[dict.into()]);
        self.b.switch_to_block(slow);
        let changed = self.help(Helper::DictSet, &[dict, key, value])?;
        self.b.ins().jump(done, &[changed.into()]);
        self.b.switch_to_block(done);
        let changed = self.param(done)?;
        self.b.def_var(var, changed);
        Ok(())
    }

    /// A key or value as the word a dict keeps.
    fn stored(&mut self, value: Value, word: Word) -> Value {
        match word {
            Word::Float => self.b.ins().bitcast(types::I64, MemFlagsData::new(), value),
            Word::Bool => self.b.ins().uextend(types::I64, value),
            Word::Int | Word::Str => value,
        }
    }

    /// A word a dict keeps as the value it is.
    fn loaded(&mut self, bits: Value, word: Word) -> Value {
        match word {
            Word::Float => self.b.ins().bitcast(types::F64, MemFlagsData::new(), bits),
            Word::Bool => self.b.ins().ireduce(types::I8, bits),
            Word::Int | Word::Str => bits,
        }
    }
}
