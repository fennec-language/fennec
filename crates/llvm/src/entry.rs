//! The module made fresh each build: what every function reads by symbol, the program's bytes,
//! and `main`, which hands them to the runtime and has it call the program's `main`.

use std::fmt::Write as _;

use fennec_ir::Program;

use crate::Shared;

/// `image` is the program as the runtime reads it, whose string table has `shared`'s strings
/// in the order they're stored.
pub fn entry(program: &Program, shared: &Shared, image: &[u8]) -> Result<String, String> {
    let main =
        program.functions.iter().find(|function| function.name == "main").ok_or("there's no `main`")?;
    if main.returns != fennec_check::Type::Unit {
        return Err("`main` gives back a value, which this backend doesn't take yet".to_owned());
    }
    let mut out = String::from(crate::header()?);
    let _ = writeln!(out, "@fennec_stack_limit = dso_local global i64 0");
    for (symbol, start) in &shared.starts {
        let _ = writeln!(out, "@\"{symbol}\" = dso_local constant i64 {start}");
    }
    for (number, symbol) in shared.strings.keys().enumerate() {
        let _ = writeln!(out, "@\"{symbol}\" = dso_local constant i64 {number}");
    }
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
    let shown: Vec<String> = words.iter().map(|word| format!("i64 {word}")).collect();
    let count = words.len();
    let _ = writeln!(out, "@fennec_image = private constant [{count} x i64] [{}]\n", shown.join(", "));
    out += "declare void @fennec_aot_image(i64)\ndeclare i32 @fennec_aot_run(ptr, i64)\n\n";
    let flagged = crate::analysis::whole(program).can_fail.contains("main");
    let args = vec!["ptr null"; main.params].join(", ");
    let call = if flagged {
        let comma = if args.is_empty() { "" } else { ", " };
        let _ = writeln!(
            out,
            "define void @fennec_entry(i64 %limit) {{\n  store i64 %limit, ptr @fennec_stack_limit\n  %failed = alloca swifterror ptr\n  store ptr null, ptr %failed\n  call swiftcc void @\"f_main\"({args}{comma}ptr swifterror %failed)\n  ret void\n}}\n"
        );
        "ptr swifterror"
    } else {
        let _ = writeln!(
            out,
            "define void @fennec_entry(i64 %limit) {{\n  store i64 %limit, ptr @fennec_stack_limit\n  call swiftcc void @\"f_main\"({args})\n  ret void\n}}\n"
        );
        ""
    };
    let types = vec!["ptr"; main.params];
    let mut types = types.join(", ");
    if !call.is_empty() {
        if !types.is_empty() {
            types += ", ";
        }
        types += call;
    }
    let _ = writeln!(out, "declare swiftcc void @\"f_main\"({types})\n");
    let _ = writeln!(
        out,
        "define i32 @main(i32 %argc, ptr %argv) {{\nstart:\n  br label %head\nhead:\n  %at = phi i64 [ 0, %start ], [ %next, %word ]\n  %more = icmp ult i64 %at, {count}\n  br i1 %more, label %word, label %done\nword:\n  %place = getelementptr [{count} x i64], ptr @fennec_image, i64 0, i64 %at\n  %value = load i64, ptr %place\n  call void @fennec_aot_image(i64 %value)\n  %next = add i64 %at, 1\n  br label %head\ndone:\n  %code = call i32 @fennec_aot_run(ptr @fennec_entry, i64 {})\n  ret i32 %code\n}}",
        image.len()
    );
    Ok(out)
}
