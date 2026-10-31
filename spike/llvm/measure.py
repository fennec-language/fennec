#!/usr/bin/env python3
"""The spike's measurements, one program at a time, niced, under the watchdog in wd.py.

  measure.py baseline            twins, the empty programs, and the JIT on each kernel
  measure.py speed [tier ...]    compile and run each kernel at each LLVM tier
  measure.py scale N             the generated program: cold, one-function edits, JIT too

Results go to results/*.jsonl, one object a line. Nothing here deletes anything outside WORK.
"""
import hashlib, json, os, re, shutil, subprocess, sys, time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
import wd

ROOT = Path(__file__).resolve().parents[2]
BASE = Path("/Volumes/BuildSSD/fennec-worktrees/llvm-spike")
WORK = BASE / "work"
PROTO = BASE / "proto/release"
FENNEC = BASE / "release/fennec"
RESULTS = Path(__file__).parent / "results"
KERNELS = ["fib", "sieve", "mandelbrot", "nbody", "spectral_norm", "fannkuch"]
SYSROOT = subprocess.run(["rustc", "--print", "sysroot"], capture_output=True, text=True).stdout.strip()
LLVM_BIN = f"{SYSROOT}/lib/rustlib/aarch64-apple-darwin/bin"
SDK = subprocess.run(["xcrun", "--show-sdk-path"], capture_output=True, text=True).stdout.strip()

SIMPLE = "sroa,early-cse,simplifycfg"
TIERS = {
    # name: (passes, codegen level, instruction selection)
    "L0": ("", 0, "default"),
    "L0m": ("function(mem2reg)", 0, "default"),
    "L1a": (f"function({SIMPLE})", 1, "default"),
    "L1b": (
        f"function({SIMPLE},instcombine<no-verify-fixpoint>,simplifycfg),"
        "cgscc(inline),"
        f"function({SIMPLE},instcombine<no-verify-fixpoint>,simplifycfg)",
        1,
        "default",
    ),
    # L1b with loop-invariant code motion and redundancy removal, what nbody and fannkuch need.
    "L1c": (
        f"function({SIMPLE},instcombine<no-verify-fixpoint>,simplifycfg),"
        "cgscc(inline),"
        f"function({SIMPLE},instcombine<no-verify-fixpoint>,simplifycfg,loop-mssa(loop-rotate,licm),gvn,simplifycfg)",
        1,
        "default",
    ),
    # L1c with induction variables, which lets bounds and overflow checks in loops go.
    "L1d": (
        f"function({SIMPLE},instcombine<no-verify-fixpoint>,simplifycfg),"
        "cgscc(inline),"
        f"function({SIMPLE},instcombine<no-verify-fixpoint>,simplifycfg,loop-mssa(loop-rotate,licm),"
        "loop(loop-idiom,indvars,loop-deletion),correlated-propagation,gvn,instcombine<no-verify-fixpoint>,simplifycfg)",
        1,
        "default",
    ),
    "L1dg": (
        f"function({SIMPLE},instcombine<no-verify-fixpoint>,simplifycfg),"
        "cgscc(inline),"
        f"function({SIMPLE},instcombine<no-verify-fixpoint>,simplifycfg,loop-mssa(loop-rotate,licm),"
        "loop(loop-idiom,indvars,loop-deletion),correlated-propagation,gvn,instcombine<no-verify-fixpoint>,simplifycfg)",
        0,
        "default",
    ),
    "O1": ("default<O1>", 1, "default"),
    "O1g": ("default<O1>", 1, "global"),
    "O1c0": ("default<O1>", 0, "default"),
    "O2c1": ("default<O2>", 1, "default"),
    "O2": ("default<O2>", 2, "default"),
    "O3": ("default<O3>", 3, "default"),
}
MODES = {
    "whole": {"whole": True, "import": 0},
    "split": {"whole": False, "import": 0},
    "import": {"whole": False, "import": 40},
}


def log(name, row):
    RESULTS.mkdir(exist_ok=True)
    row["load"] = os.getloadavg()[0]
    row["at"] = time.strftime("%H:%M:%S")
    with open(RESULTS / f"{name}.jsonl", "a") as f:
        f.write(json.dumps(row) + "\n")


def quiet(limit=12.0, wait=90):
    """Waits for the machine to calm down, up to a point, so as not to compete."""
    start = time.time()
    while os.getloadavg()[0] > limit and time.time() - start < wait:
        time.sleep(5)


def must(r):
    if r["rc"] != 0:
        raise SystemExit(f"failed ({r['killed']}): {r['cmd']}\n{r['stderr'][-2000:]}")
    return r


def fresh(path):
    if path.exists():
        shutil.rmtree(path)
    path.mkdir(parents=True)


def emit(source, out, abi, import_):
    fresh(out)
    r = must(wd.run([PROTO / "llvm-spike", source, out, "--abi", abi, "--import", str(import_)]))
    return json.loads(r["stdout"]), r


def driver(files, passes, cg, isel, out, quiet_rows=True):
    cmd = [PROTO / "driver", "--passes", passes, "--cg", str(cg), "--isel", isel, "--out", out]
    if quiet_rows:
        cmd.append("--quiet")
    r = wd.run(cmd + [str(f) for f in files])
    if r["rc"] != 0:
        return None, r
    return json.loads(r["stdout"]), r


def link_args(out, objects):
    return [f"-o", out, "-arch", "arm64", "-platform_version", "macos", "14.0", "14.0", "-syslibroot", SDK, "-lSystem", "-dead_strip"] + [str(o) for o in objects]


def link(out, objects, which="ld"):
    # Enough objects to overflow a command line go through a file list.
    tool = ["/usr/bin/ld"] if which == "ld" else [f"{LLVM_BIN}/rust-lld", "-flavor", "darwin"]
    listing = out.with_suffix(".objs")
    listing.write_text("\n".join(str(o) for o in objects) + "\n")
    return must(wd.run(tool + link_args(out, []) + ["-filelist", listing]))


def run_exe(cmd, runs=3, env=None):
    best = None
    for _ in range(runs):
        r = must(wd.run(cmd, env=env))
        if best is None:
            best = r
            best["cycles_min"] = r["cycles"]
            best["wall_min"] = r["wall_ms_outer"]
            best["instructions_min"] = r["instructions"]
        else:
            best["cycles_min"] = min(best["cycles_min"], r["cycles"])
            best["wall_min"] = min(best["wall_min"], r["wall_ms_outer"])
            best["instructions_min"] = min(best["instructions_min"], r["instructions"])
    return best


def rt_object():
    out = WORK / "rt.o"
    must(wd.run(["clang", "-O2", "-c", Path(__file__).parent / "rt.c", "-o", out]))
    return out


def pinned():
    rows = {}
    for line in (ROOT / "bench/targets/aarch64.txt").read_text().splitlines():
        if line and not line.startswith("#"):
            parts = line.split()
            rows[parts[0]] = {"instructions": int(parts[1]), "micros": int(parts[2])}
    return rows


def baseline():
    """Twins, as pinned and with overflow checks; empty programs; the JIT on each kernel."""
    out = WORK / "twins"
    out.mkdir(parents=True, exist_ok=True)
    flags = ["--edition", "2024", "-C", "opt-level=3", "-C", "codegen-units=1", "-C", "panic=abort", "-C", "debug-assertions=no"]
    (out / "empty.rs").write_text("fn main() {}\n")
    (out / "empty.c").write_text("int main(void) { return 0; }\n")
    must(wd.run(["rustc"] + flags + [out / "empty.rs", "-o", out / "empty_rs"]))
    must(wd.run(["clang", "-O2", out / "empty.c", "-o", out / "empty_c"]))
    for name in ["empty_rs", "empty_c"]:
        r = run_exe([out / name], runs=5)
        log("baseline", {"what": name, "instructions": r["instructions_min"], "cycles": r["cycles_min"], "wall": r["wall_min"], "rss": r["rss"]})
    empty = WORK / "twins/empty.fen"
    empty.write_text('def main(stdout: Stdout):\n    stdout.print(1)\n')
    home = WORK / "fh_base"
    env = dict(os.environ, FENNEC_HOME=str(home))
    for kernel in KERNELS:
        quiet()
        for name, extra in [("plain", []), ("checked", ["-C", "overflow-checks=on"])]:
            exe = out / f"{kernel}_{name}"
            must(wd.run(["rustc"] + flags + extra + [ROOT / f"bench/kernels/{kernel}.rs", "-o", exe]))
            r = run_exe([exe], runs=5)
            log("baseline", {"what": f"twin_{name}", "kernel": kernel, "instructions": r["instructions_min"], "cycles": r["cycles_min"], "wall": r["wall_min"], "rss": r["rss"], "out": hashlib.sha256(r["stdout"].encode()).hexdigest()[:16]})
        source = ROOT / f"bench/kernels/{kernel}.fen"
        must(wd.run([FENNEC, "dev", "startup", source], env=env))
        run = run_exe([FENNEC, "run", source], runs=5, env=env)
        start = run_exe([FENNEC, "dev", "startup", source], runs=5, env=env)
        log("baseline", {"what": "jit", "kernel": kernel, "instructions": run["instructions_min"], "startup_instructions": start["instructions_min"], "cycles": run["cycles_min"], "startup_cycles": start["cycles_min"], "wall": run["wall_min"], "startup_wall": start["wall_min"], "rss": run["rss"], "out": hashlib.sha256(run["stdout"].encode()).hexdigest()[:16]})


def speed(tiers):
    rt = rt_object()
    for kernel in KERNELS:
        quiet()
        source = ROOT / f"bench/kernels/{kernel}.fen"
        for abi in ["checked", "plain"]:
            for mode, spec in MODES.items():
                out = WORK / "speed" / f"{kernel}_{abi}_{mode}"
                info, _ = emit(source, out, abi, spec["import"])
                files = [out / "whole.ll"] if spec["whole"] else sorted((out / "fn").glob("*.ll"))
                for tier in tiers:
                    passes, cg, isel = TIERS[tier]
                    objs = out / tier
                    fresh(objs)
                    stats, compile_run = driver(files, passes, cg, isel, objs)
                    if stats is None:
                        log("speed", {"kernel": kernel, "abi": abi, "mode": mode, "tier": tier, "failed": compile_run["stderr"][-400:]})
                        continue
                    exe = objs / "exe"
                    objects = sorted(objs.glob("*.o")) + [rt]
                    linked = link(exe, objects)
                    r = run_exe([exe], runs=5)
                    log("speed", {
                        "kernel": kernel, "abi": abi, "mode": mode, "tier": tier, "functions": info["functions"], "ir_insts": info["ir_insts"],
                        "parse_ms": stats["parse_ms"], "opt_ms": stats["opt_ms"], "cg_ms": stats["cg_ms"], "obj_bytes": stats["obj_bytes"],
                        "compile_instructions": compile_run["instructions"], "compile_rss": compile_run["rss"], "compile_footprint": compile_run["footprint"], "link_ms": linked["wall_ms_outer"],
                        "instructions": r["instructions_min"], "cycles": r["cycles_min"], "wall": r["wall_min"], "out": hashlib.sha256(r["stdout"].encode()).hexdigest()[:16],
                    })


def scale(n, tiers):
    # Cut short at Chris's request: no-opt, the best trimmed tier, -O1 and -O3 are enough.
    tiers = [tier for tier in tiers if tier in ("L0", "L1d", "O1", "O3")]
    big = WORK / f"big{n}"
    big.mkdir(parents=True, exist_ok=True)
    source = big / "big.fen"
    text = subprocess.run(["python3", Path(__file__).parent / "gen_big.py", str(n)], capture_output=True, text=True).stdout
    source.write_text(text)
    lines = text.count("\n")
    # Two edits: a loop in an ordinary function, and a small function many callers could inline.
    # Each keeps every byte offset, so no other function's spans move.
    edits = {"medium": text.index(f"def f{n // 2}(n: int) -> int:"), "small": text.index("def energy_0(p: P0) -> float:")}

    def edited(which, rep=0):
        chars = list(text)
        at = edits[which]
        body = text[at:at + 2000]
        if which == "small":
            place = at + body.index("0.5 *")
            chars[place + 2] = str(rep + 6)
        else:
            m = re.search(r"\d+\.\d+|\d{2,}", body[body.index("\n"):])
            place = at + body.index("\n") + m.start()
            digit = str(rep + 3)
            chars[place] = digit if chars[place] != digit else "9"
        return "".join(chars)

    # The JIT, first of all: cold, then nothing changed, then each edit.
    home = big / "fh"
    if home.exists():
        shutil.rmtree(home)
    env = dict(os.environ, FENNEC_HOME=str(home))
    for rep in range(3):
        if home.exists():
            shutil.rmtree(home)
        source.write_text(text)
        r = must(wd.run([FENNEC, "dev", "startup", source], env=env))
        log("scale", {"n": n, "lines": lines, "engine": "jit", "case": "cold", "wall": r["wall_ms_outer"], "instructions": r["instructions"], "rss": r["rss"], "footprint": r["footprint"]})
    disk = sum(p.stat().st_size for p in home.rglob("*") if p.is_file())
    files = sum(1 for p in home.rglob("*") if p.is_file())
    log("scale", {"n": n, "engine": "jit", "case": "disk", "bytes": disk, "files": files})
    for rep in range(3):
        r = must(wd.run([FENNEC, "dev", "startup", source], env=env))
        log("scale", {"n": n, "engine": "jit", "case": "noop", "wall": r["wall_ms_outer"], "instructions": r["instructions"], "rss": r["rss"], "footprint": r["footprint"]})
    for which in edits:
        for rep in range(3):
            source.write_text(text)
            must(wd.run([FENNEC, "dev", "startup", source], env=env))
            source.write_text(edited(which, rep))
            r = must(wd.run([FENNEC, "dev", "startup", source], env=env))
            log("scale", {"n": n, "engine": "jit", "case": f"edit_{which}", "wall": r["wall_ms_outer"], "instructions": r["instructions"], "rss": r["rss"], "footprint": r["footprint"]})
    source.write_text(text)
    r = run_exe([FENNEC, "run", source], runs=1, env=env)
    expected = r["stdout"]
    log("scale", {"n": n, "engine": "jit", "case": "output", "out": expected.strip()})

    rt = rt_object()
    for mode, spec in MODES.items():
        out = big / mode
        info, emit_run = emit(source, out, "checked", spec["import"])
        log("scale", {"n": n, "engine": "llvm", "case": "emit", "mode": mode, **info, "wall": emit_run["wall_ms_outer"], "rss": emit_run["rss"]})
        files = [out / "whole.ll"] if spec["whole"] else sorted((out / "fn").glob("*.ll"))
        for tier in tiers:
            quiet()
            passes, cg, isel = TIERS[tier]
            objs = out / tier
            fresh(objs)
            stats, cold = driver(files, passes, cg, isel, objs)
            row = {"n": n, "engine": "llvm", "mode": mode, "tier": tier, "case": "cold", "rss": cold["rss"], "footprint": cold["footprint"], "instructions": cold["instructions"], "wall": cold["wall_ms_outer"]}
            if stats is None:
                row["killed"] = cold["killed"] or cold["stderr"][-300:]
                log("scale", row)
                continue
            log("scale", {**row, **{k: stats[k] for k in ("parse_ms", "opt_ms", "cg_ms", "obj_bytes", "text_bytes", "init_ms")}})
            objects = sorted(objs.glob("*.o")) + [rt]
            exe = objs / "exe"
            for which in ["ld", "lld"]:
                for rep in range(3):
                    linked = link(exe, objects, which)
                    log("scale", {"n": n, "engine": "llvm", "mode": mode, "tier": tier, "case": f"link_{which}", "wall": linked["wall_ms_outer"], "instructions": linked["instructions"], "rss": linked["rss"]})
            r = run_exe([exe], runs=1)
            log("scale", {"n": n, "engine": "llvm", "mode": mode, "tier": tier, "case": "output", "out": r["stdout"].strip(), "matches_jit": r["stdout"] == expected, "exe_bytes": exe.stat().st_size, "wall": r["wall_ms_outer"]})
            if spec["whole"]:
                continue
            # One-function edits: lower again, compile what changed, link everything.
            before = {f.name: hashlib.sha256(f.read_bytes()).hexdigest() for f in files}
            for which in edits:
                edit_dir = big / f"{mode}_{tier}_{which}"
                changed_source = big / f"edit_{which}.fen"
                changed_source.write_text(edited(which))
                for rep in range(3):
                    info2, emit2 = emit(changed_source, edit_dir, "checked", spec["import"])
                    changed = [f for f in sorted((edit_dir / "fn").glob("*.ll")) if before.get(f.name) != hashlib.sha256(f.read_bytes()).hexdigest()]
                    objs2 = edit_dir / "objs"
                    fresh(objs2)
                    stats2, compiled = driver(changed, passes, cg, isel, objs2)
                    if stats2 is None:
                        log("scale", {"n": n, "mode": mode, "tier": tier, "case": f"edit_{which}", "killed": compiled["stderr"][-200:]})
                        break
                    kept = [objs / f"{f.stem}.o" for f in files if not (objs2 / f"{f.stem}.o").exists()]
                    objects2 = kept + sorted(objs2.glob("*.o")) + [rt]
                    linked = link(edit_dir / "exe", objects2)
                    lld = link(edit_dir / "exe_lld", objects2, "lld")
                    log("scale", {
                        "n": n, "engine": "llvm", "mode": mode, "tier": tier, "case": f"edit_{which}", "changed": len(changed), "front_ms": info2["front_ms"], "emit_all_ms": info2["emit_all_ms"],
                        "parse_ms": stats2["parse_ms"], "opt_ms": stats2["opt_ms"], "cg_ms": stats2["cg_ms"], "compile_wall": compiled["wall_ms_outer"], "init_ms": stats2["init_ms"], "compile_rss": compiled["rss"], "compile_footprint": compiled["footprint"],
                        "emit_rss": emit2["rss"], "emit_wall": emit2["wall_ms_outer"], "ld_wall": linked["wall_ms_outer"], "ld_rss": linked["rss"], "lld_wall": lld["wall_ms_outer"], "lld_rss": lld["rss"],
                    })
                shutil.rmtree(edit_dir)
            shutil.rmtree(objs)


if __name__ == "__main__":
    command = sys.argv[1]
    if command == "baseline":
        baseline()
    elif command == "speed":
        speed(sys.argv[2:] or list(TIERS))
    elif command == "scale":
        scale(int(sys.argv[2]), sys.argv[3:] or list(TIERS))
