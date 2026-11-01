//! Plain structs, tuples and lists, laid out as the runtime and the JIT lay them out.

use fennec_interp::embed::layout::{LIST_ITEMS, LIST_LENGTH, LIST_ROOM};
use fennec_ir::{LocalId, Operand};
use fennec_layout::ListItem;
use fennec_syntax::Span;

use crate::emit::{Emit, HEADER_ACCESS, ITEM_ACCESS, LENGTH_ACCESS};
use crate::kinds::{Kind, element_symbol};

impl Emit<'_, '_, '_> {
    /// The address `offset` bytes into something.
    fn offset(&mut self, base: &str, offset: u32) -> String {
        let at = self.temp();
        self.line(&format!("{at} = getelementptr inbounds i8, ptr {base}, i64 {offset}"));
        at
    }

    fn field(&self, kind: &Kind, name: &str) -> Result<(u32, Kind), String> {
        let Kind::Struct(id) = kind else { return Err(format!("it reads a field of a {kind:?}")) };
        let layouts = self.module.layouts;
        let layout = layouts.get(*id).ok_or("no such layout")?;
        let field = layout.fields.iter().find(|field| field.name == name).ok_or("no such field")?;
        let held = Kind::from_ty(layouts, field.ty).ok_or("a field this backend doesn't take yet")?;
        Ok((field.offset, held))
    }

    pub(crate) fn make(&mut self, dest: LocalId, fields: &[Operand]) -> Result<(), String> {
        let kind = self.kind_of(dest)?.clone();
        let Kind::Struct(id) = kind else {
            return Err("it makes something that isn't a plain struct".to_owned());
        };
        let layouts = self.module.layouts;
        let layout = layouts.get(id).ok_or("no such layout")?;
        let slot = self.slot(dest);
        for (written, value) in layout.fields.iter().zip(fields) {
            let held = Kind::from_ty(layouts, written.ty).ok_or("a field this backend doesn't take yet")?;
            let (value, _) = self.operand(value)?;
            let at = self.offset(&slot, written.offset);
            self.store_field(&held, &value, &at, "");
        }
        Ok(())
    }

    pub(crate) fn get_field(&mut self, dest: LocalId, object: &Operand, field: &str) -> Result<(), String> {
        let (base, kind) = self.operand(object)?;
        let (offset, held) = self.field(&kind, field)?;
        let at = self.offset(&base, offset);
        if held.in_memory() {
            return self.assign_memory(dest, &at, "");
        }
        let value = self.load_field(&held, &at, "");
        self.assign(dest, &value)
    }

    pub(crate) fn set_field(
        &mut self,
        object: LocalId,
        path: &[String],
        value: &Operand,
    ) -> Result<(), String> {
        let mut base = self.slot(object);
        let mut kind = self.kind_of(object)?.clone();
        for name in path {
            let (offset, inner) = self.field(&kind, name)?;
            base = self.offset(&base, offset);
            kind = inner;
        }
        let (value, _) = self.operand(value)?;
        self.store_field(&kind, &value, &base, "");
        Ok(())
    }

    pub(crate) fn make_tuple(&mut self, dest: LocalId, items: &[Operand]) -> Result<(), String> {
        let slot = self.slot(dest);
        let mut offset = 0;
        for item in items {
            let (value, kind) = self.operand(item)?;
            let at = self.offset(&slot, offset);
            self.store_field(&kind, &value, &at, "");
            offset += kind.size(self.module.layouts);
        }
        Ok(())
    }

    pub(crate) fn get_tuple_item(
        &mut self,
        dest: LocalId,
        value: &Operand,
        position: usize,
    ) -> Result<(), String> {
        let (base, kind) = self.operand(value)?;
        let Kind::Tuple(items) = kind else {
            return Err("it reads an item of something not a tuple".to_owned());
        };
        let layouts = self.module.layouts;
        let offset: u32 = items.iter().take(position).map(|item| item.size(layouts)).sum();
        let held = items.get(position).ok_or("no such item")?;
        let at = self.offset(&base, offset);
        if held.in_memory() {
            return self.assign_memory(dest, &at, "");
        }
        let value = self.load_field(held, &at, "");
        self.assign(dest, &value)
    }

    fn item_kind(&self, item: ListItem) -> Result<Kind, String> {
        Kind::from_ty(self.module.layouts, item.ty())
            .ok_or_else(|| "a list item this backend doesn't take yet".to_owned())
    }

    /// A new list of `count` zeroed items.
    fn new_list(&mut self, item: ListItem, count: &str) -> String {
        let symbol = element_symbol(self.module.layouts, item);
        self.module.shared.elements.insert(symbol.clone(), item);
        self.declare(&format!("@\"{symbol}\" = external dso_local global i64"));
        self.declare("declare ptr @fennec_list_new(i64, i64) nounwind");
        let (element, list) = (self.temp(), self.temp());
        self.line(&format!("{element} = load i64, ptr @\"{symbol}\", !invariant.load !1"));
        self.line(&format!("{list} = call ptr @fennec_list_new(i64 {element}, i64 {count})"));
        list
    }

    pub(crate) fn list_length(&mut self, list: &str) -> String {
        let (at, length) = (self.temp(), self.temp());
        self.line(&format!("{at} = getelementptr inbounds i8, ptr {list}, i64 {LIST_LENGTH}"));
        self.line(&format!("{length} = load i64, ptr {at}{LENGTH_ACCESS}"));
        length
    }

    fn item_address(&mut self, list: &str, memory: &str, position: &str) -> String {
        let (items, at) = (self.temp(), self.temp());
        self.line(&format!("{items} = getelementptr inbounds i8, ptr {list}, i64 {LIST_ITEMS}"));
        self.line(&format!("{at} = getelementptr inbounds {memory}, ptr {items}, i64 {position}"));
        at
    }

    /// Where an index points: from the start, or the end if it's negative, failing outside.
    fn position(&mut self, list: &str, index: &str, span: Span) -> String {
        let length = self.list_length(list);
        let (inside, from, ok, fail) =
            (self.temp(), self.label("from_end"), self.label("ok"), self.label("outside"));
        self.line(&format!("{inside} = icmp ult i64 {index}, {length}"));
        let came_from = self.current.clone();
        self.line(&format!("br i1 {inside}, label %{ok}, label %{from}, !prof !8"));
        self.start_block(&ok);
        let (back, back_inside, resolved) = (self.temp(), self.temp(), self.temp());
        self.line(&format!("{resolved} = phi i64 [ {index}, %{came_from} ], [ {back}, %{from} ]"));
        self.declare("declare void @fennec_out_of_range(i64, i64, i64, i64) #1");
        let (place, args) = self.span_values(&fail, span);
        let failed = self.failed_exit();
        self.cold_block(&format!(
            "{from}:\n  {back} = add i64 {index}, {length}\n  {back_inside} = icmp ult i64 {back}, {length}\n  br i1 {back_inside}, label %{ok}, label %{fail}\n{fail}:\n{place}  call void @fennec_out_of_range(i64 {index}, i64 {length}, {args})\n  {failed}\n"
        ));
        resolved
    }

    pub(crate) fn get_index(
        &mut self,
        dest: LocalId,
        object: &Operand,
        index: &Operand,
        span: Span,
    ) -> Result<(), String> {
        let Operand::Local(local) = object else { return Err("it indexes a literal".to_owned()) };
        let inside = self.known.inside(*local, index);
        let (list, kind) = self.operand(object)?;
        let item = list_item(&kind)?;
        let held = self.item_kind(item)?;
        let (index, _) = self.operand(index)?;
        let position = if inside { index } else { self.position(&list, &index, span) };
        let memory = held.memory(self.module.layouts);
        let at = self.item_address(&list, &memory, &position);
        if held.in_memory() {
            return self.assign_memory(dest, &at, ITEM_ACCESS);
        }
        let value = self.load_field(&held, &at, ITEM_ACCESS);
        self.assign(dest, &value)
    }

    /// Sees that the local's list has no other owner, copying it if it has, and gives it back.
    pub(crate) fn check_alone(&mut self, local: LocalId) -> String {
        self.declare("declare ptr @fennec_list_unshare(ptr) nounwind");
        let slot = self.slot(local);
        let (list, count, alone) = (self.temp(), self.temp(), self.temp());
        let (ok, shared) = (self.label("alone"), self.label("shared"));
        self.line(&format!("{list} = load ptr, ptr {slot}"));
        self.line(&format!("{count} = load i64, ptr {list}{HEADER_ACCESS}"));
        self.line(&format!("{alone} = icmp eq i64 {count}, 1"));
        let came_from = self.current.clone();
        self.line(&format!("br i1 {alone}, label %{ok}, label %{shared}, !prof !8"));
        let (copy, resolved) = (self.temp(), self.temp());
        self.cold_block(&format!(
            "{shared}:\n  {copy} = call ptr @fennec_list_unshare(ptr {list})\n  store ptr {copy}, ptr {slot}\n  br label %{ok}\n"
        ));
        self.start_block(&ok);
        self.line(&format!("{resolved} = phi ptr [ {list}, %{came_from} ], [ {copy}, %{shared} ]"));
        resolved
    }

    /// The local's list, ready to change. One nothing else here comes to hold was checked
    /// when the local got it, which leaves LLVM free to see through the writes.
    fn unshared(&mut self, local: LocalId) -> String {
        if self.known.alone_once_checked(local) {
            let (list, slot) = (self.temp(), self.slot(local));
            self.line(&format!("{list} = load ptr, ptr {slot}"));
            return list;
        }
        self.check_alone(local)
    }

    pub(crate) fn set_index(
        &mut self,
        object: LocalId,
        index: &Operand,
        value: &Operand,
        span: Span,
    ) -> Result<(), String> {
        let inside = self.known.inside(object, index);
        let kind = self.kind_of(object)?.clone();
        let item = list_item(&kind)?;
        let held = self.item_kind(item)?;
        let (index, _) = self.operand(index)?;
        let (value, _) = self.operand(value)?;
        let list = self.unshared(object);
        let position = if inside { index } else { self.position(&list, &index, span) };
        let memory = held.memory(self.module.layouts);
        let at = self.item_address(&list, &memory, &position);
        self.store_field(&held, &value, &at, ITEM_ACCESS);
        Ok(())
    }

    pub(crate) fn make_list(&mut self, dest: LocalId, items: &[Operand], block: usize) -> Result<(), String> {
        // A list a loop right after fills is made once the loop's count is known.
        if self.known.reservations.get(&block).is_some_and(|(list, _)| *list == dest.0) && items.is_empty() {
            return Ok(());
        }
        let kind = self.kind_of(dest)?.clone();
        let item = list_item(&kind)?;
        let held = self.item_kind(item)?;
        let list = self.new_list(item, &items.len().to_string());
        let memory = held.memory(self.module.layouts);
        for (position, value) in items.iter().enumerate() {
            let (value, _) = self.operand(value)?;
            let at = self.item_address(&list, &memory, &position.to_string());
            self.store_field(&held, &value, &at, ITEM_ACCESS);
        }
        self.assign(dest, &list)
    }

    /// Makes an empty list with room for what the loop after this block adds.
    pub(crate) fn reserve(&mut self, dest: LocalId, limit: &Operand) -> Result<(), String> {
        let kind = self.kind_of(dest)?.clone();
        let item = list_item(&kind)?;
        let (count, _) = self.operand(limit)?;
        let (above, room) = (self.temp(), self.temp());
        self.line(&format!("{above} = icmp sgt i64 {count}, 0"));
        self.line(&format!("{room} = select i1 {above}, i64 {count}, i64 0"));
        let list = self.new_list(item, &room);
        let at = self.offset(&list, u32::try_from(LIST_LENGTH).unwrap_or(0));
        self.line(&format!("store i64 0, ptr {at}{HEADER_ACCESS}"));
        self.assign(dest, &list)
    }

    /// `list.append(value)`: in place when nothing else holds the list and there's room.
    pub(crate) fn append(&mut self, args: &[Operand]) -> Result<(), String> {
        let Some(Operand::Local(local)) = args.first() else {
            return Err("it appends to a literal".to_owned());
        };
        let kind = self.kind_of(*local)?.clone();
        let item = list_item(&kind)?;
        let held = self.item_kind(item)?;
        let (value, _) = self.operand(args.get(1).ok_or("no argument")?)?;
        self.declare("declare ptr @fennec_list_grow(ptr) nounwind");
        let slot = self.slot(*local);
        let (list, count, length, room_at, room) =
            (self.temp(), self.temp(), self.temp(), self.temp(), self.temp());
        let (alone, fits, ready, longer) = (self.temp(), self.temp(), self.temp(), self.temp());
        let (quick, grow, done) = (self.label("quick"), self.label("grow"), self.label("done"));
        self.line(&format!("{list} = load ptr, ptr {slot}"));
        self.line(&format!("{count} = load i64, ptr {list}{HEADER_ACCESS}"));
        let length_at = self.offset(&list, u32::try_from(LIST_LENGTH).unwrap_or(0));
        self.line(&format!("{length} = load i64, ptr {length_at}{LENGTH_ACCESS}"));
        self.line(&format!("{room_at} = getelementptr inbounds i8, ptr {list}, i64 {LIST_ROOM}"));
        self.line(&format!("{room} = load i64, ptr {room_at}{HEADER_ACCESS}"));
        self.line(&format!("{fits} = icmp ult i64 {length}, {room}"));
        if self.known.alone_once_checked(*local) {
            self.line(&format!("{ready} = and i1 {fits}, true"));
        } else {
            self.line(&format!("{alone} = icmp eq i64 {count}, 1"));
            self.line(&format!("{ready} = and i1 {alone}, {fits}"));
        }
        self.line(&format!("br i1 {ready}, label %{quick}, label %{grow}, !prof !8"));
        let grown = self.temp();
        self.cold_block(&format!(
            "{grow}:\n  {grown} = call ptr @fennec_list_grow(ptr {list})\n  store ptr {grown}, ptr {slot}\n  br label %{done}\n"
        ));
        self.start_block(&quick);
        self.line(&format!("{longer} = add i64 {length}, 1"));
        self.line(&format!("store i64 {longer}, ptr {length_at}{HEADER_ACCESS}"));
        self.line(&format!("br label %{done}"));
        self.start_block(&done);
        let resolved = self.temp();
        self.line(&format!("{resolved} = phi ptr [ {list}, %{quick} ], [ {grown}, %{grow} ]"));
        let memory = held.memory(self.module.layouts);
        let at = self.item_address(&resolved, &memory, &length);
        self.store_field(&held, &value, &at, ITEM_ACCESS);
        Ok(())
    }
}

fn list_item(list: &Kind) -> Result<ListItem, String> {
    match list {
        Kind::List(item) => Ok(*item),
        other => Err(format!("it indexes a {other:?}, which this backend doesn't take yet")),
    }
}
