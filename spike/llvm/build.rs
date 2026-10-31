// Links the driver against the libLLVM.dylib the Rust toolchain ships, so nothing's installed.
fn main() {
    let out = std::process::Command::new("rustc").args(["--print", "sysroot"]).output().unwrap();
    let sysroot = String::from_utf8(out.stdout).unwrap();
    let lib = format!("{}/lib", sysroot.trim());
    println!("cargo:rustc-link-search=native={lib}");
    println!("cargo:rustc-link-arg-bin=driver=-Wl,-rpath,{lib}");
}
