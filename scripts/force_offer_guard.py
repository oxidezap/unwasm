#!/usr/bin/env python3
"""Force open the guard that stops an outgoing offer.

`make_and_cache_offer` returns 70008 from `offer.cc:430` when both terms of

    if (arg2 == 0 && ctx->[662166] == 0)

are zero, and both are — they come from configuration a server would have
supplied. Writing them from the host does not survive, because the call
re-initialises the context. This patches a copy of the module so the first term
reads as non-zero instead.

The substitution is length-preserving, so nothing else moves:

    local.get 2   20 02   ->   i32.const 1   41 01

It is found by the byte pattern of the guard itself, which is distinctive:

    20 02        local.get 2
    0D 00        br_if 0
    20 00        local.get 0
    2D 00 ...    i32.load8_u align=0 offset=662166
    0D 00        br_if 0

For inspection only — this changes what the module does.
"""
import sys


def uleb(n):
    out = bytearray()
    while True:
        b = n & 0x7F
        n >>= 7
        out.append(b | (0x80 if n else 0))
        if not n:
            return bytes(out)


def main(src, dst):
    data = bytearray(open(src, "rb").read())

    guard = (
        b"\x20\x02"  # local.get 2
        b"\x0d\x00"  # br_if 0
        b"\x20\x00"  # local.get 0
        b"\x2d\x00" + uleb(662_166) + b"\x0d\x00"  # i32.load8_u offset=662166  # br_if 0
    )

    hits = []
    at = data.find(guard)
    while at != -1:
        hits.append(at)
        at = data.find(guard, at + 1)

    if not hits:
        print("guard pattern not found — the module or the offsets changed", file=sys.stderr)
        return 1
    if len(hits) > 1:
        print(f"pattern is not unique ({len(hits)} hits); refusing to guess", file=sys.stderr)
        return 1

    at = hits[0]
    data[at : at + 2] = b"\x41\x01"  # i32.const 1
    open(dst, "wb").write(bytes(data))
    print(f"patched guard at file offset {at}: local.get 2 -> i32.const 1")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1], sys.argv[2]))
