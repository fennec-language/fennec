//! An error's notes: a word of its own, last in an error's layout, 0 until a note is added and a
//! list of strings after. The runtime's calls take and give that word, so where it is stays native
//! code's to know: in place in an error held in place, in its block in a boxed one, and in a union
//! of errors, in whichever member it holds.

use fennec_ir::{LocalId, Operand};

use crate::emit::Emit;
use crate::kinds::Kind;
use crate::lists::FIELDS_OFFSET;

impl Emit<'_, '_, '_> {
    /// `error.add_note(error, text)`, which changes the error, and `error.notes(error)`.
    pub(crate) fn notes_builtin(
        &mut self,
        dest: Option<LocalId>,
        name: &str,
        args: &[Operand],
    ) -> Result<(), String> {
        match (name, args) {
            ("error.add_note", [Operand::Local(error), text]) => {
                let kind = self.kind_of(*error)?.clone();
                let slot = self.slot(*error);
                let at = self.notes_in(&slot, &kind, true)?.ok_or("a note on something without notes")?;
                let (value, text_kind) = self.operand(text)?;
                // The runtime takes over the text's block.
                let block = if text_kind == Kind::Str && self.headers() {
                    self.owned_text_block(&value)
                } else {
                    self.retain(&value);
                    value
                };
                let (old, word, new) = (self.temp(), self.temp(), self.temp());
                self.line(&format!("{old} = load i64, ptr {at}"));
                self.line(&format!("{word} = ptrtoint ptr {block} to i64"));
                self.declare("declare i64 @fennec_notes_add(i64, i64) nounwind");
                self.line(&format!("{new} = call i64 @fennec_notes_add(i64 {old}, i64 {word})"));
                self.line(&format!("store i64 {new}, ptr {at}"));
                Ok(())
            }
            ("error.notes", [error]) => {
                let word = self.notes_word(error)?.ok_or("the notes of something without them")?;
                self.declare("declare i64 @fennec_notes_list(i64) nounwind");
                let (got, list) = (self.temp(), self.temp());
                self.line(&format!("{got} = call i64 @fennec_notes_list(i64 {word})"));
                self.line(&format!("{list} = inttoptr i64 {got} to ptr"));
                self.assign(dest.ok_or("notes no one keeps")?, &list)
            }
            _ => Err(format!("it calls `{name}` in a way this backend doesn't take yet")),
        }
    }

    /// The notes word of an error, read where it is: `None` when it isn't an error.
    pub(crate) fn notes_word(&mut self, error: &Operand) -> Result<Option<String>, String> {
        let (value, kind) = self.operand(error)?;
        // A boxed one's block is what's held, so it's held somewhere to be read through.
        let place = if matches!(kind, Kind::Boxed(_)) { self.temp_slot(&value) } else { value };
        self.notes_word_at(&place, &kind)
    }

    /// The notes word of the error held at `place`, as [`Self::notes_in`] finds it.
    pub(crate) fn notes_word_at(&mut self, place: &str, kind: &Kind) -> Result<Option<String>, String> {
        let Some(at) = self.notes_in(place, kind, false)? else { return Ok(None) };
        let word = self.temp();
        self.line(&format!("{word} = load i64, ptr {at}"));
        Ok(Some(word))
    }

    /// `text` with the notes under it of the error held at `place`, when it's an error: a new
    /// string, `text` let go of.
    pub(crate) fn with_notes_under(
        &mut self,
        text: &str,
        place: &str,
        kind: &Kind,
    ) -> Result<String, String> {
        let Some(word) = self.notes_word_at(place, kind)? else { return Ok(text.to_owned()) };
        self.declare("declare ptr @fennec_notes_shown(i64) nounwind");
        self.declare("declare ptr @fennec_str_concat(ptr, ptr) nounwind");
        let (notes, joined) = (self.temp(), self.temp());
        self.line(&format!("{notes} = call ptr @fennec_notes_shown(i64 {word})"));
        self.line(&format!("{joined} = call ptr @fennec_str_concat(ptr {text}, ptr {notes})"));
        self.release(text);
        self.release(&notes);
        Ok(joined)
    }

    /// Where the notes word is of the error held at `place`: the value itself when it's in place,
    /// and the address of its block when boxed, which is made its own first when it's to change.
    /// `None` when it has no notes.
    fn notes_in(&mut self, place: &str, kind: &Kind, change: bool) -> Result<Option<String>, String> {
        let (Kind::Struct(id) | Kind::Enum(id) | Kind::Boxed(id)) = *kind else { return Ok(None) };
        let layout = self.layout_of(id)?;
        let notes = layout.fields.iter().find(|field| field.name == fennec_rt::packed::NOTES);
        let notes = notes.map(|field| field.offset);
        let union = layout.cases.is_some();
        if notes.is_none() && !(union && self.is_union_of_errors(id)) {
            return Ok(None);
        }
        let base = if matches!(kind, Kind::Boxed(_)) {
            let block = if change {
                self.alone_at(place, "fennec_unshare")
            } else {
                let block = self.temp();
                self.line(&format!("{block} = load ptr, ptr {place}"));
                block
            };
            self.offset(&block, FIELDS_OFFSET)
        } else {
            place.to_owned()
        };
        match notes {
            Some(offset) => Ok(Some(self.offset(&base, offset))),
            None => self.member_notes(&base, id, change).map(Some),
        }
    }

    /// Whether every case of an enum holds one error with notes, as a union of errors does.
    fn is_union_of_errors(&self, id: u32) -> bool {
        let layouts = self.module.layouts;
        let Some(cases) = layouts.get(id).and_then(|layout| layout.cases.as_ref()) else { return false };
        cases.iter().all(|case| match case.fields.as_slice() {
            [field] => match Kind::from_ty(layouts, field.ty) {
                Some(Kind::Struct(member) | Kind::Boxed(member)) => {
                    layouts.get(member).is_some_and(|layout| {
                        layout.fields.iter().any(|field| field.name == fennec_rt::packed::NOTES)
                    })
                }
                _ => false,
            },
            _ => false,
        })
    }

    /// In a union of errors at `base`, the notes word of the member it holds, by its case.
    fn member_notes(&mut self, base: &str, id: u32, change: bool) -> Result<String, String> {
        let layouts = self.module.layouts;
        let cases =
            layouts.get(id).and_then(|layout| layout.cases.as_ref()).ok_or("a union with no cases")?;
        let members: Vec<(u32, Kind)> = cases
            .iter()
            .filter_map(|case| case.fields.first())
            .map(|field| Kind::from_ty(layouts, field.ty).map(|kind| (field.offset, kind)))
            .collect::<Option<_>>()
            .ok_or("a union of errors holding something this backend doesn't take yet")?;
        let found = self.place_for(&Kind::Int);
        let (tag, done) = (self.temp(), self.label("notes_found"));
        self.line(&format!("{tag} = load i64, ptr {base}"));
        let labels: Vec<String> = members.iter().map(|_| self.label("notes_of")).collect();
        let arms: Vec<String> = labels
            .iter()
            .enumerate()
            .map(|(number, label)| format!("i64 {number}, label %{label}"))
            .collect();
        self.line(&format!("store i64 0, ptr {found}"));
        self.line(&format!("switch i64 {tag}, label %{done} [ {} ]", arms.join(" ")));
        for ((offset, member), label) in members.iter().zip(&labels) {
            self.start_block(label);
            let held = self.offset(base, *offset);
            let at =
                self.notes_in(&held, member, change)?.ok_or("a member of a union of errors without notes")?;
            let word = self.temp();
            self.line(&format!("{word} = ptrtoint ptr {at} to i64"));
            self.line(&format!("store i64 {word}, ptr {found}"));
            self.line(&format!("br label %{done}"));
        }
        self.start_block(&done);
        let (word, at) = (self.temp(), self.temp());
        self.line(&format!("{word} = load i64, ptr {found}"));
        self.line(&format!("{at} = inttoptr i64 {word} to ptr"));
        Ok(at)
    }
}
