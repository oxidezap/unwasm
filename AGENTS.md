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

Which means **the frame tests are a measurement of a particular clang**. They
were written against clang 22 and CI pins it. Under Ubuntu 24.04's clang 18 all
four fail, finding no frame at all — a fourth spelling `read_prologue` does not
know. Before concluding that a module has no frames, check what built it.

## Folding is only for expressions that cannot trap

`push_pure` folds an expression into whatever consumes it; `push_temp` gives it
a name. The line between them is not readability, it is that **a folded value
that is dropped is never emitted at all**. Fold a division and a dropped one
stops trapping on a zero divisor; fold an atomic read-modify-write and a dropped
one stops writing. Both happened in the first version and the differential tests
caught them within a minute.

So: anything that can trap or touch state gets a name. Everything that cannot —
wrapping arithmetic, comparisons, bitwise ops, casts — folds. That is about a
third of the emitted lines.

Folding also means nesting, and Rust's precedence is not wasm's: `a + b` folded
into `{0} as u32` gives `a + b as u32`, which is a different number.
`Value::as_operand` brackets anything with a top-level operator, and `is_atomic`
decides — conservatively, since a redundant bracket costs noise and a missing
one costs an answer.

## A spill is for what the next operation can change

Only pure expressions are folded, and a pure expression reads constants, locals
and globals — never memory, since a load is impure and gets a name. So:

- a **store**, a `memory.fill`, a `memory.grow` invalidate nothing on the stack;
- a **call** can set a global but cannot touch this function's Rust locals;
- a **`local.set N`** invalidates only the values that read local *N*.

`Reads` carries that per value: a 64-bit set of local indices, a flag for the
locals past it, and whether it reads a global. Getting this wrong is a wrong
answer on some inputs and not others, which is exactly what the differential
tests exist for — they were the check on this change.

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

**99.20% of lines — 72 of 8946**, on a checkout with the `#[ignore]`d tiers not
running: `module.rs` 22, `codegen.rs` 18, `analysis.rs` 12, `main.rs` 11,
`rt.rs` 6, `ops.rs` 3. `hostlib.rs` and `error.rs` are complete.

The number was **99.5%** here and **99.6%** in the README, and neither was
re-measured as the code grew. Re-measure before quoting it; a coverage figure
that is only ever copied forward is the kind of claim this project exists to
not make.

The part of `module.rs` that is unreachable rather than untested:

- the `other =>` arms over wasmparser's `#[non_exhaustive]` `TypeRef` and
  `ExternalKind`. The compiler requires the arm; the only value that reaches it
  is `FuncExact`, from the custom-descriptors proposal, which no toolchain we
  target emits and `wasm-tools` will not assemble.
- two checks that the function and code sections agree. The decoder guarantees
  it by *three* separate routes — inconsistent lengths, a missing function
  section, and a body count mismatch — and `malformed.rs` has a test for each,
  asserting the decoder's message rather than ours.

Three of `codegen.rs`'s are in the table-type map: a table entry whose function
index has no type, a type index past `u16::MAX`, and a signature missing from
the type section. All three are malformed-module defences that `wasm-tools` will
not assemble, and the third is unreachable by construction. The other fifteen
there, and the gaps in `analysis.rs`, `main.rs`, `rt.rs` and `ops.rs`, are
ordinary uncovered code — they are not defended, they are just untested.

Keep the defences. Deleting one to raise a percentage is how a decoder change
becomes a panic two years later, and the tests record which layer rejects each
case today, so a change in that answer shows up as a failing test rather than
as a crash.

Measure with LCOV, not the text report: `cargo llvm-cov --workspace --lcov` and
count `DA:` records with a zero. The text report's per-file view misses lines
and reads as better than it is — it said `codegen.rs` was complete while the
summary counted sixteen.

## Compile time is the bottleneck, and it is mostly the front end

Measured on a 2.9 MiB capture with the rustc cache off (`RUSTC_WRAPPER=`, which
matters: this machine has `kache` installed and it turns a rebuild into five
seconds of nothing):

- `cargo check` is **18.5s** of a **22.4s** `cargo build`. Codegen is not the
  cost; parsing, name resolution and type checking are. So the lever is fewer
  bytes and fewer statements, not simpler ones.
- Interleave the configurations when timing. A first run is cold and reads 30%
  slow; measuring A then B once gave a 30% "improvement" that was the page
  cache warming up. Repeat and alternate.

Two changes came out of that, both measured in *volume* rather than in a time
that is within noise: data segments as byte-string literals (`mod.rs` for the
VoIP module 8.9 MB → 3.3 MB), and spilling only what an operation can actually
change (508556 → 416101 temporaries).

The one that actually moves the wall clock is not a codegen tweak: it is
`--reachable-from … --direct-only`, which turns 21 minutes into 11 seconds by
not compiling the 82% of the module the path never touches.

## Layout is a compile-time decision, and it was measured

rustc partitions codegen units along module boundaries. The 2.0 MiB capture in
one file took **22m49s**; split at 16 functions per file — 192 files — **25.7s**.
Same binary, same behaviour, 53×.

The default was chosen from the whole curve (1 / 13 / 49 / 192 / 764 files:
22m49s / 7m36s / 2m34s / 25.7s / 10.4s), not from the first improvement that
looked good. The table is in `Layout::FUNCTIONS_PER_FILE`. Any change to it
should be measured the same way rather than argued about — and the system time
is the number to watch, since it was 1102s for one file and 12.6s for 192.

## What a single thread can and cannot say about atomics

Under one thread an atomic load, store or read-modify-write is its plain
counterpart: there is nobody to interleave with. Three things are not:
alignment (an atomic traps where a plain access does not), `notify` (wakes
zero, correctly), and `wait` — right when the value already changed or the
timeout is zero, and a trap otherwise, because the alternative is inventing a
notification that never came. Do not "fix" that trap by returning 2.

Measure before believing a feature list: the VoIP module's `target_features`
asks for `reference-types` and `multivalue`, and the module uses neither — one
`funcref` declaration and zero multi-result blocks across the 13347 functions
of `D5pLH9sfOOl`.

## A trampoline must not catch a trap

The generated `invoke_*` trampolines reproduce Emscripten's glue, including the
part that is easy to drop: `if (e !== e+0) throw e`. A host throws a C++
exception with `rt::throw`, which panics with a `GuestException`; the trampoline
catches that type and nothing else, and re-raises everything else with
`resume_unwind`. Catching all panics would turn every trap inside a `try` into a
silently handled error — the exact failure this project is built to prevent.

A trampoline is generated only when the module has all three parts: a stack
pointer, an exported `setThrew`, and a declared type for what the table holds.
Missing any of them, the import stays the host's — `analysis.rs` decides, and
the tests cover each refusal.

## Fixtures are named by their content

`assemble` and `compile_c` name their files after a digest of the source and
write through a temporary, because cargo runs test binaries in parallel *and*
tests within one binary as threads. Naming a fixture after the test lets one
test read a file another is still writing, and the failure that produces —
"unexpected end-of-file" on a module that is fine — reads as a decoder bug. It
happened twice before this was fixed properly.

## An indirect call is not a direct call

`CallGraph` keeps them apart: `calls` holds what a function calls by index, and
`calls_indirectly` holds the *signatures* it calls through the table. Merging
them would turn "could reach anything with this shape" into "calls this", which
is a much stronger claim than the module makes. What a signature could reach is
the table's business, and `unwasm table --type` is where to ask.

## A fingerprint is precise and has poor recall

`analysis::fingerprint` is the opcode sequence with everything a rebuild
renumbers left out. Across builds of the same toolchain it matches ~91%; across
emscripten versions, 2 of 33. Do not try to close that gap by coarsening it —
that was tried, and against a module sharing no code at all it matched seven of
ten. Precision is the whole value: a wrong name is worse than `f8421`, which is
also why a fingerprint two names share is dropped rather than resolved, and why
what the module says about itself is never overridden.

## An import is given the memory, not just its arguments

Every `Imports` method takes `caller: &mut rt::Caller<'_>` first. wasm hands an
import numbers, and almost all of them are numbers into linear memory; a host
without it cannot answer `fd_write` at all. The thunk builds the `Caller` from
`&mut self.memory` while `&mut self.host` is borrowed — disjoint fields, so it
type-checks without any interior mutability.

`Caller` is a struct, not a `&mut Memory` alias, because the next capability a
host needs (the table, the globals, a way back into an export) goes in it as a
field. Adding a field costs nothing; adding a parameter changes every host that
has ever been written against the trait.

## Reading affordances answer a question that was costing hours

Four of them, and all four came from someone actually reading a 9 MiB module:

- `--instrument-stores` + `Memory::watch` — who wrote this address. Every
  write, `fill` and `copy` included, since a memset is the usual answer.
- `frames --outside` — the static half: stores past the frame, and stores
  through an address computed *from* the frame, which is the indexed-array
  write that actually overruns.
- `--offsets` + `unwasm bytes` — which bytes made this line, and is that byte
  sequence unique. Hand-computed LEB arithmetic was the error source.
- call *sites*, not just callers, in the doc comment: instrumenting a body
  shared by 58 sites measures whichever one ran.

They share a shape worth keeping: each replaces a measurement someone was
making by hand with one the machine already has the numbers for.

## What a threaded run gets right, and the one thing it does not

A `notify` wakes exactly the count it was given, in parking order; a finite
wait uses an absolute deadline, because every notify wakes every waiter and a
timeout re-armed on each wake-up never expires; `grow` is a compare-exchange
loop, because load-then-store lets two threads report the same old size and
publish one page; and `atomic.fence` lowers to a real fence, which under one
thread was correctly nothing and under several is the instruction's whole
point. Each of those was wrong first and has a test that fails without it.

The divergence that remains, stated as one: **an unsatisfiable wait traps.**
The spec says block forever. Trapping when no other instance holds the memory
is a deliberate choice — a hung test is unkillable and a hung decompilation
tells the reader nothing — and it is checkable rather than assumed, but it is
not what an engine does.

## A thread is another instance over the same memory

`SharedMemory` is `Arc<[AtomicU8]>`; `Instance::spawn` gives a new instance the
same memory and its own globals. The three things that make it work and are
easy to get wrong:

- **The data segments are not placed again.** An engine does not re-run them
  for a new thread, and doing it would overwrite whatever the program has
  already written.
- **The wait trap is now a fact, not an assumption.** `strong_count == 1` means
  nobody can ever notify, so the wait is unsatisfiable rather than slow. That is
  what lets a threaded module run on one thread without hanging.
- **A catch clause is not mechanical.** `__cxa_find_matching_catch_*` and
  `__cxa_get_exception_ptr` are left to the reader on purpose: matching a throw
  against a clause means comparing types through the module's own
  `__cxa_can_catch` and adjusting the pointer for a base class. Answering with
  the exception in flight picks the wrong handler for `throw Derived; catch
  (Base&)`, which is worse than saying it is not written.
- **The table is copied, and it does not matter.** There is no `table.set`,
  `table.grow`, `table.fill` or `table.copy` in this decompiler, and an
  unmodelled opcode is refused rather than dropped — so a table cannot change
  after instantiation, and a copy of something immutable is the original. This
  was worth measuring rather than assuming: all six captured modules have zero
  table-mutation opcodes, and `D5pLH9sfOOl`'s table is declared `9291 9291`.

## The corpus rolls, and a measurement is dated by the build it was taken on

WhatsApp reissues these payloads under new ids and stops serving the old ones.
`scripts/fetch-captures.sh` pulls what is still published from the public
`oxidezap/whatspec` archive and checks each against a pinned sha256; the two
captures more than one test names are constants in `tests/common/mod.rs`.

When an id ages out, the corpus moves to the build that succeeded it and every
number a test pins is **re-read against the new module**, not carried over. The
VoIP module went `D5pLH9sfOOl` (9.4 MiB, 13347 functions, 227 imports, 125
trampolines) → `JgwtTQVeWPm` (10.2 MiB, 14733 functions, 242 imports, 134
trampolines); the 2.9 MiB one went `9Nbh3eMuVjD` → `a19OxQ3jkd2`.

Figures quoted in `README.md` were measured on the build named beside them, and
most of them name `D5pLH9sfOOl`. They are records of a run, not claims about
whatever the corpus holds today — so re-measure before reusing one, and say
which build you measured.

## Open work

1. **Drive the VoIP module.** `unwasm host` implements about half of it; the
   rest are WhatsApp's own callbacks, `stat` (a struct layout nobody should
   guess at), `asm_const` (JavaScript the module carries) and the pthread glue,
   which needs a way back into the instance from an import. The exact split was
   51 of 102 on `D5pLH9sfOOl`; on `JgwtTQVeWPm` it is 108 methods and has not
   been re-counted.
2. **Level 1 proper**: turn shadow-stack slots into named locals, now that the
   stack pointer is identified. The frame size is in the prologue; what is
   missing is tracking which offsets from it are distinct variables — with the
   caveat recorded above about byte-exactness.
3. **Leave recognised library code out.** `unwasm signatures` names it, but it
   is still decompiled in full. Stubbing it would cut most of a 9 MiB module —
   for a run being read, not run, and only as far as the catalogue's recall
   goes.
