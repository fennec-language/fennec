//! Which exports only the runtime with the interpreter has, which is how `fennec build` knows
//! a program can link against the lean one.

#[test]
#[cfg(not(feature = "lean"))]
fn the_exports_only_the_interpreter_needs_are_the_ones_the_lean_runtime_leaves_out() {
    let mut listed: Vec<&str> = fennec_runtime::interpreter_symbols().collect();
    let mut known = fennec_interp::embed::INTERPRETER_EXPORTS.to_vec();
    listed.sort_unstable();
    known.sort_unstable();
    assert_eq!(listed, known);
    assert!(listed.contains(&"fennec_std_call"));
    assert!(!listed.contains(&"fennec_alloc"));
}
