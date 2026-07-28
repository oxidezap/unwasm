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
    /// Where each passive data segment is placed, by segment index, when the
    /// module places it at a constant address.
    pub placements: std::collections::BTreeMap<u32, Placement>,
    /// Names guessed from the strings a function references, by function index.
    /// Only for functions the module did not name itself.
    pub derived_names: std::collections::BTreeMap<u32, DerivedName>,
    /// What the module registers with embind, in the order it registers it.
    pub registrations: Vec<Registration>,
}

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
    }
}

/// Which argument of each `_embind_register_*` holds the name.
///
/// From embind's own signatures. A registration not listed here is still
/// reported — that it happened is worth knowing — just without a name.
const EMBIND_NAME_ARGUMENT: &[(&str, usize)] = &[
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

    let mut registrations = Vec::new();
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
                    .and_then(|address| placed_text(module, placements, *address, None)),
                _ => None,
            };
            registrations.push(Registration {
                kind: (*field).to_string(),
                name,
            });
        }
    }
    registrations
}

/// Guesses names for unnamed functions from the messages they log.
///
/// Two rules keep this from inventing things:
///
/// - **The string must belong to one function.** A message in fifty functions
///   distinguishes none of them, and `index out of bounds` is in most of a Rust
///   module. Uniqueness is what makes the guess worth making.
/// - **The identifier must look like one.** A lowercase token with an
///   underscore in it, at the start of the message, followed by `:` or a space
///   — which is how a C or Rust log line names its function. Anything looser
///   would turn `error while parsing` into a function called `error`.
fn derive_names(
    module: &Module,
    placements: &std::collections::BTreeMap<u32, Placement>,
) -> std::collections::BTreeMap<u32, DerivedName> {
    let import_count = module.func_imports.len() as u32;

    // Which functions reference each candidate message.
    let mut owners: std::collections::BTreeMap<String, Vec<u32>> = Default::default();
    for (at, func) in module.funcs.iter().enumerate() {
        let index = import_count + at as u32;
        let mut seen: std::collections::BTreeSet<String> = Default::default();
        for (position, op) in func.body.iter().enumerate() {
            let Op::I32Const(address) = op else { continue };
            // The address a `memory.init` copies *to* is not a reference to the
            // text — it is the placement itself. Counting it would make every
            // string at the start of a segment look like it belonged to the
            // initialiser as well as to its real user, and a string in two
            // functions names neither.
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
                seen.insert(text);
            }
        }
        for text in seen {
            owners.entry(text).or_default().push(index);
        }
    }

    let mut names: std::collections::BTreeMap<u32, DerivedName> = Default::default();
    for (text, functions) in owners {
        // Shared by two functions is not evidence about either of them.
        let [function] = functions[..] else { continue };
        if module.func_name(function).is_some() {
            continue;
        }
        // `identifier_in` already accepted this text on the way in.
        let name = identifier_in(&text).unwrap_or_default();
        // A function referencing several unique messages keeps the first by
        // address order, which is stable across runs; a longer identifier is
        // preferred when they differ, being the more specific of the two.
        let candidate = DerivedName {
            name: name.to_string(),
            evidence: text,
        };
        match names.get(&function) {
            Some(existing) if existing.name.len() >= candidate.name.len() => {}
            _ => {
                names.insert(function, candidate);
            }
        }
    }
    names
}

/// The function name a log message starts with, if it starts with one.
fn identifier_in(text: &str) -> Option<&str> {
    let first = text.split([':', ' ', '(', ',']).next()?;
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
    };

    let mut stack: Vec<Tracked> = Vec::new();
    for op in &body[prologue.length..] {
        // Control flow ends what this walk can follow. If the frame address is
        // live across it, the analysis gives up rather than losing track of
        // where it went.
        if is_control_flow(op) {
            if stack.iter().any(|value| matches!(value, Tracked::Frame(_))) {
                frame.escapes = true;
                return Some(frame);
            }
            stack.clear();
            continue;
        }

        match op {
            Op::LocalGet(index) if *index == base_local => stack.push(Tracked::Frame(0)),
            Op::LocalGet(_) | Op::GlobalGet(_) => stack.push(Tracked::Other),
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
                        if matches!(left, Tracked::Frame(_)) || matches!(right, Tracked::Frame(_)) {
                            frame.escapes = true;
                        }
                        Tracked::Other
                    }
                };
                stack.push(sum);
            }

            Op::Load { kind, mem } => {
                let address = stack.pop().unwrap_or(Tracked::Other);
                if let Tracked::Frame(at) = address {
                    let offset = at + mem.offset as i32;
                    let slot = frame.slots.entry(offset).or_default();
                    slot.width = slot.width.max(width_of_load(*kind));
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
                    slot.width = slot.width.max(width_of_store(*kind));
                    slot.writes += 1;
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
                // The base being reassigned, or copied somewhere this walk no
                // longer follows.
                if matches!(value, Tracked::Frame(_)) || *index == base_local {
                    frame.escapes = true;
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

/// Matches the prologue: `global.get $sp; i32.const size; i32.sub`, then some
/// way of keeping the result.
///
/// Three spellings turn up in practice, and the third was a surprise:
///
/// ```wat
/// local.tee $base   global.set $sp     ;; the usual one
/// local.set $base   local.get $base   global.set $sp
/// local.set $base                      ;; a leaf function at -O0
/// ```
///
/// The last one never writes the stack pointer back. clang emits it for a
/// function that calls nothing: the space below the pointer is nobody else's
/// while it runs, so reserving it formally would be wasted work. Requiring the
/// `global.set` — which this did at first — misses every one of them.
fn read_prologue(body: &[Op], stack_pointer: u32) -> Option<Prologue> {
    let Op::GlobalGet(index) = body.first()? else {
        return None;
    };
    if *index != stack_pointer {
        return None;
    }
    let Op::I32Const(size) = body.get(1)? else {
        return None;
    };
    if *size <= 0 {
        return None;
    }
    if !matches!(body.get(2)?, Op::Num(num) if num.name() == "I32Sub") {
        return None;
    }

    match body.get(3)? {
        Op::LocalTee(base) => match body.get(4) {
            Some(Op::GlobalSet(sp)) if *sp == stack_pointer => Some(Prologue {
                size: *size,
                base_local: *base,
                length: 5,
                publishes: true,
            }),
            _ => Some(Prologue {
                size: *size,
                base_local: *base,
                length: 4,
                publishes: false,
            }),
        },
        Op::LocalSet(base) => {
            let published = matches!(
                (body.get(4), body.get(5)),
                (Some(Op::LocalGet(again)), Some(Op::GlobalSet(sp)))
                    if again == base && *sp == stack_pointer
            );
            Some(Prologue {
                size: *size,
                base_local: *base,
                length: if published { 6 } else { 4 },
                publishes: published,
            })
        }
        _ => None,
    }
}

/// The names a linker gives the stack pointer when it keeps names at all.
const STACK_POINTER_NAMES: &[&str] = &["__stack_pointer", "_stack_pointer", "stackPointer"];

fn find_stack_pointer(module: &Module) -> Option<StackPointer> {
    // An exported name settles it without any guessing.
    for export in &module.exports {
        if export.kind == ExportKind::Global && STACK_POINTER_NAMES.contains(&export.name.as_str())
        {
            return Some(StackPointer {
                global: export.index,
                evidence: Evidence::Exported,
            });
        }
    }

    // Otherwise, count prologues. A minified module keeps no names, but it
    // still has to reserve stack frames, and only one global is used that way.
    let mut counts = vec![0usize; module.globals.len()];
    for func in &module.funcs {
        if let Some(global) = prologue_global(&func.body)
            && let Some(count) = counts.get_mut(global as usize)
        {
            *count += 1;
        }
    }

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

/// Recognises `global.get G; i32.const N; i32.sub; ...; global.set G` at the
/// start of a function, and returns `G`.
///
/// The instructions between the subtraction and the store vary — clang emits a
/// `local.tee` to keep the frame address, and at `-O0` there is often nothing
/// at all — so the shape is matched at its two ends rather than exactly.
fn prologue_global(body: &[Op]) -> Option<u32> {
    let mut ops = body.iter();
    let global = match ops.next()? {
        Op::GlobalGet(index) => *index,
        _ => return None,
    };
    match ops.next()? {
        Op::I32Const(size) if *size > 0 => size,
        _ => return None,
    };
    if !matches!(ops.next()?, Op::Num(op) if op.name() == "I32Sub") {
        return None;
    }
    // The store back must be to the same global, and must come before anything
    // that could be a different function's business.
    for op in ops.take(3) {
        match op {
            Op::GlobalSet(index) if *index == global => return Some(global),
            Op::LocalTee(_) | Op::LocalSet(_) => {}
            _ => return None,
        }
    }
    None
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

fn static_text_placed(
    module: &Module,
    placements: &std::collections::BTreeMap<u32, Placement>,
    address: i32,
    length: Option<usize>,
) -> Option<String> {
    const SHORTEST: usize = 4;
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
                return (text.len() >= SHORTEST).then_some(text);
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
            Some(_) => (text.len() >= SHORTEST).then_some(text),
            // Ran off the end of the segment without a terminator.
            None => None,
        };
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::module::{DataSegment, Func, FuncType, GlobalDef, ValType};
    use crate::ops::NumOp;

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
                offset: Some(ConstExpr::I32(offset)),
                bytes: bytes.to_vec(),
            }],
            ..Module::default()
        }
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
                writes: 2
            }
        );
        assert_eq!(
            frame.slots[&16],
            Slot {
                width: 4,
                reads: 1,
                writes: 0
            }
        );
        assert_eq!(
            frame.slots[&20],
            Slot {
                width: 1,
                reads: 0,
                writes: 1
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
    fn copying_the_frame_address_to_another_local_is_an_escape() {
        let mut body = frame_prologue(16, 0);
        body.extend([Op::LocalGet(0), Op::LocalSet(3)]);
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
                writes: 1
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
    fn a_segment_placed_by_a_global_offset_is_not_read_from() {
        // Only an imported global can mean this, and its value is not known
        // here — so nothing in that segment resolves.
        let module = Module {
            datas: vec![DataSegment {
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
        // arithmetic on a global, not a frame being reserved.
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
                offset: None,
                bytes: b"hello\0".to_vec(),
            }],
            ..Module::default()
        };
        assert_eq!(static_text(&module, 0), None);
    }
}
