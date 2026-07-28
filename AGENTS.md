# unwasm

A WebAssembly decompiler that emits Rust. Read `README.md` first — it holds the
level model, what the real modules do, and the known limits.

## Build & verify

```sh
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo llvm-cov --workspace --summary-only
cargo test --test captured -- --ignored --nocapture   # the real modules
cargo test --test emscripten -- --ignored --nocapture # the real toolchain
```

The harness shells out to `node`, `clang`, `wasm-tools` and `rustc`. All four
are required; a missing one fails the tests rather than skipping them.

`emcc` is needed only by `tests/emscripten.rs`. It is found on `PATH` or at
`/usr/lib/emscripten/emcc`, because the Arch package's `profile.d` entry only
reaches shells started after the install — a `cargo test` in an older shell
would otherwise miss it.

## Ground rules

- **The output always compiles.** This is not a quality goal, it is the property
  the whole project rests on: code that does not compile cannot be run, and
  code that cannot be run cannot be compared against the module it came from.
  A change that makes some module emit Rust that rustc rejects is a bug of the
  same severity as emitting the wrong arithmetic.

- **Unsupported is an error, never a guess, never a skip.** There is no
  `_ => {}` over opcodes in this codebase. A construct with no faithful Rust
  form comes back as `Error::Unsupported` naming it and where it was found. A
  decompiler that drops an instruction emits code that runs and is wrong, and
  the reader has no way to tell.

- **An annotation carries its evidence.** `analysis.rs` may name the stack
  pointer or quote a string, and every such claim reaches the output with what
  it rests on — an exported name, or 214 prologues. A decompiler that says
  "this is the stack pointer" and stops has asked to be trusted, and the reader
  has no way to check. Where there is no evidence the answer is `None`, not the
  most likely index.

- **An annotation must not change behaviour.** They are comments and names. The
  differential tests cover the annotated output for exactly this reason: if
  naming a global ever changed a result, that is what would catch it. The one
  way a comment *can* break the output is by containing `*/`, so quoted text is
  escaped — and that has a test.

- **A stub that returns zero is a hypothesis.** `NoImports` traps on every
  import for exactly this reason: "no host was supplied" must stay
  distinguishable from "the host answered 0". This is the one lesson from
  `wa-wasm-oracle` that transferred unchanged.

- **Agreement with yourself proves nothing.** The reference side of every
  differential test is V8, through node. Do not replace it with an interpreter
  of our own — it would share our misreadings. The comparison covers the
  returned value, the trap, *and* the whole of linear memory; dropping the
  memory comparison would hide every store that went to the wrong address.

- **Compare NaN as `nan`, not as bits.** wasm leaves the payload and sign of a
  propagated NaN unspecified. Comparing them compares something the spec does
  not decide, and the two sides will differ for reasons that are nobody's bug.

- **64-bit values do not travel through JSON numbers.** i64 values and f64 bit
  patterns go across as strings. `f64::MAX`'s bits parse back as the bits of
  infinity otherwise, and the two sides then agree about a value neither of
  them computed. This has already happened once.

- **Semantics live in `rt.rs`, not in the emitter.** Anything wasm defines and
  Rust spells differently — trapping division, NaN-propagating `min`, truncation
  that traps rather than saturating — is a function there with a test beside it.
  `rt.rs` is compiled twice: as part of this crate, where its tests run, and as
  text embedded in every generated module.

- **One line per opcode.** `ops.rs` declares the wasmparser operator, the stack
  signature and the Rust template together. Saying them in three places is how a
  decompiler lowers `i32.shr_u` into an arithmetic shift.

- **Keep the dependency at one.** wasmparser, and nothing else. The CLI parses
  its own arguments and the errors are hand-written. A decompiler that takes ten
  seconds to rebuild is one nobody iterates on.

## Where things live

- `module.rs` — decoding, and the narrowing of wasm to what the backend models
- `ops.rs` — the opcode table: lowering, signature and Rust template in one line
- `analysis.rs` — what the module says about itself, with the evidence attached
- `codegen.rs` — the emitter, the layout, and the value-stack rules below
- `rt.rs` — the semantics, embedded in every output
- `error.rs` — the three ways this can fail, all of them named
- `tests/common/mod.rs` — the differential harness
- `tests/emscripten.rs` — the toolchain the captured modules were built with:
  its libc, its libm, its C++ vtables

## The frame walk gives up early, on purpose

`read_frame` tracks which abstract stack entries are the frame address. Anything
it cannot model exactly — an unknown arity, a frame address live across control
flow, arithmetic that is not a constant offset — sets `escapes` and stops. That
direction is the safe one: a frame wrongly called escaping costs an annotation,
while one wrongly called contained is a false statement about the code, and the
next level would build variable promotion on top of it.

The three prologue spellings are in `read_prologue`, and the third one — a leaf
function that never writes the stack pointer back — is not in any reference. It
was found by looking at what clang actually emitted at `-O0`, after the first
version silently found no frames at all.

## The two rules the value stack lives by

Both were learned from a real module rather than from reasoning:

1. **Spill the stack before opening a block.** wasm's operand stack survives a
   block boundary; a Rust `let` inside the block does not. A value spilled
   inside and consumed after would name a binding that has gone out of scope.
   This broke three functions in the first 236 KiB module tried.

2. **Spill before anything that can change what a folded value reads.** Only
   constants, locals and globals are folded into their consumer. Before a
   `local.set`, a store, or a call, everything still on the stack is named — or
   a folded `local.get 0` pushed before `local.set 0` reads the new value, and
   the output is wrong only on some inputs.

Operands of anything whose receiver is `&mut self` — calls, stores, `memory.*`
— are named too. `self.f1(self.f2())` does not borrow-check, and relying on
two-phase borrows to cover the cases where it does is a coin flip per call site.

## Unreachable code is not code

After `br`, `return`, `unreachable` or `br_table`, wasm's stack is polymorphic
and Rust's is not: the instructions that follow would not type-check if they
were emitted, so they are skipped until the frame closes. The skip tracks
nesting, because a `block` inside dead code has an `end` that must not be
mistaken for the enclosing frame's.

## Coverage is a floor, not a target

98.5% of lines. The rest is `other =>` arms over wasmparser's
`#[non_exhaustive]` enums and two checks the decoder already makes. Keep them.
Deleting a defence to raise a percentage is how a decoder change becomes a panic
two years later — and the tests in `malformed.rs` record *which* layer currently
rejects each case, so a change in that answer is visible.

## Layout is a compile-time decision, and it was measured

rustc partitions codegen units along module boundaries. The 2.0 MiB capture in
one file took **22m49s**; split at 16 functions per file — 192 files — **25.7s**.
Same binary, same behaviour, 53×.

The default was chosen from the whole curve (1 / 13 / 49 / 192 / 764 files:
22m49s / 7m36s / 2m34s / 25.7s / 10.4s), not from the first improvement that
looked good. The table is in `Layout::FUNCTIONS_PER_FILE`. Any change to it
should be measured the same way rather than argued about — and the system time
is the number to watch, since it was 1102s for one file and 12.6s for 192.

## Open work

1. **The VoIP module needs more than imported memory.** It imports
   `(memory 160 32768 shared)` and its `target_features` lists `atomics`,
   `reference-types` and `multivalue`. That is the whole threading model, and
   "faithful" for atomics in a single-threaded target is a design question, not
   an implementation one — `wa-wasm-oracle` spent a day on exactly that
   (`can_block`).
2. **Level 1 proper**: turn shadow-stack slots into named locals, now that the
   stack pointer is identified. The frame size is in the prologue; what is
   missing is tracking which offsets from it are distinct variables.
3. **Library identification.** A FLIRT-like signature over normalised opcode
   sequences would let libc, libc++ and PJSIP be recognised and left
   undecompiled. On a 9 MiB module that is most of the output.
