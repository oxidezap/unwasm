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

- **Expressions rather than one binding per instruction.** Pure arithmetic
  folds into whatever consumes it, which is about a third of the emitted lines —
  and, on the module that needed it most, 1.2 million of them.
- **The whole MVP instruction set**, plus sign extension and the saturating
  truncations: 136 numeric opcodes, all of memory, `call_indirect`, `br_table`,
  passive segments.
- **Threads, as far as one thread can go**: shared and imported memories, and
  all 67 atomic instructions. See below for where that stops being faithful and
  what happens there.
- **Structure is taken, not recovered.** wasm's `block`/`loop`/`if` become
  Rust's labelled blocks and loops; `br` becomes `break 'bN` or `continue 'bN`.
  There is no CFG reconstruction in this codebase, because there is no CFG in
  the input.
- **Imports become a trait.** A host implements `Imports`; the default
  `NoImports` traps on every call, so "nobody supplied a host" never looks like
  "the host returned 0".
- **Emscripten's `invoke_*` trampolines are generated**, not asked for. On the
  VoIP module that is 125 of 228 imports; see below.
- **Nothing is skipped.** An opcode with no faithful Rust form is an error
  naming the construct — never a comment in the output, never a stub.
- **No `unsafe`.** `unsafe_code = "forbid"` for this crate and for what it
  emits. A decompiler that needs raw pointers to model a sandbox has lost the
  property that made the sandbox worth reading.

### On the real modules

Every WhatsApp Web capture decompiles:

```
COs9e0Kj0ic:   478 functions,  73703 lines of Rust  (VOPRF/crypto, 236 KiB)
php8T1oSIZM:   321 functions,  79486 lines          (mozjpeg, 376 KiB)
9Nbh3eMuVjD:  7865 functions, 977766 lines          (2.9 MiB)
ayqr5HQtlkb:  3055 functions, 660186 lines          (2.0 MiB)
rogm88TRRiw:  2157 functions, 508994 lines          (2.1 MiB)
D5pLH9sfOOl: 13347 functions, 3611088 lines         (VoIP/PJSIP, 9.4 MiB)
```

All six decompile. The last one took a shared imported memory and 1070 atomic
instructions; see below.

`COs9e0Kj0ic` compiles with rustc in about 2.5 seconds and instantiates, which
runs the module's own `__wasm_call_ctors` and every static initialiser with it.

`ayqr5HQtlkb` — 2.0 MiB of wasm, 3055 functions, 660k lines of Rust — compiles
and instantiates in **23 seconds**, split across 192 module files. In a single
file the same module took **22m49s**.

### The split, and why it is not cosmetic

rustc partitions codegen units along module boundaries, so one enormous module
becomes one enormous unit. Measured on that capture, same binary each time:

| functions/file | files | rustc  | vs. one file |
|----------------|-------|--------|--------------|
| all            | 1     | 22m49s | 1×           |
| 256            | 13    | 7m36s  | 3×           |
| 64             | 49    | 2m34s  | 8.9×         |
| **16**         | 192   | 25.7s  | **53×**      |
| 4              | 764   | 10.4s  | 132×         |

The system time is the tell: 1102s for one file against 12.6s for 192. That is
not work, it is a single codegen unit thrashing. Sixteen is the default —
53× faster, and still a number of files a person can navigate; going four times
finer buys another 2.5× for four times the files. `--split <n>` overrides it,
and a module of 512 functions or fewer stays in one file, where it compiles in
seconds anyway.

### What the module says about itself

Level 0 translates. Alongside it, a small analysis pass *reads*, and annotates
the output with what the module states about itself — never with a guess:

```rust
/// Global #0 (mutable).
///
/// The C stack pointer, by its use: 214 functions open by subtracting a
/// frame from it and storing it back. The module keeps no names, so this is
/// evidence rather than a declaration.
pub g0_stack_pointer: i32,
```

```rust
let t0: i32 = 211967i32 /* "called `Option::unwrap()` on a `None` value" */;
```

Both claims carry their evidence. The stack pointer is found by an exported
name where one survives, and otherwise by counting prologues — one match is a
coincidence, two hundred is a calling convention. Static strings are quoted from
the data segments, and where the code passes a pointer with a length beside it
(how Rust passes `&str`) the length is used, so an unterminated string stops
where it actually stops instead of running into the next one.

On the mozjpeg capture that recovers 247 strings, including the Rust panic
messages that name the failing function — which is how `wa-wasm-oracle` worked
out that module's calling convention in the first place.

### Names, where the module never wrote any

The VoIP module is stripped: no name section, no mangled symbols, nothing. What
it does have is its own log messages, and a function that logs
`fill_relay_info Failed to copy uuid` is very probably `fill_relay_info`:

```rust
/// Function #458. The module names nothing; this one references
/// "fill_user_info_from_participant Missing participant jid…", a message no
/// other function references — so it is probably `fill_user_info_from_participant`.
pub(crate) fn f458_fill_user_info_from_participant(&mut self, ..)
```

**2654 of its 13347 functions get a name this way.** Two rules keep it from
inventing them:

- **The message must belong to one function.** A message in fifty functions
  distinguishes none of them — `index out of bounds` is in most of a Rust
  module. Uniqueness is what makes the guess worth making.
- **The identifier must look like one.** A lowercase token with an underscore,
  at the start of the message. Anything looser turns `error while parsing` into
  a function called `error`.

The index stays in the name, which is what makes the guess safe: a reader
tracing `call 4213` finds `f4213_parse_xmpp_offer` either way, and a wrong guess
costs a misleading suffix rather than a lost thread.

It is not a general technique, and the numbers say so plainly: 2654 names in the
VoIP module, 25 in the next one, and 0 in two others. It works where code logs
with a function prefix — which the WhatsApp code does and mozjpeg does not.

#### Reaching the strings at all

None of that was possible until the segments could be read. A module built for
threads places its data with `memory.init` rather than at static offsets: **all
125 of the VoIP module's segments are passive**, carrying no address, so every
string in it was unreachable. The addresses are in the code, as three constants
before the `memory.init`, and resolving them turned 0 quoted strings into
105222.

Placements are recorded only in that plainest form. Per-thread storage is placed
at a computed `base + offset`, and a segment placed at two different constant
addresses is dropped rather than picked between — both are true, so neither can
resolve an address back to text.

### The shadow stack

A C function's locals do not all fit in wasm locals: anything whose address is
taken, anything larger than a scalar, anything spilled, lives in a frame carved
out of linear memory. The analysis reads that frame back:

```rust
/// Stack frame: 32 bytes, based at `frame`, not published (nothing is
/// called while it is live).
///
/// | offset | width | reads | writes |
/// |--------|-------|-------|--------|
/// | +16    | 4B    | 1     | 1      |
/// | +20    | 2B    | 0     | 1      |
/// | +22    | 1B    | 0     | 1      |
```

That is `struct Point { int x; short y; char tag; }`, and its layout is legible
without a single name having survived. The base local is renamed `frame`; the
others keep their indices, because the index is what a `local.get 3` in the
bytes refers to.

Two things it reports honestly rather than glossing over:

- **When the address escapes.** If the frame is passed to a call, stored into
  memory, or copied to another local, the summary says so — the slots listed
  are then only the accesses that could be followed, not the whole frame.
- **When a frame is never published.** A leaf function at `-O0` computes
  `sp - 32`, uses it, and never writes it back, because nothing else will
  allocate while it runs. Requiring the write — which this did at first — misses
  every leaf function in a module.

What it does *not* do is turn slots into Rust variables. It could, for the
frames that do not escape; it would also stop the decompilation being
byte-exact, because the bytes a promoted slot used to leave in linear memory
would no longer be there. That is a real property to give up, and not one to
give up quietly, so it waits for a level that says it is doing so. Across the
captures, the frames that stay put are 0.7% to 28% of the total — so the prize
is smaller than it sounds, and would need a real dataflow analysis rather than
this linear walk to grow.

## Usage

```console
$ unwasm inspect module.wasm      # what the module contains
$ unwasm decompile module.wasm    # to stdout
$ unwasm decompile module.wasm -o out.rs      # one file
$ unwasm decompile module.wasm -o out/        # mod.rs plus parts
$ unwasm decompile module.wasm -o out/ --split 64
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
- **Emscripten modules** (`tests/emscripten.rs`), built by the toolchain the
  captured modules were built with. These run its real libc — `malloc`, `free`,
  `memcpy`, `strlen` — its libm, and C++ virtual dispatch, where the vtable
  lands in an element segment and the call goes through `call_indirect`. Tens of
  thousands of instructions the fixtures never reach, and they agreed on the
  first run.
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
cargo test --test emscripten -- --ignored --nocapture   # the real toolchain
cargo clippy --workspace --all-targets -- -D warnings
cargo llvm-cov --workspace --summary-only
```

The harness needs `node` (the reference engine), `clang` (wasm32 fixtures),
`wasm-tools` (wat assembly) and `rustc`. A missing tool **fails** the tests
rather than skipping them: a run that compared nothing must not report the same
green as a run that compared everything.

`emcc` is needed only by the `#[ignore]`d Emscripten tests. On Arch the
`emscripten` package provides it — note that it replaces `binaryen`, which it
also provides, and puts `emcc` in `/usr/lib/emscripten`; the harness looks
there as well as on `PATH`, since the packaged `profile.d` entry only reaches
shells started afterwards.

### Coverage

98.5% of lines, 96.6% of regions. `rt.rs` — the semantics — is at 100%.

The remainder is documented rather than papered over: it is the `other =>` arms
of wasmparser's `#[non_exhaustive]` enums (constructs from proposals no
toolchain we target emits), and two checks that the decoder already guarantees.
Both are kept. Deleting a defence to raise a percentage is how a decoder change
becomes a panic two years later.

### Atomics on one thread

A decompilation runs one thread, and under one thread almost every atomic does
exactly what its plain counterpart does — there is nobody to interleave with.
Three things are not "almost":

- **Alignment.** An atomic access must be naturally aligned or it traps, where a
  plain one is happy anywhere. That is a real behavioural difference and it is
  in the runtime, with tests on both sides of it.
- **`memory.atomic.notify`** wakes zero threads. Not a stub returning zero — the
  correct answer when nobody else is running.
- **`memory.atomic.wait32`/`wait64`** is exactly right in two of its three
  outcomes: the value already changed (`1`, and no waiting was needed), or the
  timeout was zero (`2`, which expires immediately whoever is running). The
  third would end only when another thread notifies, and there is no other
  thread — so it traps, saying that. Returning "timed out" there would be
  inventing an event that did not happen.

All 67 are compared instruction by instruction against V8 running a real shared
memory, so the two sides differ only where the thread count makes them.

### The `invoke_*` trampolines

Emscripten routes any call that might throw through an `invoke_*` import: the
first argument is a table index, the rest are the callee's own. The JavaScript
glue implements it as

```js
function invoke_vii(index, a, b) {
  var sp = stackSave();
  try { getWasmTableEntry(index)(a, b); }
  catch (e) { stackRestore(sp); if (e !== e+0) throw e; _setThrew(1, 0); }
}
```

Every part of that is already in the module — the table, the stack pointer, and
its own exported `setThrew` — so it is generated rather than left as one more
thing for a host to write. **The VoIP module's 228 imports become 102.**

```rust
/// `env::invoke_vii` — generated, not delegated.
fn f96(&mut self, p0: i32, p1: i32, p2: i32) {
    let saved = self.g0_stack_pointer;
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        self.call_indirect_6(p0, p1, p2)
    }));
    match outcome {
        Ok(value) => value,
        Err(payload) => {
            if payload.is::<rt::GuestException>() {
                self.g0_stack_pointer = saved;
                self.f596(1, 0);
            } else {
                std::panic::resume_unwind(payload)
            }
        }
    }
}
```

The clause worth copying carefully is `if (e !== e+0) throw e`: the glue
re-throws anything that is not one of its own exceptions. A host throws by
calling `rt::throw`, which the trampoline catches; **a trap is not an exception
and is re-raised**. A trampoline that caught everything would turn a crash into
a quietly handled error, which is the failure mode this project exists to avoid,
and it has a test of its own.

A trampoline is only generated when the module supplies all three parts. No
`setThrew`, no stack pointer, or no type for what the table holds, and the
import stays the host's to implement.

## Known limits

- **Compile time still scales with the module**, just not catastrophically:
  23 seconds for 2.0 MiB of wasm. The 9.4 MiB VoIP module would be several
  minutes, if the rest of it were supported.
- **The VoIP module decompiles but cannot yet run**: 102 host imports remain,
  and they are the ones only a host can answer — 7 WASI, 20 Emscripten runtime,
  20 C++ runtime and syscalls, 16 embind/emval, and 40 callbacks belonging to
  WhatsApp itself.
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
