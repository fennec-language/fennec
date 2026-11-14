//! Plain structs, tuples and lists, laid out as the runtime lays them out.

use std::fmt::Write as _;

use fennec_ir::{InstKind, LocalId, Operand, PlaceStep};
use fennec_layout::ListItem;
use fennec_rt::embed::layout::{FIELDS, LIST_ITEMS, LIST_LENGTH, LIST_ROOM};
use fennec_syntax::{BinOp, Span};

use crate::emit::{Emit, FIELD_ACCESS, HEADER_ACCESS, ITEM_ACCESS, VALUE_ACCESS};
use crate::kinds::{Kind, element_symbol};
use crate::uniqueness::{FieldPath, item_step, list_place};

/// Something an inline struct or enum owns, at its offset.
#[derive(Clone)]
pub(crate) enum InlinePart {
    /// A block, always there.
    Block(u32, Kind),
    /// An enum of this layout, whose blocks depend on its case.
    Enum(u32, u32),
    /// An array's items, from where it starts: how many, how far apart, and what each owns,
    /// counted in a loop rather than item by item.
    Items(u32, u32, u32, Vec<InlinePart>),
}

/// Where a shared enum case's block starts counting: 2^62, which is 2^14 times the mark for a
/// block that's never freed (`IMMORTAL`, 2^48), so a count off by one now and then stays above it.
const SHARED_CASE_COUNT: u64 = fennec_rt::embed::heap::IMMORTAL << 14;

impl InlinePart {
    fn shifted(self, by: u32) -> InlinePart {
        match self {
            InlinePart::Block(offset, held) => InlinePart::Block(offset + by, held),
            InlinePart::Enum(offset, id) => InlinePart::Enum(offset + by, id),
            InlinePart::Items(offset, count, stride, parts) => {
                InlinePart::Items(offset + by, count, stride, parts)
            }
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

    pub(crate) fn layout_of(&self, id: u32) -> Result<&'_ fennec_layout::Layout, String> {
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
        if self.is_native(id) {
            return self.new_native_block(id);
        }
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
        // A block in a local read only here is handed over, as a copy from one is: the list a
        // struct is made with, built just before, then has one owner.
        let handed = match operand {
            Operand::Local(local)
                if kind.counted()
                    && !fresh
                    && self.owns(*local)
                    && (self.known.moves(*local) || self.made_here_last(*local)) =>
            {
                Some(*local)
            }
            _ => None,
        };
        if kind.counted() && !fresh && handed.is_none() {
            self.retain_value(&value, kind);
        }
        self.store_field(kind, &value, at, access);
        if let Some(local) = handed {
            let (slot, text) = (self.slot(local), self.text_of(kind));
            self.line(&format!("store {text} zeroinitializer, ptr {slot}"));
        }
        // An inline struct in a local read only here moves its blocks over; any other gets
        // another owner for each.
        let moved = !fresh
            && matches!(operand, Operand::Local(local) if self.owns_inline(*local) && self.known.moves(*local));
        if moved {
            let zero = self.zero_line(kind, &value);
            self.line(&zero);
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
        if let Some(shape) = self.array_shape(kind).filter(|shape| !shape.boxed) {
            let each = match &shape.item {
                Kind::Struct(_) => self.inline_parts(&shape.item),
                Kind::Enum(id) if !self.case_parts(*id).is_empty() => vec![InlinePart::Enum(0, *id)],
                held if held.counted() => vec![InlinePart::Block(0, held.clone())],
                _ => Vec::new(),
            };
            return if each.is_empty() {
                Vec::new()
            } else {
                vec![InlinePart::Items(0, shape.count, shape.stride, each)]
            };
        }
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
            InlinePart::Items(offset, count, stride, parts) => {
                let base = self.offset(at, offset);
                self.each_position(&count.to_string(), |emit, position| {
                    let item = emit.temp();
                    emit.line(&format!(
                        "{item} = getelementptr inbounds [{stride} x i8], ptr {base}, i64 {position}"
                    ));
                    for part in parts.clone() {
                        emit.count_part(part, &item, more);
                    }
                    Ok(())
                })
                .unwrap_or_default();
            }
        }
    }

    /// Marks each block a constant held in place holds as living for the whole run, as a constant
    /// held in a block is, so a task anywhere can read it and it's never counted as left over.
    pub(crate) fn keep_inline(&mut self, kind: &Kind, at: &str) {
        for part in self.inline_parts(kind) {
            self.keep_part(part, at);
        }
    }

    fn keep_part(&mut self, part: InlinePart, at: &str) {
        match part {
            InlinePart::Block(offset, held) => {
                let place = self.offset(at, offset);
                let block = match &held {
                    // A string's block, when its header holds one: null when it's inline or `None`.
                    Kind::Str | Kind::MaybeText if self.headers() => {
                        let header = self.temp();
                        self.line(&format!("{header} = load {}, ptr {place}", crate::headers::HEADER));
                        self.header_block(&header)
                    }
                    Kind::Str
                    | Kind::List(_)
                    | Kind::Dict(..)
                    | Kind::Boxed(_)
                    | Kind::Bytes
                    | Kind::Maybe(_) => {
                        let block = self.temp();
                        self.line(&format!("{block} = load ptr, ptr {place}"));
                        block
                    }
                    // A closure, a generator or a capability: never a constant's.
                    _ => return,
                };
                let none = self.temp();
                self.line(&format!("{none} = icmp eq ptr {block}, null"));
                let (keep, done) = (self.label("keep_block"), self.label("kept_block"));
                self.line(&format!("br i1 {none}, label %{done}, label %{keep}"));
                self.start_block(&keep);
                self.declare("declare void @fennec_constant_kept(ptr) nounwind");
                self.line(&format!("call void @fennec_constant_kept(ptr {block})"));
                self.line(&format!("br label %{done}"));
                self.start_block(&done);
            }
            InlinePart::Enum(offset, id) => {
                let (base, tag, done) = (self.offset(at, offset), self.temp(), self.label("kept_case"));
                self.line(&format!("{tag} = load i64, ptr {base}"));
                for (number, parts) in self.case_parts(id) {
                    let (this, next, same) =
                        (self.label("keep_case"), self.label("not_kept_case"), self.temp());
                    self.line(&format!("{same} = icmp eq i64 {tag}, {number}"));
                    self.line(&format!("br i1 {same}, label %{this}, label %{next}"));
                    self.start_block(&this);
                    for part in parts {
                        self.keep_part(part, &base);
                    }
                    self.line(&format!("br label %{done}"));
                    self.start_block(&next);
                }
                self.line(&format!("br label %{done}"));
                self.start_block(&done);
            }
            InlinePart::Items(offset, count, stride, parts) => {
                let base = self.offset(at, offset);
                self.each_position(&count.to_string(), |emit, position| {
                    let item = emit.temp();
                    emit.line(&format!(
                        "{item} = getelementptr inbounds [{stride} x i8], ptr {base}, i64 {position}"
                    ));
                    for part in parts.clone() {
                        emit.keep_part(part, &item);
                    }
                    Ok(())
                })
                .unwrap_or_default();
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

    /// Whether the struct being made here is the last thing to read a local it's made with.
    fn made_here_last(&self, local: LocalId) -> bool {
        self.known.is_last_made((self.at.0, self.at.1), local)
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
        let given = fields.len();
        for ((offset, held), value) in written.iter().cloned().zip(fields) {
            let held = held.ok_or("a field this backend doesn't take yet")?;
            let at = self.offset(&base, offset);
            self.fill(&held, value, &at, access)?;
        }
        // What the IR doesn't give, an error's notes, starts with none.
        for (offset, _) in written.iter().skip(given) {
            let at = self.offset(&base, *offset);
            self.line(&format!("store i64 0, ptr {at}{access}"));
        }
        if let (Some(block), true) = (&block, self.module.threads && self.is_cell(&kind)) {
            // Before anything else holds it: what it was given goes in its table.
            self.declare("declare ptr @fennec_shared_new(ptr) nounwind");
            let made = self.temp();
            self.line(&format!("{made} = call ptr @fennec_shared_new(ptr {block})"));
            self.leave_if_failed();
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
            let held = self.place_kind(root, &list_path)?;
            if self.is_id_map(&held) {
                let span = self.here_span();
                return self.take_from_table_item(dest, (root, &list_path, &index), field, span);
            }
            if matches!(held, Kind::Dict(..)) {
                let span = self.here_span();
                return self.take_from_dict_value(dest, (root, &list_path, &index), field, span);
            }
            return self.take_from_item(dest, (root, &list_path, &index), field);
        }
        let (base, kind) = self.operand(object)?;
        // A field taken out to change: its count only means something if what holds it is alone.
        let base = match object {
            Operand::Local(local) if self.known.is_taken(dest) && matches!(kind, Kind::Boxed(_)) => {
                let place = self.slot(*local);
                self.block_to_change((*local, &[]), &place, &kind)
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
        let span = self.here_span();
        self.proof_inside(&list, &position, "item read just before still inside its list", span);
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
                self.block_to_change((root, &along), &at, &held)
            }
            None if self.is_cell(&held) => {
                let block = self.temp();
                self.line(&format!("{block} = load ptr, ptr {at}"));
                block
            }
            None => self.alone_at(&at, "fennec_unshare"),
        };
        let (offset, inner) = self.field(&held, field)?;
        let fields = self.fields_at(&block, &held);
        let place = self.offset(&fields, offset);
        let value = self.load_field(&inner, &place, FIELD_ACCESS);
        self.assign(dest, &value)
    }

    /// What's done for an instruction reading a place out for a call or writing it back, if it
    /// is one: the place is taken out when it's reached, from the root down, each block on the
    /// way made alone before anything inside it is. So its one owner is the place, and a block's
    /// count is only given up where this thread holds it. Everything else on the way isn't read.
    pub(crate) fn place_inst(&mut self, inst: &InstKind, span: Span) -> Option<Result<(), String>> {
        match inst {
            InstKind::GetField { dest, .. } | InstKind::GetIndex { dest, .. } => {
                if self.known.is_place_part(*dest) {
                    return Some(Ok(()));
                }
                self.known.place_taken(self.function, *dest)?;
                // Failing to reach it comes first, as it's written, and the rest can wait.
                if self.known.place_waits(*dest) {
                    return Some(self.walk_place(*dest, false, span).map(|_| ()));
                }
                Some(self.take_place(*dest, span))
            }
            InstKind::SetField { object, value: Operand::Local(put), .. }
            | InstKind::SetIndex { object, value: Operand::Local(put), .. } => {
                if self.known.is_place_part(*object) {
                    return Some(Ok(()));
                }
                let holder = self.known.place_holder(*put)?;
                Some(self.put_place_back(holder))
            }
            _ => None,
        }
    }

    /// Takes out each place this instruction changes that was waiting for it.
    pub(crate) fn take_waiting_places(&mut self, inst: &InstKind, span: Span) -> Result<(), String> {
        let (InstKind::Call { args, .. } | InstKind::Builtin { args, .. }) = inst else { return Ok(()) };
        for arg in args {
            if let Operand::Local(local) = arg
                && self.known.place_waits(*local)
            {
                self.take_place(*local, span)?;
            }
        }
        Ok(())
    }

    fn take_place(&mut self, value: LocalId, span: Span) -> Result<(), String> {
        let (at, kind, access) = self.walk_place(value, true, span)?;
        self.place_homes.insert(value.0, (at.clone(), kind.clone(), access));
        self.note_place_out(value, &at);
        self.read_into(value, &kind, &at, access)
    }

    /// Where a place is, from its root. Taking it, each block on the way is made alone first;
    /// otherwise this only fails if an index is outside its list.
    fn walk_place(
        &mut self,
        value: LocalId,
        taking: bool,
        span: Span,
    ) -> Result<(String, Kind, &'static str), String> {
        let function = self.function;
        let place = self.known.place_taken(function, value).ok_or("a place that isn't taken")?;
        let root = place.root;
        // What a call changes through it is put back by a crash or a failure, if it's a lock's.
        if taking && !self.saving.is_empty() {
            let fields: Vec<String> = place
                .steps
                .iter()
                .map_while(|step| match step {
                    PlaceStep::Field(name) => Some(name.clone()),
                    PlaceStep::Item(_) => None,
                })
                .collect();
            self.save_along(root, &fields)?;
        }
        let mut kind = self.kind_of(root)?.clone();
        let mut at = self.slot(root);
        let mut along: Vec<String> = Vec::new();
        let mut access = "";
        for step in &place.steps {
            match step {
                PlaceStep::Field(name) => {
                    let boxed = matches!(kind, Kind::Boxed(_));
                    let inside = if boxed {
                        // A `shared` value's block is the same one for every name.
                        let block = if taking {
                            self.block_to_change((root, &along), &at, &kind)
                        } else {
                            self.load_block(&at)
                        };
                        self.offset(&block, FIELDS_OFFSET)
                    } else {
                        at
                    };
                    let (offset, field) = self.field(&kind, name)?;
                    at = self.offset(&inside, offset);
                    kind = field;
                    if boxed {
                        access = FIELD_ACCESS;
                    }
                    along.push(name.clone());
                }
                PlaceStep::Item(index_operand) => {
                    let list = match (taking, along.is_empty()) {
                        (true, true) => self.unshared(root),
                        (true, false) if !self.saving.is_empty() => {
                            let (index, _) = self.operand(index_operand)?;
                            self.item_to_set(&at, &index)
                        }
                        (true, false) => self.alone_along((root, &along), &at, "fennec_list_unshare"),
                        (false, _) => self.load_block(&at),
                    };
                    let held = self.item_kind(list_item(&kind)?)?;
                    let (index, _) = self.operand(index_operand)?;
                    let position = if along.is_empty() && self.known.inside(root, index_operand) {
                        self.proof_inside(&list, &index, "index inside its list", span);
                        index
                    } else {
                        let known_as = self.list_key(root, &along);
                        self.position_once(known_as, (&list, index_operand, &index), span)
                    };
                    let memory = held.memory(self.module.layouts);
                    at = self.item_address(&list, &memory, &position);
                    kind = held;
                    access = ITEM_ACCESS;
                    along.push(item_step(index_operand).ok_or("an index a path can't name")?);
                }
            }
        }
        // What's known of it is known of the place, however long the local lasts.
        if let Some((step, list)) = along.split_last() {
            self.taken_items.insert(value.0, (root, list.to_vec(), step.clone()));
        }
        Ok((at, kind, access))
    }

    fn load_block(&mut self, place: &str) -> String {
        let block = self.temp();
        self.line(&format!("{block} = load ptr, ptr {place}"));
        block
    }

    /// While a place is out, in a function a crash lets go of what it owns, where it goes back to
    /// is kept beside the function's locals, for the crash to put it back there: what holds it
    /// still has the address of what's been changed, maybe moved or freed, since.
    fn note_place_out(&mut self, value: LocalId, at: &str) {
        if !self.lets_go_after_crash() {
            return;
        }
        let slot = if let Some(slot) = self.places_out.get(&value.0) {
            slot.clone()
        } else {
            let slot = self.label("%place");
            let _ = writeln!(self.allocas, "  {slot} = alloca ptr");
            let _ = writeln!(self.allocas, "  store ptr null, ptr {slot}");
            self.places_out.insert(value.0, slot.clone());
            slot
        };
        self.line(&format!("store ptr {at}, ptr {slot}"));
    }

    /// Where a crash leaves the function: each place still out goes back, before what holds it is
    /// let go of.
    pub(crate) fn put_places_back_after_crash(&mut self) -> Result<(), String> {
        let mut out: Vec<(usize, String)> =
            self.places_out.iter().map(|(value, slot)| (*value, slot.clone())).collect();
        out.sort_unstable();
        for (value, slot) in out {
            let (_, kind, access) = self.place_homes.get(&value).cloned().ok_or("a place never taken out")?;
            let (home, is_out) = (self.temp(), self.temp());
            self.line(&format!("{home} = load ptr, ptr {slot}"));
            self.line(&format!("{is_out} = icmp ne ptr {home}, null"));
            let (back, next) = (self.label("crash_place"), self.label("crash_placed"));
            self.line(&format!("br i1 {is_out}, label %{back}, label %{next}"));
            self.start_block(&back);
            self.store_place(LocalId(value), (&home, &kind, access))?;
            self.line(&format!("br label %{next}"));
            self.start_block(&next);
        }
        Ok(())
    }

    fn put_place_back(&mut self, value: LocalId) -> Result<(), String> {
        let (at, kind, access) = self.place_homes.get(&value.0).cloned().ok_or("a place never taken out")?;
        if let Some(slot) = self.places_out.get(&value.0).cloned() {
            self.line(&format!("store ptr null, ptr {slot}"));
        }
        self.store_place(value, (&at, &kind, access))
    }

    fn store_place(&mut self, value: LocalId, (at, kind, access): (&str, &Kind, &str)) -> Result<(), String> {
        let (held, _) = self.value_for(&Operand::Local(value), kind)?;
        if kind.counted() {
            let text = self.text_of(kind);
            self.line(&format!("store {text} {held}, ptr {at}{access}"));
        } else {
            self.store_field(kind, &held, at, access);
        }
        Ok(())
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
        // The block the field is in, for a `lock ... as ref` block to save it before it changes.
        let mut holder = None;
        for (step, name) in path.iter().enumerate() {
            let inside = if matches!(kind, Kind::Boxed(_)) {
                in_block = true;
                let block =
                    self.block_to_change((object, path.get(..step).unwrap_or_default()), &place, &kind);
                holder = Some(block.clone());
                self.offset(&block, FIELDS_OFFSET)
            } else {
                place
            };
            let (offset, inner) = self.field(&kind, name)?;
            place = self.offset(&inside, offset);
            kind = inner;
        }
        let access = if in_block { FIELD_ACCESS } else { VALUE_ACCESS };
        // In a `lock ... as ref` block, the old value goes to the lock's log, which may take over
        // its count, so a crash can put it back.
        let saved = match &holder {
            Some(block) if self.saving.contains(&object.0) && !put_back => {
                self.save_before_store(block, &place, &kind)
            }
            _ => false,
        };
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
            if !saved {
                self.release_kind(&old, &kind);
            }
        } else {
            if !put_back && !saved {
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
        // An optional string is a header, all zero for `None`.
        if kind == Kind::MaybeText {
            return match (case, fields) {
                ("None", _) => self.assign(dest, "zeroinitializer"),
                (_, [value]) => {
                    let (value, _) = self.operand(value)?;
                    self.retain_text(&value);
                    self.assign(dest, &value)
                }
                _ => Err("an optional string with no string".to_owned()),
            };
        }
        let (number, mut written) = self.case_of(&kind, case)?;
        // An error's notes start as 0, where the case's words are zero.
        written.retain(|(name, _, _)| !fennec_rt::packed::hidden(name));
        if let (Kind::Boxed(id), true) = (&kind, written.is_empty())
            && !crate::notes::has_notes(self.layout_of(*id)?)
        {
            let shared = self.shared_case(*id, number)?;
            self.retain_value(&shared, &kind);
            return self.assign(dest, &shared);
        }
        let block = if let Kind::Boxed(id) = kind { Some(self.new_block(id)?) } else { None };
        // Holding blocks, it's made beside what the local holds, which may be read for it.
        let beside = (block.is_none() && self.holds_inline(&kind)).then(|| self.place_for(&kind));
        let base = if let Some(block) = &block {
            self.offset(block, FIELDS_OFFSET)
        } else {
            // Words a case doesn't use are zero, so equal enums have equal words.
            let place = beside.clone().unwrap_or_else(|| self.slot(dest));
            let zero = self.zero_line(&kind, &place);
            self.line(&zero);
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

    /// The one block for case `number` of the boxed enum `id`, a case with no fields, so making
    /// one allocates nothing: the block is made the first time, counted as never freed, and
    /// kept in a global every module shares. Each value made from it is retained, so its count
    /// stays where it starts, too high for anything to take it as having one owner and change
    /// it in place, and other threads don't count it at all, as with a string literal's block. Two threads making the
    /// first one at once each make a block, and either serves.
    ///
    /// It starts at [`SHARED_CASE_COUNT`], far above the mark for a block that's never freed: a
    /// closure on the program's own thread counts as a worker does, so it skips the block, while
    /// that thread's other code and its runtime count it. A value one makes and another lets go
    /// of takes one off, and this way no run lets that many go.
    fn shared_case(&mut self, id: u32, number: i64) -> Result<String, String> {
        use fennec_rt::embed::heap::{NEVER_FREED_OWNER, OWNER_SHIFT};
        let global = format!("@\"fen_case_{id}_{number}\"");
        self.declare(&format!("{global} = linkonce_odr hidden global ptr null, align 8"));
        let (kept, none) = (self.temp(), self.temp());
        // Acquire, pairing with the release below: a thread that sees the block sees what was
        // written in it, its count and tag, before it was put here.
        self.line(&format!("{kept} = load atomic ptr, ptr {global} acquire, align 8"));
        self.line(&format!("{none} = icmp eq ptr {kept}, null"));
        let (first, ready) = (self.label("case_first"), self.label("case_ready"));
        let before = self.current.clone();
        self.line(&format!("br i1 {none}, label %{first}, label %{ready}, !prof !0"));
        self.start_block(&first);
        let made = self.new_block(id)?;
        if self.is_native(id) {
            self.uncount_native();
        }
        self.line(&format!("store i64 {SHARED_CASE_COUNT}, ptr {made}{HEADER_ACCESS}"));
        // No thread's, as every block that's never freed is, so no thread but this one, which
        // hasn't shared it yet, reads its count.
        let (shape_at, was, shape, never) = (self.offset(&made, 8), self.temp(), self.temp(), self.temp());
        self.line(&format!("{was} = load i64, ptr {shape_at}{HEADER_ACCESS}"));
        self.line(&format!("{shape} = and i64 {was}, {}", (1_u64 << OWNER_SHIFT) - 1));
        self.line(&format!("{never} = or i64 {shape}, {}", NEVER_FREED_OWNER << OWNER_SHIFT));
        self.line(&format!("store i64 {never}, ptr {shape_at}{HEADER_ACCESS}"));
        let tag_at = self.offset(&made, FIELDS_OFFSET);
        self.line(&format!("store i64 {number}, ptr {tag_at}"));
        self.line(&format!("store atomic ptr {made}, ptr {global} release, align 8"));
        let made_end = self.current.clone();
        self.line(&format!("br label %{ready}"));
        self.start_block(&ready);
        let shared = self.temp();
        self.line(&format!("{shared} = phi ptr [ {kept}, %{before} ], [ {made}, %{made_end} ]"));
        Ok(shared)
    }

    /// Whether this is one of several error types, laid out with a case for each, named for it.
    /// Whether a value is of a union of plain types, like `int | str`: each of its cases holds one
    /// value.
    pub(crate) fn is_union(&self, kind: &Kind) -> bool {
        let (Kind::Enum(id) | Kind::Boxed(id)) = kind else { return false };
        self.module.layouts.get(*id).is_some_and(|layout| {
            union_at_top(&layout.name)
                && layout.cases.as_ref().is_some_and(|cases| cases.iter().all(|case| case.fields.len() == 1))
        })
    }

    pub(crate) fn is_one_of_several(&self, kind: &Kind) -> bool {
        let (Kind::Enum(id) | Kind::Boxed(id)) = kind else { return false };
        self.module.layouts.get(*id).is_some_and(|layout| {
            layout.name.contains(" | ")
                && layout
                    .cases
                    .iter()
                    .flatten()
                    .all(|case| matches!(case.fields.as_slice(), [field] if field.name == "error"))
        })
    }

    /// Whether what's held has to change to go where `wanted` goes: an error, or a result, going
    /// where more kinds of error can be.
    pub(crate) fn widens(&self, held: &Kind, wanted: &Kind) -> bool {
        let result = |kind: &Kind| match kind {
            Kind::Enum(id) | Kind::Boxed(id) => {
                self.module.layouts.get(*id).is_some_and(|layout| layout.name.starts_with("Result["))
            }
            _ => false,
        };
        // What's in a one-word optional is that word: taking it out widens nothing.
        let inside = matches!(held, Kind::Maybe(inner) if **inner == *wanted);
        held != wanted && !inside && (self.is_one_of_several(wanted) || (result(held) && result(wanted)))
    }

    /// `value` as a `wanted`, with owners of its own: a block, or an address in this frame.
    pub(crate) fn widened(&mut self, value: &str, held: &Kind, wanted: &Kind) -> Result<String, String> {
        let at = self.label("%widened");
        let text = self.text_of(wanted);
        let _ = writeln!(self.allocas, "  {at} = alloca {text}");
        self.widen_into(value, held, wanted, &at)?;
        if wanted.in_memory() {
            return Ok(at);
        }
        let block = self.temp();
        self.line(&format!("{block} = load ptr, ptr {at}"));
        Ok(block)
    }

    /// Where an enum's words start, whether it's a block or in place.
    fn enum_words(&mut self, value: &str, kind: &Kind) -> String {
        if matches!(kind, Kind::Boxed(_)) { self.offset(value, FIELDS_OFFSET) } else { value.to_owned() }
    }

    /// The value in a field at `place`, as `kind` passes it around.
    fn field_at(&mut self, place: String, kind: &Kind) -> String {
        if kind.in_memory() { place } else { self.load_field(kind, &place, "") }
    }

    /// Writes `value` at `at` as a `wanted`, with owners of its own.
    fn widen_into(&mut self, value: &str, held: &Kind, wanted: &Kind, at: &str) -> Result<(), String> {
        if !self.widens(held, wanted) {
            self.store_field(wanted, value, at, "");
            if wanted.counted() {
                self.retain_value(value, wanted);
            } else {
                self.retain_inline(wanted, at);
            }
            return Ok(());
        }
        let block = if let Kind::Boxed(id) = wanted { Some(self.new_block(*id)?) } else { None };
        let base = if let Some(block) = &block {
            self.offset(block, FIELDS_OFFSET)
        } else {
            let zero = self.zero_line(wanted, at);
            self.line(&zero);
            at.to_owned()
        };
        if self.is_one_of_several(wanted) && !self.is_one_of_several(held) {
            let (Kind::Struct(id) | Kind::Enum(id) | Kind::Boxed(id)) = held else {
                return Err(format!("it gives a {held:?} where one of several errors goes"));
            };
            let name = self.module.layouts.get(*id).ok_or("no such layout")?.name.clone();
            let (number, written) = self.case_of(wanted, &name)?;
            let (_, offset, _) = written.into_iter().next().ok_or("a case of an error holds nothing")?;
            self.line(&format!("store i64 {number}, ptr {base}"));
            let inside = self.offset(&base, offset);
            self.widen_into(value, held, held, &inside)?;
        } else {
            self.widen_cases(value, held, wanted, &base)?;
        }
        if let Some(block) = block {
            self.line(&format!("store ptr {block}, ptr {at}"));
        }
        Ok(())
    }

    /// Writes each case `value` might be as the case of `wanted` with the same name.
    fn widen_cases(&mut self, value: &str, held: &Kind, wanted: &Kind, base: &str) -> Result<(), String> {
        let (Kind::Enum(id) | Kind::Boxed(id)) = held else {
            return Err(format!("it gives a {held:?} where a result goes"));
        };
        let layouts = self.module.layouts;
        let cases =
            layouts.get(*id).and_then(|layout| layout.cases.as_ref()).ok_or("an enum with no cases")?;
        let from = self.enum_words(value, held);
        let tag = self.temp();
        self.line(&format!("{tag} = load i64, ptr {from}"));
        let done = self.label("widened");
        for (number, case) in cases.iter().enumerate() {
            let (same, this, next) = (self.temp(), self.label("widen"), self.label("widen_not"));
            self.line(&format!("{same} = icmp eq i64 {tag}, {number}"));
            self.line(&format!("br i1 {same}, label %{this}, label %{next}"));
            self.start_block(&this);
            let (mine, written) = self.case_of(wanted, &case.name)?;
            self.line(&format!("store i64 {mine}, ptr {base}"));
            for (field, (_, offset, to)) in case.fields.iter().zip(written) {
                let from_kind =
                    Kind::from_ty(layouts, field.ty).ok_or("a field this backend doesn't take yet")?;
                let to = to.ok_or("a field this backend doesn't take yet")?;
                let place = self.offset(&from, field.offset);
                let inside = self.field_at(place, &from_kind);
                let target = self.offset(base, offset);
                self.widen_into(&inside, &from_kind, &to, &target)?;
            }
            self.line(&format!("br label %{done}"));
            self.start_block(&next);
        }
        self.line(&format!("br label %{done}"));
        self.start_block(&done);
        Ok(())
    }

    /// One of several errors read as the one type a test has shown it is.
    pub(crate) fn as_error(&mut self, dest: LocalId, value: &Operand) -> Result<(), String> {
        let (Kind::Struct(id) | Kind::Enum(id) | Kind::Boxed(id)) = *self.kind_of(dest)? else {
            return Err("an error read as something else".to_owned());
        };
        let name = self.module.layouts.get(id).ok_or("no such layout")?.name.clone();
        self.get_case_field(dest, value, &name, "error")
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
        // What's in an optional string that isn't `None` is its header.
        if matches!(value, Operand::Local(local) if *self.kind_of(*local)? == Kind::MaybeText) {
            let (held, _) = self.operand(value)?;
            self.retain_text(&held);
            return self.assign(dest, &held);
        }
        let (base, kind) = self.operand(value)?;
        let (_, written) = self.case_of(&kind, case)?;
        let (_, offset, held) =
            written.into_iter().find(|(name, _, _)| name == field).ok_or("no such field")?;
        let held = held.ok_or("a field this backend doesn't take yet")?;
        let fields = self.fields_at(&base, &kind);
        let at = self.offset(&fields, offset);
        self.read_into(dest, &held, &at, "")
    }

    /// After a result's value or error is read out with an owner of its own, where that's the
    /// last anything does with a result kept in place: the result is let go of and left holding
    /// nothing, so what was read has one owner again, and a change to it copies nothing.
    pub(crate) fn let_go_after_last_read(&mut self, dest: LocalId, value: &Operand) -> Result<(), String> {
        let Operand::Local(result) = value else { return Ok(()) };
        let kind = self.kind_of(*result)?.clone();
        if !matches!(kind, Kind::Enum(_))
            || !self.holds_inline(&kind)
            || self.known.is_borrowed(dest)
            || !self.known.is_last_read(self.at.0, self.at.1)
        {
            return Ok(());
        }
        let slot = self.slot(*result);
        self.release_inline(&kind, &slot);
        let zero = self.zero_line(&kind, &slot);
        self.line(&zero);
        Ok(())
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

    /// The fields of a plain struct, with where they are and how they're held: not an error's
    /// notes, which `==` and ordering pass over.
    fn struct_fields(&self, id: u32) -> Result<Vec<(u32, Kind)>, String> {
        let layout = self.layout_of(id)?;
        layout
            .fields
            .iter()
            .filter(|field| !fennec_rt::packed::hidden(&field.name))
            .map(|field| {
                Kind::from_ty(self.module.layouts, field.ty)
                    .map(|kind| (field.offset, kind))
                    .ok_or_else(|| "a field this backend doesn't take yet".to_owned())
            })
            .collect()
    }

    fn same_structs(&mut self, id: u32, places: (&str, &str)) -> Result<String, String> {
        if let Some(shape) = self.array_shape(&Kind::Struct(id)) {
            return self.same_arrays(&shape, places);
        }
        let fields = self.struct_fields(id)?;
        self.same_fields(&fields, places)
    }

    /// Whether these fields of what's at two addresses are all the same.
    pub(crate) fn same_fields(
        &mut self,
        fields: &[(u32, Kind)],
        (a, b): (&str, &str),
    ) -> Result<String, String> {
        let mut same = "true".to_owned();
        for (offset, kind) in fields {
            let (x_at, y_at) = (self.offset(a, *offset), self.offset(b, *offset));
            let equal = match kind {
                Kind::Struct(inner) => self.same_structs(*inner, (&x_at, &y_at))?,
                Kind::Enum(inner) => self.same_enums(BinOp::Eq, *inner, (&x_at, &y_at))?,
                _ => {
                    let (x, y) = (self.load_field(kind, &x_at, ""), self.load_field(kind, &y_at, ""));
                    match kind {
                        Kind::Int | Kind::U64 | Kind::Bool => {
                            let equal = self.temp();
                            let text = if *kind == Kind::Bool { "i1" } else { "i64" };
                            self.line(&format!("{equal} = icmp eq {text} {x}, {y}"));
                            equal
                        }
                        // An `f32` is held as the `double` that's exactly its value.
                        Kind::Float | Kind::F32 => self.float_compare(BinOp::Eq, (&x, &y)),
                        Kind::Str if self.headers() => self.same_headers(&x, &y),
                        Kind::MaybeText => self.same_maybe_headers(&x, &y),
                        Kind::Str => self.string_binary(BinOp::Eq, (&x, &y))?,
                        // A block of any kind goes to the runtime, which compares it by its shape,
                        // as it does a boxed value's.
                        Kind::Boxed(_)
                        | Kind::List(_)
                        | Kind::Dict(..)
                        | Kind::Bytes
                        | Kind::Cap
                        | Kind::TextBlock
                        | Kind::Closure(..)
                        | Kind::Gen => self.same_blocks(BinOp::Eq, (&x, &y)),
                        Kind::Maybe(_) => self.same_maybe_blocks(&x, &y),
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

    /// `==` on two optionals of a block, each the block's address or null for `None`: the same
    /// address is equal, a `None` equals only a `None`, and two blocks compare by their shape.
    fn same_maybe_blocks(&mut self, x: &str, y: &str) -> String {
        let (same, x_none, y_none, either, decided) =
            (self.temp(), self.temp(), self.temp(), self.temp(), self.temp());
        self.line(&format!("{same} = icmp eq ptr {x}, {y}"));
        self.line(&format!("{x_none} = icmp eq ptr {x}, null"));
        self.line(&format!("{y_none} = icmp eq ptr {y}, null"));
        self.line(&format!("{either} = or i1 {x_none}, {y_none}"));
        self.line(&format!("{decided} = or i1 {same}, {either}"));
        let (quick, blocks, joined) =
            (self.label("maybe_decided"), self.label("maybe_blocks"), self.label("maybe_compared"));
        self.line(&format!("br i1 {decided}, label %{quick}, label %{blocks}"));
        self.start_block(&blocks);
        let equal = self.same_blocks(BinOp::Eq, (x, y));
        let blocks_end = self.current.clone();
        self.line(&format!("br label %{joined}"));
        self.start_block(&quick);
        self.line(&format!("br label %{joined}"));
        self.start_block(&joined);
        let result = self.temp();
        self.line(&format!("{result} = phi i1 [ {same}, %{quick} ], [ {equal}, %{blocks_end} ]"));
        result
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
                        Kind::U64 => {
                            self.line(&format!("{less} = icmp ult i64 {x}, {y}"));
                            self.line(&format!("{more} = icmp ugt i64 {x}, {y}"));
                        }
                        Kind::Bool => {
                            self.line(&format!("{less} = icmp ult i1 {x}, {y}"));
                            self.line(&format!("{more} = icmp ugt i1 {x}, {y}"));
                        }
                        Kind::Float | Kind::F32 => {
                            let (is_less, is_more) = (
                                self.float_compare(BinOp::Lt, (&x, &y)),
                                self.float_compare(BinOp::Gt, (&x, &y)),
                            );
                            self.line(&format!("{less} = or i1 {is_less}, false"));
                            self.line(&format!("{more} = or i1 {is_more}, false"));
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
        // A float doesn't compare by its bits, as `0.0` is the same as `-0.0`.
        let plain = self.case_parts(id).is_empty() && !self.holds_a_float(id);
        let same = if plain {
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

    /// Whether a boxed value of this layout is an enum's, whose case is the first word of its fields.
    pub(crate) fn is_boxed_enum(&self, id: u32) -> bool {
        self.module.layouts.get(id).is_some_and(|layout| layout.cases.is_some())
    }

    /// Whether two boxed enums are equal. Another case is unequal at once, with no call, which is
    /// what most compares of a value against a case find; the same case asks the runtime, which
    /// compares that case's fields.
    pub(crate) fn same_boxed_enums(&mut self, op: BinOp, (a, b): (&str, &str)) -> String {
        if self.checks_proofs() {
            // A value read is always there: no block is only a slot not written yet, or let go.
            let (no_a, no_b, either) = (self.temp(), self.temp(), self.temp());
            self.line(&format!("{no_a} = icmp eq ptr {a}, null"));
            self.line(&format!("{no_b} = icmp eq ptr {b}, null"));
            self.line(&format!("{either} = or i1 {no_a}, {no_b}"));
            let span = self.here_span();
            self.proof_held(&either, "an enum in a block is there", span);
        }
        let (a_at, b_at) = (self.offset(a, FIELDS_OFFSET), self.offset(b, FIELDS_OFFSET));
        let (a_case, b_case, other_case) = (self.temp(), self.temp(), self.temp());
        self.line(&format!("{a_case} = load i64, ptr {a_at}"));
        self.line(&format!("{b_case} = load i64, ptr {b_at}"));
        self.line(&format!("{other_case} = icmp ne i64 {a_case}, {b_case}"));
        let (ask, done) = (self.label("same_case"), self.label("compared"));
        let came = self.current.clone();
        self.line(&format!("br i1 {other_case}, label %{done}, label %{ask}"));
        self.start_block(&ask);
        let asked = self.same_blocks(BinOp::Eq, (a, b));
        let asked_from = self.current.clone();
        self.line(&format!("br label %{done}"));
        self.start_block(&done);
        let same = self.temp();
        self.line(&format!("{same} = phi i1 [ false, %{came} ], [ {asked}, %{asked_from} ]"));
        if op == BinOp::Eq {
            return same;
        }
        let flipped = self.temp();
        self.line(&format!("{flipped} = xor i1 {same}, true"));
        flipped
    }

    /// Whether a value of this layout has a float anywhere in it, in place.
    pub(crate) fn holds_a_float(&self, id: u32) -> bool {
        let Some(layout) = self.module.layouts.get(id) else { return false };
        let fields = layout.fields.iter().chain(layout.cases.iter().flatten().flat_map(|case| &case.fields));
        fields.into_iter().any(|field| match Kind::from_ty(self.module.layouts, field.ty) {
            Some(Kind::Float | Kind::F32) => true,
            Some(Kind::Struct(inner) | Kind::Enum(inner)) => self.holds_a_float(inner),
            _ => false,
        })
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
            // An error's notes aren't compared.
            let fields: Vec<(u32, Kind)> = case
                .fields
                .iter()
                .filter(|field| !fennec_rt::packed::hidden(&field.name))
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
        self.load_length(&length, &at);
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

    /// In a build that checks proofs, ends the program if `position` isn't inside `list`, as a
    /// proof says it is: from 0 to before its length.
    fn proof_inside(&mut self, list: &str, position: &str, proof: &str, span: Span) {
        if !self.checks_proofs() {
            return;
        }
        let length = self.list_length(list);
        let outside = self.temp();
        self.line(&format!("{outside} = icmp uge i64 {position}, {length}"));
        self.proof_held(&outside, proof, span);
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
            self.proof_inside(list, &position, "index checked before still inside its list", span);
            return position;
        }
        let position = if self.index_not_negative() {
            self.position_not_negative(list, index, span)
        } else {
            self.position(list, index, span)
        };
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

    /// Whether the index of the instruction being written is proven not negative (see `signs`).
    fn index_not_negative(&self) -> bool {
        self.module
            .whole
            .non_negative
            .get(self.function.name.as_str())
            .is_some_and(|found| found.contains(&self.at))
    }

    /// Where an index proven not negative points: one unsigned comparison with the length, with
    /// no way back from the end. A build that checks proofs checks it isn't negative.
    fn position_not_negative(&mut self, list: &str, index: &str, span: Span) -> String {
        if self.checks_proofs() {
            let negative = self.temp();
            self.line(&format!("{negative} = icmp slt i64 {index}, 0"));
            self.proof_held(&negative, "index not negative", span);
        }
        let length = self.list_length(list);
        let (inside, ok, fail) = (self.temp(), self.label("ok"), self.label("outside"));
        self.line(&format!("{inside} = icmp ult i64 {index}, {length}"));
        self.line(&format!("br i1 {inside}, label %{ok}, label %{fail}, !prof !8"));
        self.start_block(&ok);
        self.declare("declare void @fennec_out_of_range(i64, i64, i64, i64) #1");
        let (place, args) = self.span_values(&fail, span);
        self.next_check(span, Some(crate::CheckKind::Bounds));
        let failed = self.failed_exit();
        self.cold_block(&format!(
            "{fail}:\n{place}  call void @fennec_out_of_range(i64 {index}, i64 {length}, {args})\n  {failed}\n"
        ));
        index.to_owned()
    }

    /// Where an index points: from the start, or the end if it's negative, failing outside.
    pub(crate) fn position(&mut self, list: &str, index: &str, span: Span) -> String {
        let length = self.list_length(list);
        self.position_in(&length, index, span)
    }

    /// Where an index points in something this long, as [`Self::position`] says.
    pub(crate) fn position_in(&mut self, length: &str, index: &str, span: Span) -> String {
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
        self.next_check(span, Some(crate::CheckKind::Bounds));
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
        if let Some(shape) = self.array_in(object) {
            return self.array_get(dest, (object, shape), index, span);
        }
        if let Some((map, _)) = self.known.walk_items.get(&dest.0).copied() {
            if matches!(self.kind_of(map)?, Kind::Dict(..)) {
                return self.dict_walk_to_next(dest, map, index, span);
            }
            return self.walk_to_next(dest, map, index, span);
        }
        // A pair from an `enumerate` never made isn't made either: where it points in the list
        // is checked, and kept for its parts.
        if let Some((list, _)) = self.known.pairs.get(&dest.0).cloned() {
            // The pairs have the list's length, so an index known inside them is inside it.
            let inside = matches!(object, Operand::Local(pairs) if self.known.inside(*pairs, index));
            let (list, _) = self.operand(&Operand::Local(list))?;
            let (index, _) = self.operand(index)?;
            let position = if inside {
                self.proof_inside(&list, &index, "index inside its list", span);
                index
            } else {
                self.position(&list, &index, span)
            };
            let slot = self.place_for(&Kind::Int);
            self.line(&format!("store i64 {position}, ptr {slot}"));
            self.pair_slots.insert(dest.0, slot);
            // Holding nothing, letting go of it lets go of nothing.
            let kind = self.kind_of(dest)?.clone();
            let held = self.slot(dest);
            if kind.in_memory() || kind.counted() {
                let zero = self.zero_line(&kind, &held);
                self.line(&zero);
            }
            return Ok(());
        }
        let proven = matches!(object, Operand::Local(local) if self.known.inside(*local, index));
        let guarded = !proven && self.known.guarded_read(self.at);
        let inside = proven || guarded;
        if let Operand::Local(local) = object
            && self.is_id_map(&self.kind_of(*local)?.clone())
        {
            return self.id_map_index(dest, object, index, span);
        }
        let (list, kind) = self.operand(object)?;
        if kind == Kind::Cap {
            return self.args_item(dest, index, span);
        }
        if let Kind::Dict(key, value) = kind {
            return self.dict_read(dest, &list, (key, value), index, span);
        }
        if kind == Kind::Str {
            if self.headers() {
                return self.header_index(dest, &list, index, span);
            }
            return self.on_blocks([&list], |emit, [block]| emit.string_index(dest, &block, index, span));
        }
        if kind == Kind::Bytes {
            return self.bytes_index(dest, &list, index, span);
        }
        let item = list_item(&kind)?;
        let held = self.item_kind(item)?;
        // An item taken out to change: its count only means something once the list is alone,
        // and with the list alone from here, putting it back needs no checks between.
        // A local list's own check is made once, ahead of its loop, so that stays as it is.
        // So does a `for mut` item, which lets go of nothing until it's put back.
        let over_loop = self.known.taken_over_loop(dest);
        // A struct or enum item holding a counted value is one too: letting go of what it held,
        // changed, mustn't let go of what another list still holds.
        let holds_counted =
            matches!(held, Kind::Struct(id) | Kind::Enum(id) if self.module.layouts.holds_counted(id));
        let taken = self
            .known
            .item_home(dest)
            .filter(|(_, path)| held.counted() || holds_counted || !path.is_empty() || over_loop)
            .cloned();
        let list = match &taken {
            Some((home, path)) if over_loop && path.is_empty() => self.unshared(*home),
            Some((home, path)) => self.alone_list(*home, path)?,
            None => list,
        };
        let index_operand = index;
        let (index, _) = self.operand(index)?;
        let position = if inside {
            let proof =
                if guarded { "index inside its list, by the guard before" } else { "index inside its list" };
            self.proof_inside(&list, &index, proof, span);
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

    /// The list a `pop` takes its last item off: the local's, when it has one owner, and otherwise
    /// what the runtime gives back, a copy, or the same list when a lock's rollback log has marked
    /// it, which then holds the item for a crash to put back. A marked list never reads as having
    /// one owner, so nothing here says it has one after. One checked once already, on the way in,
    /// was made alone then, a marked one copied, so it isn't asked about again.
    pub(crate) fn popped_from(&mut self, local: LocalId) -> String {
        if self.known.alone_once_checked(local) {
            let (list, slot) = (self.temp(), self.slot(local));
            self.line(&format!("{list} = load ptr, ptr {slot}"));
            let span = self.here_span();
            self.proof_alone(&list, "list made alone once still alone", span);
            return list;
        }
        self.declare("declare ptr @fennec_list_pop_unshare(ptr) nounwind");
        let slot = self.slot(local);
        let list = self.temp();
        let (ok, shared) = (self.label("alone"), self.label("shared"));
        self.line(&format!("{list} = load ptr, ptr {slot}"));
        let alone = self.alone(&list);
        let came_from = self.current.clone();
        self.line(&format!("br i1 {alone}, label %{ok}, label %{shared}, !prof !8"));
        let (copy, resolved) = (self.temp(), self.temp());
        self.cold_block(&format!(
            "{shared}:\n  {copy} = call ptr @fennec_list_pop_unshare(ptr {list})\n  store ptr {copy}, ptr {slot}\n  br label %{ok}\n"
        ));
        self.start_block(&ok);
        self.line(&format!("{resolved} = phi ptr [ {list}, %{came_from} ], [ {copy}, %{shared} ]"));
        resolved
    }

    /// The list at `place`, ready for its item at `index` to be set, under a lock that saves what it
    /// changes. A list the lock's log marked stays where it is, the item noted for a crash to put
    /// back, so every set asks the runtime, and the list is never taken as alone after.
    fn item_to_set(&mut self, place: &str, index: &str) -> String {
        self.declare("declare ptr @fennec_list_item_unshare(ptr, i64) nounwind");
        let list = self.temp();
        let (ok, shared) = (self.label("alone"), self.label("shared"));
        self.line(&format!("{list} = load ptr, ptr {place}"));
        let alone = self.alone(&list);
        let came_from = self.current.clone();
        self.line(&format!("br i1 {alone}, label %{ok}, label %{shared}"));
        let (copy, resolved) = (self.temp(), self.temp());
        self.cold_block(&format!(
            "{shared}:\n  {copy} = call ptr @fennec_list_item_unshare(ptr {list}, i64 {index})\n  store ptr {copy}, ptr {place}\n  br label %{ok}\n"
        ));
        self.start_block(&ok);
        self.line(&format!("{resolved} = phi ptr [ {list}, %{came_from} ], [ {copy}, %{shared} ]"));
        resolved
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
        let one = self.alone(block);
        self.assumed(&one, "a block just made alone has one owner", Span::default());
    }

    /// The local's list, ready to change. One nothing else here comes to hold was checked
    /// when the local got it, which leaves LLVM free to see through the writes.
    pub(crate) fn unshared(&mut self, local: LocalId) -> String {
        if self.known.alone_once_checked(local) {
            let (list, slot) = (self.temp(), self.slot(local));
            self.line(&format!("{list} = load ptr, ptr {slot}"));
            let span = self.here_span();
            self.proof_alone(&list, "list made alone once still alone", span);
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
                let block =
                    self.block_to_change((object, path.get(..step).unwrap_or_default()), &place, &kind);
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
        let place = self.list_place(object, path)?;
        Ok(self.alone_along((object, path), &place, "fennec_list_unshare"))
    }

    /// Where the list at a field path from a local is, with nothing it's inside having another
    /// owner.
    fn list_place(&mut self, object: LocalId, path: &[String]) -> Result<String, String> {
        let mut kind = self.kind_of(object)?.clone();
        let mut place = self.slot(object);
        for (step, name) in path.iter().enumerate() {
            let inner = if matches!(kind, Kind::Boxed(_)) {
                let block =
                    self.block_to_change((object, path.get(..step).unwrap_or_default()), &place, &kind);
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

    /// The block at a field path from a local, made alone unless that's known already, and
    /// known from here.
    /// Whether a value of this kind is a `shared` type's handle.
    pub(crate) fn is_cell(&self, kind: &Kind) -> bool {
        matches!(kind, Kind::Boxed(id) if self.module.layouts.get(*id).is_some_and(|layout| layout.cell))
    }

    /// The block at `place` for changing what's in it: a handle's own, which every name for it
    /// sees change, and otherwise one with no other owner.
    pub(crate) fn block_to_change(
        &mut self,
        (root, path): (LocalId, &[String]),
        place: &str,
        kind: &Kind,
    ) -> String {
        if self.is_cell(kind) {
            let block = self.temp();
            self.line(&format!("{block} = load ptr, ptr {place}"));
            return block;
        }
        if !self.saving.is_empty() {
            return self.changeable_along((root, path), place);
        }
        self.alone_along((root, path), place, "fennec_unshare")
    }

    /// The struct's block at `place`, ready to change under a lock that saves what it changes. One
    /// the lock's log marked stays where it is, its fields saved already, so it's never taken as
    /// alone after: its count still has the mark.
    fn changeable_along(&mut self, (root, path): (LocalId, &[String]), place: &str) -> String {
        let key = self.fact_key(root, path, 8);
        if self.alone_fields.contains(&key) {
            return self.load_block(place);
        }
        self.declare("declare ptr @fennec_lock_to_change(ptr) nounwind");
        let block = self.temp();
        let (ok, shared) = (self.label("alone"), self.label("shared"));
        self.line(&format!("{block} = load ptr, ptr {place}"));
        let alone = self.alone(&block);
        let came_from = self.current.clone();
        self.line(&format!("br i1 {alone}, label %{ok}, label %{shared}"));
        let (copy, resolved) = (self.temp(), self.temp());
        self.cold_block(&format!(
            "{shared}:\n  {copy} = call ptr @fennec_lock_to_change(ptr {block})\n  store ptr {copy}, ptr {place}\n  br label %{ok}\n"
        ));
        self.start_block(&ok);
        self.line(&format!("{resolved} = phi ptr [ {block}, %{came_from} ], [ {copy}, %{shared} ]"));
        self.alone_fields.push(key);
        resolved
    }

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
                Kind::Boxed(_) => Some(self.block_to_change((root, along), &place, &kind)),
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

    /// Whether `local` was read from `object` along `path`, a field at a time.
    fn reads_from(&self, local: LocalId, object: LocalId, path: &[String]) -> bool {
        match path.split_last() {
            None => local == object,
            Some((last, rest)) => self
                .known
                .field_of(local)
                .is_some_and(|(from, name)| name == last && self.reads_from(*from, object, rest)),
        }
    }

    pub(crate) fn set_index(
        &mut self,
        object: LocalId,
        path: &[String],
        index: &Operand,
        value: &Operand,
        span: Span,
    ) -> Result<(), String> {
        let Some(done) = self.put_in_place(object, path, index, value, span)? else {
            return self.set_index_as_ever(object, path, index, value, span);
        };
        // What the old way learns holds only on that way: the put in place learned nothing.
        let alone_fields = self.alone_fields.clone();
        self.set_index_as_ever(object, path, index, value, span)?;
        self.alone_fields = alone_fields;
        self.line(&format!("br label %{done}"));
        self.start_block(&done);
        Ok(())
    }

    /// Under a lock that saves what it changes, a dict put where the lookup finds the key, written
    /// in place when the dict and the blocks on the way to it have one owner (see
    /// `put_found_in_place`). Gives back the label the two ways join at, with the old way's block
    /// started, or `None`, writing nothing, when it doesn't fit.
    fn put_in_place(
        &mut self,
        object: LocalId,
        path: &[String],
        index: &Operand,
        value: &Operand,
        span: Span,
    ) -> Result<Option<String>, String> {
        if self.saving.is_empty() {
            return Ok(None);
        }
        let kind = self.place_kind(object, path)?;
        let Kind::Dict(key_word, value_word) = kind else { return Ok(None) };
        if self.is_id_map(&kind)
            || matches!(index, Operand::Local(sliced) if self.known.slice_keys.contains_key(&sliced.0))
        {
            return Ok(None);
        }
        let same_dict = |dict: LocalId| match path {
            [] => dict == object,
            [field] => self.known.field_of(dict).is_some_and(|(from, name)| *from == object && name == field),
            _ => false,
        };
        // A lookup on a local read from this place however deep, as a lock's `held.counts` is the
        // shared value's `value.counts`: the put uses its entry only if it searched the dict
        // that's there now (see `put_found_in_place`).
        let found = self
            .found_entries
            .iter()
            .rev()
            .find(|(dict, looked_up, _)| {
                (same_dict(*dict) || self.reads_from(*dict, object, path)) && looked_up == index
            })
            .map(|(_, _, entry)| entry.clone());
        let Some((place, alone)) = self.dict_alone_along(object, path)? else { return Ok(None) };
        self.put_found_in_place((&place, &alone), (key_word, value_word), (index, value, found), span)
    }

    /// `object.path[index] = value` as it's always been written.
    fn set_index_as_ever(
        &mut self,
        object: LocalId,
        path: &[String],
        index: &Operand,
        value: &Operand,
        span: Span,
    ) -> Result<(), String> {
        if !self.saving.is_empty() {
            self.save_along(object, path)?;
        }
        let kind = self.place_kind(object, path)?;
        if let Some(shape) = self.array_shape(&kind) {
            return self.array_set((object, path), shape, index, value, span);
        }
        let inside = path.is_empty() && self.known.inside(object, index);
        if self.is_id_map(&kind) {
            let place = self.place_of(object, path)?;
            return self.id_map_store(&place, &kind, index, value, span);
        }
        if let Kind::Dict(key, value_word) = kind {
            let place = self.place_of(object, path)?;
            if matches!(value, Operand::Local(local) if self.known.is_taken(*local)) {
                return self.dict_put_back(&place, (key, value_word), index, value, span);
            }
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
            // In a function whose locks keep what they change ready to put back, where a dict the
            // lock marked is changed in place: a lookup on a local read from this place however
            // deep, as a lock's `held.counts` is the shared value's `value.counts`. Only for the
            // runtime, which checks the entry is still that key's in this dict before writing
            // there. Other functions' stores are as they were.
            let checked = if self.saving.is_empty() {
                None
            } else {
                found.clone().or_else(|| {
                    self.found_entries
                        .iter()
                        .rev()
                        .find(|(dict, looked_up, _)| {
                            self.reads_from(*dict, object, path) && looked_up == index
                        })
                        .map(|(_, _, entry)| entry.clone())
                })
            };
            return self.dict_store_checked(&place, (key, value_word), index, value, (found, checked));
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
        let value_operand = value;
        let (value, fresh) = self.value_for(value, &held)?;
        let memory = held.memory(self.module.layouts);
        let at = if let Some(at) = from_here {
            at
        } else {
            let list = if path.is_empty() {
                self.unshared(object)
            } else if !self.saving.is_empty() {
                // Under a lock that saves what it changes, a list its log marked is set in place.
                let place = self.list_place(object, path)?;
                self.item_to_set(&place, &index)
            } else {
                self.alone_list(object, path)?
            };
            let position = if inside {
                self.proof_inside(&list, &index, "index inside its list", span);
                index
            } else {
                let place = self.list_key(object, path);
                self.position_once(place, (&list, index_operand, &index), span)
            };
            self.item_address(&list, &memory, &position)
        };
        if put_back && held.counted() {
            self.proof_goes_back(value_operand, &at);
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
            if put_back {
                self.proof_goes_back(value_operand, &at);
            }
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
        if matches!(kind, Kind::Struct(_) | Kind::Boxed(_)) {
            return self.tuple_slice(dest, (&object, &kind), start);
        }
        let lowest = i64::MIN.to_string();
        let start =
            start.map(|start| self.operand(start)).transpose()?.map_or_else(|| lowest.clone(), |held| held.0);
        let end = end.map(|end| self.operand(end)).transpose()?.map_or(lowest, |held| held.0);
        let part = self.temp();
        match kind {
            Kind::Bytes => {
                self.declare("declare ptr @fennec_bytes_slice(ptr, i64, i64) nounwind");
                self.line(&format!(
                    "{part} = call ptr @fennec_bytes_slice(ptr {object}, i64 {start}, i64 {end})"
                ));
                return self.assign(dest, &part);
            }
            Kind::Str if self.headers() => {
                let part = self.header_slice(&object, (&start, &end), span);
                return self.assign(dest, &part);
            }
            Kind::Str => {
                self.declare("declare ptr @fennec_str_slice(ptr, i64, i64, i64, i64) nounwind");
                let (from, to) = self.place_in_source(span);
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
        // An array is made as the struct it's laid out as.
        if self.array_shape(&kind).is_some() {
            return self.make(dest, items);
        }
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

    /// A list of constants, like `embed_folder`'s paths and files: made the first time it's
    /// reached and kept for the whole run, as a constant's value is, and handed out with one more
    /// owner each time after. Nothing changes one in place, as it can't be changed. In a program
    /// with tasks it's made each time instead, since a task's thread can't make a block every
    /// thread reads once they've started.
    pub(crate) fn constant_list(
        &mut self,
        dest: LocalId,
        items: &[Operand],
        block: usize,
    ) -> Result<(), String> {
        let kind = self.kind_of(dest)?.clone();
        if self.module.threads || !matches!(kind, Kind::List(_)) || self.array_shape(&kind).is_some() {
            return self.make_list(dest, items, block);
        }
        let site = self.label("list");
        let global = format!("@\"f_{}.{site}\"", self.function.name);
        // One for every module, as another module may have a copy of this function to inline.
        self.declare(&format!("{global} = linkonce_odr hidden global ptr null, align 8"));
        let (kept, none) = (self.temp(), self.temp());
        self.line(&format!("{kept} = load ptr, ptr {global}, align 8"));
        self.line(&format!("{none} = icmp eq ptr {kept}, null"));
        let (first, ready) = (self.label("list_first"), self.label("list_ready"));
        let before = self.current.clone();
        self.line(&format!("br i1 {none}, label %{first}, label %{ready}, !prof !0"));
        self.start_block(&first);
        let item = list_item(&kind)?;
        let held = self.item_kind(item)?;
        let made = self.new_list(item, &items.len().to_string());
        let memory = held.memory(self.module.layouts);
        for (position, value) in items.iter().enumerate() {
            let at = self.item_address(&made, &memory, &position.to_string());
            self.fill(&held, value, &at, ITEM_ACCESS)?;
        }
        self.declare("declare void @fennec_constant_kept(ptr) nounwind");
        self.line(&format!("call void @fennec_constant_kept(ptr {made})"));
        self.line(&format!("store ptr {made}, ptr {global}, align 8"));
        let made_end = self.current.clone();
        self.line(&format!("br label %{ready}"));
        self.start_block(&ready);
        let list = self.temp();
        self.line(&format!("{list} = phi ptr [ {kept}, %{before} ], [ {made}, %{made_end} ]"));
        self.retain_value(&list, &kind);
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
        if self.known.length_kept.contains_key(&dest.0) {
            let kept = self.kept_length(dest.0);
            self.line(&format!("store i64 0, ptr {kept}"));
        }
        // Checking proofs, the appends a list made whole of zeros leaves out are counted.
        if self.checks_proofs() && self.known.zero_filled.contains(&dest.0) {
            let counted = self.zeros_counted(dest.0);
            self.line(&format!("store i64 0, ptr {counted}"));
        }
        self.assign(dest, &list)
    }

    /// In a build that checks proofs, how many appends a list made whole of zeros has left out.
    fn zeros_counted(&mut self, list: usize) -> String {
        if let Some(counted) = self.zeros_counted.get(&list) {
            return counted.clone();
        }
        let counted = self.label("%zeros_counted");
        let _ = writeln!(self.allocas, "  {counted} = alloca i64");
        self.zeros_counted.insert(list, counted.clone());
        counted
    }

    /// The local a list's length is kept in while its loop fills it.
    fn kept_length(&mut self, list: usize) -> String {
        if let Some(kept) = self.kept_lengths.get(&list) {
            return kept.clone();
        }
        let kept = self.label("%kept_length");
        // -1 until the list is made, and again once the list holds its length itself: what a
        // crash's way out goes by.
        let _ = writeln!(self.allocas, "  {kept} = alloca i64\n  store i64 -1, ptr {kept}");
        self.kept_lengths.insert(list, kept.clone());
        kept
    }

    /// At the block after a loop that kept a list's length aside, writes it to the list. A
    /// `list[str]` kept as the string it's joined into has no length of its own to write: its
    /// pieces go straight onto the string, which knows how long it is.
    pub(crate) fn write_kept_lengths(&mut self, block: usize) {
        let mut ended: Vec<usize> = self
            .known
            .length_kept
            .iter()
            .filter(|(list, after)| {
                **after == block && matches!(self.kind_of(LocalId(**list)), Ok(Kind::List(_)))
            })
            .map(|(list, _)| *list)
            .collect();
        ended.sort_unstable();
        for list in ended {
            let (kept, slot) = (self.kept_length(list), self.slot(LocalId(list)));
            let (length, block) = (self.temp(), self.temp());
            self.line(&format!("{length} = load i64, ptr {kept}"));
            self.line(&format!("{block} = load ptr, ptr {slot}"));
            let length_at = self.offset(&block, u32::try_from(LIST_LENGTH).unwrap_or(0));
            // Checking proofs: nothing else added to the list while its length was kept aside,
            // so its own still says 0, and what was kept fits the room it was made with.
            if self.checks_proofs() {
                let span = self.here_span();
                let (own, changed) = (self.temp(), self.temp());
                self.line(&format!("{own} = load i64, ptr {length_at}{HEADER_ACCESS}"));
                self.line(&format!("{changed} = icmp ne i64 {own}, 0"));
                self.proof_held(&changed, "nothing else changed a list whose length was kept aside", span);
                let (room_at, room, over) = (self.temp(), self.temp(), self.temp());
                self.line(&format!("{room_at} = getelementptr inbounds i8, ptr {block}, i64 {LIST_ROOM}"));
                self.line(&format!("{room} = load i64, ptr {room_at}{HEADER_ACCESS}"));
                self.line(&format!("{over} = icmp ugt i64 {length}, {room}"));
                self.proof_held(&over, "a length kept aside fits its list's room", span);
            }
            self.line(&format!("store i64 {length}, ptr {length_at}{HEADER_ACCESS}"));
            self.line(&format!("store i64 -1, ptr {kept}"));
        }
    }

    /// On the way out after a crash, writes the length of each list still kept aside, in a loop
    /// the crash left partway, to the list, so letting go of it lets go of the items it holds.
    /// A list whose length is its own, or that was never made, is left as it is.
    pub(crate) fn write_kept_lengths_after_crash(&mut self) {
        let mut kept_lists: Vec<usize> = self
            .known
            .length_kept
            .keys()
            .filter(|list| matches!(self.kind_of(LocalId(**list)), Ok(Kind::List(_))))
            .copied()
            .collect();
        kept_lists.sort_unstable();
        for list in kept_lists {
            let (kept, slot) = (self.kept_length(list), self.slot(LocalId(list)));
            let (length, block, none, own, skip) =
                (self.temp(), self.temp(), self.temp(), self.temp(), self.temp());
            let (write, next) = (self.label("kept_write"), self.label("kept_written"));
            self.line(&format!("{length} = load i64, ptr {kept}"));
            self.line(&format!("{block} = load ptr, ptr {slot}"));
            self.line(&format!("{none} = icmp eq ptr {block}, null"));
            self.line(&format!("{own} = icmp slt i64 {length}, 0"));
            self.line(&format!("{skip} = or i1 {none}, {own}"));
            self.line(&format!("br i1 {skip}, label %{next}, label %{write}"));
            self.start_block(&write);
            let length_at = self.offset(&block, u32::try_from(LIST_LENGTH).unwrap_or(0));
            self.line(&format!("store i64 {length}, ptr {length_at}{HEADER_ACCESS}"));
            self.line(&format!("br label %{next}"));
            self.start_block(&next);
        }
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
        // Made whole already, of zeros. Checking proofs, each turn's append is the one at the
        // loop's count, inside the list: so there are as many as it has zeros.
        if self.known.zero_filled.contains(&local.0) {
            if self.checks_proofs()
                && let Some(count) = self.known.zero_counts.get(&local.0).copied()
            {
                let span = self.here_span();
                let counted = self.zeros_counted(local.0);
                let (done, (turn, _), list) =
                    (self.temp(), self.operand(&Operand::Local(count))?, self.temp());
                self.line(&format!("{done} = load i64, ptr {counted}"));
                let (other, slot) = (self.temp(), self.slot(*local));
                self.line(&format!("{other} = icmp ne i64 {done}, {turn}"));
                self.proof_held(&other, "a list made whole of zeros gets one a turn, from 0", span);
                self.line(&format!("{list} = load ptr, ptr {slot}"));
                self.proof_inside(&list, &done, "a list made whole of zeros is as long as its loop", span);
                let next = self.temp();
                self.line(&format!("{next} = add i64 {done}, 1"));
                self.line(&format!("store i64 {next}, ptr {counted}"));
            }
            return Ok(());
        }
        let kind = self.kind_of(*local)?.clone();
        let item = list_item(&kind)?;
        let held = self.item_kind(item)?;
        let new_item = args.get(1).ok_or("no argument")?;
        // Filled by a loop that never appends more than the room it made, with nothing else
        // there touching the list: no room to check, and nobody else holds it. With no call
        // left in the loop, LLVM keeps the length in a register while it fills.
        if self.known.room_enough.contains(&local.0) {
            let slot = self.slot(*local);
            let (list, length, longer) = (self.temp(), self.temp(), self.temp());
            self.line(&format!("{list} = load ptr, ptr {slot}"));
            // Kept aside while the loop runs, when it only leaves by its header; otherwise in
            // the list, for whatever another way out leads to.
            if self.known.length_kept.contains_key(&local.0) {
                let kept = self.kept_length(local.0);
                self.line(&format!("{length} = load i64, ptr {kept}"));
                self.line(&format!("{longer} = add nuw nsw i64 {length}, 1"));
                self.line(&format!("store i64 {longer}, ptr {kept}"));
            } else {
                let length_at = self.offset(&list, u32::try_from(LIST_LENGTH).unwrap_or(0));
                self.load_length(&length, &length_at);
                self.line(&format!("{longer} = add nuw nsw i64 {length}, 1"));
                self.line(&format!("store i64 {longer}, ptr {length_at}{HEADER_ACCESS}"));
            }
            if self.checks_proofs() {
                let span = self.here_span();
                self.proof_alone(&list, "list filled by its loop has one owner", span);
                let (room_at, room, full) = (self.temp(), self.temp(), self.temp());
                self.line(&format!("{room_at} = getelementptr inbounds i8, ptr {list}, i64 {LIST_ROOM}"));
                self.line(&format!("{room} = load i64, ptr {room_at}{HEADER_ACCESS}"));
                self.line(&format!("{full} = icmp uge i64 {length}, {room}"));
                self.proof_held(&full, "room enough for its loop's appends", span);
            }
            let memory = held.memory(self.module.layouts);
            let at = self.item_address(&list, &memory, &length);
            return self.fill(&held, new_item, &at, ITEM_ACCESS);
        }
        self.declare("declare ptr @fennec_list_grow(ptr) nounwind");
        let slot = self.slot(*local);
        let (list, length, room_at, room) = (self.temp(), self.temp(), self.temp(), self.temp());
        let (fits, ready, longer) = (self.temp(), self.temp(), self.temp());
        let (quick, grow, done) = (self.label("quick"), self.label("grow"), self.label("done"));
        self.line(&format!("{list} = load ptr, ptr {slot}"));
        let length_at = self.offset(&list, u32::try_from(LIST_LENGTH).unwrap_or(0));
        self.load_length(&length, &length_at);
        self.line(&format!("{room_at} = getelementptr inbounds i8, ptr {list}, i64 {LIST_ROOM}"));
        self.line(&format!("{room} = load i64, ptr {room_at}{HEADER_ACCESS}"));
        self.line(&format!("{fits} = icmp ult i64 {length}, {room}"));
        if self.known.alone_once_checked(*local) {
            let span = self.here_span();
            self.proof_alone(&list, "list made alone once still alone", span);
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
    /// outside it, and the item goes in it.
    pub(crate) fn insert(&mut self, args: &[Operand], call: &str, span: Span) -> Result<(), String> {
        let [Operand::Local(list), at, value] = args else {
            return Err("`insert` with the wrong arguments".to_owned());
        };
        let (from, to) = self.place_in_source(span);
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
        let (from, to) = self.place_in_source(span);
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
                // Empty, it fails with a buffer's own message, rather than as a list's pop does.
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

/// Whether a type's name is a union's, with ` | ` outside any brackets: `list[int] | str` is one,
/// and `Result[str, TooWeak | NotFound]` isn't.
fn union_at_top(name: &str) -> bool {
    let mut depth = 0_i32;
    for (at, byte) in name.bytes().enumerate() {
        match byte {
            b'[' | b'(' => depth += 1,
            b']' | b')' => depth -= 1,
            b'|' if depth == 0 && at > 0 => return true,
            _ => {}
        }
    }
    false
}
