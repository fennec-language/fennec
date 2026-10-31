//! Compiles `.ll` files in one process through LLVM's C API, timing parsing, passes and code
//! generation apart, with no process startup. It's how a compiler linking LLVM in would do it.
//!
//! `driver --passes P --cg 0..3 [--isel default|fast|global] [--out DIR] [--probe] [--quiet] FILE...`

use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::fmt::Write as _;
use std::time::Instant;

type Ptr = *mut c_void;

#[link(name = "LLVM")]
unsafe extern "C" {
    fn LLVMInitializeAArch64Target();
    fn LLVMInitializeAArch64TargetInfo();
    fn LLVMInitializeAArch64TargetMC();
    fn LLVMInitializeAArch64AsmPrinter();
    fn LLVMInitializeAArch64AsmParser();
    fn LLVMContextCreate() -> Ptr;
    fn LLVMContextDispose(c: Ptr);
    fn LLVMCreateMemoryBufferWithMemoryRangeCopy(data: *const c_char, len: usize, name: *const c_char) -> Ptr;
    fn LLVMParseIRInContext(c: Ptr, buf: Ptr, m: *mut Ptr, err: *mut *mut c_char) -> c_int;
    fn LLVMParseBitcodeInContext2(c: Ptr, buf: Ptr, m: *mut Ptr) -> c_int;
    fn LLVMWriteBitcodeToMemoryBuffer(m: Ptr) -> Ptr;
    fn LLVMCloneModule(m: Ptr) -> Ptr;
    fn LLVMDisposeModule(m: Ptr);
    fn LLVMGetTargetFromTriple(t: *const c_char, target: *mut Ptr, err: *mut *mut c_char) -> c_int;
    fn LLVMCreateTargetMachine(
        target: Ptr,
        triple: *const c_char,
        cpu: *const c_char,
        features: *const c_char,
        level: c_int,
        reloc: c_int,
        code_model: c_int,
    ) -> Ptr;
    fn LLVMCreatePassBuilderOptions() -> Ptr;
    fn LLVMPassBuilderOptionsSetLoopUnrolling(options: Ptr, on: c_int);
    fn LLVMRunPasses(m: Ptr, passes: *const c_char, tm: Ptr, options: Ptr) -> Ptr;
    fn LLVMGetErrorMessage(err: Ptr) -> *mut c_char;
    fn LLVMTargetMachineEmitToMemoryBuffer(tm: Ptr, m: Ptr, kind: c_int, err: *mut *mut c_char, buf: *mut Ptr) -> c_int;
    fn LLVMGetBufferSize(buf: Ptr) -> usize;
    fn LLVMGetBufferStart(buf: Ptr) -> *const c_char;
    fn LLVMDisposeMemoryBuffer(buf: Ptr);
    fn LLVMSetTargetMachineFastISel(tm: Ptr, on: c_int);
    fn LLVMSetTargetMachineGlobalISel(tm: Ptr, on: c_int);
}

fn ms(start: Instant) -> f64 {
    start.elapsed().as_secs_f64() * 1e3
}

fn cstr(text: &str) -> CString {
    CString::new(text).unwrap()
}

fn main() {
    let (mut passes, mut cg, mut isel, mut out_dir, mut probe, mut quiet) =
        (String::new(), 1, "default".to_owned(), None, false, false);
    let mut files = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--passes" => passes = args.next().unwrap(),
            "--cg" => cg = args.next().unwrap().parse().unwrap(),
            "--isel" => isel = args.next().unwrap(),
            "--out" => out_dir = args.next(),
            "--probe" => probe = true,
            "--quiet" => quiet = true,
            _ => files.push(arg),
        }
    }
    unsafe {
        let init = Instant::now();
        LLVMInitializeAArch64TargetInfo();
        LLVMInitializeAArch64Target();
        LLVMInitializeAArch64TargetMC();
        LLVMInitializeAArch64AsmPrinter();
        LLVMInitializeAArch64AsmParser();
        let triple = cstr("arm64-apple-macosx14.0.0");
        let mut target = std::ptr::null_mut();
        let mut err = std::ptr::null_mut();
        assert_eq!(LLVMGetTargetFromTriple(triple.as_ptr(), &mut target, &mut err), 0);
        let tm = LLVMCreateTargetMachine(target, triple.as_ptr(), cstr("apple-m1").as_ptr(), cstr("").as_ptr(), cg, 2, 0);
        match isel.as_str() {
            "fast" => {
                LLVMSetTargetMachineGlobalISel(tm, 0);
                LLVMSetTargetMachineFastISel(tm, 1);
            }
            "global" => LLVMSetTargetMachineGlobalISel(tm, 1),
            _ => {}
        }
        let options = LLVMCreatePassBuilderOptions();
        if std::env::var_os("FEN_NO_UNROLL").is_some() {
            LLVMPassBuilderOptionsSetLoopUnrolling(options, 0);
        }
        let init_ms = ms(init);
        let pipeline = cstr(&passes);
        let (mut parse_total, mut opt_total, mut cg_total, mut obj_total, mut text_total) = (0.0, 0.0, 0.0, 0_usize, 0);
        let (mut bc_write, mut bc_read, mut clone, mut bc_bytes) = (0.0, 0.0, 0.0, 0_usize);
        let mut rows = String::new();
        for file in &files {
            let text = std::fs::read(file).unwrap();
            text_total += text.len();
            let context = LLVMContextCreate();
            let start = Instant::now();
            let buf = LLVMCreateMemoryBufferWithMemoryRangeCopy(text.as_ptr().cast(), text.len(), cstr(file).as_ptr());
            let mut module = std::ptr::null_mut();
            if LLVMParseIRInContext(context, buf, &mut module, &mut err) != 0 {
                panic!("{file}: {}", CStr::from_ptr(err).to_string_lossy());
            }
            let parse_ms = ms(start);
            parse_total += parse_ms;
            if probe {
                let start = Instant::now();
                let copy = LLVMCloneModule(module);
                clone += ms(start);
                LLVMDisposeModule(copy);
                let start = Instant::now();
                let bc = LLVMWriteBitcodeToMemoryBuffer(module);
                bc_write += ms(start);
                bc_bytes += LLVMGetBufferSize(bc);
                let mut again = std::ptr::null_mut();
                let second = LLVMContextCreate();
                let start = Instant::now();
                assert_eq!(LLVMParseBitcodeInContext2(second, bc, &mut again), 0);
                bc_read += ms(start);
                LLVMDisposeModule(again);
                LLVMContextDispose(second);
                LLVMDisposeMemoryBuffer(bc);
            }
            let start = Instant::now();
            if !passes.is_empty() {
                let failed = LLVMRunPasses(module, pipeline.as_ptr(), tm, options);
                if !failed.is_null() {
                    panic!("{}", CStr::from_ptr(LLVMGetErrorMessage(failed)).to_string_lossy());
                }
            }
            let opt_ms = ms(start);
            opt_total += opt_ms;
            let start = Instant::now();
            let mut object = std::ptr::null_mut();
            if LLVMTargetMachineEmitToMemoryBuffer(tm, module, 1, &mut err, &mut object) != 0 {
                panic!("{file}: {}", CStr::from_ptr(err).to_string_lossy());
            }
            let cg_ms = ms(start);
            cg_total += cg_ms;
            let size = LLVMGetBufferSize(object);
            obj_total += size;
            if let Some(dir) = &out_dir {
                let stem = std::path::Path::new(file).file_stem().unwrap().to_string_lossy();
                let bytes = std::slice::from_raw_parts(LLVMGetBufferStart(object).cast::<u8>(), size);
                std::fs::write(format!("{dir}/{stem}.o"), bytes).unwrap();
            }
            LLVMDisposeMemoryBuffer(object);
            LLVMDisposeModule(module);
            LLVMContextDispose(context);
            if !quiet {
                let _ = writeln!(rows, "{file} parse {parse_ms:.3} opt {opt_ms:.3} cg {cg_ms:.3} obj {size}");
            }
        }
        eprint!("{rows}");
        println!(
            "{{\"init_ms\":{init_ms:.3},\"files\":{},\"text_bytes\":{text_total},\"parse_ms\":{parse_total:.3},\"opt_ms\":{opt_total:.3},\"cg_ms\":{cg_total:.3},\"obj_bytes\":{obj_total},\"clone_ms\":{clone:.3},\"bc_write_ms\":{bc_write:.3},\"bc_read_ms\":{bc_read:.3},\"bc_bytes\":{bc_bytes}}}",
            files.len()
        );
    }
}
