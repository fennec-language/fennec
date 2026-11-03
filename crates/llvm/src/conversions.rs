//! Conversions and small builtins: floats turned into ints, `PI`, `range` and `reversed` as
//! lists, a dict's or set's keys as a list, and new sets.

use fennec_interp::embed::checks::{NOT_AN_INT, ZERO_STEP};
use fennec_ir::{LocalId, Operand};
use fennec_layout::ListItem;
use fennec_syntax::Span;

use crate::emit::{Emit, ITEM_ACCESS, float_text};
use crate::kinds::Kind;

/// The most items `range` makes a list of, as the interpreter allows.
const MOST_IN_A_RANGE: i64 = 10_000_000;

/// Where `int()` stops: a float at or past 2^63 either way isn't an int.
const INT_LIMIT: f64 = 9_223_372_036_854_775_808.0;

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
            // A dict's keys, or a set's items, in their order.
            ("list", [dict], [Some(Kind::Dict(..))]) => {
                dest.and_then(|dest| self.dict_list(dest, dict, "keys"))
            }
            ("set", [], []) => dest.and_then(|dest| self.empty_set(dest)),
            // A set of a set's items is the set, shared until one changes.
            ("set", [set], [Some(Kind::Dict(..))]) => dest.and_then(|dest| self.copy(dest, set)),
            _ => return None,
        })
    }

    /// One of LLVM's own functions on a float.
    fn float_call(&mut self, function: &str, value: &str) -> String {
        self.declare(&format!("declare double @llvm.{function}.f64(double)"));
        let result = self.temp();
        self.line(&format!("{result} = call double @llvm.{function}.f64(double {value})"));
        result
    }

    /// A float as an int, rounded by `rounding` first, or toward zero: one outside an int's
    /// range, or not a number, fails as the interpreter's does.
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
