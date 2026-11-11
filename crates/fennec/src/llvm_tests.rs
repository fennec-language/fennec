//! Running a program's tests through LLVM. Each test becomes a function, and those LLVM can build
//! go into one executable with what they reach, compiled a function at a time from the cache.
//! Worker processes run it, each with its share of the tests. A test a crash left unfinished runs
//! again alone, so a crash is reported as that test's. Tests LLVM can't build yet come back for
//! the caller to fail, with why. A test that fails says why, both sides of an `assert` and what a
//! failure passed up was, and a property test's input is shrunk to the simplest that fails.

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::process::Command;

use fennec_rt::{PROPERTY_CASES, TestOutcome, property_seed};
use fennec_syntax::{Diagnostic, Expr, ExprKind, FnDef, Item, Module, Span, Stmt, StmtKind, TestDef, Trace};

use crate::test_inputs::{Inputs, PREFIX, written};

/// What running tests through LLVM did.
#[derive(Default)]
pub(crate) struct LlvmTests {
    /// The tests that ran through LLVM, and how each went.
    pub(crate) outcomes: Vec<TestOutcome>,
    /// The names of the tests LLVM wouldn't build, which fail.
    pub(crate) left: Vec<String>,
    /// Why LLVM wouldn't build some of them, the first it said.
    pub(crate) refused: Option<String>,
    /// The program functions each test that passed ran, by their keys, for the tests that's known
    /// for.
    pub(crate) ran: HashMap<String, Vec<String>>,
}

/// Functions the IR may fuse into what uses them, which then has no flag of its own to set: a
/// generator, wherever it's written. They count as having run, for every test.
fn fusible(module: &Module) -> Vec<String> {
    let mut found = Vec::new();
    let mut add = |owner: Option<&str>, def: &FnDef| {
        if fennec_syntax::fuse::has_yield(&def.body) {
            found.push(owner.map_or_else(|| def.name.clone(), |owner| format!("{owner}.{}", def.name)));
        }
    };
    for item in &module.items {
        match item {
            Item::Function(def) => add(None, def),
            Item::Struct(def) | Item::Enum(def) | Item::Error(def) => {
                def.methods.iter().for_each(|method| add(Some(&def.name), method));
            }
            Item::Extend(extension) => {
                extension.methods.iter().for_each(|method| add(Some(&extension.name), method));
            }
            _ => {}
        }
    }
    found
}

/// The program functions a test ran, by their keys, from the functions that set their flags, or
/// None if one can't be put down to a definition. Then the test depends on all it can reach, since
/// a function missed here is a test skipped wrongly.
fn traced(
    flagged: &[&str],
    fused: &[String],
    defines: &dyn Fn(&str) -> bool,
    prelude: &HashSet<String>,
) -> Option<Vec<String>> {
    let mut ran: Vec<String> = fused.to_vec();
    for name in flagged {
        if name.starts_with(TEST_FUNCTION) {
            continue;
        }
        // A copy of a generic function for one type, or a function written inside another, is
        // part of the definition around it.
        let plain = name.split('[').next().unwrap_or(name);
        let mut known = plain;
        while !defines(known) {
            match known.rsplit_once('.') {
                Some((around, _)) => known = around,
                None => break,
            }
        }
        if defines(known) {
            ran.push(known.to_owned());
        } else if !prelude.contains(plain) {
            return None;
        }
    }
    ran.sort();
    ran.dedup();
    Some(ran)
}

/// What the standard library defines, whose functions Fennec's build stands for in a test's key.
fn prelude_functions() -> HashSet<String> {
    let mut names = HashSet::new();
    for library in fennec_syntax::prelude_modules().unwrap_or_default() {
        for item in &library.items {
            match item {
                Item::Function(def) => {
                    names.insert(def.name.clone());
                }
                Item::Struct(def) | Item::Enum(def) | Item::Error(def) => {
                    names.extend(def.methods.iter().map(|method| format!("{}.{}", def.name, method.name)));
                }
                Item::Extend(extension) => {
                    names.extend(
                        extension.methods.iter().map(|method| format!("{}.{}", extension.name, method.name)),
                    );
                }
                _ => {}
            }
        }
    }
    names
}

/// How each test's function is named, which no program can write.
const TEST_FUNCTION: &str = "test:";

/// The function a property test with capabilities calls before each input, `test:world`, which
/// native code makes a new world of fakes.
const NEW_WORLD: &str = "world";

/// What each test's function is named, by its number.
fn function_name(number: usize) -> String {
    format!("{TEST_FUNCTION}{number}")
}

/// A test's body as a function that takes the capabilities and inputs it names, and gives back
/// nothing.
fn as_function(name: String, test: &TestDef) -> FnDef {
    let end = Span { start: test.span.end, end: test.span.end };
    let mut body: Vec<Stmt> = test.body.clone();
    // The body's last value is not what the test gives back.
    body.push(Stmt { kind: StmtKind::Pass, span: end });
    FnDef {
        name,
        generics: Vec::new(),
        params: test.capabilities.iter().chain(&test.inputs).cloned().collect(),
        ret: None,
        arrow: None,
        fails: None,
        body,
        annotations: Vec::new(),
        span: test.span,
    }
}

/// Whether statements say `fail` or give back a value, wherever they are among other statements.
fn fails_or_gives_back(body: &[Stmt]) -> bool {
    body.iter().any(|stmt| match &stmt.kind {
        StmtKind::Fail(_) | StmtKind::Return(Some(_)) => true,
        StmtKind::If { branches, otherwise } => {
            branches.iter().any(|(_, body)| fails_or_gives_back(body))
                || otherwise.as_deref().is_some_and(fails_or_gives_back)
        }
        StmtKind::While { body, .. }
        | StmtKind::For { body, .. }
        | StmtKind::With { body, .. }
        | StmtKind::OnFailure { body, .. } => fails_or_gives_back(body),
        _ => false,
    })
}

/// Whether a test's body can pass a failure up, which makes its function give back a result. The
/// executable refuses a test function that gives back a value, so one missed here, like a `fail`
/// in a `match`, means its tests fail as ones LLVM can't build.
fn passes_failures_up(test: &TestDef) -> bool {
    let mut found = fails_or_gives_back(&test.body);
    let mut body = test.body.clone();
    fennec_syntax::visit::in_stmts(&mut body, &mut |inner| {
        // A type made with its type arguments, like `SortedMap[int, str?]()`, writes them as an
        // expression, where `str?` reads as a `?`. They're taken out before they're looked in.
        if let ExprKind::Call { callee, .. } = &mut inner.kind
            && let ExprKind::Index { object, index } = &mut callee.kind
            && matches!(object.kind, ExprKind::Name(_))
            && reads_as_types(index)
        {
            index.kind = ExprKind::None;
        }
        found |= matches!(inner.kind, ExprKind::Try { .. });
    });
    found
}

/// Whether what's in the brackets of `Name[...]` reads as type arguments, each of them a type.
fn reads_as_types(index: &Expr) -> bool {
    match &index.kind {
        ExprKind::Tuple(items) => items.iter().all(|item| item.as_type().is_some()),
        _ => index.as_type().is_some(),
    }
}

/// The function named `test:` and the rest of the name, for a name written with `PREFIX`.
fn given_real_name(name: &str) -> Option<String> {
    name.strip_prefix(PREFIX).map(|rest| format!("{TEST_FUNCTION}{rest}"))
}

/// A bench block as a function, and a `main` that runs it between two readings of the clock and
/// prints the nanoseconds between them, or given an argument, does all of that but run it. None for
/// a block that passes a failure up, which a `main` here doesn't take yet.
pub(crate) fn timing_functions(block: &TestDef) -> Option<Vec<FnDef>> {
    let tried = if passes_failures_up(block) { "?" } else { "" };
    let source = format!(
        "def {PREFIX}main(clock: Clock, args: Args, stdout: Stdout):\n    started = clock.now()\n    \
         if args.len() == 0:\n        {PREFIX}bench(){tried}\n    stdout.print((clock.now() - started).nanos)\n"
    );
    let mut made = functions_in(&source, block.span.start)?;
    if let Some(main) = made.first_mut() {
        "main".clone_into(&mut main.name);
    }
    made.push(as_function(format!("{TEST_FUNCTION}bench"), block));
    Some(made)
}

/// The functions in some source, with the names no program can write.
fn functions_in(source: &str, start: u32) -> Option<Vec<FnDef>> {
    let parsed = fennec_syntax::parse_at(source, start).ok()?;
    let mut functions = Vec::with_capacity(parsed.items.len());
    for item in parsed.items {
        let Item::Function(mut function) = item else { return None };
        function.name = given_real_name(&function.name)?;
        fennec_syntax::visit::in_stmts(&mut function.body, &mut |inner| {
            if let ExprKind::Name(named) = &inner.kind
                && let Some(real) = given_real_name(named)
            {
                inner.kind = ExprKind::Name(real);
            }
        });
        functions.push(function);
    }
    Some(functions)
}

/// The function for test `number`, which takes the capabilities the test names, and the test's
/// body in a function of its own. A test with no inputs is its body. A property test's calls its
/// body with each input in turn. A failure the body gives back stops the test, saying what failed,
/// as a failed `main` does.
fn test_functions(number: usize, test: &TestDef, inputs: &mut Inputs<'_>) -> Option<Vec<FnDef>> {
    let name = function_name(number);
    if test.inputs.is_empty() {
        return Some(vec![as_function(name, test)]);
    }
    // A test whose inputs each have their simpler values here shrinks a failing one itself.
    let simpler: Option<Vec<String>> =
        test.inputs.iter().map(|input| inputs.simpler_for(input.ty.as_ref()?)).collect();
    if let Some(simpler) = simpler {
        return shrinking_functions(number, test, inputs, &simpler);
    }
    let capabilities: Vec<&str> = test.capabilities.iter().map(|param| param.name.as_str()).collect();
    let mut args: Vec<String> = capabilities.iter().map(ToString::to_string).collect();
    let mut made = String::new();
    let indent = "        ";
    // Each input gets fakes in a world of its own.
    if !test.capabilities.is_empty() {
        let _ = writeln!(made, "{indent}{PREFIX}{NEW_WORLD}()");
    }
    for (at, input) in test.inputs.iter().enumerate() {
        let function = inputs.function_for(input.ty.as_ref()?)?;
        let _ = writeln!(made, "{indent}input{at} = {function}(ref state, 0)");
        args.push(format!("input{at}"));
    }
    let tried = if passes_failures_up(test) { "?" } else { "" };
    let call = format!("{indent}{PREFIX}{number}_body({}){tried}\n", args.join(", "));
    let start =
        format!("    mut state = {}\n    for _ in range({}):\n", property_seed(&test.name), PROPERTY_CASES);
    let source = format!("def {PREFIX}{number}():\n{start}{made}{call}");
    let mut caller = functions_in(&source, test.span.start)?;
    if let Some(caller) = caller.first_mut() {
        caller.params.clone_from(&test.capabilities);
        caller.span = test.span;
    }
    caller.push(as_function(format!("{name}_body"), test));
    Some(caller)
}

/// The functions of property test `number` that shrinks a failing input itself: the test, which
/// tries each input in turn; an attempt, which runs the body on an input and says whether it
/// passed; and the shrinking, which tries each input's simpler values, in order, keeping the first
/// that still fails, until none does or 200 have been tried. Then it runs the body on the simplest,
/// which fails as the test's failure, saying what it was given. `simpler` has each input's
/// function giving its simpler values.
fn shrinking_functions(
    number: usize,
    test: &TestDef,
    inputs: &mut Inputs<'_>,
    simpler: &[String],
) -> Option<Vec<FnDef>> {
    let typed =
        |param: &fennec_syntax::Param| Some(format!("{}: {}", param.name, written(param.ty.as_ref()?)));
    let mut params: Vec<String> = test.capabilities.iter().map(typed).collect::<Option<_>>()?;
    params.extend(
        (0..test.inputs.len())
            .map(|at| Some(format!("input{at}: {}", written(test.inputs.get(at)?.ty.as_ref()?))))
            .collect::<Option<Vec<_>>>()?,
    );
    let params = params.join(", ");
    let capabilities: Vec<String> = test.capabilities.iter().map(|param| param.name.clone()).collect();
    let passing =
        |values: Vec<String>| capabilities.iter().cloned().chain(values).collect::<Vec<_>>().join(", ");
    let named = |prefix: &str| (0..test.inputs.len()).map(|at| format!("{prefix}{at}")).collect::<Vec<_>>();
    let world = |indent: &str| {
        if test.capabilities.is_empty() { String::new() } else { format!("{indent}{PREFIX}{NEW_WORLD}()\n") }
    };
    let mut made = String::new();
    for (at, input) in test.inputs.iter().enumerate() {
        let function = inputs.function_for(input.ty.as_ref()?)?;
        let _ = writeln!(made, "        input{at} = {function}(ref state, 0)");
    }
    let given = passing(named("input"));
    // A body that passes a failure up gives it back, which counts as failing, and the last run
    // passes it up as the test's failure.
    let tried = if passes_failures_up(test) { "?" } else { "" };
    let mut source = format!(
        "def {PREFIX}{number}():\n    mut state = {}\n    for _ in range({PROPERTY_CASES}):\n{}{made}        if not {PREFIX}{number}_attempt({given}):\n            {PREFIX}{number}_shrunk({given}){tried}\n            return\n\n",
        property_seed(&test.name),
        world("        "),
    );
    let ran = if tried.is_empty() {
        format!("    {PREFIX}{number}_body({given})\n")
    } else {
        format!("    {PREFIX}{number}_body({given}) else:\n        return False\n")
    };
    let _ = write!(source, "def {PREFIX}{number}_attempt({params}) -> bool:\n{ran}    {PREFIX}passed()\n\n");
    let _ = writeln!(source, "def {PREFIX}{number}_shrunk({params}):");
    for at in 0..test.inputs.len() {
        let _ = writeln!(source, "    mut best{at} = input{at}");
    }
    source.push_str("    mut attempts = 0\n    mut improving = True\n    while improving and attempts < 200:\n        improving = False\n");
    for (at, function) in simpler.iter().enumerate() {
        let mut tried = named("best");
        if let Some(slot) = tried.get_mut(at) {
            "candidate".clone_into(slot);
        }
        let (indent, guard) = if at == 0 {
            ("        ", String::new())
        } else {
            ("            ", "        if not improving:\n".to_owned())
        };
        let _ = write!(
            source,
            "{guard}{indent}for candidate in {function}(best{at}):\n{indent}    attempts += 1\n{}{indent}    if not {PREFIX}{number}_attempt({}):\n{indent}        best{at} = candidate\n{indent}        improving = True\n{indent}        break\n",
            world(&format!("{indent}    ")),
            passing(tried),
        );
    }
    let mut described = Vec::with_capacity(test.inputs.len());
    for (at, input) in test.inputs.iter().enumerate() {
        // A value shown in a list is shown as one on its own is.
        let _ = writeln!(source, "    shown{at} = str([best{at}])");
        described.push(format!("\"{} = \" + shown{at}[1:shown{at}.len() - 1]", input.name));
    }
    let _ = write!(
        source,
        "    {PREFIX}failed_for({})\n{}    {PREFIX}{number}_body({}){tried}\n",
        described.join(" + \", \" + "),
        world("    "),
        passing(named("best")),
    );
    let mut made = functions_in(&source, test.span.start)?;
    if let Some(caller) = made.first_mut() {
        caller.params.clone_from(&test.capabilities);
        caller.span = test.span;
    }
    made.push(as_function(format!("{}_body", function_name(number)), test));
    Some(made)
}

/// `module` with only the functions of `tests` and what they reach, or None if there's a test here
/// LLVM can't be given.
fn trimmed_for(module: &Module, tests: &[&TestDef]) -> Option<Module> {
    let mut trimmed = module.clone();
    let mut inputs = Inputs::new(module);
    let mut made = Vec::new();
    for (number, test) in tests.iter().enumerate() {
        made.extend(test_functions(number, test, &mut inputs)?);
    }
    if tests.iter().any(|test| !test.inputs.is_empty()) {
        made.extend(functions_in(inputs.source(), 0)?);
    }
    if tests.iter().any(|test| !test.inputs.is_empty() && !test.capabilities.is_empty()) {
        made.extend(functions_in(&format!("def {PREFIX}{NEW_WORLD}():\n    pass\n"), 0)?);
    }
    // What shrinking calls, which native code makes calls to the runtime.
    if tests.iter().any(|test| !test.inputs.is_empty()) {
        let shrinking = format!(
            "def {PREFIX}passed() -> bool:\n    True\n\ndef {PREFIX}failed_for(given: str):\n    pass\n"
        );
        made.extend(functions_in(&shrinking, 0)?);
    }
    trimmed.items.extend(made.into_iter().map(Item::Function));
    fennec_build::keep_what_functions_reach(&mut trimmed, &|name| name.starts_with(TEST_FUNCTION));
    Some(trimmed)
}

/// Whether the tests' functions can be written: a property test takes only inputs this can make.
fn inputs_can_be_made(module: &Module, test: &TestDef) -> bool {
    let mut inputs = Inputs::new(module);
    test.inputs.iter().all(|input| input.ty.as_ref().is_some_and(|ty| inputs.function_for(ty).is_some()))
}

/// The tests LLVM has text for, from `tests`, which may mean splitting them to find the ones it
/// can't take: what one of them reaches may stop all of them. Each that can't be taken goes in
/// `refused`.
fn takeable<'t>(
    module: &Module,
    tests: &[&'t TestDef],
    refused: &mut Vec<(&'t TestDef, String)>,
) -> Vec<&'t TestDef> {
    let answer = trimmed_for(module, tests).map_or_else(
        || Err("a test's inputs aren't ready".to_owned()),
        |trimmed| crate::aot::tests_take(&trimmed),
    );
    match answer {
        Ok(()) => tests.to_vec(),
        Err(why) => match tests {
            [] => Vec::new(),
            [test] => {
                refused.push((test, why));
                Vec::new()
            }
            _ => {
                let (first, second) = tests.split_at(tests.len() / 2);
                let mut taken = takeable(module, first, refused);
                taken.extend(takeable(module, second, refused));
                taken
            }
        },
    }
}

/// Reads two hexadecimal digits at a time.
fn from_hex(text: &str) -> Option<String> {
    let bytes: Option<Vec<u8>> = text
        .as_bytes()
        .chunks(2)
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok())
        .collect();
    String::from_utf8(bytes?).ok()
}

/// What a test executable said about one test: its number, how it failed if it did with the
/// failure's call chain, and the numbers of the flags it set.
type TestLine = (usize, Option<(Diagnostic, Trace)>, Vec<usize>);

/// Reads a line a test executable wrote.
fn read_line(line: &str) -> Option<TestLine> {
    let mut words = line.split(' ');
    let number = words.next()?.parse().ok()?;
    match words.next()? {
        "ok" => {
            let ran: Option<Vec<usize>> = words
                .next()
                .unwrap_or("")
                .split(',')
                .filter(|at| !at.is_empty())
                .map(|at| at.parse().ok())
                .collect();
            Some((number, None, ran?))
        }
        "failed" => {
            let start = words.next()?.parse().ok()?;
            let end = words.next()?.parse().ok()?;
            let message = from_hex(words.next()?)?;
            let help = from_hex(words.next().unwrap_or(""))?;
            let trace = Trace::decode(words.next().unwrap_or(""))?;
            let failure = Diagnostic::new(message, Span { start, end });
            let failure = if help.is_empty() { failure } else { failure.with_help(help) };
            Some((number, Some((failure, trace)), Vec::new()))
        }
        _ => None,
    }
}

/// How a failure reads for a test, whose function nobody wrote.
fn test_failure(failure: Diagnostic) -> Diagnostic {
    match failure.message.strip_prefix("`main` failed: ") {
        Some(error) => Diagnostic { message: format!("the test failed with {error}"), ..failure },
        None => failure,
    }
}

/// A test executable to run, hashing dict keys with a fixed seed so a test runs the same every time.
fn test_command(work: &std::path::Path) -> Command {
    let mut command = Command::new(work);
    command.env("FENNEC_HASH_SEED", "0");
    command
}

/// Runs a test executable once, with real capabilities rather than fakes, for `fennec test --real`,
/// giving back how each of its `tests` tests went. What the tests print goes straight out, in order,
/// so the executable says how each went on its standard error.
fn run_for_real(work: &std::path::Path, tests: usize) -> String {
    let numbers = (0..tests).map(|number| number.to_string());
    test_command(work)
        .args(numbers)
        .env("FENNEC_TESTS_REAL", "1")
        .stdout(std::process::Stdio::inherit())
        .output()
        .map(|output| String::from_utf8_lossy(&output.stderr).into_owned())
        .unwrap_or_default()
}

/// Runs a test executable in worker processes, each with its share of `tests` tests, giving back
/// what each wrote.
fn run_workers(work: &std::path::Path, tests: usize) -> Vec<String> {
    let workers = std::thread::available_parallelism().map_or(1, usize::from).min(tests);
    let shares: Vec<Vec<String>> = (0..workers)
        .map(|worker| {
            (0..tests).filter(|number| number % workers == worker).map(|number| number.to_string()).collect()
        })
        .collect();
    std::thread::scope(|scope| {
        let running: Vec<_> = shares
            .iter()
            .map(|share| {
                scope.spawn(move || {
                    test_command(work)
                        .args(share)
                        .output()
                        .map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
                })
            })
            .collect();
        running.into_iter().filter_map(|worker| worker.join().ok()?.ok()).collect()
    })
}

/// Runs the tests of a checked `module` named in `wanted` that LLVM can take. `defines` says
/// whether the program defines a function by a name.
pub(crate) fn run(
    module: &Module,
    wanted: &[String],
    defines: &dyn Fn(&str) -> bool,
    real: bool,
) -> LlvmTests {
    let mut result = LlvmTests::default();
    let mut plain = Vec::new();
    let tests = module.items.iter().filter_map(|item| match item {
        Item::Test(test) if !test.bench && wanted.contains(&test.name) => Some(test),
        _ => None,
    });
    let tests: Vec<&TestDef> = tests.collect();
    for test in &tests {
        let unique = tests.iter().filter(|other| other.name == test.name).count() == 1;
        if unique && inputs_can_be_made(module, test) {
            plain.push(*test);
        } else {
            result.left.push(test.name.clone());
        }
    }
    let mut refused = Vec::new();
    let taken = takeable(module, &plain, &mut refused);
    for (test, why) in refused {
        result.left.push(test.name.clone());
        result.refused.get_or_insert(why);
    }
    if taken.is_empty() {
        return result;
    }
    let names: Vec<String> = (0..taken.len()).map(function_name).collect();
    let work = crate::own_temp("tests").with_extension(std::env::consts::EXE_EXTENSION);
    let Some(trimmed) = trimmed_for(module, &taken) else {
        result.left.extend(taken.iter().map(|test| test.name.clone()));
        return result;
    };
    let flags = match crate::aot::tests_executable(&trimmed, &names, &work) {
        Ok(flags) => flags,
        Err(why) => {
            result.left.extend(taken.iter().map(|test| test.name.clone()));
            result.refused.get_or_insert(why);
            return result;
        }
    };
    let fused = fusible(module);
    let prelude = prelude_functions();
    let ran = if real { vec![run_for_real(&work, taken.len())] } else { run_workers(&work, taken.len()) };
    let mut seen = vec![false; taken.len()];
    for (number, failure, set) in ran.iter().flat_map(|output| output.lines()).filter_map(read_line) {
        let (Some(test), Some(done)) = (taken.get(number), seen.get_mut(number)) else { continue };
        *done = true;
        match failure {
            None => {
                let flagged: Vec<&str> =
                    set.iter().filter_map(|at| flags.get(*at)).map(String::as_str).collect();
                // Another input could reach more, so a property test depends on all it can reach.
                if test.inputs.is_empty()
                    && let Some(ran) = traced(&flagged, &fused, defines, &prelude)
                {
                    result.ran.insert(test.name.clone(), ran);
                }
                let cases = if test.inputs.is_empty() { 1 } else { PROPERTY_CASES };
                let trace = Trace::default();
                result.outcomes.push(TestOutcome { name: test.name.clone(), failure: None, trace, cases });
            }
            // A property test's input is the simplest that fails, as it shrank it.
            Some((failure, trace)) => {
                result.outcomes.push(TestOutcome {
                    name: test.name.clone(),
                    failure: Some(test_failure(failure)),
                    trace,
                    cases: 1,
                });
            }
        }
    }
    rerun_crashed(&work, &taken, &seen, &mut result);
    let _ = std::fs::remove_file(&work);
    result
}

/// A worker that crashed leaves the tests it hadn't finished, and each of those runs again alone,
/// so a crash is the test's own.
fn rerun_crashed(work: &std::path::Path, taken: &[&TestDef], seen: &[bool], result: &mut LlvmTests) {
    for (number, test) in taken.iter().enumerate() {
        if seen.get(number).copied().unwrap_or(true) {
            continue;
        }
        let (output, crash) = run_alone(work, number);
        let line = output.lines().filter_map(read_line).find(|(at, _, _)| *at == number);
        let failure = match line {
            Some((_, failure, _)) => failure,
            None => {
                Some((Diagnostic::new(format!("this test crashed: {crash}"), test.span), Trace::default()))
            }
        };
        let cases = if failure.is_none() && !test.inputs.is_empty() { PROPERTY_CASES } else { 1 };
        let (failure, trace) =
            failure.map_or((None, Trace::default()), |(failure, trace)| (Some(failure), trace));
        result.outcomes.push(TestOutcome {
            name: test.name.clone(),
            failure: failure.map(test_failure),
            trace,
            cases,
        });
    }
}

/// Runs test `number` of a tests executable alone, giving what it wrote, and how it ended, in words,
/// with the first thing it said on standard error.
fn run_alone(work: &std::path::Path, number: usize) -> (String, String) {
    match test_command(work).arg(number.to_string()).output() {
        Ok(output) => {
            let said = String::from_utf8_lossy(&output.stderr);
            let first = said.lines().next().unwrap_or_default();
            let ended = if first.is_empty() {
                output.status.to_string()
            } else {
                format!("{}: {first}", output.status)
            };
            (String::from_utf8_lossy(&output.stdout).into_owned(), ended)
        }
        Err(err) => (String::new(), format!("it couldn't be run: {err}")),
    }
}
