//! `fennec test`: runs `test` blocks. A property test (`test "..." for items: list[int]:`)
//! runs against generated inputs, and a failure is shrunk to the smallest input that still
//! fails. Inputs come from a seed derived from the test's name, so a failure replays exactly.

use fennec_syntax::{Diagnostic, TestDef, TypeExpr};

use crate::call::CallArg;
use crate::exec::Flow;
use crate::format;
use crate::interp::{Interp, TypeKind};
use crate::scope::Frame;
use crate::value::Value;

/// How many inputs a property test tries.
const CASES: usize = 100;

/// How many simpler inputs shrinking tries before settling.
const SHRINK_ATTEMPTS: usize = 200;

/// One test's result.
pub struct TestOutcome {
    pub name: String,
    pub failure: Option<Diagnostic>,
    /// How many inputs ran: 1 for a plain test.
    pub cases: usize,
}

type Inputs = Vec<(String, Value)>;

impl Interp<'_> {
    pub fn run_tests(&mut self) -> Vec<TestOutcome> {
        let tests = self.tests.clone();
        tests.iter().map(|test| self.run_test(test)).collect()
    }

    fn run_test(&mut self, test: &TestDef) -> TestOutcome {
        let name = test.name.clone();
        self.refill_steps();
        if test.inputs.is_empty() {
            return TestOutcome { name, failure: self.run_case(test, Vec::new()).err(), cases: 1 };
        }
        let mut rng = Rng::seeded(&test.name);
        for case in 1..=CASES {
            let inputs = match self.generate_inputs(test, &mut rng) {
                Ok(inputs) => inputs,
                Err(failure) => return TestOutcome { name, failure: Some(failure), cases: case },
            };
            if let Err(failure) = self.run_case(test, inputs.clone()) {
                let (inputs, failure) = self.shrink(test, inputs, failure);
                let described: Vec<String> =
                    inputs.iter().map(|(name, value)| format!("{name} = {}", format::repr(value))).collect();
                let help = match failure.help {
                    Some(help) => format!("it failed for {}; {help}", described.join(", ")),
                    None => format!("it failed for {}", described.join(", ")),
                };
                return TestOutcome {
                    name,
                    failure: Some(Diagnostic { help: Some(help), ..failure }),
                    cases: case,
                };
            }
        }
        TestOutcome { name, failure: None, cases: CASES }
    }

    fn run_case(&mut self, test: &TestDef, inputs: Inputs) -> Result<(), Diagnostic> {
        let caller = std::mem::replace(&mut self.frame, Frame::new());
        for (name, value) in inputs {
            self.frame.declare(&name, value, false);
        }
        let flow = self.exec_stmts(&test.body);
        self.frame = caller;
        match flow? {
            Flow::PassUp(Value::Err(error)) | Flow::Return(Value::Err(error)) => {
                Err(Diagnostic::new(format!("the test failed with {}", format::repr(&error)), test.span))
            }
            _ => Ok(()),
        }
    }

    /// Looks for simpler inputs that still fail, one change at a time.
    fn shrink(
        &mut self,
        test: &TestDef,
        mut inputs: Inputs,
        mut failure: Diagnostic,
    ) -> (Inputs, Diagnostic) {
        let mut attempts = 0;
        'improving: while attempts < SHRINK_ATTEMPTS {
            // Once the step budget is spent every input fails, so shrinking would only mislead.
            if self.steps_left == Some(0) {
                break;
            }
            for at in 0..inputs.len() {
                let Some((_, value)) = inputs.get(at) else { continue };
                for candidate in simpler(value) {
                    attempts += 1;
                    let mut tried = inputs.clone();
                    if let Some(slot) = tried.get_mut(at) {
                        slot.1 = candidate;
                    }
                    if let Err(smaller) = self.run_case(test, tried.clone()) {
                        inputs = tried;
                        failure = smaller;
                        continue 'improving;
                    }
                }
            }
            break;
        }
        (inputs, failure)
    }

    fn generate_inputs(&mut self, test: &TestDef, rng: &mut Rng) -> Result<Inputs, Diagnostic> {
        test.inputs
            .iter()
            .map(|param| {
                let ty = param
                    .ty
                    .as_ref()
                    .ok_or_else(|| Diagnostic::new("test inputs need a type", param.span))?;
                Ok((param.name.clone(), self.generate(ty, rng, 0)?))
            })
            .collect()
    }

    /// A random value of a type, for a property test.
    fn generate(&mut self, ty: &TypeExpr, rng: &mut Rng, depth: usize) -> Result<Value, Diagnostic> {
        if ty.optional && rng.below(5) == 0 {
            return Ok(Value::None);
        }
        let length = |rng: &mut Rng| if depth > 2 { 0 } else { rng.below(8) };
        Ok(match (ty.name.as_str(), ty.args.as_slice()) {
            ("int" | "i8" | "i16" | "i32" | "i64" | "u8" | "u16" | "u32" | "u64" | "usize", []) => {
                Value::Int(rng.int())
            }
            ("float" | "f32" | "f64", []) => Value::Float(crate::ops::to_float(rng.int()) / 8.0),
            ("bool", []) => Value::Bool(rng.below(2) == 0),
            ("str", []) => {
                let len = length(rng);
                Value::str(
                    &(0..len)
                        .map(|_| char::from(b'a' + u8::try_from(rng.below(26)).unwrap_or(0)))
                        .collect::<String>(),
                )
            }
            ("list", [item]) => {
                let len = length(rng);
                Value::list((0..len).map(|_| self.generate(item, rng, depth + 1)).collect::<Result<_, _>>()?)
            }
            ("tuple", items) => Value::tuple(
                items.iter().map(|item| self.generate(item, rng, depth + 1)).collect::<Result<_, _>>()?,
            ),
            (name, []) if self.types.contains_key(name) => self.generate_instance(name, rng, depth, ty)?,
            (name, _) => {
                return Err(Diagnostic::new(
                    format!("property tests can't generate a `{name}` yet"),
                    ty.span,
                )
                .with_help("use int, float, bool, str, list, tuples, or your own structs and enums"));
            }
        })
    }

    fn generate_instance(
        &mut self,
        name: &str,
        rng: &mut Rng,
        depth: usize,
        ty: &TypeExpr,
    ) -> Result<Value, Diagnostic> {
        let info =
            self.type_info(name).ok_or_else(|| Diagnostic::new(format!("unknown type `{name}`"), ty.span))?;
        for _ in 0..20 {
            let (case, fields) = if info.kind == TypeKind::Enum {
                let at = rng.below(info.def.cases.len().max(1));
                let case = info
                    .def
                    .cases
                    .get(at)
                    .ok_or_else(|| Diagnostic::new(format!("`{name}` has no cases"), ty.span))?;
                (Some(case.name.clone()), case.fields.clone())
            } else {
                (None, info.def.fields.clone())
            };
            let mut args = Vec::new();
            for field in &fields {
                if let Some(field_ty) = &field.ty {
                    let value = self.generate(field_ty, rng, depth + 1)?;
                    args.push(CallArg { name: Some(field.name.clone()), ..CallArg::plain(value, ty.span) });
                }
            }
            let built = match case {
                Some(case) => self.construct_variant(Some(name), &case, args, ty.span)?,
                None => self.construct(name, args, ty.span)?,
            };
            match built {
                Value::Ok(value) => return Ok(value.as_ref().clone()),
                Value::Err(_) => {}
                value => return Ok(value),
            }
        }
        Err(Diagnostic::new(format!("couldn't generate a `{name}` that follows its `where` rules"), ty.span))
    }
}

/// Simpler versions of a value to try when shrinking a failure.
fn simpler(value: &Value) -> Vec<Value> {
    match value {
        Value::Int(0) | Value::Bool(false) => Vec::new(),
        Value::Int(n) => vec![Value::Int(0), Value::Int(n / 2), Value::Int(n - n.signum())],
        Value::Float(x) if *x == 0.0 => Vec::new(),
        Value::Float(x) => vec![Value::Float(0.0), Value::Float(x.trunc())],
        Value::Bool(true) => vec![Value::Bool(false)],
        Value::Str(text) if text.is_empty() => Vec::new(),
        Value::Str(text) => {
            let shorter: String = text.chars().take(text.chars().count() - 1).collect();
            vec![Value::str(""), Value::str(&shorter)]
        }
        Value::List(items) if items.is_empty() => Vec::new(),
        Value::List(items) => {
            let mut options = vec![Value::list(Vec::new())];
            for skip in 0..items.len() {
                options.push(Value::list(
                    items
                        .iter()
                        .enumerate()
                        .filter(|(at, _)| *at != skip)
                        .map(|(_, item)| item.clone())
                        .collect(),
                ));
            }
            options
        }
        _ => Vec::new(),
    }
}

/// A small deterministic random number generator (xorshift), seeded from the test's name.
struct Rng(u64);

impl Rng {
    fn seeded(name: &str) -> Self {
        let seed = name.bytes().fold(0x9e37_79b9_7f4a_7c15_u64, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
        });
        Rng(seed | 1)
    }

    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, limit: usize) -> usize {
        let limit = u64::try_from(limit.max(1)).unwrap_or(u64::MAX);
        usize::try_from(self.next() % limit).unwrap_or(0)
    }

    /// A small int, often an edge case like 0, 1 or -1.
    fn int(&mut self) -> i64 {
        match self.below(10) {
            0 => 0,
            1 => 1,
            2 => -1,
            _ => i64::try_from(self.below(2001)).unwrap_or(0) - 1000,
        }
    }
}
