//! `Table`s and `Id`s, which the runtime doesn't hold natively. An `Id` is a plain struct of two
//! words: its slot and generation, 32 bits each in one, and its tag. A `Table` is a boxed struct of its items, each slot's generation,
//! how many are live, and its tag, a number from the runtime no other `Table` has, so an `Id`
//! from another finds nothing in it: a removed slot's generation is stored as `-1 - generation`, and its item
//! is zero, so a stale `Id` never finds the wrong thing and a slot is used again with the next
//! generation. A slot whose generation can't go higher is retired, never
//! used again, so a generation never comes round. The first free slot is used first, and the map says how
//! far up there's none, so filling a map doesn't look again at what it's filled already.

use fennec_ir::{LocalId, Operand};
use fennec_syntax::Span;

use crate::emit::{Emit, HEADER_ACCESS, ITEM_ACCESS};
use crate::kinds::Kind;
use crate::lists::{FIELDS_OFFSET, list_item};

/// What failing to find an `Id` says.
const REMOVED: &str = "this Id refers to something that's been removed";
const ANOTHER: &str = "this Id is from another Table";

/// A slot removed at the last generation an `Id` can hold, `-1 - u32::MAX`, which is never used
/// again.
const RETIRED: i64 = -4_294_967_296;

/// An `Id`'s slot, generation and tag, loaded.
type IdParts = (String, String, String);

/// Where a `Table`'s parts are, from its fields' start, and how its items are held.
struct Parts {
    items: u32,
    generations: u32,
    live: u32,
    free_from: u32,
    tag: u32,
    held: Kind,
}

impl Emit<'_, '_, '_> {
    /// Whether a kind is a `Table`'s.
    pub(crate) fn is_id_map(&self, kind: &Kind) -> bool {
        let Kind::Boxed(id) = kind else { return false };
        self.module.layouts.get(*id).is_some_and(|layout| layout.name.starts_with("Table["))
    }

    fn parts(&self, kind: &Kind) -> Result<Parts, String> {
        let (items, items_kind) = self.field(kind, "items")?;
        let (generations, _) = self.field(kind, "generations")?;
        let (live, _) = self.field(kind, "live")?;
        let (free_from, _) = self.field(kind, "free_from")?;
        let (tag, _) = self.field(kind, "tag")?;
        let held = self.item_kind(list_item(&items_kind)?)?;
        Ok(Parts { items, generations, live, free_from, tag, held })
    }

    /// An `Id`'s slot, generation and tag.
    fn id_parts(&mut self, id: &Operand) -> Result<IdParts, String> {
        let (at, _) = self.operand(id)?;
        let (key, slot, generation, tag) = (self.temp(), self.temp(), self.temp(), self.temp());
        let tag_at = self.offset(&at, 8);
        self.line(&format!("{key} = load i64, ptr {at}"));
        self.line(&format!("{slot} = lshr i64 {key}, 32"));
        self.line(&format!("{generation} = and i64 {key}, 4294967295"));
        self.line(&format!("{tag} = load i64, ptr {tag_at}"));
        Ok((slot, generation, tag))
    }

    /// An `Id`'s key: its slot in the high 32 bits, and its generation in the low 32.
    fn id_key_of(&mut self, id: &Operand) -> Result<String, String> {
        let (at, _) = self.operand(id)?;
        let key = self.temp();
        self.line(&format!("{key} = load i64, ptr {at}"));
        Ok(key)
    }

    /// Writes an `Id` where `base` is.
    fn write_id(&mut self, base: &str, (slot, generation, tag): (&str, &str, &str)) {
        let (high, key) = (self.temp(), self.temp());
        self.line(&format!("{high} = shl i64 {slot}, 32"));
        self.line(&format!("{key} = or i64 {high}, {generation}"));
        let tag_at = self.offset(base, 8);
        self.line(&format!("store i64 {key}, ptr {base}"));
        self.line(&format!("store i64 {tag}, ptr {tag_at}"));
    }

    /// A `Table`'s tag, from its block.
    fn tag_of(&mut self, block: &str, parts: &Parts) -> String {
        let fields = self.offset(block, FIELDS_OFFSET);
        let at = self.offset(&fields, parts.tag);
        let tag = self.temp();
        self.line(&format!("{tag} = load i64, ptr {at}"));
        tag
    }

    /// Whether an `Id` is from the `Table` whose block this is.
    fn same_table(&mut self, block: &str, parts: &Parts, id: &IdParts) -> String {
        let tag = self.tag_of(block, parts);
        let same = self.temp();
        self.line(&format!("{same} = icmp eq i64 {}, {tag}", id.2));
        same
    }

    /// Ends the program because `id` finds nothing in the `Table` whose block this is, saying
    /// whether it's from another `Table` or its item has been removed.
    fn stop_missing(&mut self, block: &str, parts: &Parts, id: &IdParts, span: Span) {
        let same = self.same_table(block, parts, id);
        let (removed, another) = (self.literal(REMOVED), self.literal(ANOTHER));
        let text = self.temp();
        self.line(&format!("{text} = select i1 {same}, ptr {removed}, ptr {another}"));
        self.stop_with(&text, span);
    }

    /// The address of the list at `offset` into a `Table`'s block, and the list.
    fn part(&mut self, block: &str, offset: u32, alone: bool) -> (String, String) {
        let fields = self.offset(block, FIELDS_OFFSET);
        let place = self.offset(&fields, offset);
        let list = if alone {
            self.alone_at(&place, "fennec_list_unshare")
        } else {
            let list = self.temp();
            self.line(&format!("{list} = load ptr, ptr {place}"));
            list
        };
        (place, list)
    }

    /// The `Table`'s block at `place`, for an item of it to be changed: its own if nothing else has
    /// it, or a copy. In a function that asks a lock's log first, one the log marked stays where it
    /// is, its own fields saved when it was marked, where making it alone would copy it.
    fn table_to_change(&mut self, place: &str) -> String {
        if self.asks_the_log() { self.changeable_at(place) } else { self.alone_at(place, "fennec_unshare") }
    }

    /// The list at `offset` into a `Table`'s block, for its item at `slot` to be written: with no
    /// other owner, or, in a function that asks a lock's log first, one the log marked where it
    /// is, the item noted as it was for a crash or a failure to put back. Making a marked list
    /// alone would copy every item of the `Table` for one write.
    fn list_for_item(&mut self, block: &str, offset: u32, slot: &str) -> String {
        if !self.asks_the_log() {
            return self.part(block, offset, true).1;
        }
        let fields = self.offset(block, FIELDS_OFFSET);
        let place = self.offset(&fields, offset);
        self.item_to_set(&place, slot)
    }

    /// Whether an `Id` still finds its item: from this `Table`, inside its slots, with the slot's
    /// generation.
    fn finds(&mut self, block: &str, parts: &Parts, id: &IdParts) -> String {
        let same = self.same_table(block, parts, id);
        let (_, generations) = self.part(block, parts.generations, false);
        let (slot, generation) = (&id.0, &id.1);
        let length = self.list_length(&generations);
        let (within, inside) = (self.temp(), self.temp());
        self.line(&format!("{within} = icmp ult i64 {slot}, {length}"));
        self.line(&format!("{inside} = and i1 {same}, {within}"));
        let (check, outside, joined) = (self.label("check"), self.label("outside"), self.label("found"));
        let before = self.current.clone();
        self.line(&format!("br i1 {inside}, label %{check}, label %{outside}"));
        self.start_block(&check);
        let at = self.item_address(&generations, "i64", slot);
        let (held, same) = (self.temp(), self.temp());
        self.line(&format!("{held} = load i64, ptr {at}{ITEM_ACCESS}"));
        self.line(&format!("{same} = icmp eq i64 {held}, {generation}"));
        self.line(&format!("br label %{joined}"));
        self.start_block(&outside);
        self.line(&format!("br label %{joined}"));
        self.start_block(&joined);
        let found = self.temp();
        self.line(&format!("{found} = phi i1 [ {same}, %{check} ], [ false, %{outside} ]"));
        let _ = before;
        found
    }

    /// Adds a value, already owned, to the end of the list at `place`.
    fn push(&mut self, place: &str, held: &Kind, value: &str) {
        self.declare("declare ptr @fennec_list_grow(ptr) nounwind");
        let (list, length, room, fits, ready, longer) =
            (self.temp(), self.temp(), self.temp(), self.temp(), self.temp(), self.temp());
        self.line(&format!("{list} = load ptr, ptr {place}"));
        let (length_at, room_at) = (self.offset(&list, 16), self.offset(&list, 24));
        self.load_length(&length, &length_at);
        self.line(&format!("{room} = load i64, ptr {room_at}{HEADER_ACCESS}"));
        let alone = self.alone(&list);
        self.line(&format!("{fits} = icmp ult i64 {length}, {room}"));
        self.line(&format!("{ready} = and i1 {alone}, {fits}"));
        let (quick, grow, done) = (self.label("quick"), self.label("grow"), self.label("pushed"));
        self.line(&format!("br i1 {ready}, label %{quick}, label %{grow}"));
        self.start_block(&grow);
        let grown = self.temp();
        self.line(&format!("{grown} = call ptr @fennec_list_grow(ptr {list})"));
        self.line(&format!("store ptr {grown}, ptr {place}"));
        self.line(&format!("br label %{done}"));
        self.start_block(&quick);
        self.line(&format!("{longer} = add i64 {length}, 1"));
        self.line(&format!("store i64 {longer}, ptr {length_at}{HEADER_ACCESS}"));
        self.line(&format!("br label %{done}"));
        self.start_block(&done);
        let resolved = self.temp();
        self.line(&format!("{resolved} = phi ptr [ {list}, %{quick} ], [ {grown}, %{grow} ]"));
        let memory = held.memory(self.module.layouts);
        let at = self.item_address(&resolved, &memory, &length);
        self.store_field(held, value, &at, ITEM_ACCESS);
    }

    /// Writes an optional where `dest` is: `Some` of what's at `at`, given an owner unless it
    /// already has one.
    fn some_from(&mut self, dest: LocalId, held: &Kind, at: &str, owned: bool) -> Result<(), String> {
        let optional = self.kind_of(dest)?.clone();
        // An item in place is copied from where it is.
        let value = if held.in_memory() { at.to_owned() } else { self.load_field(held, at, ITEM_ACCESS) };
        if held.counted() && !owned {
            self.retain_value(&value, held);
        }
        if !owned {
            self.retain_inline(held, at);
        }
        if matches!(optional, Kind::Maybe(_) | Kind::MaybeText) {
            return self.assign(dest, &value);
        }
        let (number, fields) = self.case_of(&optional, "Some")?;
        let (_, offset, _) = fields.first().cloned().ok_or("an optional with nothing in it")?;
        let base = if let Kind::Boxed(id) = optional {
            let block = self.new_block(id)?;
            self.assign(dest, &block)?;
            self.offset(&block, FIELDS_OFFSET)
        } else {
            self.release_held(dest)?;
            let (slot, memory) = (self.slot(dest), optional.memory(self.module.layouts));
            self.line(&format!("store {memory} zeroinitializer, ptr {slot}"));
            slot
        };
        self.line(&format!("store i64 {number}, ptr {base}"));
        let place = self.offset(&base, offset);
        self.store_field(held, &value, &place, "");
        Ok(())
    }

    /// Clears an item's place once its value has gone elsewhere.
    fn clear_item(&mut self, held: &Kind, at: &str) {
        let memory = held.memory(self.module.layouts);
        self.line(&format!("store {memory} zeroinitializer, ptr {at}{ITEM_ACCESS}"));
    }

    /// Removes the items a test picks, or all of them with no test.
    fn id_map_remove(&mut self, map: LocalId, parts: &Parts, test: Option<(&str, &Kind)>, span: Span) {
        let held = parts.held.clone();
        let memory = held.memory(self.module.layouts);
        let place = self.slot(map);
        let block = self.alone_at(&place, "fennec_unshare");
        let (_, generations) = self.part(&block, parts.generations, true);
        let (_, items) = self.part(&block, parts.items, true);
        let length = self.list_length(&generations);
        let (at, end) = self.loop_begin(&length);
        let generation_at = self.item_address(&generations, "i64", &at);
        let (generation, is_live) = (self.temp(), self.temp());
        self.line(&format!("{generation} = load i64, ptr {generation_at}{ITEM_ACCESS}"));
        self.line(&format!("{is_live} = icmp sge i64 {generation}, 0"));
        let (live, take, next) = (self.label("live"), self.label("take"), self.label("next"));
        self.line(&format!("br i1 {is_live}, label %{live}, label %{next}"));
        self.start_block(&live);
        let item_at = self.item_address(&items, &memory, &at);
        let value =
            if held.in_memory() { item_at.clone() } else { self.load_field(&held, &item_at, ITEM_ACCESS) };
        match test {
            Some((closure, returns)) => {
                let text = if held.in_memory() { "ptr".to_owned() } else { self.text_of(&held) };
                let gone = self.call_closure_with(closure, returns, &[format!("{text} {value}")], span);
                self.line(&format!("br i1 {gone}, label %{take}, label %{next}"));
            }
            None => self.line(&format!("br label %{take}")),
        }
        self.start_block(&take);
        if held.in_memory() {
            self.release_inline(&held, &item_at);
        } else if held.counted() {
            self.release_kind(&value, &held);
        }
        self.clear_item(&held, &item_at);
        let removed = self.temp();
        self.line(&format!("{removed} = sub i64 -1, {generation}"));
        self.line(&format!("store i64 {removed}, ptr {generation_at}{ITEM_ACCESS}"));
        let fields = self.offset(&block, FIELDS_OFFSET);
        let live_at = self.offset(&fields, parts.live);
        let (count, one_fewer) = (self.temp(), self.temp());
        self.line(&format!("{count} = load i64, ptr {live_at}"));
        self.line(&format!("{one_fewer} = add i64 {count}, -1"));
        self.line(&format!("store i64 {one_fewer}, ptr {live_at}"));
        self.free_slot(&fields, parts, &at);
        self.line(&format!("br label %{next}"));
        self.start_block(&next);
        self.loop_end(&end);
    }

    /// There's a free slot at `at` now, so nothing above it may be used first.
    fn free_slot(&mut self, fields: &str, parts: &Parts, at: &str) {
        let free_at = self.offset(fields, parts.free_from);
        let (first, lower, lowest) = (self.temp(), self.temp(), self.temp());
        self.line(&format!("{first} = load i64, ptr {free_at}"));
        self.line(&format!("{lower} = icmp slt i64 {at}, {first}"));
        self.line(&format!("{lowest} = select i1 {lower}, i64 {at}, i64 {first}"));
        self.line(&format!("store i64 {lowest}, ptr {free_at}"));
    }

    pub(crate) fn id_map_builtin(
        &mut self,
        dest: Option<LocalId>,
        name: &str,
        args: &[Operand],
        span: Span,
    ) -> Result<(), String> {
        if name == "Table" {
            return self.new_id_map(dest.ok_or("no result")?);
        }
        let map = args.first().ok_or("no argument")?;
        let Operand::Local(map) = map else { return Err("a Table that isn't a local".to_owned()) };
        let kind = self.kind_of(*map)?.clone();
        let parts = self.parts(&kind)?;
        let held = parts.held.clone();
        let memory = held.memory(self.module.layouts);
        match name {
            "Table.len" => {
                let (block, _) = self.operand(&Operand::Local(*map))?;
                let fields = self.offset(&block, FIELDS_OFFSET);
                let at = self.offset(&fields, parts.live);
                let live = self.temp();
                self.line(&format!("{live} = load i64, ptr {at}"));
                self.assign(dest.ok_or("no result")?, &live)
            }
            "Table.contains" => {
                let id = self.id_parts(args.get(1).ok_or("no argument")?)?;
                let (block, _) = self.operand(&Operand::Local(*map))?;
                let found = self.finds(&block, &parts, &id);
                self.assign(dest.ok_or("no result")?, &found)
            }
            // A loop going through the slots in place: how many there are, whether one has an
            // item, the item, and its `Id`. The loop keeps inside the slots.
            "Table.slots" => {
                let (block, _) = self.operand(&Operand::Local(*map))?;
                let (_, generations) = self.part(&block, parts.generations, false);
                let count = self.list_length(&generations);
                self.assign(dest.ok_or("no result")?, &count)
            }
            "Table.slot_live" | "Table.slot_id" => {
                let (at, _) = self.operand(args.get(1).ok_or("no slot")?)?;
                let (block, _) = self.operand(&Operand::Local(*map))?;
                let (_, generations) = self.part(&block, parts.generations, false);
                let word_at = self.item_address(&generations, "i64", &at);
                let word = self.temp();
                self.line(&format!("{word} = load i64, ptr {word_at}{ITEM_ACCESS}"));
                if name == "Table.slot_live" {
                    let live = self.temp();
                    self.line(&format!("{live} = icmp sge i64 {word}, 0"));
                    return self.assign(dest.ok_or("no result")?, &live);
                }
                let tag = self.tag_of(&block, &parts);
                let place = self.slot(dest.ok_or("no result")?);
                self.write_id(&place, (&at, &word, &tag));
                Ok(())
            }
            "Table.at_slot" => {
                let dest = dest.ok_or("no result")?;
                let (at, _) = self.operand(args.get(1).ok_or("no slot")?)?;
                // An item taken out to change in place comes from a `Table` that's alone, so the
                // change can't show through another copy of it.
                let (block, alone) = match self.known.slot_home(dest).cloned() {
                    Some((root, path)) => {
                        let place = self.place_of(root, &path)?;
                        (self.table_to_change(&place), true)
                    }
                    None => (self.operand(&Operand::Local(*map))?.0, false),
                };
                let items = if alone {
                    self.list_for_item(&block, parts.items, &at)
                } else {
                    self.part(&block, parts.items, false).1
                };
                let place = self.item_address(&items, &memory, &at);
                self.read_into(dest, &held, &place, ITEM_ACCESS)
            }
            "Table.get" => {
                let dest = dest.ok_or("no result")?;
                let id = self.id_parts(args.get(1).ok_or("no argument")?)?;
                let (block, _) = self.operand(&Operand::Local(*map))?;
                let found = self.finds(&block, &parts, &id);
                let (yes, no, done) = (self.label("got"), self.label("none"), self.label("gotten"));
                self.line(&format!("br i1 {found}, label %{yes}, label %{no}"));
                self.start_block(&yes);
                let (_, items) = self.part(&block, parts.items, false);
                let at = self.item_address(&items, &memory, &id.0);
                self.some_from(dest, &held, &at, false)?;
                self.line(&format!("br label %{done}"));
                self.start_block(&no);
                self.copy(dest, &Operand::None)?;
                self.line(&format!("br label %{done}"));
                self.start_block(&done);
                Ok(())
            }
            "Table.add" => {
                let given = args.get(1).ok_or("no argument")?;
                let (value, fresh) = self.value_for(given, &held)?;
                if held.counted() && !fresh {
                    self.retain_value(&value, &held);
                }
                // An item in place in a local read only here moves its blocks over, as one made
                // for a list or a struct does (`fill`): the local is left holding nothing, so no
                // block is shared between it and the map. Any other item's blocks get the owner
                // the map's copy is.
                let moved = !fresh
                    && matches!(given, Operand::Local(local) if self.owns_inline(*local) && self.known.moves(*local));
                if !fresh && !moved {
                    self.retain_inline(&held, &value);
                }
                let place = self.slot(*map);
                // Where a lock's log is asked first, neither list is made alone here, which would
                // copy one the log marked: a slot used again is asked for by itself, and an item
                // more on the end asks as any list's does.
                let asks = self.asks_the_log();
                let block = self.table_to_change(&place);
                let (generations_place, generations) = self.part(&block, parts.generations, !asks);
                let (items_place, items) = self.part(&block, parts.items, !asks);
                let length = self.list_length(&generations);
                // The first removed slot, if there is one, from where there could be one.
                let fields = self.offset(&block, FIELDS_OFFSET);
                let free_at = self.offset(&fields, parts.free_from);
                let first = self.temp();
                self.line(&format!("{first} = load i64, ptr {free_at}"));
                let before = self.current.clone();
                let (head, body, free, next, reuse, append, done) = (
                    self.label("look"),
                    self.label("slot"),
                    self.label("free"),
                    self.label("next"),
                    self.label("reuse"),
                    self.label("append"),
                    self.label("inserted"),
                );
                let (at, more, step) = (self.temp(), self.temp(), self.temp());
                self.line(&format!("br label %{head}"));
                self.start_block(&head);
                self.line(&format!("{at} = phi i64 [ {first}, %{before} ], [ {step}, %{next} ]"));
                self.line(&format!("{more} = icmp slt i64 {at}, {length}"));
                self.line(&format!("br i1 {more}, label %{body}, label %{append}"));
                self.start_block(&body);
                let generation_at = self.item_address(&generations, "i64", &at);
                let (stored, removed, spent) = (self.temp(), self.temp(), self.temp());
                self.line(&format!("{stored} = load i64, ptr {generation_at}{ITEM_ACCESS}"));
                self.line(&format!("{removed} = icmp slt i64 {stored}, 0"));
                self.line(&format!("br i1 {removed}, label %{free}, label %{next}"));
                // Free, and not retired: removed at a generation below the last there is. Only a
                // free slot is asked, so the slots passed on the way cost nothing more.
                self.start_block(&free);
                self.line(&format!("{spent} = icmp eq i64 {stored}, {RETIRED}"));
                self.line(&format!("br i1 {spent}, label %{next}, label %{reuse}, !prof !0"));
                self.start_block(&next);
                self.line(&format!("{step} = add i64 {at}, 1"));
                self.line(&format!("br label %{head}"));
                self.start_block(&reuse);
                let above = self.temp();
                self.line(&format!("{above} = add i64 {at}, 1"));
                self.line(&format!("store i64 {above}, ptr {free_at}"));
                let again = self.temp();
                self.line(&format!("{again} = sub i64 0, {stored}"));
                let (generation_at, items) = if asks {
                    let generations = self.list_for_item(&block, parts.generations, &at);
                    let items = self.list_for_item(&block, parts.items, &at);
                    (self.item_address(&generations, "i64", &at), items)
                } else {
                    (generation_at, items)
                };
                self.line(&format!("store i64 {again}, ptr {generation_at}{ITEM_ACCESS}"));
                let item_at = self.item_address(&items, &memory, &at);
                self.store_field(&held, &value, &item_at, ITEM_ACCESS);
                // Asking for the slot's lists may have started other blocks.
                let reused = self.current.clone();
                self.line(&format!("br label %{done}"));
                self.start_block(&append);
                let past = self.temp();
                self.line(&format!("{past} = add i64 {length}, 1"));
                self.line(&format!("store i64 {past}, ptr {free_at}"));
                self.push(&items_place, &held, &value);
                self.push(&generations_place, &Kind::Int, "0");
                let appended = self.current.clone();
                self.line(&format!("br label %{done}"));
                self.start_block(&done);
                let (slot, generation) = (self.temp(), self.temp());
                self.line(&format!("{slot} = phi i64 [ {at}, %{reused} ], [ {length}, %{appended} ]"));
                self.line(&format!("{generation} = phi i64 [ {again}, %{reused} ], [ 0, %{appended} ]"));
                let fields = self.offset(&block, FIELDS_OFFSET);
                let live_at = self.offset(&fields, parts.live);
                let (live, one_more) = (self.temp(), self.temp());
                self.line(&format!("{live} = load i64, ptr {live_at}"));
                self.line(&format!("{one_more} = add i64 {live}, 1"));
                self.line(&format!("store i64 {one_more}, ptr {live_at}"));
                if moved {
                    let zero = self.zero_line(&held, &value);
                    self.line(&zero);
                }
                if let Some(dest) = dest {
                    let tag = self.tag_of(&block, &parts);
                    let at = self.slot(dest);
                    self.write_id(&at, (&slot, &generation, &tag));
                }
                Ok(())
            }
            "Table.remove" => {
                let id = self.id_parts(args.get(1).ok_or("no argument")?)?;
                let place = self.slot(*map);
                let block = self.table_to_change(&place);
                // Where a lock's log is asked, the slot's generation is asked for once it's found.
                let early = (!self.asks_the_log()).then(|| self.part(&block, parts.generations, true).1);
                let found = self.finds(&block, &parts, &id);
                let (yes, no, done) = (self.label("take"), self.label("none"), self.label("removed"));
                self.line(&format!("br i1 {found}, label %{yes}, label %{no}"));
                self.start_block(&yes);
                let generations = match early {
                    Some(generations) => generations,
                    None => self.list_for_item(&block, parts.generations, &id.0),
                };
                let items = self.list_for_item(&block, parts.items, &id.0);
                let at = self.item_address(&items, &memory, &id.0);
                if let Some(dest) = dest {
                    self.some_from(dest, &held, &at, true)?;
                } else if held.in_memory() {
                    self.release_inline(&held, &at);
                } else {
                    let value = self.load_field(&held, &at, ITEM_ACCESS);
                    if held.counted() {
                        self.release_kind(&value, &held);
                    }
                }
                self.clear_item(&held, &at);
                let generation_at = self.item_address(&generations, "i64", &id.0);
                let gone = self.temp();
                self.line(&format!("{gone} = sub i64 -1, {}", id.1));
                self.line(&format!("store i64 {gone}, ptr {generation_at}{ITEM_ACCESS}"));
                let fields = self.offset(&block, FIELDS_OFFSET);
                let live_at = self.offset(&fields, parts.live);
                let (live, one_fewer) = (self.temp(), self.temp());
                self.line(&format!("{live} = load i64, ptr {live_at}"));
                self.line(&format!("{one_fewer} = add i64 {live}, -1"));
                self.line(&format!("store i64 {one_fewer}, ptr {live_at}"));
                self.free_slot(&fields, &parts, &id.0);
                self.line(&format!("br label %{done}"));
                self.start_block(&no);
                if let Some(dest) = dest {
                    self.copy(dest, &Operand::None)?;
                }
                self.line(&format!("br label %{done}"));
                self.start_block(&done);
                Ok(())
            }
            "Table.remove_where" => {
                let (closure, closure_kind) = self.operand(args.get(1).ok_or("no argument")?)?;
                let Kind::Closure(_, returns) = closure_kind else {
                    return Err("a test that isn't a closure".to_owned());
                };
                self.id_map_remove(*map, &parts, Some((&closure, &returns)), span);
                Ok(())
            }
            "Table.modify" => {
                let id = self.id_parts(args.get(1).ok_or("no argument")?)?;
                let (closure, closure_kind) = self.operand(args.get(2).ok_or("no argument")?)?;
                let Kind::Closure(_, returns) = closure_kind else {
                    return Err("a change that isn't a closure".to_owned());
                };
                if held == Kind::Bool {
                    return Err(
                        "it changes a bool in a `Table`, which this backend doesn't take yet".to_owned()
                    );
                }
                let place = self.slot(*map);
                let block = self.table_to_change(&place);
                let found = self.finds(&block, &parts, &id);
                let (yes, done) = (self.label("change"), self.label("changed"));
                self.line(&format!("br i1 {found}, label %{yes}, label %{done}"));
                self.start_block(&yes);
                let items = self.list_for_item(&block, parts.items, &id.0);
                let at = self.item_address(&items, &memory, &id.0);
                let given = self.call_closure_with(&closure, &returns, &[format!("ptr {at}")], span);
                if returns.in_memory() {
                    self.release_inline(&returns, &given);
                } else if returns.counted() {
                    self.release_kind(&given, &returns);
                }
                self.line(&format!("br label %{done}"));
                self.start_block(&done);
                match dest {
                    Some(dest) => self.assign(dest, &found),
                    None => Ok(()),
                }
            }
            "Table.clear" => {
                self.id_map_remove(*map, &parts, None, span);
                Ok(())
            }
            // Only gone through in order, it's never made: the loop walks the slots.
            "Table.values" | "Table.items"
                if dest.is_some_and(|dest| self.known.map_walks.contains_key(&dest.0)) =>
            {
                Ok(())
            }
            "Table.values" | "Table.ids" | "Table.items" => {
                self.id_map_list(dest.ok_or("no result")?, *map, &parts, name)
            }
            other => {
                let _ = span;
                Err(format!("it calls `{other}`, which this backend doesn't take yet"))
            }
        }
    }

    /// `std.intrinsics`' ways into an `Id`, which std uses to save and load them: a number for
    /// one, the `Id` of an item in a `Table` nothing's been taken out of, and the tags that say
    /// which `Table` each is, with a new `Table` of a tag a load gives it.
    pub(crate) fn id_intrinsic(
        &mut self,
        dest: Option<LocalId>,
        name: &str,
        args: &[Operand],
    ) -> Result<(), String> {
        let dest = dest.ok_or("no result")?;
        if name == "intrinsics.new_tag" {
            let tag = self.new_tag();
            return self.assign(dest, &tag);
        }
        let given = args.first().ok_or("no argument")?;
        match name {
            "intrinsics.id_key" => {
                let key = self.id_key_of(given)?;
                self.assign(dest, &key)
            }
            "intrinsics.id_at" => {
                let (position, _) = self.operand(given)?;
                let (tag, _) = self.operand(args.get(1).ok_or("no tag")?)?;
                let at = self.slot(dest);
                self.write_id(&at, (&position, "0", &tag));
                Ok(())
            }
            "intrinsics.id_tag" => {
                let (_, _, tag) = self.id_parts(given)?;
                self.assign(dest, &tag)
            }
            "intrinsics.table_tag" => {
                let (block, kind) = self.operand(given)?;
                let parts = self.parts(&kind)?;
                let tag = self.tag_of(&block, &parts);
                self.assign(dest, &tag)
            }
            "intrinsics.id_of_key" => {
                let (key, _) = self.operand(given)?;
                let (tag, _) = self.operand(args.get(1).ok_or("no tag")?)?;
                let at = self.slot(dest);
                let tag_at = self.offset(&at, 8);
                self.line(&format!("store i64 {key}, ptr {at}"));
                self.line(&format!("store i64 {tag}, ptr {tag_at}"));
                Ok(())
            }
            // The generations a `Table` holds are its slots as `table_slots` gives them: the
            // same list, shared until one of them changes it.
            "intrinsics.table_slots" => {
                let (block, kind) = self.operand(given)?;
                let parts = self.parts(&kind)?;
                let (_, generations) = self.part(&block, parts.generations, false);
                let list = self.kind_of(dest)?.clone();
                self.retain_value(&generations, &list);
                self.assign(dest, &generations)
            }
            "intrinsics.table_of_slots" => self.table_of_slots(dest, args),
            "intrinsics.table_tagged" => {
                let (tag, _) = self.operand(given)?;
                self.new_id_map_tagged(dest, Some(&tag))
            }
            other => Err(format!("it calls `{other}`, which this backend doesn't take yet")),
        }
    }

    /// A new `Table` of a tag, with each slot as `table_slots` gives it and the items in the
    /// slots in use, in order. A load has checked there are as many items as slots in use.
    fn table_of_slots(&mut self, dest: LocalId, args: &[Operand]) -> Result<(), String> {
        let (tag, _) = self.operand(args.first().ok_or("no tag")?)?;
        let (slots, slots_kind) = self.operand(args.get(1).ok_or("no slots")?)?;
        let (items, _) = self.operand(args.get(2).ok_or("no items")?)?;
        let kind = self.kind_of(dest)?.clone();
        let Kind::Boxed(id) = kind else { return Err("a Table that isn't boxed".to_owned()) };
        let parts = self.parts(&kind)?;
        let held = parts.held.clone();
        let memory = held.memory(self.module.layouts);
        let item = list_item(&self.field(&kind, "items")?.1)?;
        let count = self.list_length(&slots);
        let out = self.new_list(item, &count);
        let before = self.current.clone();
        let (head, body, copy, next, done) = (
            self.label("slots"),
            self.label("slot"),
            self.label("live"),
            self.label("slot_next"),
            self.label("slotted"),
        );
        let (at, taken, more, step, taken_after, taken_next) =
            (self.temp(), self.temp(), self.temp(), self.temp(), self.temp(), self.temp());
        self.line(&format!("br label %{head}"));
        self.start_block(&head);
        self.line(&format!("{at} = phi i64 [ 0, %{before} ], [ {step}, %{next} ]"));
        self.line(&format!("{taken} = phi i64 [ 0, %{before} ], [ {taken_next}, %{next} ]"));
        self.line(&format!("{more} = icmp slt i64 {at}, {count}"));
        self.line(&format!("br i1 {more}, label %{body}, label %{done}"));
        self.start_block(&body);
        let word_at = self.item_address(&slots, "i64", &at);
        let (word, live) = (self.temp(), self.temp());
        self.line(&format!("{word} = load i64, ptr {word_at}{ITEM_ACCESS}"));
        self.line(&format!("{live} = icmp sge i64 {word}, 0"));
        self.line(&format!("br i1 {live}, label %{copy}, label %{next}"));
        self.start_block(&copy);
        let from = self.item_address(&items, &memory, &taken);
        let to = self.item_address(&out, &memory, &at);
        if held.in_memory() {
            self.store_field(&held, &from, &to, ITEM_ACCESS);
            self.retain_inline(&held, &to);
        } else {
            let value = self.load_field(&held, &from, ITEM_ACCESS);
            if held.counted() {
                self.retain_value(&value, &held);
            }
            self.store_field(&held, &value, &to, ITEM_ACCESS);
        }
        self.line(&format!("{taken_after} = add i64 {taken}, 1"));
        let copied = self.current.clone();
        self.line(&format!("br label %{next}"));
        self.start_block(&next);
        self.line(&format!("{taken_next} = phi i64 [ {taken_after}, %{copied} ], [ {taken}, %{body} ]"));
        self.line(&format!("{step} = add i64 {at}, 1"));
        self.line(&format!("br label %{head}"));
        self.start_block(&done);
        let block = self.new_block(id)?;
        let fields = self.offset(&block, FIELDS_OFFSET);
        self.retain_value(&slots, &slots_kind);
        for (offset, list) in [(parts.items, &out), (parts.generations, &slots)] {
            let place = self.offset(&fields, offset);
            self.line(&format!("store ptr {list}, ptr {place}"));
        }
        let (live_at, tag_at) = (self.offset(&fields, parts.live), self.offset(&fields, parts.tag));
        self.line(&format!("store i64 {taken}, ptr {live_at}"));
        self.line(&format!("store i64 {tag}, ptr {tag_at}"));
        self.assign(dest, &block)
    }

    /// A tag no `Table` has had, from the runtime's one count.
    fn new_tag(&mut self) -> String {
        self.declare("declare i64 @fennec_new_tag() nounwind");
        let tag = self.temp();
        self.line(&format!("{tag} = call i64 @fennec_new_tag()"));
        tag
    }

    fn new_id_map(&mut self, dest: LocalId) -> Result<(), String> {
        self.new_id_map_tagged(dest, None)
    }

    /// A new, empty `Table`, with `tag` or a new one.
    fn new_id_map_tagged(&mut self, dest: LocalId, tag: Option<&str>) -> Result<(), String> {
        let kind = self.kind_of(dest)?.clone();
        let Kind::Boxed(id) = kind else { return Err("a Table that isn't boxed".to_owned()) };
        let parts = self.parts(&kind)?;
        let item = list_item(&self.field(&kind, "items")?.1)?;
        let block = self.new_block(id)?;
        let fields = self.offset(&block, FIELDS_OFFSET);
        let items = self.new_list(item, "0");
        let generations = self.new_list(fennec_layout::ListItem::Int, "0");
        for (offset, list) in [(parts.items, &items), (parts.generations, &generations)] {
            let at = self.offset(&fields, offset);
            self.line(&format!("store ptr {list}, ptr {at}"));
        }
        let tag = match tag {
            Some(tag) => tag.to_owned(),
            None => self.new_tag(),
        };
        let tag_at = self.offset(&fields, parts.tag);
        self.line(&format!("store i64 {tag}, ptr {tag_at}"));
        self.assign(dest, &block)
    }

    /// How many items a map being walked has, which is how many times its loop goes round.
    pub(crate) fn walked_live(&mut self, map: LocalId) -> Result<String, String> {
        let kind = self.kind_of(map)?.clone();
        if matches!(kind, Kind::Dict(..)) {
            let (block, _) = self.operand(&Operand::Local(map))?;
            return Ok(self.dict_length(&block));
        }
        let parts = self.parts(&kind)?;
        let (block, _) = self.operand(&Operand::Local(map))?;
        let fields = self.offset(&block, FIELDS_OFFSET);
        let live_at = self.offset(&fields, parts.live);
        let live = self.temp();
        self.line(&format!("{live} = load i64, ptr {live_at}"));
        Ok(live)
    }

    /// Goes on to the next slot with an item in it. The loop counts the items, and nothing here
    /// changes the map, so there should always be one, and running off the end fails rather than
    /// trusting that. A count of 0 starts again, in case what was never made is gone through more
    /// than once.
    pub(crate) fn walk_to_next(
        &mut self,
        dest: LocalId,
        map: LocalId,
        index: &Operand,
        span: Span,
    ) -> Result<(), String> {
        let kind = self.kind_of(map)?.clone();
        let parts = self.parts(&kind)?;
        let (block, _) = self.operand(&Operand::Local(map))?;
        let (_, generations) = self.part(&block, parts.generations, false);
        let (next_place, slot_place, generation_place) =
            (self.place_for(&Kind::Int), self.place_for(&Kind::Int), self.place_for(&Kind::Int));
        let (count, _) = self.operand(index)?;
        let (saved, first, start) = (self.temp(), self.temp(), self.temp());
        self.line(&format!("{saved} = load i64, ptr {next_place}"));
        self.line(&format!("{first} = icmp eq i64 {count}, 0"));
        self.line(&format!("{start} = select i1 {first}, i64 0, i64 {saved}"));
        let before = self.current.clone();
        let (head, again, found) = (self.label("walk"), self.label("walk_on"), self.label("walked"));
        let (going, step) = (self.temp(), self.temp());
        self.line(&format!("br label %{head}"));
        self.start_block(&head);
        self.line(&format!("{going} = phi i64 [ {start}, %{before} ], [ {step}, %{again} ]"));
        let at = self.position(&generations, &going, span);
        let generation_at = self.item_address(&generations, "i64", &at);
        let (generation, alive) = (self.temp(), self.temp());
        self.line(&format!("{generation} = load i64, ptr {generation_at}{ITEM_ACCESS}"));
        self.line(&format!("{alive} = icmp sge i64 {generation}, 0"));
        self.line(&format!("{step} = add i64 {at}, 1"));
        self.line(&format!("br i1 {alive}, label %{found}, label %{again}"));
        self.start_block(&again);
        self.line(&format!("br label %{head}"));
        self.start_block(&found);
        self.line(&format!("store i64 {step}, ptr {next_place}"));
        self.line(&format!("store i64 {at}, ptr {slot_place}"));
        self.line(&format!("store i64 {generation}, ptr {generation_place}"));
        self.walks.insert(dest.0, (slot_place, generation_place));
        if self.known.walk_items.get(&dest.0).is_some_and(|(_, pair)| *pair) {
            // Holding nothing, letting go of it lets go of nothing.
            let (held, text) = (self.slot(dest), self.text_of(&self.kind_of(dest)?.clone()));
            let dest_kind = self.kind_of(dest)?.clone();
            if dest_kind.in_memory() || dest_kind.counted() {
                self.line(&format!("store {text} zeroinitializer, ptr {held}"));
            }
            return Ok(());
        }
        let (_, items) = self.part(&block, parts.items, false);
        let memory = parts.held.memory(self.module.layouts);
        let place = self.item_address(&items, &memory, &at);
        self.read_into(dest, &parts.held, &place, ITEM_ACCESS)
    }

    /// A part of the pair a walk is on: its `Id`, or its item.
    pub(crate) fn walked_part(&mut self, dest: LocalId, pair: LocalId, part: usize) -> Result<(), String> {
        let (map, _) = self.known.walk_items.get(&pair.0).copied().ok_or("a walk with no map")?;
        if matches!(self.kind_of(map)?, Kind::Dict(..)) {
            return self.dict_walked_part(dest, pair, map, part);
        }
        let (slot_place, generation_place) =
            self.walks.get(&pair.0).cloned().ok_or("a pair read before it's made")?;
        let (at, generation) = (self.temp(), self.temp());
        self.line(&format!("{at} = load i64, ptr {slot_place}"));
        self.line(&format!("{generation} = load i64, ptr {generation_place}"));
        let kind = self.kind_of(map)?.clone();
        let parts = self.parts(&kind)?;
        let (block, _) = self.operand(&Operand::Local(map))?;
        if part == 0 {
            let tag = self.tag_of(&block, &parts);
            let base = self.slot(dest);
            self.write_id(&base, (&at, &generation, &tag));
            return Ok(());
        }
        let (_, items) = self.part(&block, parts.items, false);
        let memory = parts.held.memory(self.module.layouts);
        let place = self.item_address(&items, &memory, &at);
        self.read_into(dest, &parts.held, &place, ITEM_ACCESS)
    }

    /// The live items, `Id`s, or both as pairs, in slot order.
    fn id_map_list(&mut self, dest: LocalId, map: LocalId, parts: &Parts, name: &str) -> Result<(), String> {
        let list_kind = self.kind_of(dest)?.clone();
        let item = list_item(&list_kind)?;
        let out_held = self.item_kind(item)?;
        let out_memory = out_held.memory(self.module.layouts);
        let held = parts.held.clone();
        let memory = held.memory(self.module.layouts);
        let (block, _) = self.operand(&Operand::Local(map))?;
        let fields = self.offset(&block, FIELDS_OFFSET);
        let live_at = self.offset(&fields, parts.live);
        let live = self.temp();
        self.line(&format!("{live} = load i64, ptr {live_at}"));
        let out = self.new_list(item, &live);
        let tag = self.tag_of(&block, parts);
        let (_, generations) = self.part(&block, parts.generations, false);
        let (_, items) = self.part(&block, parts.items, false);
        let length = self.list_length(&generations);
        let before = self.current.clone();
        let (head, body, keep, next, done) = (
            self.label("each"),
            self.label("slot"),
            self.label("keep"),
            self.label("next"),
            self.label("listed"),
        );
        let (at, written, more, step, written_after, written_next) =
            (self.temp(), self.temp(), self.temp(), self.temp(), self.temp(), self.temp());
        self.line(&format!("br label %{head}"));
        self.start_block(&head);
        self.line(&format!("{at} = phi i64 [ 0, %{before} ], [ {step}, %{next} ]"));
        self.line(&format!("{written} = phi i64 [ 0, %{before} ], [ {written_next}, %{next} ]"));
        self.line(&format!("{more} = icmp slt i64 {at}, {length}"));
        self.line(&format!("br i1 {more}, label %{body}, label %{done}"));
        self.start_block(&body);
        let generation_at = self.item_address(&generations, "i64", &at);
        let (generation, alive) = (self.temp(), self.temp());
        self.line(&format!("{generation} = load i64, ptr {generation_at}{ITEM_ACCESS}"));
        self.line(&format!("{alive} = icmp sge i64 {generation}, 0"));
        self.line(&format!("br i1 {alive}, label %{keep}, label %{next}"));
        self.start_block(&keep);
        let to = self.item_address(&out, &out_memory, &written);
        let from = self.item_address(&items, &memory, &at);
        let write_id = |emit: &mut Self, base: &str| emit.write_id(base, (&at, &generation, &tag));
        match name {
            "Table.values" if held.in_memory() => {
                self.store_field(&held, &from, &to, ITEM_ACCESS);
                self.retain_inline(&held, &to);
            }
            "Table.values" => {
                let value = self.load_field(&held, &from, ITEM_ACCESS);
                if held.counted() {
                    self.retain_value(&value, &held);
                }
                self.store_field(&held, &value, &to, ITEM_ACCESS);
            }
            "Table.ids" => write_id(self, &to),
            _ => {
                let (id_at, _) = self.field(&out_held, "_0")?;
                let (value_at, _) = self.field(&out_held, "_1")?;
                if let Kind::Struct(_) = out_held {
                    // A pair of an `Id` and the item, in place in the list.
                    let id_place = self.offset(&to, id_at);
                    write_id(self, &id_place);
                    let value_place = self.offset(&to, value_at);
                    if held.in_memory() {
                        self.store_field(&held, &from, &value_place, ITEM_ACCESS);
                        self.retain_inline(&held, &value_place);
                    } else {
                        let value = self.load_field(&held, &from, ITEM_ACCESS);
                        if held.counted() {
                            self.retain_value(&value, &held);
                        }
                        self.store_field(&held, &value, &value_place, ITEM_ACCESS);
                    }
                } else {
                    // A pair of an `Id` and the item, a block of its own when it holds a block.
                    let Kind::Boxed(pair) = out_held else {
                        return Err(format!(
                            "`Table.items` gives {out_held:?}, which this backend doesn't take yet"
                        ));
                    };
                    let made = self.new_block(pair)?;
                    let pair_fields = self.offset(&made, FIELDS_OFFSET);
                    let id_place = self.offset(&pair_fields, id_at);
                    write_id(self, &id_place);
                    let value = self.load_field(&held, &from, ITEM_ACCESS);
                    if held.counted() {
                        self.retain_value(&value, &held);
                    }
                    let value_place = self.offset(&pair_fields, value_at);
                    self.store_field(&held, &value, &value_place, "");
                    self.line(&format!("store ptr {made}, ptr {to}{ITEM_ACCESS}"));
                }
            }
        }
        self.line(&format!("{written_after} = add i64 {written}, 1"));
        let kept_end = self.current.clone();
        self.line(&format!("br label %{next}"));
        self.start_block(&next);
        self.line(&format!(
            "{written_next} = phi i64 [ {written_after}, %{kept_end} ], [ {written}, %{body} ]"
        ));
        self.line(&format!("{step} = add i64 {at}, 1"));
        self.line(&format!("br label %{head}"));
        self.start_block(&done);
        self.assign(dest, &out)
    }

    /// `map[id]`, failing when the `Id`'s item has been removed.
    pub(crate) fn id_map_index(
        &mut self,
        dest: LocalId,
        map: &Operand,
        id: &Operand,
        span: Span,
    ) -> Result<(), String> {
        let (block, kind) = self.operand(map)?;
        let parts = self.parts(&kind)?;
        let held = parts.held.clone();
        let memory = held.memory(self.module.layouts);
        // An item taken out to change and put back comes from a `Table` that's alone, so the
        // change can't show through another copy of it.
        let taken = self.known.is_taken(dest).then(|| self.known.item_home(dest).cloned()).flatten();
        let (block, alone) = match taken {
            Some((root, path)) => {
                let place = self.place_of(root, &path)?;
                (self.table_to_change(&place), true)
            }
            None => (block, false),
        };
        let id = self.id_parts(id)?;
        let found = self.finds(&block, &parts, &id);
        let (yes, no) = (self.label("indexed"), self.label("stale"));
        self.line(&format!("br i1 {found}, label %{yes}, label %{no}, !prof !8"));
        self.start_block(&no);
        self.stop_missing(&block, &parts, &id, span);
        self.start_block(&yes);
        let items = if alone {
            self.list_for_item(&block, parts.items, &id.0)
        } else {
            self.part(&block, parts.items, false).1
        };
        let at = self.item_address(&items, &memory, &id.0);
        self.read_into(dest, &held, &at, ITEM_ACCESS)
    }

    /// `root.table[id].field` or `table[id].field`, taken out to change and put back: the `Table`,
    /// its items and the item are each made alone first, so the field's one owner is the item it
    /// goes back to. The item was read at this `Id` just before, so it's still there; a build
    /// checks it anyway, as a read by `Id` does.
    pub(crate) fn take_from_table_item(
        &mut self,
        dest: LocalId,
        (root, path, id): (LocalId, &[String], &Operand),
        field: &str,
        span: Span,
    ) -> Result<(), String> {
        let place = self.place_of(root, path)?;
        let kind = self.place_kind(root, path)?;
        let parts = self.parts(&kind)?;
        let held = parts.held.clone();
        if !matches!(held, Kind::Boxed(_) | Kind::Struct(_)) {
            return Err(format!("it takes a field of a {held:?} item, which this backend doesn't take yet"));
        }
        let block = self.table_to_change(&place);
        let id = self.id_parts(id)?;
        let found = self.finds(&block, &parts, &id);
        let (yes, no) = (self.label("taken"), self.label("stale"));
        self.line(&format!("br i1 {found}, label %{yes}, label %{no}, !prof !8"));
        self.start_block(&no);
        self.stop_missing(&block, &parts, &id, span);
        self.start_block(&yes);
        let items = self.list_for_item(&block, parts.items, &id.0);
        let memory = held.memory(self.module.layouts);
        let at = self.item_address(&items, &memory, &id.0);
        let (offset, inner) = self.field(&held, field)?;
        // An item in place: its field is right there in the items, which are alone.
        if matches!(held, Kind::Struct(_)) {
            let at = self.offset(&at, offset);
            let value = self.load_field(&inner, &at, ITEM_ACCESS);
            return self.assign(dest, &value);
        }
        let item = self.alone_at(&at, "fennec_unshare");
        let fields = self.fields_at(&item, &held);
        let at = self.offset(&fields, offset);
        let value = self.load_field(&inner, &at, crate::emit::FIELD_ACCESS);
        self.assign(dest, &value)
    }

    /// `map[id] = value`, with the map made alone first.
    pub(crate) fn id_map_store(
        &mut self,
        place: &str,
        kind: &Kind,
        id: &Operand,
        value: &Operand,
        span: Span,
    ) -> Result<(), String> {
        let parts = self.parts(kind)?;
        let held = parts.held.clone();
        let memory = held.memory(self.module.layouts);
        // A `for ref` item taken out of its slot coming home: the slot still points at what was
        // taken, which the item owned meanwhile, so it's moved back with nothing let go of.
        let put_back = matches!(value, Operand::Local(local) if self.known.is_taken(*local));
        let value_operand = value;
        let (value, fresh) = self.value_for(value, &held)?;
        let id = self.id_parts(id)?;
        let block = self.table_to_change(place);
        let found = self.finds(&block, &parts, &id);
        let (yes, no) = (self.label("store"), self.label("stale"));
        self.line(&format!("br i1 {found}, label %{yes}, label %{no}, !prof !8"));
        self.start_block(&no);
        self.stop_missing(&block, &parts, &id, span);
        self.start_block(&yes);
        let items = self.list_for_item(&block, parts.items, &id.0);
        let at = self.item_address(&items, &memory, &id.0);
        if put_back && held.counted() {
            self.proof_goes_back(value_operand, &at);
            let text = self.text_of(&held);
            self.line(&format!("store {text} {value}, ptr {at}{ITEM_ACCESS}"));
        } else if put_back {
            self.proof_goes_back(value_operand, &at);
            self.store_field(&held, &value, &at, ITEM_ACCESS);
        } else if held.counted() {
            if !fresh {
                self.retain_value(&value, &held);
            }
            let (old, text) = (self.temp(), self.text_of(&held));
            self.line(&format!("{old} = load {text}, ptr {at}{ITEM_ACCESS}"));
            self.line(&format!("store {text} {value}, ptr {at}{ITEM_ACCESS}"));
            self.release_kind(&old, &held);
        } else {
            self.release_inline(&held, &at);
            if !fresh {
                self.retain_inline(&held, &value);
            }
            self.store_field(&held, &value, &at, ITEM_ACCESS);
        }
        Ok(())
    }
}
