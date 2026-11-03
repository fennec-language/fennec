//! Plain structs, tuples and lists, laid out as the runtime and the JIT lay them out.

use std::fmt::Write as _;

use fennec_interp::embed::layout::{FIELDS, LIST_ITEMS, LIST_LENGTH, LIST_ROOM};
use fennec_ir::{LocalId, Operand};
use fennec_layout::ListItem;
use fennec_syntax::{BinOp, Span};

use crate::emit::{Emit, FIELD_ACCESS, HEADER_ACCESS, ITEM_ACCESS, LENGTH_ACCESS, VALUE_ACCESS};
use crate::kinds::{Kind, element_symbol};
use crate::uniqueness::{FieldPath, item_step, list_place};

/// Something an inline struct or enum owns, at its offset.
#[derive(Clone)]
pub(crate) enum InlinePart {
    /// A block, always there.
    Block(u32, Kind),
    /// An enum of this layout, whose blocks depend on its case.
    Enum(u32, u32),
}

impl InlinePart {
    fn shifted(self, by: u32) -> InlinePart {
        match self {
            InlinePart::Block(offset, held) => InlinePart::Block(offset + by, held),
            InlinePart::Enum(offset, id) => InlinePart::Enum(offset + by, id),
        }
    }
}

/// A case's field: its name, where it goes, and how it's held if this backend takes it.
type CaseField = (String, u32, Option<Kind>);

/// Where a block's fields start.
pub(crate) const FIELDS_OFFSET: u32 = FIELDS.unsigned_abs();
/// The room a string that pieces are added to starts with, in bytes.
const PIECES_ROOM: u32 = 64;

impl Emit<'_, '_, '_> {
    /// The address `offset` bytes into something.
    pub(crate) fn offset(&mut self, base: &str, offset: u32) -> String {
        let at = self.temp();
        self.line(&format!("{at} = getelementptr inbounds i8, ptr {base}, i64 {offset}"));
        at
    }

    fn layout_of(&self, id: u32) -> Result<&'_ fennec_layout::Layout, String> {
        self.module.layouts.get(id).ok_or_else(|| "no such layout".to_owned())
    }

    pub(crate) fn field(&self, kind: &Kind, name: &str) -> Result<(u32, Kind), String> {
        let (Kind::Struct(id) | Kind::Boxed(id)) = kind else {
            return Err(format!("it reads a field of a {kind:?}, which this backend doesn't take yet"));
        };
        let layout = self.layout_of(*id)?;
        let field = layout.fields.iter().find(|field| field.name == name).ok_or("no such field")?;
        let held = Kind::from_ty(self.module.layouts, field.ty)
            .ok_or_else(|| format!("its `{name}` field is one this backend doesn't take yet"))?;
        Ok((field.offset, held))
    }

    /// A new block of a boxed type, its shape's number read from a symbol.
    pub(crate) fn new_block(&mut self, id: u32) -> Result<String, String> {
        let shape = self.shape_of(id)?;
        self.declare("declare ptr @fennec_alloc(i64) nounwind");
        let block = self.temp();
        self.line(&format!("{block} = call ptr @fennec_alloc(i64 {shape})"));
        Ok(block)
    }

    /// The runtime's number for the shape of a layout's blocks, which a plain layout has too.
    pub(crate) fn shape_of(&mut self, id: u32) -> Result<String, String> {
        let name = self.layout_of(id)?.name.clone();
        let symbol = format!("h_{name}");
        self.module.shared.shapes.insert(symbol.clone(), id);
        self.declare(&format!("@\"{symbol}\" = external dso_local global i64"));
        let shape = self.temp();
        self.line(&format!("{shape} = load i64, ptr @\"{symbol}\", !invariant.load !1"));
        Ok(shape)
    }

    /// Writes a value inside something that hasn't held one before, with an owner if it needs one.
    pub(crate) fn fill(
        &mut self,
        kind: &Kind,
        operand: &Operand,
        at: &str,
        access: &str,
    ) -> Result<(), String> {
        let (value, fresh) = self.value_for(operand, kind)?;
        if kind.counted() && !fresh {
            self.retain_value(&value, kind);
        }
        self.store_field(kind, &value, at, access);
        // An inline struct in a local read only here moves its blocks over; any other gets
        // another owner for each.
        let moved =
            matches!(operand, Operand::Local(local) if self.owns_inline(*local) && self.known.moves(*local));
        if moved {
            let text = self.text_of(kind);
            self.line(&format!("store {text} zeroinitializer, ptr {value}"));
        } else if !fresh {
            self.retain_inline(kind, at);
        }
        Ok(())
    }

    /// Reads a value inside something into a local, with an owner if it needs one.
    pub(crate) fn read_into(
        &mut self,
        dest: LocalId,
        kind: &Kind,
        at: &str,
        access: &str,
    ) -> Result<(), String> {
        let wanted = self.kind_of(dest)?.clone();
        // A list's items hold an optional as an enum, where a local holds it as one word: what a
        // `Some` holds, and as `None` never writes that word, it's null for `None`.
        let (kind, at, access) =
            if matches!(wanted, Kind::Maybe(_) | Kind::MaybeText) && self.is_optional(kind) {
                let (_, fields) = self.case_of(kind, "Some")?;
                let (_, offset, field) = fields.first().cloned().ok_or("an optional with nothing in it")?;
                let (base, inner) = if matches!(kind, Kind::Boxed(_)) {
                    let block = self.temp();
                    self.line(&format!("{block} = load ptr, ptr {at}{access}"));
                    (self.fields_at(&block, kind), "")
                } else {
                    (at.to_owned(), access)
                };
                // A header in the enum and in the local: `None` leaves it all zero.
                if field == Some(Kind::Str) && wanted == Kind::MaybeText {
                    let place = self.offset(&base, offset);
                    let header = self.load_field(&Kind::Str, &place, inner);
                    if !self.known.is_borrowed(dest) {
                        self.retain_text(&header);
                    }
                    return self.assign(dest, &header);
                }
                (wanted.clone(), self.offset(&base, offset), inner)
            } else {
                (kind.clone(), at.to_owned(), access)
            };
        let (kind, at) = (&kind, at.as_str());
        if kind.in_memory() {
            return self.assign_memory(dest, at, access);
        }
        // A string a dict holds as a block, read into a local that holds a header.
        if *kind == Kind::TextBlock && matches!(wanted, Kind::Str | Kind::MaybeText) {
            let block = self.load_field(kind, at, access);
            let header = self.text_from_borrowed_block(&block);
            return self.assign(dest, &header);
        }
        let value = self.load_field(kind, at, access);
        if kind.counted() && !self.known.is_borrowed(dest) && !self.known.is_taken(dest) {
            self.retain_value(&value, kind);
        }
        self.assign(dest, &value)
    }

    /// What's owned inside an inline struct or enum, its inner structs' fields too. Nothing for
    /// anything else, or for an enum whose cases are all plain.
    pub(crate) fn inline_parts(&self, kind: &Kind) -> Vec<InlinePart> {
        match kind {
            Kind::Struct(id) => {
                self.module.layouts.get(*id).map_or_else(Vec::new, |layout| self.parts_of(&layout.fields))
            }
            Kind::Enum(id) if !self.case_parts(*id).is_empty() => vec![InlinePart::Enum(0, *id)],
            _ => Vec::new(),
        }
    }

    /// Whether an inline struct or enum owns anything.
    pub(crate) fn holds_inline(&self, kind: &Kind) -> bool {
        !self.inline_parts(kind).is_empty()
    }

    fn parts_of(&self, fields: &[fennec_layout::Field]) -> Vec<InlinePart> {
        let mut found = Vec::new();
        for field in fields {
            match Kind::from_ty(self.module.layouts, field.ty) {
                Some(inner @ Kind::Struct(_)) => {
                    found
                        .extend(self.inline_parts(&inner).into_iter().map(|part| part.shifted(field.offset)));
                }
                Some(Kind::Enum(id)) if !self.case_parts(id).is_empty() => {
                    found.push(InlinePart::Enum(field.offset, id));
                }
                Some(held) if held.counted() => found.push(InlinePart::Block(field.offset, held)),
                _ => {}
            }
        }
        found
    }

    /// Each case of an enum that owns anything: its number, and what it owns.
    fn case_parts(&self, id: u32) -> Vec<(usize, Vec<InlinePart>)> {
        let Some(cases) = self.module.layouts.get(id).and_then(|layout| layout.cases.as_ref()) else {
            return Vec::new();
        };
        cases
            .iter()
            .enumerate()
            .map(|(number, case)| (number, self.parts_of(&case.fields)))
            .filter(|(_, parts)| !parts.is_empty())
            .collect()
    }

    /// One more owner, or one fewer, for each block in what an inline part holds: for an enum,
    /// by the case it's in now.
    fn count_part(&mut self, part: InlinePart, at: &str, more: bool) {
        match part {
            InlinePart::Block(offset, held) => {
                let (place, block, text) = (self.offset(at, offset), self.temp(), self.text_of(&held));
                self.line(&format!("{block} = load {text}, ptr {place}"));
                if more {
                    self.retain_value(&block, &held);
                } else {
                    self.release_kind(&block, &held);
                }
            }
            InlinePart::Enum(offset, id) => {
                let (base, tag, done) = (self.offset(at, offset), self.temp(), self.label("counted"));
                self.line(&format!("{tag} = load i64, ptr {base}"));
                for (number, parts) in self.case_parts(id) {
                    let (this, next, same) = (self.label("case"), self.label("not_case"), self.temp());
                    self.line(&format!("{same} = icmp eq i64 {tag}, {number}"));
                    self.line(&format!("br i1 {same}, label %{this}, label %{next}"));
                    self.start_block(&this);
                    for part in parts {
                        self.count_part(part, &base, more);
                    }
                    self.line(&format!("br label %{done}"));
                    self.start_block(&next);
                }
                self.line(&format!("br label %{done}"));
                self.start_block(&done);
            }
        }
    }

    /// One more owner for each block an inline struct or enum at `at` holds.
    pub(crate) fn retain_inline(&mut self, kind: &Kind, at: &str) {
        for part in self.inline_parts(kind) {
            self.count_part(part, at, true);
        }
    }

    /// One owner fewer for each block an inline struct or enum at `at` holds.
    pub(crate) fn release_inline(&mut self, kind: &Kind, at: &str) {
        for part in self.inline_parts(kind) {
            self.count_part(part, at, false);
        }
    }

    /// Where the fields of what's in `base` start: inline, or in a block.
    pub(crate) fn fields_at(&mut self, base: &str, kind: &Kind) -> String {
        if matches!(kind, Kind::Boxed(_)) { self.offset(base, FIELDS_OFFSET) } else { base.to_owned() }
    }

    pub(crate) fn make(&mut self, dest: LocalId, fields: &[Operand]) -> Result<(), String> {
        let kind = self.kind_of(dest)?.clone();
        let (Kind::Struct(id) | Kind::Boxed(id)) = kind else {
            return Err("it makes something this backend doesn't take yet".to_owned());
        };
        let written: Vec<(u32, Option<Kind>)> = self
            .layout_of(id)?
            .fields
            .iter()
            .map(|field| (field.offset, Kind::from_ty(self.module.layouts, field.ty)))
            .collect();
        let block = if matches!(kind, Kind::Boxed(_)) { Some(self.new_block(id)?) } else { None };
        // Holding blocks, it's made beside what the local holds, which may be read for it.
        let beside = (block.is_none() && self.holds_inline(&kind)).then(|| self.place_for(&kind));
        let base = match (&block, &beside) {
            (Some(block), _) => self.offset(block, FIELDS_OFFSET),
            (None, Some(place)) => place.clone(),
            (None, None) => self.slot(dest),
        };
        let access = if block.is_some() { FIELD_ACCESS } else { "" };
        for ((offset, held), value) in written.into_iter().zip(fields) {
            let held = held.ok_or("a field this backend doesn't take yet")?;
            let at = self.offset(&base, offset);
            self.fill(&held, value, &at, access)?;
        }
        if let Some(block) = block {
            self.assign(dest, &block)?;
        }
        if let Some(place) = beside {
            self.assign_memory_owned(dest, &place)?;
        }
        Ok(())
    }

    pub(crate) fn get_field(&mut self, dest: LocalId, object: &Operand, field: &str) -> Result<(), String> {
        if let Some((root, list_path, index)) = self.known.path_home(dest).cloned() {
            return self.take_from_item(dest, (root, &list_path, &index), field);
        }
        let (base, kind) = self.operand(object)?;
        // A field taken out to change: its count only means something if what holds it is alone.
        let base = match object {
            Operand::Local(local) if self.known.is_taken(dest) && matches!(kind, Kind::Boxed(_)) => {
                let place = self.slot(*local);
                self.alone_along((*local, &[]), &place, "fennec_unshare")
            }
            _ => base,
        };
        let (offset, held) = self.field(&kind, field)?;
        let fields = self.fields_at(&base, &kind);
        let at = self.offset(&fields, offset);
        let access = if matches!(kind, Kind::Boxed(_)) { FIELD_ACCESS } else { "" };
        self.read_into(dest, &held, &at, access)
    }

    /// `root.list[index].field` or `list[index].field`, taken out to change and put back: the
    /// root, the list and the item are each made alone first, so the field's one owner is the
    /// item it goes back to.
    fn take_from_item(
        &mut self,
        dest: LocalId,
        (root, list_path, index): (LocalId, &[String], &Operand),
        field: &str,
    ) -> Result<(), String> {
        let list = self.alone_list(root, list_path)?;
        let item_kind = self.place_kind(root, list_path)?;
        let Kind::List(item) = item_kind else { return Err("an item of something else".to_owned()) };
        let held = self.item_kind(item)?;
        if !matches!(held, Kind::Boxed(_) | Kind::Struct(_)) {
            return Err(format!("it takes a field of a {held:?} item, which this backend doesn't take yet"));
        }
        // The item was read at this index just before, so it's inside the list, counting from
        // the end if it's negative.
        let index_operand = index;
        let (index, _) = self.operand(index)?;
        let length = self.list_length(&list);
        let (negative, from_end, position) = (self.temp(), self.temp(), self.temp());
        self.line(&format!("{negative} = icmp slt i64 {index}, 0"));
        self.line(&format!("{from_end} = add i64 {index}, {length}"));
        self.line(&format!("{position} = select i1 {negative}, i64 {from_end}, i64 {index}"));
        // An item in place: its field is right there in the list, which is alone.
        if matches!(held, Kind::Struct(_)) {
            let memory = held.memory(self.module.layouts);
            let at = self.item_address(&list, &memory, &position);
            let (offset, inner) = self.field(&held, field)?;
            let place = self.offset(&at, offset);
            let value = self.load_field(&inner, &place, ITEM_ACCESS);
            return self.assign(dest, &value);
        }
        let at = self.item_address(&list, "ptr", &position);
        let block = match item_step(index_operand) {
            Some(step) => {
                let along: Vec<String> = list_path.iter().cloned().chain([step]).collect();
                self.alone_along((root, &along), &at, "fennec_unshare")
            }
            None => self.alone_at(&at, "fennec_unshare"),
        };
        let (offset, inner) = self.field(&held, field)?;
        let fields = self.fields_at(&block, &held);
        let place = self.offset(&fields, offset);
        let value = self.load_field(&inner, &place, FIELD_ACCESS);
        self.assign(dest, &value)
    }

    /// A block at `place` with no other owner, so it can change.
    pub(crate) fn alone_at(&mut self, place: &str, helper: &str) -> String {
        if self.alone_slots_contains(place) {
            let block = self.temp();
            self.line(&format!("{block} = load ptr, ptr {place}"));
            return block;
        }
        self.declare(&format!("declare ptr @{helper}(ptr) nounwind"));
        let block = self.temp();
        let (ok, shared) = (self.label("alone"), self.label("shared"));
        self.line(&format!("{block} = load ptr, ptr {place}"));
        let alone = self.alone(&block);
        let came_from = self.current.clone();
        self.line(&format!("br i1 {alone}, label %{ok}, label %{shared}, !prof !8"));
        let (copy, resolved) = (self.temp(), self.temp());
        self.cold_block(&format!(
            "{shared}:\n  {copy} = call ptr @{helper}(ptr {block})\n  store ptr {copy}, ptr {place}\n  br label %{ok}\n"
        ));
        self.start_block(&ok);
        self.line(&format!("{resolved} = phi ptr [ {block}, %{came_from} ], [ {copy}, %{shared} ]"));
        self.alone_from_now(&resolved);
        resolved
    }

    pub(crate) fn set_field(
        &mut self,
        object: LocalId,
        path: &[String],
        value: &Operand,
    ) -> Result<(), String> {
        let put_back = matches!(value, Operand::Local(local) if self.known.is_taken(*local));
        let field_kind = self.place_kind(object, path)?;
        let (value, fresh) = self.value_for(value, &field_kind)?;
        let mut place = self.slot(object);
        let mut kind = self.kind_of(object)?.clone();
        let mut in_block = false;
        for (step, name) in path.iter().enumerate() {
            let inside = if matches!(kind, Kind::Boxed(_)) {
                in_block = true;
                let block = self.alone_along(
                    (object, path.get(..step).unwrap_or_default()),
                    &place,
                    "fennec_unshare",
                );
                self.offset(&block, FIELDS_OFFSET)
            } else {
                place
            };
            let (offset, inner) = self.field(&kind, name)?;
            place = self.offset(&inside, offset);
            kind = inner;
        }
        let access = if in_block { FIELD_ACCESS } else { VALUE_ACCESS };
        if put_back && kind.counted() {
            // The field's old value was taken out, and this is what became of it.
            let text = self.text_of(&kind);
            self.line(&format!("store {text} {value}, ptr {place}{access}"));
        } else if kind.counted() {
            if !fresh {
                self.retain_value(&value, &kind);
            }
            let (old, text) = (self.temp(), self.text_of(&kind));
            self.line(&format!("{old} = load {text}, ptr {place}{access}"));
            self.line(&format!("store {text} {value}, ptr {place}{access}"));
            self.release_kind(&old, &kind);
        } else {
            if !put_back {
                self.release_inline(&kind, &place);
            }
            self.store_field(&kind, &value, &place, access);
            if !put_back {
                self.retain_inline(&kind, &place);
            }
        }
        Ok(())
    }

    /// An enum's case, by name: its number, and its fields with where they go.
    pub(crate) fn case_of(&self, kind: &Kind, name: &str) -> Result<(i64, Vec<CaseField>), String> {
        let (Kind::Enum(id) | Kind::Boxed(id)) = kind else {
            return Err(format!("it reads a case of a {kind:?}, which this backend doesn't take yet"));
        };
        let cases = self.layout_of(*id)?.cases.as_ref().ok_or("a case of a struct")?;
        let number = cases.iter().position(|case| case.name == name).ok_or("no such case")?;
        let case = cases.get(number).ok_or("no such case")?;
        let fields = case
            .fields
            .iter()
            .map(|field| (field.name.clone(), field.offset, Kind::from_ty(self.module.layouts, field.ty)))
            .collect();
        Ok((i64::try_from(number).unwrap_or(0), fields))
    }

    pub(crate) fn make_case(&mut self, dest: LocalId, case: &str, fields: &[Operand]) -> Result<(), String> {
        let kind = self.kind_of(dest)?.clone();
        let (number, written) = self.case_of(&kind, case)?;
        let block = if let Kind::Boxed(id) = kind { Some(self.new_block(id)?) } else { None };
        // Holding blocks, it's made beside what the local holds, which may be read for it.
        let beside = (block.is_none() && self.holds_inline(&kind)).then(|| self.place_for(&kind));
        let base = if let Some(block) = &block {
            self.offset(block, FIELDS_OFFSET)
        } else {
            // Words a case doesn't use are zero, so equal enums have equal words.
            let place = beside.clone().unwrap_or_else(|| self.slot(dest));
            let text = self.text_of(&kind);
            self.line(&format!("store {text} zeroinitializer, ptr {place}"));
            place
        };
        self.line(&format!("store i64 {number}, ptr {base}"));
        for ((_, offset, held), value) in written.into_iter().zip(fields) {
            let held = held.ok_or("a field this backend doesn't take yet")?;
            let at = self.offset(&base, offset);
            self.fill(&held, value, &at, "")?;
        }
        if let Some(block) = block {
            self.assign(dest, &block)?;
        }
        if let Some(place) = beside {
            self.assign_memory_owned(dest, &place)?;
        }
        Ok(())
    }

    pub(crate) fn is_case(&mut self, dest: LocalId, value: &Operand, case: &str) -> Result<(), String> {
        let (base, kind) = self.operand(value)?;
        if kind == Kind::MaybeText {
            let none = self.header_is_none(&base);
            if case == "None" {
                return self.assign(dest, &none);
            }
            let some = self.temp();
            self.line(&format!("{some} = xor i1 {none}, true"));
            return self.assign(dest, &some);
        }
        if matches!(kind, Kind::Maybe(_)) {
            let cc = if case == "None" { "eq" } else { "ne" };
            let same = self.temp();
            self.line(&format!("{same} = icmp {cc} ptr {base}, null"));
            return self.assign(dest, &same);
        }
        let (number, _) = self.case_of(&kind, case)?;
        let fields = self.fields_at(&base, &kind);
        let (word, same) = (self.temp(), self.temp());
        self.line(&format!("{word} = load i64, ptr {fields}"));
        self.line(&format!("{same} = icmp eq i64 {word}, {number}"));
        self.assign(dest, &same)
    }

    pub(crate) fn get_case_field(
        &mut self,
        dest: LocalId,
        value: &Operand,
        case: &str,
        field: &str,
    ) -> Result<(), String> {
        let (base, kind) = self.operand(value)?;
        let (_, written) = self.case_of(&kind, case)?;
        let (_, offset, held) =
            written.into_iter().find(|(name, _, _)| name == field).ok_or("no such field")?;
        let held = held.ok_or("a field this backend doesn't take yet")?;
        let fields = self.fields_at(&base, &kind);
        let at = self.offset(&fields, offset);
        self.read_into(dest, &held, &at, "")
    }

    /// An operator on two plain structs or tuples at these addresses: `==` field by field, and
    /// an order by the first field that differs.
    pub(crate) fn compare_structs(
        &mut self,
        op: BinOp,
        id: u32,
        (a, b): (&str, &str),
    ) -> Result<String, String> {
        if self.layout_of(id)?.cases.is_some() {
            return Err("it orders an enum, which this backend doesn't take yet".to_owned());
        }
        if matches!(op, BinOp::Eq | BinOp::NotEq) {
            let same = self.same_structs(id, (a, b))?;
            if op == BinOp::Eq {
                return Ok(same);
            }
            let differ = self.temp();
            self.line(&format!("{differ} = xor i1 {same}, true"));
            return Ok(differ);
        }
        let ordering = self.struct_order(id, (a, b))?;
        let cc = match op {
            BinOp::Lt => "slt",
            BinOp::LtEq => "sle",
            BinOp::Gt => "sgt",
            BinOp::GtEq => "sge",
            other => {
                return Err(format!("it uses {other:?} on a struct, which this backend doesn't take yet"));
            }
        };
        let result = self.temp();
        self.line(&format!("{result} = icmp {cc} i64 {ordering}, 0"));
        Ok(result)
    }

    /// The fields of a plain struct, with where they are and how they're held.
    fn struct_fields(&self, id: u32) -> Result<Vec<(u32, Kind)>, String> {
        let layout = self.layout_of(id)?;
        layout
            .fields
            .iter()
            .map(|field| {
                Kind::from_ty(self.module.layouts, field.ty)
                    .map(|kind| (field.offset, kind))
                    .ok_or_else(|| "a field this backend doesn't take yet".to_owned())
            })
            .collect()
    }

    fn same_structs(&mut self, id: u32, places: (&str, &str)) -> Result<String, String> {
        let fields = self.struct_fields(id)?;
        self.same_fields(&fields, places)
    }

    /// Whether these fields of what's at two addresses are all the same.
    fn same_fields(&mut self, fields: &[(u32, Kind)], (a, b): (&str, &str)) -> Result<String, String> {
        let mut same = "true".to_owned();
        for (offset, kind) in fields {
            let (x_at, y_at) = (self.offset(a, *offset), self.offset(b, *offset));
            let equal = match kind {
                Kind::Struct(inner) => self.same_structs(*inner, (&x_at, &y_at))?,
                Kind::Enum(inner) => self.same_enums(BinOp::Eq, *inner, (&x_at, &y_at))?,
                _ => {
                    let (x, y) = (self.load_field(kind, &x_at, ""), self.load_field(kind, &y_at, ""));
                    match kind {
                        Kind::Int | Kind::Bool => {
                            let equal = self.temp();
                            let text = if *kind == Kind::Bool { "i1" } else { "i64" };
                            self.line(&format!("{equal} = icmp eq {text} {x}, {y}"));
                            equal
                        }
                        Kind::Float => {
                            let equal = self.temp();
                            self.line(&format!("{equal} = fcmp oeq double {x}, {y}"));
                            equal
                        }
                        Kind::Str if self.headers() => self.same_headers(&x, &y),
                        Kind::Str => self.string_binary(BinOp::Eq, (&x, &y))?,
                        Kind::Boxed(_) | Kind::List(_) | Kind::Dict(..) => {
                            self.same_blocks(BinOp::Eq, (&x, &y))
                        }
                        other => {
                            return Err(format!(
                                "it compares a struct holding a {other:?}, which this backend doesn't take yet"
                            ));
                        }
                    }
                }
            };
            let both = self.temp();
            self.line(&format!("{both} = and i1 {same}, {equal}"));
            same = both;
        }
        Ok(same)
    }

    /// -1, 0 or 1, by the first field that differs.
    fn struct_order(&mut self, id: u32, (a, b): (&str, &str)) -> Result<String, String> {
        let mut ordering = "0".to_owned();
        for (offset, kind) in self.struct_fields(id)? {
            let (x_at, y_at) = (self.offset(a, offset), self.offset(b, offset));
            let this = if let Kind::Struct(inner) = &kind {
                self.struct_order(*inner, (&x_at, &y_at))?
            } else if let Kind::Boxed(inner) = &kind {
                let (x, y) = (self.temp(), self.temp());
                self.line(&format!("{x} = load ptr, ptr {x_at}"));
                self.line(&format!("{y} = load ptr, ptr {y_at}"));
                let (x_fields, y_fields) = (self.offset(&x, FIELDS_OFFSET), self.offset(&y, FIELDS_OFFSET));
                self.struct_order(*inner, (&x_fields, &y_fields))?
            } else {
                {
                    let (x, y) = (self.load_field(&kind, &x_at, ""), self.load_field(&kind, &y_at, ""));
                    let (less, more) = (self.temp(), self.temp());
                    match &kind {
                        Kind::Int => {
                            self.line(&format!("{less} = icmp slt i64 {x}, {y}"));
                            self.line(&format!("{more} = icmp sgt i64 {x}, {y}"));
                        }
                        Kind::Bool => {
                            self.line(&format!("{less} = icmp ult i1 {x}, {y}"));
                            self.line(&format!("{more} = icmp ugt i1 {x}, {y}"));
                        }
                        Kind::Float => {
                            self.line(&format!("{less} = fcmp olt double {x}, {y}"));
                            self.line(&format!("{more} = fcmp ogt double {x}, {y}"));
                        }
                        Kind::Str => {
                            let order = self.on_blocks([&x, &y], |emit, [a, b]| emit.text_order(&a, &b));
                            self.line(&format!("{less} = icmp slt i64 {order}, 0"));
                            self.line(&format!("{more} = icmp sgt i64 {order}, 0"));
                        }
                        other => {
                            return Err(format!(
                                "it orders a struct holding a {other:?}, which this backend doesn't take yet"
                            ));
                        }
                    }
                    let (low, order) = (self.temp(), self.temp());
                    self.line(&format!("{low} = select i1 {less}, i64 -1, i64 0"));
                    self.line(&format!("{order} = select i1 {more}, i64 1, i64 {low}"));
                    order
                }
            };
            // The first field that differs decides.
            let (decided, next) = (self.temp(), self.temp());
            self.line(&format!("{decided} = icmp ne i64 {ordering}, 0"));
            self.line(&format!("{next} = select i1 {decided}, i64 {ordering}, i64 {this}"));
            ordering = next;
        }
        Ok(ordering)
    }

    /// An order on two boxed structs or tuples, by their fields.
    pub(crate) fn compare_boxed(
        &mut self,
        op: BinOp,
        id: u32,
        (a, b): (&str, &str),
    ) -> Result<String, String> {
        let (a_fields, b_fields) = (self.offset(a, FIELDS_OFFSET), self.offset(b, FIELDS_OFFSET));
        self.compare_structs(op, id, (&a_fields, &b_fields))
    }

    /// `==` on two enums in place: their words, one by one, or their cases' fields if they hold
    /// blocks, whose words can differ when what they hold doesn't.
    pub(crate) fn same_enums(&mut self, op: BinOp, id: u32, (a, b): (&str, &str)) -> Result<String, String> {
        let same = if self.case_parts(id).is_empty() {
            let words = self.module.layouts.get(id).map_or(0, |layout| layout.size / 8);
            let mut same = "true".to_owned();
            for word in 0..words {
                let (x_at, y_at) = (self.offset(a, word * 8), self.offset(b, word * 8));
                let (x, y, equal, both) = (self.temp(), self.temp(), self.temp(), self.temp());
                self.line(&format!("{x} = load i64, ptr {x_at}"));
                self.line(&format!("{y} = load i64, ptr {y_at}"));
                self.line(&format!("{equal} = icmp eq i64 {x}, {y}"));
                self.line(&format!("{both} = and i1 {same}, {equal}"));
                same = both;
            }
            same
        } else {
            self.same_cases(id, (a, b))?
        };
        if op == BinOp::NotEq {
            let flipped = self.temp();
            self.line(&format!("{flipped} = xor i1 {same}, true"));
            return Ok(flipped);
        }
        Ok(same)
    }

    /// Whether two enums at these addresses are in the same case with the same fields.
    fn same_cases(&mut self, id: u32, (a, b): (&str, &str)) -> Result<String, String> {
        let layouts = self.module.layouts;
        let cases =
            layouts.get(id).and_then(|layout| layout.cases.as_ref()).ok_or("the cases of a struct")?;
        let (x_tag, y_tag, same_tag) = (self.temp(), self.temp(), self.temp());
        self.line(&format!("{x_tag} = load i64, ptr {a}"));
        self.line(&format!("{y_tag} = load i64, ptr {b}"));
        self.line(&format!("{same_tag} = icmp eq i64 {x_tag}, {y_tag}"));
        let result = self.label("%same");
        let _ = writeln!(self.allocas, "  {result} = alloca i1");
        self.line(&format!("store i1 {same_tag}, ptr {result}"));
        let done = self.label("compared");
        for (number, case) in cases.iter().enumerate().filter(|(_, case)| !case.fields.is_empty()) {
            let fields: Vec<(u32, Kind)> = case
                .fields
                .iter()
                .map(|field| {
                    Kind::from_ty(self.module.layouts, field.ty)
                        .map(|kind| (field.offset, kind))
                        .ok_or_else(|| "a field this backend doesn't take yet".to_owned())
                })
                .collect::<Result<_, _>>()?;
            let (this, next, here, both) =
                (self.label("case"), self.label("not_case"), self.temp(), self.temp());
            self.line(&format!("{here} = icmp eq i64 {x_tag}, {number}"));
            self.line(&format!("{both} = and i1 {same_tag}, {here}"));
            self.line(&format!("br i1 {both}, label %{this}, label %{next}"));
            self.start_block(&this);
            let same = self.same_fields(&fields, (a, b))?;
            self.line(&format!("store i1 {same}, ptr {result}"));
            self.line(&format!("br label %{done}"));
            self.start_block(&next);
        }
        self.line(&format!("br label %{done}"));
        self.start_block(&done);
        let same = self.temp();
        self.line(&format!("{same} = load i1, ptr {result}"));
        Ok(same)
    }

    pub(crate) fn item_kind(&self, item: ListItem) -> Result<Kind, String> {
        Kind::from_ty(self.module.layouts, item.ty())
            .ok_or_else(|| "a list item this backend doesn't take yet".to_owned())
    }

    /// A new list of `count` zeroed items.
    pub(crate) fn new_list(&mut self, item: ListItem, count: &str) -> String {
        self.new_list_with(item, count, "fennec_list_new")
    }

    fn new_list_with(&mut self, item: ListItem, count: &str, helper: &str) -> String {
        let symbol = element_symbol(self.module.layouts, item);
        self.module.shared.elements.insert(symbol.clone(), item);
        self.declare(&format!("@\"{symbol}\" = external dso_local global i64"));
        self.declare(&format!("declare ptr @{helper}(i64, i64) nounwind"));
        let (element, list) = (self.temp(), self.temp());
        self.line(&format!("{element} = load i64, ptr @\"{symbol}\", !invariant.load !1"));
        self.line(&format!("{list} = call ptr @{helper}(i64 {element}, i64 {count})"));
        list
    }

    pub(crate) fn list_length(&mut self, list: &str) -> String {
        let (at, length) = (self.temp(), self.temp());
        self.line(&format!("{at} = getelementptr inbounds i8, ptr {list}, i64 {LIST_LENGTH}"));
        self.line(&format!("{length} = load i64, ptr {at}{LENGTH_ACCESS}"));
        length
    }

    pub(crate) fn item_address(&mut self, list: &str, memory: &str, position: &str) -> String {
        let (items, at) = (self.temp(), self.temp());
        self.line(&format!("{items} = getelementptr inbounds i8, ptr {list}, i64 {LIST_ITEMS}"));
        self.line(&format!("{at} = getelementptr inbounds {memory}, ptr {items}, i64 {position}"));
        at
    }

    /// The list a field path from a local leads to, named as what's known of it is.
    fn list_key(&self, object: LocalId, path: &[String]) -> Option<FieldPath> {
        let (root, mut along) = if self.taken_items.contains_key(&object.0) {
            self.fact_key(object, &[], 8)
        } else {
            list_place(self.known, object)?
        };
        along.extend(path.iter().cloned());
        Some((root, along))
    }

    /// Where an index points in a list, checked once: until something shortens or replaces
    /// the list, or writes the index, it's read back from where the first check left it.
    fn position_once(
        &mut self,
        place: Option<FieldPath>,
        (list, index_operand, index): (&str, &Operand, &str),
        span: Span,
    ) -> String {
        let fact = place.zip(item_step(index_operand)).map(|((root, mut path), step)| {
            path.push(step);
            (root, path)
        });
        if let Some(fact) = &fact
            && self.inside_items.contains(fact)
            && let Some(slot) = self.inside_slots.get(fact).cloned()
        {
            let position = self.temp();
            self.line(&format!("{position} = load i64, ptr {slot}"));
            return position;
        }
        let position = self.position(list, index, span);
        if let Some(fact) = fact {
            let slot = if let Some(slot) = self.inside_slots.get(&fact) {
                slot.clone()
            } else {
                let slot = self.place_for(&Kind::Int);
                self.inside_slots.insert(fact.clone(), slot.clone());
                slot
            };
            self.line(&format!("store i64 {position}, ptr {slot}"));
            self.inside_items.push(fact);
        }
        position
    }

    /// Where an index points: from the start, or the end if it's negative, failing outside.
    pub(crate) fn position(&mut self, list: &str, index: &str, span: Span) -> String {
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
        // A pair from an `enumerate` never made isn't made either: where it points in the list
        // is checked, and kept for its parts.
        if let Some((list, _)) = self.known.pairs.get(&dest.0).cloned() {
            let (list, _) = self.operand(&Operand::Local(list))?;
            let (index, _) = self.operand(index)?;
            let position = self.position(&list, &index, span);
            let slot = self.place_for(&Kind::Int);
            self.line(&format!("store i64 {position}, ptr {slot}"));
            self.pair_slots.insert(dest.0, slot);
            // Holding nothing, letting go of it lets go of nothing.
            let kind = self.kind_of(dest)?.clone();
            let (held, text) = (self.slot(dest), self.text_of(&kind));
            if kind.in_memory() || kind.counted() {
                self.line(&format!("store {text} zeroinitializer, ptr {held}"));
            }
            return Ok(());
        }
        let inside = matches!(object, Operand::Local(local) if self.known.inside(*local, index));
        if let Operand::Local(local) = object
            && self.is_id_map(&self.kind_of(*local)?.clone())
        {
            return self.id_map_index(dest, object, index, span);
        }
        let (list, kind) = self.operand(object)?;
        if let Kind::Dict(key, value) = kind {
            return self.dict_read(dest, &list, (key, value), index, span);
        }
        if kind == Kind::Str {
            if self.headers() {
                return self.header_index(dest, &list, index, span);
            }
            return self.on_blocks([&list], |emit, [block]| emit.string_index(dest, &block, index, span));
        }
        let item = list_item(&kind)?;
        let held = self.item_kind(item)?;
        // An item taken out to change: its count only means something once the list is alone,
        // and with the list alone from here, putting it back needs no checks between.
        // A local list's own check is made once, ahead of its loop, so that stays as it is.
        // So does a `for mut` item, which lets go of nothing until it's put back.
        let over_loop = self.known.taken_over_loop(dest);
        let taken = self
            .known
            .item_home(dest)
            .filter(|(_, path)| held.counted() || !path.is_empty() || over_loop)
            .cloned();
        let list = match &taken {
            Some((home, path)) if over_loop && path.is_empty() => self.unshared(*home),
            Some((home, path)) => self.alone_list(*home, path)?,
            None => list,
        };
        let index_operand = index;
        let (index, _) = self.operand(index)?;
        let position = if inside {
            index
        } else {
            let place = match object {
                Operand::Local(local) => self.list_key(*local, &[]),
                _ => None,
            };
            self.position_once(place, (&list, index_operand, &index), span)
        };
        let memory = held.memory(self.module.layouts);
        let at = self.item_address(&list, &memory, &position);
        if let Some((home, path)) = taken {
            self.taken_at.insert(dest.0, at.clone());
            if let Some(step) = item_step(index_operand) {
                self.taken_items.insert(dest.0, (home, path, step));
            }
        }
        self.read_into(dest, &held, &at, ITEM_ACCESS)
    }

    /// Sees that the local's list has no other owner, copying it if it has, and gives it back.
    pub(crate) fn check_alone(&mut self, local: LocalId) -> String {
        self.declare("declare ptr @fennec_list_unshare(ptr) nounwind");
        let slot = self.slot(local);
        let list = self.temp();
        let (ok, shared) = (self.label("alone"), self.label("shared"));
        self.line(&format!("{list} = load ptr, ptr {slot}"));
        let alone = self.alone(&list);
        let came_from = self.current.clone();
        self.line(&format!("br i1 {alone}, label %{ok}, label %{shared}, !prof !8"));
        let (copy, resolved) = (self.temp(), self.temp());
        self.cold_block(&format!(
            "{shared}:\n  {copy} = call ptr @fennec_list_unshare(ptr {list})\n  store ptr {copy}, ptr {slot}\n  br label %{ok}\n"
        ));
        self.start_block(&ok);
        self.line(&format!("{resolved} = phi ptr [ {list}, %{came_from} ], [ {copy}, %{shared} ]"));
        self.alone_from_now(&resolved);
        resolved
    }

    /// A block just made alone, either way, has one owner: saying so lets LLVM drop the next
    /// check of it when nothing between could have given it another.
    fn alone_from_now(&mut self, block: &str) {
        self.declare("declare void @llvm.assume(i1)");
        let one = self.alone(block);
        self.line(&format!("call void @llvm.assume(i1 {one})"));
    }

    /// The local's list, ready to change. One nothing else here comes to hold was checked
    /// when the local got it, which leaves LLVM free to see through the writes.
    pub(crate) fn unshared(&mut self, local: LocalId) -> String {
        if self.known.alone_once_checked(local) {
            let (list, slot) = (self.temp(), self.slot(local));
            self.line(&format!("{list} = load ptr, ptr {slot}"));
            return list;
        }
        self.check_alone(local)
    }

    /// What's at a field path from a local.
    pub(crate) fn place_kind(&self, object: LocalId, path: &[String]) -> Result<Kind, String> {
        let mut kind = self.kind_of(object)?.clone();
        for name in path {
            let (_, field) = self.field(&kind, name)?;
            kind = field;
        }
        Ok(kind)
    }

    /// Where a field path from a local leads, with no other owner of anything on the way.
    pub(crate) fn place_of(&mut self, object: LocalId, path: &[String]) -> Result<String, String> {
        let mut kind = self.kind_of(object)?.clone();
        let mut place = self.slot(object);
        for (step, name) in path.iter().enumerate() {
            let inner = if matches!(kind, Kind::Boxed(_)) {
                let block = self.alone_along(
                    (object, path.get(..step).unwrap_or_default()),
                    &place,
                    "fennec_unshare",
                );
                self.offset(&block, FIELDS_OFFSET)
            } else {
                place
            };
            let (offset, field) = self.field(&kind, name)?;
            place = self.offset(&inner, offset);
            kind = field;
        }
        Ok(place)
    }

    /// The list at a field path from a local, with no other owner, nor anything it's inside.
    fn alone_list(&mut self, object: LocalId, path: &[String]) -> Result<String, String> {
        if path.is_empty() {
            return Ok(self.check_alone(object));
        }
        let mut kind = self.kind_of(object)?.clone();
        let mut place = self.slot(object);
        for (step, name) in path.iter().enumerate() {
            let inner = if matches!(kind, Kind::Boxed(_)) {
                let block = self.alone_along(
                    (object, path.get(..step).unwrap_or_default()),
                    &place,
                    "fennec_unshare",
                );
                self.offset(&block, FIELDS_OFFSET)
            } else {
                place
            };
            let (offset, field) = self.field(&kind, name)?;
            place = self.offset(&inner, offset);
            kind = field;
        }
        Ok(self.alone_along((object, path), &place, "fennec_list_unshare"))
    }

    /// The block at a field path from a local, made alone unless that's known already, and
    /// known from here.
    pub(crate) fn alone_along(
        &mut self,
        (root, path): (LocalId, &[String]),
        place: &str,
        helper: &str,
    ) -> String {
        let key = self.fact_key(root, path, 8);
        if self.alone_fields.contains(&key) {
            let block = self.temp();
            self.line(&format!("{block} = load ptr, ptr {place}"));
            return block;
        }
        let block = self.alone_at(place, helper);
        self.alone_fields.push(key);
        block
    }

    /// A field path from a local, from the list and index it was taken from, if it's an item
    /// taken out to change: so what's known of the item outlives the local.
    fn fact_key(&self, root: LocalId, path: &[String], depth: usize) -> FieldPath {
        if let Some((home, list, step)) = self.taken_items.get(&root.0)
            && let Some(deeper) = depth.checked_sub(1)
        {
            let (home, mut along) = self.fact_key(*home, list, deeper);
            along.push(step.clone());
            along.extend(path.iter().cloned());
            return (home, along);
        }
        (root, path.to_vec())
    }

    /// Makes each block along a field path alone, unless it's known to be already: what a
    /// function changing through it, given the root, counts on.
    pub(crate) fn see_to(&mut self, root: LocalId, path: &[String]) -> Result<(), String> {
        let mut kind = self.kind_of(root)?.clone();
        let mut place = self.slot(root);
        for step in 0..=path.len() {
            let along = path.get(..step).unwrap_or_default();
            let block = match kind {
                Kind::Boxed(_) => Some(self.alone_along((root, along), &place, "fennec_unshare")),
                Kind::List(_) if step > 0 => {
                    self.alone_along((root, along), &place, "fennec_list_unshare");
                    None
                }
                _ => None,
            };
            let Some(name) = path.get(step) else { break };
            let inside = match block {
                Some(block) => self.offset(&block, FIELDS_OFFSET),
                None => place,
            };
            let Ok((offset, inner)) = self.field(&kind, name) else { break };
            place = self.offset(&inside, offset);
            kind = inner;
        }
        Ok(())
    }

    pub(crate) fn set_index(
        &mut self,
        object: LocalId,
        path: &[String],
        index: &Operand,
        value: &Operand,
        span: Span,
    ) -> Result<(), String> {
        let inside = path.is_empty() && self.known.inside(object, index);
        let kind = self.place_kind(object, path)?;
        if self.is_id_map(&kind) {
            let place = self.place_of(object, path)?;
            return self.id_map_store(&place, &kind, index, value, span);
        }
        if let Kind::Dict(key, value_word) = kind {
            let place = self.place_of(object, path)?;
            // A lookup on this dict, or on a local read from this field holding it: where it
            // found the key, or for a slice where it found the value.
            let same_dict = |dict: LocalId| match path {
                [] => dict == object,
                [field] => {
                    self.known.field_of(dict).is_some_and(|(from, name)| *from == object && name == field)
                }
                _ => false,
            };
            let found = self
                .found_entries
                .iter()
                .rev()
                .find(|(dict, looked_up, _)| same_dict(*dict) && looked_up == index)
                .map(|(_, _, entry)| entry.clone());
            if let Operand::Local(sliced) = index
                && let Some(slice) = self.known.slice_keys.get(&sliced.0).cloned()
            {
                return self.slice_set(&place, (key, value_word), (&slice, found), value);
            }
            return self.dict_store_at(&place, (key, value_word), index, Some(value), found);
        }
        let item = list_item(&kind)?;
        let held = self.item_kind(item)?;
        let put_back = matches!(value, Operand::Local(local) if self.known.is_taken(*local));
        // Taken from here, when the list was made alone, and nothing's touched it since.
        let from_here = match value {
            Operand::Local(local)
                if self
                    .known
                    .item_home(*local)
                    .is_some_and(|(home, at)| *home == object && at.as_slice() == path) =>
            {
                self.taken_at.get(&local.0).cloned()
            }
            _ => None,
        };
        let index_operand = index;
        let (index, _) = self.operand(index)?;
        let (value, fresh) = self.value_for(value, &held)?;
        let memory = held.memory(self.module.layouts);
        let at = if let Some(at) = from_here {
            at
        } else {
            let list = if path.is_empty() { self.unshared(object) } else { self.alone_list(object, path)? };
            let position = if inside {
                index
            } else {
                let place = self.list_key(object, path);
                self.position_once(place, (&list, index_operand, &index), span)
            };
            self.item_address(&list, &memory, &position)
        };
        if put_back && held.counted() {
            let text = self.text_of(&held);
            self.line(&format!("store {text} {value}, ptr {at}{ITEM_ACCESS}"));
        } else if held.counted() {
            if !fresh {
                self.retain_value(&value, &held);
            }
            let (old, text) = (self.temp(), self.text_of(&held));
            self.line(&format!("{old} = load {text}, ptr {at}{ITEM_ACCESS}"));
            self.line(&format!("store {text} {value}, ptr {at}{ITEM_ACCESS}"));
            self.release_kind(&old, &held);
        } else {
            // An inline struct's blocks: the old item's let go of, the new one's owned, unless
            // it's the item that was taken out coming back.
            if !put_back {
                self.release_inline(&held, &at);
            }
            self.store_field(&held, &value, &at, ITEM_ACCESS);
            if !put_back {
                self.retain_inline(&held, &at);
            }
        }
        Ok(())
    }

    /// `object[start:end]`, a missing end being the lowest int, as the runtime reads it.
    pub(crate) fn slice(
        &mut self,
        dest: LocalId,
        object: &Operand,
        start: Option<&Operand>,
        end: Option<&Operand>,
        span: Span,
    ) -> Result<(), String> {
        let (object, kind) = self.operand(object)?;
        let lowest = i64::MIN.to_string();
        let start =
            start.map(|start| self.operand(start)).transpose()?.map_or_else(|| lowest.clone(), |held| held.0);
        let end = end.map(|end| self.operand(end)).transpose()?.map_or(lowest, |held| held.0);
        let part = self.temp();
        match kind {
            Kind::Str if self.headers() => {
                let part = self.header_slice(&object, (&start, &end), span);
                return self.assign(dest, &part);
            }
            Kind::Str => {
                self.declare("declare ptr @fennec_str_slice(ptr, i64, i64, i64, i64) nounwind");
                let (from, to) = (i64::from(span.start), i64::from(span.end));
                self.on_blocks([&object], |emit, [block]| {
                    emit.line(&format!("{part} = call ptr @fennec_str_slice(ptr {block}, i64 {start}, i64 {end}, i64 {from}, i64 {to})"));
                });
                self.leave_if_failed();
                let part = self.text_value(&part);
                return self.assign(dest, &part);
            }
            Kind::List(_) => {
                self.declare("declare ptr @fennec_list_slice(ptr, i64, i64) nounwind");
                self.line(&format!(
                    "{part} = call ptr @fennec_list_slice(ptr {object}, i64 {start}, i64 {end})"
                ));
            }
            other => return Err(format!("it slices a {other:?}, which this backend doesn't take yet")),
        }
        self.assign(dest, &part)
    }

    pub(crate) fn make_list(&mut self, dest: LocalId, items: &[Operand], block: usize) -> Result<(), String> {
        // Pieces kept as the string they're joined into start as their own text.
        if matches!(self.kind_of(dest)?, Kind::Str | Kind::TextBlock) {
            self.declare("declare ptr @fennec_str_with_room(i64) nounwind");
            let mut text = self.temp();
            self.line(&format!("{text} = call ptr @fennec_str_with_room(i64 {PIECES_ROOM})"));
            for item in items {
                let (piece, _) = self.operand(item)?;
                text = self.append_piece(&text, &piece);
            }
            return self.assign(dest, &text);
        }
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
            let at = self.item_address(&list, &memory, &position.to_string());
            self.fill(&held, value, &at, ITEM_ACCESS)?;
        }
        self.assign(dest, &list)
    }

    /// Makes an empty list with room for what the loop after this block adds, or, for a loop
    /// adding only zeros, the whole list, as a new one is zero.
    pub(crate) fn reserve(&mut self, dest: LocalId, limit: &Operand) -> Result<(), String> {
        let kind = self.kind_of(dest)?.clone();
        let item = list_item(&kind)?;
        let (count, _) = self.operand(limit)?;
        let (above, room) = (self.temp(), self.temp());
        self.line(&format!("{above} = icmp sgt i64 {count}, 0"));
        self.line(&format!("{room} = select i1 {above}, i64 {count}, i64 0"));
        let helper = if self.known.zero_filled.contains(&dest.0) {
            "fennec_list_new"
        } else {
            "fennec_list_with_room"
        };
        let list = self.new_list_with(item, &room, helper);
        self.assign(dest, &list)
    }

    /// `list.append(value)`: in place when nothing else holds the list and there's room.
    pub(crate) fn append(&mut self, args: &[Operand]) -> Result<(), String> {
        let Some(Operand::Local(local)) = args.first() else {
            return Err("it appends to a literal".to_owned());
        };
        // A piece goes on the end of the string it'll be joined into.
        if matches!(self.kind_of(*local)?, Kind::Str | Kind::TextBlock) {
            let (piece, _) = self.operand(args.get(1).ok_or("no argument")?)?;
            let (slot, text) = (self.slot(*local), self.temp());
            self.line(&format!("{text} = load ptr, ptr {slot}"));
            let joined = self.append_piece(&text, &piece);
            self.line(&format!("store ptr {joined}, ptr {slot}"));
            return Ok(());
        }
        // Made whole already, of zeros.
        if self.known.zero_filled.contains(&local.0) {
            return Ok(());
        }
        let kind = self.kind_of(*local)?.clone();
        let item = list_item(&kind)?;
        let held = self.item_kind(item)?;
        let new_item = args.get(1).ok_or("no argument")?;
        self.declare("declare ptr @fennec_list_grow(ptr) nounwind");
        let slot = self.slot(*local);
        let (list, length, room_at, room) = (self.temp(), self.temp(), self.temp(), self.temp());
        let (fits, ready, longer) = (self.temp(), self.temp(), self.temp());
        let (quick, grow, done) = (self.label("quick"), self.label("grow"), self.label("done"));
        self.line(&format!("{list} = load ptr, ptr {slot}"));
        let length_at = self.offset(&list, u32::try_from(LIST_LENGTH).unwrap_or(0));
        self.line(&format!("{length} = load i64, ptr {length_at}{LENGTH_ACCESS}"));
        self.line(&format!("{room_at} = getelementptr inbounds i8, ptr {list}, i64 {LIST_ROOM}"));
        self.line(&format!("{room} = load i64, ptr {room_at}{HEADER_ACCESS}"));
        self.line(&format!("{fits} = icmp ult i64 {length}, {room}"));
        if self.known.alone_once_checked(*local) {
            self.line(&format!("{ready} = and i1 {fits}, true"));
        } else {
            let alone = self.alone(&list);
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
        self.fill(&held, new_item, &at, ITEM_ACCESS)?;
        Ok(())
    }
}

pub(crate) fn list_item(list: &Kind) -> Result<ListItem, String> {
    match list {
        Kind::List(item) => Ok(*item),
        other => Err(format!("it indexes a {other:?}, which this backend doesn't take yet")),
    }
}

impl Emit<'_, '_, '_> {
    /// `insert` on a list or a buffer: the runtime opens a zeroed slot at the place, failing
    /// outside it as the interpreter says, and the item goes in it.
    pub(crate) fn insert(&mut self, args: &[Operand], call: &str, span: Span) -> Result<(), String> {
        let [Operand::Local(list), at, value] = args else {
            return Err("`insert` with the wrong arguments".to_owned());
        };
        let (from, to) = (i64::from(span.start), i64::from(span.end));
        let item = list_item(&self.kind_of(*list)?.clone())?;
        let held = self.item_kind(item)?;
        let (place, _) = self.operand(at)?;
        self.declare(&format!("declare ptr @{call}(ptr, i64, i64, i64) nounwind"));
        let (slot, old, grown) = (self.slot(*list), self.temp(), self.temp());
        self.line(&format!("{old} = load ptr, ptr {slot}"));
        self.line(&format!("{grown} = call ptr @{call}(ptr {old}, i64 {place}, i64 {from}, i64 {to})"));
        self.leave_if_failed();
        self.line(&format!("store ptr {grown}, ptr {slot}"));
        let memory = held.memory(self.module.layouts);
        let opened = self.item_address(&grown, &memory, &place);
        self.fill(&held, value, &opened, ITEM_ACCESS)
    }

    /// `Buffer[T]`'s methods: a buffer is a list's block, with its room for all to see.
    pub(crate) fn buffer_builtin(
        &mut self,
        dest: Option<LocalId>,
        name: &str,
        args: &[Operand],
        span: Span,
    ) -> Result<(), String> {
        let (from, to) = (i64::from(span.start), i64::from(span.end));
        match (name, args) {
            ("Buffer.new" | "Buffer.new.capacity", _) => {
                let dest = dest.ok_or("no result")?;
                let item = list_item(&self.kind_of(dest)?.clone())?;
                let room = match args.first() {
                    Some(asked) => {
                        let (asked, _) = self.operand(asked)?;
                        let (more, room) = (self.temp(), self.temp());
                        self.line(&format!("{more} = icmp sgt i64 {asked}, 4"));
                        self.line(&format!("{room} = select i1 {more}, i64 {asked}, i64 4"));
                        room
                    }
                    None => "4".to_owned(),
                };
                let list = self.new_list_with(item, &room, "fennec_list_with_room");
                self.assign(dest, &list)
            }
            ("Buffer.push", _) => self.append(args),
            ("Buffer.pop", [Operand::Local(buffer)]) => {
                // Empty, it fails as the interpreter says, rather than as a list's pop does.
                let slot = self.slot(*buffer);
                let list = self.temp();
                self.line(&format!("{list} = load ptr, ptr {slot}"));
                let length = self.list_length(&list);
                let (empty, bad, ok) = (self.temp(), self.label("empty"), self.label("popped"));
                self.line(&format!("{empty} = icmp eq i64 {length}, 0"));
                self.line(&format!("br i1 {empty}, label %{bad}, label %{ok}, !prof !0"));
                self.start_block(&bad);
                self.declare("declare void @fennec_buffer_pop_empty(i64, i64) nounwind");
                self.line(&format!("call void @fennec_buffer_pop_empty(i64 {from}, i64 {to})"));
                let failed = self.failed_exit();
                self.line(&failed);
                self.start_block(&ok);
                self.pop(dest, args, span)
            }
            ("Buffer.len", [buffer]) => {
                let (list, _) = self.operand(buffer)?;
                let length = self.list_length(&list);
                self.assign(dest.ok_or("no result")?, &length)
            }
            ("Buffer.capacity", [buffer]) => {
                let (list, _) = self.operand(buffer)?;
                let (at, room) = (self.temp(), self.temp());
                self.line(&format!("{at} = getelementptr inbounds i8, ptr {list}, i64 {LIST_ROOM}"));
                self.line(&format!("{room} = load i64, ptr {at}{HEADER_ACCESS}"));
                self.assign(dest.ok_or("no result")?, &room)
            }
            ("Buffer.is_unique", [buffer]) => {
                let (list, _) = self.operand(buffer)?;
                let alone = self.alone(&list);
                self.assign(dest.ok_or("no result")?, &alone)
            }
            // The runtime takes the owner over and gives back the list to keep.
            ("Buffer.reserve" | "Buffer.truncate", [Operand::Local(buffer), count]) => {
                let call =
                    if name == "Buffer.reserve" { "fennec_list_reserve" } else { "fennec_list_truncate" };
                let (count, _) = self.operand(count)?;
                self.declare(&format!("declare ptr @{call}(ptr, i64) nounwind"));
                let (slot, list, changed) = (self.slot(*buffer), self.temp(), self.temp());
                self.line(&format!("{list} = load ptr, ptr {slot}"));
                self.line(&format!("{changed} = call ptr @{call}(ptr {list}, i64 {count})"));
                self.line(&format!("store ptr {changed}, ptr {slot}"));
                Ok(())
            }
            ("Buffer.extend", [Operand::Local(buffer), other]) => {
                let (other, _) = self.operand(other)?;
                self.declare("declare ptr @fennec_list_extend(ptr, ptr) nounwind");
                let (slot, list, grown) = (self.slot(*buffer), self.temp(), self.temp());
                self.line(&format!("{list} = load ptr, ptr {slot}"));
                self.line(&format!("{grown} = call ptr @fennec_list_extend(ptr {list}, ptr {other})"));
                self.line(&format!("store ptr {grown}, ptr {slot}"));
                Ok(())
            }
            ("Buffer.insert", _) => self.insert(args, "fennec_buffer_insert", span),
            ("Buffer.remove", [_, _]) => self.pop(dest, args, span),
            _ => Err(format!("it calls `{name}`, which this backend doesn't take yet")),
        }
    }
}
