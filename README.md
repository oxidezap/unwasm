# unwasm

A WebAssembly decompiler that emits Rust — and whose output always compiles,
always runs, and is checked against the module it came from.

```console
$ unwasm decompile module.wasm -o module.rs
wrote module.rs (50682 lines, 478 functions)
```

The claim other decompilers cannot make is the one this project is built
around: **the decompilation is executable, so its faithfulness is measured
rather than asserted.** Every test below runs the same calls twice — once on the
wasm module in V8, once on the generated Rust — and compares the returned value,
the trap, and the final contents of linear memory.

## Repository boundary

This repository owns only the general WebAssembly-to-Rust pipeline:
`unwasm-core`, `unwasm-cli`, the generated runtime and their differential tests.
Application hosts, protocol captures and codec derivation recipes live with
their consumers. The WhatsApp Wasmtime oracle is therefore maintained under
`whatsapp-rust/tools` and depends on `unwasm-core` by an immutable Git revision.

`cargo build -p unwasm-cli` resolves only the decompiler graph. `unwasm-core`
keeps `wasmparser` as its sole dependency; generated output remains
self-contained. Real WhatsApp modules are one external regression corpus among
several and do not define a runtime or protocol API in this workspace.

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
- **Imports that can only be answered from inside are generated**, not asked
  for: Emscripten's `invoke_*` trampolines — 134 of the VoIP module's 242
  function imports — plus `__pthread_create_js`, the main thread's own
  initialisation, and the `mmap` family, which allocates out of the module's own
  `memalign` and reads the file through the host's `fd_pread`.
- **Nothing is skipped.** An opcode with no faithful Rust form is an error
  naming the construct — never a comment in the output, never a stub.
- **No `unsafe`.** `unsafe_code = "forbid"` for this crate and for what it
  emits. A decompiler that needs raw pointers to model a sandbox has lost the
  property that made the sandbox worth reading.

### On the real modules

Every WhatsApp Web capture decompiles — the corpus as `cargo xt fetch-captures`
fetches it today, measured by `cargo test --test captured -- --ignored`:

```
COs9e0Kj0ic:   478 functions,   50682 lines of Rust  (VOPRF/crypto, 236 KiB)
php8T1oSIZM:   321 functions,   51798 lines          (mozjpeg, 376 KiB)
a19OxQ3jkd2:  9093 functions,  898131 lines          (3.3 MiB, WASI)
ayqr5HQtlkb:  3055 functions,  470524 lines          (2.0 MiB)
rogm88TRRiw:  2157 functions,  356954 lines          (2.1 MiB)
JgwtTQVeWPm: 14733 functions, 2508203 lines          (VoIP/PJSIP, 10.2 MiB)
```

All six decompile. The last one took a shared imported memory, 134 `invoke_*`
trampolines and the atomics; see below.

`COs9e0Kj0ic` compiles with rustc in about 2.5 seconds and instantiates, which
runs the module's own `__wasm_call_ctors` and every static initialiser with it.

`ayqr5HQtlkb` — 2.0 MiB of wasm, 3055 functions — compiles and instantiates in
**23 seconds**. In a single file the same module took **22m49s**.

`D5pLH9sfOOl`, the 9.4 MiB VoIP module — 13347 functions, 2.4M lines of Rust
across 415 files, a shared imported memory and 1070 atomics — **compiles and
instantiates in 13m44s**, and comes up with the 160 pages of memory its import
declares. Its `start` function runs during instantiation without needing a
host.

That run was on `D5pLH9sfOOl`, which WhatsApp has since stopped serving. Its
successor `JgwtTQVeWPm` is larger again and has now been through rustc too:

```console
$ unwasm decompile JgwtTQVeWPm.wasm -o src/generated   # 17s, 455 files, 2.5M lines
$ unwasm host JgwtTQVeWPm.wasm --defaults -o src/host.rs
$ RUST_MIN_STACK=134217728 cargo run
   Finished `dev` profile [unoptimized] target(s) in 11m 23s
instantiated in 1.454084497s
memory: 10485760 bytes
table: 10762 slots
nothing was answered with a default
```

Ten megabytes of somebody else's wasm, compiled and instantiated — and the last
line is the one that matters: its `start` ran through to the end without asking
the host a single question, so nothing in that run rests on a default. Every
rustc timing on this page names the build it was measured on.

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

### Deep nesting needs a bigger parser stack

wasm's `block`, `loop` and `if` become Rust's labelled blocks one for one, and
rustc parses that recursively on an 8 MiB stack. A `br_table` dispatch in the
VoIP module nests **2466** blocks, and rustc dies on it with `SIGSEGV` and a
backtrace through its own parser — which reads as a compiler bug rather than as
a file that needs a bigger stack:

```console
$ RUST_MIN_STACK=134217728 cargo build
```

`unwasm decompile` says so at the point it writes the file, rather than leaving
it to be discovered:

```
note: function #12919 nests 2466 blocks, and rustc parses nesting recursively.
      Compile this with RUST_MIN_STACK=134217728 set, or rustc overflows its
      stack and dies with SIGSEGV.
```

The indentation stops at 32 levels for the same reason it exists at all. Two
thousand levels of leading space is not readable, and it is not free: uncapped,
one part file of that module came to 650 MB — 644 MB of which was whitespace —
and the module to 1.8 GB of Rust. Capped it is 227 MB. The labels are what say
where a `break` goes; `'b2465` is exact where the indentation was only wide.

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

An address that holds no text but that dozens of functions reference is pointed
out too: `1352840i32 /* address, in 72 functions */`. A context pointer reads as
noise in each function separately and as shared state once you notice it is the
same number everywhere. What counts as an address is decided by the span the
module's own data segments occupy — otherwise `2147483647` comes out as the
most widely shared address in the module, and it is arithmetic.

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

**3313 of its 13347 functions get a name this way**, from two sources:

- **`__assert_fail(expr, file, line, func)`**, whose last argument is
  `__func__` — the compiler writing the name into the binary. That is not a
  guess, and it beats everything else. 17 functions in the VoIP module.
- **The messages the function logs**, ranked by *how often* it references them.

Frequency rather than uniqueness, because of inlining. A function assembled out
of several inlined ones references all of their strings, and the one message
that belongs to nobody else is often the one about a callee that *failed*:
`..._create_participant: wa_vid_quality_manager_create error: %d` names the
callee. Function #10532 was named after that callee until the rule became "a
message it logs eleven times is about this function; one it logs once may be
about anything".

Three things are refused outright: an identifier that is a Rust module path
(`call_control::…` names a module, and appears in every function of it), a
message logged once by more than one function (nothing there tells them apart),
and anything not shaped like an identifier.

The runners-up go in the doc comment. A function built from several inlined ones
has several plausible names, and seeing the ones that lost is what stops a
reader trusting the winner more than it deserves.

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

### The API the module publishes

embind is how a C++ module tells JavaScript what it exposes. The registration
runs at startup — but its arguments are constants, including the pointer to
each name *and the pointer to the array of type ids*, so the whole thing reads
statically:

```
function int initVoipStack(std::string, std::string, std::string)
function void handleIncomingSignalingOffer(std::string, std::string, std::string,
         std::string, std::string, bool, bool, std::string, Uint8List)
class Uint8List
class_constructor Uint8List::Uint8List()
```

Each type id is the address of a `std::type_info` that some other registration
names, so `std::string` and `bool` come back as themselves rather than as `i32`.
**This is the only place in a stripped module where a type has a name at all** —
everything else this crate recovers is what the compiler left behind, and this
is what the author published. 114 registrations in `JgwtTQVeWPm`, 81 named, 40
with full signatures.

Three details that each cost a wrong answer before they were right: a class
registers three ids for itself (the class, a pointer, a const pointer), so a
method returning the second is returning the class; the types can be registered
*after* the function that uses them, so it takes two passes; and `int` is three
characters, which the general text reader refuses as too short to be a string —
losing the most common type in any module.

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

Turning those slots into Rust variables is `--level 1`, and it is opt-in
because it stops the decompilation being byte-exact: the bytes a promoted slot
used to leave in linear memory are no longer there. That is a real property to
give up and not one to give up quietly, so the level says it is doing so — at
every function it changed, and at every function it refused and why:

```rust
/// **Level 1**: 5 of 5 slots are Rust bindings rather than memory —
/// `s12` at +12, `s16` at +16, `s20` at +20, `s24` at +24, `s28` at +28.
/// They are filled from the frame on entry, so a slot read before it is
/// written still sees what was there; they are never written back, so this
/// function no longer leaves those bytes in linear memory. That is the
/// exactness level 1 gives up, and it is given up here.
```

The answers still match the engine — that is tested, on a struct of `int`,
`short`, `signed char` and `unsigned char`, because a narrow binding has to mask
on store and sign-extend on load. The memory does not, and the test reports the
difference rather than hiding it.

The prize is small, and measured rather than estimated. Across the corpus, the
frames with anything promotable at all:

| capture | frames with something to promote | slots |
|---|---|---|
| `COs9e0Kj0ic` | 64 of 228 | 592 of 1355 |
| `php8T1oSIZM` | 2 of 86 | 2 of 378 |
| `a19OxQ3jkd2` | 1316 of 6842 | 4188 of 100119 |
| `ayqr5HQtlkb` | 9 of 1791 | 11 of 21782 |
| `rogm88TRRiw` | 2 of 1211 | 3 of 13858 |
| `JgwtTQVeWPm` | 53 of 4916 | 87 of 30980 |

Compiled C reaches its own frames constantly — it passes `&point` to something,
indexes an array in it, or packs two variables into a word — and what survives
all of that is a minority.

### The classes the module declares

`--level 2` is names, and the names are not inferred from behaviour — they are
read out of a declaration the compiler wrote down. The Itanium ABI puts a
`type_info` in the data segments for every polymorphic class and a vtable beside
it, and both are byte layouts: `{vptr, name}` for the first, `{offset-to-top,
type_info*, slots…}` for the second. `unwasm classes` prints what that says:

```
692 classes, 482 with vtables, across 5 `type_info` kinds
  WasmShimErrorHandler                                 vtable 0xf48d8, 12 methods
      mangled 20WasmShimErrorHandler
      derives from executorch::extension::DynamicShim
  std::__2::__shared_ptr_emplace<…>                    vtable 0xf4d60, 5 methods
      mangled NSt3__220__shared_ptr_emplaceI13canvas_windowNS_9allocatorIS1_EEEE
      derives from std::__2::__shared_weak_count
```

A vtable slot is a function, so a class with a vtable names the functions in it:
`f9821` becomes `f9821_WasmShimErrorHandler_v3`, with a doc comment giving the
`type_info`'s address, the mangled string, the vtable's address and the slot.
**The method's own name is nowhere in the module** — only its position is — so
that is what the name carries, and the index stays in it.

Four refusals keep it honest:

- **A vtable pointer only a couple of `type_info`s share is not one.** Itanium
  gives every `type_info` a vptr, and a program has a handful — one per *kind*.
  A pointer 384 candidates agree on is `__class_type_info`; a pointer one
  candidate has is two words that happened to line up. The floor is 3, swept
  over the corpus: at 2 two of the three C modules report a class that is not
  there, at 3 all three report none, and at 4 the VoIP module starts losing
  real ones.
- **A function in more than one vtable is named after none of them.**
  Inheritance puts a base's method in every derived vtable — 204 of the VoIP
  module's 1813 vtable functions are shared, one of them across 333 vtables —
  and picking one owner would be a claim the bytes do not make.
- **A name that cannot be demangled stays mangled.** Template arguments need
  Itanium's substitution table, and a substitution resolved wrongly is a name
  that says something the module did not, so `I…E` comes out as `<…>`: the
  class is named and the instantiation is visibly elided. 31 of `JgwtTQVeWPm`'s
  692 come back as their mangled string rather than as a guess.
- **What the module says about itself wins.** A function the name section
  already names, or a catalogue already recognised, keeps that name.

Measured across the corpus — where the three C modules are the control, and
their zeroes are what give the rest their meaning:

| capture | classes | with vtables | kinds | functions named |
|---|---|---|---|---|
| `COs9e0Kj0ic` | 105 | 83 | 2 | 107 of 478 |
| `php8T1oSIZM` | 5 | 2 | 1 | 6 of 321 |
| `a19OxQ3jkd2` | 0 | 0 | 0 | 0 of 9093 |
| `ayqr5HQtlkb` | 0 | 0 | 0 | 0 of 3055 |
| `rogm88TRRiw` | 0 | 0 | 0 | 0 of 2157 |
| `JgwtTQVeWPm` | 692 | 482 | 5 | 1609 of 14733 |

Level 2 gives up nothing. Level 1 trades byte-exactness for readability and says
so; level 2 is identifiers and comments, and the differential test runs the
whole thing again with the names on — same calls, same trap, same linear memory.
A class name that changed a result would be a bug of the same severity as wrong
arithmetic.

### What is left for a host

After decompiling, what remains is the part only a host can answer — and half
of it is not application-specific at all. `unwasm host` writes both: the
mechanical imports, implemented, and the rest as `todo!()`.

A small Emscripten program needs nothing beyond the mechanical set, which means
it *runs*:

```console
$ emcc -O1 hello.c -o hello.wasm -sSTANDALONE_WASM --no-entry
$ unwasm decompile hello.wasm -o src/generated.rs
$ unwasm host hello.wasm -o src/host.rs
$ cargo run
hello 42 1.500
```

That `printf` went through the module's own musl — formatting, the stdio
buffer, the iovec array — and came out of the generated `fd_write`. The
filesystem it writes to is a `BTreeMap<String, Vec<u8>>` in the host struct;
nothing escapes to the real one, because a module you are running to find out
what it does should not be able to open `/etc/passwd`. Randomness and the
clock are supplied rather than taken, so two runs produce the same bytes.

The skeleton:

```console
$ unwasm host JgwtTQVeWPm.wasm -o host.rs
wrote host.rs (106 methods, 39 of them still to implement)
```

```rust
impl Imports for Host {
    // 106 methods, 67 of them answered by the library above
    // and 39 left for you. 134 of the module's 242 imports are Emscripten
    // exception trampolines and are generated, so they are not here.

    // ---- WASI. A subset over an in-memory filesystem answers these.
    fn wasi_snapshot_preview1_fd_write(
        &mut self,
        _caller: &mut rt::Caller<'_>,
        p0: i32, ..
    ) -> i32 {
        todo!("wasi_snapshot_preview1::fd_write")
    }
    // ---- The application's own callbacks. Nothing but the application can
    // say what these should do.
    fn env_on_call_event_js_sync(&mut self, _caller: &mut rt::Caller<'_>, p0: i32, p1: i32) {
        todo!("env::on_call_event_js_sync")
    }
```

Every method takes a `rt::Caller` as well as its arguments. A wasm import is
handed numbers, and almost every interesting one is a number *into* linear
memory — `fd_write` gets the address of an iovec array, `__assert_fail` the
address of a string — so without the memory a host cannot answer at all. The
`Caller` carries it, and is a struct rather than a bare `&mut Memory` because
it is where the next capability goes: adding a field costs nothing, while
adding a parameter changes every host ever written.

Grouped by where each import comes from, because 106 methods in one list is a
wall and the same 106 split into "these are WASI", "these are the C++ runtime"
and "these are yours" is a plan. What is left is `todo!()` rather than a stub
returning zero — a stub compiles, runs, and is wrong, and the module cannot
tell "not written yet" from "answered 0".

An implementation is emitted only when the signature matches exactly.
Emscripten has changed these shapes before, and a body written for the other
shape would read the wrong argument as a pointer. `_embind_register_class`
with two arguments instead of thirteen stays a `todo!()`.

### Reading a module rather than translating it

Decompiling the VoIP module gives 365 MB of Rust. Looking at three functions in
it should not, and there are three affordances for that:

```console
$ unwasm decompile voip.wasm -o out/ --only 10532,12114,458
wrote out/ (42 Rust files plus names.json, 281573 lines, 13347 functions)
```

The functions asked for come out in full; the rest keep their signatures and
their names and become `unimplemented!()`, so the result still compiles and an
editor can still follow a call.

**`names.json`** comes with any directory output: every function's index, name,
file, line, how it was named, and which table slots reach it. That is the thing
to look a function up in, rather than `grep -n` over two million lines.

**`unwasm calls`** answers the other question a module does not record. Every
call site says what it calls; nothing says what calls *this* function, and that
is the direction a reader wants first.

```console
$ unwasm calls voip.wasm 10532
f10532  (i32,i32,i32,i32,i32,i32) -> (i32)  wa_call_group_create_participant

called by 6:
  f10425  …  wa_call_start_internal
  f10534  …  wa_call_invite_internal
  …
calls 38:
  f4174   …  wa_vid_quality_manager_create
```

Each function carries the same in its doc comment, and a function nothing calls
says which kind of nothing: exported, reached only through the table, or
neither — an entry point or dead code.

The *sites* are counted as well as the callers, and the difference is the point:
one caller that calls from a loop body is one caller and many sites. A function
reached from 58 sites is one whose body must not be instrumented to answer a
question about one of them — the measurement would be of whichever site ran.

**`--instrument-stores`** turns "who wrote this address?" from a day into a
run. The output executes, so the question is one the machine can answer:

```console
$ unwasm decompile voip.wasm -o out/ --instrument-stores
```

```rust
instance.memory.watch(0x24bed0, 4);
instance.memory.stop_on_hit(true);   // or leave it off and read memory.hits()
instance.start();
// panicked at 'watchpoint: function #10284 wrote 4 bytes at 0x24bed0 (Fill),
//              from the instruction at file offset 4667241'
```

A hit names the *instruction*, not only the function: a function with fifty
stores in it leaves the next question open, and `unwasm bytes voip.wasm 4667241
8` prints the one that fired.

Every write goes through the check — including `memory.fill` and
`memory.copy`, since a `memset` is the usual answer to "who zeroed it" and it
is not a store. A hit records the function that did it, the address, the width
and which kind of write it was; `stop` panics at the write instead, so the
backtrace is the guest's own call chain. Nothing is watched until a host asks,
and the plain output does not route through the check at all.

**`unwasm frames --outside`** is the static half of the same question. The
prologue says how big the frame is and the walk records where each store went,
so a store at or past the end — an overrun into the caller's frame — is a
finding the analysis already has the numbers for:

```console
$ unwasm frames voip.wasm --outside
f10284                          1168 bytes  47 slots  (address escapes)
    1 writes through a computed frame address — offset unknown
…
226 of 4375 frames write outside themselves or through a computed address
```

Constant overruns are rare — zero in the VoIP module, since a compiler would
have to emit one on purpose. What the list is really for is the second line:
an indexed write into a frame array, whose offset is not knowable statically.
226 of 4375 is a short enough list to read.

**`--offsets`** writes `offsets.json` beside the output: for each generated
line, the offset and length of the wasm bytes that produced it. Patching a
module by hand otherwise means computing an LEB encoding and counting bytes,
and a slip there does not look like a slip — the pattern is simply not found,
which reads as "the code changed" rather than "the arithmetic was wrong".

```console
$ unwasm decompile voip.wasm -o out/ --offsets
$ unwasm bytes voip.wasm 390 7
390 (0x186) + 7: 23 00 41 10 6b 22 09
1 occurrence of that sequence in the module — unique, so a pattern patch is safe
```

A line's span covers every operator lowered since the previous line, so the
operands folded into it are inside the span rather than missing from the map.
`unwasm bytes` answers the other half: what is actually there, and whether the
sequence is unique — which is what decides if a pattern patch is safe.

**`unwasm constants`** finds every site that pushes a value — all of them,
which is the point. An error code that turns up 481 times is not the nine sites
a `grep` of the decompiled output finds, and an account built on the nine is
guessing:

```console
$ unwasm constants voip.wasm 70008
f11198_make_and_cache_offer  i32.const at 5095372 + 4 bytes
…
482 sites push 70008, and its four bytes appear 1 more time inside the data segments
```

Each site comes with its offset and its encoded length, which is what a
same-length replacement needs: give all 482 a distinct value, run, and the
engine's own log says which one fired. The data count is separate on purpose —
counting the bytes of a number is not counting the sites that push it.

**`--only … --with-callees`** brings the functions a function calls along with
it. One level, not the transitive closure: reading a function and needing the
next is the same minute, and needing the whole closure is the whole module.

**`unwasm table`** answers the question a call site cannot: `call_indirect`
takes a *table* index, not a function index.

```console
$ unwasm table voip.wasm --type "(i32,i32,i32) -> ()"
414 of 9290 slots matching (i32,i32,i32)->()
  slot 18     f336    (i32,i32,i32) -> ()
```

Each function also says where it sits: `/// In the function table at slot 7897`.

### Recognising library code, and what that is actually worth

Most of a 9 MiB module is not the application: it is libc, libc++ and whatever
else was linked in. Those halves have names in a module you build yourself, and
none at all in a shipped one — but the bodies are the same code. A **signature
catalogue** carries the names across.

```console
$ emcc -g2 ... -o reference.wasm       # a build of your own, names kept
$ unwasm signatures reference.wasm -o libc.sigs
wrote libc.sigs (1841 signatures)
$ unwasm decompile voip.wasm -o out/ --signatures libc.sigs
```

A fingerprint is the opcode sequence with everything a rebuild renumbers left
out: constants, load and store offsets, callee indices, global and data-segment
indices. What stays is the shape — the operators, the control flow, and the
*kind* of each access, since a byte load and a word load are not the same
function however alike the rest reads.

The measurements, because the honest answer is "narrower than it sounds":

| catalogue from | matches |
| --- | --- |
| another build of the same source, same toolchain | ~91% |
| a different emscripten version | 2 of 33 |

End to end, a catalogue of the 14 functions one `printf` drags in, applied to a
236 KiB capture built by a different emscripten version, names two of its 478:
`__towrite` and `frexp` — both leaves, both plausible, and both a rounding error
against the module. `tests/captured.rs` runs exactly that.

So a match is strong evidence and a miss is no evidence at all. This names
functions; it never marks one as unrecognised, and it never overrides a name the
module gave itself. A fingerprint two differently-named functions share is
dropped rather than resolved — the constants were what told them apart, and the
constants are exactly what a fingerprint leaves out. Functions shorter than
twenty instructions are not catalogued at all, since every *return the first
argument* in a module fingerprints alike.

A coarser fingerprint was tried to close the cross-version gap and abandoned:
matched against a module sharing no code whatsoever it produced seven matches
out of ten, so what it closed was noise. Build the reference yourself; do not
expect a catalogue to recognise someone else's toolchain.

### Compiling only what a path can reach

Decompiling the VoIP module gives 2.3 million lines of Rust that take
**21 minutes** to compile. For an investigation that is the bottleneck, and the
fix is not to compile the other 82%:

```console
$ unwasm decompile voip.wasm -o out/ --reachable-from 10425 --direct-only
wrote out/ (116 Rust files plus names.json, 510667 lines, 13347 functions)
$ cargo build
Finished in 11.10s
```

**11 seconds instead of 21 minutes.** The functions left out keep their names
and signatures and become `unimplemented!()`, so the result still builds — and
a run that reaches one stops and says which function it wanted, which is a
worklist rather than a wrong answer:

```
not implemented: function #229 was not decompiled: --only
```

Without `--direct-only` the set is complete and useless: `call_indirect` names
a type rather than a target, so every table slot with a matching signature
joins it, and on this module that is 98% of the functions. That number is worth
knowing rather than hiding — it is what "could run" honestly means here.

`start` always comes along, since instantiation runs it before anything the
caller asked for.

## Usage

```console
$ unwasm inspect module.wasm      # what the module contains
$ unwasm table module.wasm        # what each table slot holds
$ unwasm classes module.wasm      # the C++ classes it declares, and their vtables
$ unwasm host module.wasm         # the skeleton of what a host must answer
$ unwasm decompile module.wasm    # to stdout
$ unwasm decompile module.wasm --level 2      # and names from the RTTI
$ unwasm decompile module.wasm -o out.rs      # one file
$ unwasm decompile module.wasm -o out/        # mod.rs, parts, names.json
$ unwasm decompile module.wasm -o out/ --only 10532,12114
$ unwasm decompile module.wasm -o out/ --reachable-from 10425 --direct-only
$ unwasm signatures reference.wasm -o libc.sigs   # a catalogue, from a build with names
$ unwasm decompile module.wasm --signatures libc.sigs
```

Every command takes the module first and its flags after it, and every one
answers `--help` on its own.

### Reading the module's memory rather than its file

```console
$ unwasm data module.wasm 0x103564 48      # guest memory at an address
$ unwasm vtable module.wasm 0x103564       # that address read as a vtable
$ unwasm vtable module.wasm --class N6webrtc20ResidualEchoDetectorE
$ unwasm stores module.wasm --offset 846 --size 1   # who writes that field
$ unwasm constants module.wasm 5103 --data # and where the data holds it
```

`bytes` takes an offset into the wasm *file*. `data` takes an address in the
*guest*, which is a different number: the segment that covers it decides the
mapping, and a threaded module's segments are passive and carry no address at
all until the `memory.init` calls have been resolved. `data` says which segment
covers the address, the file offset of the byte — the number `bytes` and `patch`
take — the hex, and the words as u32 with the strings they point at. An address
no segment covers is not an error; it is memory the module never initialises,
and it reads as zero at run time.

`vtable` reads the same bytes as a table of function pointers: table index,
function, signature. **A slot holding 0 is a pure virtual function**, and a
`call_indirect` reaching one takes table index 0, mismatches its signature and
traps — which kills the thread rather than returning an error anybody catches.
From the outside that looks like the engine dying for no reason; here it is one
line. The read stops at the first word that is neither a live table index nor a
null run a live index follows, so it does not report the next object's bytes as
methods. `--class` takes a name `classes` printed and finds the address itself.
`classes --methods` counts the same slots, nulls included.

`stores` answers "who writes byte +846 of this struct". It follows the constant
displacements a function applies to its own parameters and locals, so a field
written through `base = p - 8` as `+854` is still found at 846 — a grep of
decompiled output finds neither the number nor the write. `--exact` is the
literal search, `--kind load|store|both` picks the direction, and functions
whose operand stack the walk lost are named rather than silently skipped.

`constants <n> --data` lists every address in the data segments holding those
four bytes, marking the four-byte-aligned ones. A function pointer installed in
a vtable is written by the linker and pushed by no instruction, so a search of
the code finds nothing at all.

### Reading three functions out of fifteen thousand

```console
$ unwasm decompile module.wasm --only 7497 --bare    # just that function
$ unwasm decompile module.wasm --only 7497,7493 --spans
index   name                                     file        first    last
7493    f7493_webrtc_v2_aecm_create              mod.rs      167903   168708
7497    f7497                                    mod.rs      168750   168859
```

`--only` on its own keeps the other fifteen thousand as stubs so the result
compiles. `--bare` drops them, and the runtime and the imports with them: the
result does not compile and is a hundred lines instead of a million. `--spans`
prints where each function starts and ends in the file that would be written,
which is what a slice needs — searching for the next `fn f<n>` stops at the
wrong closing brace, and a truncated body reads as a complete one.

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
    fn env_add(&mut self, _c: &mut generated::rt::Caller<'_>, a: i32, b: i32) -> i32 {
        a + b
    }
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
cargo test --workspace                                  # 564 tests, ~20s
cargo clippy --workspace --all-targets -- -D warnings
cargo llvm-cov --workspace --summary-only

cargo xt fetch-captures                             # ~19 MB, once
cargo test --test captured -- --ignored --nocapture     # the real modules
cargo test --test emscripten -- --ignored --nocapture   # the real toolchain
```

The harness needs `node` (the reference engine), `clang` (wasm32 fixtures),
`wasm-tools` (wat assembly) and `rustc`. A missing tool **fails** the tests
rather than skipping them: a run that compared nothing must not report the same
green as a run that compared everything.

**The clang version matters**, and it is the one requirement that is not
obvious. `read_prologue` knows four spellings of the shadow-stack prologue,
each found by reading what clang actually emits at `-O0` — so which frames the
analysis can see is a property of the compiler that built the fixture. clang 22
is what these tests were written against and what CI pins; Ubuntu 24.04's
clang 18 emits the fourth spelling, where every intermediate value goes through
a local of its own, and the frame tests in `backend.rs` pass under both. A
fifth compiler may well have a fifth spelling — before concluding that a module
has no frames, check what built it.

`emcc` for a C++ fixture is `em++`. The driver decides which runtime to link,
and `emcc` links libc only: on Emscripten 6 a `.cpp` fixture given to `emcc`
fails the link with `undefined symbol: __cxa_throw`.

`cargo test --workspace` needs none of the above beyond those four tools —
it runs on a bare checkout.

### The captures

The `#[ignore]`d tiers run against the real WhatsApp Web modules. Those are
megabytes of somebody else's build output, so they are not committed:
`cargo xt fetch-captures` downloads them into `fixtures/wasm` (git-ignored)
from the public [`oxidezap/whatspec`](https://github.com/oxidezap/whatspec)
archive, and checks each one against the sha256 pinned in
`fixtures/captures.sha256`. A capture that arrives with different bytes is not
the module the tests pin their numbers to, and is refused rather than used.

WhatsApp rolls these payloads: a module is reissued under a new id and the old
id stops being served. When that happens the corpus moves to the build that
succeeded it and every number a test pins is re-read against the new module —
which is why the counts in `captured.rs` and the ones quoted throughout this
README can name different builds. Each figure below says which.

`emcc` is needed only by the `#[ignore]`d Emscripten tests. On Arch the
`emscripten` package provides it — note that it replaces `binaryen`, which it
also provides, and puts `emcc` in `/usr/lib/emscripten`; the harness looks
there as well as on `PATH`, since the packaged `profile.d` entry only reaches
shells started afterwards.

### Coverage

**99.68% of lines** — 30 of 9438, counted from `cargo llvm-cov --workspace
--lcov` on a checkout, with the `#[ignore]`d tiers not running:

| file | lines | missed |
|---|---:|---:|
| `rt.rs` | 1878 | 1 |
| `codegen.rs` | 2548 | 7 |
| `analysis.rs` | 1888 | 0 |
| `module.rs` | 515 | 20 |
| `hostlib.rs` | 1750 | 2 |
| `error.rs` | 55 | 0 |
| `ops.rs` | 137 | 0 |
| `main.rs` (CLI) | 667 | 0 |

Nearly all of what is left is unreachable rather than untested. The `module.rs`
gap is all of it:

- the `other =>` arms over wasmparser's `#[non_exhaustive]` enums. The compiler
  requires them; the only value that reaches one is from a proposal no
  toolchain we target emits, and which `wasm-tools` will not assemble.
- two checks that the function and code sections agree — which the decoder
  guarantees by three separate routes, each with a test asserting the decoder's
  message.

`codegen.rs`'s and `hostlib.rs`'s are malformed-input defences of the same kind,
and `rt.rs`'s one is the retry arm of `grow`'s compare-exchange, which needs a
race to reach.

Those are kept. Deleting a defence to raise a percentage is how a decoder change
becomes a panic two years later. The rest is ordinary uncovered code and is not
claimed to be anything else.

Measure with LCOV rather than the text report — `cargo llvm-cov --workspace
--lcov` and count `DA:` records with a zero. The per-file text view misses lines
and reads as better than it is.

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
thing for a host to write. **`D5pLH9sfOOl`'s 227 function imports become 100.**

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
  23 seconds for 2.0 MiB of wasm, 11m23s for the 10.2 MiB VoIP module.
- **The VoIP module compiles, instantiates and runs its `start` with the
  generated host.** On the current capture `JgwtTQVeWPm`: 2.5 million lines of
  Rust across 455 files plus a 106-method host, built in 11m23s and
  instantiating 160 pages of shared memory in 1.45 seconds, with nothing
  answered by a default. Most of that host is written for you: `unwasm host`
  answers **68 of its 103 methods** — WASI over an in-memory filesystem, the
  C++ runtime, Emscripten's runtime, the clock and `strftime`, embind's
  registrations — and leaves 35. Twenty-two of those are WhatsApp's own
  callbacks, six are the C++ catch-matching entry points this refuses to guess
  at, two are `emscripten_asm_const_*`, which runs JavaScript the module
  carries, and the rest are a browser canvas, `gethostbyname` and `longjmp`.
  The ones that had to reach back into the instance — the pthread glue and the
  `mmap` family — are generated rather than asked for.
- **Its allocator runs, and agrees with the engine.** `captured.rs` decompiles
  the 42 functions `malloc`, `free` and `memalign` reach, compiles them, and
  compares against V8 running the whole 10.2 MiB file: the pointers returned
  *and* the whole of linear memory.
- **No SIMD, no reference types, no exceptions, no multi-value.** Each is
  refused by name.
- **Level 0 only.** Linear memory is bytes; a C local that lived in the shadow
  stack is still an address, and a struct is still an offset.

### Calling what a module only published by number

A module built with embind exports almost nothing directly — its API is a list
of registrations, and each one is a pair of table slots. Running it produces
that list, and `invoke_slot` uses it:

```rust
instance.__wasm_call_ctors();          // where the registrations happen
let call = instance.host.embind.function("startVoipCall").unwrap().call().unwrap();
instance.invoke_slot(call.invoker, &[i64::from(call.context), argument]);
```

On the VoIP module that list is **328 entries** — `initVoipStack`,
`setHideMyIp`, `class Uint8List`, `class_function push_back` — read from the
module as it registers them. The static reader finds 78 of the same
registrations without running anything, which is the subset whose arguments are
constants sitting immediately before the call; the two can be checked against
each other.

Which argument of a registration is the invoker and which is the context comes
from embind's own C signatures, not from guessing, and `Registration::call()`
answers `None` for a registration that is a type rather than something to call.

### Threads

A module built with pthreads declares a `shared` memory, and its threads are
instances of the same module over it — each with its own globals, its own
`__stack_pointer` above all. That is what the output models:

```rust
let mut main_thread = generated::Instance::new();
let mut worker = main_thread.spawn(host::Host::default());
worker.set_stack(0x20000);                       // its own, not the main one's
std::thread::spawn(move || worker.thread_entry(arg));
```

The memory is an `Arc<[AtomicU8]>` every instance holds a handle to — no
`unsafe`, and a plain wasm access on a shared memory *is* a relaxed atomic, so
that is the faithful model rather than a conservative one. `memory.atomic.wait`
and `notify` are real: a wait blocks until another thread notifies it, and
traps only when no other instance holds the memory, which is a fact about the
handles rather than an assumption about the model.

Its size is reserved at construction, because a shared memory cannot be
reallocated while other threads hold handles to it. The default is 64 MiB;
`with_host_and_reservation` says otherwise, and growing past it returns `-1`,
which the spec allows and which says exactly what happened.

**The guest's own `pthread_create` works.** `__pthread_create_js` is generated
rather than asked of a host, for the same reason the `invoke_*` trampolines
are: it has to reach back into the instance, and the `Imports` trait cannot.
What the new thread does is the order the glue does it in, none of it optional:

```rust
let high = worker.memory.load32(pthread_ptr, 48);   // the stack the guest allocated
let low  = high - worker.memory.load32(pthread_ptr, 52);
worker.g0_stack_pointer = high;
std::thread::spawn(move || {
    worker.emscripten_stack_set_limits(high, low);
    worker._emscripten_thread_init(pthread_ptr, 0, 0, 1, 0, 0);
    let result = worker.call_indirect_7(entry, arg);
    worker._emscripten_thread_exit(result);        // what a pthread_join waits for
});
```

Those two offsets — 48 and 52 — are the one *layout* in this: Emscripten's own
`C_STRUCTS.pthread.stack` and `.stack_size`, which its `establishStackSpace`
reads. `Analysis::PTHREAD_STACK_OFFSETS` is where they are written down, so a
build that moved them is corrected in one place rather than debugged. Leave out
the stack and threads destroy each other's frames; leave out the exit and
`pthread_join` never returns — both happened while this was being written.

A threaded module's `Imports` is `Clone + Send + 'static`, and its generated
host keeps its state behind `Arc<Mutex<_>>`: a copy per thread would be four
filesystems that agree about nothing.

A thread needs a stack of its own, and that is the host's job: the globals are
per instance, so `__stack_pointer` is the thread's own — but only if somebody
sets it. Measured on an Emscripten `-pthread` build, four threads left on the
module's initial stack pointer destroyed each other's frames completely (the
worst came back with **0 of its 64 fields intact**); with 64 KiB each, every
frame survived every round. `tests/emscripten.rs` asserts the second half,
because "a race must happen" is not something a test can demand.

The table is copied into each thread rather than shared, and that is not the
divergence it looks like: this decompiler has no `table.set`, `table.grow`,
`table.fill` or `table.copy`, and an opcode it does not model is refused by
name rather than dropped — so a module that mutated its table would not have
decompiled at all. None of the six captured modules contains one, and the VoIP
module's table is declared `9291 9291`, which cannot grow.

## Where it goes

- **Level 1 — structured.** Half in: `--level 1` turns the frame slots it can
  place into Rust bindings, opt-in and saying so. What is still to come is the
  other half — parameter roles read from how the code uses them, in the manner
  of `wa-wasm-oracle`'s `abi.rs`: dereferenced means pointer, and the access
  width says what it points at.
- **Level 2 — idiomatic.** Half in as well, and it is the half that is not
  speculative at all: `--level 2` names classes and their virtual methods from
  the C++ RTTI in the data segments, and the embind registrations are read
  whatever the level. Both are things the binary declares about itself. What is
  still to come is the part that must be inferred — structs from access
  patterns, and which `call_indirect` sites a vtable slot answers.
- **Recognising more library code.** `--signatures` names it and
  `--stub-recognised` now leaves the bodies out, so the size of the cut is
  exactly the catalogue's recall — ~91% across builds of one toolchain and a
  handful across emscripten versions. The mechanism is not the limit; the
  fingerprint is.

What remains in those is a guess about intent, and only worth attempting on top
of something already known to run — which is what level 0 is for, and why it
came first. What is already in was worth doing first for the opposite reason:
a name the module wrote down costs nothing to be right about.
