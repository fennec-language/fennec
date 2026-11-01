//! std's random numbers, drawn by position as the interpreter draws them: `SplitMix64` gives a
//! seed's `n`th number, and Lemire's method narrows it to a range without bias.

use fennec_ir::{LocalId, Operand};
use fennec_syntax::Span;

use crate::emit::Emit;
use crate::kinds::Kind;

const GOLDEN: i64 = 0x9E37_79B9_7F4A_7C15_u64.cast_signed();
const FIRST: i64 = 0xBF58_476D_1CE4_E5B9_u64.cast_signed();
const SECOND: i64 = 0x94D0_49BB_1331_11EB_u64.cast_signed();

impl Emit<'_, '_, '_> {
    fn random_helpers(&mut self) {
        self.module.add_helper(
            "fen_splitmix",
            format!(
                "define internal i64 @fen_splitmix(i64 %seed, i64 %n) #0 {{\nentry:\n  %next = add i64 %n, 1\n  %step = mul i64 %next, {GOLDEN}\n  %z0 = add i64 %seed, %step\n  %s0 = lshr i64 %z0, 30\n  %x0 = xor i64 %z0, %s0\n  %z1 = mul i64 %x0, {FIRST}\n  %s1 = lshr i64 %z1, 27\n  %x1 = xor i64 %z1, %s1\n  %z2 = mul i64 %x1, {SECOND}\n  %s2 = lshr i64 %z2, 31\n  %z3 = xor i64 %z2, %s2\n  ret i64 %z3\n}}\n\n"
            ),
        );
        // A number below `count`, 0 meaning the whole range, and how many were drawn after it.
        self.module.add_helper(
            "fen_below",
            "define internal { i64, i64 } @fen_below(i64 %seed, i64 %drawn, i64 %count) #0 {\nentry:\n  %whole = icmp eq i64 %count, 0\n  %negated = sub i64 0, %count\n  %safe = select i1 %whole, i64 1, i64 %count\n  %rest = urem i64 %negated, %safe\n  %threshold = select i1 %whole, i64 0, i64 %rest\n  %wide_count = zext i64 %count to i128\n  br label %draw\ndraw:\n  %at = phi i64 [ %drawn, %entry ], [ %after, %again ]\n  %bits = call i64 @fen_splitmix(i64 %seed, i64 %at)\n  %last = icmp eq i64 %at, 9223372036854775807\n  %stepped = add i64 %at, 1\n  %after = select i1 %last, i64 %at, i64 %stepped\n  br i1 %whole, label %all, label %narrow\nall:\n  %a0 = insertvalue { i64, i64 } poison, i64 %bits, 0\n  %a1 = insertvalue { i64, i64 } %a0, i64 %after, 1\n  ret { i64, i64 } %a1\nnarrow:\n  %wide_bits = zext i64 %bits to i128\n  %wide = mul i128 %wide_bits, %wide_count\n  %low = trunc i128 %wide to i64\n  %fair = icmp uge i64 %low, %threshold\n  br i1 %fair, label %done, label %again\nagain:\n  br label %draw\ndone:\n  %high_wide = lshr i128 %wide, 64\n  %high = trunc i128 %high_wide to i64\n  %d0 = insertvalue { i64, i64 } poison, i64 %high, 0\n  %d1 = insertvalue { i64, i64 } %d0, i64 %after, 1\n  ret { i64, i64 } %d1\n}\n\n".to_owned(),
        );
    }

    /// Writes a pair into the tuple local `dest`.
    fn pair_into(&mut self, dest: LocalId, first: &str, second: &str) -> Result<(), String> {
        let kind = self.kind_of(dest)?.clone();
        let (first_at, first_kind) = self.field(&kind, "_0")?;
        let (second_at, _) = self.field(&kind, "_1")?;
        let slot = self.slot(dest);
        let (a, b) = (self.offset(&slot, first_at), self.offset(&slot, second_at));
        self.store_field(&first_kind, first, &a, "");
        self.store_field(&Kind::Int, second, &b, "");
        Ok(())
    }

    pub(crate) fn random_builtin(
        &mut self,
        dest: Option<LocalId>,
        name: &str,
        args: &[Operand],
        span: Span,
    ) -> Result<(), String> {
        self.random_helpers();
        let dest = dest.ok_or("no result")?;
        let mut ints = Vec::with_capacity(args.len());
        for arg in args {
            ints.push(self.operand(arg)?.0);
        }
        match (name, ints.as_slice()) {
            ("random.random_int", [seed, drawn, low, high]) => {
                let backwards = self.temp();
                self.line(&format!("{backwards} = icmp sgt i64 {low}, {high}"));
                let (bad, ok) = (self.label("backwards"), self.label("ranged"));
                self.line(&format!("br i1 {backwards}, label %{bad}, label %{ok}, !prof !0"));
                self.start_block(&bad);
                let message = self.pick_message(low, high);
                self.stop_with(&message, span);
                self.start_block(&ok);
                let (span_of, count, drawn_pair, offset, after, value) =
                    (self.temp(), self.temp(), self.temp(), self.temp(), self.temp(), self.temp());
                self.line(&format!("{span_of} = sub i64 {high}, {low}"));
                self.line(&format!("{count} = add i64 {span_of}, 1"));
                self.line(&format!(
                    "{drawn_pair} = call {{ i64, i64 }} @fen_below(i64 {seed}, i64 {drawn}, i64 {count})"
                ));
                self.line(&format!("{offset} = extractvalue {{ i64, i64 }} {drawn_pair}, 0"));
                self.line(&format!("{after} = extractvalue {{ i64, i64 }} {drawn_pair}, 1"));
                self.line(&format!("{value} = add i64 {low}, {offset}"));
                self.pair_into(dest, &value, &after)
            }
            ("random.random_float", [seed, drawn]) => {
                let (bits, top, wide, fraction, last, stepped, after) = (
                    self.temp(),
                    self.temp(),
                    self.temp(),
                    self.temp(),
                    self.temp(),
                    self.temp(),
                    self.temp(),
                );
                self.line(&format!("{bits} = call i64 @fen_splitmix(i64 {seed}, i64 {drawn})"));
                self.line(&format!("{top} = lshr i64 {bits}, 11"));
                self.line(&format!("{wide} = uitofp i64 {top} to double"));
                // 2 to the 53rd, exactly.
                self.line(&format!("{fraction} = fdiv double {wide}, 0x4340000000000000"));
                self.line(&format!("{last} = icmp eq i64 {drawn}, {}", i64::MAX));
                self.line(&format!("{stepped} = add i64 {drawn}, 1"));
                self.line(&format!("{after} = select i1 {last}, i64 {drawn}, i64 {stepped}"));
                self.pair_into(dest, &fraction, &after)
            }
            _ => Err(format!("it calls `{name}`, which this backend doesn't take yet")),
        }
    }

    /// "can't pick a number from LOW to HIGH: `low` is bigger than `high`", as the interpreter
    /// says it.
    fn pick_message(&mut self, low: &str, high: &str) -> String {
        self.declare("declare ptr @fennec_int_text(i64) nounwind");
        self.declare("declare ptr @fennec_str_concat(ptr, ptr) nounwind");
        let mut message = self.literal("can't pick a number from ");
        for (part, is_number) in
            [(low, true), (" to ", false), (high, true), (": `low` is bigger than `high`", false)]
        {
            let piece = if is_number {
                let text = self.temp();
                self.line(&format!("{text} = call ptr @fennec_int_text(i64 {part})"));
                text
            } else {
                self.literal(part)
            };
            let joined = self.temp();
            self.line(&format!("{joined} = call ptr @fennec_str_concat(ptr {message}, ptr {piece})"));
            message = joined;
        }
        message
    }
}
