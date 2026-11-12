//! The `fennec` command's tests, as one program.

mod cli;
mod environment;
mod examples;
mod executables;
mod game_math;
mod hints;
mod lint_fixes;
mod live;
mod llvm;
// The loader links Mach-O for Apple's arm64, and its tests compare it with Apple's linker, so
// elsewhere they'd be helpers nothing calls.
#[cfg(all(test, target_os = "macos", target_arch = "aarch64"))]
mod loader;
mod lsp;
mod parallel;
mod projects;
mod reached_runs;
mod replay;
mod selfhost;
mod syntax_errors;
mod unchanged_tests;
