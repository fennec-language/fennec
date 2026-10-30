//! Gives the WebAssembly module a bigger stack in its memory: the interpreter recurses for each
//! nested call, and the default 1 MiB can run out before the browser's own stack does.

const STACK_BYTES: u32 = 8 * 1024 * 1024;

fn main() {
    if std::env::var("CARGO_CFG_TARGET_FAMILY").is_ok_and(|family| family == "wasm") {
        println!("cargo::rustc-link-arg-cdylib=-zstack-size={STACK_BYTES}");
    }
}
