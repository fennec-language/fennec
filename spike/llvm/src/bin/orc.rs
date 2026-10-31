//! A stand-in for the daemon holding LLVM: loads cached objects into itself through ORC, runs the
//! program in-process, and swaps edited functions in through their slots, even while it runs.
//!
//! `orc --dir EMITTED --objs OBJECTS --rt rt.o --passes P --cg N`
//!     `[--no-run] [--no-load] [--edit EMITTED2]... [--reps K] [--live EMITTED2 --after MS]`

use std::collections::HashMap;
use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::sync::atomic::{AtomicPtr, Ordering};
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
    fn LLVMRunPasses(m: Ptr, passes: *const c_char, tm: Ptr, options: Ptr) -> Ptr;
    fn LLVMGetErrorMessage(err: Ptr) -> *mut c_char;
    fn LLVMTargetMachineEmitToMemoryBuffer(tm: Ptr, m: Ptr, kind: c_int, err: *mut *mut c_char, buf: *mut Ptr) -> c_int;
    fn LLVMOrcCreateLLJIT(j: *mut Ptr, builder: Ptr) -> Ptr;
    fn LLVMOrcLLJITGetMainJITDylib(j: Ptr) -> Ptr;
    fn LLVMOrcLLJITGetGlobalPrefix(j: Ptr) -> c_char;
    fn LLVMOrcCreateDynamicLibrarySearchGeneratorForProcess(g: *mut Ptr, prefix: c_char, filter: Ptr, ctx: Ptr) -> Ptr;
    fn LLVMOrcJITDylibAddGenerator(jd: Ptr, g: Ptr);
    fn LLVMOrcLLJITAddObjectFile(j: Ptr, jd: Ptr, buf: Ptr) -> Ptr;
    fn LLVMOrcLLJITLookup(j: Ptr, out: *mut u64, name: *const c_char) -> Ptr;
}

fn ms(start: Instant) -> f64 {
    start.elapsed().as_secs_f64() * 1e3
}

fn cstr(text: &str) -> CString {
    CString::new(text).unwrap()
}

unsafe fn ok(err: Ptr, what: &str) {
    if !err.is_null() {
        let message = unsafe { CStr::from_ptr(LLVMGetErrorMessage(err)) };
        panic!("{what}: {}", message.to_string_lossy());
    }
}

/// Resident memory and the memory only this process has, by the system's own tools.
fn memory(label: &str) {
    if std::env::var_os("ORC_NO_MEMORY").is_some() {
        return;
    }
    let pid = std::process::id().to_string();
    let run = |cmd: &str, args: &[&str]| {
        std::process::Command::new(cmd).args(args).output().map(|out| String::from_utf8_lossy(&out.stdout).into_owned())
    };
    let rss = run("ps", &["-o", "rss=", "-p", &pid]).unwrap_or_default();
    let rss: f64 = rss.trim().parse().unwrap_or(0.0);
    let vmmap = run("vmmap", &["-summary", &pid]).unwrap_or_default();
    let footprint =
        vmmap.lines().find(|line| line.starts_with("Physical footprint:")).map_or("?", |line| line.split(':').nth(1).unwrap_or("?").trim());
    println!("{{\"memory\":\"{label}\",\"rss_mb\":{:.1},\"footprint\":\"{footprint}\"}}", rss / 1024.0);
}

struct Engine {
    jit: Ptr,
    dylib: Ptr,
    tm: Ptr,
    options: Ptr,
    passes: CString,
    run_passes: bool,
    version: usize,
    slots: HashMap<String, u64>,
}

unsafe impl Send for Engine {}

#[derive(Default, Debug, Clone)]
struct Phases {
    parse: f64,
    opt: f64,
    cg: f64,
    add: f64,
    lookup: f64,
    changed: usize,
}

impl Engine {
    unsafe fn compile(&self, text: &str, phases: &mut Phases) -> Ptr {
        unsafe {
            let context = LLVMContextCreate();
            let mut err = std::ptr::null_mut();
            let start = Instant::now();
            let buf = LLVMCreateMemoryBufferWithMemoryRangeCopy(text.as_ptr().cast(), text.len(), cstr("m").as_ptr());
            let mut module = std::ptr::null_mut();
            if LLVMParseIRInContext(context, buf, &mut module, &mut err) != 0 {
                panic!("{}", CStr::from_ptr(err).to_string_lossy());
            }
            phases.parse += ms(start);
            let start = Instant::now();
            if self.run_passes {
                ok(LLVMRunPasses(module, self.passes.as_ptr(), self.tm, self.options), "passes");
            }
            phases.opt += ms(start);
            let start = Instant::now();
            let mut object = std::ptr::null_mut();
            if LLVMTargetMachineEmitToMemoryBuffer(self.tm, module, 1, &mut err, &mut object) != 0 {
                panic!("{}", CStr::from_ptr(err).to_string_lossy());
            }
            phases.cg += ms(start);
            LLVMDisposeModule(module);
            LLVMContextDispose(context);
            object
        }
    }

    unsafe fn add(&self, object: Ptr) {
        unsafe { ok(LLVMOrcLLJITAddObjectFile(self.jit, self.dylib, object), "add") }
    }

    unsafe fn lookup(&self, name: &str) -> u64 {
        let mut address = 0;
        unsafe { ok(LLVMOrcLLJITLookup(self.jit, &mut address, cstr(name).as_ptr()), name) };
        address
    }

    /// Points a function's slot at new code. Calls already running finish in the old code.
    unsafe fn publish(&self, name: &str, symbol: &str) {
        if let Some(slot) = self.slots.get(name) {
            let address = unsafe { self.lookup(symbol) };
            let slot = unsafe { &*(*slot as *const AtomicPtr<c_void>) };
            slot.store(address as Ptr, Ordering::Release);
        }
    }

    /// Compiles and swaps in what differs between the loaded program and an edited one. The
    /// changed functions get a new name each time, since a name can only be defined once.
    unsafe fn swap(&mut self, base: &Base, edited: &str) -> Phases {
        let mut phases = Phases::default();
        self.version += 1;
        let mut done = Vec::new();
        // `changed.txt` holds the files that differ: the cache's own lookup would say so.
        let changed = std::fs::read_to_string(format!("{edited}/changed.txt")).unwrap();
        for (name, file) in base.files.iter().filter(|(_, file)| changed.lines().any(|line| line == file)) {
            let text = std::fs::read_to_string(format!("{edited}/fn/{file}")).unwrap();
            assert!(name != "main", "main is running and can't be swapped");
            phases.changed += 1;
            let symbol = format!("f_{name}.v{}", self.version);
            let renamed = text.replace(&format!("@\"f_{name}\""), &format!("@\"{symbol}\""));
            let object = unsafe { self.compile(&renamed, &mut phases) };
            let start = Instant::now();
            unsafe { self.add(object) };
            phases.add += ms(start);
            done.push((name.clone(), symbol));
        }
        let start = Instant::now();
        for (name, symbol) in &done {
            unsafe { self.publish(name, symbol) };
        }
        phases.lookup += ms(start);
        phases
    }
}

struct Base {
    files: Vec<(String, String)>,
}

fn main() {
    let (mut dir, mut objs, mut rt, mut passes, mut cg) = (String::new(), String::new(), String::new(), String::new(), 1);
    let (mut no_run, mut no_load, mut edits, mut reps, mut live, mut after) = (false, false, Vec::new(), 1, None, 300_u64);
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--dir" => dir = args.next().unwrap(),
            "--objs" => objs = args.next().unwrap(),
            "--rt" => rt = args.next().unwrap(),
            "--passes" => passes = args.next().unwrap(),
            "--cg" => cg = args.next().unwrap().parse().unwrap(),
            "--no-run" => no_run = true,
            "--no-load" => no_load = true,
            "--edit" => edits.push(args.next().unwrap()),
            "--reps" => reps = args.next().unwrap().parse().unwrap(),
            "--live" => live = args.next(),
            "--after" => after = args.next().unwrap().parse().unwrap(),
            other => panic!("unknown flag {other}"),
        }
    }
    unsafe {
        let start = Instant::now();
        LLVMInitializeAArch64TargetInfo();
        LLVMInitializeAArch64Target();
        LLVMInitializeAArch64TargetMC();
        LLVMInitializeAArch64AsmPrinter();
        LLVMInitializeAArch64AsmParser();
        let triple = cstr("arm64-apple-macosx14.0.0");
        let (mut target, mut err) = (std::ptr::null_mut(), std::ptr::null_mut());
        assert_eq!(LLVMGetTargetFromTriple(triple.as_ptr(), &mut target, &mut err), 0);
        let tm = LLVMCreateTargetMachine(target, triple.as_ptr(), cstr("apple-m1").as_ptr(), cstr("").as_ptr(), cg, 2, 0);
        let mut jit = std::ptr::null_mut();
        ok(LLVMOrcCreateLLJIT(&mut jit, std::ptr::null_mut()), "LLJIT");
        let dylib = LLVMOrcLLJITGetMainJITDylib(jit);
        let mut generator = std::ptr::null_mut();
        ok(
            LLVMOrcCreateDynamicLibrarySearchGeneratorForProcess(
                &mut generator,
                LLVMOrcLLJITGetGlobalPrefix(jit),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            ),
            "generator",
        );
        LLVMOrcJITDylibAddGenerator(dylib, generator);
        let mut engine = Engine {
            jit,
            dylib,
            tm,
            options: LLVMCreatePassBuilderOptions(),
            run_passes: !passes.is_empty(),
            passes: cstr(&passes),
            version: 0,
            slots: HashMap::new(),
        };
        let init_ms = ms(start);
        memory("llvm loaded, nothing else");
        if no_load {
            return;
        }

        let names: Vec<String> = std::fs::read_to_string(format!("{dir}/names.txt")).unwrap().lines().map(str::to_owned).collect();
        let files: Vec<(String, String)> =
            names.iter().enumerate().map(|(at, name)| (name.clone(), format!("f{at:05}.ll"))).collect();
        let base = Base { files };

        // Load: the slots, the runtime, then every function's cached object.
        let mut phases = Phases::default();
        let slots = std::fs::read_to_string(format!("{dir}/slots.ll")).unwrap();
        let object = engine.compile(&slots, &mut phases);
        engine.add(object);
        let add_file = |path: &str| {
            let bytes = std::fs::read(path).unwrap();
            LLVMCreateMemoryBufferWithMemoryRangeCopy(bytes.as_ptr().cast(), bytes.len(), cstr(path).as_ptr())
        };
        engine.add(add_file(&rt));
        let start = Instant::now();
        // One merged object if there is one, since reading a thousand small files is slow.
        let merged = format!("{objs}/merged.obj");
        if std::path::Path::new(&merged).exists() {
            engine.add(add_file(&merged));
        } else {
            for (at, _) in names.iter().enumerate() {
                engine.add(add_file(&format!("{objs}/f{at:05}.o")));
            }
        }
        let add_ms = ms(start);
        let start = Instant::now();
        for name in &names {
            let slot = engine.lookup(&format!("s_{name}"));
            engine.slots.insert(name.clone(), slot);
        }
        let slots_ms = ms(start);
        let start = Instant::now();
        for name in &names {
            engine.publish(name, &format!("f_{name}"));
        }
        let link_ms = ms(start);
        println!(
            "{{\"init_ms\":{init_ms:.2},\"functions\":{},\"add_ms\":{add_ms:.2},\"slot_lookup_ms\":{slots_ms:.2},\"link_ms\":{link_ms:.2}}}",
            names.len()
        );
        memory("every function loaded");

        for edited in &edits {
            let mut best: Option<(f64, Phases)> = None;
            let mut totals = Vec::new();
            for _ in 0..reps {
                let start = Instant::now();
                let phases = engine.swap(&base, edited);
                let total = ms(start);
                totals.push(total);
                if best.as_ref().is_none_or(|held| total < held.0) {
                    best = Some((total, phases));
                }
            }
            let (total, p) = best.unwrap();
            println!(
                "{{\"edit\":\"{edited}\",\"changed\":{},\"swap_ms\":{total:.2},\"parse\":{:.2},\"opt\":{:.2},\"cg\":{:.2},\"add\":{:.2},\"lookup_and_store\":{:.2},\"reps\":{reps},\"all_ms\":{totals:.1?}}}",
                p.changed, p.parse, p.opt, p.cg, p.add, p.lookup
            );
            memory(&format!("after {reps} swaps of {edited}"));
        }

        if no_run {
            return;
        }
        let main_at = engine.lookup("main");
        let main: extern "C" fn() -> c_int = std::mem::transmute(main_at as usize);
        let code = if let Some(edited) = live {
            let edited_base = base;
            let mut moved = engine;
            let swapper = std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(after));
                let start = Instant::now();
                let phases = moved.swap(&edited_base, &edited);
                eprintln!("{{\"live_swap_ms\":{:.2},\"changed\":{}}}", ms(start), phases.changed);
            });
            let code = main();
            swapper.join().unwrap();
            code
        } else {
            main()
        };
        std::process::exit(code);
    }
}
