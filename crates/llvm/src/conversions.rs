//! Conversions and small builtins: floats turned into ints, `PI`, `range` and `reversed` as
//! lists, a dict's or set's keys as a list, and new sets.

use fennec_ir::{LocalId, Operand};
use fennec_layout::ListItem;
use fennec_rt::embed::checks::{NOT_AN_INT, ZERO_STEP};
use fennec_syntax::{BinOp, Span};

use crate::emit::{Emit, ITEM_ACCESS, float_text, symbol};
use crate::kinds::Kind;

/// The most items `range` makes a list of.
const MOST_IN_A_RANGE: i64 = 10_000_000;

/// Where `int()` stops: a float at or past 2^63 either way isn't an int.
const INT_LIMIT: f64 = 9_223_372_036_854_775_808.0;

/// What was made to hand the runtime an item, to let go of after.
enum Passed {
    Nothing,
    Block(String),
    /// A string's block, and what says whether it was made for this.
    Text(String, String),
}

impl Emit<'_, '_, '_> {
    /// The builtins here; gives back `None` for any other.
    pub(crate) fn conversion(
        &mut self,
        dest: Option<LocalId>,
        name: &str,
        args: &[Operand],
        span: Span,
    ) -> Option<Result<(), String>> {
        let kind_of = |emit: &Self, operand: &Operand| match operand {
            Operand::Local(local) => emit.kind_of(*local).ok().cloned(),
            Operand::Int(_) => Some(Kind::Int),
            Operand::Float(_) => Some(Kind::Float),
            Operand::Str(_) => Some(Kind::Str),
            _ => None,
        };
        let kinds: Vec<Option<Kind>> = args.iter().map(|arg| kind_of(self, arg)).collect();
        let dest = dest.ok_or_else(|| "no result".to_owned());
        Some(match (name, args, kinds.as_slice()) {
            ("PI" | "math.PI", [], []) => {
                dest.and_then(|dest| self.assign(dest, &float_text(std::f64::consts::PI)))
            }
            ("float.abs", [value], [Some(Kind::Float)]) => dest.and_then(|dest| {
                let (value, _) = self.operand(value)?;
                let absolute = self.float_call("fabs", &value);
                self.assign(dest, &absolute)
            }),
            ("float.is_nan", [value], [Some(Kind::Float)]) => dest.and_then(|dest| {
                let (value, _) = self.operand(value)?;
                let found = self.temp();
                self.line(&format!("{found} = fcmp uno double {value}, {value}"));
                self.assign(dest, &found)
            }),
            ("int", [value], [Some(Kind::Float)]) => {
                dest.and_then(|dest| self.float_to_int(dest, value, None, span))
            }
            ("math.floor", [value], [Some(Kind::Float)]) => {
                dest.and_then(|dest| self.float_to_int(dest, value, Some("floor"), span))
            }
            ("math.ceil", [value], [Some(Kind::Float)]) => {
                dest.and_then(|dest| self.float_to_int(dest, value, Some("ceil"), span))
            }
            ("round", [value], [Some(Kind::Float)]) => {
                dest.and_then(|dest| self.float_to_int(dest, value, Some("roundeven"), span))
            }
            ("range", [_, ..], _) if args.len() <= 3 && kinds.iter().all(|kind| *kind == Some(Kind::Int)) => {
                dest.and_then(|dest| self.range_list(dest, args, span))
            }
            ("reversed", [list], [Some(Kind::List(_))]) => dest.and_then(|dest| self.reversed(dest, list)),
            ("list", [_], [Some(Kind::Cap)]) => dest.and_then(|dest| self.args_list(dest)),
            ("reversed", [text], [Some(Kind::Str)]) => dest.and_then(|dest| {
                let (text, _) = self.operand(text)?;
                self.declare("declare ptr @fennec_str_reversed(ptr) nounwind");
                let turned = self.temp();
                self.on_blocks([&text], |emit, [block]| {
                    emit.line(&format!("{turned} = call ptr @fennec_str_reversed(ptr {block})"));
                });
                let turned = self.text_value(&turned);
                self.assign(dest, &turned)
            }),
            ("reversed", [tuple], [Some(Kind::Struct(_) | Kind::Boxed(_))]) => {
                dest.and_then(|dest| self.reversed_tuple(dest, tuple))
            }
            // A dict's keys, or a set's items, in their order.
            ("list", [dict], [Some(Kind::Dict(..))]) => {
                dest.and_then(|dest| self.dict_list(dest, dict, "keys"))
            }
            ("math.sin", [value], [Some(Kind::Float | Kind::Int)]) => {
                dest.and_then(|dest| self.turned(dest, "fennec_sin", value))
            }
            ("math.cos", [value], [Some(Kind::Float | Kind::Int)]) => {
                dest.and_then(|dest| self.turned(dest, "fennec_cos", value))
            }
            ("math.tan", [value], [Some(Kind::Float | Kind::Int)]) => {
                dest.and_then(|dest| self.turned(dest, "fennec_tan", value))
            }
            ("math.acos", [value], [Some(Kind::Float | Kind::Int)]) => {
                dest.and_then(|dest| self.turned(dest, "fennec_acos", value))
            }
            ("math.exp", [value], [Some(Kind::Float | Kind::Int)]) => {
                dest.and_then(|dest| self.turned(dest, "fennec_exp", value))
            }
            ("math.log", [value], [Some(Kind::Float | Kind::Int)]) => {
                dest.and_then(|dest| self.turned(dest, "fennec_log", value))
            }
            ("int", [value], [Some(Kind::Int)]) | ("float", [value], [Some(Kind::Float)]) => {
                dest.and_then(|dest| self.copy(dest, value))
            }
            // `f32(x)`: the f32 nearest a number.
            ("f32", [value], [Some(Kind::Int | Kind::Float | Kind::U64 | Kind::F32)]) => {
                dest.and_then(|dest| self.nearest_f32(dest, value))
            }
            ("float", [value], [Some(Kind::F32)]) | ("u64", [value], [Some(Kind::U64)]) => {
                dest.and_then(|dest| self.copy(dest, value))
            }
            ("float", [value], [Some(Kind::U64)]) => dest.and_then(|dest| {
                let (word, _) = self.operand(value)?;
                let float = self.temp();
                self.line(&format!("{float} = uitofp i64 {word} to double"));
                self.assign(dest, &float)
            }),
            ("int", [value], [Some(Kind::F32)]) => {
                dest.and_then(|dest| self.float_to_int(dest, value, None, span))
            }
            (sized, [value], [Some(from @ (Kind::Int | Kind::U64))])
                if fennec_check::Num::named(sized).is_some_and(|num| !num.is_float())
                    || (sized == "int" && *from == Kind::U64) =>
            {
                let from = from.clone();
                dest.and_then(|dest| self.sized_from(dest, sized, value, &from, span))
            }
            ("max" | "min", [list], [Some(Kind::List(_))]) => {
                dest.and_then(|dest| self.extreme_of_list(dest, name, list, None, span))
            }
            ("max.key", [list, key], [Some(Kind::List(_)), _]) => {
                dest.and_then(|dest| self.extreme_of_list(dest, "max", list, Some(key), span))
            }
            ("min.key", [list, key], [Some(Kind::List(_)), _]) => {
                dest.and_then(|dest| self.extreme_of_list(dest, "min", list, Some(key), span))
            }
            ("max" | "min", [_, _, ..], _) => {
                dest.and_then(|dest| self.extreme_of_values(dest, name, args, span))
            }
            ("list.contains", [list, item], [Some(Kind::List(_)), _]) => {
                dest.and_then(|dest| self.list_contains(dest, list, item))
            }
            ("list.remove", [Operand::Local(list), item], _) => self.list_remove(*list, item, span),
            ("int" | "float", [text], [Some(Kind::Str)]) => {
                dest.and_then(|dest| self.number_of_text(dest, name, text))
            }
            ("list", [text], [Some(Kind::Str)]) => dest.and_then(|dest| self.chars(dest, text)),
            ("set", [list], [Some(Kind::List(_))]) => dest.and_then(|dest| {
                let (list, _, held) = self.list_operand(list)?;
                self.set_of_list(dest, &list, &held)
            }),
            ("set", [text], [Some(Kind::Str)]) => dest.and_then(|dest| self.set_of_chars(dest, text)),
            ("assert.sides", [left, right], _) => self.assert_sides(left, right, span),
            ("assert.said", [happened], _) => self.assert_said(happened, span),
            ("generator.let_go", [Operand::Local(generator)], _) => {
                self.let_go_of_generator(*generator);
                Ok(())
            }
            ("sorted", [set], [Some(Kind::Dict(..))]) => dest.and_then(|dest| {
                self.dict_list(dest, set, "keys")?;
                self.sorted(dest, &Operand::Local(dest))
            }),
            ("bytes", [], []) => dest.and_then(|dest| self.empty_bytes(dest)),
            ("bytes", [list], [Some(Kind::List(_))]) => {
                dest.and_then(|dest| self.bytes_of_list(dest, list, span))
            }
            ("bytes.len", [bytes], _) => dest.and_then(|dest| {
                let (bytes, _) = self.operand(bytes)?;
                let length = self.list_length(&bytes);
                self.assign(dest, &length)
            }),
            ("list", [bytes], [Some(Kind::Bytes)]) => dest.and_then(|dest| self.list_of_bytes(dest, bytes)),
            ("bytes.contains", [bytes, byte], _) => {
                dest.and_then(|dest| self.bytes_contains(dest, bytes, byte))
            }
            ("bytes.hex", [bytes, rest @ ..], _) if rest.len() <= 1 => {
                dest.and_then(|dest| self.bytes_hex(dest, bytes, rest.first()))
            }
            ("bytes.decode", [bytes], _) => dest.and_then(|dest| {
                let (bytes, _) = self.operand(bytes)?;
                self.bytes_decode(dest, &bytes)
            }),
            ("bytes.fromhex", [text], _) => dest.and_then(|dest| self.bytes_from_hex(dest, text)),
            ("str.encode", [text], _) => dest.and_then(|dest| self.text_encoded(dest, text)),
            ("round", [value, places], [Some(Kind::Float), Some(Kind::Int)]) => dest.and_then(|dest| {
                let ((value, _), (places, _)) = (self.operand(value)?, self.operand(places)?);
                self.declare("declare double @fennec_round_places(double, i64) nounwind");
                let rounded = self.temp();
                self.line(&format!(
                    "{rounded} = call double @fennec_round_places(double {value}, i64 {places})"
                ));
                self.assign(dest, &rounded)
            }),
            ("str.codes", [text], _) => dest.and_then(|dest| self.codes_of_text(dest, text)),
            ("str.from_code", [code], _) => dest.and_then(|dest| self.text_from_code(dest, code)),
            ("dict.clear" | "set.clear", [Operand::Local(dict)], _) => {
                self.dict_clear(*dict);
                Ok(())
            }
            ("set.discard", [Operand::Local(set), key], _) => self.set_remove(*set, key, false, span),
            ("set.remove", [Operand::Local(set), key], _) => self.set_remove(*set, key, true, span),
            ("list.sort", [Operand::Local(list)], _) => self.sort_in_place(*list),
            ("list.sort.key", [Operand::Local(list), key], _) => self.sort_by_in_place(*list, key, span),
            ("list.clear", [Operand::Local(list)], _) => {
                let owned = self.unshared(*list);
                self.declare("declare void @fennec_list_clear(ptr) nounwind");
                self.line(&format!("call void @fennec_list_clear(ptr {owned})"));
                Ok(())
            }
            ("sorted", [text], [Some(Kind::Str)]) => dest.and_then(|dest| self.sorted_chars(dest, text)),
            ("SoaList", [], []) => dest.and_then(|dest| {
                let Kind::List(item) = self.kind_of(dest)?.clone() else {
                    return Err("a `SoaList` that isn't a list".to_owned());
                };
                let list = self.new_list(item, "0");
                self.assign(dest, &list)
            }),
            ("main.result", [result], [Some(Kind::Boxed(_) | Kind::Enum(_))]) => {
                self.main_result(result, span)
            }
            // An int `main` gives back is the program's exit code.
            ("main.result", [code], [Some(Kind::Int)]) => self.main_exit(code),
            ("enumerate", [text], [Some(Kind::Str)]) => dest.and_then(|dest| {
                let (text, _) = self.operand(text)?;
                self.on_blocks([&text], |emit, [block]| {
                    let list = emit.chars_list(&block);
                    emit.enumerated_list(dest, &list, &Kind::Str)?;
                    emit.release(&list);
                    Ok(())
                })
            }),
            ("main.failed", [error], _) => self.main_failed(error, span),
            ("shared.in", [value], _) => {
                dest.and_then(|dest| self.across_lock(dest, value, crate::locks::Way::In))
            }
            ("shared.out", [value, Operand::Int(held)], _) => {
                dest.and_then(|dest| self.across_lock(dest, value, crate::locks::Way::Out(*held)))
            }
            // `value.let_go` lets go of the result a generator for a loop was taken out of, when
            // the loop ends rather than when the function returns.
            ("shared.drop" | "value.let_go", [Operand::Local(local)], _) => self.drop_local(*local),
            ("task.failed", [error], _) => self.hand_over_failure(error),
            ("set", [], []) => dest.and_then(|dest| self.empty_set(dest)),
            // A set of a set's items is the set, shared until one changes.
            ("set", [set], [Some(Kind::Dict(..))]) => dest.and_then(|dest| self.copy(dest, set)),
            _ => return None,
        })
    }

    /// `u8(x)`: an integer as one of a size, which stops the program, saying so, if it doesn't fit.
    /// `int(x)` of a `u64` is one too.
    fn sized_from(
        &mut self,
        dest: LocalId,
        name: &str,
        value: &Operand,
        from: &Kind,
        span: Span,
    ) -> Result<(), String> {
        let (least, most) = match fennec_check::Num::named(name) {
            Some(num) => num.range().ok_or("a size that isn't an integer")?,
            None => (i128::from(i64::MIN), i128::from(i64::MAX)),
        };
        let (word, _) = self.operand(value)?;
        let bad = self.temp();
        if *from == Kind::U64 {
            // A `u64` is never below 0, so only its top can be too much.
            self.line(&format!("{bad} = icmp ugt i64 {word}, {most}"));
        } else if most > i128::from(i64::MAX) {
            // Any int at least 0 fits in a `u64`.
            self.line(&format!("{bad} = icmp slt i64 {word}, 0"));
        } else {
            let shifted = self.temp();
            self.line(&format!("{shifted} = sub i64 {word}, {least}"));
            self.line(&format!("{bad} = icmp ugt i64 {shifted}, {}", most - least));
        }
        let (stops, fits) = (self.label("too_big"), self.label("fits"));
        self.line(&format!("br i1 {bad}, label %{stops}, label %{fits}, !prof !0"));
        self.start_block(&stops);
        self.failed_with(&format!("it doesn't fit in {name}, which holds {least} to {most}: "), value, span)?;
        self.line(&format!("br label %{fits}"));
        self.start_block(&fits);
        self.assign(dest, &word)
    }

    /// `f32(x)`: the nearest `f32`, held as the `float` it is exactly.
    fn nearest_f32(&mut self, dest: LocalId, value: &Operand) -> Result<(), String> {
        let (number, kind) = self.operand(value)?;
        // An int rounds straight to an `f32`: through a `float` it could round twice.
        let to = match kind {
            Kind::Int => "sitofp i64",
            Kind::U64 => "uitofp i64",
            _ => {
                let nearest = self.rounded_to_f32(&number);
                return self.assign(dest, &nearest);
            }
        };
        let (narrow, nearest) = (self.temp(), self.temp());
        self.line(&format!("{narrow} = {to} {number} to float"));
        self.line(&format!("{nearest} = fpext float {narrow} to double"));
        self.assign(dest, &nearest)
    }

    /// `int(text)` or `float(text)`: `Ok` with the number, or `Err` with an `InvalidValue`.
    pub(crate) fn number_of_text(&mut self, dest: LocalId, name: &str, text: &Operand) -> Result<(), String> {
        let kind = self.kind_of(dest)?.clone();
        let (ok_case, ok_fields) = self.case_of(&kind, "Ok")?;
        let (err_case, err_fields) = self.case_of(&kind, "Err")?;
        let (_, ok_offset, _) = ok_fields.first().cloned().ok_or("an `Ok` holding nothing")?;
        let (_, err_offset, err_kind) = err_fields.first().cloned().ok_or("an `Err` holding nothing")?;
        let err_kind = err_kind.ok_or("an error this backend doesn't take yet")?;
        let (value, _) = self.operand(text)?;
        let boxed = if let Kind::Boxed(id) = kind { Some(self.new_block(id)?) } else { None };
        let base = if let Some(boxed) = &boxed {
            self.offset(boxed, crate::lists::FIELDS_OFFSET)
        } else {
            self.release_held(dest)?;
            let slot = self.slot(dest);
            let zero = self.zero_line(&kind, &slot);
            self.line(&zero);
            slot
        };
        // A short int written plainly is read where its header holds it, with no block made for
        // it and no call. Anything else goes to the runtime, which says what's wrong with it.
        let quick_end = if name == "int" && self.headers() {
            let (read, number) = self.plain_inline_int(&value);
            let (quick, slow, end) =
                (self.label("plain_int"), self.label("other_int"), self.label("int_read"));
            self.line(&format!("br i1 {read}, label %{quick}, label %{slow}"));
            self.start_block(&quick);
            self.line(&format!("store i64 {ok_case}, ptr {base}"));
            let number_at = self.offset(&base, ok_offset);
            self.line(&format!("store i64 {number}, ptr {number_at}"));
            self.line(&format!("br label %{end}"));
            self.start_block(&slow);
            Some(end)
        } else {
            None
        };
        // A float's text goes to the runtime as its header's words, read where it is: only text
        // that isn't a float is made a block, for the message that says so.
        let by_header = name == "float" && self.headers();
        let (block, made) = if by_header {
            (None, None)
        } else if self.headers() {
            let (block, made) = self.text_block(&value);
            (Some(block), Some(made))
        } else {
            (Some(value.clone()), None)
        };
        let bits_at = self.place_for(&Kind::Int);
        let (read, bits, is_number) = (self.temp(), self.temp(), self.temp());
        if let Some(block) = &block {
            let function = if name == "int" { "fennec_parse_int" } else { "fennec_parse_float" };
            self.declare(&format!("declare i64 @{function}(ptr, ptr) nounwind"));
            self.line(&format!("{read} = call i64 @{function}(ptr {block}, ptr {bits_at})"));
        } else {
            let [t0, t1, t2] = [0, 1, 2].map(|at| self.header_word(&value, at));
            self.declare("declare i64 @fennec_parse_float_header(i64, i64, i64, ptr) nounwind");
            self.line(&format!(
                "{read} = call i64 @fennec_parse_float_header(i64 {t0}, i64 {t1}, i64 {t2}, ptr {bits_at})"
            ));
        }
        self.line(&format!("{is_number} = icmp ne i64 {read}, 0"));
        let (good, bad, joined) = (self.label("number"), self.label("not_number"), self.label("read"));
        self.line(&format!("br i1 {is_number}, label %{good}, label %{bad}"));
        self.start_block(&good);
        self.line(&format!("store i64 {ok_case}, ptr {base}"));
        let number_at = self.offset(&base, ok_offset);
        self.line(&format!("{bits} = load i64, ptr {bits_at}"));
        self.line(&format!("store i64 {bits}, ptr {number_at}"));
        self.line(&format!("br label %{joined}"));
        self.start_block(&bad);
        self.line(&format!("store i64 {err_case}, ptr {base}"));
        self.declare("declare ptr @fennec_not_a_number(ptr, i64) nounwind");
        let message = self.temp();
        let int = i64::from(name == "int");
        // Text read by its header is made a block here, where it wasn't a number, and let go of
        // once the message is made.
        let (quoted, made_here) = if let Some(block) = &block {
            (block.clone(), None)
        } else {
            let (block, made) = self.text_block(&value);
            (block, Some(made))
        };
        self.line(&format!("{message} = call ptr @fennec_not_a_number(ptr {quoted}, i64 {int})"));
        let message = self.text_value(&message);
        let error_at = self.offset(&base, err_offset);
        self.invalid_value(&err_kind, &message, &error_at)?;
        if let Some(made) = made_here {
            self.let_go_text_block(&quoted, &made);
        }
        self.line(&format!("br label %{joined}"));
        self.start_block(&joined);
        if let (Some(block), Some(made)) = (&block, made) {
            self.let_go_text_block(block, &made);
        }
        if let Some(end) = quick_end {
            self.line(&format!("br label %{end}"));
            self.start_block(&end);
        }
        if let Some(boxed) = boxed {
            self.assign(dest, &boxed)?;
        }
        Ok(())
    }

    /// Whether an inline header holds an int written plainly, an optional `-` and 1 to 18 ASCII
    /// digits, and the int. That's too few digits to overflow, and the runtime reads each such
    /// text as the same int, so whatever's read here is what it would read.
    fn plain_inline_int(&mut self, header: &str) -> (String, String) {
        let in_block = self.header_in_block(header);
        let length = self.inline_length(header);
        let place = self.place_for_text();
        self.line(&format!("store {} {header}, ptr {place}", crate::headers::HEADER));
        let (first, negative, start, digits) = (self.temp(), self.temp(), self.temp(), self.temp());
        self.line(&format!("{first} = load i8, ptr {place}"));
        self.line(&format!("{negative} = icmp eq i8 {first}, 45"));
        self.line(&format!("{start} = zext i1 {negative} to i64"));
        self.line(&format!("{digits} = sub i64 {length}, {start}"));
        // 1 to 18 digits: one less than 19, unsigned, so none is a huge one.
        let (less, few, inline, fits) = (self.temp(), self.temp(), self.temp(), self.temp());
        self.line(&format!("{less} = sub i64 {digits}, 1"));
        self.line(&format!("{few} = icmp ult i64 {less}, 18"));
        self.line(&format!("{inline} = xor i1 {in_block}, true"));
        self.line(&format!("{fits} = and i1 {inline}, {few}"));
        let entry = self.current.clone();
        let (next, digit, done, other, read) = (
            self.label("int_digits"),
            self.label("int_digit"),
            self.label("int_done"),
            self.label("int_other"),
            self.label("int_plain"),
        );
        self.line(&format!("br i1 {fits}, label %{next}, label %{other}"));
        self.start_block(&next);
        let (at, sum, at_end, more) = (self.temp(), self.temp(), self.temp(), self.label("int_more"));
        let (byte_at, byte, wide, value, is_digit) =
            (self.temp(), self.temp(), self.temp(), self.temp(), self.temp());
        let (times, added, after) = (self.temp(), self.temp(), self.temp());
        self.line(&format!("{at} = phi i64 [ {start}, %{entry} ], [ {after}, %{digit} ]"));
        self.line(&format!("{sum} = phi i64 [ 0, %{entry} ], [ {added}, %{digit} ]"));
        self.line(&format!("{at_end} = icmp eq i64 {at}, {length}"));
        self.line(&format!("br i1 {at_end}, label %{done}, label %{more}"));
        self.start_block(&more);
        self.line(&format!("{byte_at} = getelementptr inbounds i8, ptr {place}, i64 {at}"));
        self.line(&format!("{byte} = load i8, ptr {byte_at}"));
        self.line(&format!("{wide} = zext i8 {byte} to i64"));
        self.line(&format!("{value} = sub i64 {wide}, 48"));
        self.line(&format!("{is_digit} = icmp ult i64 {value}, 10"));
        self.line(&format!("br i1 {is_digit}, label %{digit}, label %{other}"));
        self.start_block(&digit);
        self.line(&format!("{times} = mul nuw nsw i64 {sum}, 10"));
        self.line(&format!("{added} = add nuw nsw i64 {times}, {value}"));
        self.line(&format!("{after} = add nuw nsw i64 {at}, 1"));
        self.line(&format!("br label %{next}"));
        self.start_block(&done);
        let (flipped, number) = (self.temp(), self.temp());
        self.line(&format!("{flipped} = sub nsw i64 0, {sum}"));
        self.line(&format!("{number} = select i1 {negative}, i64 {flipped}, i64 {sum}"));
        self.line(&format!("br label %{read}"));
        self.start_block(&other);
        self.line(&format!("br label %{read}"));
        self.start_block(&read);
        let (plain, got) = (self.temp(), self.temp());
        self.line(&format!("{plain} = phi i1 [ true, %{done} ], [ false, %{other} ]"));
        self.line(&format!("{got} = phi i64 [ {number}, %{done} ], [ 0, %{other} ]"));
        (plain, got)
    }

    /// `text.codes()`.
    fn codes_of_text(&mut self, dest: LocalId, text: &Operand) -> Result<(), String> {
        let (text, _) = self.operand(text)?;
        let symbol = crate::kinds::element_symbol(self.module.layouts, ListItem::Int);
        self.module.shared.elements.insert(symbol.clone(), ListItem::Int);
        self.declare(&format!("@\"{symbol}\" = external dso_local global i64"));
        self.declare("declare ptr @fennec_str_codes(ptr, i64) nounwind");
        let (element, list) = (self.temp(), self.temp());
        self.line(&format!("{element} = load i64, ptr @\"{symbol}\", !invariant.load !1"));
        self.on_blocks([&text], |emit, [block]| {
            emit.line(&format!("{list} = call ptr @fennec_str_codes(ptr {block}, i64 {element})"));
        });
        self.assign(dest, &list)
    }

    /// `str.from_code(code)`: `Ok` with the character, or `Err` with an `InvalidValue`.
    fn text_from_code(&mut self, dest: LocalId, code: &Operand) -> Result<(), String> {
        let (code, _) = self.operand(code)?;
        self.declare("declare ptr @fennec_str_from_code(i64) nounwind");
        self.declare("declare ptr @fennec_str_from_code_problem(i64) nounwind");
        let made = self.temp();
        self.line(&format!("{made} = call ptr @fennec_str_from_code(i64 {code})"));
        self.made_or_invalid(dest, &made, &|emit| {
            let message = emit.temp();
            emit.line(&format!("{message} = call ptr @fennec_str_from_code_problem(i64 {code})"));
            message
        })
    }

    /// A `Result` of something made by a runtime call that gives back 0 when it can't, with the
    /// message `problem` writes in an `InvalidValue`.
    pub(crate) fn made_or_invalid(
        &mut self,
        dest: LocalId,
        made: &str,
        problem: &dyn Fn(&mut Self) -> String,
    ) -> Result<(), String> {
        let kind = self.kind_of(dest)?.clone();
        let (ok_case, ok_fields) = self.case_of(&kind, "Ok")?;
        let (err_case, err_fields) = self.case_of(&kind, "Err")?;
        let (_, ok_offset, ok_kind) = ok_fields.first().cloned().ok_or("an `Ok` holding nothing")?;
        let ok_kind = ok_kind.ok_or("a value this backend doesn't take yet")?;
        let (_, err_offset, err_kind) = err_fields.first().cloned().ok_or("an `Err` holding nothing")?;
        let err_kind = err_kind.ok_or("an error this backend doesn't take yet")?;
        let boxed = if let Kind::Boxed(id) = kind { Some(self.new_block(id)?) } else { None };
        let base = if let Some(boxed) = &boxed {
            self.offset(boxed, crate::lists::FIELDS_OFFSET)
        } else {
            self.release_held(dest)?;
            let slot = self.slot(dest);
            let zero = self.zero_line(&kind, &slot);
            self.line(&zero);
            slot
        };
        let is_made = self.temp();
        self.line(&format!("{is_made} = icmp ne ptr {made}, null"));
        let (good, bad, joined) = (self.label("made"), self.label("not_made"), self.label("done"));
        self.line(&format!("br i1 {is_made}, label %{good}, label %{bad}"));
        self.start_block(&good);
        self.line(&format!("store i64 {ok_case}, ptr {base}"));
        let value_at = self.offset(&base, ok_offset);
        let value = if ok_kind == Kind::Str { self.text_value(made) } else { made.to_owned() };
        self.store_field(&ok_kind, &value, &value_at, "");
        self.line(&format!("br label %{joined}"));
        self.start_block(&bad);
        self.line(&format!("store i64 {err_case}, ptr {base}"));
        let message = problem(self);
        let message = self.text_value(&message);
        let error_at = self.offset(&base, err_offset);
        self.invalid_value(&err_kind, &message, &error_at)?;
        self.line(&format!("br label %{joined}"));
        self.start_block(&joined);
        if let Some(boxed) = boxed {
            self.assign(dest, &boxed)?;
        }
        Ok(())
    }

    /// An `InvalidValue` written at `at`, holding a message it takes over.
    fn invalid_value(&mut self, kind: &Kind, message: &str, at: &str) -> Result<(), String> {
        let (offset, field) = self.field(kind, "message")?;
        if let Kind::Boxed(id) = kind {
            let block = self.new_block(*id)?;
            let fields = self.offset(&block, crate::lists::FIELDS_OFFSET);
            let place = self.offset(&fields, offset);
            self.store_field(&field, message, &place, "");
            self.store_field(kind, &block, at, "");
        } else {
            let place = self.offset(at, offset);
            self.store_field(&field, message, &place, "");
        }
        Ok(())
    }

    /// `tuple[start:end]` with its bounds written out: a tuple of the items they take, each with
    /// an owner of its own. A tuple's length is known, so this is only ever copying.
    pub(crate) fn tuple_slice(
        &mut self,
        dest: LocalId,
        (object, kind): (&str, &Kind),
        start: Option<&Operand>,
    ) -> Result<(), String> {
        let (Kind::Struct(whole) | Kind::Boxed(whole)) = *kind else {
            return Err(format!("it slices a {kind:?}, which this backend doesn't take yet"));
        };
        let wanted = self.kind_of(dest)?.clone();
        let (Kind::Struct(part) | Kind::Boxed(part)) = wanted else {
            return Err("a slice of a tuple that isn't a tuple".to_owned());
        };
        let count = i64::try_from(self.layout_of(whole)?.fields.len()).unwrap_or(0);
        let first = match start {
            None => 0,
            Some(Operand::Int(at)) if *at < 0 => count + at,
            Some(Operand::Int(at)) => *at,
            Some(_) => return Err("it slices a tuple from a place it can't know".to_owned()),
        };
        let items = self.layout_of(part)?.fields.len();
        let from = self.fields_at(object, kind);
        let block = if matches!(wanted, Kind::Boxed(_)) { Some(self.new_block(part)?) } else { None };
        let to = match &block {
            Some(block) => self.offset(block, crate::lists::FIELDS_OFFSET),
            None => self.place_for(&wanted),
        };
        for at in 0..items {
            let source = first + i64::try_from(at).unwrap_or(0);
            let (source_at, held) = self.field(kind, &format!("_{source}"))?;
            let (dest_at, _) = self.field(&wanted, &format!("_{at}"))?;
            let (source_at, dest_at) = (self.offset(&from, source_at), self.offset(&to, dest_at));
            let value = if held.in_memory() { source_at } else { self.load_field(&held, &source_at, "") };
            if held.counted() {
                self.retain_value(&value, &held);
            }
            self.store_field(&held, &value, &dest_at, "");
            self.retain_inline(&held, &dest_at);
        }
        if let Some(block) = block {
            return self.assign(dest, &block);
        }
        self.assign_memory_owned(dest, &to)
    }

    /// Fails the program if what `main` gave back is an error, as the runtime says it. Only then
    /// does the result cross to the runtime.
    fn main_result(&mut self, result: &Operand, span: Span) -> Result<(), String> {
        let (value, kind) = self.operand(result)?;
        let (err_case, _) = self.case_of(&kind, "Err")?;
        let fields = self.fields_at(&value, &kind);
        let (case, failed) = (self.temp(), self.temp());
        self.line(&format!("{case} = load i64, ptr {fields}"));
        self.line(&format!("{failed} = icmp eq i64 {case}, {err_case}"));
        let (bad, fine) = (self.label("main_failed"), self.label("main_done"));
        self.line(&format!("br i1 {failed}, label %{bad}, label %{fine}"));
        self.start_block(&bad);
        let block = if let Kind::Enum(id) = kind {
            let block = self.new_block(id)?;
            let fields = self.offset(&block, crate::lists::FIELDS_OFFSET);
            self.store_field(&kind, &value, &fields, "");
            self.retain_inline(&kind, &fields);
            block
        } else {
            value
        };
        self.declare("declare i64 @fennec_box_to_value(ptr) nounwind");
        self.declare("declare void @fennec_main_result(i64, i64, i64) nounwind");
        let handle = self.temp();
        let (from, to) = (i64::from(span.start), i64::from(span.end));
        self.line(&format!("{handle} = call i64 @fennec_box_to_value(ptr {block})"));
        self.line(&format!("call void @fennec_main_result(i64 {handle}, i64 {from}, i64 {to})"));
        self.leave_if_failed();
        self.line(&format!("br label %{fine}"));
        self.start_block(&fine);
        // An int in its `Ok` is the program's exit code.
        let (_, given) = self.case_of(&kind, "Ok")?;
        if let Some((_, offset, Some(Kind::Int))) = given.first() {
            let at = self.offset(&fields, *offset);
            let access = if matches!(kind, Kind::Boxed(_)) { crate::emit::FIELD_ACCESS } else { "" };
            let code = self.load_field(&Kind::Int, &at, access);
            self.declare("declare void @fennec_main_exit(i64) nounwind");
            self.line(&format!("call void @fennec_main_exit(i64 {code})"));
        }
        Ok(())
    }

    /// Hands the int `main` gave back to the runtime, which exits with it.
    fn main_exit(&mut self, code: &Operand) -> Result<(), String> {
        let (code, _) = self.operand(code)?;
        self.declare("declare void @fennec_main_exit(i64) nounwind");
        self.line(&format!("call void @fennec_main_exit(i64 {code})"));
        Ok(())
    }

    /// `sorted(text)`: a list of its characters, in order.
    fn sorted_chars(&mut self, dest: LocalId, text: &Operand) -> Result<(), String> {
        let (text, _) = self.operand(text)?;
        self.on_blocks([&text], |emit, [block]| {
            let list = emit.chars_list(&block);
            emit.declare("declare ptr @fennec_list_sorted(ptr, i64) nounwind");
            let sorted = emit.temp();
            let how = fennec_rt::embed::lists::SORT_STRINGS;
            emit.line(&format!("{sorted} = call ptr @fennec_list_sorted(ptr {list}, i64 {how})"));
            emit.release(&list);
            emit.assign(dest, &sorted)
        })
    }

    /// `set(text)`: a set of its characters.
    fn set_of_chars(&mut self, dest: LocalId, text: &Operand) -> Result<(), String> {
        let (text, _) = self.operand(text)?;
        self.on_blocks([&text], |emit, [block]| {
            let list = emit.chars_list(&block);
            emit.set_of_list(dest, &list, &Kind::Str)?;
            emit.release(&list);
            Ok(())
        })
    }

    /// `math.sin`, `math.cos`, `math.tan`, `math.acos`, `math.exp` and `math.log`, by the runtime,
    /// which computes them itself, so they're the same bits on every platform.
    fn turned(&mut self, dest: LocalId, function: &str, value: &Operand) -> Result<(), String> {
        let (value, kind) = self.operand(value)?;
        let angle = if kind == Kind::Int {
            let angle = self.temp();
            self.line(&format!("{angle} = sitofp i64 {value} to double"));
            angle
        } else {
            value
        };
        self.declare(&format!("declare double @{function}(double) nounwind"));
        let result = self.temp();
        self.line(&format!("{result} = call double @{function}(double {angle})"));
        self.assign(dest, &result)
    }

    /// `max` or `min` of a list's items, by a key if it has one: the place of the best so far is
    /// kept, so only the one that wins gets an owner. The first of equals wins.
    fn extreme_of_list(
        &mut self,
        dest: LocalId,
        name: &str,
        list: &Operand,
        key: Option<&Operand>,
        span: Span,
    ) -> Result<(), String> {
        let (list, _, held) = self.list_operand(list)?;
        let keys = key.map(|key| self.keys_of(&list, &held, key, span)).transpose()?;
        let length = self.list_length(&list);
        let (empty, stop, go) = (self.temp(), self.label("empty"), self.label("some"));
        self.line(&format!("{empty} = icmp eq i64 {length}, 0"));
        self.line(&format!("br i1 {empty}, label %{stop}, label %{go}, !prof !0"));
        self.start_block(&stop);
        let message = self.literal(&format!("`{name}` of an empty list"));
        self.stop_with(&message, span);
        self.start_block(&go);
        let (scores, score_kind) = keys.clone().unwrap_or_else(|| (list.clone(), held.clone()));
        let best_slot = self.counter();
        let (at, end) = self.loop_begin(&length);
        let best = self.temp();
        self.line(&format!("{best} = load i64, ptr {best_slot}"));
        let (score, best_score) =
            (self.item_value(&scores, &score_kind, &at), self.item_value(&scores, &score_kind, &best));
        let better = self.better(name, &score_kind, (&score, &best_score), span)?;
        let picked = self.temp();
        self.line(&format!("{picked} = select i1 {better}, i64 {at}, i64 {best}"));
        self.line(&format!("store i64 {picked}, ptr {best_slot}"));
        self.loop_end(&end);
        if let Some((keys, _)) = keys {
            self.release(&keys);
        }
        let winner = self.temp();
        self.line(&format!("{winner} = load i64, ptr {best_slot}"));
        let memory = held.memory(self.module.layouts);
        let place = self.item_address(&list, &memory, &winner);
        self.read_into(dest, &held, &place, ITEM_ACCESS)
    }

    /// `max` or `min` of values given one by one.
    fn extreme_of_values(
        &mut self,
        dest: LocalId,
        name: &str,
        args: &[Operand],
        span: Span,
    ) -> Result<(), String> {
        let (first, others) = args.split_first().ok_or("no argument")?;
        let (mut best, kind) = self.operand(first)?;
        let text = if kind.in_memory() { "ptr".to_owned() } else { self.text_of(&kind) };
        for other in others {
            let (value, other_kind) = self.operand(other)?;
            if other_kind != kind {
                return Err(format!("it calls `{name}` on a {kind:?} and a {other_kind:?}"));
            }
            let better = self.better(name, &kind, (&value, &best), span)?;
            let picked = self.temp();
            self.line(&format!("{picked} = select i1 {better}, {text} {value}, {text} {best}"));
            best = picked;
        }
        if kind.in_memory() {
            return self.assign_memory(dest, &best, "");
        }
        if kind.counted() {
            self.retain_value(&best, &kind);
        }
        self.assign(dest, &best)
    }

    /// Whether `score` beats `best` for `max` or `min`, as the language orders them. Values
    /// in memory come as addresses.
    pub(crate) fn better(
        &mut self,
        name: &str,
        kind: &Kind,
        (score, best): (&str, &str),
        span: Span,
    ) -> Result<String, String> {
        let is_max = name == "max";
        let op = if is_max { BinOp::Gt } else { BinOp::Lt };
        let compared = self.temp();
        match kind {
            Kind::Int => {
                let cc = if is_max { "sgt" } else { "slt" };
                self.line(&format!("{compared} = icmp {cc} i64 {score}, {best}"));
                Ok(compared)
            }
            Kind::Bool => {
                let cc = if is_max { "ugt" } else { "ult" };
                self.line(&format!("{compared} = icmp {cc} i1 {score}, {best}"));
                Ok(compared)
            }
            Kind::Float => {
                let (score, best) = (self.total_order_key(score), self.total_order_key(best));
                let cc = if is_max { "sgt" } else { "slt" };
                self.line(&format!("{compared} = icmp {cc} i64 {score}, {best}"));
                Ok(compared)
            }
            Kind::Str if self.headers() => self.header_binary(op, (score, best)),
            Kind::Str => self.string_binary(op, (score, best)),
            Kind::Struct(id) | Kind::Boxed(id) => {
                let layout = self.layout_of(*id)?;
                let own = format!("{}.<", layout.name);
                if self.module.whole.functions.contains_key(own.as_str()) {
                    let less = self.own_less(&own, (score, best), span)?;
                    if !is_max {
                        return Ok(less);
                    }
                    // Its `<` decides, so what's more is what's neither less nor equal.
                    let more = self.own_less(&own, (best, score), span)?;
                    let (not_less, both) = (self.temp(), self.temp());
                    self.line(&format!("{not_less} = xor i1 {less}, true"));
                    self.line(&format!("{both} = and i1 {not_less}, {more}"));
                    return Ok(both);
                }
                if !self.orders_by_fields(*id) {
                    return Err(format!(
                        "it calls `{name}` on `{}` values, which this backend doesn't take yet",
                        self.layout_of(*id)?.name
                    ));
                }
                if matches!(kind, Kind::Boxed(_)) {
                    self.compare_boxed(op, *id, (score, best))
                } else {
                    self.compare_structs(op, *id, (score, best))
                }
            }
            other => Err(format!("it calls `{name}` on a {other:?}, which this backend doesn't take yet")),
        }
    }

    /// An int that orders as floats do, with those that are equal as one.
    fn total_order_key(&mut self, value: &str) -> String {
        let (raw, zero, nan, plain, bits) = (self.temp(), self.temp(), self.temp(), self.temp(), self.temp());
        self.line(&format!("{raw} = bitcast double {value} to i64"));
        self.line(&format!("{zero} = fcmp oeq double {value}, {}", float_text(0.0)));
        self.line(&format!("{nan} = fcmp uno double {value}, {value}"));
        self.line(&format!("{plain} = select i1 {zero}, i64 0, i64 {raw}"));
        let nan_bits = f64::NAN.to_bits().cast_signed();
        self.line(&format!("{bits} = select i1 {nan}, i64 {nan_bits}, i64 {plain}"));
        let (sign, rest, key) = (self.temp(), self.temp(), self.temp());
        self.line(&format!("{sign} = ashr i64 {bits}, 63"));
        self.line(&format!("{rest} = lshr i64 {sign}, 1"));
        self.line(&format!("{key} = xor i64 {bits}, {rest}"));
        key
    }

    /// Whether only a tuple's own items order it, all the way down, with no type's own `<` in it.
    fn orders_by_fields(&self, id: u32) -> bool {
        let layouts = self.module.layouts;
        let Some(layout) = layouts.get(id) else { return false };
        layout.name.starts_with('(')
            && layout.fields.iter().all(|field| match Kind::from_ty(layouts, field.ty) {
                Some(Kind::Int | Kind::Float | Kind::Bool | Kind::Str) => true,
                Some(Kind::Struct(inner) | Kind::Boxed(inner)) => self.orders_by_fields(inner),
                _ => false,
            })
    }

    /// A call to a type's own `<`, whose values come as addresses, either way they're held.
    fn own_less(&mut self, name: &str, (a, b): (&str, &str), span: Span) -> Result<String, String> {
        let callee = *self.module.whole.functions.get(name).ok_or("a `<` that isn't lowered")?;
        if !self.module.defined.contains(name) {
            let (ret, params) = (self.module.ret_text(callee)?, self.module.params(callee, false)?);
            self.declare(&format!("declare swiftcc {ret} {}({params}) #0", symbol(name)));
        }
        self.check_stack(span);
        let flagged = self.module.flagged(name);
        let failure = if flagged { ", ptr swifterror %err" } else { "" };
        let result = self.temp();
        let target = symbol(name);
        self.line(&format!("{result} = call swiftcc i1 {target}(ptr {a}, ptr {b}{failure})"));
        if flagged {
            let (held, failed) = (self.temp(), self.temp());
            self.line(&format!("{held} = load ptr, ptr %err"));
            self.line(&format!("{failed} = icmp ne ptr {held}, null"));
            self.leave_if(&failed);
        }
        Ok(result)
    }

    /// `a + b` on lists: a copy of the first, with the second's items after them.
    pub(crate) fn joined_lists(&mut self, a: &str, b: &str) -> String {
        self.declare("declare ptr @fennec_list_slice(ptr, i64, i64) nounwind");
        self.declare("declare ptr @fennec_list_extend(ptr, ptr) nounwind");
        let (copy, joined) = (self.temp(), self.temp());
        let lowest = i64::MIN;
        self.line(&format!("{copy} = call ptr @fennec_list_slice(ptr {a}, i64 {lowest}, i64 {lowest})"));
        self.line(&format!("{joined} = call ptr @fennec_list_extend(ptr {copy}, ptr {b})"));
        joined
    }

    /// `<` and the like on lists: the first items that differ decide, or else the lengths.
    pub(crate) fn lists_in_order(
        &mut self,
        op: BinOp,
        item: ListItem,
        (a, b): (&str, &str),
        span: Span,
    ) -> Result<String, String> {
        let held = self.item_kind(item)?;
        let (a_length, b_length) = (self.list_length(a), self.list_length(b));
        let (shorter, both) = (self.temp(), self.temp());
        self.line(&format!("{shorter} = icmp slt i64 {a_length}, {b_length}"));
        self.line(&format!("{both} = select i1 {shorter}, i64 {a_length}, i64 {b_length}"));
        // Below zero if `a` comes first, above if `b` does, and zero if they've been equal so far.
        let ordering = self.counter();
        let (at, end) = self.loop_begin(&both);
        let (decided, compare, next) = (self.temp(), self.label("compare"), self.label("next"));
        let so_far = self.temp();
        self.line(&format!("{so_far} = load i64, ptr {ordering}"));
        self.line(&format!("{decided} = icmp ne i64 {so_far}, 0"));
        self.line(&format!("br i1 {decided}, label %{next}, label %{compare}"));
        self.start_block(&compare);
        let (x, y) = (self.item_value(a, &held, &at), self.item_value(b, &held, &at));
        let less = self.better("min", &held, (&x, &y), span)?;
        let more = self.better("max", &held, (&x, &y), span)?;
        let (above, order) = (self.temp(), self.temp());
        self.line(&format!("{above} = select i1 {more}, i64 1, i64 0"));
        self.line(&format!("{order} = select i1 {less}, i64 -1, i64 {above}"));
        self.line(&format!("store i64 {order}, ptr {ordering}"));
        self.line(&format!("br label %{next}"));
        self.start_block(&next);
        self.loop_end(&end);
        let (so_far, by_length, equal, length_order, result) =
            (self.temp(), self.temp(), self.temp(), self.temp(), self.temp());
        self.line(&format!("{so_far} = load i64, ptr {ordering}"));
        self.line(&format!("{by_length} = sub i64 {a_length}, {b_length}"));
        self.line(&format!("{equal} = icmp eq i64 {so_far}, 0"));
        self.line(&format!("{length_order} = select i1 {equal}, i64 {by_length}, i64 {so_far}"));
        let cc = match op {
            BinOp::Lt => "slt",
            BinOp::LtEq => "sle",
            BinOp::Gt => "sgt",
            _ => "sge",
        };
        self.line(&format!("{result} = icmp {cc} i64 {length_order}, 0"));
        Ok(result)
    }

    /// `item in list`, by the runtime, which compares as `==` does.
    fn list_contains(&mut self, dest: LocalId, list: &Operand, item: &Operand) -> Result<(), String> {
        let (list, _, held) = self.list_operand(list)?;
        let (word, passed) = self.item_word(item, &held)?;
        self.declare("declare i8 @fennec_list_contains(ptr, i64) nounwind");
        let (found, bit) = (self.temp(), self.temp());
        self.line(&format!("{found} = call i8 @fennec_list_contains(ptr {list}, i64 {word})"));
        self.line(&format!("{bit} = icmp ne i8 {found}, 0"));
        self.let_go_of_passed(passed);
        self.assign(dest, &bit)
    }

    /// `list.remove(item)`: the runtime takes out the first that's equal, or fails, in a list
    /// only this one owns.
    fn list_remove(&mut self, list: LocalId, item: &Operand, span: Span) -> Result<(), String> {
        let Kind::List(items) = self.kind_of(list)?.clone() else {
            return Err("`remove` on something else".to_owned());
        };
        let held = self.item_kind(items)?;
        let (word, passed) = self.item_word(item, &held)?;
        let owned = self.unshared(list);
        self.declare("declare void @fennec_list_remove(ptr, i64, i64, i64) nounwind");
        let (from, to) = (i64::from(span.start), i64::from(span.end));
        self.line(&format!("call void @fennec_list_remove(ptr {owned}, i64 {word}, i64 {from}, i64 {to})"));
        self.leave_if_failed();
        self.let_go_of_passed(passed);
        Ok(())
    }

    /// An item as the runtime's `in` and `remove` take it: its one word, the block of a string,
    /// or a new block of its shape for one in memory that takes more than a word or holds blocks.
    fn item_word(&mut self, item: &Operand, held: &Kind) -> Result<(String, Passed), String> {
        let (value, fresh) = self.value_for(item, held)?;
        let as_word = |emit: &mut Self, instruction: &str, text: &str, value: &str| {
            let word = emit.temp();
            emit.line(&format!("{word} = {instruction} {text} {value} to i64"));
            word
        };
        Ok(match held {
            Kind::Int => (value, Passed::Nothing),
            Kind::Bool => (as_word(self, "zext", "i1", &value), Passed::Nothing),
            Kind::Float => (as_word(self, "bitcast", "double", &value), Passed::Nothing),
            Kind::Str if self.headers() => {
                let (block, owned) = self.text_block(&value);
                (as_word(self, "ptrtoint", "ptr", &block), Passed::Text(block, owned))
            }
            Kind::Struct(id) | Kind::Enum(id) => {
                let id = *id;
                let words = self.layout_of(id)?.size / 8;
                if words <= 1 && !self.holds_inline(held) {
                    let word = self.temp();
                    self.line(&format!("{word} = load i64, ptr {value}"));
                    (word, Passed::Nothing)
                } else {
                    let block = self.new_block(id)?;
                    let fields = self.offset(&block, crate::lists::FIELDS_OFFSET);
                    self.store_field(held, &value, &fields, "");
                    if !fresh {
                        self.retain_inline(held, &fields);
                    }
                    (as_word(self, "ptrtoint", "ptr", &block), Passed::Block(block))
                }
            }
            _ => {
                let word = as_word(self, "ptrtoint", "ptr", &value);
                (word, if fresh { Passed::Block(value) } else { Passed::Nothing })
            }
        })
    }

    fn let_go_of_passed(&mut self, passed: Passed) {
        match passed {
            Passed::Nothing => {}
            Passed::Block(block) => self.release(&block),
            Passed::Text(block, owned) => self.let_go_text_block(&block, &owned),
        }
    }

    /// One of LLVM's own functions on a float.
    fn float_call(&mut self, function: &str, value: &str) -> String {
        self.declare(&format!("declare double @llvm.{function}.f64(double)"));
        let result = self.temp();
        self.line(&format!("{result} = call double @llvm.{function}.f64(double {value})"));
        result
    }

    /// A float as an int, rounded by `rounding` first, or toward zero: one outside an int's
    /// range, or not a number, fails.
    fn float_to_int(
        &mut self,
        dest: LocalId,
        value: &Operand,
        rounding: Option<&str>,
        span: Span,
    ) -> Result<(), String> {
        let (value, _) = self.operand(value)?;
        let value = match rounding {
            Some(rounding) => self.float_call(rounding, &value),
            None => value,
        };
        let (above, below, inside, outside) = (self.temp(), self.temp(), self.temp(), self.temp());
        self.line(&format!("{above} = fcmp oge double {value}, {}", float_text(-INT_LIMIT)));
        self.line(&format!("{below} = fcmp olt double {value}, {}", float_text(INT_LIMIT)));
        self.line(&format!("{inside} = and i1 {above}, {below}"));
        self.line(&format!("{outside} = xor i1 {inside}, true"));
        self.fail_if(&outside, NOT_AN_INT, span);
        let whole = self.temp();
        self.line(&format!("{whole} = fptosi double {value} to i64"));
        self.assign(dest, &whole)
    }

    /// `range(...)` as a list of its ints.
    fn range_list(&mut self, dest: LocalId, args: &[Operand], span: Span) -> Result<(), String> {
        let mut values = Vec::with_capacity(args.len());
        for arg in args {
            values.push(self.operand(arg)?.0);
        }
        let (first, end, step) = match values.as_slice() {
            [end] => ("0".to_owned(), end.clone(), "1".to_owned()),
            [first, end] => (first.clone(), end.clone(), "1".to_owned()),
            [first, end, step] => (first.clone(), end.clone(), step.clone()),
            _ => return Err("`range` takes one to three ints".to_owned()),
        };
        let zero = self.temp();
        self.line(&format!("{zero} = icmp eq i64 {step}, 0"));
        self.fail_if(&zero, ZERO_STEP, span);
        // How many: the distance over the step, rounded away from zero, worked out wide so
        // nothing overflows, and none when the step goes the other way.
        let (wide_first, wide_end, wide_step, distance) =
            (self.temp(), self.temp(), self.temp(), self.temp());
        self.line(&format!("{wide_first} = sext i64 {first} to i128"));
        self.line(&format!("{wide_end} = sext i64 {end} to i128"));
        self.line(&format!("{wide_step} = sext i64 {step} to i128"));
        self.line(&format!("{distance} = sub i128 {wide_end}, {wide_first}"));
        let (upward, toward, rounding, padded, steps, positive, some, count_wide) = (
            self.temp(),
            self.temp(),
            self.temp(),
            self.temp(),
            self.temp(),
            self.temp(),
            self.temp(),
            self.temp(),
        );
        self.line(&format!("{upward} = icmp sgt i128 {wide_step}, 0"));
        self.line(&format!("{toward} = select i1 {upward}, i128 -1, i128 1"));
        self.line(&format!("{rounding} = add i128 {wide_step}, {toward}"));
        self.line(&format!("{padded} = add i128 {distance}, {rounding}"));
        self.line(&format!("{steps} = sdiv i128 {padded}, {wide_step}"));
        self.line(&format!("{positive} = icmp sgt i128 {steps}, 0"));
        self.line(&format!("{some} = select i1 {positive}, i128 {steps}, i128 0"));
        self.line(&format!("{count_wide} = trunc i128 {some} to i64"));
        let too_many = self.temp();
        self.line(&format!("{too_many} = icmp sgt i128 {some}, {MOST_IN_A_RANGE}"));
        let (stop, go) = (self.label("too_many"), self.label("few_enough"));
        self.line(&format!("br i1 {too_many}, label %{stop}, label %{go}, !prof !0"));
        self.start_block(&stop);
        let message = self.literal(&format!("ranges are limited to {MOST_IN_A_RANGE} items for now"));
        self.stop_with(&message, span);
        self.start_block(&go);
        let list = self.new_list(ListItem::Int, &count_wide);
        let (at, end_loop) = self.loop_begin(&count_wide);
        let (offset, value) = (self.temp(), self.temp());
        self.line(&format!("{offset} = mul i64 {at}, {step}"));
        self.line(&format!("{value} = add i64 {first}, {offset}"));
        let place = self.item_address(&list, "i64", &at);
        self.line(&format!("store i64 {value}, ptr {place}{ITEM_ACCESS}"));
        self.loop_end(&end_loop);
        self.assign(dest, &list)
    }

    /// `reversed(tuple)`: a new list of its items, last first, each with an owner of its own.
    fn reversed_tuple(&mut self, dest: LocalId, tuple: &Operand) -> Result<(), String> {
        let (object, kind) = self.operand(tuple)?;
        let (Kind::Struct(id) | Kind::Boxed(id)) = kind else {
            return Err("`reversed` of something else".to_owned());
        };
        let Kind::List(item) = self.kind_of(dest)?.clone() else {
            return Err("`reversed` that isn't a list".to_owned());
        };
        let held = self.item_kind(item)?;
        let memory = held.memory(self.module.layouts);
        let count = self.layout_of(id)?.fields.len();
        let list = self.new_list(item, &count.to_string());
        let from = self.fields_at(&object, &kind);
        for at in 0..count {
            let (source_at, _) = self.field(&kind, &format!("_{}", count - 1 - at))?;
            let source_at = self.offset(&from, source_at);
            let value = if held.in_memory() { source_at } else { self.load_field(&held, &source_at, "") };
            if held.counted() {
                self.retain_value(&value, &held);
            }
            let place = self.item_address(&list, &memory, &at.to_string());
            self.store_field(&held, &value, &place, ITEM_ACCESS);
            self.retain_inline(&held, &place);
        }
        self.assign(dest, &list)
    }

    /// `reversed(list)`: a copy of the list, its items then swapped end for end where they lie,
    /// so no owner changes.
    fn reversed(&mut self, dest: LocalId, list: &Operand) -> Result<(), String> {
        let (list, kind) = self.operand(list)?;
        let Kind::List(item) = kind else { return Err("`reversed` of something else".to_owned()) };
        let memory = self.item_kind(item)?.memory(self.module.layouts);
        self.declare("declare ptr @fennec_list_slice(ptr, i64, i64) nounwind");
        let copy = self.temp();
        let lowest = i64::MIN;
        self.line(&format!("{copy} = call ptr @fennec_list_slice(ptr {list}, i64 {lowest}, i64 {lowest})"));
        let length = self.list_length(&copy);
        let half = self.temp();
        self.line(&format!("{half} = sdiv i64 {length}, 2"));
        let (at, end_loop) = self.loop_begin(&half);
        let (last, other) = (self.temp(), self.temp());
        self.line(&format!("{last} = sub i64 {length}, 1"));
        self.line(&format!("{other} = sub i64 {last}, {at}"));
        let (here, there) =
            (self.item_address(&copy, &memory, &at), self.item_address(&copy, &memory, &other));
        let (front, back) = (self.temp(), self.temp());
        self.line(&format!("{front} = load {memory}, ptr {here}"));
        self.line(&format!("{back} = load {memory}, ptr {there}"));
        self.line(&format!("store {memory} {back}, ptr {here}"));
        self.line(&format!("store {memory} {front}, ptr {there}"));
        self.loop_end(&end_loop);
        self.assign(dest, &copy)
    }
}
