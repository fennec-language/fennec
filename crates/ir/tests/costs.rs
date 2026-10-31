//! What may copy or allocate, found from lowered programs.

use fennec_ir::{Cost, Finding, costs, lower};

fn found(source: &str) -> Result<Vec<Finding>, String> {
    let module = fennec_syntax::parse(source).map_err(|problem| problem.message)?;
    let checked = fennec_check::check(&module).map_err(|problems| format!("{problems:?}"))?;
    let program = lower(&module, &checked);
    if program.not_lowered.is_empty() {
        Ok(costs(&program))
    } else {
        Err(format!("{:?}", program.not_lowered))
    }
}

/// The copies found, as messages.
fn copies(source: &str) -> Result<Vec<String>, String> {
    let found = found(source)?;
    Ok(found
        .iter()
        .filter(|finding| matches!(finding.cost, Cost::Copy { .. }))
        .map(Finding::message)
        .collect())
}

/// The allocations found, as (message, loops).
fn allocations(source: &str) -> Result<Vec<(String, usize)>, String> {
    let found = found(source)?;
    let allocating = found.iter().filter(|finding| matches!(finding.cost, Cost::Allocation { .. }));
    Ok(allocating.map(|finding| (finding.message(), finding.loops)).collect())
}

#[test]
fn changing_a_value_something_else_still_reads_may_copy_it() {
    let source = "def f(n: int) -> int:\n    mut xs = [n]\n    snapshot = xs\n    xs.append(1)\n    \
                  snapshot.len() + xs.len()\n";
    assert_eq!(copies(source).unwrap(), ["may copy `xs`: snapshot still refers to it"]);
}

#[test]
fn nothing_is_copied_when_the_other_holder_is_never_read_again() {
    let source = "def f(n: int) -> int:\n    mut xs = [n]\n    snapshot = xs\n    n2 = snapshot.len()\n    \
                  xs.append(n2)\n    xs.len()\n";
    assert_eq!(copies(source).unwrap(), Vec::<String>::new());
}

#[test]
fn a_value_is_copied_once_then_has_its_own_buffer() {
    let source = "def f(n: int) -> int:\n    mut xs = [n]\n    snapshot = xs\n    xs.append(1)\n    \
                  xs.append(2)\n    snapshot.len() + xs.len()\n";
    assert_eq!(copies(source).unwrap().len(), 1);
}

#[test]
fn changing_ones_own_copy_of_a_parameter_copies_what_the_caller_holds() {
    let source =
        "def f(items: list[int]) -> int:\n    mut mine = items\n    mine.append(1)\n    mine.len()\n";
    assert_eq!(copies(source).unwrap(), ["may copy `mine`: the caller still refers to it"]);
}

#[test]
fn a_field_changed_in_place_is_named_and_a_sibling_isnt_shared() {
    let source = "struct World:\n    positions: list[int]\n    names: list[str]\n\n\
                  def f(mut world: World) -> int:\n    snapshot = world.positions\n    world.names.append(\"a\")\n    \
                  world.positions.append(1)\n    snapshot.len()\n";
    assert_eq!(copies(source).unwrap(), ["may copy `world.positions`: snapshot still refers to it"]);
}

#[test]
fn appending_to_a_field_alone_copies_nothing() {
    let source = "struct World:\n    positions: list[int]\n\n\
                  def f(mut world: World):\n    world.positions.append(1)\n    world.positions[0] = 2\n";
    assert_eq!(copies(source).unwrap(), Vec::<String>::new());
}

#[test]
fn a_loop_that_shares_and_changes_each_time_round_is_hot() {
    let source = "def f(n: int) -> int:\n    mut xs = [n]\n    mut total = 0\n    for at in range(n):\n        \
                  snapshot = xs\n        xs[0] = at\n        total += snapshot.len()\n    total\n";
    let found = found(source).unwrap();
    let copy = found.iter().find(|finding| matches!(finding.cost, Cost::Copy { .. })).unwrap();
    assert_eq!(copy.loops, 1);
    assert!(copy.is_hot());
}

#[test]
fn allocations_say_what_and_how_many_loops_theyre_in() {
    let source = "def f(n: int) -> int:\n    mut total = 0\n    for at in range(n):\n        \
                  for other in range(n):\n            xs = [at, other]\n            total += xs.len()\n    total\n";
    assert!(
        allocations(source).unwrap().contains(&("this list may allocate".to_owned(), 2)),
        "{:?}",
        allocations(source).unwrap()
    );
}

#[test]
fn hot_code_comes_first() {
    let source = "def f(n: int) -> list[int]:\n    mut out = [n]\n    for at in range(n):\n        \
                  out.append(at)\n    out\n";
    let found = allocations(source).unwrap();
    assert_eq!(found.first().map(|(_, loops)| *loops), Some(1), "{found:?}");
}

#[test]
fn numbers_and_plain_structs_cost_nothing() {
    let source = "struct Point:\n    x: float\n    y: float\n\n\
                  def f(p: Point, n: int) -> float:\n    mut total = 0.0\n    mut at = 0\n    while at < n:\n        \
                  q = Point(x=p.x + 1.0, y=p.y)\n        total += q.x\n        at += 1\n    total\n";
    assert_eq!(found(source).unwrap(), []);
}

#[test]
fn a_function_called_from_a_loop_is_hot_however_far_away() {
    let source = "def leaf(n: int) -> int:\n    xs = [n]\n    xs.len()\n\ndef middle(n: int) -> int:\n    leaf(n)\n\n\
                  def f(n: int) -> int:\n    mut total = 0\n    for at in range(n):\n        total += middle(at)\n    total\n";
    let found = found(source).unwrap();
    let leaf = found.iter().find(|finding| finding.function == "leaf").unwrap();
    assert_eq!(leaf.loops, 0);
    assert_eq!(leaf.called_from_loop.as_deref(), Some("f"));
}

#[test]
fn every_example_can_be_reported_on() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples");
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|extension| extension == "fen") {
            let Ok(module) = fennec_syntax::parse(&std::fs::read_to_string(&path).unwrap()) else { continue };
            let Ok(checked) = fennec_check::check(&module) else { continue };
            for finding in costs(&lower(&module, &checked)) {
                assert!(!finding.span.in_prelude(), "{}: {finding:?}", path.display());
            }
        }
    }
}
