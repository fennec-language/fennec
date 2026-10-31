#!/usr/bin/env python3
"""No JIT at all: edit a function, then front end, cache lookup, compile what changed, link, run.

Every stage is timed separately and for real, on the 1000-function program, with each linker and
link strategy. What the prototype can't do yet is stated: it writes every module out to find the
changes, so that's replaced by the time to hash each function's IR plus emitting the changed ones.
"""
import hashlib, json, os, shutil, subprocess, sys, time
from pathlib import Path
import measure as m
import orc_measure as o
import wd

WORK = m.WORK
OUT = m.RESULTS / "edit_to_run.jsonl"
LLD = [f"{m.LLVM_BIN}/rust-lld", "-flavor", "darwin"]
BASE_FLAGS = ["-arch", "arm64", "-platform_version", "macos", "14.0", "14.0", "-syslibroot", m.SDK, "-lSystem"]
LINKERS = {
    "ld": ["/usr/bin/ld"] + BASE_FLAGS,
    "ld_fast": ["/usr/bin/ld"] + BASE_FLAGS + ["-no_uuid", "-no_deduplicate"],
    "lld": LLD + BASE_FLAGS,
    "lld_fast": LLD + BASE_FLAGS + ["-no_uuid"],
}
SHARDS = 16


def note(row):
    row["load"] = os.getloadavg()[0]
    row["at"] = time.strftime("%H:%M:%S")
    with open(OUT, "a") as f:
        f.write(json.dumps(row) + "\n")


def link(linker, exe, objects):
    listing = exe.with_suffix(".objs")
    listing.write_text("\n".join(str(p) for p in objects) + "\n")
    return m.must(wd.run(LINKERS[linker] + ["-o", exe, "-filelist", listing]))


def merge(out, objects):
    listing = out.with_suffix(".objs")
    listing.write_text("\n".join(str(p) for p in objects) + "\n")
    return m.must(wd.run(["/usr/bin/ld", "-r", "-arch", "arm64", "-o", out, "-filelist", listing]))


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def run_program(label, text, edits, n, import_, tier, rt, home):
    big = WORK / f"e2r{n}"
    source = big / f"{label}.fen"
    source.write_text(text)
    env = dict(os.environ, FENNEC_HOME=str(home))
    expected = m.must(wd.run([m.FENNEC, "run", source], env=env))["stdout"]
    emitted = big / f"{label}_i{import_}"
    info, _ = m.emit(source, emitted, "checked", import_)
    files = sorted((emitted / "fn").glob("*.ll"))
    before = {f.name: sha(f) for f in files}
    passes, cg, isel = m.TIERS[tier]
    objs = big / f"{label}_i{import_}_{tier}"
    m.fresh(objs)
    stats, cold = m.driver_all(files, passes, cg, isel, objs)
    objects = sorted(objs.glob("*.o")) + [rt]
    disk = sum(p.stat().st_size for p in objs.glob("*.o"))
    note({"what": "cold", "program": label, "import": import_, "tier": tier, "wall": cold["wall_ms_outer"], "llvm_ms": stats["parse_ms"] + stats["opt_ms"] + stats["cg_ms"], "rss": cold["rss"], "footprint": cold["footprint"], "disk": disk, "functions": info["functions"], "front": {k: info[k] for k in ("load_ms", "check_ms", "recheck_ms", "lower_ms", "hash_all_ms", "front_ms", "emit_one_max_ms")}})
    exe = objs / "exe"
    for linker in LINKERS:
        walls = [link(linker, exe, objects) for _ in range(5)]
        best = min(walls, key=lambda r: r["wall_ms_outer"])
        run = m.must(wd.run([exe]))
        assert run["stdout"] == expected, (linker, run["stdout"], expected)
        note({"what": "link_all", "program": label, "import": import_, "tier": tier, "linker": linker, "wall": best["wall_ms_outer"], "rss": best["rss"], "exe_bytes": exe.stat().st_size, "run_wall": run["wall_ms_outer"]})

    # Shards: the unchanged functions stay merged, so the final link sees a few files.
    shard_dir = objs / "shards"
    m.fresh(shard_dir)
    named = sorted(objs.glob("*.o"))
    groups = [named[i::SHARDS] for i in range(SHARDS)]
    start = time.monotonic()
    shard_files = []
    for at, group in enumerate(groups):
        out = shard_dir / f"s{at}.o"
        merge(out, group)
        shard_files.append(out)
    note({"what": "shards_built", "program": label, "import": import_, "tier": tier, "wall": (time.monotonic() - start) * 1000})
    for linker in LINKERS:
        best = min((link(linker, exe, shard_files + [rt]) for _ in range(5)), key=lambda r: r["wall_ms_outer"])
        note({"what": "link_shards", "program": label, "import": import_, "tier": tier, "linker": linker, "wall": best["wall_ms_outer"], "rss": best["rss"]})

    for which, at in edits.items():
        edited = o.edit_text(text, at, which)
        changed_source = big / f"{label}_{which}.fen"
        changed_source.write_text(edited)
        want = m.must(wd.run([m.FENNEC, "run", changed_source], env=env))["stdout"]
        for rep in range(3):
            out = big / f"{label}_i{import_}_{which}"
            info2, emit_run = m.emit(changed_source, out, "checked", import_)
            changed = [f for f in sorted((out / "fn").glob("*.ll")) if before.get(f.name) != sha(f)]
            new_objs = out / "objs"
            m.fresh(new_objs)
            stats2, compiled = m.driver_all(changed, passes, cg, isel, new_objs)
            fresh_names = {p.name for p in new_objs.glob("*.o")}
            final = [objs / f"{f.stem}.o" for f in files if f"{f.stem}.o" not in fresh_names] + sorted(new_objs.glob("*.o")) + [rt]
            row = {
                "what": "edit", "program": label, "import": import_, "tier": tier, "edit": which, "changed": len(changed),
                "load_ms": info2["load_ms"], "check_ms": info2["check_ms"], "recheck_ms": info2["recheck_ms"], "lower_ms": info2["lower_ms"], "front_ms": info2["front_ms"],
                "hash_all_ms": info2["hash_all_ms"], "emit_changed_ms": info2["emit_one_max_ms"] * len(changed), "emit_all_ms": info2["emit_all_ms"],
                "llvm_ms": stats2["parse_ms"] + stats2["opt_ms"] + stats2["cg_ms"], "compile_process_wall": compiled["wall_ms_outer"], "init_ms": stats2["init_ms"], "compile_rss": compiled["rss"],
            }
            ran = None
            for linker in LINKERS:
                e = out / f"exe_{linker}"
                linked = link(linker, e, final)
                row[f"link_{linker}"] = linked["wall_ms_outer"]
                ran = m.must(wd.run([e]))
                assert ran["stdout"] == want, (linker, which)
            row["run_wall"] = ran["wall_ms_outer"]
            # The changed shards only: merge them again, then link the few files.
            hit = {at for at, group in enumerate(groups) for p in group if p.name in fresh_names}
            start = time.monotonic()
            shards_now = list(shard_files)
            for at in hit:
                group = [new_objs / p.name if p.name in fresh_names else p for p in groups[at]]
                shard = out / f"s{at}.o"
                merge(shard, group)
                shards_now[at] = shard
            row["remerge_ms"] = (time.monotonic() - start) * 1000
            row["shards_remerged"] = len(hit)
            e = out / "exe_shard"
            linked = link("lld_fast", e, shards_now + [rt])
            row["link_shards_lld_fast"] = linked["wall_ms_outer"]
            check = m.must(wd.run([e]))
            assert check["stdout"] == want, which
            note(row)
        shutil.rmtree(out)
    shutil.rmtree(objs)


def main(n):
    rt = m.rt_object()
    big = WORK / f"e2r{n}"
    big.mkdir(parents=True, exist_ok=True)
    home = big / "fh"
    for label, wide in [("normal", False), ("wide", True)]:
        args = ["python3", Path(__file__).parent / "gen_big.py", str(n)] + (["wide"] if wide else [])
        text = subprocess.run(args, capture_output=True, text=True).stdout
        edits = {"wide": text.index("def helper(")} if wide else {"medium": text.index(f"def f{n // 2}(n: int) -> int:"), "small": text.index("def energy_0(p: P0) -> float:")}
        for import_ in [40, 0]:
            for tier in ["O3", "O1"]:
                m.quiet(limit=10, wait=120)
                run_program(label, text, edits, n, import_, tier, rt, home)


def only_normal(n):
    """A larger program, one setting, to see how each stage grows."""
    rt = m.rt_object()
    big = WORK / f"e2r{n}"
    big.mkdir(parents=True, exist_ok=True)
    text = subprocess.run(["python3", Path(__file__).parent / "gen_big.py", str(n)], capture_output=True, text=True).stdout
    edits = {"medium": text.index(f"def f{n // 2}(n: int) -> int:"), "small": text.index("def energy_0(p: P0) -> float:")}
    m.quiet(limit=10, wait=120)
    run_program("normal", text, edits, n, 40, "O3", rt, big / "fh")


if __name__ == "__main__":
    if len(sys.argv) > 2:
        only_normal(int(sys.argv[1]))
    else:
        main(int(sys.argv[1]))
