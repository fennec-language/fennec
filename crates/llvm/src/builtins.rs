//! Builtins written out here: loops over a list that the runtime has no call for.

use fennec_ir::{LocalId, Operand};
use fennec_layout::ListItem;
use fennec_rt::embed::layout::{TEXT, TEXT_BYTES, TEXT_CHARS};
use fennec_rt::embed::lists::{SORT_FLOATS, SORT_INTS, SORT_STRINGS, SORT_U64S};
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

    pub(crate) fn loop_end(&mut self, end: &LoopEnd) {
        let (at, next) = (self.temp(), self.temp());
        self.line(&format!("{at} = load i64, ptr {}", end.counter));
        self.line(&format!("{next} = add i64 {at}, 1"));
        self.line(&format!("store i64 {next}, ptr {}", end.counter));
        self.line(&format!("br label %{}", end.head));
        self.start_block(&end.done);
    }

    pub(crate) fn list_operand(&mut self, operand: &Operand) -> Result<(String, ListItem, Kind), String> {
        let (list, kind) = self.operand(operand)?;
        let Kind::List(item) = kind else { return Err(format!("it goes through a {kind:?} as if a list")) };
        let held = Kind::from_ty(self.module.layouts, item.ty())
            .ok_or("a list item this backend doesn't take yet")?;
        Ok((list, item, held))
    }

    /// An item, as a number or address, or the place it is if it's kept in memory.
    pub(crate) fn item_value(&mut self, list: &str, held: &Kind, at: &str) -> String {
        let memory = held.memory(self.module.layouts);
        let place = self.item_address(list, &memory, at);
        if held.in_memory() { place } else { self.load_field(held, &place, ITEM_ACCESS) }
    }

    /// Puts a value in a new list's empty place, with an owner of its own. A struct kept in
    /// place is copied as it is, so each block it holds gets an owner for the copy too, as what
    /// it's copied from keeps its own.
    pub(crate) fn put_item(&mut self, item: &Kind, kind: &Kind, value: &str, place: &str) {
        let (kind, value, owned) = self.as_header(kind, value);
        if kind.counted() && !owned {
            self.retain_value(&value, &kind);
        }
        self.store_field(item, &value, place, ITEM_ACCESS);
        if kind.in_memory() && !owned {
            self.retain_inline(item, place);
        }
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
            let (kind, value, owned) = self.as_header(kind, value);
            if kind.counted() && !owned {
                self.retain_value(&value, &kind);
            }
            self.store_field(&kind, &value, &at, "");
            if kind.in_memory() && !owned {
                self.retain_inline(&kind, &at);
            }
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

    fn sum(
        &mut self,
        dest: LocalId,
        list: &Operand,
        start: Option<&Operand>,
        span: Span,
    ) -> Result<(), String> {
        let (list, _, held) = self.list_operand(list)?;
        let length = self.list_length(&list);
        let total = self.place_for(&held);
        let zero = match start {
            Some(start) => self.operand(start)?.0,
            None if held == Kind::Float => crate::emit::float_text(0.0),
            None => "0".to_owned(),
        };
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
        self.loop_end(&end);
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
        self.loop_end(&end);
        let held_result = self.temp();
        self.line(&format!("{held_result} = load i1, ptr {result}"));
        self.assign(dest, &held_result)
    }

    /// `enumerate(list)`.
    fn enumerate(&mut self, dest: LocalId, list: &Operand) -> Result<(), String> {
        // Only counted, and its pairs only taken apart: it's never made.
        if self.known.enumerated.contains_key(&dest.0) {
            return Ok(());
        }
        let (list, _, held) = self.list_operand(list)?;
        self.enumerated_list(dest, &list, &held)
    }

    /// A new list of each of a list's items with its place.
    pub(crate) fn enumerated_list(&mut self, dest: LocalId, list: &str, held: &Kind) -> Result<(), String> {
        let Kind::List(item) = self.kind_of(dest)?.clone() else {
            return Err("`enumerate`, not as a list".to_owned());
        };
        let item_kind = Kind::from_ty(self.module.layouts, item.ty())
            .ok_or("a list item this backend doesn't take yet")?;
        let length = self.list_length(list);
        let result = self.new_list(item, &length);
        let memory = item_kind.memory(self.module.layouts);
        let (at, end) = self.loop_begin(&length);
        let value = self.item_value(list, held, &at);
        let place = self.item_address(&result, &memory, &at);
        self.put_pair(&item_kind, &place, [(&Kind::Int, &at), (held, &value)])?;
        self.loop_end(&end);
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
        self.loop_end(&end);
        self.assign(dest, &result)
    }

    fn sort_kind(kind: &Kind) -> Result<i64, String> {
        match kind {
            Kind::Int | Kind::Bool => Ok(SORT_INTS),
            Kind::U64 => Ok(SORT_U64S),
            // An `f32` is held as the float it rounds to, so it orders as a float does.
            Kind::Float | Kind::F32 => Ok(SORT_FLOATS),
            Kind::Str => Ok(SORT_STRINGS),
            other => Err(format!("it sorts by a {other:?}, which this backend doesn't take yet")),
        }
    }

    pub(crate) fn sorted(&mut self, dest: LocalId, list: &Operand) -> Result<(), String> {
        let (list, _, held) = self.list_operand(list)?;
        if matches!(held, Kind::Struct(_) | Kind::Boxed(_)) {
            let compare = self.tuple_comparison(&held)?;
            self.declare("declare ptr @fennec_list_sorted_with(ptr, ptr) nounwind");
            let result = self.temp();
            self.line(&format!("{result} = call ptr @fennec_list_sorted_with(ptr {list}, ptr {compare})"));
            // A type's own `<` can fail, which ends the sort.
            self.leave_if_failed();
            return self.assign(dest, &result);
        }
        let how = Self::sort_kind(&held)?;
        self.declare("declare ptr @fennec_list_sorted(ptr, i64) nounwind");
        let result = self.temp();
        self.line(&format!("{result} = call ptr @fennec_list_sorted(ptr {list}, i64 {how})"));
        self.assign(dest, &result)
    }

    /// The function comparing two tuples in a list, by their items' addresses, item by item.
    fn tuple_comparison(&mut self, held: &Kind) -> Result<String, String> {
        let (Kind::Struct(id) | Kind::Boxed(id)) = held else {
            return Err("a sort of something else".to_owned());
        };
        let layouts = self.module.layouts;
        let layout = layouts.get(*id).ok_or("no such layout")?;
        if !layout.name.starts_with('(') {
            return Err(format!("it sorts `{}` values, which this backend doesn't take yet", layout.name));
        }
        let boxed = matches!(held, Kind::Boxed(_));
        let name = format!("@\"cmp_{}\"", layout.name);
        let mut text = format!(
            "define internal i64 {name}(i64 %x_at, i64 %y_at) #0 {{\nentry:\n  %x_item = inttoptr i64 %x_at to ptr\n  %y_item = inttoptr i64 %y_at to ptr\n"
        );
        if boxed {
            text += "  %x_block = load ptr, ptr %x_item\n  %y_block = load ptr, ptr %y_item\n  %x = getelementptr inbounds i8, ptr %x_block, i64 16\n  %y = getelementptr inbounds i8, ptr %y_block, i64 16\n";
        } else {
            text += "  %x = getelementptr inbounds i8, ptr %x_item, i64 0\n  %y = getelementptr inbounds i8, ptr %y_item, i64 0\n";
        }
        text += "  br label %f0\n";
        for (at, field) in layout.fields.iter().enumerate() {
            let kind =
                Kind::from_ty(layouts, field.ty).ok_or("a tuple item this backend doesn't take yet")?;
            let next = at + 1;
            let offset = field.offset;
            text += &format!(
                "f{at}:\n  %xa{at} = getelementptr inbounds i8, ptr %x, i64 {offset}\n  %ya{at} = getelementptr inbounds i8, ptr %y, i64 {offset}\n"
            );
            let (below, above) = match kind {
                Kind::Int | Kind::Bool => {
                    text +=
                        &format!("  %xv{at} = load i64, ptr %xa{at}\n  %yv{at} = load i64, ptr %ya{at}\n");
                    (format!("icmp slt i64 %xv{at}, %yv{at}"), format!("icmp sgt i64 %xv{at}, %yv{at}"))
                }
                Kind::U64 => {
                    text +=
                        &format!("  %xv{at} = load i64, ptr %xa{at}\n  %yv{at} = load i64, ptr %ya{at}\n");
                    (format!("icmp ult i64 %xv{at}, %yv{at}"), format!("icmp ugt i64 %xv{at}, %yv{at}"))
                }
                // An `f32` is held as the float it rounds to.
                Kind::Float | Kind::F32 => {
                    text += &format!(
                        "  %xv{at} = load double, ptr %xa{at}\n  %yv{at} = load double, ptr %ya{at}\n"
                    );
                    // Floats order with NaN last, whatever its sign.
                    text += &format!(
                        "  %xu{at} = fcmp ult double %xv{at}, %yv{at}\n  %xo{at} = fcmp ord double %xv{at}, %xv{at}\n  %yu{at} = fcmp ult double %yv{at}, %xv{at}\n  %yo{at} = fcmp ord double %yv{at}, %yv{at}\n"
                    );
                    (format!("and i1 %xu{at}, %xo{at}"), format!("and i1 %yu{at}, %yo{at}"))
                }
                Kind::Str if self.headers() => {
                    self.header_order_helper();
                    text += &format!("  %o{at} = call i64 @fen_header_order(ptr %xa{at}, ptr %ya{at})\n");
                    (format!("icmp slt i64 %o{at}, 0"), format!("icmp sgt i64 %o{at}, 0"))
                }
                Kind::Str => {
                    self.declare("declare i64 @fennec_str_compare(ptr, ptr) nounwind");
                    text += &format!(
                        "  %xv{at} = load ptr, ptr %xa{at}\n  %yv{at} = load ptr, ptr %ya{at}\n  %o{at} = call i64 @fennec_str_compare(ptr %xv{at}, ptr %yv{at})\n"
                    );
                    (format!("icmp slt i64 %o{at}, 0"), format!("icmp sgt i64 %o{at}, 0"))
                }
                other => {
                    return Err(format!(
                        "it sorts tuples holding a {other:?}, which this backend doesn't take yet"
                    ));
                }
            };
            text += &format!(
                "  %b{at} = {below}\n  br i1 %b{at}, label %less, label %g{at}\ng{at}:\n  %a{at} = {above}\n  br i1 %a{at}, label %more, label %f{next}\n"
            );
        }
        let count = layout.fields.len();
        text += &format!("f{count}:\n  ret i64 0\nless:\n  ret i64 -1\nmore:\n  ret i64 1\n}}\n\n");
        let key = name.clone();
        self.module.add_helper(&key, text);
        Ok(name)
    }

    /// `sorted(list, key=f)`: the keys go in a list of their own, which the runtime goes by.
    pub(crate) fn sorted_by(
        &mut self,
        dest: LocalId,
        list: &Operand,
        key: &Operand,
        span: Span,
    ) -> Result<(), String> {
        let (list, _, held) = self.list_operand(list)?;
        let (keys, kind) = self.keys_of(&list, &held, key, span)?;
        if matches!(kind, Kind::Struct(_) | Kind::Boxed(_)) {
            let compare = self.tuple_comparison(&kind)?;
            self.declare("declare ptr @fennec_list_sorted_by_keys_with(ptr, ptr, ptr) nounwind");
            let result = self.temp();
            self.line(&format!(
                "{result} = call ptr @fennec_list_sorted_by_keys_with(ptr {list}, ptr {keys}, ptr {compare})"
            ));
            self.release(&keys);
            return self.assign(dest, &result);
        }
        let how = Self::sort_kind(&kind)?;
        self.declare("declare ptr @fennec_list_sorted_by(ptr, ptr, i64) nounwind");
        let result = self.temp();
        self.line(&format!("{result} = call ptr @fennec_list_sorted_by(ptr {list}, ptr {keys}, i64 {how})"));
        self.release(&keys);
        self.assign(dest, &result)
    }

    /// `list.sort()`: the list made its caller's alone, then sorted where its items lie, so no item
    /// gains or loses an owner, where a sorted copy would count every one twice.
    pub(crate) fn sort_in_place(&mut self, list: LocalId) -> Result<(), String> {
        let held = self.items_held(list)?;
        let compare = if matches!(held, Kind::Struct(_) | Kind::Boxed(_)) {
            Some(self.tuple_comparison(&held)?)
        } else {
            None
        };
        let how = if compare.is_none() { Self::sort_kind(&held)? } else { 0 };
        let owned = self.unshared(list);
        if let Some(compare) = compare {
            self.declare("declare void @fennec_list_sort_with(ptr, ptr) nounwind");
            self.line(&format!("call void @fennec_list_sort_with(ptr {owned}, ptr {compare})"));
            self.leave_if_failed();
            return Ok(());
        }
        self.declare("declare void @fennec_list_sort(ptr, i64) nounwind");
        self.line(&format!("call void @fennec_list_sort(ptr {owned}, i64 {how})"));
        Ok(())
    }

    /// `list.sort(key=f)`: as `sort_in_place`, in the order of the keys `f` gives.
    pub(crate) fn sort_by_in_place(
        &mut self,
        list: LocalId,
        key: &Operand,
        span: Span,
    ) -> Result<(), String> {
        let held = self.items_held(list)?;
        let owned = self.unshared(list);
        let (keys, kind) = self.keys_of(&owned, &held, key, span)?;
        if matches!(kind, Kind::Struct(_) | Kind::Boxed(_)) {
            let compare = self.tuple_comparison(&kind)?;
            self.declare("declare void @fennec_list_sort_by_keys_with(ptr, ptr, ptr) nounwind");
            self.line(&format!(
                "call void @fennec_list_sort_by_keys_with(ptr {owned}, ptr {keys}, ptr {compare})"
            ));
            self.release(&keys);
            self.leave_if_failed();
            return Ok(());
        }
        let how = Self::sort_kind(&kind)?;
        self.declare("declare void @fennec_list_sort_by(ptr, ptr, i64) nounwind");
        self.line(&format!("call void @fennec_list_sort_by(ptr {owned}, ptr {keys}, i64 {how})"));
        self.release(&keys);
        Ok(())
    }

    /// The standard library's `list.reverse()` and `list.swap(i, j)`, done where the items lie: an
    /// item that moves keeps its owners, so nothing is counted, where the library's code copies an
    /// item out and writes two back, counting each block it holds four times. `None` for any other
    /// call, or one this doesn't take, which is called as written.
    pub(crate) fn moving_list_method(
        &mut self,
        name: &str,
        args: &[Operand],
        span: Span,
    ) -> Option<Result<(), String>> {
        let named = |method: &str| {
            name.strip_prefix(method).is_some_and(|rest| rest.is_empty() || rest.starts_with('['))
        };
        let list = match args.first() {
            Some(Operand::Local(list)) if matches!(self.kind_of(*list), Ok(Kind::List(_))) => *list,
            _ => return None,
        };
        match args {
            [_] if named("list.reverse") => {
                let owned = self.unshared(list);
                self.declare("declare void @fennec_list_reverse(ptr) nounwind");
                self.line(&format!("call void @fennec_list_reverse(ptr {owned})"));
                Some(Ok(()))
            }
            [_, first, second] if named("list.swap") => Some(self.swap_items(list, first, second, span)),
            _ => None,
        }
    }

    /// `list.swap(i, j)`: each index checked as `list[i]` checks it, then the two items' words
    /// swapped in the list made its caller's alone.
    fn swap_items(
        &mut self,
        list: LocalId,
        first: &Operand,
        second: &Operand,
        span: Span,
    ) -> Result<(), String> {
        let held = self.items_held(list)?;
        let memory = held.memory(self.module.layouts);
        let (shared, _) = self.operand(&Operand::Local(list))?;
        let ((first, _), (second, _)) = (self.operand(first)?, self.operand(second)?);
        let first = self.position(&shared, &first, span);
        let second = self.position(&shared, &second, span);
        let owned = self.unshared(list);
        let (a, b) =
            (self.item_address(&owned, &memory, &first), self.item_address(&owned, &memory, &second));
        let (x, y) = (self.temp(), self.temp());
        self.line(&format!("{x} = load {memory}, ptr {a}"));
        self.line(&format!("{y} = load {memory}, ptr {b}"));
        self.line(&format!("store {memory} {y}, ptr {a}"));
        self.line(&format!("store {memory} {x}, ptr {b}"));
        Ok(())
    }

    /// How a list local's items are held.
    fn items_held(&self, list: LocalId) -> Result<Kind, String> {
        let Kind::List(item) = self.kind_of(list)? else {
            return Err("it sorts something that isn't a list".to_owned());
        };
        self.item_kind(*item)
    }

    /// A new list of what a key function gives for each of a list's items, and what those are.
    pub(crate) fn keys_of(
        &mut self,
        list: &str,
        held: &Kind,
        key: &Operand,
        span: Span,
    ) -> Result<(String, Kind), String> {
        let (closure, kind) = self.operand(key)?;
        let Kind::Closure(_, returns) = kind else { return Err("a key that isn't a closure".to_owned()) };
        if !matches!(*returns, Kind::Struct(_) | Kind::Boxed(_)) {
            Self::sort_kind(&returns)?;
        }
        let key_item = match *returns {
            Kind::Int => ListItem::Int,
            Kind::Bool => ListItem::Bool,
            Kind::Float => ListItem::Float,
            Kind::U64 => ListItem::U64,
            Kind::F32 => ListItem::F32,
            Kind::Struct(id) => ListItem::Struct(id),
            Kind::Boxed(id) => ListItem::Boxed(id),
            _ => ListItem::Str,
        };
        let length = self.list_length(list);
        // The stack is checked once, before the keys are made: the calls below are all from here,
        // as deep as this.
        self.check_stack(span);
        let keys = self.new_list(key_item, &length);
        let key_text = returns.memory(self.module.layouts);
        let (at, end) = self.loop_begin(&length);
        let value = self.item_value(list, held, &at);
        let text = if held.in_memory() { "ptr".to_owned() } else { self.text_of(held) };
        let (got, failed) = self.call_closure_unchecked(&closure, &returns, &[format!("{text} {value}")]);
        // A key that fails lets go of the keys made so far, the rest still zero, before leaving.
        let (let_go, kept) = (self.label("keys_let_go"), self.label("keys_kept"));
        self.line(&format!("br i1 {failed}, label %{let_go}, label %{kept}, !prof !0"));
        self.start_block(&let_go);
        self.release(&keys);
        self.leave_traced("true", span);
        self.line("unreachable");
        self.start_block(&kept);
        let place = self.item_address(&keys, &key_text, &at);
        self.store_field(&returns, &got, &place, ITEM_ACCESS);
        self.loop_end(&end);
        Ok((keys, *returns))
    }

    /// `list.pop()`: the last item's owner goes with it.
    pub(crate) fn pop(&mut self, dest: Option<LocalId>, args: &[Operand], span: Span) -> Result<(), String> {
        if let [Operand::Local(local), at] = args {
            return self.pop_at(dest, *local, at, span);
        }
        let [Operand::Local(local)] = args else {
            return Err("`list.pop` on a literal, which this backend doesn't take yet".to_owned());
        };
        let Kind::List(item) = self.kind_of(*local)?.clone() else {
            return Err("`pop` on something else".to_owned());
        };
        let held = Kind::from_ty(self.module.layouts, item.ty())
            .ok_or("a list item this backend doesn't take yet")?;
        let list = self.popped_from(*local);
        let length = self.list_length(&list);
        let (empty, fail, ok) = (self.temp(), self.label("empty"), self.label("ok"));
        self.line(&format!("{empty} = icmp eq i64 {length}, 0"));
        self.line(&format!("br i1 {empty}, label %{fail}, label %{ok}, !prof !0"));
        self.start_block(&fail);
        self.declare("declare void @fennec_pop_empty(i64, i64) #1");
        let (from, to) = self.place_in_source(span);
        self.line(&format!("call void @fennec_pop_empty(i64 {from}, i64 {to})"));
        let failed = self.failed_exit();
        self.line(&failed);
        self.start_block(&ok);
        let last = self.temp();
        self.line(&format!("{last} = add i64 {length}, -1"));
        let length_at = self.offset(&list, 16);
        self.line(&format!("store i64 {last}, ptr {length_at}"));
        let value = self.item_value(&list, &held, &last);
        // What's popped keeps the owners the list had for it.
        match dest {
            Some(dest) if held.in_memory() => self.assign_memory_owned(dest, &value),
            Some(dest) => self.assign(dest, &value),
            None if held.counted() => {
                self.release_kind(&value, &held);
                Ok(())
            }
            None => {
                self.release_inline(&held, &value);
                Ok(())
            }
        }
    }

    /// `list.pop(at)`: the item there goes with its owner, and those after it move down.
    fn pop_at(
        &mut self,
        dest: Option<LocalId>,
        local: LocalId,
        at: &Operand,
        span: Span,
    ) -> Result<(), String> {
        let Kind::List(item) = self.kind_of(local)?.clone() else {
            return Err("`pop` on something else".to_owned());
        };
        let held = self.item_kind(item)?;
        let memory = held.memory(self.module.layouts);
        let list = self.unshared(local);
        let (index, _) = self.operand(at)?;
        let position = self.position(&list, &index, span);
        let value = self.item_value(&list, &held, &position);
        // A struct in the list is copied out before what's after it moves over it, with the
        // owners the list had for it.
        if held.in_memory() {
            match dest {
                Some(dest) => self.assign_memory_owned(dest, &value)?,
                None => self.release_inline(&held, &value),
            }
        }
        let length = self.list_length(&list);
        let (next, last, moved, size) = (self.temp(), self.temp(), self.temp(), self.temp());
        self.line(&format!("{next} = add i64 {position}, 1"));
        self.line(&format!("{last} = add i64 {length}, -1"));
        self.line(&format!("{moved} = sub i64 {last}, {position}"));
        let item_size = held.size(self.module.layouts);
        self.line(&format!("{size} = mul i64 {moved}, {item_size}"));
        let (to, from) =
            (self.item_address(&list, &memory, &position), self.item_address(&list, &memory, &next));
        self.declare("declare void @llvm.memmove.p0.p0.i64(ptr, ptr, i64, i1)");
        self.line(&format!("call void @llvm.memmove.p0.p0.i64(ptr {to}, ptr {from}, i64 {size}, i1 false)"));
        let length_at = self.offset(&list, 16);
        self.line(&format!("store i64 {last}, ptr {length_at}"));
        match dest {
            _ if held.in_memory() => Ok(()),
            Some(dest) => self.assign(dest, &value),
            None if held.counted() => {
                self.release_kind(&value, &held);
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
        let gone = self.call_closure_with(&closure, &returns, &[format!("{text} {value}")], span);
        let (drop, keep, next) = (self.label("drop"), self.label("keep"), self.label("next"));
        self.line(&format!("br i1 {gone}, label %{drop}, label %{keep}"));
        self.start_block(&drop);
        if held.counted() {
            self.release_kind(&value, &held);
        }
        self.release_inline(&held, &value);
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
        self.loop_end(&end);
        let (count, length_at) = (self.temp(), self.offset(&list, 16));
        self.line(&format!("{count} = load i64, ptr {kept}"));
        self.line(&format!("store i64 {count}, ptr {length_at}"));
        Ok(())
    }

    /// The block of the character starting `byte` bytes into a string whose bytes are at
    /// `start`, and its first byte. Each lasts the whole run: an ASCII one's from the table, any
    /// other's from the runtime.
    pub(crate) fn char_at_byte(&mut self, text: &str, start: &str, byte: &str) -> (String, String) {
        self.declare("declare ptr @fennec_str_char_at(ptr, i64) nounwind");
        let (lead_at, lead) = (self.temp(), self.temp());
        self.line(&format!("{lead_at} = getelementptr inbounds i8, ptr {start}, i64 {byte}"));
        self.line(&format!("{lead} = load i8, ptr {lead_at}"));
        self.declare("@fennec_ascii = external dso_local global ptr");
        let (plain, table, wide, slot, from_table, other) =
            (self.temp(), self.temp(), self.temp(), self.temp(), self.temp(), self.temp());
        let (ascii, rest, joined) = (self.label("ascii"), self.label("rest"), self.label("joined"));
        self.line(&format!("{plain} = icmp sge i8 {lead}, 0"));
        self.line(&format!("br i1 {plain}, label %{ascii}, label %{rest}"));
        self.start_block(&ascii);
        self.line(&format!("{table} = load ptr, ptr @fennec_ascii, !invariant.load !1"));
        self.line(&format!("{wide} = zext i8 {lead} to i64"));
        self.line(&format!("{slot} = getelementptr inbounds i64, ptr {table}, i64 {wide}"));
        self.line(&format!("{from_table} = load ptr, ptr {slot}"));
        self.line(&format!("br label %{joined}"));
        self.start_block(&rest);
        self.line(&format!("{other} = call ptr @fennec_str_char_at(ptr {text}, i64 {byte})"));
        self.line(&format!("br label %{joined}"));
        self.start_block(&joined);
        let one = self.temp();
        self.line(&format!("{one} = phi ptr [ {from_table}, %{ascii} ], [ {other}, %{rest} ]"));
        (one, lead)
    }

    /// `str.byte_len` and `str.char_at_byte`, which a loop over a string's characters uses.
    pub(crate) fn text_by_bytes(
        &mut self,
        dest: Option<LocalId>,
        name: &str,
        args: &[Operand],
    ) -> Result<(), String> {
        let dest = dest.ok_or("no result")?;
        let (text, _) = self.operand(args.first().ok_or("no argument")?)?;
        if self.headers() {
            return self.header_by_bytes(dest, name, args, &text);
        }
        self.text_by_bytes_of(dest, name, args, &text)
    }

    /// [`Self::text_by_bytes`] on a string's block.
    fn text_by_bytes_of(
        &mut self,
        dest: LocalId,
        name: &str,
        args: &[Operand],
        text: &str,
    ) -> Result<(), String> {
        let text = text.to_owned();
        if name == "str.byte_len" {
            let (at, bytes) = (self.offset(&text, TEXT_BYTES.unsigned_abs()), self.temp());
            self.load_length(&bytes, &at);
            return self.assign(dest, &bytes);
        }
        if name == "str.is_ascii" {
            let (bytes_at, chars_at) = (
                self.offset(&text, TEXT_BYTES.unsigned_abs()),
                self.offset(&text, TEXT_CHARS.unsigned_abs()),
            );
            let (bytes, chars, same) = (self.temp(), self.temp(), self.temp());
            self.load_length(&bytes, &bytes_at);
            self.load_length(&chars, &chars_at);
            self.line(&format!("{same} = icmp eq i64 {bytes}, {chars}"));
            return self.assign(dest, &same);
        }
        let (byte, _) = self.operand(args.get(1).ok_or("no argument")?)?;
        let start = self.offset(&text, TEXT.unsigned_abs());
        let (one, _) = self.char_at_byte(&text, &start, &byte);
        if self.headers() {
            let header = self.text_from_borrowed_block(&one);
            return self.assign(dest, &header);
        }
        // Each character's block lasts the whole run, so a local reading it needn't own it.
        if !self.known.is_borrowed(dest) {
            self.retain(&one);
        }
        self.assign(dest, &one)
    }

    /// `text.chars()`: each character's block lasts the whole run, and comes from its byte place.
    pub(crate) fn chars(&mut self, dest: LocalId, text: &Operand) -> Result<(), String> {
        let (text, _) = self.operand(text)?;
        self.on_blocks([&text], |emit, [block]| emit.chars_of(dest, &block))
    }

    /// [`Self::chars`] of a string's block.
    fn chars_of(&mut self, dest: LocalId, text: &str) -> Result<(), String> {
        let list = self.chars_list(text);
        self.assign(dest, &list)
    }

    /// A new list of the characters of a string's block.
    pub(crate) fn chars_list(&mut self, text: &str) -> String {
        let text = text.to_owned();
        let (chars_at, count) = (self.offset(&text, 24), self.temp());
        self.line(&format!("{count} = load i64, ptr {chars_at}"));
        let list = self.new_list(ListItem::Str, &count);
        let start = self.offset(&text, TEXT.unsigned_abs());
        let byte_place = self.counter();
        let (at, end) = self.loop_begin(&count);
        let byte = self.temp();
        self.line(&format!("{byte} = load i64, ptr {byte_place}"));
        let (one, lead) = self.char_at_byte(&text, &start, &byte);
        if self.headers() {
            let header = self.text_from_borrowed_block(&one);
            let place = self.item_address(&list, crate::headers::HEADER, &at);
            self.line(&format!("store {} {header}, ptr {place}{ITEM_ACCESS}", crate::headers::HEADER));
        } else {
            self.retain(&one);
            let place = self.item_address(&list, "ptr", &at);
            self.line(&format!("store ptr {one}, ptr {place}{ITEM_ACCESS}"));
        }
        let (under_two, under_three, under_four) = (self.temp(), self.temp(), self.temp());
        self.line(&format!("{under_two} = icmp ult i8 {lead}, -64"));
        self.line(&format!("{under_three} = icmp ult i8 {lead}, -32"));
        self.line(&format!("{under_four} = icmp ult i8 {lead}, -16"));
        let (four, three, size, next) = (self.temp(), self.temp(), self.temp(), self.temp());
        self.line(&format!("{four} = select i1 {under_four}, i64 3, i64 4"));
        self.line(&format!("{three} = select i1 {under_three}, i64 2, i64 {four}"));
        self.line(&format!("{size} = select i1 {under_two}, i64 1, i64 {three}"));
        self.line(&format!("{next} = add i64 {byte}, {size}"));
        self.line(&format!("store i64 {next}, ptr {byte_place}"));
        self.loop_end(&end);
        list
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
            ("sum", [list]) => result(dest).and_then(|dest| self.sum(dest, list, None, span)),
            ("sum" | "sum.start", [list, start]) => {
                result(dest).and_then(|dest| self.sum(dest, list, Some(start), span))
            }
            ("all", [list]) => result(dest).and_then(|dest| self.all_or_any(dest, list, true)),
            ("any", [list]) => result(dest).and_then(|dest| self.all_or_any(dest, list, false)),
            ("enumerate", [list]) => result(dest).and_then(|dest| self.enumerate(dest, list)),
            ("zip", [a, b]) => result(dest).and_then(|dest| self.zip(dest, a, b)),
            ("sorted", [list]) => result(dest).and_then(|dest| self.sorted(dest, list)),
            ("sorted.key", [list, key]) => {
                result(dest).and_then(|dest| self.sorted_by(dest, list, key, span))
            }
            ("list.pop", _) => self.pop(dest, args, span),
            ("list.insert", _) => self.insert(args, "fennec_list_insert", span),
            ("list.remove_where", _) => self.remove_where(args, span),
            ("str.chars", [text]) => result(dest).and_then(|dest| self.chars(dest, text)),
            ("dict.get", _) => result(dest).and_then(|dest| self.dict_get(dest, args)),
            ("dict.pop", [_, _]) => self.dict_pop(dest, args, span),
            // Only gone through in order, a dict's keys, values or items are never made: the
            // loop walks its entries.
            ("dict.items" | "dict.keys" | "dict.values", [_])
                if dest.is_some_and(|dest| self.known.map_walks.contains_key(&dest.0)) =>
            {
                Ok(())
            }
            ("dict.items", [dict]) => result(dest).and_then(|dest| self.dict_list(dest, dict, "items")),
            ("dict.keys", [dict]) => result(dest).and_then(|dest| self.dict_list(dest, dict, "keys")),
            ("dict.values", [dict]) => result(dest).and_then(|dest| self.dict_list(dest, dict, "values")),
            ("dict.contains" | "set.contains", [dict, key]) => result(dest).and_then(|dest| {
                let found = self.dict_contains(fennec_syntax::BinOp::In, key, dict)?;
                self.assign(dest, &found)
            }),
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
            ("str.strip" | "str.lstrip" | "str.rstrip", [text]) => result(dest).and_then(|dest| {
                let (text, _) = self.operand(text)?;
                let which = match name {
                    "str.lstrip" => 1,
                    "str.rstrip" => 2,
                    _ => 0,
                };
                if self.headers() {
                    let stripped = self.header_strip(&text, which);
                    return self.assign(dest, &stripped);
                }
                let stripped = self.strip_text(&text, which);
                self.assign(dest, &stripped)
            }),
            _ => return None,
        })
    }
}
