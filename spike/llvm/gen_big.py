#!/usr/bin/env python3
"""Generates the large program: N functions of about 20 lines, mixing int loops, float math,
lists and struct methods, with every function reached and little work at run time.

usage: gen_big.py N > big.fen
"""
import random, sys

n = int(sys.argv[1])
rng = random.Random(1234)
GROUP = 50
out = ["## Generated. Every function is reached, and each does a tiny amount of work.", "", "from std.math import sqrt", ""]
for s in range(max(1, n // 50)):
    out += [f"struct P{s}:", "    x: float", "    y: float", "    vx: float", "    vy: float", "    weight: int", "",
            f"def energy_{s}(p: P{s}) -> float:", f"    0.5 * float(p.weight) * (p.vx * p.vx + p.vy * p.vy) + {s}.5", "",
            f"def step_{s}(p: P{s}, dt: float) -> P{s}:", "    mut q = p", "    q.x = q.x + q.vx * dt", "    q.y = q.y + q.vy * dt",
            "    q.vx = q.vx * 0.99", "    q.vy = q.vy * 0.99 - 0.1 * dt", "    q", ""]
structs = max(1, n // 50)
names = []
for i in range(n):
    name = f"f{i}"
    prev = names[rng.randrange(len(names))] if names else None
    c1, c2, c3 = rng.randrange(2, 1000), rng.randrange(1000, 100000), rng.randrange(3, 97)
    call = f"{prev}(n - 1)" if prev else f"n + {c1}"
    shape = i % 4
    if shape == 0:
        out += [f"def {name}(n: int) -> int:", f"    mut total = {c1}", "    for i in range(n):",
                f"        total = (total * 31 + i) % {c2 * 7 + 3}", "        if total % 2 == 0:", f"            total = total + {c3}", "        else:",
                f"            total = total - i", f"    mut scan = {c3}", "    while scan > 1:", "        if scan % 2 == 0:", "            scan = scan // 2", "        else:",
                "            scan = scan - 1", "        total = total + 1", "    if n <= 0:", "        return total", f"    total + {call}", ""]
    elif shape == 1:
        s = rng.randrange(structs)
        out += [f"def {name}(n: int) -> int:", f"    mut p = P{s}(x=1.5, y=2.5, vx={c1}.25, vy=-{c3}.75, weight=n + 1)", "    mut steps = 0",
                "    while steps < n:", f"        p = step_{s}(p, 0.01)", "        steps = steps + 1", "    mut e = 0.0", f"    e = e + energy_{s}(p)",
                "    mut d = sqrt(p.x * p.x + p.y * p.y + 1.0)", "    if d > 1000.0:", "        d = d / 1000.0", "    if n <= 0:", f"        return {c3}",
                f"    if e > {c2}.0:", f"        return {call} + 1", f"    {call} + {c1}", ""]
    elif shape == 2:
        out += [f"def {name}(n: int) -> int:", "    mut items = [0 for _ in range(n + 3)]", "    mut at = 0", "    while at < items.len():",
                f"        items[at] = (at * {c3} + {c1}) % {c2}", "        at = at + 1", "    mut best = 0", "    for i in range(items.len()):", "        if items[i] > best:",
                "            best = items[i]", "    mut low = 0", "    mut high = items.len() - 1", "    while low < high:", "        held = items[low]", "        items[low] = items[high]",
                "        items[high] = held", "        low = low + 1", "        high = high - 1", "    if n <= 0:", "        return best + items[0]", f"    best + {call}", ""]
    else:
        out += [f"def {name}(n: int) -> int:", "    mut sum = 0.0", "    mut vs = [1.5, 2.5, " + f"{c1}.5, {c3}.25]", "    for i in range(vs.len()):",
                f"        sum = sum + vs[i] * float(i + {c3})", "    mut count = 0", "    for a in range(n + 2):", "        for b in range(n + 2):",
                f"            if (a * b + {c1}) % {c3} == 0:", "                count = count + 1", "    r = 0 if sum < 0.0 else 1", "    if n <= 0:", "        return count + r", f"    count + r + {call}", ""]
    names.append(name)
for g in range(0, n, GROUP):
    out += [f"def group{g}(n: int) -> int:", "    mut total = 0"]
    for name in names[g:g + GROUP]:
        out.append(f"    total = (total + {name}(n)) % 1000003")
    out += ["    total", ""]
out += ["def main(stdout: Stdout):", "    mut total = 0"]
for g in range(0, n, GROUP):
    out.append(f"    total = (total + group{g}(2)) % 1000003")
out += ["    stdout.print(total)", ""]
print("\n".join(out))
