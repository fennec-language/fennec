//! `std.math` gives what glam gives. `examples/game_math.fen` prints each operation of the `D`
//! types on fixed inputs, and `examples/game_math_f32.fen` of the `f32` ones, and the examples'
//! tests hold their `.out`s on every engine. This reads each `.out` beside what glam printed for
//! the same inputs, `fixtures/game_math.glam` and `fixtures/game_math_f32.glam`: see
//! `bench/realworld/twin/game_math_check.rs` and `game_math_f32_check.rs`. Every number has to be
//! the same `f64` or `f32`, bit for bit: `std.math` uses glam's formulas in glam's order, and
//! `from_euler`'s three turns come out the same as glam's Shoemake method on these inputs too.

use std::path::Path;

fn lines(path: &Path) -> std::io::Result<Vec<(String, Vec<String>)>> {
    let text = std::fs::read_to_string(path)?;
    Ok(text
        .lines()
        .map(|line| {
            let (label, values) = line.split_once(": ").unwrap_or((line, ""));
            (label.to_owned(), values.split(' ').map(str::to_owned).collect())
        })
        .collect())
}

/// Each line of `ours` beside the same line of `glam`'s, as numbers read by `same`.
fn compare(ours: &str, glam: &str, same: fn(&str, &str) -> Option<bool>) -> std::io::Result<()> {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let ours = lines(&repo.join(ours))?;
    let glam = lines(&repo.join(glam))?;
    let labels =
        |all: &[(String, Vec<String>)]| all.iter().map(|(label, _)| label.clone()).collect::<Vec<_>>();
    assert_eq!(labels(&ours), labels(&glam), "the two print different lines");
    let mut wrong = Vec::new();
    for ((label, mine), (_, theirs)) in ours.iter().zip(&glam) {
        assert_eq!(mine.len(), theirs.len(), "{label}: a different number of values");
        for (at, (mine, theirs)) in mine.iter().zip(theirs).enumerate() {
            if !same(mine, theirs).unwrap_or(mine == theirs) {
                wrong.push(format!("{label} [{at}]: ours {mine}, glam's {theirs}"));
            }
        }
    }
    assert!(wrong.is_empty(), "std.math and glam differ:\n{}", wrong.join("\n"));
    Ok(())
}

#[test]
fn std_math_gives_what_glam_gives() -> std::io::Result<()> {
    compare("examples/game_math.out", "crates/fennec/tests/fixtures/game_math.glam", |mine, theirs| {
        Some(mine.parse::<f64>().ok()?.to_bits() == theirs.parse::<f64>().ok()?.to_bits())
    })
}

#[test]
fn std_maths_f32_types_give_what_glams_give() -> std::io::Result<()> {
    compare(
        "examples/game_math_f32.out",
        "crates/fennec/tests/fixtures/game_math_f32.glam",
        |mine, theirs| Some(mine.parse::<f32>().ok()?.to_bits() == theirs.parse::<f32>().ok()?.to_bits()),
    )
}
