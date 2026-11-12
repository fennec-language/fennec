//! Duration literals: a number and a unit of time, like `30s` or `1.5ms`, which makes a
//! `Duration` of whole nanoseconds. They're the only literals with units.

use crate::{helps, in_main, problems};

#[test]
fn a_duration_literal_is_a_duration() {
    let program = "from std.time import Duration\n\ndef wait(pause = 250ms) -> Duration:\n    return pause * 2\n\n\
                   def main(stdout: Stdout):\n    stdout.print((wait() + 1.5s - 2min + 1h30min).nanos, 30ns < 2us)\n";
    assert_eq!(problems(program), Vec::<String>::new());
    assert_eq!(in_main("x = 3s + 1"), ["`other` should be a Duration, but this is an int @ 1"]);
}

#[test]
fn a_duration_has_one_of_six_units_and_whole_nanoseconds() {
    assert_eq!(in_main("x = 5m"), ["parse: `5m` uses `m`, which isn't a unit of time"]);
    assert_eq!(helps("def main(stdout: Stdout):\n    x = 5m\n"), ["parse error"]);
    assert_eq!(in_main("x = 3sec"), ["parse: `3sec` uses `sec`, which isn't a unit of time"]);
    assert_eq!(in_main("x = 1.5ns"), ["parse: `1.5ns` isn't a whole number of nanoseconds"]);
    assert_eq!(in_main("x = 3000000h"), ["parse: `3000000h` is too long for a duration"]);
    assert_eq!(in_main("x = 30s1h"), ["parse: `30s1h` has its units out of order"]);
    assert_eq!(in_main("x = 1h30"), ["parse: `1h30` ends without a unit"]);
}
