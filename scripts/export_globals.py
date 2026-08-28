#!/usr/bin/env python3
"""Add exports for a module's globals, so a host can read them.

The VoIP engine keeps its call context in a wasm global (`global.get 10` in
startVoipCall) and exports no globals at all, so wasmtime cannot reach it — and
that is what has blocked every attempt to read `call->[48]` and
`group->[592]` directly. This produces an inspection-only copy with the globals
exported under `__global_<n>`.

Surgical: only the export section is touched. Everything else is copied byte for
byte.
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


def read_uleb(data, i):
    n = shift = 0
    while True:
        b = data[i]
        i += 1
        n |= (b & 0x7F) << shift
        if not b & 0x80:
            return n, i
        shift += 7


def main(src, dst, count):
    data = open(src, "rb").read()
    assert data[:4] == b"\0asm", "not a wasm module"

    i = 8
    while i < len(data):
        sid = data[i]
        size, after_size = read_uleb(data, i + 1)
        body_start, body_end = after_size, after_size + size

        if sid == 7:  # export section
            n, entries_start = read_uleb(data, body_start)
            entries = data[entries_start:body_end]

            added = bytearray()
            for g in range(count):
                name = f"__global_{g}".encode()
                added += uleb(len(name)) + name + b"\x03" + uleb(g)

            body = uleb(n + count) + entries + bytes(added)
            patched = data[:i] + bytes([7]) + uleb(len(body)) + body + data[body_end:]
            open(dst, "wb").write(patched)
            print(f"exports {n} -> {n + count}; {len(data)} -> {len(patched)} bytes")
            return 0

        i = body_end

    print("no export section found", file=sys.stderr)
    return 1


if __name__ == "__main__":
    sys.exit(main(sys.argv[1], sys.argv[2], int(sys.argv[3])))
