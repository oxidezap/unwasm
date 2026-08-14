#!/usr/bin/env python3
"""Give each `70008` in `make_and_cache_offer` its own error code.

Why this exists
---------------

`wa_call_start_internal` logs `make_and_cache_offer failed: 70008`, and
`f11198_make_and_cache_offer` sets that same 70008 at nine different sites. The
code alone cannot say which one fired.

Each site is guarded by a `voip_assert` carrying its own `offer.cc` line, so the
obvious move is to make the assert observable. That was tried and produced
contradictory measurements — see VOIP_STATUS.md. This is the move that avoids
the question entirely: renumber the sites.

`i32.const 70008` is four bytes (`41 f8 a2 04`), and so is `i32.const 70001`
(`41 f1 a2 04`) — the sleb128 encoding only changes in its low seven bits for
anything in this range. So each site can be given a distinct code without
resizing the module, without touching control flow, and without writing to
memory. The engine then prints the answer itself: `failed: 70003` names site 3.

The mapping is positional, in ascending byte order, and the sites correspond to
the table in VOIP_STATUS.md:

    70001  offer.cc:430                          a byte flag on the call object
    70002  offer.cc:463                          f10539(call) returned null
    70003  offer.cc:485                          f10530(call, …) returned null
    70004  wa_call_signaling_handler.cc:296      key index >= 6
    70005  (no assert)                           local certificate fingerprint
    70006  offer.cc:767                          a count that must be > 0
    70007  offer.cc:776                          a pointer that must be non-null
    70009  offer.cc:784                          the same pointer, again
    70010  offer.cc:789                          falls through

70008 itself is skipped so that an unpatched site — or a tenth one this script
does not know about — still reads as 70008 rather than impersonating a tagged
one.

This is an instrument. Anything measured against a patched capture must say so.

Usage: tag_offer_error_sites.py <src-dir> <dst-dir>
"""

import shutil
import sys
from pathlib import Path

MODULE = "D5pLH9sfOOl.wasm"

# From `unwasm constants <module> 70008`, filtered to the sites inside
# f11198_make_and_cache_offer (body 0x4dbed7..0x4dceb1).
SITES = [5095464, 5096178, 5096316, 5097017, 5097359, 5098981, 5099045, 5099100, 5099125]
CODES = [70001, 70002, 70003, 70004, 70005, 70006, 70007, 70009, 70010]


def sleb(value: int) -> bytes:
    out = bytearray()
    while True:
        byte = value & 0x7F
        value >>= 7
        done = (value == 0 and not byte & 0x40) or (value == -1 and byte & 0x40)
        out.append(byte if done else byte | 0x80)
        if done:
            return bytes(out)


ORIGINAL = b"\x41" + sleb(70008)


def main() -> int:
    if len(sys.argv) != 3:
        print("usage: tag_offer_error_sites.py <src-dir> <dst-dir>", file=sys.stderr)
        return 2

    src, dst = Path(sys.argv[1]), Path(sys.argv[2])
    if not (src / MODULE).is_file():
        print(f"{src / MODULE}: not found", file=sys.stderr)
        return 1

    if src.resolve() != dst.resolve():
        dst.mkdir(parents=True, exist_ok=True)
        for wasm in src.glob("*.wasm"):
            shutil.copy2(wasm, dst / wasm.name)

    target = dst / MODULE
    data = bytearray(target.read_bytes())

    for at, code in zip(SITES, CODES):
        patch = b"\x41" + sleb(code)
        # Every replacement must be the same width as what it replaces, or every
        # byte offset after it moves and nothing else in this repository — the
        # other patch scripts, the offsets from `unwasm` — still applies.
        if len(patch) != len(ORIGINAL):
            print(f"{code} does not encode in {len(ORIGINAL)} bytes", file=sys.stderr)
            return 1
        found = bytes(data[at : at + len(ORIGINAL)])
        if found == patch:
            continue
        if found != ORIGINAL:
            print(
                f"{target}: expected `i32.const 70008` at {at:#x}, found "
                f"{found.hex()} — re-derive the sites with "
                f"`unwasm constants {MODULE} 70008`",
                file=sys.stderr,
            )
            return 1
        data[at : at + len(patch)] = patch

    target.write_bytes(data)
    print(f"{target}: {len(SITES)} offer error sites given distinct codes")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
