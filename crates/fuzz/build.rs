//! Says when the fuzzer is built with `AddressSanitizer`, as `cfg(address_sanitizer)`, so the time
//! an input may take allows for the sanitizer's slowdown. Stable Rust can't read
//! `cfg(sanitize = "address")` itself, but Cargo tells a build script every cfg its target has.

fn main() {
    println!("cargo::rustc-check-cfg=cfg(address_sanitizer)");
    let sanitizers = std::env::var("CARGO_CFG_SANITIZE").unwrap_or_default();
    if sanitizers.split(',').any(|sanitizer| sanitizer == "address") {
        println!("cargo::rustc-cfg=address_sanitizer");
    }
}
