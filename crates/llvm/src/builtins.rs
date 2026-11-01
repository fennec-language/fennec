//! Builtins written out here: loops over a list that the runtime has no call for.

use fennec_interp::embed::layout::TEXT;
use fennec_interp::embed::lists::{SORT_FLOATS, SORT_INTS, SORT_STRINGS};
use fennec_ir::{LocalId, Operand};
use fennec_layout::ListItem;
use fennec_syntax::Span;

use crate::emit::{Emit, ITEM_ACCESS};
use crate::kinds::Kind;

/// A loop that's underway: the slot holding its count, and where it goes back to and out to.
pub(crate) struct LoopEnd {
    counter: String,
    head: String,
    done: String,
}

impl Emit<'_, '_, '_> {
    /// A slot holding a count, starting from zero.
    pub(crate) fn counter(&mut self) -> String {
        let at = self.place_for(&Kind::Int);
        self.line(&format!("store i64 0, ptr {at}"));
        at
    }

    /// A count from zero up to `count`. Gives back the count this time round.
    pub(crate) fn loop_begin(&mut self, count: &str) -> (String, LoopEnd) {
        let counter = self.counter();
        let (head, body, done) = (self.label("head"), self.label("body"), self.label("done"));
        self.line(&format!("br label %{head}"));
        self.start_block(&head);
        let (at, more) = (self.temp(), self.temp());
        self.line(&format!("{at} = load i64, ptr {counter}"));
        self.line(&format!("{more} = icmp slt i64 {at}, {count}"));
        self.line(&format!("br i1 {more}, label %{body}, label %{done}"));
        self.start_block(&body);
        (at, LoopEnd { counter, head, done })
    }

    pub(crate) fn loop_end(&mut self, end: LoopEnd) {
        let (at, next) = (self.temp(), self.temp());
        self.line(&format!("{at} = load i64, ptr {}", end.counter));
        self.line(&format!("{next} = add i64 {at}, 1"));
        self.line(&format!("store i64 {next}, ptr {}", end.counter));
        self.line(&format!("br label %{}", end.head));
        self.start_block(&end.done);
    }

    fn list_operand(&mut self, operand: &Operand) -> Result<(String, ListItem, Kind), String> {
        let (list, kind) = self.operand(operand)?;
        let Kind::List(item) = kind else { return Err(format!("it goes through a {kind:?} as if a list")) };
        let held = Kind::from_ty(self.module.layouts, item.ty())
            .ok_or("a list item this backend doesn't take yet")?;
        Ok((list, item, held))
    }

    /// An item, as a number or address, or the place it is if it's kept in memory.
    fn item_value(&mut self, list: &str, held: &Kind, at: &str) -> String {
        let memory = held.memory(self.module.layouts);
        let place = self.item_address(list, &memory, at);
        if held.in_memory() { place } else { self.load_field(held, &place, ITEM_ACCESS) }
    }

    /// Puts a value in a new list's empty place, with an owner of its own.
    pub(crate) fn put_item(&mut self, item: &Kind, kind: &Kind, value: &str, place: &str) {
        if kind.counted() {
            self.retain(value);
        }
        self.store_field(item, value, place, ITEM_ACCESS);
    }

    /// Puts a tuple of these in a new list's empty place.
    pub(crate) fn put_pair(
        &mut self,
        item: &Kind,
        place: &str,
        parts: [(&Kind, &str); 2],
    ) -> Result<(), String> {
        let (Kind::Struct(id) | Kind::Boxed(id)) = item else {
            return Err("a list of tuples of something else".to_owned());
        };
        let block = if matches!(item, Kind::Boxed(_)) { Some(self.new_block(*id)?) } else { None };
        let base = match &block {
            Some(block) => self.offset(block, 16),
            None => place.to_owned(),
        };
        let offsets: Vec<u32> = self
            .module
            .layouts
            .get(*id)
            .map(|layout| layout.fields.iter().map(|field| field.offset).collect())
            .unwrap_or_default();
        for ((kind, value), offset) in parts.into_iter().zip(offsets) {
            let at = self.offset(&base, offset);
            if kind.counted() {
                self.retain(value);
            }
            self.store_field(kind, value, &at, "");
        }
        if let Some(block) = block {
            self.line(&format!("store ptr {block}, ptr {place}{ITEM_ACCESS}"));
        }
        Ok(())
    }

    /// The items of a new list, which is the address they start at.
    pub(crate) fn items_of(&mut self, list: &str) -> String {
        self.offset(list, 32)
    }

    fn sum(&mut self, dest: LocalId, list: &Operand, span: Span) -> Result<(), String> {
        let (list, _, held) = self.list_operand(list)?;
        let length = self.list_length(&list);
        let total = self.place_for(&held);
        let zero = if held == Kind::Float { crate::emit::float_text(0.0) } else { "0".to_owned() };
        let text = self.text_of(&held);
        self.line(&format!("store {text} {zero}, ptr {total}"));
        let (at, end) = self.loop_begin(&length);
        let value = self.item_value(&list, &held, &at);
        let so_far = self.temp();
        self.line(&format!("{so_far} = load {text}, ptr {total}"));
        let next = if held == Kind::Float {
            let next = self.temp();
            self.line(&format!("{next} = fadd double {so_far}, {value}"));
            next
        } else {
            self.checked_add(&so_far, &value, span)
        };
        self.line(&format!("store {text} {next}, ptr {total}"));
        self.loop_end(end);
        let result = self.temp();
        self.line(&format!("{result} = load {text}, ptr {total}"));
        self.assign(dest, &result)
    }

    /// `all` or `any` over a list of bools.
    fn all_or_any(&mut self, dest: LocalId, list: &Operand, wanted_all: bool) -> Result<(), String> {
        let (list, _, held) = self.list_operand(list)?;
        let length = self.list_length(&list);
        let result = self.place_for(&Kind::Bool);
        self.line(&format!("store i1 {wanted_all}, ptr {result}"));
        let (at, end) = self.loop_begin(&length);
        let value = self.item_value(&list, &held, &at);
        let (settled, carry_on) = (self.label("settled"), self.label("on"));
        if wanted_all {
            self.line(&format!("br i1 {value}, label %{carry_on}, label %{settled}"));
        } else {
            self.line(&format!("br i1 {value}, label %{settled}, label %{carry_on}"));
        }
        self.start_block(&settled);
        self.line(&format!("store i1 {}, ptr {result}", !wanted_all));
        self.line(&format!("br label %{carry_on}"));
        self.start_block(&carry_on);
        self.loop_end(end);
        let held_result = self.temp();
        self.line(&format!("{held_result} = load i1, ptr {result}"));
        self.assign(dest, &held_result)
    }

    /// `enumerate(list)`.
    fn enumerate(&mut self, dest: LocalId, list: &Operand) -> Result<(), String> {
        let (list, _, held) = self.list_operand(list)?;
        let Kind::List(item) = self.kind_of(dest)?.clone() else {
            return Err("`enumerate`, not as a list".to_owned());
        };
        let item_kind = Kind::from_ty(self.module.layouts, item.ty())
            .ok_or("a list item this backend doesn't take yet")?;
        let length = self.list_length(&list);
        let result = self.new_list(item, &length);
        let memory = item_kind.memory(self.module.layouts);
        let (at, end) = self.loop_begin(&length);
        let value = self.item_value(&list, &held, &at);
        let place = self.item_address(&result, &memory, &at);
        self.put_pair(&item_kind, &place, [(&Kind::Int, &at), (&held, &value)])?;
        self.loop_end(end);
        self.assign(dest, &result)
    }

    /// `zip(a, b)`, as long as the shorter.
    fn zip(&mut self, dest: LocalId, a: &Operand, b: &Operand) -> Result<(), String> {
        let (a, _, a_held) = self.list_operand(a)?;
        let (b, _, b_held) = self.list_operand(b)?;
        let Kind::List(item) = self.kind_of(dest)?.clone() else {
            return Err("`zip`, not as a list".to_owned());
        };
        let item_kind = Kind::from_ty(self.module.layouts, item.ty())
            .ok_or("a list item this backend doesn't take yet")?;
        let (a_length, b_length) = (self.list_length(&a), self.list_length(&b));
        let (shorter, length) = (self.temp(), self.temp());
        self.line(&format!("{shorter} = icmp slt i64 {a_length}, {b_length}"));
        self.line(&format!("{length} = select i1 {shorter}, i64 {a_length}, i64 {b_length}"));
        let result = self.new_list(item, &length);
        let memory = item_kind.memory(self.module.layouts);
        let (at, end) = self.loop_begin(&length);
        let (x, y) = (self.item_value(&a, &a_held, &at), self.item_value(&b, &b_held, &at));
        let place = self.item_address(&result, &memory, &at);
        self.put_pair(&item_kind, &place, [(&a_held, &x), (&b_held, &y)])?;
        self.loop_end(end);
        self.assign(dest, &result)
    }

    fn sort_kind(kind: &Kind) -> Result<i64, String> {
        match kind {
            Kind::Int | Kind::Bool => Ok(SORT_INTS),
            Kind::Float => Ok(SORT_FLOATS),
            Kind::Str => Ok(SORT_STRINGS),
            other => Err(format!("it sorts by a {other:?}, which this backend doesn't take yet")),
        }
    }

    fn sorted(&mut self, dest: LocalId, list: &Operand) -> Result<(), String> {
        let (list, _, held) = self.list_operand(list)?;
        let how = Self::sort_kind(&held)?;
        self.declare("declare ptr @fennec_list_sorted(ptr, i64) nounwind");
        let result = self.temp();
        self.line(&format!("{result} = call ptr @fennec_list_sorted(ptr {list}, i64 {how})"));
        self.assign(dest, &result)
    }

    /// `sorted(list, key=f)`: the keys go in a list of their own, which the runtime goes by.
    fn sorted_by(&mut self, dest: LocalId, list: &Operand, key: &Operand, span: Span) -> Result<(), String> {
        let (list, _, held) = self.list_operand(list)?;
        let (closure, kind) = self.operand(key)?;
        let Kind::Closure(_, returns) = kind else { return Err("a key that isn't a closure".to_owned()) };
        let how = Self::sort_kind(&returns)?;
        let key_item = match *returns {
            Kind::Int => ListItem::Int,
            Kind::Bool => ListItem::Bool,
            Kind::Float => ListItem::Float,
            _ => ListItem::Str,
        };
        let length = self.list_length(&list);
        let keys = self.new_list(key_item, &length);
        let key_text = returns.memory(self.module.layouts);
        let (at, end) = self.loop_begin(&length);
        let value = self.item_value(&list, &held, &at);
        let text = if held.in_memory() { "ptr".to_owned() } else { self.text_of(&held) };
        let got = self.call_closure_with(&closure, &returns, &[format!("{text} {value}")], span)?;
        let place = self.item_address(&keys, &key_text, &at);
        self.store_field(&returns, &got, &place, ITEM_ACCESS);
        self.loop_end(end);
        self.declare("declare ptr @fennec_list_sorted_by(ptr, ptr, i64) nounwind");
        let result = self.temp();
        self.line(&format!("{result} = call ptr @fennec_list_sorted_by(ptr {list}, ptr {keys}, i64 {how})"));
        self.release(&keys);
        self.assign(dest, &result)
    }

    /// `list.pop()`: the last item's owner goes with it.
    fn pop(&mut self, dest: Option<LocalId>, args: &[Operand], span: Span) -> Result<(), String> {
        let [Operand::Local(local)] = args else {
            return Err("`list.pop` with a place, which this backend doesn't take yet".to_owned());
        };
        let Kind::List(item) = self.kind_of(*local)?.clone() else {
            return Err("`pop` on something else".to_owned());
        };
        let held = Kind::from_ty(self.module.layouts, item.ty())
            .ok_or("a list item this backend doesn't take yet")?;
        let list = self.unshared(*local);
        let length = self.list_length(&list);
        let (empty, fail, ok) = (self.temp(), self.label("empty"), self.label("ok"));
        self.line(&format!("{empty} = icmp eq i64 {length}, 0"));
        self.line(&format!("br i1 {empty}, label %{fail}, label %{ok}, !prof !0"));
        self.start_block(&fail);
        self.declare("declare void @fennec_pop_empty(i64, i64) #1");
        let (from, to) = (i64::from(span.start), i64::from(span.end));
        self.line(&format!("call void @fennec_pop_empty(i64 {from}, i64 {to})"));
        let failed = self.failed_exit();
        self.line(&failed);
        self.start_block(&ok);
        let last = self.temp();
        self.line(&format!("{last} = add i64 {length}, -1"));
        let length_at = self.offset(&list, 16);
        self.line(&format!("store i64 {last}, ptr {length_at}"));
        let value = self.item_value(&list, &held, &last);
        match dest {
            Some(dest) if held.in_memory() => self.assign_memory(dest, &value, ""),
            Some(dest) => self.assign(dest, &value),
            None if held.counted() => {
                self.release(&value);
                Ok(())
            }
            None => Ok(()),
        }
    }

    /// `list.remove_where(f)`: what's kept moves down, and what isn't is let go of.
    fn remove_where(&mut self, args: &[Operand], span: Span) -> Result<(), String> {
        let [Operand::Local(local), test] = args else {
            return Err("`remove_where` on a literal".to_owned());
        };
        let Kind::List(item) = self.kind_of(*local)?.clone() else {
            return Err("`remove_where` on something else".to_owned());
        };
        let held = Kind::from_ty(self.module.layouts, item.ty())
            .ok_or("a list item this backend doesn't take yet")?;
        let (closure, kind) = self.operand(test)?;
        let Kind::Closure(_, returns) = kind else { return Err("a test that isn't a closure".to_owned()) };
        let list = self.unshared(*local);
        let length = self.list_length(&list);
        let kept = self.counter();
        let memory = held.memory(self.module.layouts);
        let (at, end) = self.loop_begin(&length);
        let value = self.item_value(&list, &held, &at);
        let text = if held.in_memory() { "ptr".to_owned() } else { self.text_of(&held) };
        let gone = self.call_closure_with(&closure, &returns, &[format!("{text} {value}")], span)?;
        let (drop, keep, next) = (self.label("drop"), self.label("keep"), self.label("next"));
        self.line(&format!("br i1 {gone}, label %{drop}, label %{keep}"));
        self.start_block(&drop);
        if held.counted() {
            self.release(&value);
        }
        self.line(&format!("br label %{next}"));
        self.start_block(&keep);
        let (place_number, later) = (self.temp(), self.temp());
        self.line(&format!("{place_number} = load i64, ptr {kept}"));
        let place = self.item_address(&list, &memory, &place_number);
        self.store_field(&held, &value, &place, ITEM_ACCESS);
        self.line(&format!("{later} = add i64 {place_number}, 1"));
        self.line(&format!("store i64 {later}, ptr {kept}"));
        self.line(&format!("br label %{next}"));
        self.start_block(&next);
        self.loop_end(end);
        let (count, length_at) = (self.temp(), self.offset(&list, 16));
        self.line(&format!("{count} = load i64, ptr {kept}"));
        self.line(&format!("store i64 {count}, ptr {length_at}"));
        Ok(())
    }

    /// `text.chars()`: each character's block lasts the whole run, and comes from its byte place.
    fn chars(&mut self, dest: LocalId, text: &Operand) -> Result<(), String> {
        let (text, _) = self.operand(text)?;
        let (chars_at, count) = (self.offset(&text, 24), self.temp());
        self.line(&format!("{count} = load i64, ptr {chars_at}"));
        let list = self.new_list(ListItem::Str, &count);
        let start = self.offset(&text, TEXT.unsigned_abs());
        self.declare("declare ptr @fennec_str_char_at(ptr, i64) nounwind");
        let byte_place = self.counter();
        let (at, end) = self.loop_begin(&count);
        let byte = self.temp();
        self.line(&format!("{byte} = load i64, ptr {byte_place}"));
        let one = self.temp();
        self.line(&format!("{one} = call ptr @fennec_str_char_at(ptr {text}, i64 {byte})"));
        self.retain(&one);
        let place = self.item_address(&list, "ptr", &at);
        self.line(&format!("store ptr {one}, ptr {place}{ITEM_ACCESS}"));
        let (lead_at, lead, under_two, under_three, under_four) =
            (self.temp(), self.temp(), self.temp(), self.temp(), self.temp());
        self.line(&format!("{lead_at} = getelementptr inbounds i8, ptr {start}, i64 {byte}"));
        self.line(&format!("{lead} = load i8, ptr {lead_at}"));
        self.line(&format!("{under_two} = icmp ult i8 {lead}, -64"));
        self.line(&format!("{under_three} = icmp ult i8 {lead}, -32"));
        self.line(&format!("{under_four} = icmp ult i8 {lead}, -16"));
        let (four, three, size, next) = (self.temp(), self.temp(), self.temp(), self.temp());
        self.line(&format!("{four} = select i1 {under_four}, i64 3, i64 4"));
        self.line(&format!("{three} = select i1 {under_three}, i64 2, i64 {four}"));
        self.line(&format!("{size} = select i1 {under_two}, i64 1, i64 {three}"));
        self.line(&format!("{next} = add i64 {byte}, {size}"));
        self.line(&format!("store i64 {next}, ptr {byte_place}"));
        self.loop_end(end);
        self.assign(dest, &list)
    }

    /// The builtins here; gives back `None` for any other.
    pub(crate) fn loop_builtin(
        &mut self,
        dest: Option<LocalId>,
        name: &str,
        args: &[Operand],
        span: Span,
    ) -> Option<Result<(), String>> {
        let result = |dest: Option<LocalId>| dest.ok_or_else(|| "no result".to_owned());
        Some(match (name, args) {
            ("sum", [list]) => result(dest).and_then(|dest| self.sum(dest, list, span)),
            ("all", [list]) => result(dest).and_then(|dest| self.all_or_any(dest, list, true)),
            ("any", [list]) => result(dest).and_then(|dest| self.all_or_any(dest, list, false)),
            ("enumerate", [list]) => result(dest).and_then(|dest| self.enumerate(dest, list)),
            ("zip", [a, b]) => result(dest).and_then(|dest| self.zip(dest, a, b)),
            ("sorted", [list]) => result(dest).and_then(|dest| self.sorted(dest, list)),
            ("sorted.key", [list, key]) => {
                result(dest).and_then(|dest| self.sorted_by(dest, list, key, span))
            }
            ("list.pop", _) => self.pop(dest, args, span),
            ("list.remove_where", _) => self.remove_where(args, span),
            ("str.chars", [text]) => result(dest).and_then(|dest| self.chars(dest, text)),
            ("dict.get", _) => result(dest).and_then(|dest| self.dict_get(dest, args)),
            ("dict.items", [dict]) => result(dest).and_then(|dest| self.dict_list(dest, dict, "items")),
            ("dict.keys", [dict]) => result(dest).and_then(|dest| self.dict_list(dest, dict, "keys")),
            ("dict.values", [dict]) => result(dest).and_then(|dest| self.dict_list(dest, dict, "values")),
            ("dict.len" | "set.len", [dict]) => result(dest).and_then(|dest| {
                let (dict, _) = self.operand(dict)?;
                let length = self.dict_length(&dict);
                self.assign(dest, &length)
            }),
            ("set.add", [Operand::Local(local), item]) => {
                let place = self.slot(*local);
                match self.kind_of(*local).cloned() {
                    Ok(Kind::Dict(key, value)) => self.dict_store(&place, (key, value), item, None),
                    _ => Err("`add` on something else".to_owned()),
                }
            }
            ("str.strip", [text]) => result(dest).and_then(|dest| {
                let (text, _) = self.operand(text)?;
                self.declare("declare ptr @fennec_str_strip(ptr, i64) nounwind");
                let stripped = self.temp();
                self.line(&format!("{stripped} = call ptr @fennec_str_strip(ptr {text}, i64 0)"));
                self.assign(dest, &stripped)
            }),
            _ => return None,
        })
    }
}
