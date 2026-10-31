#!/usr/bin/env python3
"""Minimum of each stage across the runs of each edit, from results/edit_to_run.jsonl."""
import json
from collections import defaultdict
from pathlib import Path
rows = [json.loads(l) for l in open(Path(__file__).parent / "results/edit_to_run.jsonl")]
for r in rows:
    if r["what"] == "cold":
        print(f"cold {r['program']} import={r['import']} {r['tier']}: llvm {r['llvm_ms']/1000:.1f}s, with file writes {r['wall']/1000:.1f}s, rss {r['rss']/1e6:.0f}M, cache {r['disk']/1e6:.2f}MB, front {r['front']}")
print()
for r in rows:
    if r["what"] in ("link_all", "link_shards"):
        print(f"{r['what']:12} {r['program']} i{r['import']} {r['tier']} {r['linker']:9} {r['wall']:.0f} ms rss {r['rss']/1e6:.0f}M", f"exe {r.get('exe_bytes',0)/1e6:.2f}MB run {r.get('run_wall',0):.0f}ms" if r['what']=='link_all' else '')
    if r["what"] == "shards_built":
        print(f"shards built {r['program']} i{r['import']} {r['tier']}: {r['wall']:.0f} ms")
print()
edits = defaultdict(list)
for r in rows:
    if r["what"] == "edit":
        edits[(r["program"], r["import"], r["tier"], r["edit"])].append(r)
K = ["load_ms", "check_ms", "recheck_ms", "lower_ms", "hash_all_ms", "emit_changed_ms", "llvm_ms", "compile_process_wall", "link_ld", "link_ld_fast", "link_lld", "link_lld_fast", "remerge_ms", "link_shards_lld_fast", "run_wall"]
for key, v in edits.items():
    m = {k: min(x[k] for x in v) for k in K}
    front = m["load_ms"] + m["check_ms"] + m["recheck_ms"] + m["lower_ms"] + m["hash_all_ms"]
    best_link = min(m["link_ld"], m["link_ld_fast"], m["link_lld"], m["link_lld_fast"])
    total = front + m["emit_changed_ms"] + m["llvm_ms"] + best_link + m["run_wall"]
    no_front = m["emit_changed_ms"] + m["llvm_ms"] + best_link + m["run_wall"]
    print(key, f"changed {v[0]['changed']} TOTAL {total:.0f} (front {front:.0f} = load {m['load_ms']:.0f} check {m['check_ms']:.0f} recheck {m['recheck_ms']:.0f} lower {m['lower_ms']:.0f} hash {m['hash_all_ms']:.0f}) emit {m['emit_changed_ms']:.1f} llvm {m['llvm_ms']:.1f} (with files {m['compile_process_wall']:.0f}) link ld {m['link_ld']:.0f} ldf {m['link_ld_fast']:.0f} lld {m['link_lld']:.0f} lldf {m['link_lld_fast']:.0f} shards {m['remerge_ms']:.0f}+{m['link_shards_lld_fast']:.0f} run {m['run_wall']:.0f}; without front {no_front:.0f}")
print()
for label, v in {"JIT prior": []}.items():
    pass
