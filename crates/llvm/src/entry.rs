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

/// The module. `bytes` is the program as the runtime reads it.
pub fn entry(program: &Program, shared: &Shared, image: &Image, bytes: &[u8]) -> Result<String, String> {
    let main =
        program.functions.iter().find(|function| function.name == "main").ok_or("there's no `main`")?;
    if main.returns != fennec_check::Type::Unit {
        return Err("`main` gives back a value, which this backend doesn't take yet".to_owned());
    }
    let mut out = String::from(crate::header()?);
    out += "@fennec_stack_limit = dso_local global i64 0\n@fennec_failed = dso_local global ptr null\n";
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
    out += "declare void @fennec_aot_image(i64)\ndeclare i32 @fennec_aot_run(ptr, i64)\ndeclare i64 @fennec_aot_failed_flag()\ndeclare i64 @fennec_aot_literal(i64)\n\n";
    let flagged = crate::analysis::whole(program).can_fail.contains("main");
    let mut args = vec!["ptr null".to_owned(); main.params];
    let mut types = vec!["ptr".to_owned(); main.params];
    if flagged {
        args.push("ptr swifterror %failed".to_owned());
        types.push("ptr swifterror".to_owned());
    }
    let _ = writeln!(out, "declare swiftcc void @\"f_main\"({})\n", types.join(", "));
    out += "define void @fennec_entry(i64 %limit) {\n  store i64 %limit, ptr @fennec_stack_limit\n  %flag = call i64 @fennec_aot_failed_flag()\n  %flag_at = inttoptr i64 %flag to ptr\n  store ptr %flag_at, ptr @fennec_failed\n";
    for (number, symbol) in shared.literals.keys().enumerate() {
        let _ = writeln!(
            out,
            "  %l{number} = call i64 @fennec_aot_literal(i64 {number})\n  %a{number} = inttoptr i64 %l{number} to ptr\n  store ptr %a{number}, ptr @\"{symbol}\""
        );
    }
    if flagged {
        out += "  %failed = alloca swifterror ptr\n  store ptr null, ptr %failed\n";
    }
    let _ = writeln!(out, "  call swiftcc void @\"f_main\"({})\n  ret void\n}}\n", args.join(", "));
    let _ = writeln!(
        out,
        "define i32 @main(i32 %argc, ptr %argv) {{\nstart:\n  br label %head\nhead:\n  %at = phi i64 [ 0, %start ], [ %next, %word ]\n  %more = icmp ult i64 %at, {count}\n  br i1 %more, label %word, label %done\nword:\n  %place = getelementptr [{count} x i64], ptr @fennec_image, i64 0, i64 %at\n  %value = load i64, ptr %place\n  call void @fennec_aot_image(i64 %value)\n  %next = add i64 %at, 1\n  br label %head\ndone:\n  %code = call i32 @fennec_aot_run(ptr @fennec_entry, i64 {})\n  ret i32 %code\n}}",
        bytes.len()
    );
    Ok(out)
}
