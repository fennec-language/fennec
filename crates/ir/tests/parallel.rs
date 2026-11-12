//! `parallel` loops lowered for a backend that runs them on threads: the work for a range of
//! positions lifted into a function of its own, only when the backend asks.

use fennec_ir::{Function, InstKind, Operand, Options, Program, lower, lower_with, validate};

fn lowered(source: &str, options: Options) -> Result<Program, String> {
    let mut module = fennec_syntax::parse(source).map_err(|problem| problem.message)?;
    let checked = fennec_check::check_and_resolve(&mut module).map_err(|problems| format!("{problems:?}"))?;
    let program = lower_with(&module, &checked, options);
    validate(&program).map_err(|problems| format!("{problems:?}"))?;
    Ok(program)
}

const OUTLINED: Options =
    Options { outline_parallel: true, stop_on_failed_main: false, serve_in_order: false };

fn function_named<'p>(program: &'p Program, name: &str) -> Option<&'p Function> {
    program.functions.iter().find(|function| function.name == name)
}

/// The `parallel.for` calls in a function: their arguments and whether they change the list.
fn parallel_calls(function: &Function) -> Vec<(Vec<Operand>, bool)> {
    let insts = function.blocks.iter().flat_map(|block| &block.insts);
    insts
        .filter_map(|inst| match &inst.kind {
            InstKind::Builtin { name, args, changes, .. } if name == "parallel.for" => {
                Some((args.clone(), *changes))
            }
            _ => None,
        })
        .collect()
}

/// The builtins a function calls, by name, in order.
fn builtins(function: &Function) -> Vec<String> {
    let insts = function.blocks.iter().flat_map(|block| &block.insts);
    insts
        .filter_map(|inst| match &inst.kind {
            InstKind::Builtin { name, .. } => Some(name.clone()),
            _ => None,
        })
        .collect()
}

fn lifted(program: &Program) -> Vec<&Function> {
    program.functions.iter().filter(|function| function.name.contains(".parallel@")).collect()
}

const SCALE: &str = "\
def scale(stdout: Stdout, xs: list[float], by: float):
    for x in parallel(xs):
        stdout.print(x * by)

def main(stdout: Stdout):
    scale(stdout, [1.0, 2.0], 3.0)
";

#[test]
fn a_parallel_loop_s_body_is_lifted_into_a_function_of_its_own() {
    let program = lowered(SCALE, OUTLINED).unwrap();
    let [range] = lifted(&program)[..] else { panic!("one range is lifted: {:?}", program.functions) };
    // What its body captures comes first, then the list, then where the range starts and ends.
    assert_eq!(range.params, 5);
    let names: Vec<_> = range.locals.iter().take(5).map(|local| local.name.as_deref()).collect();
    let hidden = |name: &str| Some(format!("\0parallel.{name}"));
    let expected =
        [Some("stdout".to_owned()), Some("by".to_owned()), hidden("list"), hidden("from"), hidden("to")];
    assert_eq!(names, expected.iter().map(Option::as_deref).collect::<Vec<_>>());
    assert_eq!(range.mut_params.len(), 0);
    // It goes through its range one position at a time, as a counted loop.
    assert_eq!(range.counts.len(), 1, "{range}");
    // The loop is a call of `parallel.for` with the list and a closure of the body.
    let scale = function_named(&program, "scale").unwrap();
    let calls = parallel_calls(scale);
    assert_eq!(calls.len(), 1, "{scale}");
    let (args, changes) = &calls[0];
    assert_eq!(args.first(), Some(&Operand::Local(fennec_ir::LocalId(1))));
    assert!(!changes);
    let closure =
        scale.blocks.iter().flat_map(|block| &block.insts).any(
            |inst| matches!(&inst.kind, InstKind::MakeClosure { function, .. } if *function == range.name),
        );
    assert!(closure, "{scale}");
}

#[test]
fn a_comprehension_and_a_sum_over_parallel_lift_what_they_work_out_for_each_item() {
    let source = "\
def squares(xs: list[int]) -> list[int]:
    return [x * x for x in parallel(xs)]

def total(xs: list[float], by: float) -> float:
    return sum(x * by for x in parallel(xs))

def main(stdout: Stdout):
    stdout.print(squares([1, 2]), total([1.0, 2.0], 3.0))
";
    let program = lowered(source, OUTLINED).unwrap();
    let names: Vec<_> = lifted(&program).iter().map(|body| body.name.clone()).collect();
    let shown = |name| function_named(&program, name).map(ToString::to_string).unwrap_or_default();
    assert_eq!(names.len(), 2, "{names:?}\n{}\n{}", shown("squares"), shown("total"));
    let called = |function: &str, builtin: &str| {
        let function = function_named(&program, function).unwrap();
        function.blocks.iter().flat_map(|block| &block.insts).find_map(|inst| match &inst.kind {
            InstKind::Builtin { name, args, .. } if name == builtin => Some(args.len()),
            _ => None,
        })
    };
    // A map takes the list and the closure; a sum those and its start, here `0.0`.
    assert_eq!(called("squares", "parallel.map"), Some(2));
    assert_eq!(called("total", "parallel.sum"), Some(3));
    // A map's range takes the new list too, and puts each item's result in it.
    let map = lifted(&program).into_iter().find(|range| range.name.starts_with("squares.")).unwrap();
    assert_eq!(map.params, 4);
    assert_eq!(builtins(map), ["parallel.put"]);
    // A sum's range gives back its partial.
    let sum = lifted(&program).into_iter().find(|range| range.name.starts_with("total.")).unwrap();
    assert_eq!(sum.returns, fennec_check::Type::Float);
}

#[test]
fn a_for_mut_body_changes_its_item_and_the_loop_changes_the_list() {
    let source = "\
def grow(ref xs: list[int], ref ys: list[int], by: int):
    for ref x in parallel(xs):
        x += by
    for ref y in ys:
        y += by

def main(stdout: Stdout):
    mut xs = [1, 2]
    mut ys = [3]
    grow(ref xs, ref ys, 3)
    stdout.print(xs, ys)
";
    let program = lowered(source, OUTLINED).unwrap();
    // Only the loop written over `parallel(xs)` is lifted, though both loop over a list now.
    let [range] = lifted(&program)[..] else { panic!("one range is lifted") };
    // Each changed item goes back where it came from.
    assert_eq!(builtins(range), ["parallel.put_back"]);
    let grow = function_named(&program, "grow").unwrap();
    let calls = parallel_calls(grow);
    assert_eq!(calls.len(), 1);
    assert!(calls[0].1);
    // The list is made the loop's own before the range holds it.
    assert_eq!(builtins(grow).first().map(String::as_str), Some("parallel.own"), "{grow}");
}

#[test]
fn the_list_is_worked_out_before_what_the_body_captures() {
    // `take(mut state)` changes `state`, which the body reads: as written, the list comes
    // first, so the body sees `state` as `take` left it.
    let source = "\
def take(ref state: list[int]) -> list[int]:
    state.append(1)
    return [2, 3]

def run(stdout: Stdout, ref state: list[int]):
    for x in parallel(take(ref state)):
        stdout.print(x, state)

def main(stdout: Stdout):
    mut state = [0]
    run(stdout, ref state)
";
    let program = lowered(source, OUTLINED).unwrap();
    let run = function_named(&program, "run").unwrap();
    let insts: Vec<_> = run.blocks.iter().flat_map(|block| &block.insts).collect();
    let called = insts.iter().position(|inst| matches!(&inst.kind, InstKind::Call { .. }));
    let closure = insts.iter().position(|inst| matches!(&inst.kind, InstKind::MakeClosure { .. }));
    assert!(called.is_some() && called < closure, "{run}");
}

#[test]
fn a_body_that_leaves_early_or_can_fail_stays_an_ordinary_loop() {
    for body in ["if x > 1.0:\n            break\n        stdout.print(x)", "stdout.print(x)\n        return"]
    {
        let source = format!(
            "def scale(stdout: Stdout, xs: list[float]):\n    for x in parallel(xs):\n        {body}\n\n\
             def main(stdout: Stdout):\n    scale(stdout, [1.0])\n"
        );
        let program = lowered(&source, OUTLINED).unwrap();
        assert_eq!(lifted(&program).len(), 0, "{body}");
        assert_eq!(parallel_calls(function_named(&program, "scale").unwrap()).len(), 0, "{body}");
    }
}

#[test]
fn without_the_option_nothing_is_lifted_and_lowering_is_unchanged() {
    let mut module = fennec_syntax::parse(SCALE).unwrap();
    let checked = fennec_check::check_and_resolve(&mut module).unwrap();
    let plain = lower(&module, &checked);
    assert_eq!(plain, lower_with(&module, &checked, Options::default()));
    assert_eq!(lifted(&plain).len(), 0);
    assert_eq!(parallel_calls(function_named(&plain, "scale").unwrap()).len(), 0);
}

#[test]
fn a_sum_with_an_if_adds_into_a_name_of_its_own_and_stays_an_ordinary_loop() {
    // Lowered as a loop adding into a name from outside it, which a range would only change a
    // copy of.
    let source = "\
def left(xs: list[int], above: int) -> int:
    return sum(x for x in parallel(xs) if x > above)

def main(stdout: Stdout):
    stdout.print(left([1, 5, 9], 4))
";
    let program = lowered(source, OUTLINED).unwrap();
    assert_eq!(lifted(&program).len(), 0);
    assert_eq!(parallel_calls(function_named(&program, "left").unwrap()).len(), 0);
}

#[test]
fn what_a_sum_s_item_lifts_is_lifted_once() -> Result<(), String> {
    // A `parallel` and an arrow function inside the item of a `parallel` sum.
    let source = "\
def twice(f: (int) -> int, n: int) -> int:
    return f(f(n))

def total(grids: list[list[int]]) -> int:
    return sum(sum(twice(n => n + 1, cell) for cell in parallel(row)) for row in parallel(grids))
";
    let program = lowered(source, OUTLINED)?;
    let mut names: Vec<&str> = program.functions.iter().map(|function| function.name.as_str()).collect();
    names.sort_unstable();
    let count = names.len();
    names.dedup();
    assert_eq!(names.len(), count, "a function is there twice: {names:?}");
    Ok(())
}

const STREAMS: &str = "\
def doubled(x: int) -> int:
    return x * 2

def total(xs: list[int]) -> int:
    mut sum = 0
    for y in (doubled(x) for x in parallel(xs)):
        sum += y
    return sum

def kept(xs: list[int]) -> int:
    mut sum = 0
    for y in (doubled(x) for x in parallel(xs) if x > 1):
        sum += y
    return sum

def main(stdout: Stdout):
    stdout.print(total([1, 2, 3]), kept([1, 2, 3]))
    return
";

/// `(f(x) for x in parallel(xs))` read by a loop is a stream: a generator of its own that starts
/// the runtime's stream, takes its chunks and ends it, rather than fused into the loop, which
/// would run it in order. Its range puts each item at its place in the chunk. With an `if` it
/// runs in order as before, and so does all of it when the backend doesn't lift `parallel`.
#[test]
fn a_parallel_generator_expression_is_read_as_a_stream() {
    let program = lowered(STREAMS, OUTLINED).unwrap();
    let names: Vec<&str> = program.functions.iter().map(|function| function.name.as_str()).collect();
    let resume = program
        .functions
        .iter()
        .find(|function| function.name.starts_with("total.stream@") && function.name.ends_with("resume"))
        .unwrap_or_else(|| panic!("no stream's resume function in {names:?}"));
    let called = builtins(resume);
    for wanted in ["parallel.stream", "parallel.stream_next", "parallel.stream_end"] {
        assert!(called.iter().any(|name| name == wanted), "{wanted} isn't called: {called:?}");
    }
    let range = lifted(&program).into_iter().find(|range| range.name.starts_with("total.")).unwrap();
    assert_eq!(builtins(range), ["parallel.put"]);
    assert!(!names.iter().any(|name| name.starts_with("kept.stream@")), "{names:?}");
    let unlifted = Options { outline_parallel: false, ..OUTLINED };
    let plain = lowered(STREAMS, unlifted).unwrap();
    assert!(!plain.functions.iter().any(|function| function.name.contains(".stream@")));
}
