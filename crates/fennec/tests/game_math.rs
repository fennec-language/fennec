//! `std.math` gives what glam gives. `examples/game_math.fen` prints each operation on fixed
//! inputs, and the examples' tests hold its `.out` on every engine. This reads that `.out` beside
//! `fixtures/game_math.glam`, which glam printed for the same inputs: see
//! `bench/realworld/twin/game_math_check.rs`. Every number has to be the same `f64`, bit for bit:
//! `std.math` uses glam's formulas in glam's order, and `from_euler`'s three turns come out the
//! same as glam's Shoemake method on these inputs too.

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

#[test]
fn std_math_gives_what_glam_gives() -> std::io::Result<()> {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let ours = lines(&repo.join("examples/game_math.out"))?;
    let glam = lines(&repo.join("crates/fennec/tests/fixtures/game_math.glam"))?;
    let labels =
        |all: &[(String, Vec<String>)]| all.iter().map(|(label, _)| label.clone()).collect::<Vec<_>>();
    assert_eq!(labels(&ours), labels(&glam), "the two print different lines");
    let mut wrong = Vec::new();
    for ((label, mine), (_, theirs)) in ours.iter().zip(&glam) {
        assert_eq!(mine.len(), theirs.len(), "{label}: a different number of values");
        for (at, (mine, theirs)) in mine.iter().zip(theirs).enumerate() {
            let same = match (mine.parse::<f64>(), theirs.parse::<f64>()) {
                (Ok(mine), Ok(theirs)) => mine.to_bits() == theirs.to_bits(),
                _ => mine == theirs,
            };
            if !same {
                wrong.push(format!("{label} [{at}]: ours {mine}, glam's {theirs}"));
            }
        }
    }
    assert!(wrong.is_empty(), "std.math and glam differ:\n{}", wrong.join("\n"));
    Ok(())
}
