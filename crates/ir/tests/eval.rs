//! Working constants out while compiling, through the IR: what each gives, written back as a
//! literal, and what's left for the program to work out when it starts.

use fennec_ir::eval::{Worked, Written, literal, work_out};

/// Each constant of `source`, worked out within `steps`, with its value written as a literal.
fn worked(source: &str, steps: u64) -> Vec<(String, String)> {
    let mut module = match fennec_syntax::parse(source) {
        Ok(module) => module,
        Err(problem) => return vec![("parsing".to_owned(), problem.message)],
    };
    let signatures = match fennec_check::check_and_resolve(&mut module) {
        Ok(signatures) => signatures,
        Err(problems) => return vec![("checking".to_owned(), format!("{problems:?}"))],
    };
    let program = fennec_ir::lower(&module, &signatures);
    let own: Vec<&str> = module
        .items
        .iter()
        .filter_map(|item| match item {
            fennec_syntax::Item::Struct(def) | fennec_syntax::Item::Enum(def) => Some(def.name.as_str()),
            _ => None,
        })
        .collect();
    let mut written = Written::new(&program, &own);
    work_out(&program, steps)
        .into_iter()
        .map(|(name, worked)| {
            let said = match worked {
                Worked::Value(value) => match literal(&value, &mut written) {
                    Some(expr) => shown(&expr),
                    None => "no literal".to_owned(),
                },
                Worked::TooLong => "too long".to_owned(),
                Worked::NotHere(what) => format!("not here: {what}"),
                Worked::Failed(problem) => format!("failed: {}", problem.message),
            };
            (name, said)
        })
        .collect()
}

/// A literal as it's written.
fn shown(expr: &fennec_syntax::Expr) -> String {
    use fennec_syntax::ExprKind;
    let all = |items: &[fennec_syntax::Expr]| items.iter().map(shown).collect::<Vec<_>>().join(", ");
    match &expr.kind {
        ExprKind::Int(n) => n.to_string(),
        ExprKind::Float(x) => fennec_syntax::dump::float(*x),
        ExprKind::Str(text) => format!("{text:?}"),
        ExprKind::Bool(b) => if *b { "True" } else { "False" }.to_owned(),
        ExprKind::None => "None".to_owned(),
        ExprKind::Unary { expr, .. } => format!("-{}", shown(expr)),
        ExprKind::List(items) => format!("[{}]", all(items)),
        ExprKind::Tuple(items) => format!("({})", all(items)),
        ExprKind::Set(items) => format!("{{{}}}", all(items)),
        ExprKind::Dict(pairs) => {
            let pairs: Vec<String> =
                pairs.iter().map(|(key, value)| format!("{}: {}", shown(key), shown(value))).collect();
            format!("{{{}}}", pairs.join(", "))
        }
        ExprKind::Name(name) => name.clone(),
        ExprKind::Attr { object, name, .. } => format!("{}.{name}", shown(object)),
        ExprKind::Call { callee, args } => {
            let args: Vec<String> = args
                .iter()
                .map(|arg| format!("{}={}", arg.name.clone().unwrap_or_default(), shown(&arg.value)))
                .collect();
            format!("{}({})", shown(callee), args.join(", "))
        }
        other => format!("{other:?}"),
    }
}

/// What constant `name` of `source` gave.
fn said(source: &str, name: &str, steps: u64) -> String {
    let all = worked(source, steps);
    all.iter().find(|(own, _)| own == name).map_or_else(|| format!("{all:?}"), |(_, said)| said.clone())
}

#[test]
fn collections_comprehensions_and_structs_are_worked_out() {
    let source = "from std.math import PI, sin\n\nstruct Point:\n    x: float\n    y: float\n\n\
                  SQUARES = {n: n * n for n in range(4)}\n\
                  CORNERS = [Point(x=float(at // 2), y=float(at % 2)) for at in range(2)]\n\
                  WAVE = [sin(float(i) * PI / 2.0) for i in range(2)]\n\n\
                  def main(stdout: Stdout):\n    stdout.print(SQUARES, CORNERS, WAVE)\n";
    let all = worked(source, 1_000_000);
    let said = |name: &str| all.iter().find(|(own, _)| own == name).map(|(_, said)| said.clone()).unwrap();
    assert_eq!(said("SQUARES"), "{0: 0, 1: 1, 2: 4, 3: 9}");
    assert_eq!(said("CORNERS"), "[Point(x=0.0, y=0.0), Point(x=0.0, y=1.0)]");
    assert_eq!(said("WAVE"), "[0.0, 1.0]");
}

#[test]
fn a_constant_reads_another_and_calls_the_programs_functions() {
    let source = "LIMIT = 4\nWORDS = [\"a\", \"bb\"]\nDOUBLE = twice(LIMIT)\n\
                  LENGTHS = {word: len(word) for word in WORDS}\nGREETING = f\"{LIMIT} and {DOUBLE:.1f}\"\n\n\
                  def twice(n: int) -> int:\n    mut total = 0\n    for _ in range(2):\n        total += n\n    return total\n\n\
                  def main(stdout: Stdout):\n    stdout.print(DOUBLE, LENGTHS, GREETING)\n";
    let all = worked(source, 1_000_000);
    let said = |name: &str| all.iter().find(|(own, _)| own == name).map(|(_, said)| said.clone()).unwrap();
    assert_eq!(said("DOUBLE"), "8");
    assert_eq!(said("LENGTHS"), "{\"a\": 1, \"bb\": 2}");
    assert_eq!(said("GREETING"), "\"4 and 8.0\"");
}

#[test]
fn a_failure_is_the_programs_and_too_long_is_left_for_the_start() {
    let bad = "ITEMS = [1, 2]\nBAD = ITEMS[5]\n\ndef main(stdout: Stdout):\n    stdout.print(BAD)\n";
    assert_eq!(said(bad, "BAD", 1_000_000), "failed: index 5 is out of range for length 2");
    let slow = "SLOW = sum(i for i in range(10))\n\ndef main(stdout: Stdout):\n    stdout.print(SLOW)\n";
    assert_eq!(said(slow, "SLOW", 5), "too long");
    assert_eq!(said(slow, "SLOW", 1_000), "45");
}

#[test]
fn what_it_doesnt_do_is_left_for_the_start() {
    let found = "AT = \"abc\".find(\"b\")\n\ndef main(stdout: Stdout):\n    stdout.print(AT)\n";
    assert_eq!(said(found, "AT", 1_000_000), "not here: `str.find`");
}
