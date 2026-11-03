//! `parallel` loops lowered for a backend that runs them on threads: each body lifted into a
//! function of its own, only when the backend asks.

use fennec_ir::{Function, InstKind, Operand, Options, Program, lower, lower_with, validate};

fn lowered(source: &str, options: Options) -> Result<Program, String> {
    let mut module = fennec_syntax::parse(source).map_err(|problem| problem.message)?;
    let checked = fennec_check::check_and_resolve(&mut module).map_err(|problems| format!("{problems:?}"))?;
    let program = lower_with(&module, &checked, options);
    validate(&program).map_err(|problems| format!("{problems:?}"))?;
    Ok(program)
}

const OUTLINED: Options = Options { outline_parallel: true };

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
    let [body] = lifted(&program)[..] else { panic!("one body is lifted: {:?}", program.functions) };
    // What it captures comes first, then the item.
    assert_eq!(body.params, 3);
    let names: Vec<_> = body.locals.iter().take(3).map(|local| local.name.as_deref()).collect();
    assert_eq!(names, [Some("stdout"), Some("by"), Some("x")]);
    assert_eq!(body.mut_params.len(), 0);
    // The loop is a call of `parallel.for` with the list and a closure of the body.
    let scale = function_named(&program, "scale").unwrap();
    let calls = parallel_calls(scale);
    assert_eq!(calls.len(), 1, "{scale}");
    let (args, changes) = &calls[0];
    assert_eq!(args.first(), Some(&Operand::Local(fennec_ir::LocalId(1))));
    assert!(!changes);
    let closure =
        scale.blocks.iter().flat_map(|block| &block.insts).any(
            |inst| matches!(&inst.kind, InstKind::MakeClosure { function, .. } if *function == body.name),
        );
    assert!(closure, "{scale}");
}

#[test]
fn a_comprehension_and_a_sum_over_parallel_lift_what_they_work_out_for_each_item() {
    let source = "\
def squares(xs: list[int]) -> list[int]:
    [x * x for x in parallel(xs)]

def total(xs: list[float], by: float) -> float:
    sum(x * by for x in parallel(xs))

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
    let sum = lifted(&program).into_iter().find(|body| body.name.starts_with("total.")).unwrap();
    assert_eq!(sum.returns, fennec_check::Type::Float);
}

#[test]
fn a_for_mut_body_changes_its_item_and_the_loop_changes_the_list() {
    let source = "\
def grow(mut xs: list[int], mut ys: list[int], by: int):
    for mut x in parallel(xs):
        x += by
    for mut y in ys:
        y += by

def main(stdout: Stdout):
    mut xs = [1, 2]
    mut ys = [3]
    grow(mut xs, mut ys, 3)
    stdout.print(xs, ys)
";
    let program = lowered(source, OUTLINED).unwrap();
    // Only the loop written over `parallel(xs)` is lifted, though both loop over a list now.
    let [body] = lifted(&program)[..] else { panic!("one body is lifted") };
    assert_eq!(body.mut_params, [1]);
    let calls = parallel_calls(function_named(&program, "grow").unwrap());
    assert_eq!(calls.len(), 1);
    assert!(calls[0].1);
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
