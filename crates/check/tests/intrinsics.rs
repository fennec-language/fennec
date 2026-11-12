//! `@intrinsic` functions: std's machine operations, written as a signature with no body,
//! which the engines provide. A program's own code can't write or import one.

use crate::{problems, problems_as_written};

#[test]
fn only_the_standard_library_has_intrinsics() {
    assert_eq!(
        problems_as_written("@intrinsic\ndef int_add(a: int, b: int) -> int\n"),
        ["only the standard library has `@intrinsic` functions @ @intrinsic"]
    );
    assert_eq!(
        problems("from std.intrinsics import int_add\n\ndef main(stdout: Stdout):\n    pass\n"),
        ["`std.intrinsics` is for the standard library @ from std.intrinsics import int_add"]
    );
}

#[test]
fn a_def_without_a_body_needs_one() {
    assert_eq!(
        problems_as_written(
            "def double(n: int) -> int\n\ndef main(stdout: Stdout):\n    stdout.print(double(2))\n"
        ),
        ["`double` has no body @ def double(n: int) -> int"]
    );
    assert_eq!(
        problems_as_written("def double(n: int) -> int\n    n * 2\n"),
        ["parse: expected `:`, found the end of the line"]
    );
}

#[test]
fn a_def_takes_no_other_annotation() {
    assert_eq!(
        problems_as_written(
            "struct Point:\n    x = 0\n\n    @inline\n    def moved(self) -> int:\n        return self.x + 1\n"
        ),
        ["`@inline` isn't something a `def` can have @ @inline"]
    );
    assert_eq!(
        problems_as_written("@skip\nlimit = 3\n"),
        ["parse: an annotation goes above a field or a `def`"]
    );
}
