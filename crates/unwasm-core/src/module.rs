//! The module, decoded into something a backend can walk twice.
//!
//! wasmparser is event-driven: it hands over a section at a time and keeps
//! nothing. A code generator needs to look at a function, then at the type it
//! refers to, then back at the function, so this collects the parts into owned
//! structures first. The instruction set is narrowed on the way in — an opcode
//! that arrives here is one the backend knows how to write.

use crate::error::{Error, Result};
use crate::ops::{self, NumOp};

/// A wasm value type. The four the MVP defines; a reference or a vector comes
/// back as [`Error::Unsupported`] rather than as an approximation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ValType {
    /// 32-bit integer, signed or unsigned depending on the operator.
    I32,
    /// 64-bit integer.
    I64,
    /// 32-bit float.
    F32,
    /// 64-bit float.
    F64,
}

impl ValType {
    /// How the type is spelled in generated Rust.
    #[must_use]
    pub fn rust_name(self) -> &'static str {
        match self {
            Self::I32 => "i32",
            Self::I64 => "i64",
            Self::F32 => "f32",
            Self::F64 => "f64",
        }
    }

    /// The literal a local of this type is initialised with. wasm zeroes every
    /// local on entry, and so must the generated function.
    #[must_use]
    pub fn zero(self) -> &'static str {
        match self {
            Self::I32 | Self::I64 => "0",
            Self::F32 | Self::F64 => "0.0",
        }
    }

    fn lower(ty: wasmparser::ValType, location: &str) -> Result<Self> {
        Ok(match ty {
            wasmparser::ValType::I32 => Self::I32,
            wasmparser::ValType::I64 => Self::I64,
            wasmparser::ValType::F32 => Self::F32,
            wasmparser::ValType::F64 => Self::F64,
            wasmparser::ValType::V128 => {
                return Err(Error::unsupported("v128 (SIMD)", location.to_string()));
            }
            wasmparser::ValType::Ref(_) => {
                return Err(Error::unsupported("reference type", location.to_string()));
            }
        })
    }
}

/// A function signature.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FuncType {
    /// Parameter types, in order.
    pub params: Vec<ValType>,
    /// Result types. wasm allows several; the backend rejects more than one.
    pub results: Vec<ValType>,
}

/// The type a block, loop or `if` produces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockType {
    /// Produces nothing.
    Empty,
    /// Produces one value.
    Value(ValType),
    /// Refers to a signature in the type section — parameters, several results,
    /// or both.
    Func(u32),
}

/// The immediate of a load or store: a static offset and an alignment hint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemArg {
    /// Added to the dynamic address. Part of the address, not an index.
    pub offset: u64,
}

/// Which load, at which width, with which sign extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(missing_docs, reason = "one variant per wasm load opcode")]
pub enum LoadKind {
    I32,
    I64,
    F32,
    F64,
    I32Load8S,
    I32Load8U,
    I32Load16S,
    I32Load16U,
    I64Load8S,
    I64Load8U,
    I64Load16S,
    I64Load16U,
    I64Load32S,
    I64Load32U,
}

impl LoadKind {
    /// The type the load pushes.
    #[must_use]
    pub fn result(self) -> ValType {
        match self {
            Self::I32 | Self::I32Load8S | Self::I32Load8U | Self::I32Load16S | Self::I32Load16U => {
                ValType::I32
            }
            Self::I64
            | Self::I64Load8S
            | Self::I64Load8U
            | Self::I64Load16S
            | Self::I64Load16U
            | Self::I64Load32S
            | Self::I64Load32U => ValType::I64,
            Self::F32 => ValType::F32,
            Self::F64 => ValType::F64,
        }
    }
}

/// Which store, at which width.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(missing_docs, reason = "one variant per wasm store opcode")]
pub enum StoreKind {
    I32,
    I64,
    F32,
    F64,
    I32Store8,
    I32Store16,
    I64Store8,
    I64Store16,
    I64Store32,
}

/// What an atomic instruction does, apart from the width it does it at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AtomicKind {
    /// `*.atomic.load*`
    Load,
    /// `*.atomic.store*`
    Store,
    /// `*.atomic.rmw*.{add,sub,and,or,xor,xchg}`
    Rmw(RmwKind),
    /// `*.atomic.rmw*.cmpxchg*`
    Cmpxchg,
    /// `memory.atomic.wait32` / `wait64`
    Wait,
    /// `memory.atomic.notify`
    Notify,
}

/// The arithmetic in a read-modify-write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(missing_docs, reason = "one variant per wasm opcode suffix")]
pub enum RmwKind {
    Add,
    Sub,
    And,
    Or,
    Xor,
    Xchg,
}

impl RmwKind {
    /// The name of the matching runtime variant.
    #[must_use]
    pub fn rt_name(self) -> &'static str {
        match self {
            Self::Add => "Add",
            Self::Sub => "Sub",
            Self::And => "And",
            Self::Or => "Or",
            Self::Xor => "Xor",
            Self::Xchg => "Xchg",
        }
    }
}

/// An atomic memory instruction.
///
/// Atomics are the last thing between this and the modules that use threads,
/// and there are sixty-four of them — but they vary in only three ways: what
/// they do, the type they do it to, and how many bytes they touch. Modelling
/// that rather than the opcode list keeps the backend from growing sixty-four
/// arms.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Atomic {
    /// What it does.
    pub kind: AtomicKind,
    /// The value type: `i32` or `i64`. Also the type of the result.
    pub ty: ValType,
    /// Bytes accessed: 1, 2, 4 or 8. Narrower than the type means the value is
    /// truncated going in and zero-extended coming out.
    pub width: u32,
}

/// One instruction, narrowed to what the backend can emit.
///
/// Structure stays implicit, as it is in the binary: `Block`/`Loop`/`If` open a
/// region and `End` closes it. wasm is already structured, so there is no
/// control-flow graph to recover and none to get wrong — the nesting in the
/// bytes *is* the nesting in the source.
#[derive(Debug, Clone, PartialEq)]
#[allow(missing_docs, reason = "one variant per wasm opcode")]
pub enum Op {
    Unreachable,
    Nop,
    Block(BlockType),
    Loop(BlockType),
    If(BlockType),
    Else,
    End,
    Br(u32),
    BrIf(u32),
    BrTable {
        targets: Vec<u32>,
        default: u32,
    },
    Return,
    Call(u32),
    CallIndirect {
        type_index: u32,
    },
    Drop,
    Select,
    LocalGet(u32),
    LocalSet(u32),
    LocalTee(u32),
    GlobalGet(u32),
    GlobalSet(u32),
    I32Const(i32),
    I64Const(i64),
    F32Const(f32),
    F64Const(f64),
    Load {
        kind: LoadKind,
        mem: MemArg,
    },
    Store {
        kind: StoreKind,
        mem: MemArg,
    },
    MemorySize,
    MemoryGrow,
    MemoryCopy,
    MemoryFill,
    MemoryInit(u32),
    DataDrop(u32),
    Atomic {
        op: Atomic,
        mem: MemArg,
    },
    /// `atomic.fence`. Orders nothing that a single thread can observe.
    AtomicFence,
    /// Everything arithmetic, from [`crate::ops`].
    Num(NumOp),
}

/// A function defined in this module.
#[derive(Debug, Clone)]
pub struct Func {
    /// Index into [`Module::types`].
    pub type_index: u32,
    /// Declared locals, after the parameters, already flattened from the
    /// run-length encoding the binary uses.
    pub locals: Vec<ValType>,
    /// The body, without the implicit outer block.
    pub body: Vec<Op>,
    /// Where each operator of [`Self::body`] sits in the wasm file, as
    /// `(offset, length)`.
    ///
    /// Parallel to `body` and the same length. It exists because patching a
    /// module by hand means computing LEB encodings and counting bytes, and an
    /// arithmetic slip there looks exactly like "the code changed" — the
    /// pattern simply is not found. The decoder already knows every one of
    /// these numbers; carrying them costs eight bytes an instruction and
    /// removes the whole class of mistake.
    pub offsets: Vec<(u32, u32)>,
}

/// A function imported from the host.
#[derive(Debug, Clone)]
pub struct ImportedFunc {
    /// The two-level name: module, then field.
    pub module: String,
    /// The field name.
    pub field: String,
    /// Index into [`Module::types`].
    pub type_index: u32,
}

/// A constant expression, as far as one can appear in an initialiser.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ConstExpr {
    /// A literal.
    I32(i32),
    /// A literal.
    I64(i64),
    /// A literal.
    F32(f32),
    /// A literal.
    F64(f64),
    /// Another global's value. Legal for imported globals only, and rare.
    GlobalGet(u32),
}

/// A global variable defined in this module.
#[derive(Debug, Clone)]
pub struct GlobalDef {
    /// Its type.
    pub ty: ValType,
    /// Whether the module writes to it. The C stack pointer is the mutable one.
    pub mutable: bool,
    /// Its initial value.
    pub init: ConstExpr,
}

/// The module's memory.
#[derive(Debug, Clone)]
pub struct MemoryDef {
    /// Pages present at instantiation.
    pub min_pages: u32,
    /// Page ceiling, if declared.
    pub max_pages: Option<u32>,
    /// Declared shared. A shared memory is what a threaded module asks for, and
    /// it always declares a maximum — the engine must reserve the whole range
    /// up front because it cannot move memory another thread is reading.
    pub shared: bool,
    /// Where it comes from, when the host supplies it rather than the module.
    pub imported: Option<(String, String)>,
}

/// A data segment.
#[derive(Debug, Clone)]
pub struct DataSegment {
    /// Where it is copied at instantiation, or `None` if it is passive and only
    /// `memory.init` places it.
    pub offset: Option<ConstExpr>,
    /// The bytes.
    pub bytes: Vec<u8>,
    /// Where the bytes start in the wasm file.
    ///
    /// An address in the guest and an offset in the file are different numbers,
    /// and recovering one from the other by hand — subtracting a constant that
    /// happens to hold for one segment — is how a read lands past the end of
    /// the file. The decoder already knows this; carrying it is four bytes a
    /// segment.
    pub file_offset: u32,
}

/// An element segment: the function table's contents.
#[derive(Debug, Clone)]
pub struct ElemSegment {
    /// Where in the table it starts, or `None` if passive.
    pub offset: Option<ConstExpr>,
    /// Function indices, in table order.
    pub funcs: Vec<u32>,
}

/// What an export points at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportKind {
    /// A function.
    Func,
    /// The memory.
    Memory,
    /// A global.
    Global,
    /// The table.
    Table,
}

/// An exported name.
#[derive(Debug, Clone)]
pub struct Export {
    /// The name as the host sees it. Minified modules export single letters.
    pub name: String,
    /// What kind of thing it names.
    pub kind: ExportKind,
    /// Index into the matching index space.
    pub index: u32,
}

/// A decoded module.
#[derive(Debug, Clone, Default)]
pub struct Module {
    /// Signatures, indexed as the type section indexes them.
    pub types: Vec<FuncType>,
    /// Imported functions, which occupy the low function indices.
    pub func_imports: Vec<ImportedFunc>,
    /// Functions defined here, in order after the imports.
    pub funcs: Vec<Func>,
    /// The memory, if the module defines one.
    pub memory: Option<MemoryDef>,
    /// Globals defined here.
    pub globals: Vec<GlobalDef>,
    /// Data segments.
    pub datas: Vec<DataSegment>,
    /// Element segments.
    pub elems: Vec<ElemSegment>,
    /// Exports.
    pub exports: Vec<Export>,
    /// The start function, run at instantiation.
    pub start: Option<u32>,
    /// Function names from the name section, indexed by function index. Empty
    /// for a stripped module, which is the normal case for shipped code.
    pub func_names: Vec<(u32, String)>,
    /// Global names from the name section, indexed by global index.
    ///
    /// The same subsection as [`Self::func_names`] and just as absent from a
    /// shipped module, but it answers a question exports cannot: a linker names
    /// `__stack_pointer` here whether or not it exports it, and at that point
    /// the module has said which global holds the C stack rather than left it
    /// to be inferred from how the code uses it.
    pub global_names: Vec<(u32, String)>,
}

impl Module {
    /// Decodes a module.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Decode`] if the bytes are not a valid module, and
    /// [`Error::Unsupported`] for a construct this version does not model.
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        let mut module = Self::default();
        // Function bodies arrive in their own section, in the same order as the
        // type indices in the function section, so the indices are collected
        // first and paired up as the bodies arrive.
        let mut declared_types: Vec<u32> = Vec::new();
        let mut bodies_seen = 0usize;

        for payload in wasmparser::Parser::new(0).parse_all(bytes) {
            match payload? {
                wasmparser::Payload::TypeSection(reader) => {
                    for group in reader {
                        for subtype in group?.into_types() {
                            module.types.push(lower_subtype(&subtype)?);
                        }
                    }
                }
                wasmparser::Payload::ImportSection(reader) => {
                    // `into_imports` rather than iterating the section: the
                    // compact-imports encoding groups several imports under one
                    // module name, and iterating the section yields the groups.
                    for import in reader.into_imports() {
                        let import = import?;
                        let location = format!("import {}::{}", import.module, import.name);
                        match import.ty {
                            wasmparser::TypeRef::Func(type_index) => {
                                module.func_imports.push(ImportedFunc {
                                    module: import.module.to_string(),
                                    field: import.name.to_string(),
                                    type_index,
                                });
                            }
                            wasmparser::TypeRef::Memory(memory) => {
                                if module.memory.is_some() {
                                    return Err(Error::unsupported("multiple memories", location));
                                }
                                module.memory = Some(lower_memory_type(
                                    &memory,
                                    Some((import.module.to_string(), import.name.to_string())),
                                    &location,
                                )?);
                            }
                            wasmparser::TypeRef::Global(_) => {
                                return Err(Error::unsupported("imported global", location));
                            }
                            wasmparser::TypeRef::Table(_) => {
                                return Err(Error::unsupported("imported table", location));
                            }
                            wasmparser::TypeRef::Tag(_) => {
                                return Err(Error::unsupported("imported tag", location));
                            }
                            // From the custom-descriptors proposal. No
                            // toolchain we target emits it, and guessing that
                            // it behaves like `Func` is exactly the kind of
                            // guess this crate does not make.
                            other => {
                                return Err(Error::unsupported(
                                    format!("import of {other:?}"),
                                    location,
                                ));
                            }
                        }
                    }
                }
                wasmparser::Payload::FunctionSection(reader) => {
                    for type_index in reader {
                        declared_types.push(type_index?);
                    }
                }
                wasmparser::Payload::MemorySection(reader) => {
                    for memory in reader {
                        let memory = memory?;
                        if module.memory.is_some() {
                            return Err(Error::unsupported("multiple memories", "memory section"));
                        }
                        module.memory = Some(lower_memory_type(&memory, None, "memory section")?);
                    }
                }
                wasmparser::Payload::GlobalSection(reader) => {
                    for (index, global) in reader.into_iter().enumerate() {
                        let global = global?;
                        let location = format!("global #{index}");
                        module.globals.push(GlobalDef {
                            ty: ValType::lower(global.ty.content_type, &location)?,
                            mutable: global.ty.mutable,
                            init: lower_const_expr(&global.init_expr, &location)?,
                        });
                    }
                }
                wasmparser::Payload::ExportSection(reader) => {
                    for export in reader {
                        let export = export?;
                        let kind = match export.kind {
                            wasmparser::ExternalKind::Func => ExportKind::Func,
                            wasmparser::ExternalKind::Memory => ExportKind::Memory,
                            wasmparser::ExternalKind::Global => ExportKind::Global,
                            wasmparser::ExternalKind::Table => ExportKind::Table,
                            wasmparser::ExternalKind::Tag => {
                                return Err(Error::unsupported(
                                    "exported tag",
                                    format!("export {}", export.name),
                                ));
                            }
                            other => {
                                return Err(Error::unsupported(
                                    format!("export of {other:?}"),
                                    format!("export {}", export.name),
                                ));
                            }
                        };
                        module.exports.push(Export {
                            name: export.name.to_string(),
                            kind,
                            index: export.index,
                        });
                    }
                }
                wasmparser::Payload::StartSection { func, .. } => module.start = Some(func),
                wasmparser::Payload::DataSection(reader) => {
                    for (index, data) in reader.into_iter().enumerate() {
                        let data = data?;
                        let location = format!("data segment #{index}");
                        let offset = match data.kind {
                            wasmparser::DataKind::Active {
                                memory_index,
                                offset_expr,
                            } => {
                                if memory_index != 0 {
                                    return Err(Error::unsupported("multiple memories", location));
                                }
                                Some(lower_const_expr(&offset_expr, &location)?)
                            }
                            wasmparser::DataKind::Passive => None,
                        };
                        // wasmparser gives the record's range; the bytes are
                        // its tail, after the kind and the length.
                        let file_offset =
                            u32::try_from(data.range.end - data.data.len()).unwrap_or(0);
                        module.datas.push(DataSegment {
                            offset,
                            bytes: data.data.to_vec(),
                            file_offset,
                        });
                    }
                }
                wasmparser::Payload::ElementSection(reader) => {
                    for (index, element) in reader.into_iter().enumerate() {
                        let element = element?;
                        let location = format!("element segment #{index}");
                        let offset = match element.kind {
                            wasmparser::ElementKind::Active { offset_expr, .. } => {
                                Some(lower_const_expr(&offset_expr, &location)?)
                            }
                            wasmparser::ElementKind::Passive
                            | wasmparser::ElementKind::Declared => None,
                        };
                        let mut funcs = Vec::new();
                        match element.items {
                            wasmparser::ElementItems::Functions(items) => {
                                for func in items {
                                    funcs.push(func?);
                                }
                            }
                            wasmparser::ElementItems::Expressions(_, items) => {
                                // `ref.func` in an expression list is the same
                                // thing said differently; anything else names a
                                // value the table cannot hold for us yet.
                                for item in items {
                                    funcs.push(lower_ref_func(&item?, &location)?);
                                }
                            }
                        }
                        module.elems.push(ElemSegment { offset, funcs });
                    }
                }
                wasmparser::Payload::CodeSectionEntry(body) => {
                    let type_index = *declared_types.get(bodies_seen).ok_or_else(|| {
                        Error::invalid(
                            "more function bodies than the function section declared",
                            format!("function body #{bodies_seen}"),
                        )
                    })?;
                    let index = module.func_imports.len() + bodies_seen;
                    module.funcs.push(lower_body(&body, type_index, index)?);
                    bodies_seen += 1;
                }
                wasmparser::Payload::CustomSection(section) if section.name() == "name" => {
                    read_names(section.data(), section.data_offset(), &mut module);
                }
                _ => {}
            }
        }

        if bodies_seen != declared_types.len() {
            return Err(Error::invalid(
                format!(
                    "the function section declared {} functions but the code section has {bodies_seen}",
                    declared_types.len()
                ),
                "code section",
            ));
        }
        Ok(module)
    }

    /// The signature of a function by index, imports included.
    #[must_use]
    pub fn func_type(&self, index: u32) -> Option<&FuncType> {
        let type_index = match self.func_imports.get(index as usize) {
            Some(import) => import.type_index,
            None => {
                let defined = index as usize - self.func_imports.len();
                self.funcs.get(defined)?.type_index
            }
        };
        self.types.get(type_index as usize)
    }

    /// The name section's name for a function, if it kept one.
    #[must_use]
    pub fn func_name(&self, index: u32) -> Option<&str> {
        self.func_names
            .iter()
            .find(|(at, _)| *at == index)
            .map(|(_, name)| name.as_str())
    }
}

/// Reads a memory type, whether it was declared or imported.
///
/// A shared memory is accepted rather than refused. It says the module was
/// built for threads, and a decompilation of it runs one thread — which is a
/// limitation of the *execution*, recorded in the output, not a reason to
/// refuse to read the code. The instructions that can actually tell the
/// difference are `memory.atomic.wait*`, and those say so when they are
/// reached.
fn lower_memory_type(
    memory: &wasmparser::MemoryType,
    imported: Option<(String, String)>,
    location: &str,
) -> Result<MemoryDef> {
    if memory.memory64 {
        return Err(Error::unsupported("64-bit memory", location.to_string()));
    }
    Ok(MemoryDef {
        min_pages: u32::try_from(memory.initial)
            .map_err(|_| Error::invalid("memory minimum overflows u32", location.to_string()))?,
        max_pages: memory
            .maximum
            .map(|pages| u32::try_from(pages).unwrap_or(u32::MAX)),
        shared: memory.shared,
        imported,
    })
}

fn lower_subtype(subtype: &wasmparser::SubType) -> Result<FuncType> {
    let wasmparser::CompositeInnerType::Func(func) = &subtype.composite_type.inner else {
        return Err(Error::unsupported(
            "non-function type (struct, array or continuation)",
            "type section",
        ));
    };
    let mut lowered = FuncType::default();
    for param in func.params() {
        lowered.params.push(ValType::lower(*param, "type section")?);
    }
    for result in func.results() {
        lowered
            .results
            .push(ValType::lower(*result, "type section")?);
    }
    Ok(lowered)
}

fn lower_const_expr(expr: &wasmparser::ConstExpr<'_>, location: &str) -> Result<ConstExpr> {
    let mut reader = expr.get_operators_reader();
    let first = reader.read()?;
    let value = match first {
        wasmparser::Operator::I32Const { value } => ConstExpr::I32(value),
        wasmparser::Operator::I64Const { value } => ConstExpr::I64(value),
        wasmparser::Operator::F32Const { value } => ConstExpr::F32(f32::from_bits(value.bits())),
        wasmparser::Operator::F64Const { value } => ConstExpr::F64(f64::from_bits(value.bits())),
        wasmparser::Operator::GlobalGet { global_index } => ConstExpr::GlobalGet(global_index),
        other => {
            return Err(Error::unsupported(
                format!("{other:?} in a constant expression"),
                location.to_string(),
            ));
        }
    };
    // The expression has to be *only* that value. Reading the first operator
    // and stopping would turn `(i32.const 1) (i32.const 2) i32.add` into 1 —
    // a plausible number, at the wrong address, with nothing reported.
    match reader.read()? {
        wasmparser::Operator::End => Ok(value),
        other => Err(Error::unsupported(
            format!("{other:?} in a constant expression"),
            location.to_string(),
        )),
    }
}

fn lower_ref_func(expr: &wasmparser::ConstExpr<'_>, location: &str) -> Result<u32> {
    let mut reader = expr.get_operators_reader();
    match reader.read()? {
        wasmparser::Operator::RefFunc { function_index } => Ok(function_index),
        other => Err(Error::unsupported(
            format!("{other:?} in an element segment"),
            location.to_string(),
        )),
    }
}

fn lower_body(body: &wasmparser::FunctionBody<'_>, type_index: u32, index: usize) -> Result<Func> {
    let location = format!("function #{index}");
    let mut locals = Vec::new();
    for entry in body.get_locals_reader()? {
        let (count, ty) = entry?;
        let ty = ValType::lower(ty, &location)?;
        locals.extend(std::iter::repeat_n(ty, count as usize));
    }

    let mut ops = Vec::new();
    let mut starts = Vec::new();
    let reader = body.get_operators_reader()?;
    let end_of_body = reader.get_binary_reader().range().end;
    for entry in reader.into_iter_with_offsets() {
        let (operator, at) = entry?;
        ops.push(lower_op(&operator, &location)?);
        starts.push(at as u32);
    }
    // One operator ends where the next begins, and the last ends with the body.
    let offsets: Vec<(u32, u32)> = starts
        .iter()
        .enumerate()
        .map(|(at, start)| {
            let end = starts.get(at + 1).copied().unwrap_or(end_of_body as u32);
            (*start, end.saturating_sub(*start))
        })
        .collect();

    // The reader yields the body's final `End`; the backend supplies the
    // function's own return, so it is dropped here rather than special-cased
    // in three places later.
    let mut offsets = offsets;
    if ops.last() == Some(&Op::End) {
        ops.pop();
        offsets.pop();
    }

    Ok(Func {
        type_index,
        locals,
        body: ops,
        offsets,
    })
}

fn lower_block_type(ty: wasmparser::BlockType, location: &str) -> Result<BlockType> {
    Ok(match ty {
        wasmparser::BlockType::Empty => BlockType::Empty,
        wasmparser::BlockType::Type(ty) => BlockType::Value(ValType::lower(ty, location)?),
        wasmparser::BlockType::FuncType(index) => BlockType::Func(index),
    })
}

#[allow(
    clippy::too_many_lines,
    reason = "one arm per opcode; splitting it would only move the list"
)]
fn lower_op(operator: &wasmparser::Operator<'_>, location: &str) -> Result<Op> {
    use wasmparser::Operator as W;

    if let Some(num) = ops::lower(operator) {
        return Ok(Op::Num(num));
    }

    let op = match operator {
        W::Unreachable => Op::Unreachable,
        W::Nop => Op::Nop,
        W::Block { blockty } => Op::Block(lower_block_type(*blockty, location)?),
        W::Loop { blockty } => Op::Loop(lower_block_type(*blockty, location)?),
        W::If { blockty } => Op::If(lower_block_type(*blockty, location)?),
        W::Else => Op::Else,
        W::End => Op::End,
        W::Br { relative_depth } => Op::Br(*relative_depth),
        W::BrIf { relative_depth } => Op::BrIf(*relative_depth),
        W::BrTable { targets } => Op::BrTable {
            targets: targets
                .targets()
                .collect::<std::result::Result<Vec<_>, _>>()?,
            default: targets.default(),
        },
        W::Return => Op::Return,
        W::Call { function_index } => Op::Call(*function_index),
        W::CallIndirect {
            type_index,
            table_index,
        } => {
            if *table_index != 0 {
                return Err(Error::unsupported(
                    "call_indirect on a table other than 0",
                    location.to_string(),
                ));
            }
            Op::CallIndirect {
                type_index: *type_index,
            }
        }
        W::Drop => Op::Drop,
        W::Select => Op::Select,
        W::TypedSelect { .. } | W::TypedSelectMulti { .. } => Op::Select,
        W::LocalGet { local_index } => Op::LocalGet(*local_index),
        W::LocalSet { local_index } => Op::LocalSet(*local_index),
        W::LocalTee { local_index } => Op::LocalTee(*local_index),
        W::GlobalGet { global_index } => Op::GlobalGet(*global_index),
        W::GlobalSet { global_index } => Op::GlobalSet(*global_index),
        W::I32Const { value } => Op::I32Const(*value),
        W::I64Const { value } => Op::I64Const(*value),
        W::F32Const { value } => Op::F32Const(f32::from_bits(value.bits())),
        W::F64Const { value } => Op::F64Const(f64::from_bits(value.bits())),

        W::I32Load { memarg } => load(LoadKind::I32, memarg, location)?,
        W::I64Load { memarg } => load(LoadKind::I64, memarg, location)?,
        W::F32Load { memarg } => load(LoadKind::F32, memarg, location)?,
        W::F64Load { memarg } => load(LoadKind::F64, memarg, location)?,
        W::I32Load8S { memarg } => load(LoadKind::I32Load8S, memarg, location)?,
        W::I32Load8U { memarg } => load(LoadKind::I32Load8U, memarg, location)?,
        W::I32Load16S { memarg } => load(LoadKind::I32Load16S, memarg, location)?,
        W::I32Load16U { memarg } => load(LoadKind::I32Load16U, memarg, location)?,
        W::I64Load8S { memarg } => load(LoadKind::I64Load8S, memarg, location)?,
        W::I64Load8U { memarg } => load(LoadKind::I64Load8U, memarg, location)?,
        W::I64Load16S { memarg } => load(LoadKind::I64Load16S, memarg, location)?,
        W::I64Load16U { memarg } => load(LoadKind::I64Load16U, memarg, location)?,
        W::I64Load32S { memarg } => load(LoadKind::I64Load32S, memarg, location)?,
        W::I64Load32U { memarg } => load(LoadKind::I64Load32U, memarg, location)?,

        W::I32Store { memarg } => store(StoreKind::I32, memarg, location)?,
        W::I64Store { memarg } => store(StoreKind::I64, memarg, location)?,
        W::F32Store { memarg } => store(StoreKind::F32, memarg, location)?,
        W::F64Store { memarg } => store(StoreKind::F64, memarg, location)?,
        W::I32Store8 { memarg } => store(StoreKind::I32Store8, memarg, location)?,
        W::I32Store16 { memarg } => store(StoreKind::I32Store16, memarg, location)?,
        W::I64Store8 { memarg } => store(StoreKind::I64Store8, memarg, location)?,
        W::I64Store16 { memarg } => store(StoreKind::I64Store16, memarg, location)?,
        W::I64Store32 { memarg } => store(StoreKind::I64Store32, memarg, location)?,

        W::MemorySize { .. } => Op::MemorySize,
        W::MemoryGrow { .. } => Op::MemoryGrow,
        W::MemoryCopy { .. } => Op::MemoryCopy,
        W::MemoryFill { .. } => Op::MemoryFill,
        W::MemoryInit { data_index, .. } => Op::MemoryInit(*data_index),
        W::DataDrop { data_index } => Op::DataDrop(*data_index),

        other => {
            if let Some(atomic) = lower_atomic(other, location)? {
                return Ok(atomic);
            }
            return Err(Error::unsupported(
                format!("{other:?}"),
                location.to_string(),
            ));
        }
    };
    Ok(op)
}

/// Lowers the sixty-four atomic instructions.
///
/// Written out rather than generated: concatenating identifiers in a macro
/// needs a dependency, and this table is read far more often than it is
/// changed. Each line says the same three things — what the instruction does,
/// the type it does it to, and the bytes it touches.
fn lower_atomic(operator: &wasmparser::Operator<'_>, location: &str) -> Result<Option<Op>> {
    use wasmparser::Operator as W;

    let (memarg, kind, ty, width) = match operator {
        W::I32AtomicLoad { memarg } => (*memarg, AtomicKind::Load, ValType::I32, 4),
        W::I32AtomicLoad8U { memarg } => (*memarg, AtomicKind::Load, ValType::I32, 1),
        W::I32AtomicLoad16U { memarg } => (*memarg, AtomicKind::Load, ValType::I32, 2),
        W::I64AtomicLoad { memarg } => (*memarg, AtomicKind::Load, ValType::I64, 8),
        W::I64AtomicLoad8U { memarg } => (*memarg, AtomicKind::Load, ValType::I64, 1),
        W::I64AtomicLoad16U { memarg } => (*memarg, AtomicKind::Load, ValType::I64, 2),
        W::I64AtomicLoad32U { memarg } => (*memarg, AtomicKind::Load, ValType::I64, 4),
        W::I32AtomicStore { memarg } => (*memarg, AtomicKind::Store, ValType::I32, 4),
        W::I32AtomicStore8 { memarg } => (*memarg, AtomicKind::Store, ValType::I32, 1),
        W::I32AtomicStore16 { memarg } => (*memarg, AtomicKind::Store, ValType::I32, 2),
        W::I64AtomicStore { memarg } => (*memarg, AtomicKind::Store, ValType::I64, 8),
        W::I64AtomicStore8 { memarg } => (*memarg, AtomicKind::Store, ValType::I64, 1),
        W::I64AtomicStore16 { memarg } => (*memarg, AtomicKind::Store, ValType::I64, 2),
        W::I64AtomicStore32 { memarg } => (*memarg, AtomicKind::Store, ValType::I64, 4),
        W::I32AtomicRmwAdd { memarg } => (*memarg, AtomicKind::Rmw(RmwKind::Add), ValType::I32, 4),
        W::I32AtomicRmwSub { memarg } => (*memarg, AtomicKind::Rmw(RmwKind::Sub), ValType::I32, 4),
        W::I32AtomicRmwAnd { memarg } => (*memarg, AtomicKind::Rmw(RmwKind::And), ValType::I32, 4),
        W::I32AtomicRmwOr { memarg } => (*memarg, AtomicKind::Rmw(RmwKind::Or), ValType::I32, 4),
        W::I32AtomicRmwXor { memarg } => (*memarg, AtomicKind::Rmw(RmwKind::Xor), ValType::I32, 4),
        W::I32AtomicRmwXchg { memarg } => {
            (*memarg, AtomicKind::Rmw(RmwKind::Xchg), ValType::I32, 4)
        }
        W::I32AtomicRmwCmpxchg { memarg } => (*memarg, AtomicKind::Cmpxchg, ValType::I32, 4),
        W::I32AtomicRmw8AddU { memarg } => {
            (*memarg, AtomicKind::Rmw(RmwKind::Add), ValType::I32, 1)
        }
        W::I32AtomicRmw8SubU { memarg } => {
            (*memarg, AtomicKind::Rmw(RmwKind::Sub), ValType::I32, 1)
        }
        W::I32AtomicRmw8AndU { memarg } => {
            (*memarg, AtomicKind::Rmw(RmwKind::And), ValType::I32, 1)
        }
        W::I32AtomicRmw8OrU { memarg } => (*memarg, AtomicKind::Rmw(RmwKind::Or), ValType::I32, 1),
        W::I32AtomicRmw8XorU { memarg } => {
            (*memarg, AtomicKind::Rmw(RmwKind::Xor), ValType::I32, 1)
        }
        W::I32AtomicRmw8XchgU { memarg } => {
            (*memarg, AtomicKind::Rmw(RmwKind::Xchg), ValType::I32, 1)
        }
        W::I32AtomicRmw8CmpxchgU { memarg } => (*memarg, AtomicKind::Cmpxchg, ValType::I32, 1),
        W::I32AtomicRmw16AddU { memarg } => {
            (*memarg, AtomicKind::Rmw(RmwKind::Add), ValType::I32, 2)
        }
        W::I32AtomicRmw16SubU { memarg } => {
            (*memarg, AtomicKind::Rmw(RmwKind::Sub), ValType::I32, 2)
        }
        W::I32AtomicRmw16AndU { memarg } => {
            (*memarg, AtomicKind::Rmw(RmwKind::And), ValType::I32, 2)
        }
        W::I32AtomicRmw16OrU { memarg } => (*memarg, AtomicKind::Rmw(RmwKind::Or), ValType::I32, 2),
        W::I32AtomicRmw16XorU { memarg } => {
            (*memarg, AtomicKind::Rmw(RmwKind::Xor), ValType::I32, 2)
        }
        W::I32AtomicRmw16XchgU { memarg } => {
            (*memarg, AtomicKind::Rmw(RmwKind::Xchg), ValType::I32, 2)
        }
        W::I32AtomicRmw16CmpxchgU { memarg } => (*memarg, AtomicKind::Cmpxchg, ValType::I32, 2),
        W::I64AtomicRmwAdd { memarg } => (*memarg, AtomicKind::Rmw(RmwKind::Add), ValType::I64, 8),
        W::I64AtomicRmwSub { memarg } => (*memarg, AtomicKind::Rmw(RmwKind::Sub), ValType::I64, 8),
        W::I64AtomicRmwAnd { memarg } => (*memarg, AtomicKind::Rmw(RmwKind::And), ValType::I64, 8),
        W::I64AtomicRmwOr { memarg } => (*memarg, AtomicKind::Rmw(RmwKind::Or), ValType::I64, 8),
        W::I64AtomicRmwXor { memarg } => (*memarg, AtomicKind::Rmw(RmwKind::Xor), ValType::I64, 8),
        W::I64AtomicRmwXchg { memarg } => {
            (*memarg, AtomicKind::Rmw(RmwKind::Xchg), ValType::I64, 8)
        }
        W::I64AtomicRmwCmpxchg { memarg } => (*memarg, AtomicKind::Cmpxchg, ValType::I64, 8),
        W::I64AtomicRmw8AddU { memarg } => {
            (*memarg, AtomicKind::Rmw(RmwKind::Add), ValType::I64, 1)
        }
        W::I64AtomicRmw8SubU { memarg } => {
            (*memarg, AtomicKind::Rmw(RmwKind::Sub), ValType::I64, 1)
        }
        W::I64AtomicRmw8AndU { memarg } => {
            (*memarg, AtomicKind::Rmw(RmwKind::And), ValType::I64, 1)
        }
        W::I64AtomicRmw8OrU { memarg } => (*memarg, AtomicKind::Rmw(RmwKind::Or), ValType::I64, 1),
        W::I64AtomicRmw8XorU { memarg } => {
            (*memarg, AtomicKind::Rmw(RmwKind::Xor), ValType::I64, 1)
        }
        W::I64AtomicRmw8XchgU { memarg } => {
            (*memarg, AtomicKind::Rmw(RmwKind::Xchg), ValType::I64, 1)
        }
        W::I64AtomicRmw8CmpxchgU { memarg } => (*memarg, AtomicKind::Cmpxchg, ValType::I64, 1),
        W::I64AtomicRmw16AddU { memarg } => {
            (*memarg, AtomicKind::Rmw(RmwKind::Add), ValType::I64, 2)
        }
        W::I64AtomicRmw16SubU { memarg } => {
            (*memarg, AtomicKind::Rmw(RmwKind::Sub), ValType::I64, 2)
        }
        W::I64AtomicRmw16AndU { memarg } => {
            (*memarg, AtomicKind::Rmw(RmwKind::And), ValType::I64, 2)
        }
        W::I64AtomicRmw16OrU { memarg } => (*memarg, AtomicKind::Rmw(RmwKind::Or), ValType::I64, 2),
        W::I64AtomicRmw16XorU { memarg } => {
            (*memarg, AtomicKind::Rmw(RmwKind::Xor), ValType::I64, 2)
        }
        W::I64AtomicRmw16XchgU { memarg } => {
            (*memarg, AtomicKind::Rmw(RmwKind::Xchg), ValType::I64, 2)
        }
        W::I64AtomicRmw16CmpxchgU { memarg } => (*memarg, AtomicKind::Cmpxchg, ValType::I64, 2),
        W::I64AtomicRmw32AddU { memarg } => {
            (*memarg, AtomicKind::Rmw(RmwKind::Add), ValType::I64, 4)
        }
        W::I64AtomicRmw32SubU { memarg } => {
            (*memarg, AtomicKind::Rmw(RmwKind::Sub), ValType::I64, 4)
        }
        W::I64AtomicRmw32AndU { memarg } => {
            (*memarg, AtomicKind::Rmw(RmwKind::And), ValType::I64, 4)
        }
        W::I64AtomicRmw32OrU { memarg } => (*memarg, AtomicKind::Rmw(RmwKind::Or), ValType::I64, 4),
        W::I64AtomicRmw32XorU { memarg } => {
            (*memarg, AtomicKind::Rmw(RmwKind::Xor), ValType::I64, 4)
        }
        W::I64AtomicRmw32XchgU { memarg } => {
            (*memarg, AtomicKind::Rmw(RmwKind::Xchg), ValType::I64, 4)
        }
        W::I64AtomicRmw32CmpxchgU { memarg } => (*memarg, AtomicKind::Cmpxchg, ValType::I64, 4),
        W::MemoryAtomicNotify { memarg } => (*memarg, AtomicKind::Notify, ValType::I32, 4),
        W::MemoryAtomicWait32 { memarg } => (*memarg, AtomicKind::Wait, ValType::I32, 4),
        W::MemoryAtomicWait64 { memarg } => (*memarg, AtomicKind::Wait, ValType::I64, 8),
        // Nothing a single thread can observe is ordered by a fence, and a
        // decompilation that dropped it silently would still be wrong: it is
        // kept in the IR and emitted as a comment.
        W::AtomicFence => return Ok(Some(Op::AtomicFence)),
        _ => return Ok(None),
    };

    // One check for all sixty-six, rather than one per line. The table says
    // what each instruction is and nothing else, which is the only thing that
    // varies between them.
    check_memory_index(&memarg, location)?;
    Ok(Some(Op::Atomic {
        op: Atomic { kind, ty, width },
        mem: MemArg {
            offset: memarg.offset,
        },
    }))
}

fn load(kind: LoadKind, memarg: &wasmparser::MemArg, location: &str) -> Result<Op> {
    check_memory_index(memarg, location)?;
    Ok(Op::Load {
        kind,
        mem: MemArg {
            offset: memarg.offset,
        },
    })
}

fn store(kind: StoreKind, memarg: &wasmparser::MemArg, location: &str) -> Result<Op> {
    check_memory_index(memarg, location)?;
    Ok(Op::Store {
        kind,
        mem: MemArg {
            offset: memarg.offset,
        },
    })
}

fn check_memory_index(memarg: &wasmparser::MemArg, location: &str) -> Result<()> {
    if memarg.memory != 0 {
        return Err(Error::unsupported(
            "access to a memory other than 0",
            location.to_string(),
        ));
    }
    Ok(())
}

/// Reads the function and global names out of the name section.
///
/// A malformed name section is not a reason to refuse a module: names are a
/// convenience, and every shipped module has none at all. What it must not do
/// is invent one, so a subsection that fails to parse contributes nothing.
fn read_names(data: &[u8], offset: usize, module: &mut Module) {
    let reader = wasmparser::NameSectionReader::new(wasmparser::BinaryReader::new(data, offset));
    for subsection in reader {
        let into = match subsection {
            Ok(wasmparser::Name::Function(names)) => (names, &mut module.func_names),
            Ok(wasmparser::Name::Global(names)) => (names, &mut module.global_names),
            _ => continue,
        };
        let (names, out) = into;
        for naming in names {
            let Ok(naming) = naming else { continue };
            out.push((naming.index, naming.name.to_string()));
        }
    }
}
