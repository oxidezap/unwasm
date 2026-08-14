# unwasm

A WebAssembly decompiler that emits Rust. Read `README.md` first — it holds the
level model, what the real modules do, and the known limits.

## This workspace holds two projects, and they are deliberately not one

`crates/oracle-core` and `crates/oracle-cli` are the other half: they run the
original `.wasm` under wasmtime instead of decompiling it. **`AGENTS-oracle.md`
is their guide and it is not optional reading before touching them** — it
carries the host-environment rules, the capture-lock rules, and a table of
hypotheses already ruled out by measurement.

Three things that catch people out when working across both:

- **The duplication between the two host environments is the design.** See the
  long note in `Cargo.toml`. If you are about to remove it, read RFC-0005 of
  `wa-codegen-research` first.
- **`oracle-core` may depend on `unwasm-core`; never the reverse.** The
  decompiler's one dependency is a property its output relies on. Today the
  dependency is used in exactly one place, `carry.rs`.
- **Lints differ per crate in exactly one respect.** The decompiler keeps
  `unsafe_code = "forbid"`; the oracle sets `deny`, because reading wasmtime's
  shared memory needs `unsafe` and each site carries a SAFETY note. Everything
  else, `missing_docs` included, is the workspace's and is enforced on both
  halves — the 154 undocumented public items the oracle arrived with are paid
  off, so `-D warnings` is clean across the workspace and stays that way.

Running `cargo test --workspace` runs both, which takes about four minutes
because the oracle brings up a PJSIP worker pool. `cargo test -p unwasm-core`
is still the fast loop.

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

It follows a copy, though. A frame address put into another local is tracked
rather than given up on, and forgotten at a control-flow boundary — which is
where it stops being followable and becomes an escape. Without that, clang 18's
epilogue (the size in one local, the restored pointer in another) makes every
frame in a module look like it escapes.

The four prologue spellings are in `match_prologue`, and none of the last two is
in any reference. The third — a leaf function that never writes the stack
pointer back — was found by looking at what clang actually emitted at `-O0`,
after the first version silently found no frames at all. The fourth is clang
18's, where every intermediate value goes through its own local, so the
subtraction reads two locals rather than the operand stack; before it was known,
all four frame tests found no frame under that compiler and the gap was recorded
here as a fact about the compiler.

Which means **the frame tests are still a measurement of a particular clang**.
They pass under 18 and 22, and CI pins 22. A fifth compiler may well have a
fifth spelling: before concluding that a module has no frames, check what built
it — `unwasm frames` on a module whose functions all have prologues and no
frames is the symptom.

The counting evidence asks for more than the walk does. A prologue that writes
the reservation back is enough on its own; a leaf's is the same shape as
arithmetic on a global, so it counts only when the function goes on to address
memory through the reserved address. And the name section is consulted before
either: a linker writes `__stack_pointer` there whether or not it exports the
global, and that is the module saying which one it is.

## Level 1 is opt-in because it gives something up, and it says which

`--level 1` turns a frame slot into a Rust binding. `analysis::promotable_slots`
decides, and what it asks for is not a heuristic: **every** access to the frame
has to be one it can see and place. The address never escapes and was never
copied into a local that could reach a slot; no store goes through an address
computed at run time; the function has no `memory.fill`/`copy`/`init`, whose
destination is a value rather than an offset; the slot's accesses all agreed on
one width and one type; and the slot lies inside the frame and overlaps no
other.

The emitter's rule has to match the analysis's exactly, or a slot would be a
binding at one access and memory at another — which is worse than not promoting
it. So `Value::frame_base` is set only by `local.get $base`, carried through a
spill (a name for the frame is still the frame), and never by arithmetic; and
`copied` refuses the whole frame when the address reaches another local that
could still address a slot. `frame + size` copied in the epilogue does not
count, since a memarg offset is never negative and that address cannot name
anything inside the frame — without that exception, level 1 would refuse every
function an unoptimised clang built.

A binding is filled from the frame on entry, because those bytes belong to
whatever ran before and a slot read before it is written has to see them. It is
never written back. **That is the trade, and it is the whole of it**: the
answers still match the engine, the memory no longer does. `assert_agrees_at_
level_1` compares the answers and reports the memory rather than asserting it.

Three things follow that a reader should know:

- **A narrow slot is kept zero-extended**, exactly as `load8_u` returns it, so a
  store masks and a signed load sign-extends. Either one wrong is a wrong
  answer, and the differential test on a `short`/`signed char` struct is what
  says which.
- **A trap moves.** The bindings are filled at entry, so a frame that would trap
  on its first access traps at the top instead. The values are the same; the
  point at which a run stops is not.
- **The aliasing assumption is not proved.** Nothing in a module says a store
  cannot land below the stack pointer. No compiler emits one — which is an
  assumption, and is why this level says it is guessing rather than being the
  default.

And the prize is small, measured on the corpus by `how_much_of_each_capture_
level_1_can_place`: between 0.2% and 28% of frames have anything promotable at
all. Compiled C reaches its own frames constantly.

## Level 2 reads a declaration, it does not infer one

`--level 2` names functions from the C++ RTTI. That is not the guess the level
model's word "speculative" suggests, and the distinction is the whole design:
Itanium's ABI *writes the names down*. A `type_info` is `{vptr, name}` in the
data segments, a vtable is `{offset-to-top, type_info*, slots…}` beside it, and
a slot is a table index that resolves to a function. `analysis::classes` reads
those bytes; nothing in it looks at what any function does.

What keeps it from finding classes in modules that have none is counted rather
than assumed:

- **`TYPE_INFO_KIND_FLOOR`** is how many `type_info` candidates must share a
  vptr before that vptr is a kind. It is 3, and 3 was swept, not picked: at 2
  two of the three C captures report a class that is not there; at 3 all three
  report none; at 4 `JgwtTQVeWPm` loses three real classes and a whole kind. The C
  modules coming back empty is what gives the 692 their meaning, so
  `what_level_2_reads_out_of_each_capture` asserts it rather than printing it.
- **The single-inheritance kind is counted too.** Itanium's third word is the
  base's `type_info`, but only for `__si_class_type_info`, and which kind that
  is is not knowable from a vptr value. So it is the group that answers: a kind
  with at least `TYPE_INFO_KIND_FLOOR` members whose third word points at
  another candidate is that kind. A pointer into the object's own twelve bytes
  is excluded — an object cannot contain its own base — which is a bound, not a
  heuristic.
- **A base a confirmed class names is a class.** That is the one route by which
  a `type_info` the count missed still gets named, and it is the module's own
  statement rather than an inference from it. One hop only: an admitted class's
  kind was never confirmed, so its third word is not read as a base. On the
  corpus this adds nothing at all (`by_base` is 0 for all six); on a small
  fixture it is the difference between finding `Square` and finding `Square`
  and `Shape`, which is why the emscripten test is where it earns its keep.
- **A function in more than one vtable is named after none of them.**
  Inheritance puts a base's method in every derived vtable — 204 of
  `JgwtTQVeWPm`'s 1813 vtable functions are shared, the worst across 333
  vtables — so an owner picked from several is a claim the bytes do not make.
- **A name that cannot be demangled stays mangled**, for the reason the
  fingerprint catalogue drops an ambiguous name: a wrong name is worse than
  `f8421`. `demangle_type` reads nested names, source names and `St`, and elides
  a template argument list as `<…>` rather than resolving substitutions it
  cannot check. Skipping an argument list is the part that goes wrong quietly:
  the anonymous namespace is a source name holding an `N`, and counting that as
  structure runs the skip past the `E` closing the name around it — which reads
  as a demangling that worked. Ten of `JgwtTQVeWPm`'s names were readable only
  by that accident; with source names, substitutions and `St` handled, 661 of
  its 692 read and 31 stay mangled.

And unlike level 1, **level 2 gives nothing up**. It is identifiers and doc
comments, so `assert_agrees_at_level_2` is the level-0 assertion unweakened —
the same calls, the trap, and the whole of linear memory — run again with the
names on. It also compiles the renamed output, which is the other way a name can
go wrong: `sanitize` is what stands between a demangled string and an identifier
position.

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

## Nesting is the module's, and it costs more than it looks

wasm's `block`/`loop`/`if` become Rust's labelled blocks one for one, so the
output nests exactly as deep as the module does. A `br_table` dispatch in the
VoIP module nests **2466** of them, and two things follow that neither the
fixtures nor the small captures show:

- **rustc parses nesting recursively, on an 8 MiB stack, and overflows it.** It
  dies with `SIGSEGV` and a backtrace through `parse_expr_assoc_with`, which
  reads as a compiler bug. `RUST_MIN_STACK=134217728` compiles the same file.
  The harness sets it, and `unwasm decompile` says so — `analysis::deepest_nesting`
  and `NESTING_RUSTC_HANDLES` — because the alternative is a reader concluding
  the output does not compile.
- **The indentation is capped at 32 levels** (`MAX_INDENT`). Uncapped, a line
  inside that dispatch carried 7964 spaces: one part file was 650 MB, 644 MB of
  it whitespace, and the module 1.8 GB. Capped it is 227 MB and the
  decompilation itself halves. Nothing is lost — nobody counts two thousand
  levels of leading space, and `'b2465` says exactly what the space only
  suggested.

Both were invisible until the whole VoIP module was taken through rustc, which
is the argument for doing that rather than trusting that what works on 2 MiB
works on 10.

## Unreachable code is not code

After `br`, `return`, `unreachable` or `br_table`, wasm's stack is polymorphic
and Rust's is not: the instructions that follow would not type-check if they
were emitted, so they are skipped until the frame closes. The skip tracks
nesting, because a `block` inside dead code has an `end` that must not be
mistaken for the enclosing frame's.

## Coverage is a floor, not a target

**99.06% of lines — 99 of 10578**, on a checkout with the `#[ignore]`d tiers not
running: `codegen.rs` 40, `analysis.rs` 30, `module.rs` 20, `main.rs` 6,
`hostlib.rs` 3. `rt.rs`, `error.rs` and `ops.rs` are complete.

It was 99.68% before level 2, and the drop is the level's own doing: the class
recovery's evidence lives in real modules, so the paths that read a *placed*
segment or a name it cannot demangle are reached by the `#[ignore]`d tiers and
not by this number. What the fast tier does cover is a hand-laid RTTI image in
`backend.rs` and `analysis.rs`'s own tests, which is why it is 99.06% and not
the 98.27% the first level-2 draft measured.

The number was **99.5%** here and **99.6%** in the README before either was
re-measured, and **99.20%** after. Re-measure before quoting it; a coverage
figure that is only ever copied forward is the kind of claim this project exists
to not make.

What is left is almost all unreachable rather than untested. In `module.rs`:

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
not assemble, and the third is unreachable by construction. `hostlib.rs`'s are
the same shape — a directory key already ending in `/`, and a descriptor number
overflowing `i32`. The rest is ordinary uncovered code and is not claimed to be
anything else.

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

**Decompiling was itself quadratic until recently, and every wall-clock figure
on this page was measured with that in it.** The index recorded which line each
function starts on by counting the lines of everything written so far, once per
function — 152 of the 154 seconds a 3.3 MiB capture took, and twenty-five
minutes of the ten. Counted forward it is 11.8 seconds and 33.2 seconds, for
byte-identical output, and the whole `captured` tier went from 1887 seconds to
47. The lesson generalises: before optimising what the output costs to *compile*,
check what it costs to *produce* — `unwasm inspect` is parse and analysis alone,
and the difference between it and `decompile` is the emitter's own bill.

## Layout is a compile-time decision, and it was measured

rustc partitions codegen units along module boundaries. The 2.0 MiB capture in
one file took **22m49s**; split at 16 functions per file — 192 files — **25.7s**.
Same binary, same behaviour, 53×.

The default was chosen from the whole curve (1 / 13 / 49 / 192 / 764 files:
22m49s / 7m36s / 2m34s / 25.7s / 10.4s), not from the first improvement that
looked good. The table is in `Layout::FUNCTIONS_PER_FILE`. Any change to it
should be measured the same way rather than argued about — and the system time
is the number to watch, since it was 1102s for one file and 12.6s for 192.

Those numbers are rustc's, and they still stand: 1102s of *system* time is not a
figure a decompiler's own arithmetic produces. But the one-file row was taken
before the quadratic above was found, so some of its 22m49s was ours. The shape
of the curve is what the default rests on, and that has not changed; if the
exact figure ever matters, re-measure it rather than quoting this one.

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

1. **Drive the VoIP module.** `unwasm host` writes **103 methods for
   `JgwtTQVeWPm`, 35 of them still `todo!()`**. Its allocator already runs:
   `captured.rs` compiles a 42-function slice and compares it against the
   engine, linear memory included. What the remaining 35 are is worth knowing
   before adding to them:

   - **22 are WhatsApp's own callbacks** — eleven `call_*_js_sync` capture and
     playback drivers, plus `on_call_event_js_sync`, `renderVideoFrame_js`,
     `sendSignalingXMPP_js_sync` and the rest. Nothing but the application can
     answer them.
   - **6 are the C++ catch-matching and primary-exception entry points**,
     refused on purpose — see the note on `__cxa_find_matching_catch_*` above.
   - **2 are `asm_const`**, JavaScript the module carries.
   - the rest are `gethostbyname`, the offscreen canvas, `longjmp`,
     `emscripten_receive_on_main_thread_js` and the mailbox postmessage.

   The last two are the only ones a capability would unblock, and neither is a
   missing capability — `emscripten_receive_on_main_thread_js` marshals its
   arguments as *doubles* and relies on JavaScript coercing each one to the
   callee's parameter type, and reproducing that coercion is a guess about a
   conversion nobody here can check. It stays a `todo!()` for the same reason
   `__cxa_find_matching_catch_*` does.

   What is *not* left is anything a standard already decides, or anything that
   needed the instance. Both of those were written.
2. **Level 2 — the rest of it.** The half that is in is the half the module
   writes down: class names and vtables from the C++ RTTI (`--level 2`,
   `unwasm classes`), and what embind's registrations declare. What is still
   open is the half that has to be inferred — structs from access patterns, and
   parameter roles read from how the code uses them, in the manner of
   `wa-wasm-oracle`'s `abi.rs`: dereferenced means pointer, and the access width
   says what it points at. That half *is* speculative, and it needs the same
   treatment level 1 got — opt-in, and saying at every site what it rests on.
3. **Improve what a catalogue recognises.** `--stub-recognised` leaves the
   bodies out now, so the size of the cut is exactly the catalogue's recall:
   ~91% across builds of one toolchain and a handful across emscripten
   versions. The mechanism is not the limit; the fingerprint is.
