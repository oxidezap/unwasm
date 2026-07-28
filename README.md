# unwasm

A WebAssembly decompiler that emits Rust — and whose output always compiles,
always runs, and is checked against the module it came from.

```console
$ unwasm decompile module.wasm -o module.rs
wrote module.rs (73657 lines, 478 functions)
```

The claim other decompilers cannot make is the one this project is built
around: **the decompilation is executable, so its faithfulness is measured
rather than asserted.** Every test below runs the same calls twice — once on the
wasm module in V8, once on the generated Rust — and compares the returned value,
the trap, and the final contents of linear memory.

## Why another one

The field, as of July 2026:

| tool | output | state |
|---|---|---|
| **wasm2c** (wabt) | C | maintained; a faithful transpiler, not readable as source |
| **wasm-decompile** (wabt) | a C-like language of its own | maintained; does not compile |
| **ghidra-wasm-plugin** | Ghidra pseudo-C | maintained; best open-source reading experience |
| **JEB** | pseudo-C | commercial |
| **rewasm** | Rust-ish | last commit 2021, "proper type recovery still missing" |
| **wasm2rs** | Rust | last commit 2022; no `loop`, no `if/else`, no globals |
| **WaDec** (fine-tuned LLM) | C | 52% recompiles, 43% re-executes |

Nothing maintained targets Rust, and nothing at all treats "does the output
actually behave like the module" as a property to test rather than a hope.

## What it does today — level 0

A faithful translation. Linear memory is a `Vec<u8>`, wasm's four value types
are their Rust counterparts, and every trap is a trap:

```rust
fn f2(&mut self, p0: i32) -> i32 {
    let mut l0: i32 = p0;
    let mut l1: i32 = 0;
    let t0: i32 = ((l0 <= 0i32) as i32);
    'b0: { if t0 != 0 {
        return 0i32;
    } }
    let t1: i32 = l0.wrapping_sub(1i32);
    l1 = t1;
    let t2: i64 = (l1 as u32 as i64);
    // ... the closed form LLVM turned this loop into
}
```

That is a `for` loop summing `0..n` in the C source. The decompilation shows
what the module *does*, which is Gauss's formula — because that is what the
compiler emitted. Level 0 does not pretend otherwise.

Concretely:

- **The whole MVP instruction set**, plus sign extension and the saturating
  truncations: 136 numeric opcodes, all of memory, `call_indirect`, `br_table`,
  passive segments.
- **Structure is taken, not recovered.** wasm's `block`/`loop`/`if` become
  Rust's labelled blocks and loops; `br` becomes `break 'bN` or `continue 'bN`.
  There is no CFG reconstruction in this codebase, because there is no CFG in
  the input.
- **Imports become a trait.** A host implements `Imports`; the default
  `NoImports` traps on every call, so "nobody supplied a host" never looks like
  "the host returned 0".
- **Nothing is skipped.** An opcode with no faithful Rust form is an error
  naming the construct — never a comment in the output, never a stub.
- **No `unsafe`.** `unsafe_code = "forbid"` for this crate and for what it
  emits. A decompiler that needs raw pointers to model a sandbox has lost the
  property that made the sandbox worth reading.

### On the real modules

Five of the six WhatsApp Web captures decompile, and the sixth is refused by
name:

```
COs9e0Kj0ic:   478 functions,  73703 lines of Rust  (VOPRF/crypto, 236 KiB)
php8T1oSIZM:   321 functions,  79486 lines          (mozjpeg, 376 KiB)
9Nbh3eMuVjD:  7865 functions, 977766 lines          (2.9 MiB)
ayqr5HQtlkb:  3055 functions, 655585 lines          (2.0 MiB)
rogm88TRRiw:  2157 functions, 508994 lines          (2.1 MiB)
D5pLH9sfOOl:  refused — imported memory             (VoIP/PJSIP, 9.4 MiB)
```

`COs9e0Kj0ic` compiles with rustc in about 2.5 seconds and instantiates, which
runs the module's own `__wasm_call_ctors` and every static initialiser with it.

`ayqr5HQtlkb` — 2.0 MiB of wasm, 3055 functions, 655k lines of Rust in one file
— also compiles and instantiates, in **22m49s**. That is the scaling limit, and
it is rustc's single compilation unit rather than anything in the decompiler:
the emitter takes about a second.

## Usage

```console
$ unwasm inspect module.wasm      # what the module contains
$ unwasm decompile module.wasm    # to stdout
$ unwasm decompile module.wasm -o out.rs
```

The output is a self-contained Rust module — no dependencies, runtime embedded:

```rust
mod generated;

fn main() {
    let mut instance = generated::Instance::new();
    println!("{}", instance.add(2, 3));
}
```

With a host:

```rust
struct Host;
impl generated::Imports for Host {
    fn env_add(&mut self, a: i32, b: i32) -> i32 { a + b }
}
let mut instance = generated::Instance::with_host(Host);
```

## How faithfulness is checked

`tests/common/mod.rs` is the harness. For each fixture it assembles or compiles
a module, decompiles it, builds the result with rustc, and runs both sides:

- **136 opcodes, one at a time** (`tests/opcodes.rs`), each over a spread of
  values chosen where the semantics turn — zero, the minimum, a shift count past
  the width, NaN, both zeros, the infinities.
- **C fixtures at every optimisation level** (`tests/differential.rs`), `-O0`
  through `-Oz`, since `-O0` keeps everything in the shadow stack and `-Oz`
  restructures the control flow.
- **Modules that decode but do not add up** (`tests/malformed.rs`), built byte
  by byte, to pin that each one is refused by name.

Three bugs this caught that reading would not have:

1. **A value spilled inside a block and used after it.** wasm's operand stack
   crosses a block boundary; a Rust `let` does not. Found by the first real
   module, in three functions.
2. **`data.drop` is observable.** The bytes are a `const` in the output, so
   dropping looks like a no-op — but a later `memory.init` from a dropped
   segment traps in every engine. The comment in the source said the opposite
   until the harness disagreed.
3. **A constant initialiser was read as its first operator.** `(i32.const 1)
   (i32.const 2) i32.add` came back as `1`: a plausible number, at the wrong
   address, with nothing reported.

Two more the harness caught about *itself*: f64 bit patterns lose precision
through a JSON number, and NaN payloads are not determined by the spec — so
those are compared as `nan`, not as bits.

## Building and testing

```sh
cargo test --workspace                                  # 178 tests, ~15s
cargo test --test captured -- --ignored --nocapture     # the real modules
cargo clippy --workspace --all-targets -- -D warnings
cargo llvm-cov --workspace --summary-only
```

The harness needs `node` (the reference engine), `clang` (wasm32 fixtures),
`wasm-tools` (wat assembly) and `rustc`. A missing tool **fails** the tests
rather than skipping them: a run that compared nothing must not report the same
green as a run that compared everything.

### Coverage

98.5% of lines, 96.6% of regions. `rt.rs` — the semantics — is at 100%.

The remainder is documented rather than papered over: it is the `other =>` arms
of wasmparser's `#[non_exhaustive]` enums (constructs from proposals no
toolchain we target emits), and two checks that the decoder already guarantees.
Both are kept. Deleting a defence to raise a percentage is how a decoder change
becomes a panic two years later.

## Known limits

- **One file.** 655k lines in a single compilation unit is 23 minutes of rustc
  (against ~1 second to emit them). Splitting the output into several `mod`s so
  the compiler can parallelise is the next practical piece of work, and it is
  purely mechanical.
- **Imported memory is refused**, which is what the 9.4 MiB VoIP module needs —
  it expects a shared memory from its host. Threads and atomics with it.
- **No SIMD, no reference types, no exceptions, no multi-value.** Each is
  refused by name.
- **Level 0 only.** Linear memory is bytes; a C local that lived in the shadow
  stack is still an address, and a struct is still an offset.

## Where it goes

- **Level 1 — structured.** Recover the shadow stack (the `__stack_pointer`
  global is right there), so frame slots become named locals. Recover parameter
  roles from how the code uses them, in the manner of `wa-wasm-oracle`'s
  `abi.rs`: dereferenced means pointer, and the access width says what it points
  at.
- **Level 2 — idiomatic, and always speculative.** Structs from access
  patterns, vtables from `call_indirect` plus the element segments, class names
  from the C++ RTTI in the data segments, and embind's `_embind_register_*`
  calls — which are high-level types the binary declares about itself.
- **Library identification.** A FLIRT-like signature over normalised opcode
  sequences, so libc, libc++, libjpeg and PJSIP are recognised and *not*
  decompiled. On a 9 MiB module that is most of the output.

Every one of those is a guess about intent. They are only worth attempting on
top of something already known to run — which is what level 0 is for, and why
it came first.
