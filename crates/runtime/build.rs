//! Stamps the runtime library with the build of Fennec it's from, the same `FENNEC_BUILD` the
//! `fennec` command is built with, so `fennec` can refuse to link programs with a runtime library
//! built from other source, which would fail in ways that say nothing about why.
//!
//! The stamp is a static whose value and name both hold the build: see `stamp.rs` as written here.

include!("../fennec/build_hash.rs");

fn main() {
    let build = build_hash();
    println!("cargo::rustc-env=FENNEC_BUILD={build}");
    // The release's bitcode packs a string's bytes into bits, where reading the file finds no
    // stamp, but keeps each name as written, so the name holds the build too.
    let stamp = format!(
        "/// The build of Fennec this library is from, which `fennec` reads out of the library's file to tell one\n\
         /// built from other source: in an archive, from the value, after its marker; in the bitcode a release\n\
         /// links in, from the name. Kept by `#[used]`, since nothing calls it.\n\
         #[used]\n\
         static RUNTIME_BUILD_STAMP_{}: &str = concat!(\"fennec-runtime-build:\", env!(\"FENNEC_BUILD\"));\n",
        build.to_ascii_uppercase()
    );
    let written =
        std::env::var_os("OUT_DIR").ok_or_else(|| "Cargo gave no OUT_DIR".to_owned()).and_then(|out| {
            std::fs::write(std::path::Path::new(&out).join("stamp.rs"), stamp).map_err(|err| err.to_string())
        });
    if let Err(why) = written {
        println!("cargo::error=couldn't write the runtime's stamp: {why}");
    }
}
