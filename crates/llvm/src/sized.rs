//! The sized numbers that don't sit in an `int`'s or a `float`'s word as one: a `u64`, its bits
//! read as unsigned, and an `f32`, held as the `f64` that's exactly its value and rounded to an
//! `f32` after each operation. That rounding gives exactly what `f32` arithmetic does for `+`,
//! `-`, `*`, `/` and square roots, as an `f64` has more than twice an `f32`'s digits.

use fennec_ir::Operand;
use fennec_syntax::{BinOp, Span};

use crate::emit::{DIVISION_BY_ZERO, Emit};

impl Emit<'_, '_, '_> {
    /// A sized integer's `wrapping_add`, `wrapping_sub` or `wrapping_mul`, or its `saturating_` forms,
    /// done natively, when `name` is one this does: `None` for any other call. The standard library writes each
    /// in Fennec (`sized.fen`), which a backend gives the same answers as. Wrapping is the operation
    /// on the word, cut to the size's bits after, as the low bits of a sum or product are the same
    /// whatever the sign. A saturating `*` below 64 bits is the whole product, which fits a word
    /// (unsigned, for a `u32`'s), held to the size's ends. A `u64`'s saturating `+` and `-` are LLVM's
    /// `*.sat`, and its `*` stops at its top when the product overflows. A saturating `+` or `-` below
    /// 64 bits stays the standard library's.
    pub(crate) fn sized_arithmetic(
        &mut self,
        dest: fennec_ir::LocalId,
        name: &str,
        (a, b): (&Operand, &Operand),
    ) -> Option<Result<(), String>> {
        let (size, method) = name.split_once('.')?;
        let num = fennec_check::Num::named(size).filter(|num| *num != fennec_check::Num::F32)?;
        let (saturating, op) = match method.split_once('_')? {
            ("wrapping", op) => (false, op),
            ("saturating", op) => (true, op),
            _ => return None,
        };
        let op = match op {
            "add" => "add",
            "sub" => "sub",
            "mul" => "mul",
            _ => return None,
        };
        // A saturating `+` or `-` below 64 bits is left to the standard library's Fennec, as two
        // compares on the whole sum: LLVM drops an end the operands can't reach there, and fuses
        // one step into the next, which it doesn't for the same test written here (measured: an
        // `i16`'s +25% to +44% by instructions done here).
        if saturating && op != "mul" && num.bits() < 64 {
            return None;
        }
        Some(self.sized_arithmetic_on(dest, (num, saturating, op), (a, b)))
    }

    fn sized_arithmetic_on(
        &mut self,
        dest: fennec_ir::LocalId,
        (num, saturating, op): (fennec_check::Num, bool, &str),
        (a, b): (&Operand, &Operand),
    ) -> Result<(), String> {
        let ((a, _), (b, _)) = (self.operand(a)?, self.operand(b)?);
        let bits = num.bits();
        let value = if !saturating {
            let whole = self.temp();
            self.line(&format!("{whole} = {op} i64 {a}, {b}"));
            self.within_width(&whole, num)
        } else if op == "mul" && bits < 64 && num.is_unsigned() {
            // Each side is below 2^32, so their product is below 2^64: whole as an unsigned word,
            // held to the top.
            let (_, most) = num.range().ok_or("a size with no range")?;
            let (whole, held) = (self.temp(), self.temp());
            self.line(&format!("{whole} = mul i64 {a}, {b}"));
            self.declare("declare i64 @llvm.umin.i64(i64, i64)");
            self.line(&format!("{held} = call i64 @llvm.umin.i64(i64 {whole}, i64 {most})"));
            held
        } else if op == "mul" && bits < 64 {
            // Each side is at least -2^31 and below 2^31, so their product fits a signed word, and
            // is held to the ends.
            let (least, most) = num.range().ok_or("a size with no range")?;
            self.held_to(&format!("mul i64 {a}, {b}"), (least, most))
        } else if op == "mul" {
            // A `u64`, its top when the product doesn't fit.
            self.declare("declare { i64, i1 } @llvm.umul.with.overflow.i64(i64, i64)");
            let (both, product, over, value) = (self.temp(), self.temp(), self.temp(), self.temp());
            self.line(&format!("{both} = call {{ i64, i1 }} @llvm.umul.with.overflow.i64(i64 {a}, i64 {b})"));
            self.line(&format!("{product} = extractvalue {{ i64, i1 }} {both}, 0"));
            self.line(&format!("{over} = extractvalue {{ i64, i1 }} {both}, 1"));
            self.line(&format!("{value} = select i1 {over}, i64 -1, i64 {product}"));
            value
        } else {
            // A `u64`'s `+` or `-`, which fills its word.
            let intrinsic = format!("llvm.u{op}.sat.i64");
            self.declare(&format!("declare i64 @{intrinsic}(i64, i64)"));
            let held = self.temp();
            self.line(&format!("{held} = call i64 @{intrinsic}(i64 {a}, i64 {b})"));
            held
        };
        self.assign(dest, &value)
    }

    /// `whole`, an instruction giving a word, held to `least` and `most`: the least when it's below
    /// it, the most when it's past it, as `sized.fen`'s saturating methods test it.
    fn held_to(&mut self, whole: &str, (least, most): (i128, i128)) -> String {
        let (value, below, above, low, held) =
            (self.temp(), self.temp(), self.temp(), self.temp(), self.temp());
        self.line(&format!("{value} = {whole}"));
        self.line(&format!("{below} = icmp slt i64 {value}, {least}"));
        self.line(&format!("{low} = select i1 {below}, i64 {least}, i64 {value}"));
        self.line(&format!("{above} = icmp sgt i64 {value}, {most}"));
        self.line(&format!("{held} = select i1 {above}, i64 {most}, i64 {low}"));
        held
    }

    /// An operator on two `u64`s: unsigned arithmetic, each checked, and unsigned comparisons.
    pub(crate) fn u64_binary(
        &mut self,
        op: BinOp,
        (a, b): (&str, &str),
        rhs: &Operand,
        span: Span,
    ) -> Result<String, String> {
        let compare = match op {
            BinOp::Eq => Some("eq"),
            BinOp::NotEq => Some("ne"),
            BinOp::Lt => Some("ult"),
            BinOp::LtEq => Some("ule"),
            BinOp::Gt => Some("ugt"),
            BinOp::GtEq => Some("uge"),
            _ => None,
        };
        let temp = self.temp();
        if let Some(cc) = compare {
            self.line(&format!("{temp} = icmp {cc} i64 {a}, {b}"));
            return Ok(temp);
        }
        let checked = |name: &str| format!("@llvm.{name}.with.overflow.i64");
        match op {
            BinOp::Add | BinOp::Sub | BinOp::Mul => {
                let intrinsic = checked(match op {
                    BinOp::Add => "uadd",
                    BinOp::Sub => "usub",
                    _ => "umul",
                });
                self.declare(&format!("declare {{ i64, i1 }} {intrinsic}(i64, i64)"));
                let (pair, bad) = (self.temp(), self.temp());
                self.line(&format!("{pair} = call {{ i64, i1 }} {intrinsic}(i64 {a}, i64 {b})"));
                self.line(&format!("{bad} = extractvalue {{ i64, i1 }} {pair}, 1"));
                self.size_overflow(&bad, fennec_check::Num::U64, span);
                self.line(&format!("{temp} = extractvalue {{ i64, i1 }} {pair}, 0"));
            }
            BinOp::FloorDiv | BinOp::Mod => {
                self.not_by_zero(b, rhs, span);
                let word = if op == BinOp::FloorDiv { "udiv" } else { "urem" };
                self.line(&format!("{temp} = {word} i64 {a}, {b}"));
            }
            BinOp::Div => {
                self.not_by_zero(b, rhs, span);
                let (x, y) = (self.temp(), self.temp());
                self.line(&format!("{x} = uitofp i64 {a} to double"));
                self.line(&format!("{y} = uitofp i64 {b} to double"));
                self.line(&format!("{temp} = fdiv double {x}, {y}"));
            }
            BinOp::Pow => {
                self.u64_power_helper();
                let (pair, bad) = (self.temp(), self.temp());
                self.line(&format!("{pair} = call {{ i64, i1 }} @fen_u64_power(i64 {a}, i64 {b})"));
                self.line(&format!("{bad} = extractvalue {{ i64, i1 }} {pair}, 1"));
                self.size_overflow(&bad, fennec_check::Num::U64, span);
                self.line(&format!("{temp} = extractvalue {{ i64, i1 }} {pair}, 0"));
            }
            BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor => {
                let word = match op {
                    BinOp::BitAnd => "and",
                    BinOp::BitOr => "or",
                    _ => "xor",
                };
                self.line(&format!("{temp} = {word} i64 {a}, {b}"));
            }
            BinOp::ShiftLeft | BinOp::ShiftRight => {
                // A shift by 64 or more stops the program. Bits shifted off the top are lost, as on
                // every integer type: a shift is bit work, not arithmetic.
                let far = self.temp();
                self.line(&format!("{far} = icmp uge i64 {b}, 64"));
                self.fail_if(&far, fennec_rt::embed::checks::BAD_SHIFT, span);
                let word = if op == BinOp::ShiftRight { "lshr" } else { "shl" };
                self.line(&format!("{temp} = {word} i64 {a}, {b}"));
            }
            other => {
                return Err(format!(
                    "it uses `{}` on u64s, which this backend doesn't take yet",
                    other.symbol()
                ));
            }
        }
        Ok(temp)
    }

    /// Defines `@fen_u64_power(a, b)`: `a ** b` on `u64`s, by squaring, and whether it overflowed.
    /// A square overflowing counts only when more of the exponent's bits follow, as only then is
    /// it used.
    fn u64_power_helper(&mut self) {
        self.declare("declare { i64, i1 } @llvm.umul.with.overflow.i64(i64, i64)");
        self.module.add_helper(
            "fen_u64_power",
            "define internal { i64, i1 } @fen_u64_power(i64 %a, i64 %b) #0 {
entry:
  br label %head
head:
  %result = phi i64 [ 1, %entry ], [ %result_next, %squared ]
  %base = phi i64 [ %a, %entry ], [ %base_next, %squared ]
  %left = phi i64 [ %b, %entry ], [ %rest, %squared ]
  %over = phi i1 [ false, %entry ], [ %over_next, %squared ]
  %done = icmp eq i64 %left, 0
  br i1 %done, label %out, label %step
step:
  %bit = and i64 %left, 1
  %odd = icmp ne i64 %bit, 0
  %times = call { i64, i1 } @llvm.umul.with.overflow.i64(i64 %result, i64 %base)
  %times_value = extractvalue { i64, i1 } %times, 0
  %times_over = extractvalue { i64, i1 } %times, 1
  %result_next = select i1 %odd, i64 %times_value, i64 %result
  %times_bad = and i1 %odd, %times_over
  %rest = lshr i64 %left, 1
  %more = icmp ne i64 %rest, 0
  %square = call { i64, i1 } @llvm.umul.with.overflow.i64(i64 %base, i64 %base)
  %base_next = extractvalue { i64, i1 } %square, 0
  %square_over = extractvalue { i64, i1 } %square, 1
  %square_bad = and i1 %more, %square_over
  %bad = or i1 %times_bad, %square_bad
  %over_next = or i1 %over, %bad
  br label %squared
squared:
  br label %head
out:
  %made = insertvalue { i64, i1 } poison, i64 %result, 0
  %whole = insertvalue { i64, i1 } %made, i1 %over, 1
  ret { i64, i1 } %whole
}

"
            .to_owned(),
        );
    }

    /// Stops the program when an integer divisor is zero, unless it's a number written out that
    /// isn't.
    fn not_by_zero(&mut self, b: &str, rhs: &Operand, span: Span) {
        if matches!(rhs, Operand::Local(_) | Operand::Int(0)) {
            let zero = self.temp();
            self.line(&format!("{zero} = icmp eq i64 {b}, 0"));
            self.fail_if(&zero, DIVISION_BY_ZERO, span);
        }
    }

    /// An operator on two `f32`s: as on floats, from each side as an `f32`, and an arithmetic
    /// result rounded to one.
    pub(crate) fn f32_binary(
        &mut self,
        op: BinOp,
        (a, b): (&str, &str),
        rhs: &Operand,
        span: Span,
    ) -> Result<String, String> {
        let (a, b) = (self.rounded_to_f32(a), self.rounded_to_f32(b));
        let value = self.float_binary(op, (&a, &b), rhs, span)?;
        Ok(if matches!(op, BinOp::Eq | BinOp::NotEq | BinOp::Lt | BinOp::LtEq | BinOp::Gt | BinOp::GtEq) {
            value
        } else {
            self.rounded_to_f32(&value)
        })
    }

    /// A float as the nearest `f32`, held as the `f64` it is.
    pub(crate) fn rounded_to_f32(&mut self, value: &str) -> String {
        let (narrow, wide) = (self.temp(), self.temp());
        self.line(&format!("{narrow} = fptrunc double {value} to float"));
        self.line(&format!("{wide} = fpext float {narrow} to double"));
        wide
    }
}
