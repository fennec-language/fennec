//! Names the platform this launcher is built for, as a release's manifest names it, so it fetches
//! the archive built for the same.

fn main() {
    let target = std::env::var("TARGET").unwrap_or_default();
    println!("cargo::rustc-env=FENNEC_TARGET={target}");
}
