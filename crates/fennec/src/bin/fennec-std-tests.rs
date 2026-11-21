//! `fennec-std-tests`: the `fennec` command as the repository's tests build it, with the
//! `std-private` feature, where a program can import std's private names to test them. It's never
//! built otherwise, and `fennec` itself can't import them.

fn main() -> std::process::ExitCode {
    fennec::main_with(fennec::StdNames::PrivateToo)
}
