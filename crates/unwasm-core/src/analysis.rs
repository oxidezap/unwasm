//! What the module says about itself.
//!
//! Level 0 translates; this reads. Nothing here changes what the generated code
//! does — it changes what a person can tell from looking at it, which on a
//! minified module is the difference between `self.g0` and `the C stack
//! pointer`, and between `i32.const 211967` and the panic message it addresses.
//!
//! Every answer carries its evidence. A decompiler that says "this is the stack
//! pointer" without saying why has asked the reader to trust it, and the reader
//! has no way to check. So [`StackPointer`] records how it was found and how
//! many times the pattern appeared, and anything with no evidence comes back as
//! `None` rather than as the most likely index.

use crate::module::{ConstExpr, ExportKind, Module, Op, ValType};

/// How the C stack pointer was identified.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Evidence {
    /// The module exports it under the name the linker gives it. Conclusive,
    /// and present in unstripped builds.
    Exported,
    /// The name section names it. Just as conclusive as an export and rather
    /// more common in a debug build: the linker writes `__stack_pointer` into
    /// the name section whether or not it also exports the global, and a
    /// module that names it has said which global holds the C stack rather
    /// than left it to be read off how the code uses it.
    Named,
    /// Found by its use: the function prologue that reserves a frame.
    ///
    /// ```wat
    /// global.get $sp
    /// i32.const 32
    /// i32.sub
    /// global.set $sp     ;; and usually local.tee first
    /// ```
    Prologue {
        /// How many functions open with it. One is a coincidence; hundreds are
        /// a calling convention.
        functions: usize,
    },
}

/// The global the C stack lives in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StackPointer {
    /// Its index in the global index space.
    pub global: u32,
    /// Why we believe it.
    pub evidence: Evidence,
}

/// One slot of a function's stack frame: an offset that is read or written.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Slot {
    /// The widest access seen, in bytes. Usually the size of what lives there.
    pub width: u32,
    /// How many times it is read.
    pub reads: usize,
    /// How many times it is written.
    pub writes: usize,
    /// The width and type every access agreed on, if they all did.
    ///
    /// `None` once two accesses disagree — a four-byte store and a one-byte
    /// load at the same offset is a union, a narrowing, or two variables the
    /// compiler packed together, and none of the three is one variable. This is
    /// what decides whether the slot can become a single Rust binding.
    pub uniform: Option<(u32, ValType)>,
    /// Set once two accesses disagreed, so a third that happens to match the
    /// first cannot put [`Self::uniform`] back.
    pub mixed: bool,
    /// Whether any access reached this offset by arithmetic rather than by the
    /// base local and a static offset.
    ///
    /// `frame + 8` computed with an `i32.add` names the same byte as a memarg
    /// of 8, and the walk resolves both — but only the second is a shape the
    /// emitter recognises. A slot reached both ways would be a binding at one
    /// access and memory at the other, so level 1 refuses it.
    pub indirect: bool,
}

impl Slot {
    /// Records an access, and whether it still looks like one variable.
    fn observe(&mut self, width: u32, ty: ValType) {
        self.width = self.width.max(width);
        if self.mixed {
            return;
        }
        match self.uniform {
            None => self.uniform = Some((width, ty)),
            Some(seen) if seen == (width, ty) => {}
            Some(_) => {
                self.uniform = None;
                self.mixed = true;
            }
        }
    }
}

/// A function's stack frame, as far as its own code reveals it.
///
/// This is the shadow stack: the region a C compiler carves out of linear
/// memory for locals it cannot keep in wasm locals — anything whose address is
/// taken, anything larger than a scalar, anything spilled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    /// Bytes reserved by the prologue.
    pub size: i32,
    /// The wasm local holding the frame's base address.
    pub base_local: u32,
    /// The offsets used, and how.
    pub slots: std::collections::BTreeMap<i32, Slot>,
    /// Whether the base address goes somewhere this analysis cannot follow —
    /// passed to a call, stored into memory, copied to another local.
    ///
    /// This is the question that decides whether the frame could ever be
    /// modelled as variables rather than as memory, and it is answered
    /// conservatively: anything not understood sets it.
    pub escapes: bool,
    /// Whether the prologue writes the reserved address back to the stack
    /// pointer.
    ///
    /// A leaf function often does not: at `-O0` clang computes `sp - 32`, uses
    /// it, and never publishes it, because nothing else will allocate while it
    /// runs. A function that *does* publish is one that expects to call
    /// something — which is a fact about the function worth having.
    pub publishes: bool,
    /// Stores through an address computed from the frame at run time.
    ///
    /// `frame + i * 4` with `i` unknown: an array in the frame, indexed. The
    /// offset cannot be checked against the frame size, so this counts the
    /// writes that *could* leave it and that no static check will ever catch.
    /// Anyone hunting a corrupted address wants this list before the constant
    /// one — a constant store past the end is a bug a compiler would have to
    /// have emitted on purpose.
    pub computed_writes: usize,
    /// Whether the frame address was ever copied into another local.
    ///
    /// A copy is followed rather than given up on, so it is not an escape — but
    /// it does mean an access can reach a slot through a local other than the
    /// base. Anything that promotes a slot to a Rust binding has to see *every*
    /// access to it, so it asks for a frame nobody copied.
    pub copied: bool,
    /// How many instructions the prologue occupies.
    ///
    /// Where the frame address becomes valid, which is the earliest point
    /// anything can be initialised from it.
    pub prologue: usize,
}

impl Frame {
    /// The writes that land outside this frame, as `(offset, width)`.
    ///
    /// A store at `frame + 1168` in a function whose prologue reserved 1168
    /// bytes is writing into its *caller's* frame — an overrun of a local
    /// array, or an index one past the end. It is the shape of the bug that
    /// otherwise costs a day of bisection, and the analysis already has every
    /// number needed to spot it: the prologue said how big the frame is, and
    /// the walk recorded where each store went.
    ///
    /// Reads are not reported. Reading past the end is how a compiler spells
    /// "my caller left arguments up there" in some ABIs, and a list that
    /// includes it stops being short enough to act on.
    #[must_use]
    pub fn writes_outside(&self) -> Vec<(i32, u32)> {
        self.slots
            .iter()
            .filter(|(offset, slot)| {
                slot.writes > 0 && (**offset < 0 || **offset + slot.width as i32 > self.size)
            })
            .map(|(offset, slot)| (*offset, slot.width))
            .collect()
    }
}

/// An `invoke_*` import that can be generated instead of asked of the host.
///
/// Emscripten routes any call that might throw through one of these: the first
/// argument is a table index, the rest are the callee's own arguments. The
/// JavaScript glue implements it as
///
/// ```js
/// function invoke_vii(index, a, b) {
///   var sp = stackSave();
///   try { getWasmTableEntry(index)(a, b); }
///   catch (e) { stackRestore(sp); if (e !== e+0) throw e; _setThrew(1, 0); }
/// }
/// ```
///
/// Every part of that is available here: the table, the stack pointer, and the
/// module's own exported `setThrew`. So it is generated rather than left as one
/// more thing for a host to write — 125 of the VoIP module's 228 imports are
/// these, and none of them says anything about the module that the module does
/// not already say.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Invoke {
    /// The imported function's index.
    pub import: u32,
    /// The type of what it dispatches to: the import's signature without its
    /// leading table index.
    pub callee_type: u32,
}

/// What it takes to answer `__pthread_create_js` without inventing anything.
///
/// Emscripten's JavaScript spawns a worker, instantiates the module over the
/// same memory, and has the worker call `_emscripten_thread_init` and then the
/// start routine through the table. Every part of that is here — the memory is
/// shared, the table holds the routine, and `_emscripten_thread_init` is one
/// of the module's own exports — so it is generated rather than left as one
/// more thing to write.
///
/// The order matters and is the glue's own:
///
/// ```js
/// establishStackSpace(pthread_ptr);                       // the thread's stack
/// __emscripten_thread_init(pthread_ptr, 0, 0, /*can_block=*/1, 0, 0);
/// invokeEntryPoint(start_routine, arg);
/// ```
///
/// The stack comes first and does not come from `thread_init`: the guest
/// allocated it inside `pthread_create` and wrote it into its own pthread
/// struct, and the glue reads it back out from there. Those two offsets are
/// the one thing here that is a *layout* — Emscripten's own
/// `C_STRUCTS.pthread.stack` and `.stack_size`, 48 and 52 — and
/// [`Analysis::PTHREAD_STACK_OFFSETS`] is where they are written down, so a
/// build that moved them can be corrected in one place instead of debugged.
///
/// Getting the stack wrong is not subtle: measured on a real `-pthread` build,
/// four threads sharing one left a frame with 0 of its 64 fields intact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Spawn {
    /// The imported `__pthread_create_js`.
    pub import: u32,
    /// The exported `_emscripten_thread_init` the new thread calls.
    pub thread_init: u32,
    /// How many arguments that takes.
    pub thread_init_arity: usize,
    /// The exported `_emscripten_stack_set_limits`, which the thread calls
    /// with the stack it found in its own pthread struct.
    pub stack_set_limits: Option<u32>,
    /// The exported `_emscripten_thread_exit`, which the thread calls with
    /// what its start routine returned. Without it a `pthread_join` waits
    /// forever: nothing else tells the joining thread the thread is done.
    pub thread_exit: Option<u32>,
    /// The type of the start routine: `(i32) -> i32`, by index.
    pub entry_type: u32,
}

/// The `mmap` family, when the module supplies everything they need.
///
/// Emscripten's glue answers `__mmap_js` by allocating with the module's *own*
/// allocator and reading the file into it — so it reaches back into the
/// instance, which is what makes it something to generate rather than to ask a
/// host for. Same shape as the pthread glue: the parts are named, checked, and
/// if any is missing the import stays the host's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mmap {
    /// `env::_mmap_js`, if the module imports it.
    pub map: Option<u32>,
    /// `env::_munmap_js`.
    pub unmap: Option<u32>,
    /// `env::_msync_js`.
    pub sync: Option<u32>,
    /// The exported `emscripten_builtin_memalign`, which is where the mapping
    /// comes from. Without it there is nowhere to put a file.
    pub memalign: u32,
    /// The exported `free`, which releases the scratch an iovec needs and the
    /// mapping `munmap` gives back.
    pub free: u32,
    /// The imported `fd_pread`, which is how the bytes get in. A mapping
    /// invented without reading the file is a page of zeros presented as a
    /// file, which is worse than saying it is not written.
    pub pread: u32,
    /// The imported `fd_pwrite`, which is how `msync` gets them out.
    pub pwrite: Option<u32>,
}

/// Finds the `mmap` glue, if every part of it is there.
fn find_mmap(module: &Module) -> Option<Mmap> {
    let import = |field: &str, params: &[ValType], results: &[ValType]| -> Option<u32> {
        let at = module
            .func_imports
            .iter()
            .position(|import| import.field == field)? as u32;
        let ty = module
            .types
            .get(module.func_imports[at as usize].type_index as usize)?;
        (ty.params == params && ty.results == results).then_some(at)
    };
    use ValType::{I32, I64};

    // `__mmap_js(len, prot, flags, fd, offset, allocated, addr) -> errno`, and
    // the two that unwind it. A different shape is a different function, and
    // reading the wrong argument as a file descriptor is exactly the kind of
    // mistake a signature check exists to stop.
    let map = import("_mmap_js", &[I32, I32, I32, I32, I64, I32, I32], &[I32]);
    let unmap = import("_munmap_js", &[I32, I32, I32, I32, I32, I64], &[I32]);
    let sync = import("_msync_js", &[I32, I32, I32, I32, I32, I64], &[I32]);
    if map.is_none() && unmap.is_none() && sync.is_none() {
        return None;
    }

    let memalign = find_export(module, &["emscripten_builtin_memalign", "memalign"])?;
    let free = find_export(module, &["free"])?;
    if func_type_of(module, memalign)?.params != vec![I32, I32]
        || func_type_of(module, memalign)?.results != vec![I32]
        || func_type_of(module, free)?.params != vec![I32]
        || !func_type_of(module, free)?.results.is_empty()
    {
        return None;
    }
    let pread = import("fd_pread", &[I32, I32, I32, I64, I32], &[I32])?;
    let pwrite = import("fd_pwrite", &[I32, I32, I32, I64, I32], &[I32]);

    // Writing a mapping back needs `fd_pwrite`. Without it those two stay the
    // host's — claiming them and then emitting nothing would leave a call to a
    // method that does not exist, and the output has to compile.
    let (sync, unmap) = if pwrite.is_some() {
        (sync, unmap)
    } else {
        (None, None)
    };
    if map.is_none() && sync.is_none() && unmap.is_none() {
        return None;
    }

    Some(Mmap {
        map,
        unmap,
        sync,
        memalign,
        free,
        pread,
        pwrite,
    })
}

/// `__emscripten_init_main_thread_js`, which is the main thread's version of
/// what a worker does before it runs anything.
///
/// The glue answers it by calling the module's own `_emscripten_thread_init`
/// with `is_main` and `is_runtime` set — so it reaches back into the instance,
/// which is what makes it something to generate rather than to ask a host for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InitMainThread {
    /// The import.
    pub import: u32,
    /// The exported `_emscripten_thread_init` it calls.
    pub thread_init: u32,
    /// How many arguments that takes.
    pub thread_init_arity: usize,
}

/// Finds it, if the module has both halves.
fn find_init_main_thread(module: &Module) -> Option<InitMainThread> {
    let import = module.func_imports.iter().position(|import| {
        import.field == "__emscripten_init_main_thread_js"
            || import.field == "_emscripten_init_main_thread_js"
    })? as u32;
    let ty = module
        .types
        .get(module.func_imports[import as usize].type_index as usize)?;
    if ty.params != vec![ValType::I32] || !ty.results.is_empty() {
        return None;
    }
    let thread_init = find_export(module, &["_emscripten_thread_init"])?;
    let init_type = func_type_of(module, thread_init)?;
    if init_type.params.is_empty() || init_type.params.iter().any(|param| *param != ValType::I32) {
        return None;
    }
    Some(InitMainThread {
        import,
        thread_init,
        thread_init_arity: init_type.params.len(),
    })
}

/// Finds the pthread glue, if all of it is there.
fn find_spawn(module: &Module) -> Option<Spawn> {
    // A thread is another instance over the same memory. Without a shared one
    // there is nothing to join.
    if !module.memory.as_ref().is_some_and(|memory| memory.shared) {
        return None;
    }
    let import = module.func_imports.iter().position(|import| {
        import.field == "__pthread_create_js" || import.field == "_pthread_create_js"
    })? as u32;
    // `(thread, attr, entry, arg) -> errno`, which is the shape every version
    // that has this import uses. A different one is a different function.
    let ty = module
        .types
        .get(module.func_imports[import as usize].type_index as usize)?;
    if ty.params.len() != 4
        || ty.params.iter().any(|param| *param != ValType::I32)
        || ty.results != vec![ValType::I32]
    {
        return None;
    }

    let thread_init = find_export(module, &["_emscripten_thread_init"])?;
    let init_type = func_type_of(module, thread_init)?;
    if init_type.params.is_empty() || init_type.params.iter().any(|param| *param != ValType::I32) {
        return None;
    }

    // The start routine is `void *(*)(void *)`, which lowers to `(i32) -> i32`.
    let entry_type = module
        .types
        .iter()
        .position(|ty| ty.params == vec![ValType::I32] && ty.results == vec![ValType::I32])?
        as u32;

    Some(Spawn {
        import,
        thread_init,
        thread_init_arity: init_type.params.len(),
        stack_set_limits: find_export(
            module,
            &[
                "emscripten_stack_set_limits",
                "_emscripten_stack_set_limits",
            ],
        ),
        thread_exit: find_export(module, &["_emscripten_thread_exit"]),
        entry_type,
    })
}

/// The signature of a function, whether imported or defined.
fn func_type_of(module: &Module, func: u32) -> Option<crate::module::FuncType> {
    let index = type_index_of(module, func)?;
    module.types.get(index as usize).cloned()
}

/// Where a passive data segment ends up in memory.
///
/// A module built for threads places its data with `memory.init` rather than
/// with static offsets — the main thread copies it once, and the segments
/// themselves carry no address. The VoIP module's 125 segments are all like
/// this, which is why nothing was readable in it until the placements were
/// resolved: every string in the module is in a segment that says nothing
/// about where it lives.
///
/// The address is in the code, though, as three constants before the
/// `memory.init`. Anything less definite than that is not recorded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Placement {
    /// Where the copy lands.
    pub address: i32,
    /// Where in the segment it starts.
    pub offset: u32,
    /// How many bytes.
    pub length: u32,
}

/// A name guessed for a function from a string it references.
///
/// A stripped module has no name section and no mangled symbols — the VoIP
/// module has neither — but its data segments are full of messages that name
/// the code that logs them: `parse_xmpp_offer: invalid call-creator jid`. A
/// function referencing one is very probably that function.
///
/// "Very probably" is the whole point, and it is why this carries its evidence
/// and why the index stays in the emitted name. It is a hypothesis with a
/// reason attached, not a symbol table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DerivedName {
    /// The identifier taken out of the message.
    pub name: String,
    /// The message it came from, in full.
    pub evidence: String,
    /// Where the name came from, which is how much to trust it.
    pub source: NameSource,
    /// The names that lost, with why. A function built out of several inlined
    /// ones references all of their messages, so the runners-up are worth
    /// seeing — a reader who knows the code can tell at a glance when the
    /// wrong one won.
    pub rejected: Vec<String>,
}

/// How a name was arrived at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameSource {
    /// From `__assert_fail(expr, file, line, func)`, whose last argument is
    /// literally `__func__`. This is the compiler naming the function, not a
    /// guess about it, so it beats everything else.
    Assert,
    /// From a message the function logs.
    Message {
        /// How many times this function references it. A message it logs
        /// eleven times is about this function; one it logs once is often
        /// about a callee that failed.
        references: usize,
        /// Whether any other function references it too. Inlining spreads a
        /// message across every function it was inlined into, so this is
        /// weaker evidence than it looks.
        unique: bool,
    },
}

/// Something the module registers with embind at startup.
///
/// embind is how a C++ module tells JavaScript what it exposes: classes, their
/// methods, free functions, enums. The registration happens at run time, but
/// the arguments are constants in the code — including the pointer to the name
/// — so it can be read without running anything.
///
/// This is the one place a stripped module describes its own API. Everything
/// else here recovers what the compiler left behind; this recovers what the
/// author meant to publish.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Registration {
    /// Which `_embind_register_*` was called.
    pub kind: String,
    /// The name it registered, when the name is a constant this can resolve.
    pub name: Option<String>,
    /// The C++ signature, where the registration carries one.
    ///
    /// embind passes the types as a pointer to an array of type ids, and each
    /// id is the address of a `std::type_info` that some other registration
    /// names. Both halves are static, so `startVoipCall` comes back as taking
    /// a `std::string` rather than an `i32` — which is the only place in a
    /// stripped module where a type has a name at all.
    pub signature: Option<String>,
    /// For a method or a constructor, the class it belongs to.
    pub class: Option<String>,
}

/// Who calls whom.
///
/// A call site says which function it calls; nothing says who calls *this* one,
/// and that is the direction a reader wants — "what reaches this code" is the
/// first question about any function in a stripped binary. Reading it out of
/// the module is a scan; recovering it by grep over two million lines of output
/// is an afternoon.
#[derive(Debug, Clone, Default)]
pub struct CallGraph {
    /// For each function, the functions it calls directly.
    pub calls: std::collections::BTreeMap<u32, std::collections::BTreeSet<u32>>,
    /// For each function, the functions that call it directly.
    pub called_by: std::collections::BTreeMap<u32, std::collections::BTreeSet<u32>>,
    /// For each function, the signatures it calls through the table.
    ///
    /// An indirect call names a type, not a target. Which functions that could
    /// reach is the table's business — see [`Analysis::table`] — and keeping
    /// the two apart is the difference between "calls this" and "could call
    /// anything with this shape".
    pub calls_indirectly: std::collections::BTreeMap<u32, std::collections::BTreeSet<u32>>,
    /// For each function, how many call *sites* reach it.
    ///
    /// Not the same number as `called_by.len()`, and the difference is what
    /// makes it worth keeping: one caller that calls a function in a loop body
    /// forty times is one entry in `called_by` and forty sites. Instrumenting
    /// a shared function's body measures whichever site happened to run, so
    /// the count is the warning that a body patch will answer the wrong
    /// question.
    pub call_sites: std::collections::BTreeMap<u32, usize>,
}

impl CallGraph {
    /// The functions this one calls directly.
    #[must_use]
    pub fn calls_from(&self, func: u32) -> &std::collections::BTreeSet<u32> {
        static EMPTY: std::sync::LazyLock<std::collections::BTreeSet<u32>> =
            std::sync::LazyLock::new(Default::default);
        self.calls.get(&func).unwrap_or(&EMPTY)
    }

    /// How many call sites reach this function.
    #[must_use]
    pub fn sites_reaching(&self, func: u32) -> usize {
        self.call_sites.get(&func).copied().unwrap_or_default()
    }

    /// The functions that call this one directly.
    #[must_use]
    pub fn callers_of(&self, func: u32) -> &std::collections::BTreeSet<u32> {
        static EMPTY: std::sync::LazyLock<std::collections::BTreeSet<u32>> =
            std::sync::LazyLock::new(Default::default);
        self.called_by.get(&func).unwrap_or(&EMPTY)
    }
}

/// What could be read out of a module.
#[derive(Debug, Clone, Default)]
pub struct Analysis {
    /// The C stack pointer, if the module gave a reason to name one.
    pub stack_pointer: Option<StackPointer>,
    /// Frames, by function index. Absent for a function with no prologue.
    pub frames: std::collections::BTreeMap<u32, Frame>,
    /// The `invoke_*` trampolines that can be generated.
    pub invokes: Vec<Invoke>,
    /// The module's exported `setThrew`, which a trampoline needs to report a
    /// caught exception back to the guest.
    pub set_threw: Option<u32>,
    /// The pthread glue, when every part of it is present.
    pub spawn: Option<Spawn>,
    /// The main thread's own initialisation, when the module asks for it.
    pub init_main_thread: Option<InitMainThread>,
    /// The `mmap` glue, when the module supplies the allocator and the reads
    /// it needs.
    pub mmap: Option<Mmap>,
    /// Where each passive data segment is placed, by segment index, when the
    /// module places it at a constant address.
    pub placements: std::collections::BTreeMap<u32, Placement>,
    /// Names guessed from the strings a function references, by function index.
    /// Only for functions the module did not name itself.
    pub derived_names: std::collections::BTreeMap<u32, DerivedName>,
    /// What the module registers with embind, in the order it registers it.
    pub registrations: Vec<Registration>,
    /// Addresses that many functions reference and that hold no text: the
    /// module's shared state. Address to the number of functions using it.
    ///
    /// A pointer to a context struct appears as a bare number in dozens of
    /// functions, and every one of them reads as noise until you notice it is
    /// the same number. Counting is what makes it noticeable.
    pub hot_addresses: std::collections::BTreeMap<i32, usize>,
    /// Who calls whom.
    pub call_graph: CallGraph,
    /// What the function table holds: slot to function index.
    ///
    /// `call_indirect` takes a *table* index, not a function index, so reading
    /// a call site means knowing which function is in which slot — and
    /// installing a callback means knowing which slot to put it in.
    pub table: std::collections::BTreeMap<u32, u32>,
}

/// A fingerprint of a function's body, for recognising the same code in
/// another module.
///
/// Constants, call targets and memory offsets are left out: those move between
/// builds while the shape of the code does not. What is left is exact — the
/// instruction sequence at the level of "a 4-byte load" rather than "a 4-byte
/// load at offset 12".
///
/// Measured: two programs built by the same toolchain fingerprint 91% of their
/// shared library functions identically. Across *different* emscripten
/// versions it is a handful — the library is the same source, but the compiler
/// that emitted it is not. A coarser fingerprint was tried to close that gap
/// and was abandoned: matched against a module sharing no code at all, it
/// produced seven matches out of ten, so the gap it closed was noise.
///
/// So a match here is strong evidence and a miss is no evidence. This names
/// functions; it never marks one as unrecognised.
#[must_use]
pub fn fingerprint(module: &Module, func: &crate::module::Func) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut write = |text: &str| {
        for byte in text.bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
    };
    // The signature first. Two bodies of the same shape that take different
    // arguments are not the same function, and leaving the type out let a
    // catalogue name one of them after the other.
    if let Some(ty) = module.types.get(func.type_index as usize) {
        for param in &ty.params {
            write(&format!("p{param:?}"));
        }
        for result in &ty.results {
            write(&format!("r{result:?}"));
        }
    }
    for op in &func.body {
        match op {
            // The things that differ between builds of the same source.
            Op::I32Const(_) => write("c"),
            Op::I64Const(_) => write("C"),
            Op::F32Const(_) | Op::F64Const(_) => write("F"),
            Op::Call(_) => write("k"),
            Op::CallIndirect { .. } => write("K"),
            Op::Load { kind, .. } => write(&format!("l{kind:?}")),
            Op::Store { kind, .. } => write(&format!("s{kind:?}")),
            Op::GlobalGet(_) | Op::GlobalSet(_) => write("g"),
            Op::MemoryInit(_) | Op::DataDrop(_) => write("d"),
            other => write(&format!("{other:?}")),
        }
    }
    hash
}

/// The shortest function worth fingerprinting.
///
/// Below this, distinct functions collide: a three-instruction accessor has the
/// same shape as every other three-instruction accessor, and naming one after
/// another would be worse than leaving both unnamed.
pub const FINGERPRINT_FLOOR: usize = 20;

/// Reads a module for the things worth naming.
#[must_use]
pub fn analyse(module: &Module) -> Analysis {
    let stack_pointer = find_stack_pointer(module);
    let set_threw = find_export(module, &["setThrew", "_setThrew"]);
    // A trampoline needs somewhere to report the exception it caught and a
    // stack pointer to restore. Without either, generating one would be
    // inventing behaviour rather than reproducing the glue's.
    let invokes = if set_threw.is_some() && stack_pointer.is_some() {
        find_invokes(module)
    } else {
        Vec::new()
    };
    let placements = find_placements(module);
    Analysis {
        frames: stack_pointer
            .map(|sp| find_frames(module, sp))
            .unwrap_or_default(),
        stack_pointer,
        invokes,
        set_threw,
        placements: placements.clone(),
        derived_names: derive_names(module, &placements),
        registrations: find_registrations(module, &placements),
        spawn: find_spawn(module),
        init_main_thread: find_init_main_thread(module),
        mmap: find_mmap(module),
        table: read_table(module),
        hot_addresses: find_hot_addresses(module, &placements),
        call_graph: read_call_graph(module),
    }
}

/// The frame slots that could become Rust bindings instead of memory.
///
/// This is the question level 1 asks, and the answer is deliberately hard to
/// earn. A slot is promotable only when *every* access to the frame is one this
/// can see and place:
///
/// - the address never escapes and was never copied into another local, so
///   every access goes through the base local and nothing else can reach it;
/// - no store goes through an address computed from the frame at run time —
///   an indexed array write could land on any slot;
/// - the function has no `memory.fill`, `memory.copy` or `memory.init`, whose
///   destination is a value rather than an offset and could cover the frame;
/// - the slot's accesses all used one width and one type, so it is one
///   variable rather than a union or a packed pair;
/// - and the slot lies inside the frame and overlaps no other.
///
/// What it does *not* prove is that no unrelated pointer aliases the region.
/// Nothing in a wasm module says a store cannot land below the stack pointer,
/// and no compiler emits one — which is an assumption, and is why this is a
/// level that says it is guessing rather than the default.
#[must_use]
pub fn promotable_slots(
    frame: &Frame,
    body: &[Op],
) -> std::collections::BTreeMap<i32, (u32, ValType)> {
    let mut promoted = std::collections::BTreeMap::new();
    if frame.escapes || frame.computed_writes > 0 || frame.copied {
        return promoted;
    }
    if body
        .iter()
        .any(|op| matches!(op, Op::MemoryFill | Op::MemoryCopy | Op::MemoryInit(_)))
    {
        return promoted;
    }
    for (offset, slot) in &frame.slots {
        // Reached by arithmetic somewhere, so the emitter cannot see every
        // access to it: `frame + 8` computed with an `i32.add` names the same
        // byte a memarg of 8 does, and only the second is a shape it matches.
        if slot.indirect {
            continue;
        }
        let Some((width, ty)) = slot.uniform else {
            continue;
        };
        // Inside the frame it reserved. A slot past the end is a write into the
        // caller's frame, which `writes_outside` reports and which nothing here
        // is going to quietly turn into a variable.
        if *offset < 0 || offset.saturating_add(width as i32) > frame.size {
            continue;
        }
        // And overlapping nothing. Two slots sharing a byte are not two
        // variables, whatever their widths agreed on separately.
        let overlaps = frame.slots.iter().any(|(other, candidate)| {
            other != offset
                && *other < offset + width as i32
                && offset < &(other + candidate.width.max(1) as i32)
        });
        if overlaps {
            continue;
        }
        promoted.insert(*offset, (width, ty));
    }
    promoted
}

/// The most deeply nested function in the module, and how deep it goes.
///
/// wasm's nesting becomes Rust's, one labelled block per `block`, `loop` or
/// `if` — and rustc parses that recursively on an 8 MiB stack. A module whose
/// `br_table` dispatch nests two thousand blocks makes rustc overflow it and
/// die with `SIGSEGV`, which reads as a compiler bug rather than as a module
/// that needs a bigger stack. Knowing the number before compiling is what turns
/// that into `RUST_MIN_STACK`.
///
/// Returns `None` for a module with no functions.
#[must_use]
pub fn deepest_nesting(module: &Module) -> Option<(u32, usize)> {
    let import_count = module.func_imports.len() as u32;
    let mut deepest: Option<(u32, usize)> = None;
    for (at, func) in module.funcs.iter().enumerate() {
        let mut depth = 0usize;
        let mut most = 0usize;
        for op in &func.body {
            match op {
                Op::Block(_) | Op::Loop(_) | Op::If(_) => {
                    depth += 1;
                    most = most.max(depth);
                }
                Op::End => depth = depth.saturating_sub(1),
                _ => {}
            }
        }
        if deepest.is_none_or(|(_, seen)| most > seen) {
            deepest = Some((import_count + at as u32, most));
        }
    }
    deepest
}

/// How deep a function can nest before rustc's default stack is not enough.
///
/// Not a guess: rustc parses 2466 nested blocks — the VoIP module's worst
/// function — by overflowing an 8 MiB stack, and compiles the same file with
/// `RUST_MIN_STACK` raised. The threshold is set well below that, because the
/// cost of saying so unnecessarily is a line of output and the cost of not
/// saying so is a segfault nobody can place.
pub const NESTING_RUSTC_HANDLES: usize = 300;

/// Reads the call graph out of the function bodies.
fn read_call_graph(module: &Module) -> CallGraph {
    let import_count = module.func_imports.len() as u32;
    let mut graph = CallGraph::default();
    for (at, func) in module.funcs.iter().enumerate() {
        let caller = import_count + at as u32;
        for op in &func.body {
            match op {
                Op::Call(callee) => {
                    graph.calls.entry(caller).or_default().insert(*callee);
                    graph.called_by.entry(*callee).or_default().insert(caller);
                    *graph.call_sites.entry(*callee).or_default() += 1;
                }
                Op::CallIndirect { type_index } => {
                    graph
                        .calls_indirectly
                        .entry(caller)
                        .or_default()
                        .insert(*type_index);
                }
                _ => {}
            }
        }
    }
    graph
}

impl Analysis {
    /// Where a pthread struct keeps its stack: `(stack, stack_size)`, in bytes
    /// from the start of the struct.
    ///
    /// Emscripten's own `C_STRUCTS.pthread`, which is what its `establishStackSpace`
    /// reads. It is a layout rather than something the wasm says, so it is
    /// version-specific — and it is the number to check first if a threaded
    /// module misbehaves, since a wrong one puts every thread on a stack that
    /// is not its own.
    pub const PTHREAD_STACK_OFFSETS: (u64, u64) = (48, 52);

    /// Every function reachable from `start`, including through the table.
    ///
    /// A direct call names its callee; an indirect one names only a type, so
    /// everything in the table with that type joins the set. That is the
    /// module's own claim about what could run — `call_indirect` really can
    /// reach any slot of the right shape — and it is why this is a reachable
    /// set rather than a call tree.
    ///
    /// What it is for is not curiosity. Decompiling the VoIP module gives 2.3
    /// million lines of Rust that take twenty minutes to compile, and a reader
    /// chasing one path does not need the other 81% of it to be code rather
    /// than a stub.
    #[must_use]
    pub fn reachable_from(&self, module: &Module, start: u32) -> std::collections::BTreeSet<u32> {
        self.reached(module, start, true)
    }

    /// Every function reachable from `start` by *direct* calls only.
    ///
    /// A smaller set, and an incomplete one: an indirect call from inside it
    /// can land anywhere in the table. It is worth having anyway, because the
    /// complete set is not a reduction — on the VoIP module 98% of the module
    /// is reachable once `call_indirect` is followed, since a common signature
    /// reaches thousands of slots — and this is 19%.
    ///
    /// What makes it usable rather than wrong is what happens at the edge: a
    /// function left out keeps its name and its signature and its body is
    /// `unimplemented!()`, so a run that reaches one stops and says which
    /// function it wanted. That is a worklist, not a silent wrong answer.
    #[must_use]
    pub fn directly_reachable_from(
        &self,
        module: &Module,
        start: u32,
    ) -> std::collections::BTreeSet<u32> {
        self.reached(module, start, false)
    }

    fn reached(
        &self,
        module: &Module,
        start: u32,
        through_the_table: bool,
    ) -> std::collections::BTreeSet<u32> {
        // Which functions the table holds, by type, so an indirect call can be
        // resolved to the set it could reach.
        let mut by_type: std::collections::BTreeMap<u32, Vec<u32>> = Default::default();
        for func in self.table.values() {
            if let Some(ty) = type_index_of(module, *func) {
                by_type.entry(ty).or_default().push(*func);
            }
        }

        let mut reached = std::collections::BTreeSet::new();
        let mut pending = vec![start];
        while let Some(func) = pending.pop() {
            if !reached.insert(func) {
                continue;
            }
            pending.extend(self.call_graph.calls_from(func).iter().copied());
            if !through_the_table {
                continue;
            }
            for ty in self
                .call_graph
                .calls_indirectly
                .get(&func)
                .into_iter()
                .flatten()
            {
                pending.extend(by_type.get(ty).into_iter().flatten().copied());
            }
        }
        reached
    }
}

/// The type index of a function, whether imported or defined.
#[must_use]
pub fn type_index_of(module: &Module, func: u32) -> Option<u32> {
    let imports = module.func_imports.len() as u32;
    if func < imports {
        module.func_imports.get(func as usize).map(|i| i.type_index)
    } else {
        module
            .funcs
            .get((func - imports) as usize)
            .map(|f| f.type_index)
    }
}

/// How many functions must share an address before it is worth pointing out.
///
/// Low enough to catch a context pointer, high enough that a constant two
/// functions happen to share is not called shared state.
pub const HOT_ADDRESS_FUNCTIONS: usize = 8;

/// Finds the addresses that many functions reference and that are not text.
///
/// "Address" is decided by the module's own layout rather than by size: a
/// constant counts only if it falls inside the span its data segments occupy.
/// Without that, `2147483647` and `4096` come out as the module's most widely
/// shared addresses, and they are arithmetic.
fn find_hot_addresses(
    module: &Module,
    placements: &std::collections::BTreeMap<u32, Placement>,
) -> std::collections::BTreeMap<i32, usize> {
    let Some((low, high)) = static_data_span(module, placements) else {
        return Default::default();
    };

    let mut counts: std::collections::BTreeMap<i32, usize> = Default::default();
    for func in &module.funcs {
        let mut seen: std::collections::BTreeSet<i32> = Default::default();
        for (position, op) in func.body.iter().enumerate() {
            let Op::I32Const(value) = op else { continue };
            // A `memory.init` destination is a placement, not a use of what is
            // there.
            if *value < low
                || *value > high
                || matches!(func.body.get(position + 3), Some(Op::MemoryInit(_)))
            {
                continue;
            }
            seen.insert(*value);
        }
        for value in seen {
            *counts.entry(value).or_default() += 1;
        }
    }
    counts.retain(|address, functions| {
        *functions >= HOT_ADDRESS_FUNCTIONS
            // Text has a better annotation already.
            && placed_text(module, placements, *address, None).is_none()
    });
    counts
}

/// The span of memory the module's data segments occupy, if it has any.
fn static_data_span(
    module: &Module,
    placements: &std::collections::BTreeMap<u32, Placement>,
) -> Option<(i32, i32)> {
    let mut low = i32::MAX;
    let mut high = i32::MIN;
    for (index, segment) in module.datas.iter().enumerate() {
        let base = match segment.offset {
            Some(ConstExpr::I32(base)) => base,
            Some(_) => continue,
            None => placements.get(&(index as u32))?.address,
        };
        low = low.min(base);
        high = high.max(base + segment.bytes.len() as i32);
    }
    (low <= high).then_some((low, high))
}

/// Reads the function table's contents from the element segments.
/// How long a run of null slots may be before it is the end of the table.
///
/// A pure virtual function is a zero in the middle of a vtable; the zeros after
/// the last method are the next object. Four, because a run longer than that is
/// more often the end than a run of pure virtuals — and because the *only*
/// thing that makes a run part of the table is a live table index directly
/// after it. The next vtable's `{0, type_info}` header does not qualify: the
/// `type_info` is an address in the hundreds of thousands, not a table index.
const NULL_RUN: usize = 4;

/// Reads a table of function pointers: each slot as a table index.
///
/// `None` is a slot holding 0. The read stops at the first word that is neither
/// a live table index nor part of a null run a live index follows.
fn read_pointer_table(
    image: &DataImage<'_>,
    table: &std::collections::BTreeMap<u32, u32>,
    address: i32,
    highest: u32,
    cap: usize,
) -> Vec<Option<u32>> {
    let live = |word: i32| -> Option<u32> {
        (word > 0 && word as u32 <= highest)
            .then(|| table.get(&(word as u32)).copied())
            .flatten()
    };
    let mut slots = Vec::new();
    let mut cursor = address;
    while slots.len() < cap {
        let Some(word) = image.read32(cursor) else {
            break;
        };
        if let Some(func) = live(word) {
            slots.push(Some(func));
            cursor += 4;
            continue;
        }
        if word != 0 {
            break;
        }
        // A run of zeros counts only if a real method comes directly after it.
        let mut run = 1;
        while run <= NULL_RUN && image.read32(cursor + 4 * run as i32) == Some(0) {
            run += 1;
        }
        let after = image.read32(cursor + 4 * run as i32);
        if run > NULL_RUN || after.and_then(live).is_none() {
            break;
        }
        for _ in 0..run {
            slots.push(None);
        }
        cursor += 4 * run as i32;
    }
    slots.truncate(cap);
    slots
}

/// Reads a table of function pointers at an address, for `unwasm vtable`.
///
/// Each slot is a table index; `None` is a slot holding 0, which a
/// `call_indirect` cannot survive. `cap` bounds the read; without one it stops
/// where the table stops looking like one — see [`read_pointer_table`].
#[must_use]
pub fn pointer_table(
    image: &DataImage<'_>,
    table: &std::collections::BTreeMap<u32, u32>,
    address: i32,
    cap: Option<usize>,
) -> Vec<Option<u32>> {
    let highest = table.keys().max().copied().unwrap_or(0);
    match cap {
        Some(cap) => {
            // An explicit count reads that many words whatever they hold: the
            // caller asked, and refusing would hide the bytes they asked about.
            let mut slots = Vec::with_capacity(cap);
            for slot in 0..cap {
                let Some(word) = image.read32(address.wrapping_add((slot * 4) as i32)) else {
                    break;
                };
                slots.push(
                    (word > 0)
                        .then(|| table.get(&(word as u32)).copied())
                        .flatten(),
                );
            }
            slots
        }
        None => read_pointer_table(image, table, address, highest, usize::MAX),
    }
}

fn read_table(module: &Module) -> std::collections::BTreeMap<u32, u32> {
    let mut table = std::collections::BTreeMap::new();
    for segment in &module.elems {
        // A passive or declared segment is not in the table until something
        // puts it there, and nothing here models `table.init`.
        let Some(ConstExpr::I32(base)) = segment.offset else {
            continue;
        };
        for (offset, func) in segment.funcs.iter().enumerate() {
            table.insert(base as u32 + offset as u32, *func);
        }
    }
    table
}

/// Which argument of each `_embind_register_*` holds the count and the pointer
/// to its array of type ids, for the registrations that carry a signature.
///
/// From embind's own declarations: `_embind_register_function(name, argCount,
/// argTypes, ..)`, `_embind_register_class_function(classType, methodName,
/// argCount, argTypes, ..)`.
const EMBIND_TYPES_ARGUMENT: &[(&str, usize, usize)] = &[
    ("_embind_register_function", 1, 2),
    ("_embind_register_class_function", 2, 3),
    ("_embind_register_class_class_function", 2, 3),
    ("_embind_register_class_constructor", 1, 2),
];

/// Which argument holds the type id a registration is *defining*, for the ones
/// that define a type. Always the first.
const EMBIND_DEFINES_TYPE: &[&str] = &[
    "_embind_register_void",
    "_embind_register_bool",
    "_embind_register_integer",
    "_embind_register_bigint",
    "_embind_register_float",
    "_embind_register_std_string",
    "_embind_register_std_wstring",
    "_embind_register_emval",
    "_embind_register_memory_view",
    "_embind_register_value_array",
    "_embind_register_value_object",
    "_embind_register_class",
    "_embind_register_enum",
];

/// Which argument of each `_embind_register_*` holds the name.
///
/// From embind's own signatures. A registration not listed here is still
/// reported — that it happened is worth knowing — just without a name.
/// Which argument of each `_embind_register_*` call carries the name.
///
/// Public because the host generator answers the same calls at run time and
/// must agree with what the static reader claims about them.
pub const EMBIND_NAME_ARGUMENT: &[(&str, usize)] = &[
    ("_embind_register_void", 1),
    ("_embind_register_bool", 1),
    ("_embind_register_integer", 1),
    ("_embind_register_bigint", 1),
    ("_embind_register_float", 1),
    ("_embind_register_std_string", 1),
    ("_embind_register_std_wstring", 2),
    ("_embind_register_emval", 1),
    ("_embind_register_memory_view", 2),
    ("_embind_register_function", 0),
    ("_embind_register_value_array", 1),
    ("_embind_register_value_object", 1),
    ("_embind_register_class", 10),
    ("_embind_register_class_function", 1),
    ("_embind_register_class_property", 1),
    ("_embind_register_class_class_function", 1),
    ("_embind_register_enum", 1),
    ("_embind_register_enum_value", 1),
    ("_embind_register_constant", 0),
];

/// Reads what the module registers with embind.
///
/// The arguments are taken only when every one of them is a constant sitting
/// immediately before the call, which is what Emscripten emits — a registration
/// assembled at run time is reported without a name rather than with a guess at
/// one.
fn find_registrations(
    module: &Module,
    placements: &std::collections::BTreeMap<u32, Placement>,
) -> Vec<Registration> {
    // Which imports are embind registrations, and where their name sits.
    let mut registrars: std::collections::BTreeMap<u32, (&str, Option<usize>)> = Default::default();
    for (index, import) in module.func_imports.iter().enumerate() {
        if !import.field.starts_with("_embind_register") {
            continue;
        }
        let at = EMBIND_NAME_ARGUMENT
            .iter()
            .find(|(name, _)| *name == import.field)
            .map(|(_, at)| *at);
        registrars.insert(index as u32, (import.field.as_str(), at));
    }
    if registrars.is_empty() {
        return Vec::new();
    }

    let mut registrations: Vec<(Registration, Option<Vec<i32>>)> = Vec::new();
    for func in &module.funcs {
        for (position, op) in func.body.iter().enumerate() {
            let Op::Call(callee) = op else { continue };
            let Some((field, name_at)) = registrars.get(callee) else {
                continue;
            };
            let arity = module
                .func_type(*callee)
                .map_or(0, |signature| signature.params.len());

            // The arguments, if they are all constants immediately before the
            // call. Anything else and the name is not knowable from here.
            let arguments: Option<Vec<i32>> = position.checked_sub(arity).and_then(|first| {
                func.body[first..position]
                    .iter()
                    .map(|op| match op {
                        Op::I32Const(value) => Some(*value),
                        _ => None,
                    })
                    .collect()
            });

            let name = match (name_at, &arguments) {
                (Some(at), Some(arguments)) => arguments
                    .get(*at)
                    .and_then(|address| placed_name(module, placements, *address)),
                _ => None,
            };
            registrations.push((
                Registration {
                    kind: (*field).to_string(),
                    name,
                    signature: None,
                    class: None,
                },
                arguments,
            ));
        }
    }

    resolve_types(module, placements, registrations)
}

/// Fills in the signatures, once every type has been seen.
///
/// Two passes rather than one, because a type can be registered after the
/// function that takes it — the order is the order the module's initialisers
/// happen to run in, and nothing says the types come first.
fn resolve_types(
    module: &Module,
    placements: &std::collections::BTreeMap<u32, Placement>,
    registrations: Vec<(Registration, Option<Vec<i32>>)>,
) -> Vec<Registration> {
    // Type id to the name it was registered under. The id is the address of
    // the type's `std::type_info`, and the first argument of whichever
    // registration defines it.
    let mut type_names: std::collections::BTreeMap<i32, String> = Default::default();
    for (registration, arguments) in &registrations {
        if !EMBIND_DEFINES_TYPE.contains(&registration.kind.as_str()) {
            continue;
        }
        let (Some(arguments), Some(name)) = (arguments, &registration.name) else {
            continue;
        };
        if let Some(id) = arguments.first() {
            type_names.insert(*id, name.clone());
        }
        // A class registers three ids for itself: the class, a pointer to it,
        // and a const pointer. A method returning the second is returning the
        // class, and saying `type@972492` instead would be reporting an
        // address where there is a name.
        if registration.kind == "_embind_register_class" {
            if let Some(id) = arguments.get(1) {
                type_names.insert(*id, format!("{name}*"));
            }
            if let Some(id) = arguments.get(2) {
                type_names.insert(*id, format!("const {name}*"));
            }
        }
    }

    let mut out = Vec::new();
    let mut last_class: Option<String> = None;
    for (mut registration, arguments) in registrations {
        if registration.kind == "_embind_register_class" {
            last_class.clone_from(&registration.name);
        } else if registration.kind.starts_with("_embind_register_class") {
            // A method belongs to the class registered before it, which is how
            // embind's own generated code is ordered.
            registration.class.clone_from(&last_class);
        }

        if let Some((_, count_at, types_at)) = EMBIND_TYPES_ARGUMENT
            .iter()
            .find(|(kind, _, _)| *kind == registration.kind)
            && let Some(arguments) = &arguments
            && let (Some(count), Some(types)) = (arguments.get(*count_at), arguments.get(*types_at))
            && *count > 0
            && *count < 64
        {
            let named = |id: Option<i32>| match id {
                Some(id) => type_names
                    .get(&id)
                    .cloned()
                    .unwrap_or_else(|| format!("type@{id}")),
                None => "?".to_string(),
            };
            // The first entry is the return type; the rest are the arguments.
            let ids: Vec<Option<i32>> = (0..*count)
                .map(|at| static_i32(module, placements, types + at * 4))
                .collect();
            let parameters: Vec<String> = ids[1..].iter().map(|id| named(*id)).collect();
            registration.signature = Some(if registration.kind.ends_with("constructor") {
                // A constructor is named after its class and returns it; both
                // halves would otherwise print as a placeholder and an address.
                format!(
                    "{}({})",
                    registration
                        .class
                        .clone()
                        .unwrap_or_else(|| "…".to_string()),
                    parameters.join(", ")
                )
            } else {
                format!(
                    "{} {}({})",
                    named(ids[0]),
                    registration.name.clone().unwrap_or_else(|| "…".to_string()),
                    parameters.join(", ")
                )
            });
        }
        out.push(registration);
    }
    out
}

/// Reads four bytes of static memory.
///
/// The same lookup [`placed_text`] does, for a number rather than text: an
/// embind type id array lives in a data segment, and reading it is how a
/// registration's types are recovered.
#[must_use]
pub fn static_i32(
    module: &Module,
    placements: &std::collections::BTreeMap<u32, Placement>,
    address: i32,
) -> Option<i32> {
    for (index, segment) in module.datas.iter().enumerate() {
        let (base, within) = match segment.offset {
            Some(ConstExpr::I32(base)) => (base, 0usize),
            Some(_) => continue,
            None => match placements.get(&(index as u32)) {
                Some(placement) => (placement.address, placement.offset as usize),
                None => continue,
            },
        };
        if address < base {
            continue;
        }
        let at = (address - base) as usize + within;
        if at + 4 > segment.bytes.len() {
            continue;
        }
        return Some(i32::from_le_bytes([
            segment.bytes[at],
            segment.bytes[at + 1],
            segment.bytes[at + 2],
            segment.bytes[at + 3],
        ]));
    }
    None
}

/// Guesses names for unnamed functions from what they say about themselves.
///
/// Two sources, in order of how much they are worth:
///
/// 1. **`__assert_fail(expr, file, line, func)`.** Its last argument is
///    `__func__` — the compiler writing the function's name into the binary.
///    That is not a guess and beats anything else.
/// 2. **The messages it logs.** A function that logs
///    `parse_xmpp_offer: invalid jid` is very probably `parse_xmpp_offer`.
///
/// For the second, how *often* a message is referenced matters more than
/// whether it is unique. Inlining is why: a function built out of several
/// inlined ones references all of their strings, and the one message that
/// belongs to nobody else is often the one about a callee that failed —
/// `..._create_participant: wa_vid_quality_manager_create error: %d` names the
/// callee, not the caller. A message the function logs eleven times is about
/// that function; one it logs once may be about anything.
fn derive_names(
    module: &Module,
    placements: &std::collections::BTreeMap<u32, Placement>,
) -> std::collections::BTreeMap<u32, DerivedName> {
    let import_count = module.func_imports.len() as u32;
    let assert_fail = module
        .func_imports
        .iter()
        .position(|import| import.field.contains("assert_fail"))
        .map(|index| index as u32);

    // What each function references, and how often.
    let mut per_function: Vec<std::collections::BTreeMap<String, usize>> =
        vec![Default::default(); module.funcs.len()];
    let mut asserted: std::collections::BTreeMap<u32, String> = Default::default();

    for (at, func) in module.funcs.iter().enumerate() {
        let index = import_count + at as u32;
        for (position, op) in func.body.iter().enumerate() {
            // The name an `__assert_fail` carries, which settles the question.
            if let (Some(assert_fail), Op::Call(callee)) = (assert_fail, op)
                && *callee == assert_fail
                && let Some(Op::I32Const(address)) =
                    position.checked_sub(1).and_then(|at| func.body.get(at))
                && let Some(text) = placed_text(module, placements, *address, None)
                && is_c_identifier(&text)
            {
                asserted.entry(index).or_insert(text);
            }

            let Op::I32Const(address) = op else { continue };
            // The address a `memory.init` copies *to* is not a reference to the
            // text — it is the placement itself. Counting it would make every
            // string at the start of a segment look like it belonged to the
            // initialiser as well as to its real user.
            if matches!(func.body.get(position + 3), Some(Op::MemoryInit(_))) {
                continue;
            }
            let length = match func.body.get(position + 1) {
                Some(Op::I32Const(length)) if *length > 0 => Some(*length as u32),
                _ => None,
            };
            let text = length
                .and_then(|length| placed_text(module, placements, *address, Some(length)))
                .or_else(|| placed_text(module, placements, *address, None));
            let Some(text) = text else { continue };
            if identifier_in(&text).is_some() {
                *per_function[at].entry(text).or_default() += 1;
            }
        }
    }

    // How many functions reference each message at all.
    let mut owners: std::collections::BTreeMap<&str, usize> = Default::default();
    for messages in &per_function {
        for text in messages.keys() {
            *owners.entry(text.as_str()).or_default() += 1;
        }
    }

    let mut names: std::collections::BTreeMap<u32, DerivedName> = Default::default();
    for (at, messages) in per_function.iter().enumerate() {
        let index = import_count + at as u32;
        if module.func_name(index).is_some() {
            continue;
        }

        if let Some(name) = asserted.get(&index) {
            names.insert(
                index,
                DerivedName {
                    name: name.clone(),
                    evidence: format!("__assert_fail(.., \"{name}\")"),
                    source: NameSource::Assert,
                    rejected: best_messages(messages, &owners, 3)
                        .into_iter()
                        .filter(|candidate| candidate.0 != *name)
                        .map(|candidate| candidate.0)
                        .collect(),
                },
            );
            continue;
        }

        let mut ranked = best_messages(messages, &owners, 4).into_iter();
        let Some((name, text, references, unique)) = ranked.next() else {
            continue;
        };
        names.insert(
            index,
            DerivedName {
                name,
                evidence: text,
                source: NameSource::Message { references, unique },
                rejected: ranked.map(|candidate| candidate.0).collect(),
            },
        );
    }
    names
}

/// The best candidate names a function's messages offer, strongest first.
///
/// Ordered by how often the function references the message, then by whether
/// anything else references it, then by how specific the identifier is. The
/// first of those is the one that matters: it is what tells a message about
/// this function from a message about something it called.
fn best_messages(
    messages: &std::collections::BTreeMap<String, usize>,
    owners: &std::collections::BTreeMap<&str, usize>,
    most: usize,
) -> Vec<(String, String, usize, bool)> {
    let mut candidates: Vec<(String, String, usize, bool)> = messages
        .iter()
        .filter_map(|(text, references)| {
            let name = identifier_in(text)?;
            let unique = owners.get(text.as_str()).copied().unwrap_or(0) <= 1;
            // Referenced once, by more than one function: there is nothing here
            // to tell those functions apart, and naming both after it would
            // give two functions the same name for no reason.
            if *references == 1 && !unique {
                return None;
            }
            Some((name.to_string(), text.clone(), *references, unique))
        })
        .collect();
    candidates.sort_by(|left, right| {
        right
            .2
            .cmp(&left.2)
            .then(right.3.cmp(&left.3))
            .then(right.0.len().cmp(&left.0.len()))
            .then(left.0.cmp(&right.0))
    });
    // Several messages can yield the same identifier — a bare
    // `wa_call_group_create_participant` and one with a suffix after it. As
    // candidates they are the same answer twice.
    let mut seen: std::collections::BTreeSet<String> = Default::default();
    candidates.retain(|candidate| seen.insert(candidate.0.clone()));
    candidates.truncate(most);
    candidates
}

/// Whether text is a plain C identifier, as `__func__` always is.
fn is_c_identifier(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= 120
        && text.starts_with(|ch: char| ch.is_ascii_alphabetic() || ch == '_')
        && text
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}

/// The function name a log message starts with, if it starts with one.
fn identifier_in(text: &str) -> Option<&str> {
    let first = text.split([':', ' ', '(', ',']).next()?;
    // A Rust module path is not a function name. `call_control::ffi` is the
    // crate saying where it is, and taking `call_control` from it names a
    // function after its module — which, once messages are ranked by how often
    // they appear, is the mistake that wins, because a path appears everywhere
    // in its own module.
    if text[first.len()..].starts_with("::") {
        return None;
    }
    let long_enough = first.len() >= 6 && first.len() <= 60;
    let shaped_like_an_identifier = first.contains('_')
        && first
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_')
        && first.starts_with(|ch: char| ch.is_ascii_lowercase());
    (long_enough && shaped_like_an_identifier).then_some(first)
}

/// Finds where the module places its passive data segments.
///
/// Only the plainest form counts: three constants and a `memory.init`, which is
/// what `__wasm_init_memory` emits for the main thread. A module can also place
/// a segment at a computed address — per-thread storage does, at `base + 32` —
/// and where the address is computed, this records nothing rather than guess at
/// one of them.
///
/// A segment placed at two different constant addresses is dropped for the same
/// reason: both are true, so neither can be used to resolve an address back to
/// text.
fn find_placements(module: &Module) -> std::collections::BTreeMap<u32, Placement> {
    let mut placements: std::collections::BTreeMap<u32, Placement> = Default::default();
    let mut ambiguous: std::collections::BTreeSet<u32> = Default::default();

    for func in &module.funcs {
        for (at, op) in func.body.iter().enumerate() {
            let Op::MemoryInit(segment) = op else {
                continue;
            };
            // The three operands, immediately before and all constant.
            let (
                Some(Op::I32Const(address)),
                Some(Op::I32Const(offset)),
                Some(Op::I32Const(length)),
            ) = (
                at.checked_sub(3).and_then(|index| func.body.get(index)),
                at.checked_sub(2).and_then(|index| func.body.get(index)),
                at.checked_sub(1).and_then(|index| func.body.get(index)),
            )
            else {
                continue;
            };
            if *offset < 0 || *length <= 0 {
                continue;
            }
            let placement = Placement {
                address: *address,
                offset: *offset as u32,
                length: *length as u32,
            };
            match placements.get(segment) {
                Some(existing) if *existing != placement => {
                    ambiguous.insert(*segment);
                }
                _ => {
                    placements.insert(*segment, placement);
                }
            }
        }
    }
    for segment in ambiguous {
        placements.remove(&segment);
    }
    placements
}

fn find_export(module: &Module, names: &[&str]) -> Option<u32> {
    module
        .exports
        .iter()
        .find(|export| export.kind == ExportKind::Func && names.contains(&export.name.as_str()))
        .map(|export| export.index)
}

/// Finds the `invoke_*` imports whose dispatch target the module already has a
/// type for.
///
/// The name is the signal, and it is a reliable one: import names are the
/// module's interface to its host, so a minifier cannot touch them — the VoIP
/// module keeps `invoke_vii` while every function name is gone. The signature
/// is then checked rather than trusted: the first parameter must be the table
/// index, and the rest must match a type the module declares, or there is no
/// dispatcher to call.
fn find_invokes(module: &Module) -> Vec<Invoke> {
    let mut invokes = Vec::new();
    for (index, import) in module.func_imports.iter().enumerate() {
        if !import.field.starts_with("invoke_") {
            continue;
        }
        let Some(ty) = module.types.get(import.type_index as usize) else {
            continue;
        };
        let Some((first, rest)) = ty.params.split_first() else {
            continue;
        };
        if *first != ValType::I32 {
            continue;
        }
        // The callee's signature is the import's without the table index.
        let callee_type = module
            .types
            .iter()
            .position(|candidate| candidate.params == rest && candidate.results == ty.results);
        if let Some(callee_type) = callee_type {
            invokes.push(Invoke {
                import: index as u32,
                callee_type: callee_type as u32,
            });
        }
    }
    invokes
}

fn find_frames(
    module: &Module,
    stack_pointer: StackPointer,
) -> std::collections::BTreeMap<u32, Frame> {
    let import_count = module.func_imports.len() as u32;
    let mut frames = std::collections::BTreeMap::new();
    for (at, func) in module.funcs.iter().enumerate() {
        if let Some(frame) = read_frame(module, &func.body, stack_pointer.global) {
            frames.insert(import_count + at as u32, frame);
        }
    }
    frames
}

/// What a value on the abstract stack is, as far as this analysis cares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tracked {
    /// The frame base plus a constant.
    Frame(i32),
    /// The frame base plus something that is not a constant — an index into an
    /// array that lives in the frame, computed at run time.
    ///
    /// The offset is unknown, so nothing can be said about whether it stays
    /// inside the frame. Keeping it apart from `Other` is what lets a store
    /// through it be counted: that is the write that overruns an array, and it
    /// is invisible to a check that only knows constant offsets.
    Derived,
    /// A constant.
    Const(i32),
    /// Anything else.
    Other,
}

/// Reads one function's frame.
///
/// The walk is a small abstract interpretation: it tracks which stack entries
/// are the frame address and watches what happens to them. It is deliberately
/// easy to defeat — anything it cannot model exactly sets `escapes` and stops
/// claiming to know. That direction is the safe one: a frame wrongly marked as
/// escaping costs a missed annotation, while one wrongly marked as contained
/// would be a false statement about the code.
fn read_frame(module: &Module, body: &[Op], stack_pointer: u32) -> Option<Frame> {
    let prologue = read_prologue(body, stack_pointer)?;
    let (size, base_local) = (prologue.size, prologue.base_local);
    let mut frame = Frame {
        size,
        base_local,
        slots: std::collections::BTreeMap::new(),
        escapes: false,
        publishes: prologue.publishes,
        computed_writes: 0,
        copied: false,
        prologue: prologue.length,
    };

    let mut stack: Vec<Tracked> = Vec::new();
    // What each local holds, for the locals this walk has watched being
    // written. At `-O0` an older clang routes every intermediate value through
    // one of these — the size of the frame, and the frame address itself in the
    // epilogue — so a walk that only follows the operand stack sees a function
    // whose frame escapes immediately and can say nothing about it.
    let mut locals: std::collections::BTreeMap<u32, Tracked> = std::collections::BTreeMap::new();
    for op in &body[prologue.length..] {
        // Control flow ends what this walk can follow. If the frame address is
        // live across it, the analysis gives up rather than losing track of
        // where it went.
        if is_control_flow(op) {
            if stack.iter().any(|value| matches!(value, Tracked::Frame(_))) {
                frame.escapes = true;
                return Some(frame);
            }
            // A local written on one path and read on another holds whatever
            // the path that ran put there, and this walk follows one path. So
            // what a local holds is forgotten at the boundary — and a local
            // holding the frame address when it is forgotten has gone somewhere
            // this can no longer follow, which is what `escapes` says.
            if locals
                .values()
                .any(|value| matches!(value, Tracked::Frame(_) | Tracked::Derived))
            {
                frame.escapes = true;
            }
            locals.clear();
            stack.clear();
            continue;
        }

        match op {
            Op::LocalGet(index) if *index == base_local => stack.push(Tracked::Frame(0)),
            Op::LocalGet(index) => {
                stack.push(locals.get(index).copied().unwrap_or(Tracked::Other));
            }
            Op::GlobalGet(_) => stack.push(Tracked::Other),
            Op::I32Const(value) => stack.push(Tracked::Const(*value)),
            Op::I64Const(_) | Op::F32Const(_) | Op::F64Const(_) => stack.push(Tracked::Other),

            Op::Num(num) if num.name() == "I32Add" => {
                let right = stack.pop().unwrap_or(Tracked::Other);
                let left = stack.pop().unwrap_or(Tracked::Other);
                // `frame + k` stays the frame; anything else is arithmetic on
                // an address we can no longer place.
                let sum = match (left, right) {
                    (Tracked::Frame(at), Tracked::Const(k))
                    | (Tracked::Const(k), Tracked::Frame(at)) => Tracked::Frame(at + k),
                    (Tracked::Const(a), Tracked::Const(b)) => Tracked::Const(a.wrapping_add(b)),
                    (left, right) => {
                        if matches!(left, Tracked::Frame(_) | Tracked::Derived)
                            || matches!(right, Tracked::Frame(_) | Tracked::Derived)
                        {
                            // Still the frame, at an offset nobody knows.
                            frame.escapes = true;
                            Tracked::Derived
                        } else {
                            Tracked::Other
                        }
                    }
                };
                stack.push(sum);
            }

            Op::Load { kind, mem } => {
                let address = stack.pop().unwrap_or(Tracked::Other);
                if let Tracked::Frame(at) = address {
                    let offset = at + mem.offset as i32;
                    let slot = frame.slots.entry(offset).or_default();
                    slot.observe(width_of_load(*kind), kind.result());
                    slot.indirect |= at != 0;
                    slot.reads += 1;
                }
                stack.push(Tracked::Other);
            }
            Op::Store { kind, mem } => {
                let value = stack.pop().unwrap_or(Tracked::Other);
                let address = stack.pop().unwrap_or(Tracked::Other);
                // Storing the frame address *into* memory is the plainest
                // escape there is.
                if matches!(value, Tracked::Frame(_)) {
                    frame.escapes = true;
                }
                if let Tracked::Frame(at) = address {
                    let offset = at + mem.offset as i32;
                    let slot = frame.slots.entry(offset).or_default();
                    slot.observe(width_of_store(*kind), type_of_store(*kind));
                    slot.indirect |= at != 0;
                    slot.writes += 1;
                }
                if matches!(address, Tracked::Derived) {
                    frame.computed_writes += 1;
                }
            }

            // The epilogue: `frame + size` written back to the stack pointer.
            Op::GlobalSet(index) if *index == stack_pointer => {
                let value = stack.pop().unwrap_or(Tracked::Other);
                if !matches!(value, Tracked::Frame(at) if at == size) {
                    frame.escapes = true;
                }
            }

            Op::LocalSet(index) | Op::LocalTee(index) => {
                let value = stack.pop().unwrap_or(Tracked::Other);
                // The base being reassigned: the local this frame is named
                // after no longer holds it, and every offset read after this
                // point would be measured from the wrong address.
                if *index == base_local {
                    frame.escapes = true;
                } else {
                    // Anything else is followed rather than given up on. A copy
                    // of the frame address is still the frame address, and the
                    // epilogue of an unoptimised build is exactly that.
                    // Only a copy that could still reach a slot counts. The
                    // epilogue copies `frame + size` on its way back to the
                    // stack pointer, and a memarg offset is never negative, so
                    // that address cannot name anything inside the frame — and
                    // treating it as a copy would refuse level 1 on every
                    // function an unoptimised clang built.
                    if matches!(value, Tracked::Frame(at) if at < size) {
                        frame.copied = true;
                    }
                    locals.insert(*index, value);
                }
                if matches!(op, Op::LocalTee(_)) {
                    stack.push(value);
                }
            }

            Op::Drop => {
                stack.pop();
            }

            Op::Call(index) => {
                // An unknown callee has an unknown arity, and the walk cannot
                // keep its footing past it.
                let Some(signature) = module.func_type(*index) else {
                    frame.escapes = true;
                    return Some(frame);
                };
                let (takes, makes) = (signature.params.len(), signature.results.len());
                consume(&mut frame, &mut stack, takes);
                stack.extend(std::iter::repeat_n(Tracked::Other, makes));
            }

            // Everything else: pop what it takes, push what it makes, and treat
            // a frame address reaching it as an escape.
            other => {
                let Some((takes, makes)) = arity_of(module, other) else {
                    frame.escapes = true;
                    return Some(frame);
                };
                consume(&mut frame, &mut stack, takes);
                stack.extend(std::iter::repeat_n(Tracked::Other, makes));
            }
        }
    }
    Some(frame)
}

/// Pops `count` values, marking the frame as escaping if any of them was the
/// frame address.
fn consume(frame: &mut Frame, stack: &mut Vec<Tracked>, count: usize) {
    for _ in 0..count {
        if matches!(stack.pop(), Some(Tracked::Frame(_))) {
            frame.escapes = true;
        }
    }
}

fn is_control_flow(op: &Op) -> bool {
    matches!(
        op,
        Op::Block(_)
            | Op::Loop(_)
            | Op::If(_)
            | Op::Else
            | Op::End
            | Op::Br(_)
            | Op::BrIf(_)
            | Op::BrTable { .. }
            | Op::Return
            | Op::Unreachable
    )
}

/// How many values an instruction takes and makes. `None` for anything this
/// analysis does not model, which makes the caller give up.
fn arity_of(module: &Module, op: &Op) -> Option<(usize, usize)> {
    Some(match op {
        Op::Nop | Op::DataDrop(_) => (0, 0),
        Op::Num(num) => (num.operands().len(), 1),
        Op::Select => (3, 1),
        Op::MemorySize => (0, 1),
        Op::MemoryGrow => (1, 1),
        Op::MemoryCopy | Op::MemoryFill | Op::MemoryInit(_) => (3, 0),
        Op::CallIndirect { type_index } => {
            let ty = module.types.get(*type_index as usize)?;
            (ty.params.len() + 1, ty.results.len())
        }
        _ => return None,
    })
}

fn width_of_load(kind: crate::module::LoadKind) -> u32 {
    use crate::module::LoadKind as L;
    match kind {
        L::I32Load8S | L::I32Load8U | L::I64Load8S | L::I64Load8U => 1,
        L::I32Load16S | L::I32Load16U | L::I64Load16S | L::I64Load16U => 2,
        L::I32 | L::F32 | L::I64Load32S | L::I64Load32U => 4,
        L::I64 | L::F64 => 8,
    }
}

/// The value type a store writes, which is the type of what lives there.
fn type_of_store(kind: crate::module::StoreKind) -> ValType {
    use crate::module::StoreKind as S;
    match kind {
        S::I32 | S::I32Store8 | S::I32Store16 => ValType::I32,
        S::I64 | S::I64Store8 | S::I64Store16 | S::I64Store32 => ValType::I64,
        S::F32 => ValType::F32,
        S::F64 => ValType::F64,
    }
}

fn width_of_store(kind: crate::module::StoreKind) -> u32 {
    use crate::module::StoreKind as S;
    match kind {
        S::I32Store8 | S::I64Store8 => 1,
        S::I32Store16 | S::I64Store16 => 2,
        S::I32 | S::F32 | S::I64Store32 => 4,
        S::I64 | S::F64 => 8,
    }
}

/// What a matched prologue says.
struct Prologue {
    size: i32,
    base_local: u32,
    /// Instructions the prologue occupies.
    length: usize,
    publishes: bool,
}

/// Matches the prologue against the stack pointer this module was told to use.
fn read_prologue(body: &[Op], stack_pointer: u32) -> Option<Prologue> {
    let (global, prologue) = match_prologue(body)?;
    (global == stack_pointer).then_some(prologue)
}

/// Matches the prologue: the stack pointer, less a constant, kept in a local.
///
/// Four spellings turn up in practice, and the last two were surprises:
///
/// ```wat
/// global.get $sp  i32.const 32  i32.sub  local.tee $base  global.set $sp
/// global.get $sp  i32.const 32  i32.sub  local.set $base  local.get $base  global.set $sp
/// global.get $sp  i32.const 32  i32.sub  local.set $base   ;; a leaf function at -O0
/// global.get $sp  local.set $a  i32.const 32  local.set $b ;; every value through a local
/// local.get $a    local.get $b  i32.sub       local.set $base
/// ```
///
/// The third never writes the stack pointer back. clang emits it for a function
/// that calls nothing: the space below the pointer is nobody else's while it
/// runs, so reserving it formally would be wasted work. Requiring the
/// `global.set` — which this did at first — misses every one of them.
///
/// The fourth is what clang 18 emits at `-O0`, where every intermediate value
/// goes through its own local and the subtraction reads two of them rather than
/// the operand stack. clang 22 folds it; the two compilers describe the same
/// frame. Missing this spelling is how the analysis reported that a module
/// built by an older clang had no frames at all — a false statement about the
/// code, made from the compiler's spelling rather than from the module's.
fn match_prologue(body: &[Op]) -> Option<(u32, Prologue)> {
    let Op::GlobalGet(global) = body.first()? else {
        return None;
    };
    // The folded spellings read the stack pointer and the size straight off the
    // operand stack; the unfolded one parks both in locals first.
    let (size, rest) = match (body.get(1)?, body.get(2)?, body.get(3)?) {
        (Op::I32Const(size), Op::Num(sub), _) if sub.name() == "I32Sub" => (*size, 3),
        (Op::LocalSet(pointer), Op::I32Const(size), Op::LocalSet(reserved)) => {
            if !matches!(body.get(4)?, Op::LocalGet(index) if index == pointer) {
                return None;
            }
            if !matches!(body.get(5)?, Op::LocalGet(index) if index == reserved) {
                return None;
            }
            if !matches!(body.get(6)?, Op::Num(sub) if sub.name() == "I32Sub") {
                return None;
            }
            (*size, 7)
        }
        _ => return None,
    };
    if size <= 0 {
        return None;
    }

    // However it got here, the reserved address is now on the stack, and what
    // keeps it decides both which local names the frame and whether the
    // reservation is published.
    let (base_local, kept) = match body.get(rest)? {
        Op::LocalTee(base) | Op::LocalSet(base) => (*base, matches!(body[rest], Op::LocalTee(_))),
        _ => return None,
    };
    let (length, publishes) = match (kept, body.get(rest + 1), body.get(rest + 2)) {
        // `local.tee $base; global.set $sp`
        (true, Some(Op::GlobalSet(sp)), _) if sp == global => (rest + 2, true),
        // `local.set $base; local.get $base; global.set $sp`
        (false, Some(Op::LocalGet(again)), Some(Op::GlobalSet(sp)))
            if again == &base_local && sp == global =>
        {
            (rest + 3, true)
        }
        _ => (rest + 1, false),
    };
    Some((
        *global,
        Prologue {
            size,
            base_local,
            length,
            publishes,
        },
    ))
}

/// The names a linker gives the stack pointer when it keeps names at all.
const STACK_POINTER_NAMES: &[&str] = &["__stack_pointer", "_stack_pointer", "stackPointer"];

fn find_stack_pointer(module: &Module) -> Option<StackPointer> {
    // A name settles it without any guessing — but only for a global that is
    // there. An index past the end names a field the output does not declare,
    // and the generated Rust would not compile, which is the one thing it must
    // always do. An imported global cannot shift the numbering here: this
    // decompiler refuses those by name rather than modelling them.
    let declared = |index: u32| module.globals.get(index as usize).map(|_| index);

    // An exported name is the module's own declaration.
    for export in &module.exports {
        if export.kind == ExportKind::Global
            && STACK_POINTER_NAMES.contains(&export.name.as_str())
            && let Some(global) = declared(export.index)
        {
            return Some(StackPointer {
                global,
                evidence: Evidence::Exported,
            });
        }
    }

    // Failing that, the name section. It survives `--export-all` not reaching
    // a mutable global, which is how a debug build can name the stack pointer
    // in every disassembly and still not export it.
    for (index, name) in &module.global_names {
        if STACK_POINTER_NAMES.contains(&name.as_str())
            && let Some(global) = declared(*index)
        {
            return Some(StackPointer {
                global,
                evidence: Evidence::Named,
            });
        }
    }

    // Otherwise, count prologues. A minified module keeps no names, but it
    // still has to reserve stack frames, and only one global is used that way.
    //
    // Two counts, because they are not equally good evidence. A prologue that
    // writes the reservation back says the global is shared with everything the
    // function calls, which is what a stack pointer is for, and the shape alone
    // is enough. A leaf's prologue only takes the space, and `global.get;
    // i32.const; i32.sub; local.set` is also just arithmetic on a global — so
    // that one is counted only when the function goes on to address memory
    // through the reserved address, which is what makes it a frame rather than
    // a number.
    //
    // The published ones decide it whenever there are any; the bare
    // reservations answer only for a build where nothing writes it back at all,
    // which is what an older clang emits for a module whose functions are all
    // leaves.
    let mut published = vec![0usize; module.globals.len()];
    let mut unpublished: Vec<(u32, &[Op])> = Vec::new();
    for func in &module.funcs {
        let Some((global, prologue)) = match_prologue(&func.body) else {
            continue;
        };
        if prologue.publishes {
            if let Some(count) = published.get_mut(global as usize) {
                *count += 1;
            }
        } else {
            unpublished.push((global, &func.body));
        }
    }
    // The second walk only happens when the first found nothing. Reading every
    // leaf's frame to fill a tally that is about to be thrown away is a whole
    // extra pass over 14733 functions on the module where it matters most, and
    // an Emscripten build always has published prologues.
    let counts = if published.iter().any(|count| *count > 0) {
        published
    } else {
        let mut addressed = vec![0usize; module.globals.len()];
        for (global, body) in unpublished {
            if read_frame(module, body, global).is_some_and(|frame| !frame.slots.is_empty())
                && let Some(count) = addressed.get_mut(global as usize)
            {
                *count += 1;
            }
        }
        addressed
    };

    let (global, functions) = counts
        .iter()
        .enumerate()
        .max_by_key(|(_, count)| **count)
        .map(|(index, count)| (index as u32, *count))?;

    // One function reserving a frame proves nothing: a module with a single
    // arithmetic helper can match by accident. Two is the smallest number that
    // is a convention rather than a coincidence.
    if functions < 2 {
        return None;
    }
    // A stack pointer is written to. An immutable global that happens to be
    // read in the same shape is something else.
    if !module.globals.get(global as usize)?.mutable {
        return None;
    }
    Some(StackPointer {
        global,
        evidence: Evidence::Prologue { functions },
    })
}

// ---- what a C++ module declares about its own classes ----

/// A C++ class the module declares, and what it declares about it.
///
/// This is not inference from how the code behaves. Itanium's ABI puts a
/// `type_info` object in the data segments for every polymorphic class and a
/// vtable beside it, and both are *written down*: the class's name is a string
/// the compiler emitted, and the vtable's entries are the table slots its
/// virtual calls land on. Recovering them is reading a declaration, not
/// guessing at one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Class {
    /// Where the `type_info` object sits.
    pub type_info: i32,
    /// The name exactly as the module wrote it, mangled.
    ///
    /// Kept beside the readable form because it is the evidence: a demangling
    /// this could not do in full is still checkable against the bytes.
    pub mangled: String,
    /// The readable form, as far as it could be read. The mangled string when
    /// it could not be read at all.
    pub name: String,
    /// The last component of the name, without template arguments — what a
    /// function derived from this class is named after.
    pub short: String,
    /// The `type_info` of the class this one derives from, when the module
    /// writes one down.
    ///
    /// `None` covers both "no base" and "more than one": Itanium spells single
    /// inheritance as a third word and everything else as a variable-length
    /// record this does not read, so an absent base is *unstated*, not stated
    /// to be absent.
    pub base: Option<i32>,
    /// Where the vtable sits, when one points at this `type_info`.
    pub vtable: Option<i32>,
    /// The vtable's slots, in order.
    ///
    /// `None` is a slot holding 0 — a pure virtual function. It is kept rather
    /// than skipped for two reasons: the slot numbers after it stay right, and
    /// a `call_indirect` reaching one takes table index 0, mismatches its
    /// signature and traps. A vtable read that stopped at the first zero
    /// under-reported both.
    pub methods: Vec<Option<u32>>,
}

/// What the class recovery rests on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClassEvidence {
    /// How many distinct `__class_type_info` vtables the classes share.
    ///
    /// Itanium gives every `type_info` a vtable pointer, and there are only a
    /// handful of them in a program — one per *kind* of type_info. So a pointer
    /// hundreds of `type_info` objects agree on is that; one a single candidate
    /// has is two words that happened to look like a pair.
    pub kinds: usize,
    /// How many classes were confirmed that way.
    pub classes: usize,
    /// How many of them a vtable points at.
    pub with_vtables: usize,
    /// How many of them the count alone missed, and a confirmed class named as
    /// its base.
    pub by_base: usize,
}

/// How many `type_info` candidates have to share a vtable pointer before it is
/// one.
///
/// Measured rather than picked. The floor was swept over the whole corpus: at 2
/// two of the three C modules — which declare no classes at all — each report a
/// kind and two classes, and both are accidents; at 3 all three report none,
/// while the C++ modules and the tiny emscripten fixture are unchanged. Raising
/// it further only loses real ones: at 4 `JgwtTQVeWPm` drops three classes and
/// one of its five kinds. So 3 is the lowest value that admits no accident,
/// which is the side to err on — a wrong class name is worse than none.
const TYPE_INFO_KIND_FLOOR: usize = 3;

/// The classes a module declares, and what they rest on.
///
/// Not part of [`analyse`]: it reads every four-byte-aligned word of the data
/// segments, which on a 10 MiB module is worth doing when asked and not on
/// every run.
#[must_use]
pub fn classes(
    module: &Module,
    placements: &std::collections::BTreeMap<u32, Placement>,
) -> (Vec<Class>, ClassEvidence) {
    let image = DataImage::of(module, placements);
    let table = read_table(module);

    // Every `{something, name}` pair whose second word points at a mangled
    // type name. Most are real; the rest are two words that happened to line up.
    let mut candidates: Vec<(i32, i32, String)> = Vec::new();
    for placed in &image.segments {
        let (base, bytes) = (&placed.base, &placed.bytes);
        let mut at = base.next_multiple_of(4);
        while (at as u64) + 8 <= u64::from(*base) + bytes.len() as u64 {
            if let (Some(kind), Some(name_at)) =
                (image.read32(at as i32), image.read32(at as i32 + 4))
                && kind != 0
                && let Some(name) = image.cstring(name_at)
                && is_mangled_type(&name)
            {
                candidates.push((at as i32, kind, name));
            }
            at += 4;
        }
    }

    // The counted evidence: a vtable pointer only a few candidates share is not
    // `__class_type_info`, it is a coincidence.
    let mut kinds: std::collections::BTreeMap<i32, usize> = Default::default();
    for (_, kind, _) in &candidates {
        *kinds.entry(*kind).or_default() += 1;
    }
    kinds.retain(|kind, count| {
        *count >= TYPE_INFO_KIND_FLOOR && image.holds(*kind) && image.holds(*kind + 4)
    });

    let by_address: std::collections::BTreeMap<i32, (i32, String)> = candidates
        .iter()
        .map(|(at, kind, mangled)| (*at, (*kind, mangled.clone())))
        .collect();

    let mut classes: std::collections::BTreeMap<i32, Class> = Default::default();
    let mut confirmed_kind: std::collections::BTreeMap<i32, i32> = Default::default();
    for (at, kind, mangled) in candidates {
        if !kinds.contains_key(&kind) {
            continue;
        }
        confirmed_kind.insert(at, kind);
        let (name, short) = demangle_type(&mangled);
        classes.insert(
            at,
            Class {
                type_info: at,
                mangled,
                name,
                short,
                base: None,
                vtable: None,
                methods: Vec::new(),
            },
        );
    }

    // Itanium gives a singly-inherited class a third word: its base's
    // `type_info`. Which of the confirmed kinds that is, is counted rather than
    // assumed — the single-inheritance kind has members whose third word points
    // at another `type_info`, and a kind of two-word objects does not.
    //
    // The one reading that has to be excluded is a pointer into the object
    // itself: a three-word `type_info` occupies `[at, at + 12)`, so a "base"
    // at one of those addresses would be the object overlapping itself. That
    // is a coincidence rather than a base, and what says so is the object's
    // own bounds rather than a guess about how the segments are laid out.
    let base_of = |at: i32| -> Option<i32> {
        let base = image.read32(at + 8)?;
        (by_address.contains_key(&base) && !(at..at + 12).contains(&base)).then_some(base)
    };
    let single_inheritance: std::collections::BTreeSet<i32> = kinds
        .keys()
        .copied()
        .filter(|kind| {
            confirmed_kind
                .iter()
                .filter(|(at, member)| *member == kind && base_of(**at).is_some())
                .count()
                >= TYPE_INFO_KIND_FLOOR
        })
        .collect();

    // Which is also the one way a class the count missed still gets named: a
    // base named by a confirmed derived class is written down by the module, not
    // inferred from it. One hop only — the admitted class's own kind was never
    // confirmed, so its third word is not read as a base.
    let mut adopt: Vec<(i32, i32)> = Vec::new();
    for (at, kind) in &confirmed_kind {
        if !single_inheritance.contains(kind) {
            continue;
        }
        if let Some(base) = base_of(*at) {
            adopt.push((*at, base));
        }
    }
    let mut by_base = 0usize;
    for (at, base) in adopt {
        if let Some(class) = classes.get_mut(&at) {
            class.base = Some(base);
        }
        if classes.contains_key(&base) {
            continue;
        }
        let (_, mangled) = &by_address[&base];
        let (name, short) = demangle_type(mangled);
        classes.insert(
            base,
            Class {
                type_info: base,
                mangled: mangled.clone(),
                name,
                short,
                base: None,
                vtable: None,
                methods: Vec::new(),
            },
        );
        by_base += 1;
    }

    // And the vtables: `{offset-to-top, type_info*, slot, slot, …}`, which is
    // Itanium's layout. Three independent things have to agree — a zero, a
    // `type_info` already confirmed, and a run of live table slots — which is
    // what makes a vtable recognisable at all.
    let highest = table.keys().max().copied().unwrap_or(0);
    for placed in &image.segments {
        let (base, bytes) = (&placed.base, &placed.bytes);
        let mut at = base.next_multiple_of(4);
        while (at as u64) + 8 <= u64::from(*base) + bytes.len() as u64 {
            let here = at as i32;
            at += 4;
            if image.read32(here) != Some(0) {
                continue;
            }
            let Some(info) = image.read32(here + 4) else {
                continue;
            };
            if !classes.contains_key(&info) {
                continue;
            }
            let methods = read_pointer_table(&image, &table, here + 8, highest, usize::MAX);
            if !methods.iter().any(Option::is_some) {
                continue;
            }
            // The first vtable wins. A class has one; a second match at another
            // address is a construction-vtable or an accident, and picking
            // between them is not something the bytes decide.
            let class = classes.get_mut(&info).expect("just checked");
            if class.vtable.is_none() {
                class.vtable = Some(here);
                class.methods = methods;
            }
        }
    }

    let classes: Vec<Class> = classes.into_values().collect();
    let evidence = ClassEvidence {
        kinds: kinds.len(),
        classes: classes.len(),
        with_vtables: classes
            .iter()
            .filter(|class| class.vtable.is_some())
            .count(),
        by_base,
    };
    (classes, evidence)
}

/// Whether a string is the shape `_ZTS` holds for a class type.
///
/// A leading digit is a source name and a leading `N` a nested one; those two
/// are the class types, which are the ones a vtable belongs to. Anything else —
/// a builtin, a pointer, a function type — is not something to name a class
/// after.
fn is_mangled_type(name: &str) -> bool {
    name.len() >= 3
        && name
            .as_bytes()
            .first()
            .is_some_and(|byte| byte.is_ascii_digit() || *byte == b'N')
        && name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '$' || ch == '.')
}

/// Reads an Itanium type name into something a person can read, and says so
/// only as far as it got.
///
/// Returns `(readable, short)` — the whole path, and its last component without
/// template arguments, which is what a function derived from the class is
/// named after. When the name cannot be read at all, both are the mangled
/// string: **a wrong name is worse than a mangled one**, and this is the same
/// rule the fingerprint catalogue follows.
///
/// What it reads is the part that carries the meaning: nested names, source
/// names, and `St` for `std`. What it deliberately does *not* read is the
/// inside of a template argument list, because that needs Itanium's
/// substitution table — `NS_9allocatorIS1_EE` refers back to components by
/// number — and a substitution resolved wrongly is a name that says something
/// the module did not. So `I…E` comes out as `<…>`: the class is named, and the
/// instantiation is visibly elided rather than invented.
fn demangle_type(mangled: &str) -> (String, String) {
    let unreadable = || (mangled.to_string(), mangled.to_string());
    let bytes = mangled.as_bytes();
    let mut at = 0usize;

    // A bare source name: `20WasmShimErrorHandler`.
    let source_name = |at: &mut usize| -> Option<String> {
        let start = *at;
        while bytes.get(*at).is_some_and(u8::is_ascii_digit) {
            *at += 1;
        }
        if *at == start {
            return None;
        }
        let length: usize = mangled[start..*at].parse().ok()?;
        let end = at.checked_add(length)?;
        let name = mangled.get(*at..end)?;
        *at = end;
        Some(name.to_string())
    };

    // `I … E`, skipped as a whole: what is inside needs the substitution table,
    // and finding the end is counting rather than parsing.
    //
    // Counting bytes is not enough, though. A source name's *text* can hold any
    // letter — `12_GLOBAL__N_1` is the anonymous namespace, and it carries an
    // `N` — so a scan that counts letters wherever it finds them goes out of
    // step and swallows the `E` that closes the name around the list. Ten names
    // in the VoIP module read as demangled that way, correctly by luck. So a
    // source name is skipped by its length, and only the five characters that
    // really open a scope are counted.
    let skip_template = |at: &mut usize| -> Option<()> {
        let mut depth = 0usize;
        loop {
            let byte = *bytes.get(*at)?;
            if byte.is_ascii_digit() {
                let start = *at;
                while bytes.get(*at).is_some_and(u8::is_ascii_digit) {
                    *at += 1;
                }
                let length: usize = mangled[start..*at].parse().ok()?;
                *at = at.checked_add(length)?;
                if *at > bytes.len() {
                    return None;
                }
                continue;
            }
            // A substitution is `S <seq-id> _`, and its sequence id is base 36 —
            // so it holds letters that would otherwise read as openers, and
            // digits that would otherwise read as a source name's length.
            if byte == b'S' {
                *at += 1;
                let id = *at;
                while bytes
                    .get(*at)
                    .is_some_and(|byte| byte.is_ascii_digit() || byte.is_ascii_uppercase())
                {
                    *at += 1;
                }
                if bytes.get(*at) == Some(&b'_') {
                    *at += 1;
                } else if *at == id {
                    // `St`, `Sa`, `Ss` — the fixed abbreviations, two characters
                    // and no underscore. `St3__2` is one of them followed by a
                    // source name, and reading the `3` as an id loses both.
                    *at += 1;
                }
                continue;
            }
            match byte {
                // `I` a template list, `N` a nested name, `J` an argument pack,
                // `Z` a local name, `L` a literal, `F` a function type — each
                // closed by `E`.
                b'I' | b'N' | b'J' | b'Z' | b'L' | b'F' => depth += 1,
                b'E' => {
                    depth = depth.checked_sub(1)?;
                    if depth == 0 {
                        *at += 1;
                        return Some(());
                    }
                }
                _ => {}
            }
            *at += 1;
        }
    };

    let mut components: Vec<String> = Vec::new();
    let nested = bytes.first() == Some(&b'N');
    if nested {
        at += 1;
    }
    // `N` opens a scope `E` closes. A scope that never closes is a name that
    // ran out, not a name that ended, and the two must not read the same.
    let mut closed = !nested;
    loop {
        match bytes.get(at) {
            None => break,
            Some(b'E') if nested => {
                at += 1;
                closed = true;
                break;
            }
            // `St` is `std`, and the only substitution with a fixed meaning.
            Some(b'S') if bytes.get(at + 1) == Some(&b't') => {
                at += 2;
                components.push("std".to_string());
            }
            Some(b'I') => {
                // A template argument list belongs to the component before it.
                if skip_template(&mut at).is_none() {
                    return unreadable();
                }
                match components.last_mut() {
                    Some(last) => last.push_str("<…>"),
                    None => return unreadable(),
                }
            }
            Some(byte) if byte.is_ascii_digit() => match source_name(&mut at) {
                Some(name) => components.push(name),
                None => return unreadable(),
            },
            // Anything else — a substitution, a qualifier, a builtin — is a
            // construct this does not read, and guessing at it is the one thing
            // it must not do.
            Some(_) => return unreadable(),
        }
    }
    if components.is_empty() || at != bytes.len() || !closed {
        return unreadable();
    }
    let short = components
        .last()
        .expect("just checked")
        .split('<')
        .next()
        .unwrap_or_default()
        .to_string();
    (components.join("::"), short)
}

/// Where an address sits in the module's data.
///
/// The answer a static read of guest memory needs first: an address is not a
/// file offset, and the segment that covers it is what turns one into the
/// other. An address no segment covers is not an error — it is memory the
/// module never initialises, which reads as zero at run time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Located {
    /// Which data segment, by index into [`Module::datas`].
    pub segment: u32,
    /// Where the segment starts in guest memory.
    pub base: u32,
    /// How far into the segment the address is.
    pub offset: u32,
    /// The segment's length in bytes.
    pub length: u32,
    /// Whether the segment declares its own address.
    ///
    /// An active segment carries a constant offset. A passive one carries none
    /// — a threaded module's segments all are — and its address is whatever
    /// [`Placement`] recovered from the `memory.init` that copies it.
    pub active: bool,
    /// Where this address's byte sits in the wasm file.
    ///
    /// The number `unwasm bytes` and `unwasm patch` take. Computing it by hand
    /// means subtracting a constant that holds for one segment and not for the
    /// next, which is how a read lands past the end of the file.
    pub file_offset: u32,
}

impl Located {
    /// The guest address this located.
    #[must_use]
    pub fn address(&self) -> u32 {
        self.base + self.offset
    }
}

/// One placed data segment.
struct Placed<'a> {
    index: u32,
    base: u32,
    bytes: &'a [u8],
    active: bool,
    file_offset: u32,
}

/// The data segments as one addressable image.
///
/// Public because reading guest memory by address is a question on its own:
/// a vtable, a table of function pointers, a struct the module initialises
/// statically. Everything here is what the module *starts* with — nothing that
/// ran has written to it yet.
pub struct DataImage<'a> {
    /// Each placed segment, sorted by address.
    segments: Vec<Placed<'a>>,
}

impl<'a> DataImage<'a> {
    /// Builds the image from a module and the placements the analysis recovered.
    ///
    /// A passive segment says nothing about where it goes, so without the
    /// placements a threaded module's data is entirely unaddressable — see
    /// [`Placement`].
    #[must_use]
    pub fn of(module: &'a Module, placements: &std::collections::BTreeMap<u32, Placement>) -> Self {
        let mut segments = Vec::new();
        for (index, segment) in module.datas.iter().enumerate() {
            let (base, active) = match segment.offset {
                Some(ConstExpr::I32(base)) => (base as u32, true),
                Some(_) => continue,
                None => match placements.get(&(index as u32)) {
                    // The placement records where a *part* of the segment went;
                    // the segment's own start is that much earlier.
                    Some(placement) => {
                        match (placement.address as u32).checked_sub(placement.offset) {
                            Some(base) => (base, false),
                            None => continue,
                        }
                    }
                    None => continue,
                },
            };
            if !segment.bytes.is_empty() {
                segments.push(Placed {
                    index: index as u32,
                    base,
                    bytes: segment.bytes.as_slice(),
                    active,
                    file_offset: segment.file_offset,
                });
            }
        }
        segments.sort_by_key(|placed| placed.base);
        Self { segments }
    }

    /// How many segments the image could place.
    #[must_use]
    pub fn placed(&self) -> usize {
        self.segments.len()
    }

    /// The address range the placed segments span, lowest base to highest end.
    #[must_use]
    pub fn extent(&self) -> Option<(u32, u32)> {
        let low = self.segments.first()?.base;
        let high = self
            .segments
            .iter()
            .map(|placed| placed.base + placed.bytes.len() as u32)
            .max()?;
        Some((low, high))
    }

    /// Which segment covers an address, if one does.
    ///
    /// Linear rather than binary: segments can overlap and can share a base, and
    /// the first covering one is the answer a reader wants rather than the last
    /// one whose base sorts below the address.
    #[must_use]
    pub fn locate(&self, address: i32) -> Option<Located> {
        let address = address as u32;
        self.segments
            .iter()
            .find(|placed| {
                address >= placed.base && address - placed.base < placed.bytes.len() as u32
            })
            .map(|placed| Located {
                segment: placed.index,
                base: placed.base,
                offset: address - placed.base,
                length: placed.bytes.len() as u32,
                active: placed.active,
                file_offset: placed.file_offset + (address - placed.base),
            })
    }

    /// The segment nearest below an address, for saying what a miss is near.
    #[must_use]
    pub fn nearest_below(&self, address: i32) -> Option<Located> {
        let address = address as u32;
        self.segments
            .iter()
            .filter(|placed| placed.base <= address)
            .max_by_key(|placed| placed.base)
            .map(|placed| Located {
                segment: placed.index,
                base: placed.base,
                offset: address - placed.base,
                length: placed.bytes.len() as u32,
                active: placed.active,
                file_offset: placed.file_offset + (address - placed.base),
            })
    }

    /// The bytes at an address, if a segment covers all of them.
    ///
    /// `None` covers both "no segment is there" and "the range runs off the end
    /// of the one that is": a read that spans two segments is not answered from
    /// one of them, because the gap between them is not in the module at all.
    #[must_use]
    pub fn bytes(&self, address: i32, length: usize) -> Option<&'a [u8]> {
        let address = address as u32;
        let at = self
            .segments
            .partition_point(|placed| placed.base <= address)
            .checked_sub(1)?;
        let placed = &self.segments[at];
        let within = (address - placed.base) as usize;
        placed.bytes.get(within..within.checked_add(length)?)
    }

    /// The 32-bit little-endian word at an address.
    #[must_use]
    pub fn read32(&self, address: i32) -> Option<i32> {
        let bytes = self.bytes(address, 4)?;
        Some(i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    /// Whether any segment covers this address.
    #[must_use]
    pub fn holds(&self, address: i32) -> bool {
        self.bytes(address, 1).is_some()
    }

    /// Every address at which the four bytes of `value` appear in the data.
    ///
    /// The other half of `constants`: a function pointer installed in a vtable
    /// is never pushed by any instruction, so a search of the code finds
    /// nothing and the table it sits in stays invisible.
    #[must_use]
    pub fn find32(&self, value: i32) -> Vec<Located> {
        let wanted = value.to_le_bytes();
        let mut found = Vec::new();
        for placed in &self.segments {
            for (offset, window) in placed.bytes.windows(4).enumerate() {
                if window == wanted {
                    found.push(Located {
                        segment: placed.index,
                        base: placed.base,
                        offset: offset as u32,
                        length: placed.bytes.len() as u32,
                        active: placed.active,
                        file_offset: placed.file_offset + offset as u32,
                    });
                }
            }
        }
        found
    }

    /// The NUL-terminated readable text at an address, spaces included.
    ///
    /// [`Self::cstring`] refuses a space because a mangled type name never has
    /// one, and admitting them there would let a sentence be read as a class.
    /// A string a reader is being shown has no such constraint, and `"Mobile
    /// platform audio pr"` is not an improvement on the address.
    #[must_use]
    pub fn text(&self, address: i32) -> Option<String> {
        let mut out = Vec::new();
        let mut cursor = address;
        loop {
            let byte = *self.bytes(cursor, 1)?.first()?;
            if byte == 0 {
                break;
            }
            if !(byte.is_ascii_graphic() || byte == b' ') || out.len() >= 512 {
                return None;
            }
            out.push(byte);
            cursor += 1;
        }
        (!out.is_empty()).then(|| String::from_utf8_lossy(&out).into_owned())
    }

    /// The NUL-terminated printable text at an address.
    #[must_use]
    pub fn cstring(&self, address: i32) -> Option<String> {
        let bytes = self.bytes(address, 1)?;
        let _ = bytes;
        let mut out = Vec::new();
        let mut cursor = address;
        loop {
            let byte = *self.bytes(cursor, 1)?.first()?;
            if byte == 0 {
                break;
            }
            if !byte.is_ascii_graphic() || out.len() >= 512 {
                return None;
            }
            out.push(byte);
            cursor += 1;
        }
        (!out.is_empty()).then(|| String::from_utf8_lossy(&out).into_owned())
    }
}

/// The NUL-terminated text at an address, if a data segment puts text there.
///
/// This is what makes a minified module readable at all: the constants that
/// address static strings are often the only names left in it. The oracle work
/// in wa-wasm-oracle found mozjpeg's calling convention this way — a constant
/// resolved to `called \`Option::unwrap()\` on a \`None\` value`, and the panic
/// named the function.
///
/// Returns `None` unless the bytes really look like text: printable ASCII,
/// NUL-terminated, and long enough that it is not three bytes of a struct that
/// happen to be letters.
#[must_use]
pub fn static_text(module: &Module, address: i32) -> Option<String> {
    static_text_inner(module, address, None)
}

/// The text of a known length at an address.
///
/// Rust's strings carry a length instead of a terminator, so reading to the
/// next NUL runs straight through the next string and the one after it — which
/// is why an unbounded read of a Rust module returns things like
/// `"0123456789abcdefcalled \`Option::unwrap()\`…"`. The length is right there
/// in the code, though: a `&str` is passed as `i32.const ptr; i32.const len`,
/// so the instruction after the address says where the string stops.
///
/// Returns `None` if the length does not describe printable text, which is what
/// happens when the second constant was never a length at all.
#[must_use]
pub fn static_text_of_length(module: &Module, address: i32, length: u32) -> Option<String> {
    // A length of zero is an empty string, and one past a few hundred is not a
    // message being passed to something.
    if length == 0 || length > 512 {
        return None;
    }
    static_text_inner(module, address, Some(length as usize))
}

fn static_text_inner(module: &Module, address: i32, length: Option<usize>) -> Option<String> {
    static_text_placed(module, &Default::default(), address, length)
}

/// The text at an address, including in segments the module places itself.
///
/// A threaded module's segments are passive and carry no address, so their
/// contents are unreachable until the placements are resolved — and *every*
/// string in the VoIP module is in one.
#[must_use]
pub fn placed_text(
    module: &Module,
    placements: &std::collections::BTreeMap<u32, Placement>,
    address: i32,
    length: Option<u32>,
) -> Option<String> {
    static_text_placed(module, placements, address, length.map(|len| len as usize))
}

/// Text at an address that is declared to be a name.
///
/// The same read as [`placed_text`] without its minimum length. That minimum
/// is there to stop three stray printable bytes being reported as a string —
/// but an embind registration's name argument *is* a name, and `int` is three
/// characters. Requiring four loses the most common type in any module.
#[must_use]
pub fn placed_name(
    module: &Module,
    placements: &std::collections::BTreeMap<u32, Placement>,
    address: i32,
) -> Option<String> {
    static_text_placed_with(module, placements, address, None, 1)
}

fn static_text_placed(
    module: &Module,
    placements: &std::collections::BTreeMap<u32, Placement>,
    address: i32,
    length: Option<usize>,
) -> Option<String> {
    static_text_placed_with(module, placements, address, length, 4)
}

fn static_text_placed_with(
    module: &Module,
    placements: &std::collections::BTreeMap<u32, Placement>,
    address: i32,
    length: Option<usize>,
    shortest: usize,
) -> Option<String> {
    const LONGEST: usize = 120;

    let address = address as u32;
    for (index, segment) in module.datas.iter().enumerate() {
        // An active segment says where it goes; a passive one is wherever the
        // module put it, if it did so at a constant address.
        let (base, within) = match segment.offset {
            Some(ConstExpr::I32(base)) => (base as u32, 0usize),
            Some(_) => continue,
            None => match placements.get(&(index as u32)) {
                Some(placement) => (placement.address as u32, placement.offset as usize),
                None => continue,
            },
        };
        if address < base {
            continue;
        }
        let at = (address - base) as usize + within;
        if at >= segment.bytes.len() {
            continue;
        }

        // With a length, the slice is exactly that long and needs no
        // terminator; without one, it runs to the next NUL.
        let available = &segment.bytes[at..];
        let slice = match length {
            Some(length) if length <= available.len() => &available[..length],
            Some(_) => return None,
            None => available,
        };

        let mut text = String::new();
        for &byte in slice {
            if byte == 0 && length.is_none() {
                return (text.len() >= shortest).then_some(text);
            }
            // Printable ASCII, plus the whitespace that appears in messages.
            // Anything else means these bytes are not a string.
            if !(byte.is_ascii_graphic() || byte == b' ' || byte == b'\n' || byte == b'\t') {
                return None;
            }
            if text.len() == LONGEST {
                text.push('…');
                return Some(text);
            }
            text.push(byte as char);
        }
        return match length {
            // A length said where it ends, so it ended there.
            Some(_) => (text.len() >= shortest).then_some(text),
            // Ran off the end of the segment without a terminator.
            None => None,
        };
    }
    None
}

/// One memory access at a statically-known offset.
///
/// The answer to "who writes byte +846 of this struct". Reading it out of the
/// module is a scan; recovering it by decompiling candidates and grepping the
/// text is an afternoon, and it misses the ones the compiler wrote through a
/// displaced base.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Access {
    /// The function that makes it.
    pub func: u32,
    /// Where in that function's body, as an index into [`crate::module::Func::body`].
    pub position: usize,
    /// Where the instruction sits in the wasm file, when the decoder recorded it.
    pub file_offset: Option<u32>,
    /// Whether it reads rather than writes.
    pub load: bool,
    /// How many bytes it touches.
    pub width: u32,
    /// The offset encoded in the instruction itself.
    pub encoded: u64,
    /// What the address operand turned out to be.
    pub address: AddressOf,
}

impl Access {
    /// The offset relative to whatever base the address is expressed against.
    ///
    /// For [`AddressOf::Local`] that is the local's value at entry, and this is
    /// the number a struct field is at. For anything else it is the encoded
    /// offset, which is all the instruction says.
    #[must_use]
    pub fn effective(&self) -> i64 {
        match self.address {
            AddressOf::Local { displacement, .. } => displacement + self.encoded as i64,
            AddressOf::Absolute(base) => base + self.encoded as i64,
            AddressOf::Unknown => self.encoded as i64,
        }
    }
}

/// What the address operand of an [`Access`] was known to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddressOf {
    /// `local + displacement`, where the local is a parameter or a declared
    /// local and the displacement is what the function added to it.
    ///
    /// This is the case that matters. A function handed a pointer eight bytes
    /// into a struct writes the field at +846 as +838; one that computes
    /// `base = p - 8` first writes it as +854. Neither encodes 846 anywhere,
    /// and a search for the literal finds neither.
    Local {
        /// Which local, by index — parameters first.
        local: u32,
        /// What was added to it before the instruction's own offset.
        displacement: i64,
    },
    /// A constant address: static memory rather than a field of anything.
    Absolute(i64),
    /// An address this could not follow — a load, a call's result, arithmetic
    /// on two unknowns.
    Unknown,
}

/// Which accesses to collect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Writes only.
    Store,
    /// Reads only.
    Load,
    /// Both.
    Both,
}

impl Kind {
    fn wants(self, load: bool) -> bool {
        match self {
            Self::Store => !load,
            Self::Load => load,
            Self::Both => true,
        }
    }
}

/// Every access in the module whose offset matches, and how it was arrived at.
///
/// `width` of `None` means any width.
///
/// The base tracking is intraprocedural and forward only: a displacement a
/// caller applied is not visible here, and one applied inside a loop is
/// discarded rather than carried across the back edge. What it does see is the
/// straight-line `base = p + k` the compiler emits, which is the case that
/// hides a field from a literal search.
#[must_use]
pub fn accesses_at(
    module: &Module,
    offset: i64,
    width: Option<u32>,
    kind: Kind,
    exact: bool,
) -> AccessReport {
    let import_count = module.func_imports.len() as u32;
    let mut report = AccessReport::default();
    for (at, func) in module.funcs.iter().enumerate() {
        let index = import_count + at as u32;
        let mut walk = Walk::new(module, func, index);
        walk.run();
        if walk.lost {
            report.lost.push(index);
        }
        for access in walk.found {
            if !kind.wants(access.load) {
                continue;
            }
            if width.is_some_and(|wanted| wanted != access.width) {
                continue;
            }
            let matched = if exact {
                access.encoded as i64 == offset
            } else {
                access.effective() == offset
            };
            if matched {
                report.found.push(access);
            }
        }
    }
    report
}

/// What [`accesses_at`] found, and where it could not look.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AccessReport {
    /// The matching accesses.
    pub found: Vec<Access>,
    /// Functions whose operand stack the walk lost track of, after which
    /// nothing more was recorded for them.
    ///
    /// Reported rather than swallowed: a search that silently skipped a
    /// function is a search whose empty answer means nothing.
    pub lost: Vec<u32>,
}

/// A value on the abstract operand stack.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Abstract {
    Const(i64),
    /// `local + displacement`.
    Local {
        local: u32,
        displacement: i64,
    },
    Unknown,
}

/// One open control-flow region.
struct Region {
    /// The operand stack height its body starts at.
    height: usize,
    /// How many values it leaves behind.
    results: usize,
    /// Locals written inside it, which its exit has to forget: a value that was
    /// only assigned on one path is not the value on the other.
    written: Vec<u32>,
}

/// Walks a function's body, following what each address operand is made of.
struct Walk<'a> {
    module: &'a Module,
    func: &'a crate::module::Func,
    index: u32,
    stack: Vec<Abstract>,
    locals: Vec<Abstract>,
    regions: Vec<Region>,
    /// Set when the code after a branch cannot be reached, and cleared at the
    /// `end` or `else` that makes it reachable again.
    unreachable: bool,
    /// Nesting still to be skipped while unreachable.
    skip: usize,
    /// Set when the stack simulation lost track, after which nothing more is
    /// recorded for this function rather than recording something wrong.
    lost: bool,
    found: Vec<Access>,
}

impl<'a> Walk<'a> {
    fn new(module: &'a Module, func: &'a crate::module::Func, index: u32) -> Self {
        let params = module
            .func_type(index)
            .map_or(0, |signature| signature.params.len());
        let count = params + func.locals.len();
        // Every local starts as itself at displacement zero, which is true of a
        // parameter on entry and of a declared local's zero.
        let locals = (0..count)
            .map(|local| Abstract::Local {
                local: local as u32,
                displacement: 0,
            })
            .collect();
        Self {
            module,
            func,
            index,
            stack: Vec::new(),
            locals,
            regions: Vec::new(),
            unreachable: false,
            skip: 0,
            lost: false,
            found: Vec::new(),
        }
    }

    fn pop(&mut self) -> Abstract {
        match self.stack.pop() {
            Some(value) => value,
            None => {
                self.lost = true;
                Abstract::Unknown
            }
        }
    }

    fn popn(&mut self, count: usize) {
        for _ in 0..count {
            let _ = self.pop();
        }
    }

    fn block_arity(&self, ty: crate::module::BlockType) -> (usize, usize) {
        match ty {
            crate::module::BlockType::Empty => (0, 0),
            crate::module::BlockType::Value(_) => (0, 1),
            crate::module::BlockType::Func(index) => self
                .module
                .types
                .get(index as usize)
                .map_or((0, 0), |ty| (ty.params.len(), ty.results.len())),
        }
    }

    fn open(&mut self, ty: crate::module::BlockType) {
        let (params, results) = self.block_arity(ty);
        let height = self.stack.len().saturating_sub(params);
        self.regions.push(Region {
            height,
            results,
            written: Vec::new(),
        });
    }

    /// Forgets what a region's assignments said, and returns to its height.
    fn close(&mut self) {
        let Some(region) = self.regions.pop() else {
            self.lost = true;
            return;
        };
        for local in &region.written {
            if let Some(slot) = self.locals.get_mut(*local as usize) {
                *slot = Abstract::Local {
                    local: *local,
                    displacement: 0,
                };
            }
        }
        self.stack.truncate(region.height);
        for _ in 0..region.results {
            self.stack.push(Abstract::Unknown);
        }
    }

    fn wrote(&mut self, local: u32) {
        if let Some(region) = self.regions.last_mut() {
            region.written.push(local);
        }
    }

    fn go_unreachable(&mut self) {
        self.unreachable = true;
        self.skip = 0;
    }

    fn run(&mut self) {
        for position in 0..self.func.body.len() {
            if self.lost {
                return;
            }
            let op = &self.func.body[position];
            if self.unreachable {
                self.skip_op(op);
                continue;
            }
            self.step(position, op);
        }
    }

    /// While unreachable, only the nesting matters.
    fn skip_op(&mut self, op: &Op) {
        match op {
            Op::Block(_) | Op::Loop(_) | Op::If(_) => self.skip += 1,
            Op::Else if self.skip == 0 => {
                self.unreachable = false;
                // The `else` arm starts from where the `if` did.
                if let Some(region) = self.regions.last() {
                    let height = region.height;
                    self.stack.truncate(height);
                }
                let written: Vec<u32> = self
                    .regions
                    .last()
                    .map(|region| region.written.clone())
                    .unwrap_or_default();
                for local in written {
                    if let Some(slot) = self.locals.get_mut(local as usize) {
                        *slot = Abstract::Local {
                            local,
                            displacement: 0,
                        };
                    }
                }
            }
            Op::End => {
                if self.skip > 0 {
                    self.skip -= 1;
                } else {
                    self.unreachable = false;
                    self.close();
                }
            }
            _ => {}
        }
    }

    fn step(&mut self, position: usize, op: &Op) {
        use crate::module::{AtomicKind, StoreKind};
        match op {
            Op::Unreachable => self.go_unreachable(),
            Op::Nop | Op::DataDrop(_) | Op::AtomicFence => {}
            Op::Block(ty) | Op::Loop(ty) => {
                let ty = *ty;
                // A loop's body can be re-entered with different locals, so
                // nothing carried into it survives the back edge.
                if matches!(op, Op::Loop(_)) {
                    for (local, slot) in self.locals.iter_mut().enumerate() {
                        *slot = Abstract::Local {
                            local: local as u32,
                            displacement: 0,
                        };
                    }
                }
                self.open(ty);
            }
            Op::If(ty) => {
                let ty = *ty;
                let _ = self.pop();
                self.open(ty);
            }
            Op::Else => {
                // Everything the `then` arm assigned is forgotten, and the
                // stack goes back to where the `if` left it.
                let (height, written) = match self.regions.last() {
                    Some(region) => (region.height, region.written.clone()),
                    None => {
                        self.lost = true;
                        return;
                    }
                };
                for local in written {
                    if let Some(slot) = self.locals.get_mut(local as usize) {
                        *slot = Abstract::Local {
                            local,
                            displacement: 0,
                        };
                    }
                }
                self.stack.truncate(height);
            }
            Op::End => {
                if self.regions.is_empty() {
                    // The function's own end.
                    return;
                }
                self.close();
            }
            Op::Br(_) | Op::BrTable { .. } | Op::Return => self.go_unreachable(),
            Op::BrIf(_) => {
                let _ = self.pop();
            }
            Op::Call(callee) => {
                let (params, results) = self
                    .module
                    .func_type(*callee)
                    .map_or((0, 0), |ty| (ty.params.len(), ty.results.len()));
                self.popn(params);
                for _ in 0..results {
                    self.stack.push(Abstract::Unknown);
                }
            }
            Op::CallIndirect { type_index } => {
                let (params, results) = self
                    .module
                    .types
                    .get(*type_index as usize)
                    .map_or((0, 0), |ty| (ty.params.len(), ty.results.len()));
                // The table index, then the arguments.
                self.popn(params + 1);
                for _ in 0..results {
                    self.stack.push(Abstract::Unknown);
                }
            }
            Op::Drop => {
                let _ = self.pop();
            }
            Op::Select => {
                self.popn(3);
                self.stack.push(Abstract::Unknown);
            }
            Op::LocalGet(index) => {
                let value = self
                    .locals
                    .get(*index as usize)
                    .copied()
                    .unwrap_or(Abstract::Unknown);
                self.stack.push(value);
            }
            Op::LocalSet(index) => {
                let value = self.pop();
                self.assign(*index, value);
            }
            Op::LocalTee(index) => {
                let value = self.pop();
                self.assign(*index, value);
                let now = self
                    .locals
                    .get(*index as usize)
                    .copied()
                    .unwrap_or(Abstract::Unknown);
                self.stack.push(now);
            }
            Op::GlobalGet(_) => self.stack.push(Abstract::Unknown),
            Op::GlobalSet(_) => {
                let _ = self.pop();
            }
            Op::I32Const(value) => self.stack.push(Abstract::Const(i64::from(*value))),
            Op::I64Const(value) => self.stack.push(Abstract::Const(*value)),
            Op::F32Const(_) | Op::F64Const(_) => self.stack.push(Abstract::Unknown),
            Op::Load { kind, mem } => {
                let address = self.pop();
                self.record(position, true, load_width(*kind), mem.offset, address);
                self.stack.push(Abstract::Unknown);
            }
            Op::Store { kind, mem } => {
                let _value = self.pop();
                let address = self.pop();
                let width = match kind {
                    StoreKind::I32Store8 | StoreKind::I64Store8 => 1,
                    StoreKind::I32Store16 | StoreKind::I64Store16 => 2,
                    StoreKind::I32 | StoreKind::F32 | StoreKind::I64Store32 => 4,
                    StoreKind::I64 | StoreKind::F64 => 8,
                };
                self.record(position, false, width, mem.offset, address);
            }
            Op::MemorySize => self.stack.push(Abstract::Unknown),
            Op::MemoryGrow => {
                let _ = self.pop();
                self.stack.push(Abstract::Unknown);
            }
            Op::MemoryCopy | Op::MemoryFill | Op::MemoryInit(_) => self.popn(3),
            Op::Atomic { op: atomic, mem } => {
                let (pops, pushes) = match atomic.kind {
                    AtomicKind::Load => (1, 1),
                    AtomicKind::Store => (2, 0),
                    AtomicKind::Rmw(_) => (2, 1),
                    AtomicKind::Cmpxchg => (3, 1),
                    AtomicKind::Notify => (2, 1),
                    AtomicKind::Wait => (3, 1),
                };
                // The address is the deepest operand, so it comes off last.
                let mut operands = Vec::with_capacity(pops);
                for _ in 0..pops {
                    operands.push(self.pop());
                }
                let address = operands.pop().unwrap_or(Abstract::Unknown);
                let load = !matches!(atomic.kind, AtomicKind::Store);
                self.record(position, load, atomic.width, mem.offset, address);
                for _ in 0..pushes {
                    self.stack.push(Abstract::Unknown);
                }
            }
            Op::Num(num) => {
                let operands = num.operands().len();
                let folded = self.fold(*num, operands);
                self.popn(operands);
                self.stack.push(folded);
            }
        }
    }

    /// `a + k` and `a - k`, which is all a base displacement is made of.
    fn fold(&self, num: crate::ops::NumOp, operands: usize) -> Abstract {
        use crate::ops::NumOp;
        if operands != 2 || self.stack.len() < 2 {
            return Abstract::Unknown;
        }
        let right = self.stack[self.stack.len() - 1];
        let left = self.stack[self.stack.len() - 2];
        match num {
            NumOp::I32Add | NumOp::I64Add => match (left, right) {
                (Abstract::Const(a), Abstract::Const(b)) => Abstract::Const(a.wrapping_add(b)),
                (
                    Abstract::Local {
                        local,
                        displacement,
                    },
                    Abstract::Const(b),
                )
                | (
                    Abstract::Const(b),
                    Abstract::Local {
                        local,
                        displacement,
                    },
                ) => Abstract::Local {
                    local,
                    displacement: displacement.wrapping_add(b),
                },
                _ => Abstract::Unknown,
            },
            NumOp::I32Sub | NumOp::I64Sub => match (left, right) {
                (Abstract::Const(a), Abstract::Const(b)) => Abstract::Const(a.wrapping_sub(b)),
                (
                    Abstract::Local {
                        local,
                        displacement,
                    },
                    Abstract::Const(b),
                ) => Abstract::Local {
                    local,
                    displacement: displacement.wrapping_sub(b),
                },
                _ => Abstract::Unknown,
            },
            _ => Abstract::Unknown,
        }
    }

    fn assign(&mut self, index: u32, value: Abstract) {
        self.wrote(index);
        // Anything else that described itself in terms of this local described
        // the *old* value, and is now stale. A chain kept across a reassignment
        // is the one way this reports a confident wrong number.
        for local in 0..self.locals.len() {
            if local as u32 == index {
                continue;
            }
            if matches!(self.locals[local], Abstract::Local { local: base, .. } if base == index) {
                self.locals[local] = Abstract::Local {
                    local: local as u32,
                    displacement: 0,
                };
            }
        }
        // A local assigned from itself-plus-something keeps describing itself;
        // one assigned from another local describes that one. And a local
        // assigned something this cannot describe still describes *itself* —
        // which is not a guess, it is what the local now holds, and it is what
        // makes `frame = g0 - 1264` a base rather than a dead end. Almost every
        // store in a C function goes through one of those.
        let described = match value {
            Abstract::Local { local, .. } if local == index => Abstract::Local {
                local: index,
                displacement: 0,
            },
            Abstract::Unknown => Abstract::Local {
                local: index,
                displacement: 0,
            },
            other => other,
        };
        if let Some(slot) = self.locals.get_mut(index as usize) {
            *slot = described;
        }
    }

    fn record(&mut self, position: usize, load: bool, width: u32, encoded: u64, address: Abstract) {
        let address = match address {
            Abstract::Local {
                local,
                displacement,
            } => AddressOf::Local {
                local,
                displacement,
            },
            Abstract::Const(value) => AddressOf::Absolute(value),
            Abstract::Unknown => AddressOf::Unknown,
        };
        self.found.push(Access {
            func: self.index,
            position,
            file_offset: self.func.offsets.get(position).map(|(offset, _)| *offset),
            load,
            width,
            encoded,
            address,
        });
    }
}

fn load_width(kind: crate::module::LoadKind) -> u32 {
    use crate::module::LoadKind;
    match kind {
        LoadKind::I32Load8S | LoadKind::I32Load8U | LoadKind::I64Load8S | LoadKind::I64Load8U => 1,
        LoadKind::I32Load16S
        | LoadKind::I32Load16U
        | LoadKind::I64Load16S
        | LoadKind::I64Load16U => 2,
        LoadKind::I32 | LoadKind::F32 | LoadKind::I64Load32S | LoadKind::I64Load32U => 4,
        LoadKind::I64 | LoadKind::F64 => 8,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::module::{DataSegment, Func, FuncType, GlobalDef, ValType};
    use crate::ops::NumOp;

    #[test]
    fn a_function_whose_type_is_missing_still_fingerprints() {
        // A module can name a type index that is not there. The fingerprint
        // has to come out regardless — refusing here would mean a catalogue
        // could not be built from a module the decoder accepted.
        let module = Module {
            funcs: vec![Func {
                type_index: 9,
                locals: Vec::new(),
                body: vec![Op::I32Const(1), Op::Drop],
                offsets: Vec::new(),
            }],
            ..Module::default()
        };
        let without = fingerprint(&module, &module.funcs[0]);

        let mut typed = module.clone();
        typed.types = vec![FuncType {
            params: vec![ValType::I32],
            results: Vec::new(),
        }];
        typed.funcs[0].type_index = 0;
        assert_ne!(
            without,
            fingerprint(&typed, &typed.funcs[0]),
            "and a known signature is part of it"
        );
    }

    fn module_with_globals(count: usize, mutable: bool) -> Module {
        Module {
            types: vec![FuncType::default()],
            globals: (0..count)
                .map(|_| GlobalDef {
                    ty: ValType::I32,
                    mutable,
                    init: ConstExpr::I32(65536),
                })
                .collect(),
            ..Module::default()
        }
    }

    fn prologue(global: u32) -> Vec<Op> {
        vec![
            Op::GlobalGet(global),
            Op::I32Const(32),
            Op::Num(NumOp::I32Sub),
            Op::LocalTee(0),
            Op::GlobalSet(global),
        ]
    }

    fn with_bodies(mut module: Module, bodies: Vec<Vec<Op>>) -> Module {
        module.funcs = bodies
            .into_iter()
            .map(|body| Func {
                type_index: 0,
                locals: Vec::new(),
                body,
                offsets: Vec::new(),
            })
            .collect();
        module
    }

    #[test]
    fn an_exported_name_settles_it() {
        let mut module = module_with_globals(3, true);
        module.exports.push(crate::module::Export {
            name: "__stack_pointer".into(),
            kind: ExportKind::Global,
            index: 2,
        });
        let found = analyse(&module).stack_pointer.expect("named");
        assert_eq!(found.global, 2);
        assert_eq!(found.evidence, Evidence::Exported);
    }

    #[test]
    fn a_global_exported_under_another_name_is_not_taken_for_it() {
        let mut module = module_with_globals(1, true);
        module.exports.push(crate::module::Export {
            name: "__heap_base".into(),
            kind: ExportKind::Global,
            index: 0,
        });
        assert!(analyse(&module).stack_pointer.is_none());
    }

    #[test]
    fn the_deepest_nesting_is_the_deepest_one_function_reaches() {
        // Two functions, and the answer is the deeper one — not the sum, and
        // not the last. Nesting that closes and reopens is not deeper.
        let shallow = vec![
            Op::Block(crate::module::BlockType::Empty),
            Op::End,
            Op::Block(crate::module::BlockType::Empty),
            Op::Block(crate::module::BlockType::Empty),
            Op::End,
            Op::End,
        ];
        let deep = vec![
            Op::Block(crate::module::BlockType::Empty),
            Op::Loop(crate::module::BlockType::Empty),
            Op::If(crate::module::BlockType::Empty),
            Op::End,
            Op::End,
            Op::End,
        ];
        let module = with_bodies(module_with_globals(1, true), vec![shallow, deep]);
        assert_eq!(deepest_nesting(&module), Some((1, 3)));
        assert_eq!(deepest_nesting(&Module::default()), None);
    }

    #[test]
    fn the_name_section_settles_it_when_nothing_exports_it() {
        // A debug build names the stack pointer and does not necessarily export
        // it: `--export-all` under some linkers leaves the mutable global out.
        // The name is still the module saying which global it is.
        let mut module = module_with_globals(3, true);
        module.global_names.push((1, "__stack_pointer".into()));
        let found = analyse(&module).stack_pointer.expect("named");
        assert_eq!(found.global, 1);
        assert_eq!(found.evidence, Evidence::Named);
    }

    #[test]
    fn an_export_outranks_a_name() {
        // Both are conclusive, so this is only about which one is quoted back;
        // the export is the module's interface and cannot be stripped without
        // changing it.
        let mut module = module_with_globals(3, true);
        module.global_names.push((1, "__stack_pointer".into()));
        module.exports.push(crate::module::Export {
            name: "__stack_pointer".into(),
            kind: ExportKind::Global,
            index: 2,
        });
        let found = analyse(&module).stack_pointer.expect("named");
        assert_eq!(found.global, 2);
        assert_eq!(found.evidence, Evidence::Exported);
    }

    #[test]
    fn a_global_named_something_else_is_not_taken_for_it() {
        let mut module = module_with_globals(1, true);
        module.global_names.push((0, "__heap_base".into()));
        assert!(analyse(&module).stack_pointer.is_none());
    }

    #[test]
    fn a_name_for_a_global_that_is_not_there_names_nothing() {
        // The index reaches the output as a field name. One past the end would
        // be `self.g9_stack_pointer` on a struct that declares one global, and
        // the generated Rust would not compile — which is the one thing it must
        // always do. Both naming routes are checked, since a malformed module
        // can carry either.
        let mut module = module_with_globals(1, true);
        module.global_names.push((9, "__stack_pointer".into()));
        assert!(analyse(&module).stack_pointer.is_none());

        let mut module = module_with_globals(1, true);
        module.exports.push(crate::module::Export {
            name: "__stack_pointer".into(),
            kind: ExportKind::Global,
            index: 9,
        });
        assert!(analyse(&module).stack_pointer.is_none());
    }

    #[test]
    fn the_prologue_identifies_it_in_a_stripped_module() {
        let module = with_bodies(
            module_with_globals(2, true),
            vec![prologue(1), prologue(1), vec![Op::Nop]],
        );
        let found = analyse(&module).stack_pointer.expect("found by its use");
        assert_eq!(found.global, 1);
        assert_eq!(found.evidence, Evidence::Prologue { functions: 2 });
    }

    #[test]
    fn one_prologue_is_a_coincidence_and_is_not_reported() {
        let module = with_bodies(module_with_globals(1, true), vec![prologue(0)]);
        assert!(
            analyse(&module).stack_pointer.is_none(),
            "a single match is not a calling convention"
        );
    }

    #[test]
    fn an_immutable_global_is_never_the_stack_pointer() {
        // The shape can appear around a constant — `base - 32` — and a stack
        // pointer that cannot be written to is not one.
        let module = with_bodies(
            module_with_globals(1, false),
            vec![prologue(0), prologue(0)],
        );
        assert!(analyse(&module).stack_pointer.is_none());
    }

    #[test]
    fn a_prologue_that_stores_somewhere_else_does_not_count() {
        let body = vec![
            Op::GlobalGet(0),
            Op::I32Const(32),
            Op::Num(NumOp::I32Sub),
            Op::GlobalSet(1),
        ];
        let module = with_bodies(module_with_globals(2, true), vec![body.clone(), body]);
        assert!(analyse(&module).stack_pointer.is_none());
    }

    #[test]
    fn adding_to_a_global_is_an_epilogue_not_a_prologue() {
        let body = vec![
            Op::GlobalGet(0),
            Op::I32Const(32),
            Op::Num(NumOp::I32Add),
            Op::GlobalSet(0),
        ];
        let module = with_bodies(module_with_globals(1, true), vec![body.clone(), body]);
        assert!(analyse(&module).stack_pointer.is_none());
    }

    #[test]
    fn a_module_with_no_globals_reports_nothing() {
        assert!(analyse(&Module::default()).stack_pointer.is_none());
        let module = with_bodies(module_with_globals(0, true), vec![prologue(0)]);
        assert!(analyse(&module).stack_pointer.is_none());
    }

    fn module_with_data(offset: i32, bytes: &[u8]) -> Module {
        Module {
            datas: vec![DataSegment {
                file_offset: 0,
                offset: Some(ConstExpr::I32(offset)),
                bytes: bytes.to_vec(),
            }],
            ..Module::default()
        }
    }

    /// A one-function module: one i32 parameter, `locals` extra i32 locals.
    fn module_with_body(locals: usize, body: Vec<Op>) -> Module {
        Module {
            types: vec![FuncType {
                params: vec![ValType::I32],
                results: Vec::new(),
            }],
            funcs: vec![Func {
                type_index: 0,
                locals: vec![ValType::I32; locals],
                body,
                offsets: Vec::new(),
            }],
            ..Module::default()
        }
    }

    fn store8(offset: u64) -> Op {
        Op::Store {
            kind: crate::module::StoreKind::I32Store8,
            mem: crate::module::MemArg { offset },
        }
    }

    #[test]
    fn a_store_through_a_displaced_base_reports_the_offset_it_never_encodes() {
        // `l1 = p0 - 8; store8(l1 + 854)` writes p0 + 846, and 846 is nowhere
        // in the instruction stream. This is the write a grep does not find.
        let module = module_with_body(
            1,
            vec![
                Op::LocalGet(0),
                Op::I32Const(-8),
                Op::Num(crate::ops::NumOp::I32Add),
                Op::LocalSet(1),
                Op::LocalGet(1),
                Op::I32Const(9),
                store8(854),
                Op::End,
            ],
        );
        let report = accesses_at(&module, 846, Some(1), Kind::Store, false);
        assert_eq!(report.found.len(), 1, "{:?}", report.found);
        assert_eq!(
            report.found[0].address,
            AddressOf::Local {
                local: 0,
                displacement: -8
            }
        );
        assert_eq!(report.found[0].encoded, 854);
        assert!(report.lost.is_empty());

        // And the literal search finds it at 854 and not at 846, which is the
        // whole difference.
        assert!(
            accesses_at(&module, 846, Some(1), Kind::Store, true)
                .found
                .is_empty()
        );
        assert_eq!(
            accesses_at(&module, 854, Some(1), Kind::Store, true)
                .found
                .len(),
            1
        );
    }

    #[test]
    fn a_local_that_cannot_be_described_still_describes_itself() {
        // `frame = g0 - 1264` is the shadow stack, and almost every store in a
        // C function goes through it. Treating the global as unknown and
        // stopping there would lose all of them.
        let module = Module {
            globals: vec![GlobalDef {
                ty: ValType::I32,
                mutable: true,
                init: ConstExpr::I32(0),
            }],
            ..module_with_body(
                1,
                vec![
                    Op::GlobalGet(0),
                    Op::I32Const(1264),
                    Op::Num(crate::ops::NumOp::I32Sub),
                    Op::LocalSet(1),
                    Op::LocalGet(1),
                    Op::I32Const(7),
                    store8(846),
                    Op::End,
                ],
            )
        };
        let report = accesses_at(&module, 846, Some(1), Kind::Store, false);
        assert_eq!(report.found.len(), 1);
        assert_eq!(
            report.found[0].address,
            AddressOf::Local {
                local: 1,
                displacement: 0
            },
            "the offset is relative to the frame, which is what a struct field is"
        );
    }

    #[test]
    fn a_chain_across_a_reassignment_is_dropped_rather_than_carried() {
        // `l1 = p0 + 100; p0 = something else; store8(l1 + 46)` no longer
        // writes p0 + 146 — p0 moved. Keeping the chain is the one way this
        // reports a confident wrong number.
        let module = module_with_body(
            1,
            vec![
                Op::LocalGet(0),
                Op::I32Const(100),
                Op::Num(crate::ops::NumOp::I32Add),
                Op::LocalSet(1),
                Op::I32Const(4096),
                Op::LocalSet(0),
                Op::LocalGet(1),
                Op::I32Const(7),
                store8(46),
                Op::End,
            ],
        );
        assert!(
            accesses_at(&module, 146, Some(1), Kind::Store, false)
                .found
                .is_empty(),
            "p0 is not what it was"
        );
        // It is still found relative to the local it actually goes through.
        let report = accesses_at(&module, 46, Some(1), Kind::Store, false);
        assert_eq!(
            report.found[0].address,
            AddressOf::Local {
                local: 1,
                displacement: 0
            }
        );
    }

    #[test]
    fn a_displacement_formed_inside_a_loop_does_not_cross_the_back_edge() {
        // The walk is forward only, so a value carried round a loop is not
        // something it can claim to know.
        let module = module_with_body(
            1,
            vec![
                Op::Loop(crate::module::BlockType::Empty),
                Op::LocalGet(1),
                Op::I32Const(7),
                store8(846),
                Op::LocalGet(0),
                Op::I32Const(-8),
                Op::Num(crate::ops::NumOp::I32Add),
                Op::LocalSet(1),
                Op::End,
                Op::End,
            ],
        );
        let report = accesses_at(&module, 846, Some(1), Kind::Store, false);
        assert_eq!(report.found.len(), 1);
        assert_eq!(
            report.found[0].address,
            AddressOf::Local {
                local: 1,
                displacement: 0
            },
            "not p0 - 8: that assignment happens after this read, and again before it"
        );
    }

    #[test]
    fn a_store_at_a_constant_address_is_not_a_field_of_anything() {
        let module = module_with_body(
            0,
            vec![Op::I32Const(1024), Op::I32Const(7), store8(20), Op::End],
        );
        let report = accesses_at(&module, 1044, Some(1), Kind::Store, false);
        assert_eq!(report.found.len(), 1);
        assert_eq!(report.found[0].address, AddressOf::Absolute(1024));
    }

    #[test]
    fn loads_and_stores_are_asked_for_separately() {
        let module = module_with_body(
            0,
            vec![
                Op::LocalGet(0),
                Op::Load {
                    kind: crate::module::LoadKind::I32,
                    mem: crate::module::MemArg { offset: 20 },
                },
                Op::Drop,
                Op::End,
            ],
        );
        assert!(
            accesses_at(&module, 20, Some(4), Kind::Store, false)
                .found
                .is_empty()
        );
        assert_eq!(
            accesses_at(&module, 20, Some(4), Kind::Load, false)
                .found
                .len(),
            1
        );
        assert_eq!(
            accesses_at(&module, 20, Some(4), Kind::Both, false)
                .found
                .len(),
            1
        );
        // The width matters: the same offset at another width is another field.
        assert!(
            accesses_at(&module, 20, Some(1), Kind::Load, false)
                .found
                .is_empty()
        );
    }

    #[test]
    fn the_data_image_says_which_segment_covers_an_address() {
        let module = Module {
            datas: vec![
                DataSegment {
                    file_offset: 100,
                    offset: Some(ConstExpr::I32(1024)),
                    bytes: vec![1, 2, 3, 4],
                },
                DataSegment {
                    file_offset: 200,
                    offset: Some(ConstExpr::I32(4096)),
                    bytes: vec![9, 9],
                },
            ],
            ..Module::default()
        };
        let image = DataImage::of(&module, &Default::default());
        let found = image.locate(1026).expect("covered");
        assert_eq!(found.segment, 0);
        assert_eq!(found.offset, 2);
        assert_eq!(found.address(), 1026);
        // The file offset is the whole point: an address and an offset into the
        // wasm file are different numbers, and recovering one from the other by
        // subtracting a constant holds for one segment and not for the next.
        assert_eq!(found.file_offset, 102);
        assert!(found.active);
        assert_eq!(image.locate(4097).map(|found| found.file_offset), Some(201));

        // A gap is not an error; it is memory the module never initialises.
        assert_eq!(image.locate(2048), None);
        assert_eq!(image.extent(), Some((1024, 4098)));
        assert_eq!(image.nearest_below(2048).map(|near| near.segment), Some(0));
        // And a read that would span two segments is answered by neither: the
        // bytes between them are not in the module at all.
        assert_eq!(image.bytes(1026, 4), None);
    }

    #[test]
    fn a_passive_segment_is_addressable_once_its_placement_is_known() {
        let module = Module {
            datas: vec![DataSegment {
                file_offset: 42,
                offset: None,
                bytes: vec![7, 0, 0, 0],
            }],
            ..Module::default()
        };
        // Unplaced, a passive segment has no address at all.
        let image = DataImage::of(&module, &Default::default());
        assert!(!image.holds(2048));

        let placements = [(
            0u32,
            Placement {
                address: 2048,
                offset: 0,
                length: 4,
            },
        )]
        .into_iter()
        .collect();
        let image = DataImage::of(&module, &placements);
        assert_eq!(image.read32(2048), Some(7));
        let found = image.locate(2048).expect("placed");
        assert!(
            !found.active,
            "placed by a memory.init, not by its own offset"
        );
        assert_eq!(found.file_offset, 42);
    }

    #[test]
    fn find32_locates_a_word_no_instruction_ever_pushes() {
        // A function pointer installed in a vtable is written by the linker,
        // not by any code, so a search of the instructions finds nothing.
        let module = module_with_data(1024, &[0, 0, 0, 0, 0x2f, 0x14, 0, 0]);
        let image = DataImage::of(&module, &Default::default());
        let found = image.find32(5167);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].address(), 1028);
    }

    #[test]
    fn a_pointer_table_stops_where_it_stops_looking_like_one() {
        let table: std::collections::BTreeMap<u32, u32> =
            [(1u32, 10u32), (2, 20), (3, 30)].into_iter().collect();
        let mut bytes = Vec::new();
        for word in [1i32, 2, 0, 0, 3, -1, 1] {
            bytes.extend_from_slice(&word.to_le_bytes());
        }
        let module = module_with_data(1024, &bytes);
        let image = DataImage::of(&module, &Default::default());

        // The nulls in the middle are pure virtuals — a real entry follows
        // them. The -1 is not a table index, and it ends the table; the 1 after
        // it belongs to whatever comes next.
        let slots = pointer_table(&image, &table, 1024, None);
        assert_eq!(
            slots,
            vec![Some(10), Some(20), None, None, Some(30)],
            "a null slot is kept, so the numbering after it stays right"
        );

        // An explicit count reads what was asked for, whatever is there.
        let slots = pointer_table(&image, &table, 1024, Some(7));
        assert_eq!(slots.len(), 7);
        assert_eq!(slots[5], None, "-1 is not a table index");
    }

    #[test]
    fn a_run_of_nulls_that_nothing_follows_is_the_end_of_the_table() {
        // The case that matters: two vtables laid out next to each other. The
        // second one's `{0, type_info}` header would read as a null slot and a
        // huge word — and following it would report the next class's methods as
        // this one's.
        let table: std::collections::BTreeMap<u32, u32> =
            [(1u32, 10u32), (2, 20)].into_iter().collect();
        let mut bytes = Vec::new();
        for word in [1i32, 0, 999_999, 2] {
            bytes.extend_from_slice(&word.to_le_bytes());
        }
        let module = module_with_data(1024, &bytes);
        let image = DataImage::of(&module, &Default::default());
        let slots = pointer_table(&image, &table, 1024, None);
        assert_eq!(slots, vec![Some(10)], "the next object is not this table");
    }

    #[test]
    fn a_constant_that_addresses_text_reads_back_as_that_text() {
        let module = module_with_data(1024, b"hello, world\0and more\0");
        assert_eq!(static_text(&module, 1024).as_deref(), Some("hello, world"));
        // Mid-string is still text: it is where a suffix would start.
        assert_eq!(static_text(&module, 1031).as_deref(), Some("world"));
        assert_eq!(static_text(&module, 1037).as_deref(), Some("and more"));
    }

    #[test]
    fn bytes_that_are_not_text_are_not_reported_as_text() {
        // A struct whose first field happens to be small integers.
        let module = module_with_data(0, &[0x01, 0x02, 0x03, 0x04, 0x00]);
        assert_eq!(static_text(&module, 0), None);
        // Text that is too short to be a name rather than a coincidence.
        let module = module_with_data(0, b"ab\0");
        assert_eq!(static_text(&module, 0), None);
        // High bytes: UTF-8 or binary, either way not something to quote raw.
        let module = module_with_data(0, &[b'a', b'b', b'c', 0xE2, 0x82, 0xAC, 0]);
        assert_eq!(static_text(&module, 0), None);
    }

    #[test]
    fn text_without_a_terminator_is_not_reported() {
        let module = module_with_data(0, b"unterminated");
        assert_eq!(static_text(&module, 0), None);
    }

    #[test]
    fn a_very_long_string_is_cut_rather_than_pasted_whole() {
        let long = "x".repeat(300);
        let mut bytes = long.into_bytes();
        bytes.push(0);
        let module = module_with_data(0, &bytes);
        let text = static_text(&module, 0).expect("text");
        assert!(text.ends_with('…'), "{text}");
        assert!(text.chars().count() <= 121);
    }

    // ---- frames ----

    fn framed_module(body: Vec<Op>) -> Module {
        let mut module = module_with_globals(1, true);
        module.exports.push(crate::module::Export {
            name: "__stack_pointer".into(),
            kind: ExportKind::Global,
            index: 0,
        });
        module.types = vec![FuncType::default()];
        with_bodies(module, vec![body])
    }

    /// `global.get $sp; i32.const size; i32.sub; local.tee $base; global.set $sp`
    fn frame_prologue(size: i32, base: u32) -> Vec<Op> {
        vec![
            Op::GlobalGet(0),
            Op::I32Const(size),
            Op::Num(NumOp::I32Sub),
            Op::LocalTee(base),
            Op::GlobalSet(0),
        ]
    }

    fn frame_epilogue(size: i32, base: u32) -> Vec<Op> {
        vec![
            Op::LocalGet(base),
            Op::I32Const(size),
            Op::Num(NumOp::I32Add),
            Op::GlobalSet(0),
        ]
    }

    fn frame_of(body: Vec<Op>) -> Frame {
        let module = framed_module(body);
        analyse(&module)
            .frames
            .get(&0)
            .cloned()
            .expect("the function has a frame")
    }

    fn store(offset: u64) -> Op {
        Op::Store {
            kind: crate::module::StoreKind::I32,
            mem: crate::module::MemArg { offset },
        }
    }

    fn load(offset: u64) -> Op {
        Op::Load {
            kind: crate::module::LoadKind::I32,
            mem: crate::module::MemArg { offset },
        }
    }

    /// The same frame, spelled the way clang 18 spells it at `-O0`: every
    /// intermediate value parked in its own local, including the stack pointer
    /// and the size, so the subtraction reads two locals rather than the
    /// operand stack.
    fn unfolded_prologue(size: i32, base: u32) -> Vec<Op> {
        vec![
            Op::GlobalGet(0),
            Op::LocalSet(8),
            Op::I32Const(size),
            Op::LocalSet(9),
            Op::LocalGet(8),
            Op::LocalGet(9),
            Op::Num(NumOp::I32Sub),
            Op::LocalSet(base),
            Op::LocalGet(base),
            Op::GlobalSet(0),
        ]
    }

    /// The epilogue that goes with it: the size through a local again, and the
    /// restored pointer through one more before it reaches the global.
    fn unfolded_epilogue(size: i32, base: u32) -> Vec<Op> {
        vec![
            Op::I32Const(size),
            Op::LocalSet(9),
            Op::LocalGet(base),
            Op::LocalGet(9),
            Op::Num(NumOp::I32Add),
            Op::LocalSet(10),
            Op::LocalGet(10),
            Op::GlobalSet(0),
        ]
    }

    #[test]
    fn a_frame_reads_the_same_whether_the_prologue_is_folded_or_not() {
        // The two compilers describe the same frame. An analysis that only
        // knows the folded spelling reports that a module built by the other
        // one has no frames at all, which is a statement about the compiler
        // dressed up as one about the code.
        let mut folded = frame_prologue(32, 4);
        folded.extend([Op::LocalGet(4), Op::I32Const(7), store(12)]);
        folded.extend(frame_epilogue(32, 4));

        let mut unfolded = unfolded_prologue(32, 4);
        unfolded.extend([Op::LocalGet(4), Op::I32Const(7), store(12)]);
        unfolded.extend(unfolded_epilogue(32, 4));

        let (folded, unfolded) = (frame_of(folded), frame_of(unfolded));
        // Everything but the prologue's length, which *is* the spelling: five
        // instructions against ten, describing the same frame.
        assert_eq!(folded.prologue, 5);
        assert_eq!(unfolded.prologue, 10);
        assert_eq!(
            Frame {
                prologue: 5,
                ..unfolded.clone()
            },
            folded
        );
        assert_eq!(unfolded.size, 32);
        assert_eq!(unfolded.base_local, 4);
        assert!(unfolded.publishes);
        assert!(
            !unfolded.escapes,
            "the address only ever went into locals this walk follows"
        );
        assert_eq!(unfolded.slots[&12].writes, 1);
    }

    #[test]
    fn an_unfolded_leaf_prologue_is_read_and_marked_unpublished() {
        // clang drops the write-back for a function that calls nothing, in this
        // spelling as in the folded one.
        let mut body = unfolded_prologue(16, 3);
        body.truncate(8);
        body.extend([Op::LocalGet(3), Op::I32Const(1), store(4)]);
        let frame = frame_of(body);
        assert_eq!(frame.size, 16);
        assert_eq!(frame.base_local, 3);
        assert!(!frame.publishes);
        assert_eq!(frame.slots[&4].writes, 1);
    }

    #[test]
    fn an_unfolded_prologue_identifies_the_stack_pointer_too() {
        // No export, no name section: the shape is the only evidence, and it
        // has to be countable in this spelling as well.
        let module = with_bodies(
            module_with_globals(2, true),
            vec![
                {
                    let mut body = unfolded_prologue(32, 4);
                    body.extend(unfolded_epilogue(32, 4));
                    body
                },
                {
                    let mut body = unfolded_prologue(16, 4);
                    body.extend(unfolded_epilogue(16, 4));
                    body
                },
            ],
        );
        let found = analyse(&module).stack_pointer.expect("found by its use");
        assert_eq!(found.global, 0);
        assert_eq!(found.evidence, Evidence::Prologue { functions: 2 });
    }

    #[test]
    fn the_unfolded_prologue_is_matched_exactly_and_not_by_its_shape() {
        // The four instructions between the two `local.set`s say *which* locals
        // are subtracted. Reading a different pair is arithmetic on a global
        // that happens to start the same way, and a frame claimed for it would
        // have the wrong base and the wrong size.
        let module = |body: Vec<Op>| {
            let mut module = module_with_globals(1, true);
            module.exports.push(crate::module::Export {
                name: "__stack_pointer".into(),
                kind: ExportKind::Global,
                index: 0,
            });
            with_bodies(module, vec![body])
        };
        let good = unfolded_prologue(16, 3);
        for spoiled in [
            // Not the local the stack pointer went into.
            (4, Op::LocalGet(7)),
            // Not the local the size went into.
            (5, Op::LocalGet(7)),
            // Not a subtraction.
            (6, Op::Num(NumOp::I32Add)),
        ] {
            let (at, op) = spoiled;
            let mut body = good.clone();
            body[at] = op;
            assert!(
                analyse(&module(body)).frames.is_empty(),
                "the prologue was matched with instruction {at} replaced"
            );
        }
        assert!(!analyse(&module(good)).frames.is_empty(), "and unspoiled");
    }

    #[test]
    fn leaf_prologues_are_evidence_only_when_they_address_memory() {
        // Nothing writes the stack pointer back in a module of leaves, so the
        // reservations are all there is — and a reservation that never becomes
        // an address is indistinguishable from arithmetic on a global.
        let bare = || {
            let mut body = unfolded_prologue(16, 3);
            body.truncate(8);
            body
        };
        let module = with_bodies(module_with_globals(1, true), vec![bare(), bare()]);
        assert!(
            analyse(&module).stack_pointer.is_none(),
            "a number, not a frame"
        );

        let addressing = || {
            let mut body = bare();
            body.extend([Op::LocalGet(3), Op::I32Const(1), store(4)]);
            body
        };
        let module = with_bodies(
            module_with_globals(1, true),
            vec![addressing(), addressing()],
        );
        let found = analyse(&module).stack_pointer.expect("found by its use");
        assert_eq!(found.evidence, Evidence::Prologue { functions: 2 });
    }

    #[test]
    fn a_frame_records_its_size_and_the_local_that_holds_it() {
        let mut body = frame_prologue(32, 1);
        body.extend(frame_epilogue(32, 1));
        let frame = frame_of(body);
        assert_eq!(frame.size, 32);
        assert_eq!(frame.base_local, 1);
        assert!(!frame.escapes);
        assert!(frame.slots.is_empty());
    }

    #[test]
    fn a_store_past_the_end_of_the_frame_is_reported() {
        let mut body = frame_prologue(32, 0);
        // frame[28] is the last word inside a 32-byte frame; frame[32] is the
        // first one in the caller's.
        body.extend([Op::LocalGet(0), Op::I32Const(1), store(28)]);
        body.extend([Op::LocalGet(0), Op::I32Const(1), store(32)]);
        let frame = frame_of(body);
        assert_eq!(frame.writes_outside(), vec![(32, 4)]);
    }

    #[test]
    fn a_store_straddling_the_end_of_the_frame_is_reported() {
        // The off-by-one a comparison of the start address alone would miss:
        // the last byte of this store is outside.
        let mut body = frame_prologue(32, 0);
        body.extend([Op::LocalGet(0), Op::I32Const(1), store(30)]);
        assert_eq!(frame_of(body).writes_outside(), vec![(30, 4)]);
    }

    #[test]
    fn reading_past_the_end_of_the_frame_is_not_reported() {
        // Some ABIs put the caller's arguments up there, so a list that
        // included reads would not be short enough to act on.
        let mut body = frame_prologue(32, 0);
        body.extend([Op::LocalGet(0), load(48), Op::Drop]);
        let frame = frame_of(body);
        assert!(frame.writes_outside().is_empty());
        assert!(frame.slots.contains_key(&48), "it is still a slot");
    }

    #[test]
    fn a_store_inside_the_frame_is_not_reported() {
        let mut body = frame_prologue(32, 0);
        body.extend([Op::LocalGet(0), Op::I32Const(1), store(28)]);
        assert!(frame_of(body).writes_outside().is_empty());
    }

    #[test]
    fn a_store_through_a_computed_frame_address_is_counted_separately() {
        // `frame + n` with `n` unknown: an indexed array in the frame. The
        // offset cannot be checked, and this is the count that says so.
        let mut body = frame_prologue(32, 0);
        body.extend([
            Op::LocalGet(0),
            Op::LocalGet(1),
            Op::Num(NumOp::I32Add),
            Op::I32Const(1),
            store(0),
        ]);
        let frame = frame_of(body);
        assert_eq!(frame.computed_writes, 1);
        assert!(frame.escapes, "an offset nobody knows is still an escape");
        assert!(
            frame.writes_outside().is_empty(),
            "and it is not claimed to be outside, because nobody knows"
        );
    }

    #[test]
    fn arithmetic_that_never_touched_the_frame_is_not_a_computed_write() {
        let mut body = frame_prologue(32, 0);
        body.extend([
            Op::LocalGet(1),
            Op::LocalGet(2),
            Op::Num(NumOp::I32Add),
            Op::I32Const(1),
            store(0),
        ]);
        assert_eq!(frame_of(body).computed_writes, 0);
    }

    #[test]
    fn a_function_with_no_prologue_has_no_frame() {
        let module = framed_module(vec![Op::I32Const(1), Op::Drop]);
        assert!(analyse(&module).frames.is_empty());
    }

    #[test]
    fn slots_are_collected_with_their_width_and_direction() {
        let mut body = frame_prologue(48, 0);
        // frame[12] = x, twice
        body.extend([Op::LocalGet(0), Op::I32Const(7), store(12)]);
        body.extend([Op::LocalGet(0), Op::I32Const(9), store(12)]);
        // read frame[12] and frame[16]
        body.extend([Op::LocalGet(0), load(12), Op::Drop]);
        body.extend([Op::LocalGet(0), load(16), Op::Drop]);
        // a byte at frame[20]
        body.extend([
            Op::LocalGet(0),
            Op::I32Const(1),
            Op::Store {
                kind: crate::module::StoreKind::I32Store8,
                mem: crate::module::MemArg { offset: 20 },
            },
        ]);
        body.extend(frame_epilogue(48, 0));

        let frame = frame_of(body);
        assert!(!frame.escapes, "nothing here leaves the function");
        assert_eq!(frame.slots.len(), 3);
        assert_eq!(
            frame.slots[&12],
            Slot {
                width: 4,
                reads: 1,
                writes: 2,
                uniform: Some((4, ValType::I32)),
                mixed: false,
                indirect: false,
            }
        );
        assert_eq!(
            frame.slots[&16],
            Slot {
                width: 4,
                reads: 1,
                writes: 0,
                uniform: Some((4, ValType::I32)),
                mixed: false,
                indirect: false,
            }
        );
        assert_eq!(
            frame.slots[&20],
            Slot {
                width: 1,
                reads: 0,
                writes: 1,
                uniform: Some((1, ValType::I32)),
                mixed: false,
                indirect: false,
            }
        );
    }

    #[test]
    fn an_offset_added_before_the_access_lands_in_the_same_place() {
        // `frame + 8` then `i32.load offset=4` is frame[12], the same slot the
        // direct `i32.load offset=12` reaches.
        let mut body = frame_prologue(16, 0);
        body.extend([
            Op::LocalGet(0),
            Op::I32Const(8),
            Op::Num(NumOp::I32Add),
            load(4),
            Op::Drop,
        ]);
        body.extend([Op::LocalGet(0), load(12), Op::Drop]);
        body.extend(frame_epilogue(16, 0));

        let frame = frame_of(body);
        assert_eq!(frame.slots.len(), 1, "{:?}", frame.slots);
        assert_eq!(frame.slots[&12].reads, 2);
    }

    #[test]
    fn passing_the_frame_address_to_a_call_is_an_escape() {
        let mut module = module_with_globals(1, true);
        module.exports.push(crate::module::Export {
            name: "__stack_pointer".into(),
            kind: ExportKind::Global,
            index: 0,
        });
        module.types = vec![
            FuncType::default(),
            FuncType {
                params: vec![ValType::I32],
                results: Vec::new(),
            },
        ];
        let mut body = frame_prologue(16, 0);
        body.extend([Op::LocalGet(0), Op::Call(1)]);
        body.extend(frame_epilogue(16, 0));
        let mut module = with_bodies(module, vec![body, Vec::new()]);
        module.funcs[1].type_index = 1;

        let frame = analyse(&module).frames[&0].clone();
        assert!(
            frame.escapes,
            "the callee can do anything with that address"
        );
    }

    #[test]
    fn storing_the_frame_address_into_memory_is_an_escape() {
        let mut body = frame_prologue(16, 0);
        // frame[0] = frame — a self-referential pointer, and an address that
        // now lives somewhere this walk cannot follow.
        body.extend([Op::LocalGet(0), Op::LocalGet(0), store(0)]);
        body.extend(frame_epilogue(16, 0));
        assert!(frame_of(body).escapes);
    }

    #[test]
    fn a_copy_of_the_frame_address_is_followed_through_the_local() {
        // A copy is not an escape by itself: the walk knows which local it went
        // into, so an access through the copy is an access to the frame and
        // lands in the table like any other. Giving up here is what made an
        // unoptimised build — where every value goes through a local — report a
        // frame it could say nothing about.
        let mut body = frame_prologue(16, 0);
        body.extend([
            Op::LocalGet(0),
            Op::LocalSet(3),
            Op::LocalGet(3),
            Op::I32Const(7),
            store(4),
        ]);
        body.extend(frame_epilogue(16, 0));
        let frame = frame_of(body);
        assert!(!frame.escapes, "the copy never left the function");
        assert_eq!(frame.slots[&4].writes, 1, "the write went to `frame + 4`");
    }

    #[test]
    fn a_copy_of_the_frame_address_live_across_control_flow_is_an_escape() {
        // The other side of following a copy: the walk follows one path, so at
        // a branch it forgets what a local holds — and a local holding the
        // frame address when that happens has gone where it cannot be followed.
        let mut body = frame_prologue(16, 0);
        body.extend([
            Op::LocalGet(0),
            Op::LocalSet(3),
            Op::Block(crate::module::BlockType::Empty),
            Op::End,
        ]);
        body.extend(frame_epilogue(16, 0));
        assert!(frame_of(body).escapes);
    }

    #[test]
    fn reassigning_the_base_local_is_an_escape() {
        let mut body = frame_prologue(16, 0);
        body.extend([Op::I32Const(1234), Op::LocalSet(0)]);
        body.extend(frame_epilogue(16, 0));
        assert!(frame_of(body).escapes);
    }

    #[test]
    fn arithmetic_that_is_not_a_constant_offset_is_an_escape() {
        // `frame + n` where n is a parameter: the slot could be anywhere.
        let mut body = frame_prologue(16, 0);
        body.extend([
            Op::LocalGet(0),
            Op::LocalGet(1),
            Op::Num(NumOp::I32Add),
            load(0),
            Op::Drop,
        ]);
        body.extend(frame_epilogue(16, 0));
        assert!(frame_of(body).escapes);
    }

    #[test]
    fn a_frame_address_live_across_control_flow_ends_the_analysis() {
        let mut body = frame_prologue(16, 0);
        body.extend([Op::LocalGet(0), Op::Block(crate::module::BlockType::Empty)]);
        body.extend(frame_epilogue(16, 0));
        assert!(
            frame_of(body).escapes,
            "the walk cannot follow it, so it must not claim to"
        );
    }

    #[test]
    fn control_flow_with_no_frame_address_pending_is_fine() {
        let mut body = frame_prologue(16, 0);
        body.extend([Op::LocalGet(0), Op::I32Const(1), store(4)]);
        body.push(Op::Block(crate::module::BlockType::Empty));
        body.push(Op::End);
        body.extend([Op::LocalGet(0), load(4), Op::Drop]);
        body.extend(frame_epilogue(16, 0));
        let frame = frame_of(body);
        assert!(!frame.escapes);
        assert_eq!(
            frame.slots[&4],
            Slot {
                width: 4,
                reads: 1,
                writes: 1,
                uniform: Some((4, ValType::I32)),
                mixed: false,
                indirect: false,
            }
        );
    }

    #[test]
    fn an_epilogue_that_restores_the_wrong_amount_is_an_escape() {
        // Off by a frame size means the stack pointer is left somewhere else,
        // and whatever this function is doing is not the shape assumed here.
        let mut body = frame_prologue(16, 0);
        body.extend(frame_epilogue(32, 0));
        assert!(frame_of(body).escapes);
    }

    #[test]
    fn an_instruction_the_walk_does_not_model_ends_it() {
        let mut body = frame_prologue(16, 0);
        body.push(Op::LocalGet(0));
        // A call_indirect naming a type that does not exist: the walk cannot
        // know the arity, so it must stop rather than lose track of the stack.
        body.push(Op::CallIndirect { type_index: 99 });
        assert!(frame_of(body).escapes);
    }

    #[test]
    fn a_prologue_written_with_a_set_and_a_get_is_recognised() {
        let mut body = vec![
            Op::GlobalGet(0),
            Op::I32Const(64),
            Op::Num(NumOp::I32Sub),
            Op::LocalSet(2),
            Op::LocalGet(2),
            Op::GlobalSet(0),
        ];
        body.extend([Op::LocalGet(2), Op::I32Const(5), store(8)]);
        body.extend(frame_epilogue(64, 2));
        let frame = frame_of(body);
        assert_eq!(frame.size, 64);
        assert_eq!(frame.base_local, 2);
        assert!(!frame.escapes);
        assert_eq!(frame.slots[&8].writes, 1);
    }

    #[test]
    fn the_walk_keeps_its_footing_through_the_rest_of_the_instruction_set() {
        // None of these involve the frame address, and all of them have to
        // leave the abstract stack balanced — if the arity were wrong, a later
        // `local.get $base` would be popped by the wrong instruction and an
        // escape would go unnoticed.
        let mut body = frame_prologue(32, 0);
        body.extend([
            Op::Nop,
            Op::I64Const(1),
            Op::Drop,
            Op::F32Const(1.0),
            Op::Drop,
            Op::F64Const(1.0),
            Op::Drop,
            Op::I32Const(2),
            Op::I32Const(3),
            Op::Num(NumOp::I32Add),
            Op::Drop,
            Op::MemorySize,
            Op::Drop,
            Op::I32Const(1),
            Op::MemoryGrow,
            Op::Drop,
            Op::I32Const(0),
            Op::I32Const(0),
            Op::I32Const(0),
            Op::MemoryFill,
            Op::I32Const(0),
            Op::I32Const(0),
            Op::I32Const(0),
            Op::MemoryCopy,
            Op::I32Const(0),
            Op::I32Const(0),
            Op::I32Const(0),
            Op::MemoryInit(0),
            Op::DataDrop(0),
            Op::I32Const(1),
            Op::I32Const(2),
            Op::I32Const(3),
            Op::Select,
            Op::Drop,
            Op::I32Const(7),
            Op::LocalTee(4),
            Op::Drop,
            Op::I32Const(9),
            Op::GlobalSet(0),
        ]);
        // And after all of that, an access still lands in the right slot.
        body.extend([Op::LocalGet(0), Op::I32Const(11), store(4)]);
        let frame = frame_of(body);
        assert_eq!(frame.slots[&4].writes, 1, "{:?}", frame.slots);
    }

    #[test]
    fn eight_byte_accesses_are_recorded_as_eight_bytes() {
        let mut body = frame_prologue(32, 0);
        body.extend([
            Op::LocalGet(0),
            Op::I64Const(1),
            Op::Store {
                kind: crate::module::StoreKind::I64,
                mem: crate::module::MemArg { offset: 0 },
            },
            Op::LocalGet(0),
            Op::Load {
                kind: crate::module::LoadKind::F64,
                mem: crate::module::MemArg { offset: 8 },
            },
            Op::Drop,
        ]);
        let frame = frame_of(body);
        assert_eq!(frame.slots[&0].width, 8);
        assert_eq!(frame.slots[&8].width, 8);
    }

    #[test]
    fn a_call_to_a_function_that_is_not_there_ends_the_walk() {
        // The arity is unknown, so the stack cannot be kept straight past it.
        let mut body = frame_prologue(16, 0);
        body.push(Op::Call(77));
        assert!(frame_of(body).escapes);
    }

    #[test]
    fn a_call_that_does_not_take_the_frame_address_is_not_an_escape() {
        let mut module = module_with_globals(1, true);
        module.exports.push(crate::module::Export {
            name: "__stack_pointer".into(),
            kind: ExportKind::Global,
            index: 0,
        });
        module.types = vec![
            FuncType::default(),
            FuncType {
                params: vec![ValType::I32],
                results: vec![ValType::I32],
            },
        ];
        let mut body = frame_prologue(16, 0);
        body.extend([Op::I32Const(5), Op::Call(1), Op::Drop]);
        body.extend([Op::LocalGet(0), Op::I32Const(1), store(0)]);
        let mut module = with_bodies(module, vec![body, Vec::new()]);
        module.funcs[1].type_index = 1;

        let frame = analyse(&module).frames[&0].clone();
        assert!(!frame.escapes, "{frame:?}");
        assert_eq!(frame.slots[&0].writes, 1);
    }

    #[test]
    fn an_indirect_call_taking_the_frame_address_is_an_escape() {
        let mut module = module_with_globals(1, true);
        module.exports.push(crate::module::Export {
            name: "__stack_pointer".into(),
            kind: ExportKind::Global,
            index: 0,
        });
        module.types = vec![
            FuncType::default(),
            FuncType {
                params: vec![ValType::I32],
                results: Vec::new(),
            },
        ];
        let mut body = frame_prologue(16, 0);
        body.extend([
            Op::LocalGet(0),
            Op::I32Const(0),
            Op::CallIndirect { type_index: 1 },
        ]);
        let module = with_bodies(module, vec![body]);
        assert!(analyse(&module).frames[&0].escapes);
    }

    #[test]
    fn a_prologue_that_is_not_one_is_refused_at_every_step() {
        let cases: Vec<Vec<Op>> = vec![
            // Does not start with the stack pointer.
            vec![Op::I32Const(16), Op::Num(NumOp::I32Sub)],
            // Reads a different global.
            vec![
                Op::GlobalGet(1),
                Op::I32Const(16),
                Op::Num(NumOp::I32Sub),
                Op::LocalTee(0),
            ],
            // Reserves nothing.
            vec![
                Op::GlobalGet(0),
                Op::I32Const(0),
                Op::Num(NumOp::I32Sub),
                Op::LocalTee(0),
            ],
            // Adds instead of subtracting: that is an epilogue.
            vec![
                Op::GlobalGet(0),
                Op::I32Const(16),
                Op::Num(NumOp::I32Add),
                Op::LocalTee(0),
            ],
            // Keeps the result nowhere this analysis can name.
            vec![
                Op::GlobalGet(0),
                Op::I32Const(16),
                Op::Num(NumOp::I32Sub),
                Op::GlobalSet(0),
            ],
            // Runs out before it says where the address went.
            vec![Op::GlobalGet(0), Op::I32Const(16), Op::Num(NumOp::I32Sub)],
        ];
        for (at, body) in cases.into_iter().enumerate() {
            let module = framed_module(body);
            assert!(
                analyse(&module).frames.is_empty(),
                "case {at} was read as a prologue"
            );
        }
    }

    #[test]
    fn a_leaf_prologue_is_recognised_and_marked_unpublished() {
        // No `global.set`: clang omits it for a function that calls nothing.
        let mut body = vec![
            Op::GlobalGet(0),
            Op::I32Const(16),
            Op::Num(NumOp::I32Sub),
            Op::LocalSet(1),
        ];
        body.extend([Op::LocalGet(1), Op::I32Const(3), store(0)]);
        let frame = frame_of(body);
        assert!(!frame.publishes);
        assert_eq!(frame.base_local, 1);
        assert_eq!(frame.slots[&0].writes, 1);

        // The `local.tee` spelling of the same thing.
        let mut body = vec![
            Op::GlobalGet(0),
            Op::I32Const(16),
            Op::Num(NumOp::I32Sub),
            Op::LocalTee(1),
            Op::Drop,
        ];
        body.extend([Op::LocalGet(1), Op::I32Const(3), store(0)]);
        let frame = frame_of(body);
        assert!(!frame.publishes);
    }

    #[test]
    fn a_published_prologue_says_so() {
        let mut body = frame_prologue(16, 0);
        body.extend(frame_epilogue(16, 0));
        assert!(frame_of(body).publishes);
    }

    // ---- trampolines ----

    /// A module with the three things a trampoline needs: a stack pointer, an
    /// exported `setThrew`, and a type for what the table holds.
    fn trampoline_module(import_params: Vec<ValType>, import_results: Vec<ValType>) -> Module {
        let mut module = module_with_globals(1, true);
        module.exports.push(crate::module::Export {
            name: "__stack_pointer".into(),
            kind: ExportKind::Global,
            index: 0,
        });
        module.exports.push(crate::module::Export {
            name: "setThrew".into(),
            kind: ExportKind::Func,
            index: 1,
        });
        module.types = vec![
            FuncType {
                params: import_params,
                results: import_results,
            },
            // The callee's type: one i32, no result.
            FuncType {
                params: vec![ValType::I32],
                results: Vec::new(),
            },
        ];
        module.func_imports.push(crate::module::ImportedFunc {
            module: "env".into(),
            field: "invoke_vi".into(),
            type_index: 0,
        });
        // Two prologues, so the stack pointer is identified by use as well.
        with_bodies(module, vec![prologue(0), prologue(0)])
    }

    #[test]
    fn a_trampoline_is_found_by_its_name_and_checked_by_its_signature() {
        let module = trampoline_module(vec![ValType::I32, ValType::I32], Vec::new());
        let analysis = analyse(&module);
        assert_eq!(analysis.invokes.len(), 1);
        assert_eq!(analysis.invokes[0].import, 0);
        assert_eq!(
            analysis.invokes[0].callee_type, 1,
            "the type without the index"
        );
        assert_eq!(analysis.set_threw, Some(1));
    }

    #[test]
    fn an_invoke_with_no_leading_table_index_is_not_one() {
        // The name matches and the signature does not: nothing to dispatch.
        let module = trampoline_module(Vec::new(), Vec::new());
        assert!(analyse(&module).invokes.is_empty());
        // A first parameter that is not an index either.
        let module = trampoline_module(vec![ValType::F64, ValType::I32], Vec::new());
        assert!(analyse(&module).invokes.is_empty());
    }

    #[test]
    fn an_invoke_whose_callee_type_the_module_lacks_is_left_alone() {
        // `(i32, i64) -> ()` would dispatch to `(i64) -> ()`, and the module
        // declares no such type — so there is no dispatcher to call.
        let module = trampoline_module(vec![ValType::I32, ValType::I64], Vec::new());
        assert!(analyse(&module).invokes.is_empty());
    }

    #[test]
    fn an_invoke_whose_type_index_is_missing_is_left_alone() {
        let mut module = trampoline_module(vec![ValType::I32, ValType::I32], Vec::new());
        module.func_imports[0].type_index = 99;
        assert!(analyse(&module).invokes.is_empty());
    }

    #[test]
    fn an_import_that_is_not_an_invoke_is_never_taken_for_one() {
        let mut module = trampoline_module(vec![ValType::I32, ValType::I32], Vec::new());
        module.func_imports[0].field = "call_something".into();
        assert!(analyse(&module).invokes.is_empty());
    }

    #[test]
    fn without_set_threw_no_trampoline_is_generated() {
        let mut module = trampoline_module(vec![ValType::I32, ValType::I32], Vec::new());
        module.exports.retain(|export| export.name != "setThrew");
        let analysis = analyse(&module);
        assert!(analysis.set_threw.is_none());
        assert!(analysis.invokes.is_empty());
    }

    #[test]
    fn the_underscored_spelling_of_set_threw_counts_too() {
        let mut module = trampoline_module(vec![ValType::I32, ValType::I32], Vec::new());
        for export in &mut module.exports {
            if export.name == "setThrew" {
                export.name = "_setThrew".into();
            }
        }
        assert_eq!(analyse(&module).set_threw, Some(1));
    }

    #[test]
    fn a_global_exported_as_set_threw_is_not_a_function() {
        let mut module = trampoline_module(vec![ValType::I32, ValType::I32], Vec::new());
        for export in &mut module.exports {
            if export.name == "setThrew" {
                export.kind = ExportKind::Global;
            }
        }
        assert!(analyse(&module).set_threw.is_none());
    }

    #[test]
    fn frames_are_not_looked_for_without_a_stack_pointer() {
        // No stack pointer identified means no basis for calling anything a
        // frame — the prologue shape alone is not enough.
        let module = with_bodies(module_with_globals(1, false), vec![frame_prologue(16, 0)]);
        let analysis = analyse(&module);
        assert!(analysis.stack_pointer.is_none());
        assert!(analysis.frames.is_empty());
    }

    #[test]
    fn a_function_with_two_unique_messages_keeps_the_more_specific_one() {
        // Both belong to it alone; the longer identifier says more.
        let module = Module {
            datas: vec![DataSegment {
                file_offset: 0,
                offset: Some(ConstExpr::I32(0)),
                bytes: b"short_one: x\0handle_incoming_signalling_offer: y\0".to_vec(),
            }],
            types: vec![FuncType::default()],
            ..Module::default()
        };
        let module = with_bodies(
            module,
            vec![vec![Op::I32Const(0), Op::Drop, Op::I32Const(13), Op::Drop]],
        );
        let derived = analyse(&module).derived_names;
        assert_eq!(
            derived[&0].name, "handle_incoming_signalling_offer",
            "{derived:?}"
        );
    }

    #[test]
    fn static_reads_skip_segments_that_cannot_answer() {
        // Three ways a segment is not the one holding an address: it is placed
        // by a global, it is passive with no known placement, and it sits
        // after the address being asked about.
        let module = Module {
            datas: vec![
                DataSegment {
                    file_offset: 0,
                    offset: Some(ConstExpr::GlobalGet(0)),
                    bytes: vec![1, 2, 3, 4],
                },
                DataSegment {
                    file_offset: 0,
                    offset: None,
                    bytes: vec![5, 6, 7, 8],
                },
                DataSegment {
                    file_offset: 0,
                    offset: Some(ConstExpr::I32(4096)),
                    bytes: vec![9, 0, 0, 0],
                },
            ],
            ..Module::default()
        };
        let placements = Default::default();
        // Before every segment that could answer.
        assert_eq!(static_i32(&module, &placements, 0), None);
        // And the one that can.
        assert_eq!(static_i32(&module, &placements, 4096), Some(9));
    }

    #[test]
    fn a_segment_placed_by_a_global_offset_is_not_read_from() {
        // Only an imported global can mean this, and its value is not known
        // here — so nothing in that segment resolves.
        let module = Module {
            datas: vec![DataSegment {
                file_offset: 0,
                offset: Some(ConstExpr::GlobalGet(0)),
                bytes: b"unreachable_text\0".to_vec(),
            }],
            ..Module::default()
        };
        assert_eq!(static_text(&module, 0), None);
    }

    #[test]
    fn an_instruction_outside_the_walks_vocabulary_ends_it() {
        // An atomic: the walk models plain loads and stores, not these, and
        // stopping is the only safe answer to an instruction it cannot account
        // for on the stack.
        let mut body = frame_prologue(16, 0);
        body.push(Op::Atomic {
            op: crate::module::Atomic {
                kind: crate::module::AtomicKind::Load,
                ty: ValType::I32,
                width: 4,
            },
            mem: crate::module::MemArg { offset: 0 },
        });
        assert!(frame_of(body).escapes);
    }

    #[test]
    fn a_prologue_whose_second_instruction_is_not_a_size_is_not_one() {
        let module = framed_module(vec![Op::GlobalGet(0), Op::Nop, Op::LocalTee(0)]);
        assert!(analyse(&module).frames.is_empty());
    }

    #[test]
    fn a_call_that_is_not_a_registration_is_walked_past() {
        // The embind scan has to keep going through every other call in the
        // module, of which there are rather more.
        let mut module = Module {
            types: vec![
                FuncType::default(),
                FuncType {
                    params: vec![ValType::I32; 7],
                    results: Vec::new(),
                },
            ],
            datas: vec![DataSegment {
                file_offset: 0,
                offset: Some(ConstExpr::I32(0)),
                bytes: b"registered_name\0".to_vec(),
            }],
            ..Module::default()
        };
        module.func_imports.push(crate::module::ImportedFunc {
            module: "env".into(),
            field: "_embind_register_function".into(),
            type_index: 1,
        });
        module.func_imports.push(crate::module::ImportedFunc {
            module: "env".into(),
            field: "something_else".into(),
            type_index: 0,
        });
        let body = vec![
            Op::Call(1),
            Op::I32Const(0),
            Op::I32Const(0),
            Op::I32Const(0),
            Op::I32Const(0),
            Op::I32Const(0),
            Op::I32Const(0),
            Op::I32Const(0),
            Op::Call(0),
            Op::Call(1),
        ];
        let module = with_bodies(module, vec![body]);
        let registrations = analyse(&module).registrations;
        assert_eq!(registrations.len(), 1, "{registrations:?}");
        assert_eq!(registrations[0].name.as_deref(), Some("registered_name"));
    }

    #[test]
    fn a_prologue_that_never_stores_back_is_not_one() {
        // `global.get; i32.const; i32.sub` and then something else entirely:
        // arithmetic on a global, not a frame being reserved. It is the same
        // shape a leaf function's prologue has, which is why the leaf spelling
        // counts as evidence only when the address is then used to reach
        // memory — and nothing here ever is.
        let body = vec![
            Op::GlobalGet(0),
            Op::I32Const(32),
            Op::Num(NumOp::I32Sub),
            Op::LocalSet(0),
            Op::LocalSet(1),
            Op::LocalSet(2),
        ];
        let module = with_bodies(module_with_globals(1, true), vec![body.clone(), body]);
        assert!(analyse(&module).stack_pointer.is_none());
    }

    #[test]
    fn a_body_too_short_to_be_a_prologue_is_not_one() {
        let module = with_bodies(
            module_with_globals(1, true),
            vec![
                vec![Op::GlobalGet(0)],
                vec![Op::GlobalGet(0), Op::I32Const(4)],
            ],
        );
        assert!(analyse(&module).stack_pointer.is_none());
    }

    #[test]
    fn a_length_that_runs_past_the_segment_is_refused() {
        let module = module_with_data(0, b"short");
        assert_eq!(static_text_of_length(&module, 0, 50), None);
        // And a length that is not a length at all.
        assert_eq!(static_text_of_length(&module, 0, 0), None);
        assert_eq!(static_text_of_length(&module, 0, 100_000), None);
    }

    #[test]
    fn a_length_that_covers_only_binary_is_refused() {
        let module = module_with_data(0, &[0x01, 0x02, 0x03, 0x04, 0x05]);
        assert_eq!(static_text_of_length(&module, 0, 4), None);
    }

    #[test]
    fn a_length_shorter_than_a_name_is_refused() {
        let module = module_with_data(0, b"abcdef");
        assert_eq!(static_text_of_length(&module, 0, 2), None);
        assert_eq!(
            static_text_of_length(&module, 0, 6).as_deref(),
            Some("abcdef")
        );
    }

    #[test]
    fn a_very_long_slice_is_cut_like_a_very_long_string() {
        let bytes = "y".repeat(300).into_bytes();
        let module = module_with_data(0, &bytes);
        let text = static_text_of_length(&module, 0, 300).expect("text");
        assert!(text.ends_with('…'), "{text}");
    }

    #[test]
    fn an_address_outside_every_segment_reads_back_as_nothing() {
        let module = module_with_data(1024, b"hello\0");
        assert_eq!(static_text(&module, 0), None);
        assert_eq!(static_text(&module, 2048), None);
        // Passive segments are not at any address until `memory.init` puts them
        // somewhere, so they cannot answer this question.
        let module = Module {
            datas: vec![DataSegment {
                file_offset: 0,
                offset: None,
                bytes: b"hello\0".to_vec(),
            }],
            ..Module::default()
        };
        assert_eq!(static_text(&module, 0), None);
    }

    // ---- the C++ RTTI, laid out by hand ----

    /// Lays out an image the way the Itanium ABI does, so the rules below are
    /// tested against a layout rather than against a module nobody can edit.
    ///
    /// The base is 1024 because address 0 is not a `type_info` anywhere — a null
    /// vptr is how "no kind" is spelled, and starting at 0 would make the first
    /// object indistinguishable from it.
    struct Image {
        bytes: Vec<u8>,
    }

    impl Image {
        const BASE: i32 = 1024;

        fn new() -> Self {
            Self { bytes: Vec::new() }
        }

        fn here(&self) -> i32 {
            Self::BASE + self.bytes.len() as i32
        }

        fn word(&mut self, value: i32) -> i32 {
            let at = self.here();
            self.bytes.extend_from_slice(&value.to_le_bytes());
            at
        }

        fn text(&mut self, value: &str) -> i32 {
            let at = self.here();
            self.bytes.extend_from_slice(value.as_bytes());
            self.bytes.push(0);
            while !self.bytes.len().is_multiple_of(4) {
                self.bytes.push(0);
            }
            at
        }

        /// Somewhere for a `type_info`'s vptr to point at. It has to be inside
        /// the image — a vptr that addresses nothing is not one — and two zero
        /// words are not a `type_info` themselves.
        fn vtable(&mut self) -> i32 {
            let at = self.word(0);
            self.word(0);
            at
        }

        /// `{vptr, name}` — a class with no base, which is what Itanium's
        /// `__class_type_info` is.
        fn plain(&mut self, kind: i32, name: &str) -> i32 {
            let name_at = self.text(name);
            let at = self.word(kind);
            self.word(name_at);
            at
        }

        /// `{vptr, name, base}` — `__si_class_type_info`.
        fn derived(&mut self, kind: i32, name: &str, base: i32) -> i32 {
            let at = self.plain(kind, name);
            self.word(base);
            at
        }

        fn module(self) -> Module {
            module_with_data(Self::BASE, &self.bytes)
        }
    }

    fn class_names(module: &Module) -> Vec<String> {
        let (classes, _) = classes(module, &Default::default());
        classes.into_iter().map(|class| class.name).collect()
    }

    #[test]
    fn a_vtable_pointer_too_few_type_infos_share_is_not_one() {
        // Two candidates agreeing on a word is two words that lined up. The
        // floor is what separates that from a `__class_type_info` vtable, and
        // it is the whole defence against naming a class that is not there.
        let mut image = Image::new();
        let kind = image.vtable();
        image.plain(kind, "5Alpha");
        image.plain(kind, "4Beta");
        assert_eq!(class_names(&image.module()), Vec::<String>::new());

        let mut image = Image::new();
        let kind = image.vtable();
        image.plain(kind, "5Alpha");
        image.plain(kind, "4Beta");
        image.plain(kind, "5Gamma");
        assert_eq!(class_names(&image.module()), ["Alpha", "Beta", "Gamma"]);
    }

    #[test]
    fn a_base_a_confirmed_class_names_is_a_class_the_count_missed() {
        // Three derived classes confirm their kind; the base each of them points
        // at has a kind of its own that nothing else shares, so only the base
        // pointers can name it. That is a statement the module makes, not one
        // this inferred.
        let mut image = Image::new();
        let (si, alone) = (image.vtable(), image.vtable());
        let base = image.plain(alone, "5Shape");
        for name in ["6Square", "6Circle", "9Rectangle"] {
            image.derived(si, name, base);
        }
        let module = image.module();
        let (classes, evidence) = classes(&module, &Default::default());

        let names: Vec<&str> = classes.iter().map(|class| class.name.as_str()).collect();
        assert_eq!(names, ["Shape", "Square", "Circle", "Rectangle"]);
        assert_eq!(evidence.by_base, 1, "one of them the count did not reach");
        assert_eq!(evidence.kinds, 1, "and only one kind was ever confirmed");
        let square = classes
            .iter()
            .find(|class| class.name == "Square")
            .expect("declared");
        assert_eq!(square.base, Some(base));
        let shape = classes
            .iter()
            .find(|class| class.name == "Shape")
            .expect("named by its derived classes");
        assert_eq!(
            shape.base, None,
            "one hop: an admitted class's own third word is not read as a base"
        );
    }

    #[test]
    fn a_kind_whose_members_carry_no_base_pointer_reads_none() {
        // Three two-word objects are packed, so each one's third word is the
        // next one's first. Reading that as a base would give every class in the
        // module a bogus parent, and the last one a parent past the segment.
        let mut image = Image::new();
        let kind = image.vtable();
        for name in ["5Alpha", "4Beta", "5Gamma"] {
            image.plain(kind, name);
        }
        let module = image.module();
        let (classes, evidence) = classes(&module, &Default::default());
        assert_eq!(classes.len(), 3);
        assert!(
            classes.iter().all(|class| class.base.is_none()),
            "{classes:?}"
        );
        assert_eq!(evidence.by_base, 0);
    }

    #[test]
    fn a_base_pointing_inside_its_own_object_is_not_a_base() {
        // A three-word `type_info` occupies twelve bytes, so a base pointer into
        // those twelve is the object overlapping itself. The three that do point
        // outside are enough to confirm the kind, which is what makes the fourth
        // a rejection rather than a group that never formed.
        let mut image = Image::new();
        let (si, alone) = (image.vtable(), image.vtable());
        let base = image.plain(alone, "5Shape");
        for name in ["6Square", "6Circle", "9Rectangle"] {
            image.derived(si, name, base);
        }
        let liar = image.here() + 8; // the object itself, past its own name
        image.derived(si, "5Wrong", liar);

        let module = image.module();
        let (classes, _) = classes(&module, &Default::default());
        let wrong = classes
            .iter()
            .find(|class| class.name == "Wrong")
            .expect("still a class: its kind was confirmed");
        assert_eq!(wrong.base, None, "but the word inside it is not a base");
    }

    #[test]
    fn a_mangled_type_reads_as_far_as_it_can_and_no_further() {
        // What it reads: source names, nesting, `St`, and a template argument
        // list elided as a whole.
        assert_eq!(
            demangle_type("20WasmShimErrorHandler"),
            (
                "WasmShimErrorHandler".to_string(),
                "WasmShimErrorHandler".to_string()
            )
        );
        assert_eq!(
            demangle_type("N10__cxxabiv120__si_class_type_infoE"),
            (
                "__cxxabiv1::__si_class_type_info".to_string(),
                "__si_class_type_info".to_string()
            )
        );
        let (name, short) =
            demangle_type("NSt3__212basic_stringIcNS_11char_traitsIcEENS_9allocatorIcEEEE");
        assert_eq!(name, "std::__2::basic_string<…>");
        assert_eq!(short, "basic_string", "the short name drops the arguments");

        // Skipping the argument list is where this can go wrong quietly. The
        // anonymous namespace is a *source name* holding an `N`, a substitution
        // holds base-36 digits, and `St` is two characters with no `_` — count
        // any of those as structure and the skip runs past the `E` that closes
        // the name, which reads as a demangling that worked.
        assert_eq!(
            demangle_type(
                "N5folly6detail30StaticSingletonManagerWithRtti3SrcINS_11ThreadLocalINS_20Single\
                 tonThreadLocalINS_12_GLOBAL__N_120BufferedRandomDeviceENS5_9RandomTagEvS7_E7Wr\
                 apperES7_vEES7_EE"
            )
            .0,
            "folly::detail::StaticSingletonManagerWithRtti::Src<…>"
        );
        assert_eq!(
            demangle_type("N5folly5tag_tIJvEEE").0,
            "folly::tag_t<…>",
            "`J` opens an argument pack"
        );
        assert_eq!(
            demangle_type("NSt3__210__function6__baseIFvmEEE").0,
            "std::__2::__function::__base<…>",
            "`F` opens a function type"
        );

        // And what it refuses. A substitution refers back to a component by
        // number; resolving one wrongly is a name that says something the
        // module did not, so the mangled string comes back untouched.
        for mangled in [
            "NS_9allocatorIS1_EE", // a substitution
            "PKc",                 // a pointer, not a class
            "N10__cxxabiv1",       // nesting that never closes
            "12Truncated",         // a length past the end of the string
            "IcE",                 // arguments with nothing to attach them to
            "3AbcE",               // an `E` with nothing open
        ] {
            assert_eq!(
                demangle_type(mangled),
                (mangled.to_string(), mangled.to_string()),
                "{mangled} is not something this can read"
            );
        }
    }

    #[test]
    fn a_type_info_whose_name_is_not_a_type_is_not_a_candidate() {
        // `_ZTS` holds a mangled type; a word pointing at ordinary text is a
        // word pointing at ordinary text.
        let mut image = Image::new();
        let kind = image.vtable();
        for name in ["hello, world", "GET /index.html", "%s: %d\n"] {
            image.plain(kind, name);
        }
        assert_eq!(class_names(&image.module()), Vec::<String>::new());
    }
}
