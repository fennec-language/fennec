//! The module made fresh each build: what every function reads by symbol, the program's bytes,
//! and `main`, which hands them to the runtime and has it call the program's `main`.

use std::fmt::Write as _;

use fennec_interp::embed::Tables;
use fennec_ir::Program;
use fennec_layout::{Layouts, Registry, Ty};

use crate::Shared;

/// What goes in a program's image, for a program with these strings and kinds of list item, and
/// the numbers the module made fresh gives them.
pub struct Image {
    pub tables: Tables,
    pub literals: Vec<String>,
    elements: Vec<(String, i64)>,
    strings: Vec<(String, i64)>,
}

impl Image {
    pub fn of(layouts: &Layouts, shared: &Shared) -> Result<Image, String> {
        let mut registry = Registry::new(layouts);
        let mut elements = Vec::with_capacity(shared.elements.len());
        for (symbol, item) in &shared.elements {
            let number =
                registry.element(*item).ok_or_else(|| format!("a list of {item:?} has no kind of item"))?;
            elements.push((symbol.clone(), number));
        }
        let boxed = shared.shapes.iter().filter_map(|(symbol, id)| {
            let number = registry.shape(Ty::Boxed(*id))?;
            Some((symbol.clone(), i64::try_from(number).ok()?))
        });
        elements.extend(boxed);
        let str_shape = u64::try_from(layouts.len()).unwrap_or(0);
        let dicts = shared.dicts.iter().filter_map(|(symbol, (key, value))| {
            let number = fennec_interp::embed::dicts::shape(str_shape, *key, *value)?;
            Some((symbol.clone(), i64::try_from(number).ok()?))
        });
        elements.extend(dicts);
        let strings =
            shared.strings.iter().map(|(symbol, text)| (symbol.clone(), registry.string(text))).collect();
        Ok(Image {
            tables: registry.finish(),
            literals: shared.literals.values().cloned().collect(),
            elements,
            strings,
        })
    }
}

fn words_of(image: &[u8]) -> Vec<u64> {
    let mut words: Vec<u64> = image
        .chunks(8)
        .map(|chunk| {
            let mut word = [0_u8; 8];
            for (to, from) in word.iter_mut().zip(chunk) {
                *to = *from;
            }
            u64::from_le_bytes(word)
        })
        .collect();
    if words.is_empty() {
        words.push(0);
    }
    words
}

/// What a module for a program starts with: what every function reads by symbol, and the
/// program's bytes, in words, as it gives back how many there are.
fn start_of(
    program: &Program,
    shared: &Shared,
    image: &Image,
    bytes: &[u8],
) -> Result<(String, usize), String> {
    let mut out = String::from(crate::header()?);
    // A program running `parallel` keeps a stack limit and a failure flag for each thread.
    let local = if crate::parallel::uses_parallel(program) { "thread_local(initialexec) " } else { "" };
    let _ = writeln!(out, "@fennec_stack_limit = dso_local {local}global i64 0");
    let _ = writeln!(out, "@fennec_failed = dso_local {local}global ptr null");
    if crate::parallel::uses_parallel(program) {
        out += "@fennec_thread = dso_local thread_local(initialexec) global i64 0\n";
    }
    out += "@fennec_ascii = dso_local global ptr null\n";
    for (symbol, start) in &shared.starts {
        let _ = writeln!(out, "@\"{symbol}\" = dso_local constant i64 {start}");
    }
    for (symbol, number) in image.strings.iter().chain(&image.elements) {
        let _ = writeln!(out, "@\"{symbol}\" = dso_local constant i64 {number}");
    }
    for symbol in shared.literals.keys() {
        let _ = writeln!(out, "@\"{symbol}\" = dso_local global ptr null");
    }
    let words = words_of(bytes);
    let shown: Vec<String> = words.iter().map(|word| format!("i64 {word}")).collect();
    let count = words.len();
    let _ = writeln!(out, "@fennec_image = private constant [{count} x i64] [{}]\n", shown.join(", "));
    out += "declare void @fennec_aot_image(i64)\ndeclare i64 @fennec_aot_failed_flag()\ndeclare i64 @fennec_aot_literal(i64)\ndeclare i64 @fennec_aot_ascii()\n\n";
    Ok((out, count))
}

/// `fennec_entry` up to where it runs the program, setting what functions read. Gives back what
/// it needs defined after it: what closes a generator let go partway.
fn setup(out: &mut String, program: &Program, shared: &Shared, image: &Image) -> String {
    *out += "define void @fennec_entry(i64 %limit) {\n  store i64 %limit, ptr @fennec_stack_limit\n  %flag = call i64 @fennec_aot_failed_flag()\n  %flag_at = inttoptr i64 %flag to ptr\n  store ptr %flag_at, ptr @fennec_failed\n  %ascii = call i64 @fennec_aot_ascii()\n  %ascii_at = inttoptr i64 %ascii to ptr\n  store ptr %ascii_at, ptr @fennec_ascii\n";
    for (number, symbol) in shared.literals.keys().enumerate() {
        let _ = writeln!(
            out,
            "  %l{number} = call i64 @fennec_aot_literal(i64 {number})\n  %a{number} = inttoptr i64 %l{number} to ptr\n  store ptr %a{number}, ptr @\"{symbol}\""
        );
    }
    // What closes a generator let go partway, by its shape, for those that are ever made.
    let mut thunks = String::new();
    for def in &program.generators {
        let Some(abandon) = &def.abandon else { continue };
        let shape = format!("h_{}", def.name);
        let Some((_, number)) = image.elements.iter().find(|(symbol, _)| *symbol == shape) else { continue };
        let thunk = format!("@\"t_{}\"", def.name);
        let _ = writeln!(out, "  call void @fennec_set_abandon(i64 {number}, ptr {thunk})");
        let _ = writeln!(
            thunks,
            "declare swiftcc void {code}(ptr, ptr swifterror)\ndefine internal void {thunk}(i64 %g) {{\n  %at = inttoptr i64 %g to ptr\n  %failed = alloca swifterror ptr\n  store ptr null, ptr %failed\n  call swiftcc void {code}(ptr %at, ptr swifterror %failed)\n  ret void\n}}\n",
            code = crate::emit::symbol(abandon)
        );
    }
    if !thunks.is_empty() {
        thunks += "declare void @fennec_set_abandon(i64, ptr)\n";
    }
    thunks += &own_texts(out, program, shared);
    thunks
}

/// Tells the runtime how to write each type that writes its own text, which it asks for as it
/// goes through a value: a function given where the value is, which hands over the text. Gives
/// back those functions, for after `fennec_entry`.
fn own_texts(out: &mut String, program: &Program, shared: &Shared) -> String {
    let can_fail = crate::analysis::whole(program).can_fail;
    let mut thunks = String::new();
    for (number, (literal, function, boxed)) in shared.own_texts.iter().enumerate() {
        let at = shared.literals.keys().position(|symbol| symbol == literal).unwrap_or(0);
        let thunk = format!("@\"o_{number}\"");
        let _ = writeln!(out, "  call void @fennec_own_text(i64 %l{at}, ptr {thunk})");
        let _ = writeln!(
            thunks,
            "{}",
            own_text_thunk(&thunk, function, *boxed, can_fail.contains(function.as_str()))
        );
    }
    if !thunks.is_empty() {
        thunks +=
            "declare void @fennec_own_text(i64, ptr)\ndeclare void @fennec_own_text_written(i64, i64, i64)\n";
    }
    thunks
}

/// A function taking where a value's fields are, which has `function` write the value's text and
/// hands it over. If that fails, there's no text, and the failure is there for what's running.
fn own_text_thunk(thunk: &str, function: &str, boxed: bool, flagged: bool) -> String {
    let code = crate::emit::symbol(function);
    // A block's fields come after its header, and it's the block that's passed.
    let back = if boxed { crate::lists::FIELDS_OFFSET } else { 0 };
    let (types, args) = if flagged { (", ptr swifterror", ", ptr swifterror %failed") } else { ("", "") };
    format!(
        "declare swiftcc [3 x i64] {code}(ptr{types})\n\
         define internal void {thunk}(i64 %place) {{\n\
         start:\n\
         \x20 %failed = alloca swifterror ptr\n\
         \x20 store ptr null, ptr %failed\n\
         \x20 %at = inttoptr i64 %place to ptr\n\
         \x20 %value = getelementptr i8, ptr %at, i64 -{back}\n\
         \x20 %text = call swiftcc [3 x i64] {code}(ptr %value{args})\n\
         \x20 %error = load ptr, ptr %failed\n\
         \x20 %fine = icmp eq ptr %error, null\n\
         \x20 br i1 %fine, label %written, label %done\n\
         written:\n\
         \x20 %t0 = extractvalue [3 x i64] %text, 0\n\
         \x20 %t1 = extractvalue [3 x i64] %text, 1\n\
         \x20 %t2 = extractvalue [3 x i64] %text, 2\n\
         \x20 call void @fennec_own_text_written(i64 %t0, i64 %t1, i64 %t2)\n\
         \x20 br label %done\n\
         done:\n\
         \x20 ret void\n\
         }}"
    )
}

/// The C `main`: hands the runtime the program's bytes, then has it run with this call.
fn c_main(count: usize, run: &str) -> String {
    format!(
        "define i32 @main(i32 %argc, ptr %argv) {{\nstart:\n  br label %head\nhead:\n  %at = phi i64 [ 0, %start ], [ %next, %word ]\n  %more = icmp ult i64 %at, {count}\n  br i1 %more, label %word, label %done\nword:\n  %place = getelementptr [{count} x i64], ptr @fennec_image, i64 0, i64 %at\n  %value = load i64, ptr %place\n  call void @fennec_aot_image(i64 %value)\n  %next = add i64 %at, 1\n  br label %head\ndone:\n  %code = call i32 {run}\n  ret i32 %code\n}}"
    )
}

/// The module for an executable that runs the functions `tests`, which take nothing and give
/// back nothing, by their numbers there. `bytes` is the program as the runtime reads it.
pub fn tests_entry(
    program: &Program,
    shared: &Shared,
    image: &Image,
    bytes: &[u8],
    tests: &[String],
) -> Result<String, String> {
    let (mut out, count) = start_of(program, shared, image, bytes)?;
    out += "declare i32 @fennec_aot_run(ptr, i64)\ndeclare void @fennec_aot_tests_start()\ndeclare i64 @fennec_aot_next_test()\ndeclare void @fennec_aot_test_ran(i64)\ndeclare void @fennec_aot_test_done(i64)\n\n";
    // The flag each function sets when it runs, in the order `Shared::ran` lists them.
    let flags: Vec<String> = shared.ran.iter().map(|name| format!("@\"ran_{name}\"")).collect();
    for flag in &flags {
        let _ = writeln!(out, "{flag} = dso_local global i8 0");
    }
    let table: Vec<String> = flags.iter().map(|flag| format!("ptr {flag}")).collect();
    let many = flags.len();
    let _ = writeln!(out, "@fennec_ran_flags = private constant [{many} x ptr] [{}]\n", table.join(", "));
    // Says which flags the test just run set, clearing them for the next one.
    let _ = writeln!(
        out,
        "define internal void @fennec_report_ran() {{\nstart:\n  br label %head\nhead:\n  %at = phi i64 [ 0, %start ], [ %next, %step ]\n  %more = icmp ult i64 %at, {many}\n  br i1 %more, label %read, label %done\nread:\n  %place = getelementptr [{many} x ptr], ptr @fennec_ran_flags, i64 0, i64 %at\n  %flag = load ptr, ptr %place\n  %value = load i8, ptr %flag\n  %set = icmp ne i8 %value, 0\n  br i1 %set, label %seen, label %step\nseen:\n  store i8 0, ptr %flag\n  call void @fennec_aot_test_ran(i64 %at)\n  br label %step\nstep:\n  %next = add i64 %at, 1\n  br label %head\ndone:\n  ret void\n}}\n"
    );
    let thunks = setup(&mut out, program, shared, image);
    // The runtime says which tests to run, and hears how each went.
    out += "  call void @fennec_aot_tests_start()\n  br label %next\nnext:\n  %number = call i64 @fennec_aot_next_test()\n  %more = icmp sge i64 %number, 0\n  br i1 %more, label %run, label %end\nrun:\n  call void @fennec_run_test(i64 %number)\n  call void @fennec_report_ran()\n  call void @fennec_aot_test_done(i64 %number)\n  br label %next\nend:\n  ret void\n}\n\n";
    out += &thunks;
    let can_fail = crate::analysis::whole(program).can_fail;
    let mut cases = String::new();
    let mut blocks = String::new();
    for (number, test) in tests.iter().enumerate() {
        let code = crate::emit::symbol(test);
        let flagged = can_fail.contains(test.as_str());
        let (types, args) = if flagged { ("ptr swifterror", "ptr swifterror %failed") } else { ("", "") };
        let _ = writeln!(out, "declare swiftcc void {code}({types})");
        let _ = write!(cases, " i64 {number}, label %test{number}");
        let _ = writeln!(blocks, "test{number}:\n  call swiftcc void {code}({args})\n  br label %done");
    }
    let _ = writeln!(
        out,
        "\ndefine void @fennec_run_test(i64 %number) {{\nstart:\n  %failed = alloca swifterror ptr\n  store ptr null, ptr %failed\n  switch i64 %number, label %done [{cases} ]\n{blocks}done:\n  ret void\n}}\n"
    );
    out += &c_main(count, &format!("@fennec_aot_run(ptr @fennec_entry, i64 {})", bytes.len()));
    Ok(out)
}

/// The module. `bytes` is the program as the runtime reads it.
pub fn entry(program: &Program, shared: &Shared, image: &Image, bytes: &[u8]) -> Result<String, String> {
    let main =
        program.functions.iter().find(|function| function.name == "main").ok_or("there's no `main`")?;
    if main.returns != fennec_check::Type::Unit {
        return Err("`main` gives back a value, which this backend doesn't take yet".to_owned());
    }
    let (mut out, count) = start_of(program, shared, image, bytes)?;
    out += "declare i32 @fennec_aot_run(ptr, i64)\n\n";
    let flagged = crate::analysis::whole(program).can_fail.contains("main");
    // Each capability `main` takes is the runtime's whole one of its kind.
    let mut args = Vec::with_capacity(main.params + 1);
    let mut capabilities = String::new();
    for (at, local) in main.locals.iter().take(main.params).enumerate() {
        let kind = match &local.ty {
            fennec_check::Type::Named(name) => fennec_layout::Cap::named(name).map(|cap| match cap {
                fennec_layout::Cap::Stdout => 0,
                fennec_layout::Cap::Fs => 1,
                fennec_layout::Cap::Clock => 2,
                fennec_layout::Cap::Args => 3,
                fennec_layout::Cap::Stdin => 4,
                fennec_layout::Cap::Process => 5,
                fennec_layout::Cap::Entropy => 6,
                fennec_layout::Cap::Net => 7,
            }),
            _ => None,
        };
        match kind {
            Some(kind) => {
                let _ = writeln!(
                    capabilities,
                    "  %cap{at} = call i64 @fennec_capability_block(i64 {kind})\n  %capability{at} = inttoptr i64 %cap{at} to ptr"
                );
                args.push(format!("ptr %capability{at}"));
            }
            None => args.push("ptr null".to_owned()),
        }
    }
    let mut types = vec!["ptr".to_owned(); main.params];
    if flagged {
        args.push("ptr swifterror %failed".to_owned());
        types.push("ptr swifterror".to_owned());
    }
    let _ = writeln!(out, "declare swiftcc void @\"f_main\"({})\n", types.join(", "));
    let mut thunks = setup(&mut out, program, shared, image);
    if flagged {
        out += "  %failed = alloca swifterror ptr\n  store ptr null, ptr %failed\n";
    }
    if !capabilities.is_empty() {
        out += &capabilities;
        thunks += "declare i64 @fennec_capability_block(i64)\n";
    }
    let _ = writeln!(out, "  call swiftcc void @\"f_main\"({})\n  ret void\n}}\n", args.join(", "));
    out += &thunks;
    out += &c_main(count, &format!("@fennec_aot_run(ptr @fennec_entry, i64 {})", bytes.len()));
    Ok(out)
}
