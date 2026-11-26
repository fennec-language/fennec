//! The calls `std.net` makes on a served connection for each request, through the runtime's typed
//! exports, `fennec_conn_*` (`fennec_rt::embed::conn_calls`): their arguments go as words and what
//! they give back comes as words or blocks, where `fennec_net_call` is handed the method's name
//! and its arguments as values, and gives back a value.
//!
//! They're made in every build, a test's included: the exports answer a test's fake network
//! themselves, so what a test runs is what a server runs. `FENNEC_NAMED_CONN_CALLS=1` has a build
//! call them by name instead, as before, for a test that compares the two: a dev compiler only.
//!
//! - **A failure** comes back as `-1 - n`, the temporary `n` holding the call's `Err`, which is
//!   taken from there as a named call's is.
//! - **A stop**, a bug or a replay that doesn't fit, sets the failure flag and gives back 0, so the
//!   flag is looked at before the answer is.

use fennec_ir::{LocalId, Operand};
use fennec_layout::ListItem;
use fennec_syntax::Span;

use crate::emit::{Emit, FIELD_ACCESS};
use crate::kinds::Kind;
use crate::lists::FIELDS_OFFSET;

/// What the exports are given for `timeout` on a call written without one: it waits as long as it
/// takes. The runtime's `conn_calls::NO_TIMEOUT`.
const NO_TIMEOUT: i64 = i64::MIN;

/// `Head`'s cases, by the number `fennec_conn_read_head` gives back for each.
const HEAD_CASES: [&str; 5] = ["Lines", "Ended", "Unended", "LongFirst", "TooBig"];

/// Whether a build calls a connection's per-request methods by name, as `FENNEC_NAMED_CONN_CALLS=1`
/// asks, rather than through their typed exports: only in a dev compiler, for the test that compares
/// the two. It changes what a build makes, so a build's cache key says it.
#[must_use]
pub(crate) fn named_conn_calls() -> bool {
    cfg!(debug_assertions) && std::env::var("FENNEC_NAMED_CONN_CALLS").as_deref() == Ok("1")
}

impl Emit<'_, '_, '_> {
    /// A connection's method `name` through its typed export, when it has one: says whether it
    /// did. `handle` is the connection's, and `rest` the method's arguments, `timeout=` last when
    /// `name` ends in `.timeout`.
    pub(crate) fn typed_conn_call(
        &mut self,
        dest: Option<LocalId>,
        handle: &str,
        name: &str,
        rest: &[Operand],
        span: Span,
    ) -> Result<bool, String> {
        if named_conn_calls() {
            return Ok(false);
        }
        let (method, timed) = name.strip_suffix(".timeout").map_or((name, false), |method| (method, true));
        let (given, timeout) = if timed {
            let (timeout, given) = rest.split_last().ok_or("a timeout with nothing to time")?;
            (given, Some(timeout))
        } else {
            (rest, None)
        };
        match (method, given) {
            ("Connection._reads_within", [after]) if timeout.is_none() => {
                let after = self.nanos_of(after)?;
                self.declare("declare i64 @fennec_conn_reads_within(i64, i64) nounwind");
                let word = self.temp();
                self.line(&format!("{word} = call i64 @fennec_conn_reads_within(i64 {handle}, i64 {after})"));
                self.leave_if_failed();
            }
            ("Connection.draining", []) if timeout.is_none() => {
                self.declare("declare i64 @fennec_conn_draining(i64) nounwind");
                let word = self.temp();
                self.line(&format!("{word} = call i64 @fennec_conn_draining(i64 {handle})"));
                self.leave_if_failed();
                self.yes_into(dest, &word)?;
            }
            ("Connection._room_for_body", [size]) if timeout.is_none() => {
                let (size, _) = self.operand(size)?;
                let (from, to) = self.place_in_source(span);
                self.declare("declare i64 @fennec_conn_room_for_body(i64, i64, i64, i64) nounwind");
                let word = self.temp();
                self.line(&format!(
                    "{word} = call i64 @fennec_conn_room_for_body(i64 {handle}, i64 {size}, i64 {from}, i64 {to})"
                ));
                self.leave_if_failed();
                self.yes_into(dest, &word)?;
            }
            ("Connection.write", [data]) => {
                let timeout = self.timeout_of(timeout)?;
                let (data, made) = self.data_block(data)?;
                let (from, to) = self.place_in_source(span);
                let mark = self.mark();
                self.declare("declare i64 @fennec_conn_write(i64, i64, i64, i64, i64) nounwind");
                let word = self.temp();
                self.line(&format!(
                    "{word} = call i64 @fennec_conn_write(i64 {handle}, i64 {data}, i64 {timeout}, i64 {from}, i64 {to})"
                ));
                if let Some(made) = made {
                    self.release_block(&made, &Kind::Str);
                }
                self.leave_if_failed();
                self.result_into(dest, &word, &mark, span, |_, _, _, _| Ok(()))?;
            }
            // A response's head and body in one write, with no block made for the two joined.
            ("Connection._write_parts", [head, body]) => {
                let timeout = self.timeout_of(timeout)?;
                let (head, made) = self.data_block(head)?;
                let (body, _) = self.data_block(body)?;
                let (from, to) = self.place_in_source(span);
                let mark = self.mark();
                self.declare("declare i64 @fennec_conn_write_parts(i64, i64, i64, i64, i64, i64) nounwind");
                let word = self.temp();
                self.line(&format!(
                    "{word} = call i64 @fennec_conn_write_parts(i64 {handle}, i64 {head}, i64 {body}, i64 {timeout}, i64 {from}, i64 {to})"
                ));
                if let Some(made) = made {
                    self.release_block(&made, &Kind::Str);
                }
                self.leave_if_failed();
                self.result_into(dest, &word, &mark, span, |_, _, _, _| Ok(()))?;
            }
            ("Connection._body", [size]) => {
                let timeout = self.timeout_of(timeout)?;
                let (size, _) = self.operand(size)?;
                let (from, to) = self.place_in_source(span);
                let mark = self.mark();
                self.declare("declare i64 @fennec_conn_body(i64, i64, i64, i64, i64) nounwind");
                let word = self.temp();
                self.line(&format!(
                    "{word} = call i64 @fennec_conn_body(i64 {handle}, i64 {size}, i64 {timeout}, i64 {from}, i64 {to})"
                ));
                self.leave_if_failed();
                if dest.is_none() {
                    // A body no one takes is let go of: the word is its block whenever it isn't below 0.
                    let (kept, body) = (self.temp(), self.temp());
                    self.line(&format!("{kept} = icmp sge i64 {word}, 0"));
                    self.line(&format!("{body} = inttoptr i64 {word} to ptr"));
                    self.release_if(&kept, &body, &Kind::Bytes);
                }
                self.result_into(dest, &word, &mark, span, |emit, word, at, _| {
                    let body = emit.temp();
                    emit.line(&format!("{body} = inttoptr i64 {word} to ptr"));
                    emit.store_field(&Kind::Bytes, &body, at, "");
                    Ok(())
                })?;
            }
            ("Connection._read_head", [most_bytes, most_lines, most_blank]) => {
                let timeout = self.timeout_of(timeout)?;
                let [most_bytes, most_lines, most_blank] =
                    [most_bytes, most_lines, most_blank].map(|most| self.operand(most).map(|(word, _)| word));
                let (most_bytes, most_lines, most_blank) = (most_bytes?, most_lines?, most_blank?);
                let strings = self.element_number(ListItem::Str);
                let lines = self.place_for(&Kind::Int);
                self.line(&format!("store i64 0, ptr {lines}"));
                let (from, to) = self.place_in_source(span);
                let mark = self.mark();
                self.declare(
                    "declare i64 @fennec_conn_read_head(i64, i64, i64, i64, i64, i64, ptr, i64, i64) nounwind",
                );
                let word = self.temp();
                self.line(&format!(
                    "{word} = call i64 @fennec_conn_read_head(i64 {handle}, i64 {most_bytes}, i64 {most_lines}, \
                     i64 {most_blank}, i64 {timeout}, i64 {strings}, ptr {lines}, i64 {from}, i64 {to})"
                ));
                // A stop gives back 0 too, which is `Lines`, so the flag goes first.
                self.leave_if_failed();
                if dest.is_none() {
                    // `Lines`' list, which only case 0 sets, is let go of when no one takes it.
                    let (kept, list) = (self.temp(), self.temp());
                    self.line(&format!("{kept} = icmp eq i64 {word}, 0"));
                    self.line(&format!("{list} = load ptr, ptr {lines}"));
                    self.release_if(&kept, &list, &Kind::List(ListItem::Str));
                }
                self.result_into(dest, &word, &mark, span, |emit, word, at, head| {
                    emit.head_at(word, at, head, &lines)
                })?;
            }
            _ => return Ok(false),
        }
        Ok(true)
    }

    /// Lets go of `block`, a `kind`, when `kept` holds.
    fn release_if(&mut self, kept: &str, block: &str, kind: &Kind) {
        let (release, after) = (self.label("conn_drop"), self.label("conn_dropped"));
        self.line(&format!("br i1 {kept}, label %{release}, label %{after}"));
        self.start_block(&release);
        self.release_block(block, kind);
        self.line(&format!("br label %{after}"));
        self.start_block(&after);
    }

    /// The nanoseconds of a `Duration`.
    fn nanos_of(&mut self, duration: &Operand) -> Result<String, String> {
        let (base, kind) = self.operand(duration)?;
        let (offset, held) = self.field(&kind, "nanos")?;
        let fields = self.fields_at(&base, &kind);
        let at = self.offset(&fields, offset);
        let access = if matches!(kind, Kind::Boxed(_)) { FIELD_ACCESS } else { "" };
        Ok(self.load_field(&held, &at, access))
    }

    /// What an export is given for `timeout`: the nanoseconds of the one given, or `NO_TIMEOUT`.
    fn timeout_of(&mut self, timeout: Option<&Operand>) -> Result<String, String> {
        timeout.map_or_else(|| Ok(NO_TIMEOUT.to_string()), |timeout| self.nanos_of(timeout))
    }

    /// The block of a `str` or `bytes` a write sends, as a word, and the block made for it to let
    /// go of after, when a string held as a header has none of its own.
    fn data_block(&mut self, data: &Operand) -> Result<(String, Option<String>), String> {
        let (value, kind) = self.operand(data)?;
        let (block, made) = match kind {
            Kind::Str if self.headers() => {
                let block = self.owned_text_block(&value);
                (block.clone(), Some(block))
            }
            Kind::Str | Kind::Bytes => (value, None),
            other => return Err(format!("it writes a {other:?} to a connection")),
        };
        let word = self.temp();
        self.line(&format!("{word} = ptrtoint ptr {block} to i64"));
        Ok((word, made))
    }

    /// Where the temporaries start, to go back to once a failure's been taken.
    fn mark(&mut self) -> String {
        self.declare("declare i64 @fennec_mark() nounwind");
        let mark = self.temp();
        self.line(&format!("{mark} = call i64 @fennec_mark()"));
        mark
    }

    /// `dest`, a `bool`, as whether `word` isn't 0.
    fn yes_into(&mut self, dest: Option<LocalId>, word: &str) -> Result<(), String> {
        let Some(dest) = dest else { return Ok(()) };
        let yes = self.temp();
        self.line(&format!("{yes} = icmp ne i64 {word}, 0"));
        self.assign(dest, &yes)
    }

    /// `dest`, a result, from an export's `word`: its `Err`, from the temporary a negative word
    /// names, or else `Ok`, whose value `ok` writes at where it goes from the word. Then the
    /// temporaries go back to `mark`.
    fn result_into(
        &mut self,
        dest: Option<LocalId>,
        word: &str,
        mark: &str,
        span: Span,
        ok: impl FnOnce(&mut Self, &str, &str, &Kind) -> Result<(), String>,
    ) -> Result<(), String> {
        self.declare("declare void @fennec_truncate(i64) nounwind");
        let Some(dest) = dest else {
            self.line(&format!("call void @fennec_truncate(i64 {mark})"));
            return Ok(());
        };
        let failed = self.temp();
        self.line(&format!("{failed} = icmp slt i64 {word}, 0"));
        let (err, gave, done) =
            (self.label("conn_failed"), self.label("conn_gave"), self.label("conn_taken"));
        self.line(&format!("br i1 {failed}, label %{err}, label %{gave}"));
        // A failure is taken from its temporary as a named call's is: rarely, so `NetError` is
        // made in one place.
        self.start_block(&err);
        let temporary = self.temp();
        self.line(&format!("{temporary} = sub i64 -1, {word}"));
        if let Some(block) = self.result_from(dest, &temporary, span)? {
            self.assign(dest, &block)?;
        }
        self.line(&format!("br label %{done}"));
        self.start_block(&gave);
        let kind = self.kind_of(dest)?.clone();
        let block = if let Kind::Boxed(id) = kind { Some(self.new_block(id)?) } else { None };
        let base = if let Some(block) = &block {
            self.offset(block, FIELDS_OFFSET)
        } else {
            self.release_held(dest)?;
            let slot = self.slot(dest);
            let zero = self.zero_line(&kind, &slot);
            self.line(&zero);
            slot
        };
        let (number, fields) = self.case_of(&kind, "Ok")?;
        self.line(&format!("store i64 {number}, ptr {base}"));
        if let Some((_, offset, Some(held))) = fields.first().cloned()
            && held != Kind::Unit
        {
            let at = self.offset(&base, offset);
            ok(self, word, &at, &held)?;
        }
        if let Some(block) = block {
            self.assign(dest, &block)?;
        }
        self.line(&format!("br label %{done}"));
        self.start_block(&done);
        self.line(&format!("call void @fennec_truncate(i64 {mark})"));
        Ok(())
    }

    /// Writes the `Head` whose case `fennec_conn_read_head` gave back as `word` at `at`, where a
    /// result holds it: for `Lines`, the list it set at `lines`.
    fn head_at(&mut self, word: &str, at: &str, head: &Kind, lines: &str) -> Result<(), String> {
        let (block, base) = match *head {
            Kind::Boxed(id) => {
                let block = self.new_block(id)?;
                let base = self.offset(&block, FIELDS_OFFSET);
                (Some(block), base)
            }
            _ => (None, at.to_owned()),
        };
        // The case's number in this build's layout, by its name, from the export's.
        let mut number = "0".to_owned();
        for (given, name) in (0_i64..).zip(HEAD_CASES) {
            let (case, _) = self.case_of(head, name)?;
            let (same, picked) = (self.temp(), self.temp());
            self.line(&format!("{same} = icmp eq i64 {word}, {given}"));
            self.line(&format!("{picked} = select i1 {same}, i64 {case}, i64 {number}"));
            number = picked;
        }
        self.line(&format!("store i64 {number}, ptr {base}"));
        let (_, fields) = self.case_of(head, "Lines")?;
        let (_, offset, _) = fields.first().cloned().ok_or("`Lines` with no lines")?;
        // Only `Lines` has a list: every other case leaves `lines` as 0, and the slot as it was.
        let (is_lines, list, place) = (self.temp(), self.temp(), self.offset(&base, offset));
        self.line(&format!("{is_lines} = icmp eq i64 {word}, 0"));
        let (set, after) = (self.label("head_lines"), self.label("head_made"));
        self.line(&format!("br i1 {is_lines}, label %{set}, label %{after}"));
        self.start_block(&set);
        self.line(&format!("{list} = load ptr, ptr {lines}"));
        self.line(&format!("store ptr {list}, ptr {place}"));
        self.line(&format!("br label %{after}"));
        self.start_block(&after);
        if let Some(block) = block {
            self.line(&format!("store ptr {block}, ptr {at}"));
        }
        Ok(())
    }
}
