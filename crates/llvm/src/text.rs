//! Printing and f-strings.

use std::fmt::Write as _;

use fennec_ir::{FormatPart, LocalId, Operand};
use fennec_rt::embed::layout::{TEXT, TEXT_BYTES, TEXT_CHARS, TEXT_ROOM};
use fennec_syntax::{BinOp, Span};

use crate::emit::{Emit, HEADER_ACCESS};
use crate::kinds::Kind;

/// How a value of one of several types is written: as itself, as `print` and `str` write it, or
/// as it's shown inside a list or a failure's message, a string member quoted.
#[derive(Clone, Copy)]
enum Shown {
    Plain,
    Quoted,
}

impl Emit<'_, '_, '_> {
    /// The number of a string, read from its symbol, since it depends on the whole program.
    fn string_number(&mut self, text: &str) -> String {
        let name = format!("s_{}", fennec_blake3::hash(text.as_bytes()).to_hex());
        self.declare(&format!("@\"{name}\" = external dso_local global i64"));
        self.module.shared.strings.insert(name.clone(), text.to_owned());
        let temp = self.temp();
        self.line(&format!("{temp} = load i64, ptr @\"{name}\", !invariant.load !1"));
        temp
    }

    /// A literal as a string value: its header when strings are held as headers, and its
    /// block's address otherwise.
    pub(crate) fn literal_value(&mut self, text: &str) -> String {
        if self.headers() {
            return self.literal_header(text);
        }
        self.literal(text)
    }

    /// The address of a literal's block, filled in when the program starts.
    pub(crate) fn literal(&mut self, text: &str) -> String {
        let symbol = self.literal_symbol(text);
        let temp = self.temp();
        self.line(&format!("{temp} = load ptr, ptr {symbol}, !invariant.load !1"));
        temp
    }

    /// The global that holds a literal's block, for code outside a function's own to load.
    pub(crate) fn literal_symbol(&mut self, text: &str) -> String {
        let name = format!("l_{}", fennec_blake3::hash(text.as_bytes()).to_hex());
        self.declare(&format!("@\"{name}\" = external dso_local global ptr"));
        self.module.shared.literals.insert(name.clone(), text.to_owned());
        format!("@\"{name}\"")
    }

    fn print_text(&mut self, text: &str) {
        let number = self.string_number(text);
        self.declare("declare void @fennec_print_str(i64) nounwind");
        self.line(&format!("call void @fennec_print_str(i64 {number})"));
        self.leave_if_failed();
    }

    pub(crate) fn print(&mut self, args: &[Operand]) -> Result<(), String> {
        self.write(args)?;
        self.print_text("\n");
        Ok(())
    }

    /// `stdout.write`: what `print` shows, with no newline.
    pub(crate) fn write(&mut self, args: &[Operand]) -> Result<(), String> {
        // Everything is worked out before anything prints.
        let mut values = Vec::with_capacity(args.len());
        for arg in args {
            values.push(match arg {
                Operand::Str(text) => (text.clone(), None),
                // A generator shows the type it's of.
                Operand::Local(local) if *self.kind_of(*local)? == Kind::Gen => {
                    let ty = self.function.locals.get(local.0).map(|held| held.ty.to_string());
                    (format!("<{}>", ty.unwrap_or_default()), None)
                }
                other => {
                    let (value, kind) = self.operand(other)?;
                    (value, Some(kind))
                }
            });
        }
        for (at, (value, kind)) in values.iter().enumerate() {
            if at > 0 {
                self.print_text(" ");
            }
            match kind {
                None => self.print_text(value),
                Some(kind) => self.print_one(value, kind)?,
            }
        }
        Ok(())
    }

    /// Prints one value: an optional as what it holds, or `None`.
    fn print_one(&mut self, value: &str, kind: &Kind) -> Result<(), String> {
        // An `Id` shows its slot.
        if let Kind::Struct(id) = kind
            && self.module.layouts.get(*id).is_some_and(|layout| layout.name == "Id")
        {
            self.print_text("Id(");
            let (key, slot) = (self.temp(), self.temp());
            self.line(&format!("{key} = load i64, ptr {value}"));
            self.line(&format!("{slot} = lshr i64 {key}, 32"));
            self.print_one(&slot, &Kind::Int)?;
            self.print_text(")");
            return Ok(());
        }
        if *kind == Kind::MaybeText {
            let none = self.header_is_none(value);
            let (holds, missing, joined) = (self.label("holds"), self.label("none"), self.label("printed"));
            self.line(&format!("br i1 {none}, label %{missing}, label %{holds}"));
            self.start_block(&holds);
            self.print_one(value, &Kind::Str)?;
            self.line(&format!("br label %{joined}"));
            self.start_block(&missing);
            self.print_text("None");
            self.line(&format!("br label %{joined}"));
            self.start_block(&joined);
            return Ok(());
        }
        if let Kind::Maybe(inner) = kind {
            let none = self.temp();
            self.line(&format!("{none} = icmp eq ptr {value}, null"));
            let (holds, missing, joined) = (self.label("holds"), self.label("none"), self.label("printed"));
            self.line(&format!("br i1 {none}, label %{missing}, label %{holds}"));
            self.start_block(&holds);
            self.print_one(value, inner)?;
            self.line(&format!("br label %{joined}"));
            self.start_block(&missing);
            self.print_text("None");
            self.line(&format!("br label %{joined}"));
            self.start_block(&joined);
            return Ok(());
        }
        if self.is_optional(kind) {
            let (number, fields) = self.case_of(kind, "Some")?;
            let (_, offset, held) = fields.first().cloned().ok_or("an optional with nothing in it")?;
            let held = held.ok_or("an optional of something this backend doesn't take yet")?;
            let base = self.fields_at(value, kind);
            let (tag, some) = (self.temp(), self.temp());
            self.line(&format!("{tag} = load i64, ptr {base}"));
            self.line(&format!("{some} = icmp eq i64 {tag}, {number}"));
            let (holds, none, joined) = (self.label("holds"), self.label("none"), self.label("printed"));
            self.line(&format!("br i1 {some}, label %{holds}, label %{none}"));
            self.start_block(&holds);
            let at = self.offset(&base, offset);
            let inner = if held.in_memory() { at } else { self.load_field(&held, &at, "") };
            self.print_one(&inner, &held)?;
            self.line(&format!("br label %{joined}"));
            self.start_block(&none);
            self.print_text("None");
            self.line(&format!("br label %{joined}"));
            self.start_block(&joined);
            return Ok(());
        }
        match kind {
            Kind::Int => {
                self.declare("declare void @fennec_print_int(i64) nounwind");
                self.line(&format!("call void @fennec_print_int(i64 {value})"));
                self.leave_if_failed();
            }
            Kind::Float => {
                self.declare("declare void @fennec_print_float(double) nounwind");
                self.line(&format!("call void @fennec_print_float(double {value})"));
                self.leave_if_failed();
            }
            Kind::U64 => {
                self.declare("declare void @fennec_print_u64(i64) nounwind");
                self.line(&format!("call void @fennec_print_u64(i64 {value})"));
                self.leave_if_failed();
            }
            Kind::F32 => {
                self.declare("declare void @fennec_print_f32(double) nounwind");
                self.line(&format!("call void @fennec_print_f32(double {value})"));
                self.leave_if_failed();
            }
            Kind::Bool => {
                self.declare("declare void @fennec_print_bool(i8) nounwind");
                let wide = self.temp();
                self.line(&format!("{wide} = zext i1 {value} to i8"));
                self.line(&format!("call void @fennec_print_bool(i8 {wide})"));
                self.leave_if_failed();
            }
            Kind::TextBlock => {
                self.declare("declare void @fennec_str_print(ptr) nounwind");
                self.line(&format!("call void @fennec_str_print(ptr {value})"));
                self.leave_if_failed();
            }
            Kind::Str => {
                self.declare("declare void @fennec_str_print(ptr) nounwind");
                self.on_blocks([value], |emit, [block]| {
                    emit.line(&format!("call void @fennec_str_print(ptr {block})"));
                });
                self.leave_if_failed();
            }
            _ if self.is_one_of_several(kind) || self.is_union(kind) => {
                let text = self.plain_text(value, kind)?;
                self.declare("declare void @fennec_str_print(ptr) nounwind");
                self.line(&format!("call void @fennec_str_print(ptr {text})"));
                self.release(&text);
                self.leave_if_failed();
            }
            Kind::List(_) | Kind::Boxed(_) | Kind::Bytes | Kind::Dict(..) => {
                self.declare("declare void @fennec_print_block(ptr) nounwind");
                self.line(&format!("call void @fennec_print_block(ptr {value})"));
                self.leave_if_failed();
            }
            // A plain struct or enum, in place: its text, as an f-string writes it.
            Kind::Struct(_) | Kind::Enum(_) => {
                let text = self.plain_text(value, kind)?;
                self.declare("declare void @fennec_str_print(ptr) nounwind");
                self.line(&format!("call void @fennec_str_print(ptr {text})"));
                self.release(&text);
                self.leave_if_failed();
            }
            // A name narrowed to `None`, like `x` after `x = None`, holds nothing but prints so.
            Kind::Unit => self.print_text("None"),
            other => {
                return Err(format!("it prints a {other:?}, which this backend doesn't take yet"));
            }
        }
        Ok(())
    }

    /// A new string with a plain value's text, as it prints: `True`, `(1, 2)`,
    /// `Point(x=1, y=2)` or `Kind.Stay`. `value` is a bool, or a plain struct's or enum's address.
    fn plain_text(&mut self, value: &str, kind: &Kind) -> Result<String, String> {
        if self.is_one_of_several(kind) || self.is_union(kind) {
            return self.one_of_several_text(value, kind, Shown::Plain);
        }
        self.declare("declare ptr @fennec_str_concat(ptr, ptr) nounwind");
        self.declare("declare ptr @fennec_int_text(i64) nounwind");
        self.declare("declare ptr @fennec_float_text(double) nounwind");
        match kind {
            Kind::Bool => {
                let (yes, no, text) = (self.literal("True"), self.literal("False"), self.temp());
                self.line(&format!("{text} = select i1 {value}, ptr {yes}, ptr {no}"));
                self.retain(&text);
                Ok(text)
            }
            Kind::Int => {
                let text = self.temp();
                self.line(&format!("{text} = call ptr @fennec_int_text(i64 {value})"));
                Ok(text)
            }
            Kind::Float => {
                let text = self.temp();
                self.line(&format!("{text} = call ptr @fennec_float_text(double {value})"));
                Ok(text)
            }
            Kind::U64 | Kind::F32 => Ok(self.sized_text(value, kind)),
            // Holding strings or lists, or an enum's fields, it's written by the runtime, from a
            // block made of it. So is an array, which is written as a list.
            Kind::Struct(id) | Kind::Enum(id)
                if self.holds_inline(kind)
                    || self.array_shape(kind).is_some()
                    || self.module.layouts.get(*id).is_some_and(|layout| {
                        layout.cases.iter().flatten().any(|case| !case.fields.is_empty())
                    }) =>
            {
                let block = self.new_block(*id)?;
                let fields = self.offset(&block, crate::lists::FIELDS_OFFSET);
                self.store_field(kind, value, &fields, "");
                self.retain_inline(kind, &fields);
                self.declare("declare ptr @fennec_block_text(ptr) nounwind");
                let text = self.temp();
                self.line(&format!("{text} = call ptr @fennec_block_text(ptr {block})"));
                self.release(&block);
                Ok(text)
            }
            // An `Id` shows its slot and never its tag, which differs from run to run.
            Kind::Struct(id) if self.module.layouts.get(*id).is_some_and(|layout| layout.name == "Id") => {
                let (key, slot) = (self.temp(), self.temp());
                self.line(&format!("{key} = load i64, ptr {value}"));
                self.line(&format!("{slot} = lshr i64 {key}, 32"));
                let number = self.temp();
                self.line(&format!("{number} = call ptr @fennec_int_text(i64 {slot})"));
                let opening = self.literal("Id(");
                self.retain(&opening);
                let text = self.joined(&opening, &number, true);
                let closing = self.literal(")");
                Ok(self.joined(&text, &closing, false))
            }
            Kind::Struct(id) => {
                let layouts = self.module.layouts;
                let layout = layouts.get(*id).ok_or("no such layout")?;
                let tuple = layout.name.starts_with('(');
                let mut text = self.literal(if tuple { "(" } else { "" });
                self.retain(&text);
                if !tuple {
                    // A program's own type named like one of std's is marked apart inside, not when shown.
                    let shown = fennec_syntax::without_kits(layout.name.trim_end_matches('\''));
                    // A generic type's layout is named with its arguments, like `Grid[Tile]`, and
                    // its values just `Grid`, as the runtime shows one it holds as a block.
                    let shown = shown.split_once('[').map_or(shown.as_str(), |(declared, _)| declared);
                    let name = self.literal(&format!("{shown}("));
                    text = self.joined(&text, &name, false);
                }
                // An error's notes aren't shown with its fields.
                let shown = layout.fields.iter().filter(|field| !fennec_rt::packed::hidden(&field.name));
                for (at, field) in shown.enumerate() {
                    let field_kind = Kind::from_ty(self.module.layouts, field.ty)
                        .ok_or("a field this backend doesn't take yet")?;
                    let label = match (at, tuple) {
                        (0, true) => String::new(),
                        (_, true) => ", ".to_owned(),
                        (0, false) => format!("{}=", field.name),
                        (_, false) => format!(", {}=", field.name),
                    };
                    if !label.is_empty() {
                        let label = self.literal(&label);
                        text = self.joined(&text, &label, false);
                    }
                    let place = self.offset(value, field.offset);
                    let held =
                        if field_kind.in_memory() { place } else { self.load_field(&field_kind, &place, "") };
                    let piece = self.plain_text(&held, &field_kind)?;
                    text = self.joined(&text, &piece, true);
                }
                let end = self.literal(if tuple && layout.fields.len() == 1 { ",)" } else { ")" });
                Ok(self.joined(&text, &end, false))
            }
            Kind::Enum(id) => {
                let layouts = self.module.layouts;
                let layout = layouts.get(*id).ok_or("no such layout")?;
                let cases = layout.cases.as_ref().ok_or("an enum with no cases")?;
                let tag = self.temp();
                self.line(&format!("{tag} = load i64, ptr {value}"));
                let mut text = self.literal(&format!(
                    "{}.{}",
                    layout.name,
                    cases.first().map_or("", |case| case.name.as_str())
                ));
                for (number, case) in cases.iter().enumerate().skip(1) {
                    let (this, same, picked) =
                        (self.literal(&format!("{}.{}", layout.name, case.name)), self.temp(), self.temp());
                    self.line(&format!("{same} = icmp eq i64 {tag}, {number}"));
                    self.line(&format!("{picked} = select i1 {same}, ptr {this}, ptr {text}"));
                    text = picked;
                }
                self.retain(&text);
                Ok(text)
            }
            other => {
                Err(format!("it formats a {other:?} inside a struct, which this backend doesn't take yet"))
            }
        }
    }

    /// A failed comparison in an `assert`, saying what each side was. Nothing made for this is let
    /// go of, since the program is ending. A value it has no text for leaves the failure to say
    /// only that it failed.
    /// Fails an `assert ... fails`, with what really happened as its help.
    pub(crate) fn assert_said(&mut self, happened: &Operand, span: Span) -> Result<(), String> {
        let (value, kind) = self.operand(happened)?;
        let happened =
            if kind == Kind::Str && self.headers() { self.owned_text_block(&value) } else { value };
        self.declare("declare void @fennec_assert_said(ptr, i64, i64) #1");
        let (from, to) = self.place_in_source(span);
        self.line(&format!("call void @fennec_assert_said(ptr {happened}, i64 {from}, i64 {to})"));
        self.leave_if_failed();
        Ok(())
    }

    pub(crate) fn assert_sides(&mut self, left: &Operand, right: &Operand, span: Span) -> Result<(), String> {
        let (Some(left), Some(right)) = (self.side_text(left)?, self.side_text(right)?) else {
            return Ok(());
        };
        self.declare("declare void @fennec_assert_sides(ptr, ptr, i64, i64) #1");
        let (from, to) = self.place_in_source(span);
        self.line(&format!("call void @fennec_assert_sides(ptr {left}, ptr {right}, i64 {from}, i64 {to})"));
        self.leave_if_failed();
        Ok(())
    }

    /// Ends the program, saying what `main` failed with. What the IR ends this block with is
    /// never reached, so it goes in a block of its own.
    pub(crate) fn main_failed(&mut self, error: &Operand, span: Span) -> Result<(), String> {
        let (value, kind) = self.operand(error)?;
        let shown = self.shown_text(&value, &kind)?;
        let start = self.literal("`main` failed: ");
        let text = self.joined(&start, &shown, true);
        // Its notes go under it, a line each, and its call chain: where it was raised, and each
        // `?` it passed up through.
        let word = self.notes_word(error)?;
        let text = match &word {
            Some(word) => {
                self.declare("declare ptr @fennec_notes_shown(i64) nounwind");
                let notes = self.temp();
                self.line(&format!("{notes} = call ptr @fennec_notes_shown(i64 {word})"));
                self.joined(&text, &notes, true)
            }
            None => text,
        };
        if let Some(word) = word {
            self.declare("declare void @fennec_notes_traced(i64) nounwind");
            self.stop_then(&text, span, &format!("call void @fennec_notes_traced(i64 {word})"));
        } else {
            self.stop_with(&text, span);
        }
        let after = self.label("stopped");
        self.start_block(&after);
        Ok(())
    }

    /// Ends the program or a server's task, saying `start` and what it failed with. What the IR ends
    /// this block with is never reached, so it goes in a block of its own.
    pub(crate) fn failed_with(&mut self, start: &str, error: &Operand, span: Span) -> Result<(), String> {
        let (value, kind) = self.operand(error)?;
        let shown = self.shown_text(&value, &kind)?;
        let start = self.literal(start);
        let text = self.joined(&start, &shown, true);
        self.stop_with(&text, span);
        let after = self.label("stopped");
        self.start_block(&after);
        Ok(())
    }

    /// A new string with the text of the error it is, with no sign of the others it might be.
    fn one_of_several_text(&mut self, value: &str, kind: &Kind, how: Shown) -> Result<String, String> {
        let (Kind::Enum(id) | Kind::Boxed(id)) = kind else { return Err("not one of several".to_owned()) };
        let layouts = self.module.layouts;
        let cases =
            layouts.get(*id).and_then(|layout| layout.cases.as_ref()).ok_or("an enum with no cases")?;
        let base = if matches!(kind, Kind::Boxed(_)) {
            self.offset(value, crate::lists::FIELDS_OFFSET)
        } else {
            value.to_owned()
        };
        let tag = self.temp();
        self.line(&format!("{tag} = load i64, ptr {base}"));
        let empty = self.literal("");
        let shown = self.temp_slot(&empty);
        let joined = self.label("error_shown");
        for (number, case) in cases.iter().enumerate() {
            let field = case.fields.first().ok_or("a case of an error holds nothing")?;
            let held = Kind::from_ty(layouts, field.ty).ok_or("a field this backend doesn't take yet")?;
            let (same, this, next) = (self.temp(), self.label("error_is"), self.label("error_not"));
            self.line(&format!("{same} = icmp eq i64 {tag}, {number}"));
            self.line(&format!("br i1 {same}, label %{this}, label %{next}"));
            self.start_block(&this);
            let place = self.offset(&base, field.offset);
            let inside = if held.in_memory() { place } else { self.load_field(&held, &place, "") };
            // Printed as itself, a string member is its text, unquoted; anything else is written
            // as it's shown.
            let text = match (how, &held) {
                (Shown::Plain, Kind::Str) if self.headers() => self.owned_text_block(&inside),
                (Shown::Plain, Kind::Str) => {
                    self.retain(&inside);
                    inside
                }
                _ => self.shown_text(&inside, &held)?,
            };
            self.line(&format!("store ptr {text}, ptr {shown}"));
            self.line(&format!("br label %{joined}"));
            self.start_block(&next);
        }
        self.line(&format!("br label %{joined}"));
        self.start_block(&joined);
        let text = self.temp();
        self.line(&format!("{text} = load ptr, ptr {shown}"));
        Ok(text)
    }

    /// A new string with a side of a comparison, if it has text.
    fn side_text(&mut self, side: &Operand) -> Result<Option<String>, String> {
        if *side == Operand::None {
            return Ok(Some(self.literal("None")));
        }
        let (value, kind) = self.operand(side)?;
        if !self.has_shown_text(&kind) {
            return Ok(None);
        }
        self.shown_text(&value, &kind).map(Some)
    }

    fn has_shown_text(&self, kind: &Kind) -> bool {
        match kind {
            Kind::Maybe(inner) => self.has_shown_text(inner),
            _ if self.is_optional(kind) => self.case_of(kind, "Some").is_ok_and(|(_, fields)| {
                fields
                    .first()
                    .is_some_and(|(_, _, held)| held.as_ref().is_some_and(|held| self.has_shown_text(held)))
            }),
            Kind::Int
            | Kind::Float
            | Kind::Bool
            | Kind::Str
            | Kind::TextBlock
            | Kind::MaybeText
            | Kind::Struct(_)
            | Kind::Enum(_)
            | Kind::List(_)
            | Kind::Boxed(_)
            | Kind::Bytes
            | Kind::Dict(..) => true,
            _ => false,
        }
    }

    /// A new string with a value's text as it's shown, with a string in quotes.
    fn shown_text(&mut self, value: &str, kind: &Kind) -> Result<String, String> {
        if self.is_one_of_several(kind) {
            return self.one_of_several_text(value, kind, Shown::Quoted);
        }
        let none = self.literal("None");
        if let Kind::Maybe(inner) = kind {
            let (missing, shown) = (self.temp(), self.temp_slot(&none));
            self.line(&format!("{missing} = icmp eq ptr {value}, null"));
            let (holds, joined) = (self.label("holds"), self.label("shown"));
            self.line(&format!("br i1 {missing}, label %{joined}, label %{holds}"));
            self.start_block(&holds);
            let text = self.shown_text(value, inner)?;
            self.line(&format!("store ptr {text}, ptr {shown}"));
            self.line(&format!("br label %{joined}"));
            self.start_block(&joined);
            let text = self.temp();
            self.line(&format!("{text} = load ptr, ptr {shown}"));
            return Ok(text);
        }
        if *kind == Kind::MaybeText {
            let (missing, shown) = (self.header_is_none(value), self.temp_slot(&none));
            let (holds, joined) = (self.label("holds"), self.label("shown"));
            self.line(&format!("br i1 {missing}, label %{joined}, label %{holds}"));
            self.start_block(&holds);
            let text = self.shown_text(value, &Kind::Str)?;
            self.line(&format!("store ptr {text}, ptr {shown}"));
            self.line(&format!("br label %{joined}"));
            self.start_block(&joined);
            let text = self.temp();
            self.line(&format!("{text} = load ptr, ptr {shown}"));
            return Ok(text);
        }
        if self.is_optional(kind) {
            let (number, fields) = self.case_of(kind, "Some")?;
            let (_, offset, held) = fields.first().cloned().ok_or("an optional with nothing in it")?;
            let held = held.ok_or("an optional of something this backend doesn't take yet")?;
            let base = self.fields_at(value, kind);
            let (tag, some, shown) = (self.temp(), self.temp(), self.temp_slot(&none));
            self.line(&format!("{tag} = load i64, ptr {base}"));
            self.line(&format!("{some} = icmp eq i64 {tag}, {number}"));
            let (holds, joined) = (self.label("holds"), self.label("shown"));
            self.line(&format!("br i1 {some}, label %{holds}, label %{joined}"));
            self.start_block(&holds);
            let at = self.offset(&base, offset);
            let inner = if held.in_memory() { at } else { self.load_field(&held, &at, "") };
            let text = self.shown_text(&inner, &held)?;
            self.line(&format!("store ptr {text}, ptr {shown}"));
            self.line(&format!("br label %{joined}"));
            self.start_block(&joined);
            let text = self.temp();
            self.line(&format!("{text} = load ptr, ptr {shown}"));
            return Ok(text);
        }
        match kind {
            Kind::Str | Kind::TextBlock => {
                let block = if *kind == Kind::Str && self.headers() {
                    self.owned_text_block(value)
                } else {
                    value.to_owned()
                };
                self.declare("declare ptr @fennec_str_repr(ptr) nounwind");
                let text = self.temp();
                self.line(&format!("{text} = call ptr @fennec_str_repr(ptr {block})"));
                Ok(text)
            }
            Kind::List(_) | Kind::Boxed(_) | Kind::Bytes | Kind::Dict(..) => Ok(self.text_of_block(value)),
            _ => self.plain_text(value, kind),
        }
    }

    /// Two strings joined into a new one, letting go of the first and, if it's owned, the second.
    fn joined(&mut self, first: &str, second: &str, second_owned: bool) -> String {
        self.declare("declare ptr @fennec_str_concat(ptr, ptr) nounwind");
        let text = self.temp();
        self.line(&format!("{text} = call ptr @fennec_str_concat(ptr {first}, ptr {second})"));
        self.release(first);
        if second_owned {
            self.release(second);
        }
        text
    }

    /// A new string with the text of a list, a dict or a block of a struct, which the runtime
    /// writes from the block.
    fn text_of_block(&mut self, value: &str) -> String {
        self.declare("declare ptr @fennec_block_text(ptr) nounwind");
        let text = self.temp();
        self.line(&format!("{text} = call ptr @fennec_block_text(ptr {value})"));
        text
    }

    /// A new string holding a value, by a spec if it has one.
    fn piece(&mut self, value: &Operand, spec: Option<&str>, span: Span) -> Result<String, String> {
        let (value, kind) = self.operand(value)?;
        let temp = self.temp();
        let Some(spec) = spec else {
            match kind {
                Kind::Int => {
                    self.declare("declare ptr @fennec_int_text(i64) nounwind");
                    self.line(&format!("{temp} = call ptr @fennec_int_text(i64 {value})"));
                }
                Kind::Float => {
                    self.declare("declare ptr @fennec_float_text(double) nounwind");
                    self.line(&format!("{temp} = call ptr @fennec_float_text(double {value})"));
                }
                Kind::U64 | Kind::F32 => return Ok(self.sized_text(&value, &kind)),
                Kind::Str if self.headers() => return Ok(self.owned_text_block(&value)),
                Kind::Str => {
                    self.retain(&value);
                    return Ok(value);
                }
                Kind::Bool | Kind::Struct(_) | Kind::Enum(_) => return self.plain_text(&value, &kind),
                // A union of plain types is its value's text, written as `print` writes it.
                Kind::Boxed(_) if self.is_union(&kind) => return self.plain_text(&value, &kind),
                Kind::List(_) | Kind::Boxed(_) | Kind::Bytes | Kind::Dict(..) => {
                    return Ok(self.text_of_block(&value));
                }
                // A name narrowed to `None` is written so.
                Kind::Unit => {
                    let none = self.literal("None");
                    self.retain(&none);
                    return Ok(none);
                }
                other => return Err(format!("it formats a {other:?}, which this backend doesn't take yet")),
            }
            return Ok(temp);
        };
        let spec = self.literal(spec);
        let (from, to) = self.place_in_source(span);
        match kind {
            Kind::Int => {
                self.declare("declare ptr @fennec_int_spec_text(i64, ptr, i64, i64) nounwind");
                self.line(&format!(
                    "{temp} = call ptr @fennec_int_spec_text(i64 {value}, ptr {spec}, i64 {from}, i64 {to})"
                ));
            }
            Kind::Float => {
                self.declare("declare ptr @fennec_float_spec_text(double, ptr, i64, i64) nounwind");
                self.line(&format!(
                    "{temp} = call ptr @fennec_float_spec_text(double {value}, ptr {spec}, i64 {from}, i64 {to})"
                ));
            }
            Kind::U64 | Kind::F32 => {
                let (call, ty) = if kind == Kind::U64 { ("u64", "i64") } else { ("f32", "double") };
                self.declare(&format!("declare ptr @fennec_{call}_spec_text({ty}, ptr, i64, i64) nounwind"));
                self.line(&format!(
                    "{temp} = call ptr @fennec_{call}_spec_text({ty} {value}, ptr {spec}, i64 {from}, i64 {to})"
                ));
                // A spec it doesn't take yet fails it.
                self.leave_if_failed();
            }
            other => {
                return Err(format!("it formats a {other:?} by a spec, which this backend doesn't take yet"));
            }
        }
        self.leave_if_failed();
        Ok(temp)
    }

    /// An f-string: every piece worked out first, then one block with room for them all, the
    /// pieces copied in.
    pub(crate) fn format(&mut self, dest: LocalId, parts: &[FormatPart], span: Span) -> Result<(), String> {
        if self.headers() {
            return self.format_headers(dest, parts, span);
        }
        let mut pieces = Vec::with_capacity(parts.len());
        for part in parts {
            pieces.push(match part {
                FormatPart::Text(text) => (self.literal(text), false),
                FormatPart::Value { value, spec } => (self.piece(value, spec.as_deref(), span)?, true),
            });
        }
        let value = match pieces.as_slice() {
            [] => {
                let empty = self.literal("");
                self.retain(&empty);
                empty
            }
            [(only, owned)] => {
                if !owned {
                    self.retain(only);
                }
                only.clone()
            }
            _ => {
                let header = HEADER_ACCESS;
                let mut bytes = "0".to_owned();
                let mut chars = "0".to_owned();
                let mut sizes = Vec::with_capacity(pieces.len());
                for (piece, _) in &pieces {
                    let (bytes_at, chars_at) = (
                        self.offset(piece, TEXT_BYTES.unsigned_abs()),
                        self.offset(piece, TEXT_CHARS.unsigned_abs()),
                    );
                    let (size, count, more_bytes, more_chars) =
                        (self.temp(), self.temp(), self.temp(), self.temp());
                    self.line(&format!("{size} = load i64, ptr {bytes_at}{header}"));
                    self.line(&format!("{count} = load i64, ptr {chars_at}{header}"));
                    self.line(&format!("{more_bytes} = add i64 {bytes}, {size}"));
                    self.line(&format!("{more_chars} = add i64 {chars}, {count}"));
                    sizes.push(size);
                    bytes = more_bytes;
                    chars = more_chars;
                }
                self.declare("declare ptr @fennec_str_with_room(i64) nounwind");
                self.declare("declare void @llvm.memcpy.p0.p0.i64(ptr, ptr, i64, i1)");
                // With headers, one that fits is made inline, with no block.
                let paths = self.headers().then(|| {
                    let (inline, block, done) =
                        (self.label("format_inline"), self.label("format_block"), self.label("format_done"));
                    let fits = self.temp();
                    self.line(&format!("{fits} = icmp ule i64 {bytes}, 23"));
                    self.line(&format!("br i1 {fits}, label %{inline}, label %{block}"));
                    self.start_block(&inline);
                    let out = self.place_for_text();
                    self.line(&format!("store {} zeroinitializer, ptr {out}", crate::headers::HEADER));
                    self.copy_pieces(&out, &pieces, &sizes, true);
                    let (tag, byte, tag_at, made) = (self.temp(), self.temp(), self.temp(), self.temp());
                    self.line(&format!("{tag} = or i64 {bytes}, 128"));
                    self.line(&format!("{byte} = trunc i64 {tag} to i8"));
                    self.line(&format!("{tag_at} = getelementptr inbounds i8, ptr {out}, i64 23"));
                    self.line(&format!("store i8 {byte}, ptr {tag_at}"));
                    self.line(&format!("{made} = load {}, ptr {out}", crate::headers::HEADER));
                    let inline_end = self.current.clone();
                    self.line(&format!("br label %{done}"));
                    self.start_block(&block);
                    (made, inline_end, done)
                });
                let made = self.temp();
                self.line(&format!("{made} = call ptr @fennec_str_with_room(i64 {bytes})"));
                let start = self.offset(&made, TEXT.unsigned_abs());
                self.copy_pieces(&start, &pieces, &sizes, false);
                let (bytes_at, chars_at) = (
                    self.offset(&made, TEXT_BYTES.unsigned_abs()),
                    self.offset(&made, TEXT_CHARS.unsigned_abs()),
                );
                self.line(&format!("store i64 {bytes}, ptr {bytes_at}{header}"));
                self.line(&format!("store i64 {chars}, ptr {chars_at}{header}"));
                let value = match paths {
                    Some((inline, inline_end, done)) => {
                        // More than 23 bytes come here, so the header is the block's address and
                        // length.
                        let from_block = self.text_of_long_block(&made, &bytes);
                        let block_end = self.current.clone();
                        self.line(&format!("br label %{done}"));
                        self.start_block(&done);
                        let value = self.temp();
                        self.line(&format!(
                            "{value} = phi {} [ {inline}, %{inline_end} ], [ {from_block}, %{block_end} ]",
                            crate::headers::HEADER
                        ));
                        value
                    }
                    None => made,
                };
                for (piece, owned) in &pieces {
                    if *owned {
                        self.release(piece);
                    }
                }
                if self.headers() {
                    return self.assign(dest, &value);
                }
                value
            }
        };
        let value = self.text_value(&value);
        self.assign(dest, &value)
    }

    /// An f-string where strings are headers, made in one go: each piece is read where it is,
    /// with no block made for it, the lengths are added up, and the text is written once, inline
    /// when it fits. A literal is constant bytes, a string its header's, and an int or a float
    /// a header of its own; only other values are written out as a string first.
    fn format_headers(&mut self, dest: LocalId, parts: &[FormatPart], span: Span) -> Result<(), String> {
        // A string on its own is that string, with another owner.
        if let [FormatPart::Value { value, spec: None }] = parts
            && matches!(self.operand(value)?.1, Kind::Str)
        {
            let (text, _) = self.operand(value)?;
            self.retain_text(&text);
            return self.assign(dest, &text);
        }
        let mut pieces = Vec::with_capacity(parts.len());
        for part in parts {
            match part {
                FormatPart::Text(text) if text.is_empty() => {}
                FormatPart::Text(text) => pieces.push(self.literal_piece(text)),
                FormatPart::Value { value, spec } => {
                    pieces.push(self.value_piece(value, spec.as_deref(), span)?);
                }
            }
        }
        let value = self.built_from(&pieces);
        for piece in &pieces {
            match &piece.owned {
                Some(Owned::Header(header)) => self.release_text(header),
                Some(Owned::Block(block)) => self.release(block),
                None => {}
            }
        }
        self.assign(dest, &value)
    }

    /// A text made of pieces one after another: inline when it fits in a header, and otherwise
    /// one block made at its size, each piece copied in once. Nothing given is let go of.
    fn built_from(&mut self, pieces: &[Piece]) -> String {
        let (mut bytes, mut chars) = ("0".to_owned(), "0".to_owned());
        for piece in pieces {
            let (more_bytes, more_chars) = (self.temp(), self.temp());
            self.line(&format!("{more_bytes} = add nuw nsw i64 {bytes}, {}", piece.size));
            self.line(&format!("{more_chars} = add nuw nsw i64 {chars}, {}", piece.chars));
            (bytes, chars) = (more_bytes, more_chars);
        }
        let (inline, block, done) =
            (self.label("format_inline"), self.label("format_block"), self.label("format_done"));
        let fits = self.temp();
        self.line(&format!("{fits} = icmp ule i64 {bytes}, 23"));
        self.line(&format!("br i1 {fits}, label %{inline}, label %{block}"));
        self.start_block(&inline);
        let out = self.place_for_text();
        self.line(&format!("store {} zeroinitializer, ptr {out}", crate::headers::HEADER));
        self.write_pieces(&out, pieces, true);
        let (tag, byte, tag_at, from_inline) = (self.temp(), self.temp(), self.temp(), self.temp());
        self.line(&format!("{tag} = or i64 {bytes}, 128"));
        self.line(&format!("{byte} = trunc i64 {tag} to i8"));
        self.line(&format!("{tag_at} = getelementptr inbounds i8, ptr {out}, i64 23"));
        self.line(&format!("store i8 {byte}, ptr {tag_at}"));
        self.line(&format!("{from_inline} = load {}, ptr {out}", crate::headers::HEADER));
        let inline_end = self.current.clone();
        self.line(&format!("br label %{done}"));
        self.start_block(&block);
        self.declare("declare ptr @fennec_str_with_room(i64) nounwind");
        let made = self.temp();
        self.line(&format!("{made} = call ptr @fennec_str_with_room(i64 {bytes})"));
        let start = self.offset(&made, TEXT.unsigned_abs());
        self.write_pieces(&start, pieces, false);
        let (bytes_at, chars_at) =
            (self.offset(&made, TEXT_BYTES.unsigned_abs()), self.offset(&made, TEXT_CHARS.unsigned_abs()));
        self.line(&format!("store i64 {bytes}, ptr {bytes_at}{HEADER_ACCESS}"));
        self.line(&format!("store i64 {chars}, ptr {chars_at}{HEADER_ACCESS}"));
        // More than 23 bytes come here, so the header is the block's address and length.
        let from_block = self.text_of_long_block(&made, &bytes);
        let block_end = self.current.clone();
        self.line(&format!("br label %{done}"));
        self.start_block(&done);
        let value = self.temp();
        self.line(&format!(
            "{value} = phi {} [ {from_inline}, %{inline_end} ], [ {from_block}, %{block_end} ]",
            crate::headers::HEADER
        ));
        value
    }

    /// `a + b` of texts that won't fit in a header together or aren't both inline: one block made
    /// at the size of both, each copied in once, with neither boxed first.
    pub(crate) fn joined_texts(&mut self, texts: &[&str]) -> String {
        let pieces: Vec<Piece> = texts
            .iter()
            .map(|text| Piece {
                from: self.header_bytes(text),
                size: self.header_byte_length(text),
                chars: self.header_chars(text),
                owned: None,
            })
            .collect();
        self.built_from(&pieces)
    }

    /// A literal's bytes, as a constant of the module's.
    fn literal_piece(&mut self, text: &str) -> Piece {
        let name = format!("@\"fs_{}\"", fennec_blake3::hash(text.as_bytes()).to_hex());
        let escaped = text.bytes().fold(String::new(), |mut escaped, byte| {
            let _ = write!(escaped, "\\{byte:02X}");
            escaped
        });
        let size = text.len();
        self.declare(&format!(
            "{name} = private unnamed_addr constant [{size} x i8] c\"{escaped}\", align 1"
        ));
        Piece { from: name, size: size.to_string(), chars: text.chars().count().to_string(), owned: None }
    }

    /// A value's text, read where it is: a string's header, an int's or a float's header made
    /// for it, or anything else written out as a string block.
    fn value_piece(&mut self, value: &Operand, spec: Option<&str>, span: Span) -> Result<Piece, String> {
        let (held, kind) = self.operand(value)?;
        let header = crate::headers::HEADER;
        let made = match (&kind, spec) {
            (Kind::Str, None) => None,
            (Kind::Int, None) => {
                let (place, made) = (self.place_for_text(), self.temp());
                self.int_header_helper();
                self.line(&format!("call void @fen_int_header(ptr {place}, i64 {held})"));
                self.line(&format!("{made} = load {header}, ptr {place}"));
                Some(made)
            }
            (Kind::Float, None) => {
                let (call, ty) = ("float", "double");
                self.declare(&format!(
                    "declare void @fennec_{call}_header(ptr sret({header}) align 8, {ty}) nounwind"
                ));
                let (place, made) = (self.place_for_text(), self.temp());
                self.line(&format!(
                    "call void @fennec_{call}_header(ptr sret({header}) align 8 {place}, {ty} {held})"
                ));
                self.line(&format!("{made} = load {header}, ptr {place}"));
                Some(made)
            }
            _ => {
                let block = self.piece(value, spec, span)?;
                let (size, chars) = (self.temp(), self.temp());
                let (bytes_at, chars_at) = (
                    self.offset(&block, TEXT_BYTES.unsigned_abs()),
                    self.offset(&block, TEXT_CHARS.unsigned_abs()),
                );
                self.line(&format!("{size} = load i64, ptr {bytes_at}{HEADER_ACCESS}"));
                self.line(&format!("{chars} = load i64, ptr {chars_at}{HEADER_ACCESS}"));
                let from = self.offset(&block, TEXT.unsigned_abs());
                return Ok(Piece { from, size, chars, owned: Some(Owned::Block(block)) });
            }
        };
        let text = made.clone().unwrap_or(held);
        let (from, size, chars) =
            (self.header_bytes(&text), self.header_byte_length(&text), self.header_chars(&text));
        Ok(Piece { from, size, chars, owned: made.map(Owned::Header) })
    }

    /// Writes an f-string's pieces one after another from `start`: with a few moves each when
    /// they're going in a header, of 23 bytes at most, and with `memcpy` otherwise.
    fn write_pieces(&mut self, start: &str, pieces: &[Piece], small: bool) {
        self.declare("declare void @llvm.memcpy.p0.p0.i64(ptr, ptr, i64, i1)");
        let mut at = "0".to_owned();
        for piece in pieces {
            let (to, after) = (self.temp(), self.temp());
            self.line(&format!("{to} = getelementptr inbounds i8, ptr {start}, i64 {at}"));
            if small {
                self.small_copy(&to, &piece.from, &piece.size);
            } else {
                self.line(&format!(
                    "call void @llvm.memcpy.p0.p0.i64(ptr {to}, ptr {}, i64 {}, i1 false)",
                    piece.from, piece.size
                ));
            }
            self.line(&format!("{after} = add nuw nsw i64 {at}, {}", piece.size));
            at = after;
        }
    }

    /// Copies an f-string's pieces' bytes one after another from `start`, each `small`, of 24
    /// bytes at most, if they're going in a header.
    fn copy_pieces(&mut self, start: &str, pieces: &[(String, bool)], sizes: &[String], small: bool) {
        let mut at = "0".to_owned();
        for ((piece, _), size) in pieces.iter().zip(sizes) {
            let (to, from, after) = (self.temp(), self.offset(piece, TEXT.unsigned_abs()), self.temp());
            self.line(&format!("{to} = getelementptr inbounds i8, ptr {start}, i64 {at}"));
            if small {
                self.small_copy(&to, &from, size);
            } else {
                self.line(&format!(
                    "call void @llvm.memcpy.p0.p0.i64(ptr {to}, ptr {from}, i64 {size}, i1 false)"
                ));
            }
            self.line(&format!("{after} = add i64 {at}, {size}"));
            at = after;
        }
    }

    /// `strip` (0), `lstrip` (1) or `rstrip` (2) of a string. ASCII is looked at here: one with
    /// nothing to take off is shared, as is what's left when that's nothing or one byte, and the
    /// rest is copied once. Other text goes to the
    /// runtime, which knows every kind of space.
    pub(crate) fn strip_text(&mut self, text: &str, which: i64) -> String {
        self.declare("declare ptr @fennec_str_strip(ptr, i64) nounwind");
        self.declare("declare ptr @fennec_str_with_room(i64) nounwind");
        self.declare("declare void @llvm.memcpy.p0.p0.i64(ptr, ptr, i64, i1)");
        // A space, a tab, a line feed, a vertical tab, a form feed or a carriage return.
        let space = |byte: &str, name: &str| {
            format!(
                "  %{name}_off = sub i8 {byte}, 9\n  %{name}_ctl = icmp ult i8 %{name}_off, 5\n  %{name}_sp = icmp eq i8 {byte}, 32\n  %{name} = or i1 %{name}_ctl, %{name}_sp\n"
            )
        };
        let (front, back) = (space("%fb", "fw"), space("%bb", "bw"));
        self.retain_helper();
        self.module.add_helper(
            "fen_strip",
            format!(
                "define internal ptr @fen_strip(ptr %text, i64 %which, ptr %empty) #0 {{\nentry:\n  %bytes_at = getelementptr inbounds i8, ptr %text, i64 {TEXT_BYTES}\n  %n = load i64, ptr %bytes_at\n  %chars_at = getelementptr inbounds i8, ptr %text, i64 {TEXT_CHARS}\n  %chars = load i64, ptr %chars_at\n  %body = getelementptr inbounds i8, ptr %text, i64 {TEXT}\n  %ascii = icmp eq i64 %n, %chars\n  br i1 %ascii, label %front, label %slow\nfront:\n  %keep_front = icmp eq i64 %which, 2\n  br i1 %keep_front, label %ends, label %front_loop\nfront_loop:\n  %s = phi i64 [ 0, %front ], [ %s_next, %front_on ]\n  %s_in = icmp ult i64 %s, %n\n  br i1 %s_in, label %front_test, label %front_done\nfront_test:\n  %fp = getelementptr inbounds i8, ptr %body, i64 %s\n  %fb = load i8, ptr %fp\n{front}  br i1 %fw, label %front_on, label %front_done\nfront_on:\n  %s_next = add i64 %s, 1\n  br label %front_loop\nfront_done:\n  br label %ends\nends:\n  %start = phi i64 [ 0, %front ], [ %s, %front_done ]\n  %keep_back = icmp eq i64 %which, 1\n  br i1 %keep_back, label %scanned, label %back_loop\nback_loop:\n  %e = phi i64 [ %n, %ends ], [ %e_less, %back_on ]\n  %e_in = icmp ugt i64 %e, %start\n  br i1 %e_in, label %back_test, label %back_done\nback_test:\n  %e_less = sub i64 %e, 1\n  %bp = getelementptr inbounds i8, ptr %body, i64 %e_less\n  %bb = load i8, ptr %bp\n{back}  br i1 %bw, label %back_on, label %back_done\nback_on:\n  br label %back_loop\nback_done:\n  br label %scanned\nscanned:\n  %end = phi i64 [ %n, %ends ], [ %e, %back_done ]\n  %from_start = icmp eq i64 %start, 0\n  %to_end = icmp eq i64 %end, %n\n  %whole = and i1 %from_start, %to_end\n  br i1 %whole, label %share, label %copy\nshare:\n  call void @fen_retain(ptr %text)\n  ret ptr %text\ncopy:\n  %length = sub i64 %end, %start\n  %none_left = icmp eq i64 %length, 0\n  br i1 %none_left, label %nothing, label %some\nnothing:\n  call void @fen_retain(ptr %empty)\n  ret ptr %empty\nsome:\n  %one_left = icmp eq i64 %length, 1\n  br i1 %one_left, label %one, label %many\none:\n  %only_at = getelementptr inbounds i8, ptr %body, i64 %start\n  %only = load i8, ptr %only_at\n  %only_wide = zext i8 %only to i64\n  %table = load ptr, ptr @fennec_ascii, !invariant.load !1\n  %entry_at = getelementptr inbounds i64, ptr %table, i64 %only_wide\n  %shared = load ptr, ptr %entry_at\n  call void @fen_retain(ptr %shared)\n  ret ptr %shared\nmany:\n  %made = call ptr @fennec_str_with_room(i64 %length)\n  %to = getelementptr inbounds i8, ptr %made, i64 {TEXT}\n  %from = getelementptr inbounds i8, ptr %body, i64 %start\n  call void @llvm.memcpy.p0.p0.i64(ptr %to, ptr %from, i64 %length, i1 false)\n  %made_bytes = getelementptr inbounds i8, ptr %made, i64 {TEXT_BYTES}\n  store i64 %length, ptr %made_bytes\n  %made_chars = getelementptr inbounds i8, ptr %made, i64 {TEXT_CHARS}\n  store i64 %length, ptr %made_chars\n  ret ptr %made\nslow:\n  %stripped = call ptr @fennec_str_strip(ptr %text, i64 %which)\n  ret ptr %stripped\n}}\n\n"
            ),
        );
        // Nothing left is the one empty string, and one byte left the runtime's string of it.
        self.declare("@fennec_ascii = external dso_local global ptr");
        let empty = self.literal("");
        let stripped = self.temp();
        self.line(&format!("{stripped} = call ptr @fen_strip(ptr {text}, i64 {which}, ptr {empty})"));
        stripped
    }

    /// An f-string's parts, or `str(value)`, put straight on the end of the string pieces are
    /// joined into, rather than made a string of their own first.
    pub(crate) fn append_parts(
        &mut self,
        joined: LocalId,
        parts: &[FormatPart],
        span: Span,
    ) -> Result<(), String> {
        let (slot, mut text) = (self.slot(joined), self.temp());
        self.line(&format!("{text} = load ptr, ptr {slot}"));
        for part in parts {
            // A string's header goes straight on, with no block made for it, and so does a
            // number's text, written as a header: its digits always fit in one.
            if let FormatPart::Value { value, spec: None } = part
                && self.headers()
                && matches!(self.operand_kind(value)?, Kind::Str | Kind::Int | Kind::Float)
            {
                let (held, kind) = self.operand(value)?;
                let header = match kind {
                    Kind::Int => {
                        let (place, made) = (self.place_for_text(), self.temp());
                        self.int_header_helper();
                        self.line(&format!("call void @fen_int_header(ptr {place}, i64 {held})"));
                        self.line(&format!("{made} = load {}, ptr {place}", crate::headers::HEADER));
                        made
                    }
                    Kind::Float => {
                        let header = crate::headers::HEADER;
                        self.declare(&format!(
                            "declare void @fennec_float_header(ptr sret({header}) align 8, double) nounwind"
                        ));
                        let (place, made) = (self.place_for_text(), self.temp());
                        self.line(&format!(
                            "call void @fennec_float_header(ptr sret({header}) align 8 {place}, double {held})"
                        ));
                        self.line(&format!("{made} = load {header}, ptr {place}"));
                        made
                    }
                    _ => held,
                };
                text = self.append_piece(&text, &header);
                continue;
            }
            let (piece, owned) = match part {
                FormatPart::Text(literal) => (self.literal(literal), false),
                FormatPart::Value { value, spec } => (self.piece(value, spec.as_deref(), span)?, true),
            };
            text = self.append_text(&text, &piece);
            if owned {
                self.release(&piece);
            }
        }
        self.line(&format!("store ptr {text}, ptr {slot}"));
        Ok(())
    }

    /// `text += more`, taking over `text`'s owner: in place when nothing else holds it and it
    /// has room, which needs no call, or by the runtime, which may make a bigger block.
    pub(crate) fn append_text(&mut self, text: &str, more: &str) -> String {
        self.declare("declare ptr @fennec_str_append(ptr, ptr) nounwind");
        self.declare("declare void @llvm.memcpy.p0.p0.i64(ptr, ptr, i64, i1)");
        let header = HEADER_ACCESS;
        let at = |emit: &mut Self, block: &str, offset: i32| emit.offset(block, offset.unsigned_abs());
        let alone = self.alone(text);
        let (length_at, more_length_at, room_at) =
            (at(self, text, TEXT_BYTES), at(self, more, TEXT_BYTES), at(self, text, TEXT_ROOM));
        let (length, more_length, total, room, fits, quick) =
            (self.temp(), self.temp(), self.temp(), self.temp(), self.temp(), self.temp());
        self.line(&format!("{length} = load i64, ptr {length_at}{header}"));
        self.line(&format!("{more_length} = load i64, ptr {more_length_at}{header}"));
        self.line(&format!("{total} = add i64 {length}, {more_length}"));
        self.line(&format!("{room} = load i64, ptr {room_at}{header}"));
        self.line(&format!("{fits} = icmp ule i64 {total}, {room}"));
        self.line(&format!("{quick} = and i1 {alone}, {fits}"));
        let (in_place, by_runtime, joined) =
            (self.label("in_place"), self.label("grow"), self.label("appended"));
        self.line(&format!("br i1 {quick}, label %{in_place}, label %{by_runtime}, !prof !8"));
        self.start_block(&in_place);
        let (start, from) = (at(self, text, TEXT), at(self, more, TEXT));
        let to = self.temp();
        self.line(&format!("{to} = getelementptr inbounds i8, ptr {start}, i64 {length}"));
        self.line(&format!(
            "call void @llvm.memcpy.p0.p0.i64(ptr {to}, ptr {from}, i64 {more_length}, i1 false)"
        ));
        self.line(&format!("store i64 {total}, ptr {length_at}{header}"));
        let (chars_at, more_chars_at) = (at(self, text, TEXT_CHARS), at(self, more, TEXT_CHARS));
        let (chars, more_chars, all_chars) = (self.temp(), self.temp(), self.temp());
        self.line(&format!("{chars} = load i64, ptr {chars_at}{header}"));
        self.line(&format!("{more_chars} = load i64, ptr {more_chars_at}{header}"));
        self.line(&format!("{all_chars} = add i64 {chars}, {more_chars}"));
        self.line(&format!("store i64 {all_chars}, ptr {chars_at}{header}"));
        let quick_end = self.current.clone();
        self.line(&format!("br label %{joined}"));
        self.start_block(&by_runtime);
        let grown = self.temp();
        self.line(&format!("{grown} = call ptr @fennec_str_append(ptr {text}, ptr {more})"));
        let slow_end = self.current.clone();
        self.line(&format!("br label %{joined}"));
        self.start_block(&joined);
        let result = self.temp();
        self.line(&format!("{result} = phi ptr [ {text}, %{quick_end} ], [ {grown}, %{slow_end} ]"));
        result
    }

    /// Which of two strings comes first, as -1, 0 or 1.
    pub(crate) fn text_order(&mut self, a: &str, b: &str) -> String {
        self.declare("declare i64 @fennec_str_compare(ptr, ptr) nounwind");
        // Strings of up to a word compare as big-endian numbers, the bytes past their
        // ends masked off, then by length; longer ones go to the runtime.
        self.declare("declare i64 @llvm.bswap.i64(i64)");
        self.module.add_helper(
            "fen_compare_text",
            format!(
                "define internal i64 @fen_compare_text(ptr %a, ptr %b) #0 {{\nentry:\n  %a_at = getelementptr inbounds i8, ptr %a, i64 {TEXT_BYTES}\n  %b_at = getelementptr inbounds i8, ptr %b, i64 {TEXT_BYTES}\n  %an = load i64, ptr %a_at\n  %bn = load i64, ptr %b_at\n  %a_short = icmp ule i64 %an, 8\n  %b_short = icmp ule i64 %bn, 8\n  %both = and i1 %a_short, %b_short\n  br i1 %both, label %word, label %slow\nword:\n  %a_text = getelementptr inbounds i8, ptr %a, i64 {TEXT}\n  %b_text = getelementptr inbounds i8, ptr %b, i64 {TEXT}\n  %aw = load i64, ptr %a_text\n  %bw = load i64, ptr %b_text\n  %as = call i64 @llvm.bswap.i64(i64 %aw)\n  %bs = call i64 @llvm.bswap.i64(i64 %bw)\n  %a_bits = shl i64 %an, 3\n  %b_bits = shl i64 %bn, 3\n  %a_shift = sub i64 64, %a_bits\n  %b_shift = sub i64 64, %b_bits\n  %a_empty = icmp eq i64 %an, 0\n  %b_empty = icmp eq i64 %bn, 0\n  %a_keep = select i1 %a_empty, i64 0, i64 %a_shift\n  %b_keep = select i1 %b_empty, i64 0, i64 %b_shift\n  %a_ones = shl i64 -1, %a_keep\n  %b_ones = shl i64 -1, %b_keep\n  %a_mask = select i1 %a_empty, i64 0, i64 %a_ones\n  %b_mask = select i1 %b_empty, i64 0, i64 %b_ones\n  %ak = and i64 %as, %a_mask\n  %bk = and i64 %bs, %b_mask\n  %same = icmp eq i64 %ak, %bk\n  %lt = icmp ult i64 %ak, %bk\n  %gt = icmp ugt i64 %ak, %bk\n  %nlt = icmp ult i64 %an, %bn\n  %ngt = icmp ugt i64 %an, %bn\n  %less = select i1 %same, i1 %nlt, i1 %lt\n  %more = select i1 %same, i1 %ngt, i1 %gt\n  %low = select i1 %less, i64 -1, i64 0\n  %order = select i1 %more, i64 1, i64 %low\n  ret i64 %order\nslow:\n  %counted = call i64 @fennec_str_compare(ptr %a, ptr %b)\n  ret i64 %counted\n}}\n\n"
            ),
        );
        let ordering = self.temp();
        self.line(&format!("{ordering} = call i64 @fen_compare_text(ptr {a}, ptr {b})"));
        ordering
    }

    /// An operator on two strings.
    pub(crate) fn string_binary(&mut self, op: BinOp, (a, b): (&str, &str)) -> Result<String, String> {
        match op {
            BinOp::Add => {
                self.declare("declare ptr @fennec_str_concat(ptr, ptr) nounwind");
                let joined = self.temp();
                self.line(&format!("{joined} = call ptr @fennec_str_concat(ptr {a}, ptr {b})"));
                Ok(joined)
            }
            BinOp::Eq | BinOp::NotEq => Ok(self.same_text(op, (a, b))),
            BinOp::Lt | BinOp::LtEq | BinOp::Gt | BinOp::GtEq => {
                let cc = match op {
                    BinOp::Lt => "slt",
                    BinOp::LtEq => "sle",
                    BinOp::Gt => "sgt",
                    _ => "sge",
                };
                let (ordering, result) = (self.text_order(a, b), self.temp());
                self.line(&format!("{result} = icmp {cc} i64 {ordering}, 0"));
                Ok(result)
            }
            BinOp::In | BinOp::NotIn => {
                self.declare("declare i8 @fennec_str_contains(ptr, ptr) nounwind");
                let (found, bit) = (self.temp(), self.temp());
                self.line(&format!("{found} = call i8 @fennec_str_contains(ptr {b}, ptr {a})"));
                self.line(&format!("{bit} = trunc i8 {found} to i1"));
                if op == BinOp::In {
                    return Ok(bit);
                }
                let flipped = self.temp();
                self.line(&format!("{flipped} = xor i1 {bit}, true"));
                Ok(flipped)
            }
            other => Err(format!("it uses {other:?} on strings, which this backend doesn't take yet")),
        }
    }

    /// `==` with a literal of at most eight bytes: its length and its one word, zeros after
    /// the bytes as in every string's block, are known here.
    pub(crate) fn same_as_short(&mut self, op: BinOp, text: &str, literal: &str) -> String {
        let mut bytes = [0_u8; 8];
        for (slot, byte) in bytes.iter_mut().zip(literal.bytes()) {
            *slot = byte;
        }
        let word = i64::from_le_bytes(bytes);
        let (length_at, word_at) =
            (self.offset(text, TEXT_BYTES.unsigned_abs()), self.offset(text, TEXT.unsigned_abs()));
        let (length, held, same_length, same_word, same) =
            (self.temp(), self.temp(), self.temp(), self.temp(), self.temp());
        self.load_length(&length, &length_at);
        self.line(&format!("{held} = load i64, ptr {word_at}"));
        self.line(&format!("{same_length} = icmp eq i64 {length}, {}", literal.len()));
        self.line(&format!("{same_word} = icmp eq i64 {held}, {word}"));
        self.line(&format!("{same} = and i1 {same_length}, {same_word}"));
        if op == BinOp::Eq {
            return same;
        }
        let differs = self.temp();
        self.line(&format!("{differs} = xor i1 {same}, true"));
        differs
    }

    /// `==` on two strings: the same block, different lengths and short ones decide it here.
    pub(crate) fn same_text(&mut self, op: BinOp, (a, b): (&str, &str)) -> String {
        self.declare("declare i8 @fennec_box_equal(ptr, ptr) nounwind");
        self.module.add_helper(
            "fen_same_text",
            format!(
                "define internal i1 @fen_same_text(ptr %a, ptr %b) #0 {{\nentry:\n  %same = icmp eq ptr %a, %b\n  br i1 %same, label %yes, label %lengths\nlengths:\n  %a_at = getelementptr inbounds i8, ptr %a, i64 {TEXT_BYTES}\n  %b_at = getelementptr inbounds i8, ptr %b, i64 {TEXT_BYTES}\n  %a_bytes = load i64, ptr %a_at\n  %b_bytes = load i64, ptr %b_at\n  %alike = icmp eq i64 %a_bytes, %b_bytes\n  br i1 %alike, label %size, label %no\nsize:\n  %short = icmp ule i64 %a_bytes, 8\n  br i1 %short, label %word, label %slow\nword:\n  %a_text = getelementptr inbounds i8, ptr %a, i64 {TEXT}\n  %b_text = getelementptr inbounds i8, ptr %b, i64 {TEXT}\n  %a_word = load i64, ptr %a_text\n  %b_word = load i64, ptr %b_text\n  %equal_word = icmp eq i64 %a_word, %b_word\n  ret i1 %equal_word\nslow:\n  %equal = call i8 @fennec_box_equal(ptr %a, ptr %b)\n  %is = icmp ne i8 %equal, 0\n  ret i1 %is\nyes:\n  ret i1 true\nno:\n  ret i1 false\n}}\n\n"
            ),
        );
        let bit = self.temp();
        self.line(&format!("{bit} = call i1 @fen_same_text(ptr {a}, ptr {b})"));
        if op == BinOp::Eq {
            return bit;
        }
        let flipped = self.temp();
        self.line(&format!("{flipped} = xor i1 {bit}, true"));
        flipped
    }

    /// `==` on two blocks, as the runtime compares them.
    pub(crate) fn same_blocks(&mut self, op: BinOp, (a, b): (&str, &str)) -> String {
        self.declare("declare i8 @fennec_box_equal(ptr, ptr) nounwind");
        let (same, bit) = (self.temp(), self.temp());
        self.line(&format!("{same} = call i8 @fennec_box_equal(ptr {a}, ptr {b})"));
        self.line(&format!("{bit} = trunc i8 {same} to i1"));
        if op == BinOp::Eq {
            return bit;
        }
        let flipped = self.temp();
        self.line(&format!("{flipped} = xor i1 {bit}, true"));
        flipped
    }

    /// `text[index]`, which leaves the function if it's out of range.
    pub(crate) fn string_index(
        &mut self,
        dest: LocalId,
        text: &str,
        index: &Operand,
        span: Span,
    ) -> Result<(), String> {
        let (index, _) = self.operand(index)?;
        // In an ASCII string a character's place is its byte's, so one inside it comes from the
        // table, and only the rest go to the runtime, which counts characters from the start.
        let (bytes_at, chars_at) = (self.offset(text, 16), self.offset(text, 24));
        let (bytes, chars, ascii, inside, quick) =
            (self.temp(), self.temp(), self.temp(), self.temp(), self.temp());
        self.line(&format!("{bytes} = load i64, ptr {bytes_at}"));
        self.line(&format!("{chars} = load i64, ptr {chars_at}"));
        self.line(&format!("{ascii} = icmp eq i64 {bytes}, {chars}"));
        self.line(&format!("{inside} = icmp ult i64 {index}, {bytes}"));
        self.line(&format!("{quick} = and i1 {ascii}, {inside}"));
        let (table_path, runtime_path, joined) =
            (self.label("table"), self.label("count"), self.label("indexed"));
        self.line(&format!("br i1 {quick}, label %{table_path}, label %{runtime_path}, !prof !8"));
        self.start_block(&table_path);
        self.declare("@fennec_ascii = external dso_local global ptr");
        let start = self.offset(text, crate::emit::TEXT_START);
        let (lead_at, lead, wide, table, slot, from_table) =
            (self.temp(), self.temp(), self.temp(), self.temp(), self.temp(), self.temp());
        self.line(&format!("{lead_at} = getelementptr inbounds i8, ptr {start}, i64 {index}"));
        self.line(&format!("{lead} = load i8, ptr {lead_at}"));
        self.line(&format!("{wide} = zext i8 {lead} to i64"));
        self.line(&format!("{table} = load ptr, ptr @fennec_ascii, !invariant.load !1"));
        self.line(&format!("{slot} = getelementptr inbounds i64, ptr {table}, i64 {wide}"));
        self.line(&format!("{from_table} = load ptr, ptr {slot}"));
        self.retain(&from_table);
        let table_end = self.current.clone();
        self.line(&format!("br label %{joined}"));
        self.start_block(&runtime_path);
        self.declare("declare ptr @fennec_str_index(ptr, i64, i64, i64) nounwind");
        let (from, to) = self.place_in_source(span);
        let counted = self.temp();
        self.line(&format!(
            "{counted} = call ptr @fennec_str_index(ptr {text}, i64 {index}, i64 {from}, i64 {to})"
        ));
        // An index past the end fails here: a block made for the text goes on the way out.
        self.leave_if_failed();
        let runtime_end = self.current.clone();
        self.line(&format!("br label %{joined}"));
        self.start_block(&joined);
        let one = self.temp();
        self.line(&format!("{one} = phi ptr [ {from_table}, %{table_end} ], [ {counted}, %{runtime_end} ]"));
        let one = self.text_value(&one);
        self.assign(dest, &one)
    }

    /// Leaves the function if the runtime has recorded a failure.
    /// Where this thread's failure flag is.
    pub(crate) fn failure_flag(&mut self) -> String {
        let (declared, symbol) = if self.module.threads && !self.on_workers() {
            ("@fennec_failed_main = external dso_local global ptr", "@fennec_failed_main")
        } else {
            (crate::parallel::failed_global(self.module.threads), "@fennec_failed")
        };
        self.declare(declared);
        let flag_at = self.temp();
        self.line(&format!("{flag_at} = load ptr, ptr {symbol}, !invariant.load !1"));
        flag_at
    }

    pub(crate) fn leave_if_failed(&mut self) {
        self.note_runtime_check();
        let flag_at = self.failure_flag();
        let (flag, failed) = (self.temp(), self.temp());
        self.line(&format!("{flag} = load i8, ptr {flag_at}"));
        self.line(&format!("{failed} = icmp ne i8 {flag}, 0"));
        self.leave_if(&failed);
    }

    /// Defines `@fen_int_header(out, n)`, which writes `n`'s decimal text into the inline header
    /// at `out`, as `fennec_int_header` does: no call, so LLVM can fold it into where it's used.
    /// It counts the digits from the highest bit set, then writes them from the last, two at a
    /// time from a table of the hundred pairs.
    pub(crate) fn int_header_helper(&mut self) {
        use crate::headers::HEADER;
        self.declare("declare i64 @llvm.ctlz.i64(i64, i1)");
        self.module.add_helper(
            "fen_int_header",
            format!(
                "@fen_pow10 = private unnamed_addr constant [20 x i64] [i64 1, i64 10, i64 100, i64 1000, i64 10000, i64 100000, i64 1000000, i64 10000000, i64 100000000, i64 1000000000, i64 10000000000, i64 100000000000, i64 1000000000000, i64 10000000000000, i64 100000000000000, i64 1000000000000000, i64 10000000000000000, i64 100000000000000000, i64 1000000000000000000, i64 10000000000000000000]
@fen_digit_pairs = private unnamed_addr constant [200 x i8] c\"00010203040506070809101112131415161718192021222324252627282930313233343536373839404142434445464748495051525354555657585960616263646566676869707172737475767778798081828384858687888990919293949596979899\"

define internal void @fen_int_header(ptr %out, i64 %n) #0 {{
entry:
  store {HEADER} zeroinitializer, ptr %out
  %negative = icmp slt i64 %n, 0
  %flipped = sub i64 0, %n
  %size = select i1 %negative, i64 %flipped, i64 %n
  %some = or i64 %size, 1
  %zeros = call i64 @llvm.ctlz.i64(i64 %some, i1 true)
  %bits = sub nuw nsw i64 64, %zeros
  %scaled = mul nuw nsw i64 %bits, 1233
  %guess = lshr i64 %scaled, 12
  %power_at = getelementptr inbounds [20 x i64], ptr @fen_pow10, i64 0, i64 %guess
  %power = load i64, ptr %power_at
  %below = icmp ult i64 %size, %power
  %less = zext i1 %below to i64
  %counted = sub i64 %guess, %less
  %digits_or_none = add i64 %counted, 1
  %no_digits = icmp eq i64 %digits_or_none, 0
  %digits = select i1 %no_digits, i64 1, i64 %digits_or_none
  %sign = zext i1 %negative to i64
  %length = add nuw nsw i64 %digits, %sign
  br i1 %negative, label %minus, label %pairs
minus:
  store i8 45, ptr %out
  br label %pairs
pairs:
  %left = phi i64 [ %size, %entry ], [ %size, %minus ], [ %rest, %pair ]
  %end = phi i64 [ %length, %entry ], [ %length, %minus ], [ %pair_at, %pair ]
  %more = icmp uge i64 %left, 100
  br i1 %more, label %pair, label %last
pair:
  %rest = udiv i64 %left, 100
  %hundreds = mul i64 %rest, 100
  %two = sub i64 %left, %hundreds
  %pair_at = sub i64 %end, 2
  %two_at = shl nuw nsw i64 %two, 1
  %from = getelementptr inbounds i8, ptr @fen_digit_pairs, i64 %two_at
  %both = load i16, ptr %from, align 1
  %to = getelementptr inbounds i8, ptr %out, i64 %pair_at
  store i16 %both, ptr %to, align 1
  br label %pairs
last:
  %tens = icmp uge i64 %left, 10
  br i1 %tens, label %last_two, label %last_one
last_two:
  %last_at = sub i64 %end, 2
  %last_pair_at = shl nuw nsw i64 %left, 1
  %last_from = getelementptr inbounds i8, ptr @fen_digit_pairs, i64 %last_pair_at
  %last_both = load i16, ptr %last_from, align 1
  %last_to = getelementptr inbounds i8, ptr %out, i64 %last_at
  store i16 %last_both, ptr %last_to, align 1
  br label %tagged
last_one:
  %one_at = sub i64 %end, 1
  %narrow = trunc i64 %left to i8
  %digit = add i8 %narrow, 48
  %one_to = getelementptr inbounds i8, ptr %out, i64 %one_at
  store i8 %digit, ptr %one_to
  br label %tagged
tagged:
  %tag = or i64 %length, 128
  %tag_byte = trunc i64 %tag to i8
  %tag_at = getelementptr inbounds i8, ptr %out, i64 23
  store i8 %tag_byte, ptr %tag_at
  ret void
}}

"
            ),
        );
    }

    /// A new string with a `u64`'s or an `f32`'s text: unsigned, or the shortest that reads back
    /// as the same `f32`.
    fn sized_text(&mut self, value: &str, kind: &Kind) -> String {
        let (call, ty) = if *kind == Kind::U64 { ("u64", "i64") } else { ("f32", "double") };
        self.declare(&format!("declare ptr @fennec_{call}_text({ty}) nounwind"));
        let text = self.temp();
        self.line(&format!("{text} = call ptr @fennec_{call}_text({ty} {value})"));
        text
    }

    /// `str(value)`.
    pub(crate) fn text_of_value(&mut self, dest: LocalId, value: &Operand, span: Span) -> Result<(), String> {
        let (held, kind) = self.operand(value)?;
        let temp = self.temp();
        if self.headers() && kind == Kind::Int {
            let place = self.place_for_text();
            self.int_header_helper();
            self.line(&format!("call void @fen_int_header(ptr {place}, i64 {held})"));
            self.line(&format!("{temp} = load {}, ptr {place}", crate::headers::HEADER));
            return self.assign(dest, &temp);
        }
        if self.headers() && matches!(kind, Kind::Float | Kind::U64 | Kind::F32) {
            let (call, ty) = match kind {
                Kind::U64 => ("u64", "i64"),
                Kind::F32 => ("f32", "double"),
                _ => ("float", "double"),
            };
            let header = crate::headers::HEADER;
            self.declare(&format!(
                "declare void @fennec_{call}_header(ptr sret({header}) align 8, {ty}) nounwind"
            ));
            let place = self.place_for_text();
            self.line(&format!(
                "call void @fennec_{call}_header(ptr sret({header}) align 8 {place}, {ty} {held})"
            ));
            self.line(&format!("{temp} = load {header}, ptr {place}"));
            return self.assign(dest, &temp);
        }
        match kind {
            Kind::Int => {
                self.declare("declare ptr @fennec_int_text(i64) nounwind");
                self.line(&format!("{temp} = call ptr @fennec_int_text(i64 {held})"));
            }
            Kind::Float => {
                self.declare("declare ptr @fennec_float_text(double) nounwind");
                self.line(&format!("{temp} = call ptr @fennec_float_text(double {held})"));
            }
            Kind::U64 | Kind::F32 => {
                let text = self.sized_text(&held, &kind);
                return self.assign(dest, &text);
            }
            Kind::Str => {
                self.retain_value(&held, &Kind::Str);
                return self.assign(dest, &held);
            }
            _ => {
                let text = self.piece(value, None, span)?;
                let text = self.text_value(&text);
                return self.assign(dest, &text);
            }
        }
        let temp = self.text_value(&temp);
        self.assign(dest, &temp)
    }

    /// No case of a `match` took the value. The runtime's message shows it where it can.
    pub(crate) fn no_match(&mut self, value: &Operand, span: Span) -> Result<(), String> {
        let (held, kind) = self.operand(value)?;
        let handle = self.temp();
        match kind {
            Kind::Int => {
                self.declare("declare i64 @fennec_int(i64) nounwind");
                self.line(&format!("{handle} = call i64 @fennec_int(i64 {held})"));
            }
            Kind::Float => {
                self.declare("declare i64 @fennec_float(double) nounwind");
                self.line(&format!("{handle} = call i64 @fennec_float(double {held})"));
            }
            Kind::Str => {
                self.declare("declare i64 @fennec_box_to_value(ptr) nounwind");
                self.on_blocks([&held], |emit, [block]| {
                    emit.line(&format!("{handle} = call i64 @fennec_box_to_value(ptr {block})"));
                });
            }
            Kind::List(_) | Kind::Boxed(_) => {
                self.declare("declare i64 @fennec_box_to_value(ptr) nounwind");
                self.line(&format!("{handle} = call i64 @fennec_box_to_value(ptr {held})"));
            }
            Kind::Bool => {
                self.declare("declare i64 @fennec_bool(i8) nounwind");
                let byte = self.temp();
                self.line(&format!("{byte} = zext i1 {held} to i8"));
                self.line(&format!("{handle} = call i64 @fennec_bool(i8 {byte})"));
            }
            // The checker has shown a `match` on an enum takes every case, so nothing's shown.
            Kind::Enum(_) => {
                self.declare("declare i64 @fennec_unit() nounwind");
                self.line(&format!("{handle} = call i64 @fennec_unit()"));
            }
            other => return Err(format!("it matches on a {other:?}, which this backend doesn't take yet")),
        }
        self.declare("declare void @fennec_no_match(i64, i64, i64) #1");
        let (from, to) = self.place_in_source(span);
        self.line(&format!("call void @fennec_no_match(i64 {handle}, i64 {from}, i64 {to})"));
        // A match the checker saw covers everything, so this is never a check of the program's.
        self.next_check(span, None);
        let failed = self.failed_exit();
        self.line(&failed);
        Ok(())
    }

    /// A builtin on strings.
    pub(crate) fn text_builtin(
        &mut self,
        dest: Option<LocalId>,
        name: &str,
        args: &[Operand],
        span: Span,
    ) -> Result<(), String> {
        if self.headers()
            && let (Some(dest), Some(first)) = (dest, args.first())
        {
            match name {
                "str.len" => {
                    let (text, _) = self.operand(first)?;
                    let length = self.header_chars(&text);
                    return self.assign(dest, &length);
                }
                "str.code" => {
                    let (text, _) = self.operand(first)?;
                    return self.header_code(dest, &text, span);
                }
                _ => {}
            }
        }
        let mut made = Vec::new();
        let result = self.text_builtin_on(dest, name, args, span, &mut made);
        for (block, owned) in &made {
            self.let_go_text_block(block, owned);
        }
        result
    }

    /// A string operand as a block: with headers, one made for this call is kept in `made`, to
    /// be let go of after.
    fn block_operand(
        &mut self,
        operand: &Operand,
        made: &mut Vec<(String, String)>,
    ) -> Result<String, String> {
        let (value, kind) = self.operand(operand)?;
        if kind != Kind::Str || !self.headers() {
            return Ok(value);
        }
        let (block, owned) = self.text_block(&value);
        made.push((block.clone(), owned));
        Ok(block)
    }

    fn text_builtin_on(
        &mut self,
        dest: Option<LocalId>,
        name: &str,
        args: &[Operand],
        span: Span,
        made: &mut Vec<(String, String)>,
    ) -> Result<(), String> {
        let first = args.first();
        match name {
            "str.len" => {
                let text = self.block_operand(first.ok_or("no argument")?, made)?;
                let (at, length) = (self.temp(), self.temp());
                self.line(&format!("{at} = getelementptr inbounds i8, ptr {text}, i64 {TEXT_CHARS}"));
                self.load_length(&length, &at);
                self.assign(dest.ok_or("no result")?, &length)
            }
            "str" => self.text_of_value(dest.ok_or("no result")?, first.ok_or("no argument")?, span),
            "str.code" => {
                // One byte of ASCII is its own code; anything else goes to the runtime, which
                // also fails for a string that isn't one character.
                let text = self.block_operand(first.ok_or("no argument")?, made)?;
                let bytes_at = self.offset(&text, TEXT_BYTES.unsigned_abs());
                let (bytes, one) = (self.temp(), self.temp());
                self.load_length(&bytes, &bytes_at);
                self.line(&format!("{one} = icmp eq i64 {bytes}, 1"));
                let (quick, slow, joined) = (self.label("byte"), self.label("code"), self.label("coded"));
                self.line(&format!("br i1 {one}, label %{quick}, label %{slow}, !prof !8"));
                self.start_block(&quick);
                let start = self.offset(&text, TEXT.unsigned_abs());
                let (byte, wide) = (self.temp(), self.temp());
                self.line(&format!("{byte} = load i8, ptr {start}"));
                self.line(&format!("{wide} = zext i8 {byte} to i64"));
                self.line(&format!("br label %{joined}"));
                self.start_block(&slow);
                self.declare("declare i64 @fennec_str_code(ptr, i64, i64) nounwind");
                let (from, to) = self.place_in_source(span);
                let counted = self.temp();
                self.line(&format!(
                    "{counted} = call i64 @fennec_str_code(ptr {text}, i64 {from}, i64 {to})"
                ));
                self.leave_if_failed();
                let slow_end = self.current.clone();
                self.line(&format!("br label %{joined}"));
                self.start_block(&joined);
                let code = self.temp();
                self.line(&format!("{code} = phi i64 [ {wide}, %{quick} ], [ {counted}, %{slow_end} ]"));
                self.assign(dest.ok_or("no result")?, &code)
            }
            "str.contains" => {
                let (text, part) = (first.ok_or("no argument")?, args.get(1).ok_or("no argument")?);
                let found = match (self.operand(text)?, self.operand(part)?) {
                    ((text, Kind::Str), (part, Kind::Str)) if self.headers() => {
                        self.headers_contain(&part, &text)
                    }
                    _ => {
                        let text = self.block_operand(text, made)?;
                        let part = self.block_operand(part, made)?;
                        self.string_binary(BinOp::In, (&part, &text))?
                    }
                };
                self.assign(dest.ok_or("no result")?, &found)
            }
            "str.join" => {
                let separator = self.block_operand(first.ok_or("no argument")?, made)?;
                let (list, kind) = self.operand(args.get(1).ok_or("no argument")?)?;
                // Pieces kept as the string they're joined into are that string.
                if kind == Kind::Str {
                    self.retain(&list);
                    return self.assign(dest.ok_or("no result")?, &list);
                }
                if kind == Kind::TextBlock {
                    let header = self.text_from_borrowed_block(&list);
                    return self.assign(dest.ok_or("no result")?, &header);
                }
                self.declare("declare ptr @fennec_str_join(ptr, ptr) nounwind");
                let temp = self.temp();
                self.line(&format!("{temp} = call ptr @fennec_str_join(ptr {separator}, ptr {list})"));
                let temp = self.text_value(&temp);
                self.assign(dest.ok_or("no result")?, &temp)
            }
            // The runtime's own code: a new block owned once.
            "str.upper" | "str.lower" | "str.replace" => {
                let mut texts = Vec::with_capacity(args.len());
                for arg in args {
                    texts.push(format!("ptr {}", self.block_operand(arg, made)?));
                }
                let symbol = name.replace('.', "_");
                let params = vec!["ptr"; texts.len()].join(", ");
                self.declare(&format!("declare ptr @fennec_{symbol}({params}) nounwind"));
                let new = self.temp();
                self.line(&format!("{new} = call ptr @fennec_{symbol}({})", texts.join(", ")));
                let new = self.text_value(&new);
                self.assign(dest.ok_or("no result")?, &new)
            }
            "str.find" => {
                let at = self.temp();
                if self.headers() {
                    // Both go as their headers' words, so neither is made a block first.
                    let (text, _) = self.operand(first.ok_or("no argument")?)?;
                    let (part, _) = self.operand(args.get(1).ok_or("no argument")?)?;
                    let [t0, t1, t2] = [0, 1, 2].map(|at| self.header_word(&text, at));
                    let [p0, p1, p2] = [0, 1, 2].map(|at| self.header_word(&part, at));
                    self.declare(
                        "declare i64 @fennec_str_find_header(i64, i64, i64, i64, i64, i64) nounwind",
                    );
                    self.line(&format!(
                        "{at} = call i64 @fennec_str_find_header(i64 {t0}, i64 {t1}, i64 {t2}, i64 {p0}, i64 {p1}, i64 {p2})"
                    ));
                } else {
                    let text = self.block_operand(first.ok_or("no argument")?, made)?;
                    let part = self.block_operand(args.get(1).ok_or("no argument")?, made)?;
                    self.declare("declare i64 @fennec_str_find(ptr, ptr) nounwind");
                    self.line(&format!("{at} = call i64 @fennec_str_find(ptr {text}, ptr {part})"));
                }
                // -1 when it isn't there, which is `None`.
                let dest = dest.ok_or("no result")?;
                let kind = self.kind_of(dest)?.clone();
                let ((some, fields), (none, _)) =
                    (self.case_of(&kind, "Some")?, self.case_of(&kind, "None")?);
                let offset = fields.first().map(|(_, offset, _)| *offset).ok_or("no field in `Some`")?;
                let (found, number, value) = (self.temp(), self.temp(), self.temp());
                self.line(&format!("{found} = icmp sge i64 {at}, 0"));
                self.line(&format!("{number} = select i1 {found}, i64 {some}, i64 {none}"));
                self.line(&format!("{value} = select i1 {found}, i64 {at}, i64 0"));
                let (slot, memory) = (self.slot(dest), kind.memory(self.module.layouts));
                self.line(&format!("store {memory} zeroinitializer, ptr {slot}"));
                self.line(&format!("store i64 {number}, ptr {slot}"));
                let place = self.offset(&slot, offset);
                self.line(&format!("store i64 {value}, ptr {place}"));
                Ok(())
            }
            // With no separator, on runs of spaces. An empty one fails. Headers go to the runtime
            // as their words, so neither is made a block first; three zeros, which no header is,
            // say there's no separator.
            "str.split" if self.headers() => {
                let (text, _) = self.operand(first.ok_or("no argument")?)?;
                let [t0, t1, t2] = [0, 1, 2].map(|at| self.header_word(&text, at));
                let [s0, s1, s2] = match args.get(1) {
                    Some(separator) => {
                        let (separator, _) = self.operand(separator)?;
                        [0, 1, 2].map(|at| self.header_word(&separator, at))
                    }
                    None => ["0".to_owned(), "0".to_owned(), "0".to_owned()],
                };
                self.declare(
                    "declare ptr @fennec_str_split_header(i64, i64, i64, i64, i64, i64, i64, i64) nounwind",
                );
                let (from, to) = self.place_in_source(span);
                let parts = self.temp();
                self.line(&format!(
                    "{parts} = call ptr @fennec_str_split_header(i64 {t0}, i64 {t1}, i64 {t2}, i64 {s0}, i64 {s1}, i64 {s2}, i64 {from}, i64 {to})"
                ));
                self.leave_if_failed();
                self.assign(dest.ok_or("no result")?, &parts)
            }
            "str.split" => {
                let text = self.block_operand(first.ok_or("no argument")?, made)?;
                let separator = match args.get(1) {
                    Some(separator) => self.block_operand(separator, made)?,
                    None => "null".to_owned(),
                };
                self.declare("declare ptr @fennec_str_split(ptr, ptr, i64, i64) nounwind");
                let (from, to) = self.place_in_source(span);
                let parts = self.temp();
                self.line(&format!(
                    "{parts} = call ptr @fennec_str_split(ptr {text}, ptr {separator}, i64 {from}, i64 {to})"
                ));
                self.leave_if_failed();
                self.assign(dest.ok_or("no result")?, &parts)
            }
            other => Err(format!("it calls `{other}`, which this backend doesn't take yet")),
        }
    }
}

/// One piece of an f-string, read where it is: where its bytes start, how many there are, how
/// many characters they make, and what was made for it to let go of once it's written.
struct Piece {
    from: String,
    size: String,
    chars: String,
    owned: Option<Owned>,
}

/// What an f-string's piece made to be read from.
enum Owned {
    Header(String),
    Block(String),
}
