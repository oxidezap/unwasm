#!/usr/bin/env python3
"""Neutralise emscripten's thread-status profiler in the VoIP module.

Why this exists
---------------

`f12302` is emscripten's `emscripten_conditional_set_current_thread_status`. It
is reached from the futex wait path, which every engine worker sits in, and its
body is:

    if (*(u8*)0x14B958 == 0) return;      // profiler disabled — the normal case
    if (pthread_self() == 0) return;
    load(load_atomic(pthread_self() + 112));   // the profiler block

`0x14B958` is written by nothing this module ever calls: the only writer is
`emscripten_thread_profiler_enable` (`f13134`), which has zero call sites and
zero table slots. The flag should therefore be zero for the whole run, and it
reads zero both before a worker enters its routine and after one returns.

It is not zero when the sampler runs. Forcing this test to fail takes the run
from 27 engine log lines and eleven traps to 167 lines and none — so the flag
had to have been non-zero, and static data at `0x14B958` is being corrupted at
runtime. The same corruption is what puts an out-of-linear-memory pointer in
`pthread + 112`, which is where the traps land.

So this is an instrument, not a fix: it masks a symptom of a memory corruption
that is still unexplained, in order to let the engine run far enough to be
studied. Anything measured against a patched capture has to say so.

The patch
---------

The guard's `i32.load8_u` (3 bytes, `2d 00 00`) becomes `drop; i32.const 0`
(3 bytes, `1a 41 00`): the address is discarded and the test always reads zero,
so the function returns immediately. Same length, so every other byte offset in
the module is unchanged and offsets from `unwasm` still line up.

Usage: neutralize_thread_profiler.py <src-dir> <dst-dir>
"""

import shutil
import sys
from pathlib import Path

# The guard's `i32.const 0x14B958`, and the load that follows it. Both are found
# by `unwasm constants <module> 1358168`, which reports the push site; the load
# is the next instruction.
FLAG_ADDRESS = 1358168
CONST_AT = 8338642
CONST_BYTES = bytes.fromhex("41d8f2d200")  # i32.const 1358168
LOAD_BYTES = bytes.fromhex("2d0000")  # i32.load8_u align=0 offset=0
PATCH_BYTES = bytes.fromhex("1a4100")  # drop ; i32.const 0

MODULE = "D5pLH9sfOOl.wasm"


def main() -> int:
    if len(sys.argv) != 3:
        print(__doc__.strip().splitlines()[-1], file=sys.stderr)
        return 2

    src, dst = Path(sys.argv[1]), Path(sys.argv[2])
    if not (src / MODULE).is_file():
        print(f"{src / MODULE}: not found", file=sys.stderr)
        return 1

    dst.mkdir(parents=True, exist_ok=True)
    for wasm in src.glob("*.wasm"):
        shutil.copy2(wasm, dst / wasm.name)

    target = dst / MODULE
    data = bytearray(target.read_bytes())

    # Refuse to patch a module whose bytes are not the ones this was written
    # against: a silently misplaced three-byte write would corrupt the module
    # into something that still loads and behaves differently for no visible
    # reason, which is far worse than not running.
    at = CONST_AT
    if bytes(data[at : at + len(CONST_BYTES)]) != CONST_BYTES:
        print(
            f"{target}: expected `i32.const {FLAG_ADDRESS}` at {at:#x}, "
            f"found {bytes(data[at : at + len(CONST_BYTES)]).hex()} — "
            "this is a different capture; re-derive the site with "
            f"`unwasm constants {MODULE} {FLAG_ADDRESS}`",
            file=sys.stderr,
        )
        return 1

    load_at = at + len(CONST_BYTES)
    found = bytes(data[load_at : load_at + len(LOAD_BYTES)])
    if found == PATCH_BYTES:
        print(f"{target}: already patched")
        return 0
    if found != LOAD_BYTES:
        print(
            f"{target}: expected `i32.load8_u` at {load_at:#x}, found {found.hex()}",
            file=sys.stderr,
        )
        return 1

    data[load_at : load_at + len(PATCH_BYTES)] = PATCH_BYTES
    target.write_bytes(data)
    print(f"{target}: thread-status profiler neutralised at {load_at:#x}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
