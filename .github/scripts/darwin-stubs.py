#!/usr/bin/env python3
"""Writes the stubs LLD links a Mac program against, in place of Xcode's SDK, so building on a Mac
needs nothing of Xcode's. A stub names a system library and the symbols it exports, and nothing
more: no code. These name only what Fennec's programs can call, which is what the runtime
libraries leave undefined, and what LLVM's own code calls, each found in the SDK's own stub for it.

    darwin-stubs.py <SDK> <folder> <runtime library>...

with `NM` naming LLVM's `llvm-nm`, which reads the bitcode the runtime libraries hold, as the
system's `nm` may not. It lays them out in <folder> as an SDK does, for `-syslibroot`: `usr/lib/libSystem.tbd`, and
`System/Library/Frameworks/<name>.framework/<name>.tbd` for each framework a program links.
"""

import os
import re
import subprocess
import sys
from pathlib import Path

# Each library a program links, by where its stub goes and the name a program loads it by.
LIBRARIES = [
    ("usr/lib/libSystem.tbd", "usr/lib/libSystem.B.tbd", "/usr/lib/libSystem.B.dylib"),
    (
        "System/Library/Frameworks/Security.framework/Security.tbd",
        "System/Library/Frameworks/Security.framework/Security.tbd",
        "/System/Library/Frameworks/Security.framework/Versions/A/Security",
    ),
    (
        "System/Library/Frameworks/CoreFoundation.framework/CoreFoundation.tbd",
        "System/Library/Frameworks/CoreFoundation.framework/CoreFoundation.tbd",
        "/System/Library/Frameworks/CoreFoundation.framework/Versions/A/CoreFoundation",
    ),
]

# What LLVM's code calls itself, in a program's own functions as well as the runtime's.
FROM_LLVM = ["_memcpy", "_memmove", "_memset", "_bzero", "___stack_chk_fail", "___stack_chk_guard"]

# What the linker makes itself, which no library exports.
FROM_THE_LINKER = {"___dso_handle"}

TARGETS = "[ x86_64-macos, arm64-macos ]"


def undefined(libraries):
    """Every symbol the runtime libraries call that none of their own parts defines."""
    def listed(*flags):
        found = set()
        for library in libraries:
            nm = os.environ.get("NM", "llvm-nm")
            ran = subprocess.run([nm, *flags, "-j", library], capture_output=True, text=True, check=True)
            found.update(line.strip() for line in ran.stdout.splitlines() if line.strip().startswith("_"))
        return found

    # A part of a library calls what another part defines, which isn't the system's.
    return (listed("-u") - listed("-g", "-U")) | set(FROM_LLVM)


def exported(stub):
    """Every symbol an SDK stub names, from anything it re-exports too."""
    return set(re.findall(r"(?<![A-Za-z0-9_$.])_[A-Za-z0-9_$.]+", stub.read_text()))


def written(install_name, symbols):
    listed = ",\n                       ".join(f"'{name}'" for name in sorted(symbols))
    return (
        "--- !tapi-tbd\n"
        "tbd-version:     4\n"
        f"targets:         {TARGETS}\n"
        f"install-name:    '{install_name}'\n"
        "exports:\n"
        f"  - targets:         {TARGETS}\n"
        f"    symbols:         [ {listed} ]\n"
        "...\n"
    )


def main():
    if len(sys.argv) < 4:
        sys.exit(__doc__)
    sdk, folder, libraries = Path(sys.argv[1]), Path(sys.argv[2]), sys.argv[3:]
    wanted = undefined(libraries)
    found = set()
    for place, source, install_name in LIBRARIES:
        symbols = wanted & exported(sdk / source)
        found |= symbols
        path = folder / place
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(written(install_name, symbols))
        print(f"{place}: {len(symbols)} symbols")
    # What a runtime library calls that no system library exports is a mistake in the list above,
    # or a library missing from it, which linking a program would only find later.
    missing = sorted(name for name in wanted - found - FROM_THE_LINKER if name not in FROM_LLVM)
    if missing:
        sys.exit("no system library exports: " + ", ".join(missing))


main()
