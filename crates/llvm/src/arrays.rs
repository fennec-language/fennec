//! Fixed-size arrays, `array[T, N]`. One that fits in `fennec_ir::arrays::IN_PLACE_BYTES` is a
//! struct with a field for each item, so it's kept where it is like any plain struct: copying it
//! copies its items, and what they hold gets another owner. Its items are LLVM's own array, so an
//! item is reached by its position, as a loop over them reads them. A bigger array reaches here as
//! a list, which only needs `array.new` to make it. One whose items a plain struct couldn't hold
//! either, like optional strings, is boxed as such a struct is: a block shared until it changes.

use std::fmt::Write as _;

use fennec_ir::{LocalId, Operand};
use fennec_syntax::Span;

use crate::emit::Emit;
use crate::kinds::{Kind, is_array};
use crate::lists::{FIELDS_OFFSET, list_item};

/// The most bytes of a value held in memory that are copied or zeroed as words written out, rather
/// than as bytes.
const LARGEST_WRITTEN_OUT: u32 = 256;

/// An array laid out as a struct: what its items are, how many, how far apart in bytes, and
/// whether it's boxed.
pub(crate) struct Shape {
    pub(crate) item: Kind,
    pub(crate) count: u32,
    pub(crate) stride: u32,
    pub(crate) boxed: bool,
}

impl Emit<'_, '_, '_> {
    /// The shape of an array kept in place, if that's what a value of this kind is.
    pub(crate) fn array_shape(&self, kind: &Kind) -> Option<Shape> {
        let (Kind::Struct(id) | Kind::Boxed(id)) = kind else { return None };
        let layout = self.module.layouts.get(*id).filter(|layout| is_array(layout))?;
        let item = Kind::from_ty(self.module.layouts, layout.fields.first()?.ty)?;
        let count = u32::try_from(layout.fields.len()).ok()?;
        Some(Shape { item, count, stride: layout.size / count, boxed: matches!(kind, Kind::Boxed(_)) })
    }

    /// The shape of the array a local holds, if it holds one kept in place.
    pub(crate) fn array_in(&self, object: &Operand) -> Option<Shape> {
        let Operand::Local(local) = object else { return None };
        self.array_shape(self.kind_of(*local).ok()?)
    }

    /// `array[index]`: the item, checked to be there, counting from the end if it's negative.
    pub(crate) fn array_get(
        &mut self,
        dest: LocalId,
        (object, shape): (&Operand, Shape),
        index: &Operand,
        span: Span,
    ) -> Result<(), String> {
        // An item taken out to change and put back is read from where the array is, made alone
        // first when it's in a block, so the change can't show through a copy of it.
        let home = self.known.is_taken(dest).then(|| self.known.item_home(dest).cloned()).flatten();
        let base = if let Some((root, path)) = home {
            let place = self.place_of(root, &path)?;
            if shape.boxed {
                let kind = self.place_kind(root, &path)?;
                let block = self.block_to_change((root, &path), &place, &kind);
                self.offset(&block, FIELDS_OFFSET)
            } else {
                place
            }
        } else {
            let (base, _) = self.operand(object)?;
            if shape.boxed { self.offset(&base, FIELDS_OFFSET) } else { base }
        };
        let position = self.array_position(index, shape.count, span)?;
        let at = self.item_at(&base, &shape, &position);
        self.read_into(dest, &shape.item, &at, "")
    }

    /// `array[index] = value`, for an array at a field path from a local.
    pub(crate) fn array_set(
        &mut self,
        (object, path): (LocalId, &[String]),
        shape: Shape,
        index: &Operand,
        value: &Operand,
        span: Span,
    ) -> Result<(), String> {
        // The old item is let go of only once the index is known to be there.
        let place = self.place_of(object, path)?;
        let position = self.array_position(index, shape.count, span)?;
        let base = if shape.boxed {
            let kind = self.place_kind(object, path)?;
            let block = self.block_to_change((object, path), &place, &kind);
            self.offset(&block, FIELDS_OFFSET)
        } else {
            place
        };
        let at = self.item_at(&base, &shape, &position);
        let put_back = matches!(value, Operand::Local(local) if self.known.is_taken(*local));
        let item = shape.item;
        let (value, fresh) = self.value_for(value, &item)?;
        if item.counted() {
            if put_back {
                let text = self.text_of(&item);
                self.line(&format!("store {text} {value}, ptr {at}"));
                return Ok(());
            }
            if !fresh {
                self.retain_value(&value, &item);
            }
            let (old, text) = (self.temp(), self.text_of(&item));
            self.line(&format!("{old} = load {text}, ptr {at}"));
            self.line(&format!("store {text} {value}, ptr {at}"));
            self.release_kind(&old, &item);
            return Ok(());
        }
        if !put_back {
            self.release_inline(&item, &at);
        }
        self.store_field(&item, &value, &at, "");
        if !put_back {
            self.retain_inline(&item, &at);
        }
        Ok(())
    }

    /// `array[T, N](zero)`: N items, each the zero. One too big to keep in place is a list.
    pub(crate) fn new_array(&mut self, dest: LocalId, args: &[Operand]) -> Result<(), String> {
        let [zero, Operand::Int(count)] = args else {
            return Err("an array made without its size".to_owned());
        };
        let kind = self.kind_of(dest)?.clone();
        if let (Some(shape), Kind::Boxed(id)) = (self.array_shape(&kind), &kind) {
            let block = self.new_block(*id)?;
            let base = self.offset(&block, FIELDS_OFFSET);
            let stride = shape.stride;
            self.fill_items(&shape.item, zero, &shape.count.to_string(), &|emit, position| {
                let at = emit.temp();
                emit.line(&format!(
                    "{at} = getelementptr inbounds [{stride} x i8], ptr {base}, i64 {position}"
                ));
                at
            })?;
            return self.assign(dest, &block);
        }
        if let Some(shape) = self.array_shape(&kind) {
            // Holding blocks, it's made beside what the local holds, which may be read for it.
            let beside = self.holds_inline(&kind).then(|| self.place_for(&kind));
            let base = beside.clone().unwrap_or_else(|| self.slot(dest));
            let stride = shape.stride;
            self.fill_items(&shape.item, zero, &shape.count.to_string(), &|emit, position| {
                let at = emit.temp();
                emit.line(&format!(
                    "{at} = getelementptr inbounds [{stride} x i8], ptr {base}, i64 {position}"
                ));
                at
            })?;
            if let Some(place) = beside {
                self.assign_memory_owned(dest, &place)?;
            }
            return Ok(());
        }
        let item = list_item(&kind)?;
        let held = self.item_kind(item)?;
        let list = self.new_list(item, &count.to_string());
        let memory = held.memory(self.module.layouts);
        self.fill_items(&held, zero, &count.to_string(), &|emit, position| {
            emit.item_address(&list, &memory, position)
        })?;
        self.assign(dest, &list)
    }

    /// Writes the same value into each of `count` places that held nothing, each another owner
    /// of what it holds, in a loop. `address` gives where each is from its position.
    fn fill_items(
        &mut self,
        item: &Kind,
        value: &Operand,
        count: &str,
        address: &dyn Fn(&mut Self, &str) -> String,
    ) -> Result<(), String> {
        let (value, fresh) = self.value_for(value, item)?;
        self.each_position(count, |emit, position| {
            let at = address(emit, position);
            emit.store_field(item, &value, &at, "");
            if item.counted() {
                emit.retain_value(&value, item);
            } else {
                emit.retain_inline(item, &at);
            }
            Ok(())
        })?;
        // A value made for this was owned once, and each item now owns it.
        if fresh {
            if item.counted() {
                self.release_kind(&value, item);
            } else {
                self.release_inline(item, &value);
            }
        }
        Ok(())
    }

    /// Copies a value held in memory from one place to another as bytes, if it's big enough that
    /// LLVM would otherwise copy it a word at a time, written out: as a big array is. Gives back
    /// whether it did.
    pub(crate) fn copied_as_bytes(&mut self, kind: &Kind, from: &str, to: &str) -> bool {
        let size = kind.size(self.module.layouts);
        if !kind.in_memory() || size <= LARGEST_WRITTEN_OUT {
            return false;
        }
        self.declare("declare void @llvm.memcpy.p0.p0.i64(ptr, ptr, i64, i1)");
        self.line(&format!("call void @llvm.memcpy.p0.p0.i64(ptr {to}, ptr {from}, i64 {size}, i1 false)"));
        true
    }

    /// The line making what's at `place` all zero: as bytes, for a value as big as a big array, which
    /// LLVM would otherwise split into a store for each word and take seconds over.
    pub(crate) fn zero_line(&mut self, kind: &Kind, place: &str) -> String {
        let size = kind.size(self.module.layouts);
        if kind.in_memory() && size > LARGEST_WRITTEN_OUT {
            self.declare("declare void @llvm.memset.p0.i64(ptr, i8, i64, i1)");
            return format!("call void @llvm.memset.p0.i64(ptr {place}, i8 0, i64 {size}, i1 false)");
        }
        format!("store {} zeroinitializer, ptr {place}", self.text_of(kind))
    }

    /// Runs `body` for each position from 0 up to `count`, in a loop, given where it is. The count
    /// is kept in memory, so what `body` emits may branch.
    pub(crate) fn each_position<F>(&mut self, count: &str, mut body: F) -> Result<(), String>
    where
        F: FnMut(&mut Self, &str) -> Result<(), String>,
    {
        let counter = self.place_for(&Kind::Int);
        self.line(&format!("store i64 0, ptr {counter}"));
        let (head, each, done) = (self.label("each"), self.label("each_item"), self.label("each_done"));
        self.line(&format!("br label %{head}"));
        self.start_block(&head);
        let (position, more) = (self.temp(), self.temp());
        self.line(&format!("{position} = load i64, ptr {counter}"));
        self.line(&format!("{more} = icmp ult i64 {position}, {count}"));
        self.line(&format!("br i1 {more}, label %{each}, label %{done}"));
        self.start_block(&each);
        let done_body = body(self, &position);
        let next = self.temp();
        self.line(&format!("{next} = add nuw i64 {position}, 1"));
        self.line(&format!("store i64 {next}, ptr {counter}"));
        self.line(&format!("br label %{head}"));
        self.start_block(&done);
        done_body
    }

    /// Whether two arrays in place at these addresses hold the same items, compared a pair at a
    /// time in a loop.
    pub(crate) fn same_arrays(&mut self, shape: &Shape, (a, b): (&str, &str)) -> Result<String, String> {
        let same = self.label("%same");
        let _ = writeln!(self.allocas, "  {same} = alloca i1");
        self.line(&format!("store i1 true, ptr {same}"));
        let (stride, item) = (shape.stride, shape.item.clone());
        self.each_position(&shape.count.to_string(), |emit, position| {
            let (x, y) = (emit.temp(), emit.temp());
            emit.line(&format!("{x} = getelementptr inbounds [{stride} x i8], ptr {a}, i64 {position}"));
            emit.line(&format!("{y} = getelementptr inbounds [{stride} x i8], ptr {b}, i64 {position}"));
            let equal = emit.same_fields(&[(0, item.clone())], (&x, &y))?;
            let (differ, next) = (emit.label("differ"), emit.label("same_so_far"));
            emit.line(&format!("br i1 {equal}, label %{next}, label %{differ}"));
            emit.start_block(&differ);
            emit.line(&format!("store i1 false, ptr {same}"));
            emit.line(&format!("br label %{next}"));
            emit.start_block(&next);
            Ok(())
        })?;
        let result = self.temp();
        self.line(&format!("{result} = load i1, ptr {same}"));
        Ok(result)
    }

    /// Where an index points in an array of `count` items: from the start, or the end if it's
    /// negative, failing outside. One written out is checked here.
    fn array_position(&mut self, index: &Operand, count: u32, span: Span) -> Result<String, String> {
        if let Operand::Int(at) = index {
            let count = i64::from(count);
            let from_start = if *at < 0 { at + count } else { *at };
            if (0..count).contains(&from_start) {
                return Ok(from_start.to_string());
            }
        }
        let (index, _) = self.operand(index)?;
        Ok(self.position_in(&count.to_string(), &index, span))
    }

    fn item_at(&mut self, base: &str, shape: &Shape, position: &str) -> String {
        let at = self.temp();
        let stride = shape.stride;
        self.line(&format!("{at} = getelementptr inbounds [{stride} x i8], ptr {base}, i64 {position}"));
        at
    }
}
