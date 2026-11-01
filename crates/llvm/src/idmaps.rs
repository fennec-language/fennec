//! `IdMap`s and `Id`s, which the runtime doesn't hold natively. An `Id` is a plain struct of its
//! slot and generation. An `IdMap` is a boxed struct of its items, each slot's generation, and
//! how many are live: a removed slot's generation is stored as `-1 - generation`, and its item
//! is zero, so a stale `Id` never finds the wrong thing and a slot is used again with the next
//! generation, as the interpreter's are.

use fennec_ir::{LocalId, Operand};
use fennec_syntax::Span;

use crate::emit::{Emit, HEADER_ACCESS, ITEM_ACCESS, LENGTH_ACCESS};
use crate::kinds::Kind;
use crate::lists::{FIELDS_OFFSET, list_item};

/// What failing to find an `Id` says, as the interpreter does.
const REMOVED: &str = "this Id refers to something that's been removed";

/// Where an `IdMap`'s parts are, from its fields' start, and how its items are held.
struct Parts {
    items: u32,
    generations: u32,
    live: u32,
    held: Kind,
}

impl Emit<'_, '_, '_> {
    /// Whether a kind is an `IdMap`'s.
    pub(crate) fn is_id_map(&self, kind: &Kind) -> bool {
        let Kind::Boxed(id) = kind else { return false };
        self.module.layouts.get(*id).is_some_and(|layout| layout.name.starts_with("IdMap["))
    }

    fn parts(&self, kind: &Kind) -> Result<Parts, String> {
        let (items, items_kind) = self.field(kind, "items")?;
        let (generations, _) = self.field(kind, "generations")?;
        let (live, _) = self.field(kind, "live")?;
        let held = self.item_kind(list_item(&items_kind)?)?;
        Ok(Parts { items, generations, live, held })
    }

    /// An `Id`'s slot and generation.
    fn id_parts(&mut self, id: &Operand) -> Result<(String, String), String> {
        let (at, _) = self.operand(id)?;
        let (slot, generation) = (self.temp(), self.temp());
        let generation_at = self.offset(&at, 8);
        self.line(&format!("{slot} = load i64, ptr {at}"));
        self.line(&format!("{generation} = load i64, ptr {generation_at}"));
        Ok((slot, generation))
    }

    /// The address of the list at `offset` into an `IdMap`'s block, and the list.
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

    /// Whether an `Id` still finds its item: inside the slots, with the slot's generation.
    fn finds(&mut self, generations: &str, (slot, generation): (&str, &str)) -> String {
        let length = self.list_length(generations);
        let inside = self.temp();
        self.line(&format!("{inside} = icmp ult i64 {slot}, {length}"));
        let (check, outside, joined) = (self.label("check"), self.label("outside"), self.label("found"));
        let before = self.current.clone();
        self.line(&format!("br i1 {inside}, label %{check}, label %{outside}"));
        self.start_block(&check);
        let at = self.item_address(generations, "i64", slot);
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
        let (list, count, length, room, alone, fits, ready, longer) = (
            self.temp(),
            self.temp(),
            self.temp(),
            self.temp(),
            self.temp(),
            self.temp(),
            self.temp(),
            self.temp(),
        );
        self.line(&format!("{list} = load ptr, ptr {place}"));
        self.line(&format!("{count} = load i64, ptr {list}{HEADER_ACCESS}"));
        let (length_at, room_at) = (self.offset(&list, 16), self.offset(&list, 24));
        self.line(&format!("{length} = load i64, ptr {length_at}{LENGTH_ACCESS}"));
        self.line(&format!("{room} = load i64, ptr {room_at}{HEADER_ACCESS}"));
        self.line(&format!("{alone} = icmp eq i64 {count}, 1"));
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
        let value = self.load_field(held, at, ITEM_ACCESS);
        if held.counted() && !owned {
            self.retain_value(&value, held);
        }
        if matches!(optional, Kind::Maybe(_)) {
            return self.assign(dest, &value);
        }
        let (number, fields) = self.case_of(&optional, "Some")?;
        let (_, offset, _) = fields.first().cloned().ok_or("an optional with nothing in it")?;
        let base = if let Kind::Boxed(id) = optional {
            let block = self.new_block(id)?;
            self.assign(dest, &block)?;
            self.offset(&block, FIELDS_OFFSET)
        } else {
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

    pub(crate) fn id_map_builtin(
        &mut self,
        dest: Option<LocalId>,
        name: &str,
        args: &[Operand],
        span: Span,
    ) -> Result<(), String> {
        if name == "IdMap" {
            return self.new_id_map(dest.ok_or("no result")?);
        }
        let map = args.first().ok_or("no argument")?;
        let Operand::Local(map) = map else { return Err("an IdMap that isn't a local".to_owned()) };
        let kind = self.kind_of(*map)?.clone();
        let parts = self.parts(&kind)?;
        let held = parts.held.clone();
        let memory = held.memory(self.module.layouts);
        match name {
            "IdMap.len" => {
                let (block, _) = self.operand(&Operand::Local(*map))?;
                let fields = self.offset(&block, FIELDS_OFFSET);
                let at = self.offset(&fields, parts.live);
                let live = self.temp();
                self.line(&format!("{live} = load i64, ptr {at}"));
                self.assign(dest.ok_or("no result")?, &live)
            }
            "IdMap.get" => {
                let dest = dest.ok_or("no result")?;
                let id = self.id_parts(args.get(1).ok_or("no argument")?)?;
                let (block, _) = self.operand(&Operand::Local(*map))?;
                let (_, generations) = self.part(&block, parts.generations, false);
                let found = self.finds(&generations, (&id.0, &id.1));
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
            "IdMap.insert" => {
                let value = args.get(1).ok_or("no argument")?;
                let (value, fresh) = self.value_for(value, &held)?;
                if held.counted() && !fresh {
                    self.retain_value(&value, &held);
                }
                let place = self.slot(*map);
                let block = self.alone_at(&place, "fennec_unshare");
                let (generations_place, generations) = self.part(&block, parts.generations, true);
                let (items_place, items) = self.part(&block, parts.items, true);
                let length = self.list_length(&generations);
                // The first removed slot, if there is one.
                let before = self.current.clone();
                let (head, body, next, reuse, append, done) = (
                    self.label("look"),
                    self.label("slot"),
                    self.label("next"),
                    self.label("reuse"),
                    self.label("append"),
                    self.label("inserted"),
                );
                let (at, more, step) = (self.temp(), self.temp(), self.temp());
                self.line(&format!("br label %{head}"));
                self.start_block(&head);
                self.line(&format!("{at} = phi i64 [ 0, %{before} ], [ {step}, %{next} ]"));
                self.line(&format!("{more} = icmp slt i64 {at}, {length}"));
                self.line(&format!("br i1 {more}, label %{body}, label %{append}"));
                self.start_block(&body);
                let generation_at = self.item_address(&generations, "i64", &at);
                let (stored, removed) = (self.temp(), self.temp());
                self.line(&format!("{stored} = load i64, ptr {generation_at}{ITEM_ACCESS}"));
                self.line(&format!("{removed} = icmp slt i64 {stored}, 0"));
                self.line(&format!("br i1 {removed}, label %{reuse}, label %{next}"));
                self.start_block(&next);
                self.line(&format!("{step} = add i64 {at}, 1"));
                self.line(&format!("br label %{head}"));
                self.start_block(&reuse);
                let again = self.temp();
                self.line(&format!("{again} = sub i64 0, {stored}"));
                self.line(&format!("store i64 {again}, ptr {generation_at}{ITEM_ACCESS}"));
                let item_at = self.item_address(&items, &memory, &at);
                self.store_field(&held, &value, &item_at, ITEM_ACCESS);
                self.line(&format!("br label %{done}"));
                self.start_block(&append);
                self.push(&items_place, &held, &value);
                self.push(&generations_place, &Kind::Int, "0");
                let appended = self.current.clone();
                self.line(&format!("br label %{done}"));
                self.start_block(&done);
                let (slot, generation) = (self.temp(), self.temp());
                self.line(&format!("{slot} = phi i64 [ {at}, %{reuse} ], [ {length}, %{appended} ]"));
                self.line(&format!("{generation} = phi i64 [ {again}, %{reuse} ], [ 0, %{appended} ]"));
                let fields = self.offset(&block, FIELDS_OFFSET);
                let live_at = self.offset(&fields, parts.live);
                let (live, one_more) = (self.temp(), self.temp());
                self.line(&format!("{live} = load i64, ptr {live_at}"));
                self.line(&format!("{one_more} = add i64 {live}, 1"));
                self.line(&format!("store i64 {one_more}, ptr {live_at}"));
                if let Some(dest) = dest {
                    let at = self.slot(dest);
                    let generation_at = self.offset(&at, 8);
                    self.line(&format!("store i64 {slot}, ptr {at}"));
                    self.line(&format!("store i64 {generation}, ptr {generation_at}"));
                }
                Ok(())
            }
            "IdMap.remove" => {
                let id = self.id_parts(args.get(1).ok_or("no argument")?)?;
                let place = self.slot(*map);
                let block = self.alone_at(&place, "fennec_unshare");
                let (_, generations) = self.part(&block, parts.generations, true);
                let found = self.finds(&generations, (&id.0, &id.1));
                let (yes, no, done) = (self.label("take"), self.label("none"), self.label("removed"));
                self.line(&format!("br i1 {found}, label %{yes}, label %{no}"));
                self.start_block(&yes);
                let (_, items) = self.part(&block, parts.items, true);
                let at = self.item_address(&items, &memory, &id.0);
                if let Some(dest) = dest {
                    self.some_from(dest, &held, &at, true)?;
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
                self.line(&format!("br label %{done}"));
                self.start_block(&no);
                if let Some(dest) = dest {
                    self.copy(dest, &Operand::None)?;
                }
                self.line(&format!("br label %{done}"));
                self.start_block(&done);
                Ok(())
            }
            "IdMap.values" | "IdMap.ids" | "IdMap.items" => {
                self.id_map_list(dest.ok_or("no result")?, *map, &parts, name)
            }
            other => {
                let _ = span;
                Err(format!("it calls `{other}`, which this backend doesn't take yet"))
            }
        }
    }

    fn new_id_map(&mut self, dest: LocalId) -> Result<(), String> {
        let kind = self.kind_of(dest)?.clone();
        let Kind::Boxed(id) = kind else { return Err("an IdMap that isn't boxed".to_owned()) };
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
        self.assign(dest, &block)
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
        let write_id = |emit: &mut Self, base: &str| {
            let generation_place = emit.offset(base, 8);
            emit.line(&format!("store i64 {at}, ptr {base}"));
            emit.line(&format!("store i64 {generation}, ptr {generation_place}"));
        };
        match name {
            "IdMap.values" => {
                let value = self.load_field(&held, &from, ITEM_ACCESS);
                if held.counted() {
                    self.retain_value(&value, &held);
                }
                self.store_field(&held, &value, &to, ITEM_ACCESS);
            }
            "IdMap.ids" => write_id(self, &to),
            _ => {
                // A pair of an `Id` and the item, a block of its own when it holds a block.
                let Kind::Boxed(pair) = out_held else {
                    return Err(format!(
                        "`IdMap.items` gives {out_held:?}, which this backend doesn't take yet"
                    ));
                };
                let (id_at, _) = self.field(&out_held, "_0")?;
                let (value_at, _) = self.field(&out_held, "_1")?;
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

    /// `map[id]`, failing as the interpreter does when the `Id`'s item has been removed.
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
        let id = self.id_parts(id)?;
        let (_, generations) = self.part(&block, parts.generations, false);
        let found = self.finds(&generations, (&id.0, &id.1));
        let (yes, no) = (self.label("indexed"), self.label("stale"));
        self.line(&format!("br i1 {found}, label %{yes}, label %{no}, !prof !8"));
        self.start_block(&no);
        self.stop(REMOVED, span);
        self.start_block(&yes);
        let (_, items) = self.part(&block, parts.items, false);
        let at = self.item_address(&items, &memory, &id.0);
        self.read_into(dest, &held, &at, ITEM_ACCESS)
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
        let (value, fresh) = self.value_for(value, &held)?;
        let id = self.id_parts(id)?;
        let block = self.alone_at(place, "fennec_unshare");
        let (_, generations) = self.part(&block, parts.generations, false);
        let found = self.finds(&generations, (&id.0, &id.1));
        let (yes, no) = (self.label("store"), self.label("stale"));
        self.line(&format!("br i1 {found}, label %{yes}, label %{no}, !prof !8"));
        self.start_block(&no);
        self.stop(REMOVED, span);
        self.start_block(&yes);
        let (_, items) = self.part(&block, parts.items, true);
        let at = self.item_address(&items, &memory, &id.0);
        if held.counted() {
            if !fresh {
                self.retain_value(&value, &held);
            }
            let old = self.temp();
            self.line(&format!("{old} = load ptr, ptr {at}{ITEM_ACCESS}"));
            self.line(&format!("store ptr {value}, ptr {at}{ITEM_ACCESS}"));
            self.release_kind(&old, &held);
        } else {
            self.store_field(&held, &value, &at, ITEM_ACCESS);
        }
        Ok(())
    }
}
