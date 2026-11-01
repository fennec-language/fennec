//! Regular expressions, by the runtime's calls on strings' blocks. What a pattern finds comes
//! back as a list of ints, each match's start and end in characters and then its groups', so
//! the `Match` values are built here, laid out as the program's `Match` is.

use fennec_ir::{LocalId, Operand};
use fennec_layout::ListItem;
use fennec_syntax::Span;

use crate::emit::{Emit, ITEM_ACCESS};
use crate::kinds::{Kind, element_symbol};
use crate::lists::list_item;

/// Where a block's fields start.
const FIELDS: u32 = fennec_interp::embed::layout::FIELDS.unsigned_abs();

impl Emit<'_, '_, '_> {
    /// A list kind's number, for the runtime to make lists of it.
    fn element_number(&mut self, item: ListItem) -> String {
        let symbol = element_symbol(self.module.layouts, item);
        self.module.shared.elements.insert(symbol.clone(), item);
        self.declare(&format!("@\"{symbol}\" = external dso_local global i64"));
        let number = self.temp();
        self.line(&format!("{number} = load i64, ptr @\"{symbol}\", !invariant.load !1"));
        number
    }

    /// A `Regex`'s pattern, borrowed for a call.
    fn pattern(&mut self, regex: &Operand) -> Result<String, String> {
        let (block, kind) = self.operand(regex)?;
        let (offset, _) = self.field(&kind, "pattern")?;
        let fields = self.fields_at(&block, &kind);
        let at = self.offset(&fields, offset);
        let pattern = self.temp();
        self.line(&format!("{pattern} = load ptr, ptr {at}"));
        Ok(pattern)
    }

    pub(crate) fn regex_builtin(
        &mut self,
        dest: Option<LocalId>,
        name: &str,
        args: &[Operand],
        span: Span,
    ) -> Result<(), String> {
        let dest = dest.ok_or("no result")?;
        let arg = |at: usize| args.get(at).ok_or_else(|| "no argument".to_owned());
        let (from, to) = (i64::from(span.start), i64::from(span.end));
        if name == "is_regex" || name == "re.is_regex" {
            let (pattern, _) = self.operand(arg(0)?)?;
            self.declare("declare i8 @fennec_regex_is_valid(ptr) nounwind");
            let (valid, bit) = (self.temp(), self.temp());
            self.line(&format!("{valid} = call i8 @fennec_regex_is_valid(ptr {pattern})"));
            self.line(&format!("{bit} = icmp ne i8 {valid}, 0"));
            return self.assign(dest, &bit);
        }
        let pattern = self.pattern(arg(0)?)?;
        let (whole, _) = self.operand(arg(1)?)?;
        match name {
            "Regex.replace" => {
                let (with, _) = self.operand(arg(2)?)?;
                self.declare("declare ptr @fennec_regex_replace(ptr, ptr, ptr, i64, i64) nounwind");
                let made = self.temp();
                self.line(&format!(
                    "{made} = call ptr @fennec_regex_replace(ptr {pattern}, ptr {whole}, ptr {with}, i64 {from}, i64 {to})"
                ));
                self.leave_if_failed();
                self.assign(dest, &made)
            }
            "Regex.split" => {
                let strings = self.element_number(ListItem::Str);
                self.declare("declare ptr @fennec_regex_split(ptr, ptr, i64, i64, i64) nounwind");
                let pieces = self.temp();
                self.line(&format!(
                    "{pieces} = call ptr @fennec_regex_split(ptr {pattern}, ptr {whole}, i64 {strings}, i64 {from}, i64 {to})"
                ));
                self.leave_if_failed();
                self.assign(dest, &pieces)
            }
            "Regex.find_all" => {
                let ints = self.element_number(ListItem::Int);
                self.declare("declare ptr @fennec_regex_find_all(ptr, ptr, i64, i64, i64) nounwind");
                let places = self.temp();
                self.line(&format!(
                    "{places} = call ptr @fennec_regex_find_all(ptr {pattern}, ptr {whole}, i64 {ints}, i64 {from}, i64 {to})"
                ));
                self.leave_if_failed();
                let groups = self.group_count(&pattern);
                let (per, per_match, count) = (self.temp(), self.temp(), self.temp());
                self.line(&format!("{per} = shl i64 {groups}, 1"));
                self.line(&format!("{per_match} = add i64 {per}, 2"));
                let length = self.list_length(&places);
                self.line(&format!("{count} = udiv i64 {length}, {per_match}"));
                let kind = self.kind_of(dest)?.clone();
                let item = list_item(&kind)?;
                let held = self.item_kind(item)?;
                let found = self.new_list(item, &count);
                let memory = held.memory(self.module.layouts);
                let at_match = self.count_loop(&count, |emit, number| {
                    let base = emit.temp();
                    emit.line(&format!("{base} = mul i64 {number}, {per_match}"));
                    let made = emit.build_match(&whole, &places, &base, &groups, span)?;
                    let at = emit.item_address(&found, &memory, number);
                    emit.line(&format!("store ptr {made}, ptr {at}{ITEM_ACCESS}"));
                    Ok(())
                });
                at_match?;
                self.release(&places);
                self.assign(dest, &found)
            }
            "Regex.search" | "Regex.match" | "Regex.fullmatch" => {
                let anchor = match name {
                    "Regex.match" => 1,
                    "Regex.fullmatch" => 2,
                    _ => 0,
                };
                let ints = self.element_number(ListItem::Int);
                self.declare("declare ptr @fennec_regex_find(ptr, ptr, i64, i64, i64, i64) nounwind");
                let places = self.temp();
                self.line(&format!(
                    "{places} = call ptr @fennec_regex_find(ptr {pattern}, ptr {whole}, i64 {anchor}, i64 {ints}, i64 {from}, i64 {to})"
                ));
                self.leave_if_failed();
                let groups = self.group_count(&pattern);
                self.optional_match(dest, &whole, &places, &groups, span)?;
                self.release(&places);
                Ok(())
            }
            other => Err(format!("it calls `{other}`, which this backend doesn't take yet")),
        }
    }

    fn group_count(&mut self, pattern: &str) -> String {
        self.declare("declare i64 @fennec_regex_groups(ptr) nounwind");
        let groups = self.temp();
        self.line(&format!("{groups} = call i64 @fennec_regex_groups(ptr {pattern})"));
        groups
    }

    /// Runs `body` with each number from 0 up to `count`, as a loop.
    fn count_loop(
        &mut self,
        count: &str,
        body: impl FnOnce(&mut Self, &str) -> Result<(), String>,
    ) -> Result<(), String> {
        let before = self.current.clone();
        let (head, inside, latch, done) =
            (self.label("each"), self.label("body"), self.label("next"), self.label("done"));
        let (number, next, more) = (self.temp(), self.temp(), self.temp());
        self.line(&format!("br label %{head}"));
        self.start_block(&head);
        self.line(&format!("{number} = phi i64 [ 0, %{before} ], [ {next}, %{latch} ]"));
        self.line(&format!("{more} = icmp slt i64 {number}, {count}"));
        self.line(&format!("br i1 {more}, label %{inside}, label %{done}"));
        self.start_block(&inside);
        body(self, &number)?;
        self.line(&format!("br label %{latch}"));
        self.start_block(&latch);
        self.line(&format!("{next} = add i64 {number}, 1"));
        self.line(&format!("br label %{head}"));
        self.start_block(&done);
        Ok(())
    }

    /// The int at `position` in a list of ints.
    fn int_at(&mut self, list: &str, position: &str) -> String {
        let at = self.item_address(list, "i64", position);
        let value = self.temp();
        self.line(&format!("{value} = load i64, ptr {at}{ITEM_ACCESS}"));
        value
    }

    /// The characters from `start` to `end` of a string, as a new one.
    fn chars_between(&mut self, whole: &str, start: &str, end: &str, span: Span) -> String {
        self.declare("declare ptr @fennec_str_slice(ptr, i64, i64, i64, i64) nounwind");
        let (from, to) = (i64::from(span.start), i64::from(span.end));
        let part = self.temp();
        self.line(&format!(
            "{part} = call ptr @fennec_str_slice(ptr {whole}, i64 {start}, i64 {end}, i64 {from}, i64 {to})"
        ));
        part
    }

    /// A new `Match` block, for the match whose numbers start at `base`.
    fn build_match(
        &mut self,
        whole: &str,
        places: &str,
        base: &str,
        groups: &str,
        span: Span,
    ) -> Result<String, String> {
        let id = self.module.layouts.id("Match").ok_or("there's no `Match` laid out")?;
        let kind = Kind::Boxed(id);
        let (text_at, _) = self.field(&kind, "text")?;
        let (start_at, _) = self.field(&kind, "start")?;
        let (end_at, _) = self.field(&kind, "end")?;
        let (groups_at, groups_kind) = self.field(&kind, "groups")?;
        let start = self.int_at(places, base);
        let after = self.temp();
        self.line(&format!("{after} = add i64 {base}, 1"));
        let end = self.int_at(places, &after);
        let text = self.chars_between(whole, &start, &end, span);
        let item = list_item(&groups_kind)?;
        let held = self.item_kind(item)?;
        let (Kind::Enum(_) | Kind::Boxed(_)) = held else {
            return Err(format!("a `Match`'s groups are {held:?}, which this backend doesn't take yet"));
        };
        let caught = self.new_list(item, groups);
        let memory = held.memory(self.module.layouts);
        self.count_loop(groups, |emit, group| {
            let (twice, first) = (emit.temp(), emit.temp());
            emit.line(&format!("{twice} = shl i64 {group}, 1"));
            emit.line(&format!("{first} = add i64 {base}, {twice}"));
            let (start_place, end_place) = (emit.temp(), emit.temp());
            emit.line(&format!("{start_place} = add i64 {first}, 2"));
            emit.line(&format!("{end_place} = add i64 {first}, 3"));
            let group_start = emit.int_at(places, &start_place);
            let group_end = emit.int_at(places, &end_place);
            let at = emit.item_address(&caught, &memory, group);
            let (took_part, some, none, joined) =
                (emit.temp(), emit.label("some"), emit.label("none"), emit.label("joined"));
            emit.line(&format!("{took_part} = icmp sge i64 {group_start}, 0"));
            emit.line(&format!("br i1 {took_part}, label %{some}, label %{none}"));
            emit.start_block(&some);
            let part = emit.chars_between(whole, &group_start, &group_end, span);
            emit.write_option(&held, Some(&part), &at)?;
            emit.line(&format!("br label %{joined}"));
            emit.start_block(&none);
            emit.write_option(&held, None, &at)?;
            emit.line(&format!("br label %{joined}"));
            emit.start_block(&joined);
            Ok(())
        })?;
        let made = self.new_block(id)?;
        let fields = self.offset(&made, FIELDS);
        for (offset, value, text) in [
            (text_at, &text, "ptr"),
            (start_at, &start, "i64"),
            (end_at, &end, "i64"),
            (groups_at, &caught, "ptr"),
        ] {
            let at = self.offset(&fields, offset);
            self.line(&format!("store {text} {value}, ptr {at}"));
        }
        Ok(made)
    }

    /// Writes an optional block where nothing was: `Some` of an owned block, or `None`.
    fn write_option(&mut self, kind: &Kind, value: Option<&str>, at: &str) -> Result<(), String> {
        if let Kind::Maybe(_) = kind {
            self.line(&format!("store ptr {}, ptr {at}", value.unwrap_or("null")));
            return Ok(());
        }
        let case = if value.is_some() { "Some" } else { "None" };
        let (number, fields) = self.case_of(kind, case)?;
        let base = if let Kind::Boxed(id) = kind {
            let block = self.new_block(*id)?;
            self.line(&format!("store ptr {block}, ptr {at}"));
            self.offset(&block, FIELDS)
        } else {
            // Words a case doesn't use are zero, so equal optionals have equal words.
            let text = kind.memory(self.module.layouts);
            self.line(&format!("store {text} zeroinitializer, ptr {at}"));
            at.to_owned()
        };
        self.line(&format!("store i64 {number}, ptr {base}"));
        if let (Some(value), Some((_, offset, _))) = (value, fields.first()) {
            let place = self.offset(&base, *offset);
            self.line(&format!("store ptr {value}, ptr {place}"));
        }
        Ok(())
    }

    /// `Some(Match)` when the runtime found one, otherwise `None`.
    fn optional_match(
        &mut self,
        dest: LocalId,
        whole: &str,
        places: &str,
        groups: &str,
        span: Span,
    ) -> Result<(), String> {
        let kind = self.kind_of(dest)?.clone();
        let length = self.list_length(places);
        let (found, some, none, joined) =
            (self.temp(), self.label("found"), self.label("missed"), self.label("joined"));
        self.line(&format!("{found} = icmp ne i64 {length}, 0"));
        let place = self.place_for(&kind);
        self.line(&format!("br i1 {found}, label %{some}, label %{none}"));
        self.start_block(&some);
        let made = self.build_match(whole, places, "0", groups, span)?;
        self.write_option(&kind, Some(&made), &place)?;
        self.line(&format!("br label %{joined}"));
        self.start_block(&none);
        self.write_option(&kind, None, &place)?;
        self.line(&format!("br label %{joined}"));
        self.start_block(&joined);
        if kind.in_memory() {
            self.assign_memory(dest, &place, "")
        } else {
            let block = self.temp();
            self.line(&format!("{block} = load ptr, ptr {place}"));
            self.assign(dest, &block)
        }
    }
}
