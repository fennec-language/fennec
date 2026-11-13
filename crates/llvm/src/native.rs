//! Native blocks: values of a boxed enum the runtime never sees, made and freed by generated code
//! straight through the allocator, with no place in the runtime's table.
//!
//! The runtime reads only blocks it holds, as boxes in its table, so a block made here must
//! never reach it: not as an argument to anything of its, not inside a list, dict or set, not
//! printed or compared by it, not in a generator, a closure or another thread. A type gets
//! native blocks only when the whole program shows that, and stays in the table otherwise.
//!
//! For now that's enums whose fields are numbers, strings, lists of numbers or strings, or the
//! enum itself. A string or a list is the runtime's as ever, held by its header or its address,
//! which a native block's drop lets go of; an enum holding one gets native blocks only in a
//! program with no other threads, which would count their owners their own way. While a run
//! counts what it allocates, under `FENNEC_COUNTS` or the copy tracer, each native block made
//! says so.

use std::collections::{BTreeSet, HashSet};

use fennec_check::Type;
use fennec_ir::{InstKind, Operand, Program};
use fennec_layout::{Layouts, ListItem};

use crate::analysis::visit;
use crate::kinds::Kind;

/// The layouts of the boxed enums whose blocks can be native in `program`.
pub(crate) fn native_types(program: &Program, layouts: &Layouts) -> BTreeSet<u32> {
    program
        .enums
        .iter()
        .filter(|def| qualifies(program, layouts, &def.name))
        .filter_map(|def| layouts.id(&def.name))
        .collect()
}

/// Whether every value of the enum `name` stays with generated code.
fn qualifies(program: &Program, layouts: &Layouts, name: &str) -> bool {
    let Some(id) = layouts.id(name) else { return false };
    let Some(layout) = layouts.get(id) else { return false };
    if !layout.boxed || layout.cell || layout.cases.is_none() {
        return false;
    }
    // Its fields are numbers, strings, lists of numbers or strings, or itself, so freeing it
    // needs nothing of the runtime's but letting go of its strings and lists, which it does by
    // their headers and addresses.
    let fields = || layout.cases.iter().flatten().flat_map(|case| &case.fields);
    let plain = fields().all(|field| match Kind::from_ty(layouts, field.ty) {
        Some(Kind::Int | Kind::Float | Kind::Bool) => true,
        Some(Kind::Boxed(of)) => of == id,
        Some(Kind::Str) => layouts.text_headers(),
        Some(Kind::List(item)) => runtime_items(item),
        _ => false,
    });
    if !plain {
        return false;
    }
    // Another thread counts a string's or a list's owners its own way, which a drop here doesn't.
    if fields().any(|field| matches!(Kind::from_ty(layouts, field.ty), Some(Kind::Str | Kind::List(_))))
        && (crate::parallel::uses_parallel(program) || crate::parallel::uses_launch(program))
    {
        return false;
    }
    let exact = Type::Named(name.to_owned());
    let mentions = |ty: &Type| *ty != exact && mentions(program, ty, name, &mut HashSet::new());
    // Nothing the runtime keeps holds one: a generator's slots, a constant, a type that writes
    // its own text.
    if program.generators.iter().flat_map(|generator| &generator.slots).any(|ty| *ty == exact || mentions(ty))
        || program
            .constants
            .iter()
            .any(|constant| constant.locals.iter().any(|local| local.ty == exact || mentions(&local.ty)))
        || program.own_texts.iter().any(|own| own.ty == exact || mentions(&own.ty))
    {
        return false;
    }
    let functions: HashSet<&str> = program.functions.iter().map(|function| function.name.as_str()).collect();
    program.functions.iter().all(|function| {
        // A local holding something that holds one, like a list of them, puts one where the
        // runtime frees or reads it.
        if function.locals.iter().any(|local| mentions(&local.ty)) {
            return false;
        }
        let is_it = |operand: &Operand| match operand {
            Operand::Local(local) => function.locals.get(local.0).is_some_and(|local| local.ty == exact),
            _ => false,
        };
        function.blocks.iter().flat_map(|block| &block.insts).all(|inst| {
            let (mut read, mut written) = (false, false);
            visit(
                &inst.kind,
                |operand| read |= is_it(operand),
                |local| written |= is_it(&Operand::Local(local)),
            );
            !(read || written) || allowed(&inst.kind, &functions)
        })
    })
}

/// Whether a list of `item`s holds only what's the runtime's own, numbers and strings, so the
/// runtime frees it whole once a native block's drop lets go of it.
fn runtime_items(item: ListItem) -> bool {
    matches!(
        item,
        ListItem::Int | ListItem::Float | ListItem::Bool | ListItem::U64 | ListItem::F32 | ListItem::Str
    )
}

/// The instructions a native value may meet: making one, looking at its case and fields,
/// copying it, and passing it to and from the program's own functions.
fn allowed(inst: &InstKind, functions: &HashSet<&str>) -> bool {
    match inst {
        InstKind::MakeCase { .. }
        | InstKind::IsCase { .. }
        | InstKind::GetCaseField { .. }
        | InstKind::Copy { .. } => true,
        InstKind::Call { function, .. } => functions.contains(function.as_str()),
        _ => false,
    }
}

/// Whether `ty` holds a `target` anywhere, through fields, items or anything else. Anything
/// it can't see into counts as holding one.
fn mentions(program: &Program, ty: &Type, target: &str, seen: &mut HashSet<String>) -> bool {
    let mut inside = |ty: &Type| mentions(program, ty, target, seen);
    match ty {
        Type::Unknown
        | Type::Unit
        | Type::None
        | Type::Bool
        | Type::Int
        | Type::Float
        | Type::Sized(_)
        | Type::Str
        | Type::LiteralStr
        | Type::Bytes => false,
        Type::List(item)
        | Type::Buffer(item)
        | Type::Array(item, _)
        | Type::Set(item)
        | Type::Iterator(item)
        | Type::Optional(item)
        | Type::Id(item)
        | Type::Table(item) => inside(item),
        Type::Dict(key, value) | Type::Result(key, value) => inside(key) || inside(value),
        Type::Tuple(items) | Type::Union(items) => items.iter().any(inside),
        Type::Arrow(params, returns) => params.iter().any(&mut inside) || inside(returns),
        Type::Named(name) => name == target || named_mentions(program, name, target, seen),
        Type::Generic(name, args) => {
            name == target || args.iter().any(&mut inside) || named_mentions(program, name, target, seen)
        }
        // What fails more than one way holds what each of its errors holds.
        Type::Errors(names) => {
            names.iter().any(|name| name == target || named_mentions(program, name, target, seen))
        }
        // A function value, a type parameter, or one not worked out yet.
        _ => true,
    }
}

/// Whether a struct, enum or error of the program named `name` holds a `target` in a field.
fn named_mentions(program: &Program, name: &str, target: &str, seen: &mut HashSet<String>) -> bool {
    if !seen.insert(name.to_owned()) {
        return false;
    }
    let structs = program.structs.iter().chain(&program.errors).filter(|def| def.name == name);
    let struct_fields: Vec<Type> =
        structs.flat_map(|def| def.fields.iter().map(|(_, ty)| ty.clone())).collect();
    let enum_fields: Vec<Type> = program
        .enums
        .iter()
        .filter(|def| def.name == name)
        .flat_map(|def| def.cases.iter().flat_map(|case| case.fields.iter().map(|(_, ty)| ty.clone())))
        .collect();
    struct_fields.iter().chain(&enum_fields).any(|ty| mentions(program, ty, target, seen))
}

/// What frees a native block back to where it came from, and what makes one: the system's own
/// under the address sanitizer, as the runtime's are then, and mimalloc otherwise.
fn allocator() -> (&'static str, &'static str) {
    if std::env::var("FENNEC_SANITIZE").as_deref() == Ok("address") {
        ("declare noalias ptr @malloc(i64) nounwind", "declare void @free(ptr) nounwind")
    } else {
        ("declare noalias ptr @mi_malloc(i64) nounwind", "declare void @mi_free(ptr) nounwind")
    }
}

impl crate::emit::Emit<'_, '_, '_> {
    /// Whether values of layout `id` are native blocks.
    pub(crate) fn is_native(&self, id: u32) -> bool {
        self.module.shared.native.contains(&id)
    }

    /// A new native block for layout `id`, owned once, its fields zero. A block that can't be
    /// had ends the program, as the runtime's do.
    pub(crate) fn new_native_block(&mut self, id: u32) -> Result<String, String> {
        use fennec_rt::embed::layout::FIELDS;
        let size = u64::from(self.layout_of(id)?.size);
        let bytes = (u64::from(FIELDS.unsigned_abs()) + size).div_ceil(8) * 8;
        let shape = self.shape_of(id)?;
        let (made, (alloc, _)) = (self.temp(), allocator());
        self.declare(alloc);
        let call = if alloc.contains("@malloc") { "@malloc" } else { "@mi_malloc" };
        self.line(&format!("{made} = call ptr {call}(i64 {bytes})"));
        self.declare("declare void @abort() noreturn nounwind");
        let (none, short, have) = (self.temp(), self.label("no_memory"), self.label("have_memory"));
        self.line(&format!("{none} = icmp eq ptr {made}, null"));
        self.line(&format!("br i1 {none}, label %{short}, label %{have}, !prof !0"));
        self.start_block(&short);
        self.line("call void @abort()");
        self.line("unreachable");
        self.start_block(&have);
        // Zeroed with stores LLVM sees, which the case's own stores mostly replace, rather than
        // by the allocator's call to clear it.
        self.line(&format!("store [{} x i64] zeroinitializer, ptr {made}", bytes / 8));
        self.count_native(bytes);
        if self.module.shared.native_blocks == crate::NativeBlocks::Watched {
            self.declare("declare void @fennec_native_made(ptr) nounwind");
            self.line(&format!("call void @fennec_native_made(ptr {made})"));
        }
        self.line(&format!("store i64 1, ptr {made}{}", crate::emit::HEADER_ACCESS));
        let shape_at = self.offset(&made, 8);
        self.line(&format!("store i64 {shape}, ptr {shape_at}{}", crate::emit::HEADER_ACCESS));
        Ok(made)
    }

    /// Says a native block of `bytes` was made, when the run counts what it allocates: read once
    /// at the start, so a run that doesn't count only tests it.
    fn count_native(&mut self, bytes: u64) {
        self.declare("@fennec_counting_native = external dso_local global i64");
        self.declare("declare void @fennec_trace_allocated(i64) nounwind");
        let (counting, on, count, go) =
            (self.temp(), self.temp(), self.label("count_native"), self.label("counted_native"));
        self.line(&format!("{counting} = load i64, ptr @fennec_counting_native"));
        self.line(&format!("{on} = icmp ne i64 {counting}, 0"));
        self.line(&format!("br i1 {on}, label %{count}, label %{go}, !prof !0"));
        self.start_block(&count);
        self.line(&format!("call void @fennec_trace_allocated(i64 {bytes})"));
        self.line(&format!("br label %{go}"));
        self.start_block(&go);
    }

    /// The function a block whose last owner let it go is freed with: the runtime's `free`, or,
    /// when the program has native blocks, one that frees those itself and gives the rest to it.
    pub(crate) fn free_function(&mut self) -> Result<&'static str, String> {
        self.declare("declare void @fennec_free(ptr) nounwind");
        let native: Vec<u32> = self.module.shared.native.iter().copied().collect();
        if native.is_empty() {
            return Ok("@fennec_free");
        }
        let mut body = String::from(
            "define internal void @fen_free_any(ptr %block) nounwind {\nentry:\n  %shape_at = getelementptr inbounds i8, ptr %block, i64 8\n  %held = load i64, ptr %shape_at\n  %shape = and i64 %held, 281474976710655\n  br label %native0\n",
        );
        for (at, id) in native.iter().enumerate() {
            let name = self.layout_of(*id)?.name.clone();
            let symbol = format!("h_{name}");
            self.module.shared.shapes.insert(symbol.clone(), *id);
            self.declare(&format!("@\"{symbol}\" = external dso_local global i64"));
            self.define_drop(*id)?;
            let next = at + 1;
            body.push_str(&format!(
                "native{at}:\n  %wanted{at} = load i64, ptr @\"{symbol}\", !invariant.load !1\n  %is{at} = icmp eq i64 %shape, %wanted{at}\n  br i1 %is{at}, label %drop{at}, label %native{next}\ndrop{at}:\n  tail call void @fen_drop_{id}(ptr %block)\n  ret void\n"
            ));
        }
        body.push_str(&format!(
            "native{}:\n  tail call void @fennec_free(ptr %block)\n  ret void\n}}\n\n",
            native.len()
        ));
        self.module.add_helper("fen_free_any", body);
        Ok("@fen_free_any")
    }

    /// Defines `@fen_drop_{id}(block)`, which frees a native block of layout `id` whose last
    /// owner let it go: it lets go of the blocks its case holds, freeing each that loses its
    /// last owner, then frees the block. Those of its own type wait their turn in one loop
    /// rather than each being freed by a call, so no chain of them needs any stack: not through
    /// its last field, nor any other, as a recursion through `left` once did 200,000 calls deep.
    fn define_drop(&mut self, id: u32) -> Result<(), String> {
        use fennec_rt::embed::layout::FIELDS;
        let (_, free) = allocator();
        self.declare(free);
        let free_call = if free.contains("@free") { "@free" } else { "@mi_free" };
        // A build that watches native blocks tells the runtime each one's gone.
        let watched = self.module.shared.native_blocks == crate::NativeBlocks::Watched;
        if watched {
            self.declare("declare void @fennec_native_freed(ptr) nounwind");
        }
        let freed = if watched { "call void @fennec_native_freed(ptr %block)\n  " } else { "" };
        let layout = self.layout_of(id)?;
        let fields_at = u64::from(FIELDS.unsigned_abs());
        let mut cases = Vec::new();
        for (number, case) in layout.cases.iter().flatten().enumerate() {
            let held_by = |kind: Kind| -> Vec<u64> {
                case.fields
                    .iter()
                    .filter(|field| Kind::from_ty(self.module.layouts, field.ty) == Some(kind.clone()))
                    .map(|field| fields_at + u64::from(field.offset))
                    .collect()
            };
            let (held, texts) = (held_by(Kind::Boxed(id)), held_by(Kind::Str));
            let lists: Vec<u64> = case
                .fields
                .iter()
                .filter(|field| matches!(Kind::from_ty(self.module.layouts, field.ty), Some(Kind::List(_))))
                .map(|field| fields_at + u64::from(field.offset))
                .collect();
            if !held.is_empty() || !texts.is_empty() || !lists.is_empty() {
                cases.push((number, held, texts, lists));
            }
        }
        // The blocks found with no owner left wait in a list made of themselves: a block nobody
        // holds is this drop's, so its count's word holds the next one waiting. Nothing recurses,
        // so a chain however deep, and however it's built, needs no stack.
        let mut text = format!(
            "define internal void @fen_drop_{id}(ptr %start) nounwind {{\nentry:\n  %waiting = alloca ptr\n  store ptr null, ptr %waiting\n  br label %top\ntop:\n  %block = phi ptr [ %start, %entry ], [ %next, %pop ]\n  %tag_at = getelementptr inbounds i8, ptr %block, i64 {fields_at}\n  %tag = load i64, ptr %tag_at\n  switch i64 %tag, label %free ["
        );
        for (number, _, _, _) in &cases {
            text.push_str(&format!(" i64 {number}, label %case{number}"));
        }
        text.push_str(" ]\n");
        for (number, held, texts, lists) in &cases {
            text.push_str(&format!("case{number}:\n"));
            // Its strings first: each header's block, if it has one, loses an owner, and is
            // given to the runtime's free once it has none.
            for (at, offset) in texts.iter().enumerate() {
                let it = format!("{number}_text{at}");
                text.push_str(&format!(
                    "  %tag_at{it} = getelementptr inbounds i8, ptr %block, i64 {}\n  %tag{it} = load i64, ptr %tag_at{it}\n  %in_block{it} = icmp sgt i64 %tag{it}, -1\n  br i1 %in_block{it}, label %held{it}, label %kept{it}\nheld{it}:\n  %word_at{it} = getelementptr inbounds i8, ptr %block, i64 {offset}\n  %text{it} = load ptr, ptr %word_at{it}\n  %count{it} = load i64, ptr %text{it}\n  %left{it} = sub i64 %count{it}, 1\n  store i64 %left{it}, ptr %text{it}\n  %gone{it} = icmp eq i64 %left{it}, 0\n  br i1 %gone{it}, label %free{it}, label %kept{it}\nfree{it}:\n  call void @fennec_free(ptr %text{it})\n  br label %kept{it}\nkept{it}:\n",
                    offset + 16
                ));
            }
            // Then its lists: each, if made, loses an owner, and is given to the runtime's free,
            // which lets go of its items, once it has none.
            for (at, offset) in lists.iter().enumerate() {
                let it = format!("{number}_list{at}");
                text.push_str(&format!(
                    "  %word_at{it} = getelementptr inbounds i8, ptr %block, i64 {offset}\n  %list{it} = load ptr, ptr %word_at{it}\n  %none{it} = icmp eq ptr %list{it}, null\n  br i1 %none{it}, label %kept{it}, label %held{it}\nheld{it}:\n  %count{it} = load i64, ptr %list{it}\n  %left{it} = sub i64 %count{it}, 1\n  store i64 %left{it}, ptr %list{it}\n  %gone{it} = icmp eq i64 %left{it}, 0\n  br i1 %gone{it}, label %free{it}, label %kept{it}\nfree{it}:\n  call void @fennec_free(ptr %list{it})\n  br label %kept{it}\nkept{it}:\n"
                ));
            }
            // Then the blocks of its own type it holds: each loses an owner, and joins those
            // waiting once it has none, its count's word now holding the next one waiting.
            //
            // Why nothing reads that word as a count while the block waits:
            // - Its count was 0, so no owner is left: no local, and no field of a block of this
            //   type, holds its address. Those are the only places one can be. `qualifies` lets
            //   a native value meet nothing but making one, reading its case and fields, copying
            //   it and the program's own calls (`allowed`), so it's never in a list, a dict, a
            //   closure, a generator, a constant, a channel or a task, and never handed to the
            //   runtime, and the runtime never walks native blocks or reads their counts.
            // - Fennec has no weak references, so nothing holds an address without being one of
            //   its owners.
            // - No other thread can reach it: a native value never leaves the thread that made
            //   it, which is also why its counts are changed without atomics.
            // - A build that watches native blocks only keeps their addresses in a set, and drops
            //   this one when it's freed (`fennec_native_freed`), never reading its words.
            // - This drop reads a waiting block's case and fields, from `FIELDS` on, and only
            //   word 0, its count, holds the link.
            for (at, offset) in held.iter().enumerate() {
                let it = format!("{number}_{at}");
                text.push_str(&format!(
                    "  %at{it} = getelementptr inbounds i8, ptr %block, i64 {offset}\n  %child{it} = load ptr, ptr %at{it}\n  %count{it} = load i64, ptr %child{it}\n  %left{it} = sub i64 %count{it}, 1\n  store i64 %left{it}, ptr %child{it}\n  %gone{it} = icmp eq i64 %left{it}, 0\n  br i1 %gone{it}, label %wait{it}, label %kept{it}\nwait{it}:\n  %ahead{it} = load ptr, ptr %waiting\n  store ptr %ahead{it}, ptr %child{it}\n  store ptr %child{it}, ptr %waiting\n  br label %kept{it}\nkept{it}:\n"
                ));
            }
            text.push_str("  br label %free\n");
        }
        text.push_str(&format!(
            "free:\n  {freed}call void {free_call}(ptr %block)\n  %next = load ptr, ptr %waiting\n  %empty = icmp eq ptr %next, null\n  br i1 %empty, label %done, label %pop\npop:\n  %rest = load ptr, ptr %next\n  store ptr %rest, ptr %waiting\n  br label %top\ndone:\n  ret void\n}}\n\n"
        ));
        self.module.add_helper(&format!("fen_drop_{id}"), text);
        Ok(())
    }
}
