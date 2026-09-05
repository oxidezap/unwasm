//! The command line.
//!
//! Argument parsing is a dozen lines here rather than a dependency: the tool
//! has two verbs, and a decompiler that takes ten seconds to rebuild is a
//! decompiler nobody runs twice.

use std::fmt::Write;
use std::path::Path;
use std::process::ExitCode;

use unwasm_core::{Module, codegen};

const USAGE: &str = "\
unwasm — a WebAssembly decompiler whose output compiles

usage:
  unwasm decompile <module.wasm> [-o <out>] [--level <n>] [--split <n>]
                   [--only <indices>] [--bare] [--spans]
                   [--reachable-from <indices>]
                   [--signatures <file>] [--stub-recognised]
                   [--instrument-stores] [--offsets]
  unwasm host      <module.wasm> [-o <host.rs>] [--defaults]
  unwasm table     <module.wasm> [--type <signature>]
  unwasm calls     <module.wasm> <index>
  unwasm signatures <module.wasm> [-o <sigs.txt>]
  unwasm classes   <module.wasm> [--methods]
  unwasm frames    <module.wasm> [--outside]
  unwasm data      <module.wasm> <address> [<length>]
  unwasm vtable    <module.wasm> <address|--class <name>> [--slots <n>]
  unwasm stores    <module.wasm> --offset <n> [--size <n>] [--kind <k>]
  unwasm bytes     <module.wasm> <offset> <length>
  unwasm constants <module.wasm> <value> [--data]
  unwasm patch     <module.wasm> <offset> <value> -o <patched.wasm>
  unwasm inspect   <module.wasm>

Every command takes the module first and its flags after it, and every one
answers `--help` on its own.

  -o <out>       a path ending in .rs writes one file; any other path is a
                 directory, written as mod.rs plus part0.rs, part1.rs, …
                 Without -o, the Rust goes to stdout.
  --split <n>    roughly how many lines to put in each part file. Implies a
                 directory. Without it, a directory gets the layout the
                 module's size calls for.
  --only <list>  decompile only these function indices. The rest keep their
                 signatures and become `unimplemented!()`, so the result still
                 compiles — for reading three functions out of thirteen
                 thousand without producing 365 MB.
  --with-callees  with --only: and everything they call directly
  --reachable-from <list>  decompile what these functions can reach, and stub
                 the rest — the closure over direct calls plus every table
                 entry an indirect call could land on
  --direct-only  with --reachable-from: follow direct calls only. Smaller and
                 incomplete; a stub it reaches says which function to add
  --level <n>    0 (default) is a faithful translation; 1 also turns the frame
                 slots it can place into Rust bindings, which stops the output
                 being byte-exact and says so at every function it did it to;
                 2 also names functions from the C++ RTTI the module carries,
                 which changes nothing it does
  --signatures <file>  name library code using a catalogue from `signatures`
  --stub-recognised  with --signatures: leave the bodies of the functions the
                 catalogue named out, keeping their names and signatures. For
                 reading, not running — anything that calls one stops there
  --instrument-stores  route every memory write through the watchpoint runtime,
                 so `instance.memory.watch(addr, len)` reports who wrote it
  --offsets      also write offsets.json: which wasm bytes made each line,
                 comma-separated

A directory output also gets `names.json`: every function's index, name, file,
line and table slots. That is the index to look things up in rather than
grepping the output.

`table` lists what the function table holds, slot by slot, with each entry's
signature. `call_indirect` takes a table index rather than a function index, so
this is what says which slot a call site reaches — and which slot a callback
has to go into. `--type \"(i32,i32,i32)->()\"` narrows it to one signature.

`signatures` writes a catalogue of fingerprint-to-name from a module that kept
its names, and `decompile --signatures <file>` uses one to name library code in
a module that did not. A fingerprint is the shape of a function's body with the
things that move between builds left out. It matches ~91% across builds of the
same toolchain and only a handful across different emscripten versions, so it
is worth generating from a build of your own rather than expecting it to
recognise someone else's.

`classes` reads the C++ RTTI: every class the module declares a `type_info`
for, its name as the compiler mangled it, and the functions its vtable holds.
That is a declaration rather than an inference — the ABI writes both down — and
it is what `decompile --level 2` names functions after. `--methods` lists the
vtable slot by slot.

`data` reads guest memory at an address rather than an offset into the file:
which segment covers it, the file offset of the byte, the hex, and the words as
u32 with the strings they point at. An address no segment covers reads as zero
at run time, and it says so.

`vtable` reads a C++ vtable slot by slot: table index, function, signature —
and marks the slots holding 0, which are pure virtual. A `call_indirect` on one
of those reaches table slot 0, mismatches its signature and traps, which from
the outside looks like the engine dying for no reason.

`stores` answers \"who writes offset N of this struct\", following the constant
displacements a function applies to its own parameters so a field at +846
written through `p - 8` as +854 is still found.

`constants` finds every site that pushes a value, with the file offset and the
encoded length of each — all of them, which a hand-counted subset is not. An
error code that occurs 481 times is not nine sites, and an account built on the
nine is guessing.

`patch` rewrites the constant at an offset, keeping the encoding the same
length so nothing else in the module moves. The SLEB arithmetic is where a
hand-written patch goes wrong — `775533` is `ed aa 2f`, and assuming `ad aa 2f`
finds nothing, which looks exactly like the code having changed.

`host --defaults` answers what nothing here can with the zero of its type and
records that it did. It is for *reaching* a path — the imports in the way are
often ones only the application can answer — and `Host::unanswered.report()`
prints the list any result from such a run has to be read against.

`frames --outside` lists the functions whose stores land past the end of their
own stack frame — an overrun of a local array writes into the caller's frame,
and this is the short list of suspects when an address is being corrupted.

`calls` answers the question a call site cannot: what reaches this function.
The module says what each function calls; nothing in it says what calls a given
one, and that is usually the direction you want.

`host` writes a skeleton `impl Imports`: every import the module still needs,
grouped by where it comes from, each one a `todo!()`. Emscripten's `invoke_*`
trampolines are not among them — those are generated.

A large module wants the split: rustc partitions codegen units along module
boundaries, so half a million lines in one file becomes one enormous unit.

Decompilation is faithful, not idiomatic: linear memory stays a byte vector and
every trap stays a trap, so the result can be run against the module it came
from. Anything unsupported is an error, never a silent omission.

`--level 1` is the one exception, and it is opt-in for that reason. A frame slot
it promotes is a Rust binding rather than bytes in linear memory, so a run no
longer leaves those bytes behind — the answers still match the engine, the
memory no longer does. Every function it changed says so in its doc comment,
and every function it refused says why it refused.
";

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    match run(&arguments) {
        Ok(output) => {
            // Written rather than printed: `unwasm table … | head` closes the
            // pipe, and `print!` panics on that. A tool whose output is meant
            // to be filtered should not fall over when it is.
            use std::io::Write as _;
            let _ = std::io::stdout().write_all(output.as_bytes());
            ExitCode::SUCCESS
        }
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::FAILURE
        }
    }
}

fn run(arguments: &[String]) -> Result<String, String> {
    match arguments.first().map(String::as_str) {
        Some("decompile") => decompile(&arguments[1..]),
        Some("host") => host(&arguments[1..]),
        Some("table") => table(&arguments[1..]),
        Some("calls") => calls(&arguments[1..]),
        Some("signatures") => signatures(&arguments[1..]),
        Some("classes") => classes(&arguments[1..]),
        Some("frames") => frames(&arguments[1..]),
        Some("bytes") => bytes(&arguments[1..]),
        Some("data") => data(&arguments[1..]),
        Some("vtable") => vtable(&arguments[1..]),
        Some("stores") => stores(&arguments[1..]),
        Some("patch") => patch(&arguments[1..]),
        Some("constants") => constants(&arguments[1..]),
        Some("inspect") => inspect(&arguments[1..]),
        Some("-h" | "--help" | "help") | None => Ok(USAGE.to_string()),
        Some(other) => Err(format!("unknown command `{other}`\n\n{USAGE}")),
    }
}

fn decompile(arguments: &[String]) -> Result<String, String> {
    if wants_help(arguments) {
        return help_for("decompile");
    }
    let (path, rest_args) = positional(arguments, "decompile")?;
    let mut destination = None;
    let mut split = None;
    let mut only: std::collections::BTreeSet<u32> = std::collections::BTreeSet::new();
    let mut catalogue: Option<codegen::Signatures> = None;
    let mut with_callees = false;
    let mut bare = false;
    let mut spans = false;
    let mut reachable_from: Vec<u32> = Vec::new();
    let mut direct_only = false;
    let mut instrument = false;
    let mut stub_recognised = false;
    let mut level = 0u8;
    let mut map_offsets = false;
    let mut rest = rest_args.iter();
    while let Some(argument) = rest.next() {
        match argument.as_str() {
            "-o" | "--output" => {
                destination = Some(rest.next().ok_or("-o needs a path")?.clone());
            }
            "--split" => {
                let value = rest.next().ok_or("--split needs a number")?;
                split = Some(
                    value
                        .parse::<usize>()
                        .map_err(|_| format!("--split needs a number, not `{value}`"))?,
                );
            }
            "--signatures" => {
                let value = rest.next().ok_or("--signatures needs a path")?;
                catalogue = Some(read_signatures(value)?);
            }
            "--with-callees" => with_callees = true,
            "--bare" => bare = true,
            "--spans" => spans = true,
            "--direct-only" => direct_only = true,
            "--reachable-from" => {
                let value = rest
                    .next()
                    .ok_or("--reachable-from needs a function index")?;
                for part in value.split(',').filter(|part| !part.is_empty()) {
                    reachable_from.push(
                        part.trim()
                            .parse::<u32>()
                            .map_err(|_| format!("--reachable-from takes indices, not `{part}`"))?,
                    );
                }
                if reachable_from.is_empty() {
                    return Err("--reachable-from needs at least one index".to_string());
                }
            }
            "--instrument-stores" => instrument = true,
            "--stub-recognised" => stub_recognised = true,
            "--level" => {
                let value = rest.next().ok_or("--level needs a number")?;
                level = match value.as_str() {
                    "0" => 0,
                    "1" => 1,
                    "2" => 2,
                    other => {
                        return Err(format!(
                            "--level is 0 (faithful), 1 (frame slots as bindings) or 2 (and \
                             names from the C++ RTTI), not `{other}`"
                        ));
                    }
                };
            }
            "--offsets" => map_offsets = true,
            "--only" => {
                let value = rest.next().ok_or("--only needs a list of indices")?;
                for part in value.split(',').filter(|part| !part.is_empty()) {
                    only.insert(
                        part.trim()
                            .parse::<u32>()
                            .map_err(|_| format!("--only takes indices, not `{part}`"))?,
                    );
                }
                if only.is_empty() {
                    return Err("--only needs at least one index".to_string());
                }
            }
            other => return Err(unexpected("decompile", other)),
        }
    }

    let module = read(path)?;
    if direct_only && reachable_from.is_empty() {
        return Err("--direct-only modifies --reachable-from".to_string());
    }
    if !reachable_from.is_empty() {
        let count = module.func_imports.len() + module.funcs.len();
        let analysis = unwasm_core::analysis::analyse(&module);
        for start in reachable_from {
            if start as usize >= count {
                return Err(format!(
                    "the module has {count} functions, and #{start} is not one of them"
                ));
            }
            only.extend(if direct_only {
                analysis.directly_reachable_from(&module, start)
            } else {
                analysis.reachable_from(&module, start)
            });
        }
        // Instantiation runs `start` before anything else does, so leaving it
        // out means the first thing that happens is a stub.
        if let Some(start) = module.start {
            only.extend(if direct_only {
                analysis.directly_reachable_from(&module, start)
            } else {
                analysis.reachable_from(&module, start)
            });
        }
    }
    if with_callees {
        if only.is_empty() {
            return Err("--with-callees expands --only, so it needs one".to_string());
        }
        // One level, because that is the one you always want: reading a
        // function and needing the next is the same minute; needing the whole
        // transitive closure is the whole module again.
        let analysis = unwasm_core::analysis::analyse(&module);
        let import_count = module.func_imports.len() as u32;
        let callees: Vec<u32> = only
            .iter()
            .flat_map(|index| analysis.call_graph.calls_from(*index).iter().copied())
            // An import has no body to decompile; its thunk is emitted anyway.
            .filter(|callee| *callee >= import_count)
            .collect();
        only.extend(callees);
    }

    // A catalogue is the only thing that recognises anything, so asking to
    // leave the recognised out without one leaves nothing out — and would
    // report success having done nothing.
    if stub_recognised && catalogue.is_none() {
        return Err(
            "--stub-recognised needs --signatures: without a catalogue nothing is recognised"
                .to_string(),
        );
    }
    let stubbed = if stub_recognised {
        codegen::recognised_functions(&module, catalogue.as_ref().expect("just checked")).len()
    } else {
        0
    };

    // `--bare` is meaningless without a selection: it *is* the selection,
    // emitted alone, and without one it would print the whole module with the
    // scaffolding removed — which is neither readable nor runnable.
    if bare && only.is_empty() {
        return Err(
            "--bare emits only the functions asked for, so it needs --only or --reachable-from"
                .to_string(),
        );
    }
    if bare && map_offsets {
        return Err(
            "--offsets maps a file this does not write; drop --bare or --offsets".to_string(),
        );
    }

    // The spans of what would be written, so a slice of it can be cut exactly.
    // Grepping for the next `fn f<n>` is how a body gets truncated at the wrong
    // closing brace, and a truncated body reads as a complete one.
    if spans {
        let files = codegen::generate_options(
            &module,
            &codegen::Options {
                layout: match (&destination, split) {
                    (Some(path), None) if path.ends_with(".rs") => codegen::Layout::Single,
                    (None, None) => codegen::Layout::Single,
                    (_, Some(lines_per_file)) => codegen::Layout::Split { lines_per_file },
                    (Some(_), None) => codegen::Layout::for_module(&module),
                },
                only: (!only.is_empty()).then(|| only.clone()),
                signatures: catalogue.clone().unwrap_or_default(),
                instrument_stores: instrument,
                map_offsets: false,
                stub_recognised,
                promote_frames: level >= 1,
                name_classes: level >= 2,
                bare,
            },
        )
        .map_err(|error| error.to_string())?;
        let index = files
            .iter()
            .find(|file| file.name == "names.json")
            .ok_or("the generator wrote no index")?;
        let mut out = String::from(
            "index   name                                     file        first    last\n",
        );
        for line in index.contents.lines() {
            let field = |key: &str| -> Option<String> {
                let at = line.find(&format!("\"{key}\": "))? + key.len() + 4;
                let rest = &line[at..];
                let end = rest.find([',', '}'])?;
                Some(rest[..end].trim().trim_matches('"').to_string())
            };
            let (Some(index), Some(name), Some(file), Some(first), Some(last)) = (
                field("index"),
                field("name"),
                field("file"),
                field("line"),
                field("last_line"),
            ) else {
                continue;
            };
            // Only what was asked for: the stubs have spans too, and fifteen
            // thousand of them is the problem this is here to solve.
            if let Ok(number) = index.parse::<u32>()
                && !only.is_empty()
                && !only.contains(&number)
            {
                continue;
            }
            // The generator calls its single file `mod.rs`; a reader slicing
            // one wants the name they asked for.
            let file = match (&destination, split) {
                (Some(path), None) if path.ends_with(".rs") && file == "mod.rs" => path.clone(),
                _ => file,
            };
            let _ = writeln!(out, "{index:<7} {name:<40} {file:<11} {first:<8} {last}");
        }
        // Written as well, when a destination was given: the spans describe a
        // file, and a file nobody wrote is spans of nothing.
        if let Some(destination) = &destination {
            let single = destination.ends_with(".rs") && split.is_none();
            if single {
                std::fs::write(destination, &files[0].contents)
                    .map_err(|error| format!("writing {destination}: {error}"))?;
            } else {
                let directory = Path::new(destination);
                std::fs::create_dir_all(directory)
                    .map_err(|error| format!("creating {destination}: {error}"))?;
                for file in &files {
                    let at = directory.join(&file.name);
                    std::fs::write(&at, &file.contents)
                        .map_err(|error| format!("writing {}: {error}", at.display()))?;
                }
            }
            let _ = writeln!(out, "\nwrote {destination}");
        }
        return Ok(out);
    }

    let Some(destination) = destination else {
        if split.is_some() {
            return Err("--split writes several files, so it needs -o <directory>".to_string());
        }
        if map_offsets {
            return Err(
                "--offsets writes offsets.json beside the output, so it needs -o <directory>"
                    .to_string(),
            );
        }
        if only.is_empty() && catalogue.is_none() && !instrument && level == 0 && !bare {
            return codegen::generate(&module).map_err(|error| error.to_string());
        }
        let files = codegen::generate_options(
            &module,
            &codegen::Options {
                layout: codegen::Layout::Single,
                only: (!only.is_empty()).then(|| only.clone()),
                signatures: catalogue.clone().unwrap_or_default(),
                instrument_stores: instrument,
                map_offsets,
                stub_recognised,
                promote_frames: level >= 1,
                name_classes: level >= 2,
                bare,
            },
        )
        .map_err(|error| error.to_string())?;
        return Ok(files[0].contents.clone());
    };

    // A `.rs` destination is one file; anything else is a directory. The
    // distinction is the path itself rather than a flag, because that is the
    // question the user already answered by choosing the name.
    let single_file = destination.ends_with(".rs") && split.is_none();
    let layout = if single_file {
        codegen::Layout::Single
    } else {
        match split {
            Some(lines_per_file) => codegen::Layout::Split { lines_per_file },
            None => codegen::Layout::for_module(&module),
        }
    };

    let files = codegen::generate_options(
        &module,
        &codegen::Options {
            layout,
            only: (!only.is_empty()).then(|| only.clone()),
            signatures: catalogue.unwrap_or_default(),
            instrument_stores: instrument,
            map_offsets,
            stub_recognised,
            promote_frames: level >= 1,
            name_classes: level >= 2,
            bare,
        },
    )
    .map_err(|error| error.to_string())?;
    let lines: usize = files.iter().map(|file| file.contents.lines().count()).sum();

    // Said before rustc says it its own way. wasm's nesting becomes Rust's, and
    // rustc parses that recursively: past a couple of thousand blocks it
    // overflows its stack and dies with SIGSEGV, which reads as a compiler bug
    // rather than as a file that needs a bigger stack to parse.
    let stubbed = if stubbed > 0 {
        format!(
            "left {stubbed} of {} functions out as recognised library code\n",
            module.funcs.len()
        )
    } else {
        String::new()
    };
    let note = match unwasm_core::analysis::deepest_nesting(&module) {
        Some((func, depth)) if depth > unwasm_core::analysis::NESTING_RUSTC_HANDLES => format!(
            "note: function #{func} nests {depth} blocks, and rustc parses nesting \
             recursively.\n      Compile this with RUST_MIN_STACK=134217728 set, or \
             rustc overflows its\n      stack and dies with SIGSEGV.\n"
        ),
        _ => String::new(),
    };

    // A single-file destination gets the Rust; the index needs a directory to
    // live beside it.
    if single_file {
        std::fs::write(&destination, &files[0].contents)
            .map_err(|error| format!("writing {destination}: {error}"))?;
        return Ok(format!(
            "wrote {destination} ({} lines, {} functions)\n{stubbed}{note}",
            files[0].contents.lines().count(),
            module.funcs.len()
        ));
    }

    let directory = Path::new(&destination);
    std::fs::create_dir_all(directory)
        .map_err(|error| format!("creating {destination}: {error}"))?;
    for file in &files {
        let at = directory.join(&file.name);
        std::fs::write(&at, &file.contents)
            .map_err(|error| format!("writing {}: {error}", at.display()))?;
    }
    Ok(format!(
        "wrote {destination}/ ({} Rust files plus names.json, {lines} lines, {} functions)\n{stubbed}{note}",
        files.len() - 1,
        module.funcs.len()
    ))
}

fn host(arguments: &[String]) -> Result<String, String> {
    if wants_help(arguments) {
        return help_for("host");
    }
    let mut defaults = false;
    let (path, rest_args) = positional(arguments, "host")?;
    let mut destination = None;
    let mut rest = rest_args.iter();
    while let Some(argument) = rest.next() {
        match argument.as_str() {
            "-o" | "--output" => {
                destination = Some(rest.next().ok_or("-o needs a path")?.clone());
            }
            "--defaults" => defaults = true,
            other => return Err(format!("unexpected argument `{other}`")),
        }
    }

    let module = read(path)?;
    let skeleton =
        codegen::generate_host_with(&module, defaults).map_err(|error| error.to_string())?;
    match destination {
        Some(destination) => {
            std::fs::write(&destination, &skeleton)
                .map_err(|error| format!("writing {destination}: {error}"))?;
            // Only the trait's own methods: the embedded library above it
            // has plenty of `fn`s and none of them are the host's to write.
            let trait_body = skeleton
                .split_once("impl Imports for Host {")
                .map_or("", |(_, rest)| rest);
            Ok(format!(
                "wrote {destination} ({} methods, {} of them still to implement)\n",
                trait_body.matches("\n    fn ").count(),
                trait_body.matches("todo!(\"").count()
            ))
        }
        None => Ok(skeleton),
    }
}

/// Rewrites one `i32.const` in place, keeping its encoding the same length.
///
/// This is the other half of `constants`: that command lists the sites, and
/// giving each one a distinct value is how a reader learns which fired. Doing
/// it by hand means computing a signed LEB128 and counting bytes, which is
/// exactly where it goes wrong — an encoding one byte short is not found, and
/// "the pattern is not in the module" reads like the code having changed
/// rather than like arithmetic.
///
/// Same length or nothing: a shorter or longer encoding would move every byte
/// after it, and every offset anybody had written down with it.
fn patch(arguments: &[String]) -> Result<String, String> {
    if wants_help(arguments) {
        return help_for("patch");
    }
    let (path, rest_args) = positional(arguments, "patch")?;
    let offset: usize = arguments
        .get(1)
        .ok_or("patch needs the offset of the instruction")?
        .parse()
        .map_err(|_| "the offset must be a number".to_string())?;
    let value: i64 = arguments
        .get(2)
        .ok_or("patch needs the value to put there")?
        .parse()
        .map_err(|_| "the value must be a number".to_string())?;
    let mut destination = None;
    let mut rest = rest_args[2..].iter();
    while let Some(argument) = rest.next() {
        match argument.as_str() {
            "-o" | "--output" => {
                destination = Some(rest.next().ok_or("-o needs a path")?.clone());
            }
            other => return Err(format!("unexpected argument `{other}`")),
        }
    }
    let destination = destination.ok_or("patch writes a new module, so it needs -o <path>")?;

    let mut raw = std::fs::read(path).map_err(|error| format!("reading {path}: {error}"))?;
    match raw.get(offset) {
        Some(0x41) => {}
        Some(other) => {
            return Err(format!(
                "{offset} holds {other:#04x}, not an i32.const (0x41). \
                 `unwasm constants` gives the offset of one"
            ));
        }
        None => return Err(format!("{offset} is past the end of {path}")),
    }

    let was = read_signed_leb(&raw[offset + 1..])
        .ok_or_else(|| format!("the constant at {offset} does not decode"))?;
    let encoded = signed_leb(value);
    if encoded.len() != was.1 {
        return Err(format!(
            "{value} encodes to {} bytes and the constant there takes {}. \
             A different length moves everything after it, and every offset \
             written down with it — pick a value that fits, or patch the \
             surrounding instruction instead",
            encoded.len(),
            was.1
        ));
    }

    raw.splice(offset + 1..offset + 1 + was.1, encoded.iter().copied());
    std::fs::write(&destination, &raw)
        .map_err(|error| format!("writing {destination}: {error}"))?;
    Ok(format!(
        "wrote {destination}: i32.const {} at {offset} is now i32.const {value} \
         ({} bytes, unmoved)\n",
        was.0, was.1
    ))
}

/// Decodes a signed LEB128, returning the value and how many bytes it took.
fn read_signed_leb(bytes: &[u8]) -> Option<(i64, usize)> {
    let mut value: i64 = 0;
    let mut shift = 0;
    for (at, byte) in bytes.iter().enumerate() {
        value |= i64::from(byte & 0x7F) << shift;
        shift += 7;
        if byte & 0x80 == 0 {
            // Sign-extend from the last bit that was written.
            if shift < 64 && byte & 0x40 != 0 {
                value |= -1i64 << shift;
            }
            return Some((value, at + 1));
        }
        if shift >= 64 {
            return None;
        }
    }
    None
}

/// Encodes a signed LEB128, in the shortest form — which is what a toolchain
/// emits, and what a same-length replacement has to match.
fn signed_leb(mut value: i64) -> Vec<u8> {
    let mut bytes = Vec::new();
    loop {
        let byte = (value & 0x7F) as u8;
        value >>= 7;
        let done = (value == 0 && byte & 0x40 == 0) || (value == -1 && byte & 0x40 != 0);
        bytes.push(if done { byte } else { byte | 0x80 });
        if done {
            return bytes;
        }
    }
}

/// Every site that pushes a constant, and every data segment that contains it.
///
/// The question this answers is "which of these is the one that ran". Giving
/// each site a distinct value is how that gets measured, and doing it needs
/// the offset of each and the number of bytes it occupies — a replacement of
/// the same encoded length moves nothing else in the module.
fn constants(arguments: &[String]) -> Result<String, String> {
    if wants_help(arguments) {
        return help_for("constants");
    }
    let (path, rest_args) = positional(arguments, "constants")?;
    let value: i64 = arguments
        .get(1)
        .ok_or("constants needs a value")?
        .parse()
        .map_err(|_| "the value must be a number".to_string())?;
    let mut in_data = false;
    for extra in &rest_args[1..] {
        match extra.as_str() {
            "--data" => in_data = true,
            other => return Err(unexpected("constants", other)),
        }
    }

    let module = read(path)?;
    let analysis = unwasm_core::analysis::analyse(&module);
    let import_count = module.func_imports.len() as u32;
    let mut out = String::new();
    let mut sites = 0;
    for (at, func) in module.funcs.iter().enumerate() {
        let index = import_count + at as u32;
        for (position, op) in func.body.iter().enumerate() {
            let width = match op {
                unwasm_core::module::Op::I32Const(found) if i64::from(*found) == value => "i32",
                unwasm_core::module::Op::I64Const(found) if *found == value => "i64",
                _ => continue,
            };
            sites += 1;
            let name = codegen::function_ident(index, &analysis);
            // The decoder fills one span per operator, so the fallback is
            // never taken; it is here because printing a wrong offset would be
            // worse than printing none, and a patch computed from one would
            // land in the middle of another instruction.
            let place = func.offsets.get(position).map_or(
                "at an offset the decoder did not record".to_string(),
                |(offset, length)| format!("at {offset} + {length} bytes"),
            );
            let _ = writeln!(out, "{name:<28} {width}.const {place}");
        }
    }

    // The same number can sit in the data as well, which is why counting
    // occurrences of its bytes is not the same as counting the sites that
    // push it — and why a function pointer installed in a vtable is invisible
    // to a search of the code. Nothing ever pushes it; a data segment holds it.
    let image = unwasm_core::analysis::DataImage::of(&module, &analysis.placements);
    let holders = image.find32(value as i32);

    // A word in a table is four-byte aligned. An unaligned match is four bytes
    // of something else that happen to read as this number, and on a 10 MiB
    // module there are always a few — which is why they are marked rather than
    // listed as if they were the same kind of thing.
    let aligned = holders
        .iter()
        .filter(|held| held.address().is_multiple_of(4))
        .count();
    if in_data && !holders.is_empty() {
        let _ = writeln!(
            out,
            "\nand at {} address{} in the data, which no instruction pushes ({aligned} \
             four-byte aligned):",
            holders.len(),
            if holders.len() == 1 { "" } else { "es" }
        );
        for held in holders.iter().take(200) {
            let at = held.address();
            let _ = writeln!(
                out,
                "  {} segment #{:<4} file {:<10} {}",
                hex_addr(at as i32),
                held.segment,
                held.file_offset,
                if at.is_multiple_of(4) {
                    format!("aligned — `unwasm data {at} 32` or `unwasm vtable {at}` reads it")
                } else {
                    "not aligned: four bytes of something else that read as this".to_string()
                }
            );
        }
        if holders.len() > 200 {
            let _ = writeln!(out, "  … and {} more", holders.len() - 200);
        }
    }

    Ok(format!(
        "{out}\n{sites} site{} push {value}{}\n",
        if sites == 1 { "" } else { "s" },
        if holders.is_empty() {
            String::new()
        } else {
            format!(
                ", and its four bytes sit at {} address{} inside the data segments \
                 ({aligned} aligned){}",
                holders.len(),
                if holders.len() == 1 { "" } else { "es" },
                if in_data {
                    ""
                } else {
                    " — `--data` lists them"
                }
            )
        }
    ))
}

/// Prints the bytes at an offset, and says whether they are unique.
///
/// The two questions a hand-written patch asks, and the two that were being
/// answered by computing LEB encodings by hand: what is actually there, and
/// will the pattern I am about to search for match one place or forty.
fn bytes(arguments: &[String]) -> Result<String, String> {
    if wants_help(arguments) {
        return help_for("bytes");
    }
    let (path, rest_args) = positional(arguments, "bytes")?;
    let offset: usize = arguments
        .get(1)
        .ok_or("bytes needs an offset")?
        .parse()
        .map_err(|_| "the offset must be a number".to_string())?;
    let length: usize = arguments
        .get(2)
        .ok_or("bytes needs a length")?
        .parse()
        .map_err(|_| "the length must be a number".to_string())?;
    if let Some(extra) = rest_args.get(2) {
        return Err(format!("unexpected argument `{extra}`"));
    }

    let raw = std::fs::read(path).map_err(|error| format!("reading {path}: {error}"))?;
    let end = offset
        .checked_add(length)
        .filter(|end| *end <= raw.len())
        .ok_or_else(|| {
            format!(
                "{offset}..{} is past the end of {path} ({} bytes)",
                offset.saturating_add(length),
                raw.len()
            )
        })?;
    let wanted = &raw[offset..end];
    let hex: Vec<String> = wanted.iter().map(|byte| format!("{byte:02x}")).collect();

    // How many times this exact sequence appears anywhere in the file. One
    // means a search-and-replace patch is safe; more means it is not.
    let occurrences = if wanted.is_empty() {
        0
    } else {
        raw.windows(wanted.len())
            .filter(|window| *window == wanted)
            .count()
    };

    Ok(format!(
        "{offset} ({offset:#x}) + {length}: {}\n{occurrences} occurrence{} of that sequence in the module{}\n",
        hex.join(" "),
        if occurrences == 1 { "" } else { "s" },
        if occurrences == 1 {
            " — unique, so a pattern patch is safe"
        } else {
            " — a pattern patch would hit all of them"
        }
    ))
}

/// `classes`: what the C++ ABI wrote down about the module's own types.
fn classes(arguments: &[String]) -> Result<String, String> {
    if wants_help(arguments) {
        return help_for("classes");
    }
    let (path, rest_args) = positional(arguments, "classes")?;
    let mut methods = false;
    for argument in rest_args {
        match argument.as_str() {
            "--methods" => methods = true,
            other => return Err(format!("unexpected argument `{other}`")),
        }
    }
    let module = read(path)?;
    let analysis = unwasm_core::analysis::analyse(&module);
    let (classes, evidence) = unwasm_core::analysis::classes(&module, &analysis.placements);
    if classes.is_empty() {
        return Ok(format!(
            "{path} declares no C++ classes: no `type_info` object here is pointed at\nby enough others to be one. A module built from C has none.\n"
        ));
    }

    let mut out = format!(
        "{} classes, {} with vtables, across {} `type_info` {}\n",
        evidence.classes,
        evidence.with_vtables,
        evidence.kinds,
        if evidence.kinds == 1 { "kind" } else { "kinds" }
    );
    if evidence.by_base > 0 {
        let _ = writeln!(
            out,
            "  {} of them named by a derived class rather than by the count",
            evidence.by_base
        );
    }
    let named: std::collections::BTreeMap<i32, &str> = classes
        .iter()
        .map(|class| (class.type_info, class.name.as_str()))
        .collect();
    for class in &classes {
        let _ = writeln!(
            out,
            "  {:<60} {}",
            class.name,
            match class.vtable {
                Some(at) => {
                    let nulls = class.methods.iter().filter(|slot| slot.is_none()).count();
                    format!(
                        "vtable {at:#x}, {} slots, {} methods{}",
                        class.methods.len(),
                        class.methods.len() - nulls,
                        if nulls == 0 {
                            String::new()
                        } else {
                            format!(", {nulls} pure virtual")
                        }
                    )
                }
                None => "no vtable".to_string(),
            }
        );
        // The mangled form when the readable one is not the whole story, so a
        // reader can check the name against the bytes.
        if class.name != class.mangled {
            let _ = writeln!(out, "      mangled {}", class.mangled);
        }
        // Only single inheritance is written down as a pointer, so a class with
        // no line here may still have bases — see `analysis::Class::base`.
        if let Some(base) = class.base.and_then(|at| named.get(&at)) {
            let _ = writeln!(out, "      derives from {base}");
        }
        if methods {
            for (slot, func) in class.methods.iter().enumerate() {
                let _ = match func {
                    Some(func) => writeln!(out, "      slot {slot:<3} f{func}"),
                    // A zero here is a pure virtual, and `unwasm vtable` says
                    // what calling one does.
                    None => writeln!(out, "      slot {slot:<3} 0 — pure virtual"),
                };
            }
        }
    }
    Ok(out)
}

fn frames(arguments: &[String]) -> Result<String, String> {
    if wants_help(arguments) {
        return help_for("frames");
    }
    let (path, rest_args) = positional(arguments, "frames")?;
    let mut only_outside = false;
    for argument in rest_args {
        match argument.as_str() {
            "--outside" => only_outside = true,
            other => return Err(format!("unexpected argument `{other}`")),
        }
    }

    let module = read(path)?;
    let analysis = unwasm_core::analysis::analyse(&module);
    let mut out = String::new();
    let mut shown = 0;
    for (index, frame) in &analysis.frames {
        let outside = frame.writes_outside();
        if only_outside && outside.is_empty() && frame.computed_writes == 0 {
            continue;
        }
        shown += 1;
        let name = codegen::function_ident(*index, &analysis);
        let _ = writeln!(
            out,
            "{name:<28} {:>6} bytes  {} slots{}",
            frame.size,
            frame.slots.len(),
            if frame.escapes {
                "  (address escapes)"
            } else {
                ""
            }
        );
        for (offset, width) in outside {
            let _ = writeln!(out, "    writes frame + {offset} ({width} bytes) — outside");
        }
        if frame.computed_writes > 0 {
            let _ = writeln!(
                out,
                "    {} writes through a computed frame address — offset unknown",
                frame.computed_writes
            );
        }
    }
    let of = analysis.frames.len();
    Ok(format!(
        "{out}\n{shown} of {of} frames{}\n",
        if only_outside {
            " write outside themselves or through a computed address"
        } else {
            ""
        }
    ))
}

fn signatures(arguments: &[String]) -> Result<String, String> {
    if wants_help(arguments) {
        return help_for("signatures");
    }
    let (path, rest_args) = positional(arguments, "signatures")?;
    let mut destination = None;
    let mut rest = rest_args.iter();
    while let Some(argument) = rest.next() {
        match argument.as_str() {
            "-o" | "--output" => {
                destination = Some(rest.next().ok_or("-o needs a path")?.clone());
            }
            other => return Err(format!("unexpected argument `{other}`")),
        }
    }

    let module = read(path)?;
    let catalogue = codegen::extract_signatures(&module);
    if catalogue.is_empty() {
        return Err(format!(
            "{path} names none of its functions, so there is nothing to catalogue. \
             Build the reference with `-g2` to keep the name section."
        ));
    }
    let mut lines: Vec<String> = catalogue
        .iter()
        .map(|(fingerprint, name)| format!("{fingerprint:016x} {name}"))
        .collect();
    lines.sort();
    let text = format!("{}\n", lines.join("\n"));

    match destination {
        Some(destination) => {
            std::fs::write(&destination, &text)
                .map_err(|error| format!("writing {destination}: {error}"))?;
            Ok(format!(
                "wrote {destination} ({} signatures)\n",
                catalogue.len()
            ))
        }
        None => Ok(text),
    }
}

/// Reads a catalogue back.
fn read_signatures(path: &str) -> Result<codegen::Signatures, String> {
    let text = std::fs::read_to_string(path).map_err(|error| format!("reading {path}: {error}"))?;
    let mut catalogue = codegen::Signatures::new();
    for (at, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let (fingerprint, name) = line
            .split_once(' ')
            .ok_or_else(|| format!("{path}:{}: expected `<fingerprint> <name>`", at + 1))?;
        let fingerprint = u64::from_str_radix(fingerprint, 16)
            .map_err(|_| format!("{path}:{}: `{fingerprint}` is not a fingerprint", at + 1))?;
        catalogue.insert(fingerprint, name.to_string());
    }
    Ok(catalogue)
}

fn calls(arguments: &[String]) -> Result<String, String> {
    if wants_help(arguments) {
        return help_for("calls");
    }
    let (path, rest_args) = positional(arguments, "calls")?;
    let index = arguments
        .get(1)
        .ok_or("calls needs a function index")?
        .parse::<u32>()
        .map_err(|_| "calls takes a function index".to_string())?;
    if let Some(extra) = rest_args.get(1) {
        return Err(format!("unexpected argument `{extra}`"));
    }

    let module = read(path)?;
    let analysis = unwasm_core::analysis::analyse(&module);
    let total = module.func_imports.len() + module.funcs.len();
    if index as usize >= total {
        return Err(format!(
            "the module has {total} functions; #{index} is not one of them"
        ));
    }

    let describe = |func: u32| {
        let name = module
            .func_name(func)
            .map(str::to_string)
            .or_else(|| {
                analysis
                    .derived_names
                    .get(&func)
                    .map(|derived| derived.name.clone())
            })
            .unwrap_or_default();
        let signature = module
            .func_type(func)
            .map_or_else(|| "?".to_string(), unwasm_core::codegen::signature_text);
        format!(
            "  f{func:<6} {signature}{}{name}\n",
            if name.is_empty() { "" } else { "  " }
        )
    };

    let mut out = describe(index).replacen("  ", "", 1);
    let callers = analysis.call_graph.callers_of(index);
    let callees = analysis.call_graph.calls_from(index);

    let slots: Vec<String> = analysis
        .table
        .iter()
        .filter(|(_, func)| **func == index)
        .map(|(slot, _)| slot.to_string())
        .collect();
    if !slots.is_empty() {
        out.push_str(&format!("in table slots: {}\n", slots.join(", ")));
    }
    if let Some(export) = module.exports.iter().find(|export| {
        export.kind == unwasm_core::module::ExportKind::Func && export.index == index
    }) {
        out.push_str(&format!("exported as: {}\n", export.name));
    }

    // Sites, not callers: patching a body measures whichever site ran.
    let sites = analysis.call_graph.sites_reaching(index);
    out.push_str(&format!(
        "\ncalled by {} from {sites} call sites:\n",
        callers.len()
    ));
    for caller in callers {
        out.push_str(&describe(*caller));
    }
    out.push_str(&format!("\ncalls {}:\n", callees.len()));
    for callee in callees {
        out.push_str(&describe(*callee));
    }
    if let Some(types) = analysis.call_graph.calls_indirectly.get(&index) {
        out.push_str("\nand through the table, functions of type:\n");
        for ty in types {
            let signature = module.types.get(*ty as usize).map_or_else(
                || format!("type {ty}"),
                unwasm_core::codegen::signature_text,
            );
            // How many slots could actually answer such a call.
            let wanted = module.types.get(*ty as usize);
            let holding = analysis
                .table
                .values()
                .filter(|func| module.func_type(**func) == wanted)
                .count();
            out.push_str(&format!("  {signature}  ({holding} slots hold one)\n"));
        }
    }
    Ok(out)
}

fn table(arguments: &[String]) -> Result<String, String> {
    if wants_help(arguments) {
        return help_for("table");
    }
    let (path, rest_args) = positional(arguments, "table")?;
    let mut wanted = None;
    let mut rest = rest_args.iter();
    while let Some(argument) = rest.next() {
        match argument.as_str() {
            "--type" => wanted = Some(rest.next().ok_or("--type needs a signature")?.clone()),
            other => return Err(format!("unexpected argument `{other}`")),
        }
    }

    let module = read(path)?;
    let analysis = unwasm_core::analysis::analyse(&module);
    if analysis.table.is_empty() {
        return Ok("the table is empty: no element segment puts anything in it\n".to_string());
    }

    // Matching ignores spaces, so `(i32, i32) -> ()` and `(i32,i32)->()` are
    // the same request.
    let tidy = |text: &str| text.replace(' ', "");
    let wanted = wanted.map(|signature| tidy(&signature));

    let mut out = String::new();
    let mut shown = 0usize;
    for (slot, func) in &analysis.table {
        let signature = module
            .func_type(*func)
            .map_or_else(|| "?".to_string(), unwasm_core::codegen::signature_text);
        if let Some(wanted) = &wanted
            && tidy(&signature) != *wanted
        {
            continue;
        }
        let name = module
            .func_name(*func)
            .map(str::to_string)
            .or_else(|| {
                analysis
                    .derived_names
                    .get(func)
                    .map(|derived| format!("{} (guessed)", derived.name))
            })
            .unwrap_or_default();
        out.push_str(&format!(
            "  slot {slot:<6} f{func:<6} {signature}{}{name}\n",
            if name.is_empty() { "" } else { "  " }
        ));
        shown += 1;
    }
    Ok(format!(
        "{} of {} slots{}\n{out}",
        shown,
        analysis.table.len(),
        wanted.map_or(String::new(), |signature| format!(" matching {signature}"))
    ))
}

fn inspect(arguments: &[String]) -> Result<String, String> {
    if wants_help(arguments) {
        return help_for("inspect");
    }
    let (path, rest_args) = positional(arguments, "inspect")?;
    if let Some(extra) = rest_args.first() {
        return Err(unexpected("inspect", extra));
    }
    let module = read(path)?;
    let mut out = String::new();
    out.push_str(&format!(
        "{} functions ({} imported), {} globals, {} data segments\n",
        module.funcs.len() + module.func_imports.len(),
        module.func_imports.len(),
        module.globals.len(),
        module.datas.len()
    ));
    match &module.memory {
        Some(memory) => out.push_str(&format!(
            "memory: {} pages initial{}{}{}\n",
            memory.min_pages,
            memory
                .max_pages
                .map_or(String::new(), |max| format!(", {max} maximum")),
            if memory.shared { ", shared" } else { "" },
            memory
                .imported
                .as_ref()
                .map_or(String::new(), |(module, field)| {
                    format!(", imported from {module}::{field}")
                })
        )),
        None => out.push_str("memory: none\n"),
    }
    if !module.func_imports.is_empty() {
        out.push_str("\nimports:\n");
        for import in &module.func_imports {
            out.push_str(&format!("  {}::{}\n", import.module, import.field));
        }
    }
    // What the analysis could make of the module, in one line each: this is
    // the cheapest way to see whether a module is worth reading further.
    let analysis = unwasm_core::analysis::analyse(&module);
    match analysis.stack_pointer {
        Some(found) => out.push_str(&format!(
            "stack pointer: global #{} ({})\n",
            found.global,
            match found.evidence {
                unwasm_core::analysis::Evidence::Exported => "by its exported name".to_string(),
                unwasm_core::analysis::Evidence::Named =>
                    "by its name in the name section".to_string(),
                unwasm_core::analysis::Evidence::Prologue { functions } =>
                    format!("by {functions} prologues"),
            }
        )),
        None => out.push_str("stack pointer: not identified\n"),
    }
    if !analysis.frames.is_empty() {
        let contained = analysis
            .frames
            .values()
            .filter(|frame| !frame.escapes)
            .count();
        let slots: usize = analysis
            .frames
            .values()
            .map(|frame| frame.slots.len())
            .sum();
        let largest = analysis
            .frames
            .values()
            .map(|frame| frame.size)
            .max()
            .unwrap_or(0);
        out.push_str(&format!(
            "frames: {} functions, {contained} whose address stays put, {slots} slots, largest {largest} bytes\n",
            analysis.frames.len()
        ));
    }

    if !analysis.registrations.is_empty() {
        let named = analysis
            .registrations
            .iter()
            .filter(|registration| registration.name.is_some() || registration.signature.is_some())
            .count();
        out.push_str(&format!(
            "embind: {} registrations, {named} of them named\n",
            analysis.registrations.len()
        ));
        for registration in &analysis.registrations {
            let kind = registration.kind.trim_start_matches("_embind_register_");
            match (
                &registration.signature,
                &registration.class,
                &registration.name,
            ) {
                (Some(signature), Some(class), _) => {
                    out.push_str(&format!("  {kind} {class}::{signature}\n"));
                }
                (Some(signature), None, _) => out.push_str(&format!("  {kind} {signature}\n")),
                (None, _, Some(name)) => out.push_str(&format!("  {kind} {name}\n")),
                (None, _, None) => {}
            }
        }
    }

    out.push_str("\nexports:\n");
    for export in &module.exports {
        out.push_str(&format!(
            "  {:?} {} -> #{}\n",
            export.kind, export.name, export.index
        ));
    }
    Ok(out)
}

/// Parses a number that may be hexadecimal, decimal, or underscored.
///
/// An address written down in a debugger is `0x103564`; one measured at run
/// time is `1_324_800`. Making the reader convert between them is the step at
/// which the wrong number gets typed.
fn number(text: &str, what: &str) -> Result<i64, String> {
    let tidy: String = text.chars().filter(|ch| *ch != '_').collect();
    let (body, radix) = match tidy.strip_prefix("0x").or_else(|| tidy.strip_prefix("0X")) {
        Some(rest) => (rest, 16),
        None => (tidy.as_str(), 10),
    };
    i64::from_str_radix(body, radix)
        .map_err(|_| format!("{what} takes a number, decimal or 0x-prefixed, not `{text}`"))
}

/// Guest memory at an address, as the module's data segments leave it.
///
/// The question `bytes` cannot answer. `bytes` takes an offset into the *file*,
/// and turning a guest address into one means knowing which segment covers it
/// and where that segment's bytes start — a subtraction that holds for one
/// segment and silently lands somewhere else for the next. A threaded module
/// makes it worse: its segments are passive and carry no address at all, so the
/// mapping only exists once the `memory.init` calls have been resolved.
///
/// An address no segment covers is not an error. It is memory the module never
/// initialises, and it reads as zero at run time — which is an answer, and
/// often the answer being looked for.
fn data(arguments: &[String]) -> Result<String, String> {
    if wants_help(arguments) {
        return help_for("data");
    }
    let (path, rest) = positional(arguments, "data")?;
    let address = number(
        rest.first()
            .ok_or("data needs an address in guest memory")?,
        "the address",
    )? as i32;
    let length = match rest.get(1) {
        Some(text) => number(text, "the length")?,
        None => 32,
    };
    let length = usize::try_from(length).map_err(|_| "the length must not be negative")?;
    if let Some(extra) = rest.get(2) {
        return Err(format!("unexpected argument `{extra}`"));
    }

    let module = read(path)?;
    let analysis = unwasm_core::analysis::analyse(&module);
    let image = unwasm_core::analysis::DataImage::of(&module, &analysis.placements);
    Ok(dump(&image, address, length))
}

/// The body of `data`, so `vtable` can print the same block.
fn dump(image: &unwasm_core::analysis::DataImage<'_>, address: i32, length: usize) -> String {
    let mut out = String::new();
    let Some(located) = image.locate(address) else {
        let _ = writeln!(
            out,
            "{} ({address}) is not covered by any data segment — zero at run time.",
            hex_addr(address)
        );
        match image.extent() {
            Some((low, high)) => {
                let _ = writeln!(
                    out,
                    "The module's {} placed segments span {} .. {} ({low} .. {high}).",
                    image.placed(),
                    hex_addr(low as i32),
                    hex_addr(high as i32)
                );
            }
            None => out.push_str("The module places no data segment at a known address.\n"),
        }
        // Only when it is near one. "Ends 98 megabytes before this" is not a
        // near miss, and printing it as one invites reading it as one.
        if let Some(near) = image.nearest_below(address) {
            let short = near.offset - near.length + 1;
            if short <= 65536 {
                let _ = writeln!(
                    out,
                    "The nearest segment below is #{} at {} + {} bytes, which ends {short} \
                     byte{} before this.",
                    near.segment,
                    hex_addr(near.base as i32),
                    near.length,
                    if short == 1 { "" } else { "s" }
                );
            }
        }
        return out;
    };

    let _ = writeln!(
        out,
        "{} ({address}) is in data segment #{} ({}), which covers {} + {} bytes.",
        hex_addr(address),
        located.segment,
        if located.active {
            "active, its own offset"
        } else {
            "passive, placed by a memory.init"
        },
        hex_addr(located.base as i32),
        located.length
    );
    let _ = writeln!(
        out,
        "At {} into the segment; the byte is at file offset {} — that is what `bytes` and `patch` take.",
        located.offset, located.file_offset
    );

    // What is actually readable: the read stops where the segment does rather
    // than reporting bytes from the next one, which are not adjacent in memory.
    let available = (located.length - located.offset) as usize;
    if length > available {
        let _ = writeln!(
            out,
            "\nSegment #{} ends {} bytes in, so {} of the {length} asked for are not in the module.",
            located.segment,
            available,
            length - available
        );
    }
    let readable = length.min(available);
    let bytes = image.bytes(address, readable).unwrap_or_default();

    out.push('\n');
    for (row, chunk) in bytes.chunks(16).enumerate() {
        let at = address.wrapping_add((row * 16) as i32);
        let hex: Vec<String> = chunk.iter().map(|byte| format!("{byte:02x}")).collect();
        let text: String = chunk
            .iter()
            .map(|byte| {
                if byte.is_ascii_graphic() || *byte == b' ' {
                    *byte as char
                } else {
                    '.'
                }
            })
            .collect();
        let _ = writeln!(
            out,
            "  {:<10} {:<47}  |{text}|",
            hex_addr(at),
            hex.join(" ")
        );
    }

    if readable >= 4 {
        out.push_str("\nas u32 little-endian:\n");
        for (word, chunk) in bytes.as_chunks::<4>().0.iter().enumerate() {
            let value = i32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
            let note = if value == 0 {
                "  zero".to_string()
            } else {
                match image.text(value) {
                    Some(text) => format!("  -> \"{}\"", text.escape_default()),
                    None if image.holds(value) => "  -> into the data".to_string(),
                    None => String::new(),
                }
            };
            let _ = writeln!(
                out,
                "  +{:<5} {:>12}  {}{note}",
                word * 4,
                value,
                hex_addr(value)
            );
        }
    }
    out
}

fn hex_addr(value: i32) -> String {
    if value < 0 {
        format!("{value:#x}")
    } else {
        format!("{value:#010x}")
    }
}

/// Reads a C++ vtable: each slot as a table index, and what it reaches.
///
/// The defect this exists for: a slot holding 0 is a pure virtual function, and
/// a `call_indirect` on it goes to table index 0 — a slot whose signature is
/// not the one the call site declared, so the engine traps and the thread dies.
/// From the outside that looks like the engine failing for no reason; from
/// here it is one line of output.
fn vtable(arguments: &[String]) -> Result<String, String> {
    if wants_help(arguments) {
        return help_for("vtable");
    }
    let (path, rest) = positional(arguments, "vtable")?;
    let mut slots: Option<usize> = None;
    let mut class: Option<String> = None;
    let mut address: Option<i32> = None;
    let mut rest = rest.iter();
    while let Some(argument) = rest.next() {
        match argument.as_str() {
            "--slots" => {
                let value = rest.next().ok_or("--slots needs a number")?;
                slots = Some(number(value, "--slots")?.max(0) as usize);
            }
            "--class" => class = Some(rest.next().ok_or("--class needs a name")?.clone()),
            other if !other.starts_with('-') && address.is_none() => {
                address = Some(number(other, "the address")? as i32);
            }
            other => return Err(unexpected("vtable", other)),
        }
    }
    if address.is_some() && class.is_some() {
        return Err("vtable takes an address or --class, not both".to_string());
    }

    let module = read(path)?;
    let analysis = unwasm_core::analysis::analyse(&module);
    let image = unwasm_core::analysis::DataImage::of(&module, &analysis.placements);
    let (classes, _) = unwasm_core::analysis::classes(&module, &analysis.placements);

    let mut out = String::new();
    let address = match (address, &class) {
        (Some(address), _) => address,
        (None, Some(wanted)) => {
            // Exact first — the mangled name is the unique one, and the
            // readable one loses the template arguments, so six classes in this
            // module are all called `webrtc::RefCountedObject<…>`. Then a
            // substring, which is how a reader who knows the C++ name types it.
            let exact: Vec<&unwasm_core::analysis::Class> = classes
                .iter()
                .filter(|class| class.mangled == *wanted)
                .collect();
            let matches = if exact.is_empty() {
                let named: Vec<&unwasm_core::analysis::Class> = classes
                    .iter()
                    .filter(|class| class.name == *wanted)
                    .collect();
                if named.is_empty() {
                    classes
                        .iter()
                        .filter(|class| {
                            class.mangled.contains(wanted.as_str())
                                || class.name.contains(wanted.as_str())
                        })
                        .collect()
                } else {
                    named
                }
            } else {
                exact
            };
            let found = match matches.as_slice() {
                [only] => *only,
                [] => {
                    return Err(format!(
                        "no class named `{wanted}`. `unwasm classes {path}` lists the {} it found",
                        classes.len()
                    ));
                }
                several => {
                    // Refused rather than picked: which of six vtables was
                    // meant is not something the name decides.
                    return Err(format!(
                        "`{wanted}` matches {} classes. The mangled name is the unique one:\n{}",
                        several.len(),
                        several
                            .iter()
                            .take(12)
                            .map(|class| format!("       {}", class.mangled))
                            .collect::<Vec<_>>()
                            .join("\n")
                    ));
                }
            };
            let header = found.vtable.ok_or_else(|| {
                format!(
                    "`{}` declares a type_info but no vtable points at it",
                    found.name
                )
            })?;
            let _ = writeln!(
                out,
                "`{}`: type_info at {}, vtable header at {}, slots from {}.",
                found.name,
                hex_addr(found.type_info),
                hex_addr(header),
                hex_addr(header + 8)
            );
            // Itanium puts `{offset-to-top, type_info}` in front of the slots,
            // and the pointer an object holds is the slots' address.
            header + 8
        }
        (None, None) => return Err("vtable needs an address, or --class <name>".to_string()),
    };

    if !image.holds(address) {
        return Ok(format!("{out}{}", dump(&image, address, 32)));
    }

    // The Itanium header, when the two words before the slots are one. A zero
    // and a word that points at *some* data is not a header — the second word
    // has to be a `type_info` a class was actually recovered from, or the
    // claim is two coincidences read as a declaration.
    let owner = match (image.read32(address - 8), image.read32(address - 4)) {
        (Some(0), Some(info)) => classes
            .iter()
            .find(|class| class.type_info == info)
            .map(|class| (info, class)),
        _ => None,
    };
    match owner {
        Some((info, class)) => {
            let _ = writeln!(
                out,
                "header at {}: offset-to-top 0, type_info {} — `{}`{}",
                hex_addr(address - 8),
                hex_addr(info),
                class.name,
                // The mangled form as well: the readable one loses the template
                // arguments, and `RefCountedObject<…>` names nothing while
                // `…ResidualEchoDetector…` names the class.
                if class.mangled == class.name {
                    String::new()
                } else {
                    format!(" ({})", class.mangled)
                }
            );
        }
        None => {
            let _ = writeln!(
                out,
                "no C++ vtable header at {}: the two words before the slots are not \
                 `{{0, type_info}}`\nfor any class this module declares. Either the address \
                 given is the header rather than\nthe slots, or this is a plain table of \
                 function pointers — a C ops struct, which\nreads the same way and traps the \
                 same way on a null slot.",
                hex_addr(address - 8)
            );
        }
    }

    // Where to stop. A vtable ends at the first word that is neither a live
    // table index nor a zero followed by more of them — reading past that
    // reports the next object's bytes as methods.
    // Where the table stops: the same rule the class recovery uses, so
    // `classes --methods` and this never disagree about how long a vtable is.
    let words = unwasm_core::analysis::pointer_table(&image, &analysis.table, address, slots);

    if words.is_empty() {
        let _ = writeln!(
            out,
            "\nnothing at {} looks like a table of function pointers: the first word is \
             neither\nzero nor a live table index. Pass --slots <n> to read it anyway, or \
             `unwasm data`\nto see the bytes.",
            hex_addr(address)
        );
        return Ok(out);
    }

    let _ = writeln!(
        out,
        "\n{} slots at {}{}:",
        words.len(),
        hex_addr(address),
        if slots.is_none() {
            " (stopped at the first word that is neither a live table index nor a null \
             followed by one — `--slots <n>` reads further)"
        } else {
            ""
        }
    );
    let mut pure = Vec::new();
    for (slot, func) in words.iter().enumerate() {
        let at = slot * 4;
        let word = image
            .read32(address.wrapping_add(at as i32))
            .unwrap_or_default();
        match func {
            Some(func) => {
                let signature = module
                    .func_type(*func)
                    .map_or_else(|| "?".to_string(), unwasm_core::codegen::signature_text);
                let name = module
                    .func_name(*func)
                    .map(str::to_string)
                    .or_else(|| {
                        analysis
                            .derived_names
                            .get(func)
                            .map(|derived| format!("{} (guessed)", derived.name))
                    })
                    .unwrap_or_default();
                let _ = writeln!(
                    out,
                    "  slot {slot:<3} +{at:<5} {word:>10}   f{func:<6} {signature}{}{name}",
                    if name.is_empty() { "" } else { "  " }
                );
            }
            None if word == 0 => {
                pure.push(slot);
                let _ = writeln!(
                    out,
                    "  slot {slot:<3} +{at:<5} {word:>10}   NULL — pure virtual, or an operation \
                     this build does not provide;\n{:<34}a call here is call_indirect(0), \
                     which traps",
                    ""
                );
            }
            None => {
                let _ = writeln!(
                    out,
                    "  slot {slot:<3} +{at:<5} {word:>10}   not a live table index{}",
                    match image.text(word) {
                        Some(text) => format!(" — points at \"{}\"", text.escape_default()),
                        None => String::new(),
                    }
                );
            }
        }
    }

    if !pure.is_empty() {
        let _ = writeln!(
            out,
            "\n{} null slot{}: {}.\nA `call_indirect` reaching one of these takes table \
             index 0, whose signature is not the\ncall site's, so the engine traps — which \
             kills the thread rather than returning an\nerror anybody catches.",
            pure.len(),
            if pure.len() == 1 { "" } else { "s" },
            pure.iter()
                .map(|slot| format!("{slot} (+{})", slot * 4))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    Ok(out)
}

/// Every function that reads or writes a given offset.
///
/// "Who writes byte +846 of this struct" is answerable from the module, and the
/// way it was being answered — decompiling candidates and grepping the text —
/// misses the ones the compiler wrote through a displaced base. So this follows
/// the displacement.
fn stores(arguments: &[String]) -> Result<String, String> {
    if wants_help(arguments) {
        return help_for("stores");
    }
    let (path, rest) = positional(arguments, "stores")?;
    let mut offset: Option<i64> = None;
    let mut width: Option<u32> = None;
    let mut kind = unwasm_core::analysis::Kind::Store;
    let mut exact = false;
    let mut rest = rest.iter();
    while let Some(argument) = rest.next() {
        match argument.as_str() {
            "--offset" => {
                let value = rest.next().ok_or("--offset needs a number")?;
                offset = Some(number(value, "--offset")?);
            }
            "--size" => {
                let value = rest.next().ok_or("--size needs 1, 2, 4 or 8")?;
                let size = number(value, "--size")?;
                if !matches!(size, 1 | 2 | 4 | 8) {
                    return Err(format!("--size is 1, 2, 4 or 8, not `{value}`"));
                }
                width = Some(size as u32);
            }
            "--kind" => {
                let value = rest.next().ok_or("--kind is store, load or both")?;
                kind = match value.as_str() {
                    "store" => unwasm_core::analysis::Kind::Store,
                    "load" => unwasm_core::analysis::Kind::Load,
                    "both" => unwasm_core::analysis::Kind::Both,
                    other => return Err(format!("--kind is store, load or both, not `{other}`")),
                };
            }
            "--loads" => kind = unwasm_core::analysis::Kind::Load,
            "--exact" => exact = true,
            other => return Err(unexpected("stores", other)),
        }
    }
    let offset = offset.ok_or("stores needs --offset <n>")?;

    let module = read(path)?;
    let analysis = unwasm_core::analysis::analyse(&module);
    let report = unwasm_core::analysis::accesses_at(&module, offset, width, kind, exact);

    let mut out = String::new();
    let mut through_a_base = 0usize;
    for access in &report.found {
        let name = unwasm_core::codegen::function_ident(access.func, &analysis);
        let what = format!(
            "{}{}",
            if access.load { "load" } else { "store" },
            access.width * 8
        );
        // The frame base by name: `l2 + 846` in a function whose frame lives in
        // local 2 is the shadow stack, and saying so saves the reader looking
        // it up.
        let base_name = |local: u32| match analysis.frames.get(&access.func) {
            Some(frame) if frame.base_local == local => "frame".to_string(),
            _ => format!("l{local}"),
        };
        // The effective offset rather than the one asked for: under --exact the
        // two are not the same number, and printing the request back would say
        // a write lands somewhere it does not.
        let effective = access.effective();
        let (where_, note) = match access.address {
            unwasm_core::analysis::AddressOf::Local {
                local,
                displacement: 0,
            } => (format!("{}+{effective}", base_name(local)), String::new()),
            unwasm_core::analysis::AddressOf::Local {
                local,
                displacement,
            } => {
                through_a_base += 1;
                (
                    format!("{}+{effective}", base_name(local)),
                    format!(
                        "  through a base at {}{}{}, so the instruction encodes {}",
                        base_name(local),
                        if displacement < 0 { "-" } else { "+" },
                        displacement.abs(),
                        access.encoded
                    ),
                )
            }
            unwasm_core::analysis::AddressOf::Absolute(_) => (
                format!("{effective}"),
                "  a constant address, not a field of anything".to_string(),
            ),
            unwasm_core::analysis::AddressOf::Unknown => (
                format!("?+{}", access.encoded),
                "  through an address this could not follow".to_string(),
            ),
        };
        let _ = writeln!(
            out,
            "{name:<38} {what:<8} op #{:<6}{}  {where_}{note}",
            access.position,
            match access.file_offset {
                Some(at) => format!(" file {at:<9}"),
                None => String::new(),
            }
        );
    }
    let found = report.found;

    let kind = match kind {
        unwasm_core::analysis::Kind::Store => "write",
        unwasm_core::analysis::Kind::Load => "read",
        unwasm_core::analysis::Kind::Both => "touch",
    };
    Ok(format!(
        "{out}\n{} access{} {kind} offset {offset}{}{}{}\n",
        found.len(),
        if found.len() == 1 { "" } else { "es" },
        width.map_or(String::new(), |width| format!(
            " at {width} byte{}",
            if width == 1 { "" } else { "s" }
        )),
        if exact {
            ", matching the encoded offset only"
        } else {
            ""
        },
        if through_a_base > 0 {
            format!(
                "\n{through_a_base} of them reach it through a displaced base, and encode a \
                 different number — a\ngrep for {offset} in decompiled output would not have \
                 found {}.",
                if through_a_base == 1 { "it" } else { "them" }
            )
        } else {
            String::new()
        }
    ) + &if found.is_empty() {
        String::new()
    } else {
        // The next command, spelled out: a list of indices is not an answer
        // until something reads them.
        let mut functions: Vec<u32> = found.iter().map(|access| access.func).collect();
        functions.sort_unstable();
        functions.dedup();
        let indices: Vec<String> = functions.iter().map(u32::to_string).collect();
        format!(
            "\nRead {}: unwasm decompile {path} --only {} --bare\n",
            if indices.len() == 1 { "it" } else { "them" },
            indices.join(",")
        )
    } + &if report.lost.is_empty() {
        String::new()
    } else {
        // Said rather than swallowed: an empty answer from a search that
        // skipped functions means nothing.
        format!(
            "\nThe operand stack could not be followed in {} function{} \
             ({}{}), and nothing\nafter that point in them was searched.\n",
            report.lost.len(),
            if report.lost.len() == 1 { "" } else { "s" },
            report
                .lost
                .iter()
                .take(6)
                .map(|index| format!("f{index}"))
                .collect::<Vec<_>>()
                .join(", "),
            if report.lost.len() > 6 { ", …" } else { "" }
        )
    })
}

/// Whether the arguments ask for help rather than for work.
///
/// Checked before anything is parsed, because `unwasm decompile --help` used to
/// be read as "decompile the module named `--help`" and answered with a file
/// system error. A tool nobody can ask about is a tool nobody uses twice.
fn wants_help(arguments: &[String]) -> bool {
    arguments
        .iter()
        .any(|argument| argument == "-h" || argument == "--help")
}

/// Splits off the module path, which every command takes first.
///
/// The order is not negotiable — the flags are parsed positionally after it —
/// so when a flag comes first the module ends up swallowed as some flag's
/// value, and the error that reaches the user names an index rather than the
/// order. This says the order instead.
fn positional<'a>(
    arguments: &'a [String],
    command: &str,
) -> Result<(&'a str, &'a [String]), String> {
    match arguments.first() {
        None => Err(format!("{command} needs a module path")),
        Some(first) if first.starts_with('-') => {
            let path = arguments
                .iter()
                .skip(1)
                .find(|argument| argument.ends_with(".wasm"));
            Err(match path {
                Some(path) => format!(
                    "the module comes first: `unwasm {command} {path} {}`.\n       \
                     `{first}` came first, so {path} was read as a flag's value \
                     rather than as the module.",
                    arguments
                        .iter()
                        .filter(|argument| *argument != path)
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(" ")
                ),
                None => format!(
                    "the module comes first: `unwasm {command} <module.wasm> …`, \
                     and `{first}` is a flag.\n       Try `unwasm {command} --help`."
                ),
            })
        }
        Some(path) => Ok((path.as_str(), &arguments[1..])),
    }
}

/// The error an unrecognised argument gets, with the ordering rule when the
/// argument looks like it was meant to be the module.
fn unexpected(command: &str, argument: &str) -> String {
    if argument.starts_with('-') {
        format!("unexpected argument `{argument}`\n       Try `unwasm {command} --help`.")
    } else {
        format!(
            "unexpected argument `{argument}`: `{command}` takes the module first \
             and flags after it.\n       Try `unwasm {command} --help`."
        )
    }
}

/// Per-command help.
///
/// The whole `USAGE` is four screens, and a reader who typed one verb wants one
/// verb's worth of it.
const HELP: &[(&str, &str)] = &[
    (
        "decompile",
        "\
usage: unwasm decompile <module.wasm> [-o <out>] [--level <n>] [--split <n>]
                        [--only <indices>] [--with-callees] [--bare] [--spans]
                        [--reachable-from <indices>] [--direct-only]
                        [--signatures <file>] [--stub-recognised]
                        [--instrument-stores] [--offsets]

  -o <out>       a path ending in .rs writes one file; any other path is a
                 directory, written as mod.rs plus part0.rs, part1.rs, …
                 Without -o, the Rust goes to stdout.
  --split <n>    roughly how many lines to put in each part file. Implies a
                 directory.
  --only <list>  decompile only these function indices; the rest keep their
                 signatures and become `unimplemented!()`, so the result
                 still compiles.
  --with-callees  with --only: and everything they call directly
  --bare         with --only: emit *only* those functions, with no stubs, no
                 runtime and no imports. The result does not compile — it is
                 for reading, and it is a few dozen lines rather than fifteen
                 thousand stubs to slice with awk.
  --spans        print `index  name  first  last` for the generated file
                 instead of the Rust, so a slice of it can be cut exactly.
  --reachable-from <list>  decompile what these can reach, stub the rest
  --direct-only  with --reachable-from: follow direct calls only
  --level <n>    0 faithful, 1 frame slots as bindings, 2 also RTTI names
  --signatures <file>  name library code using a catalogue from `signatures`
  --stub-recognised  with --signatures: leave recognised bodies out
  --instrument-stores  route every memory write through the watchpoint runtime
  --offsets      also write offsets.json: which wasm bytes made each line

A directory output also gets `names.json`: every function's index, name, file,
line and table slots.",
    ),
    (
        "data",
        "\
usage: unwasm data <module.wasm> <address> [<length>]

Reads guest memory at an address, as the data segments leave it before anything
runs. `<address>` may be decimal or 0x-prefixed; `<length>` defaults to 32.

This is the command `bytes` is not: `bytes` takes an offset into the wasm file,
and converting an address into one by hand means subtracting a constant that
holds for one segment and lands past the end of the file for the next. It
prints which segment covers the address, the file offset of the byte — the
number `bytes` and `patch` take — the hex, and the words as u32 little-endian
with any string or in-data pointer each one resolves to.

An address no segment covers is not an error: it is memory the module never
initialises, and it reads as zero at run time.",
    ),
    (
        "vtable",
        "\
usage: unwasm vtable <module.wasm> <address> [--slots <n>]
       unwasm vtable <module.wasm> --class <name> [--slots <n>]

Reads a C++ vtable: each slot as a table index, the function it names, and its
signature. `<address>` is where the *slots* start — the value an object holds —
so the Itanium header (offset-to-top, type_info) is the two words before it.
`--class` takes the name `unwasm classes` prints and finds the address itself.

A slot holding 0 is a pure virtual function. Calling one is
`call_indirect(0)`, which reaches table slot 0 — a signature mismatch, and a
trap that kills the thread. Those slots are called out, because a vtable whose
zero slot is reached looks from the outside like the engine dying for no
reason.

Without --slots the read stops where the vtable stops looking like one: at a
word that is neither zero nor a live table index.",
    ),
    (
        "stores",
        "\
usage: unwasm stores <module.wasm> --offset <n> [--size 1|2|4|8]
                     [--kind store|load|both] [--loads] [--exact]

Every function that writes (or reads) a constant offset. The question is \"who
writes byte +846 of this struct\", and the answer used to be decompiling
candidates and grepping the text.

Compilers do not always write the offset you are looking for. A function given
a pointer eight bytes into a struct writes the field at +846 as +838, and one
that computes `base = p - 8` first writes it as +854. So the default follows
the constant displacements a function applies to its own parameters and locals
and reports the *effective* offset relative to the base, saying which local it
went through. `--exact` turns that off and matches the encoded offset only.",
    ),
    (
        "host",
        "\
usage: unwasm host <module.wasm> [-o <host.rs>] [--defaults]

Writes a skeleton `impl Imports`: every import the module still needs, grouped
by where it comes from, each one a `todo!()`. `--defaults` answers what nothing
here can with the zero of its type and records that it did.",
    ),
    (
        "table",
        "\
usage: unwasm table <module.wasm> [--type <signature>]

What the function table holds, slot by slot, with each entry's signature.
`call_indirect` takes a table index rather than a function index, so this is
what says which slot a call site reaches. `--type \"(i32,i32,i32)->()\"`
narrows it to one signature.",
    ),
    (
        "calls",
        "\
usage: unwasm calls <module.wasm> <index>

What reaches a function: its callers, its callees, the table slots it sits in,
and the signatures it calls through the table. The module says what each
function calls; nothing in it says what calls a given one.",
    ),
    (
        "signatures",
        "\
usage: unwasm signatures <module.wasm> [-o <sigs.txt>]

Writes a catalogue of fingerprint-to-name from a module that kept its names.
`decompile --signatures <file>` uses one to name library code in a module that
did not.",
    ),
    (
        "classes",
        "\
usage: unwasm classes <module.wasm> [--methods]

The C++ RTTI: every class the module declares a `type_info` for, its mangled
name, and the functions its vtable holds. `--methods` lists the vtable slot by
slot. The vtable address printed is the Itanium header; the slots start eight
bytes after it, and `unwasm vtable --class <name>` reads them from there.",
    ),
    (
        "frames",
        "\
usage: unwasm frames <module.wasm> [--outside]

Each function's stack frame. `--outside` lists the ones whose stores land past
the end of their own frame — the short list of suspects when an address is
being corrupted.",
    ),
    (
        "bytes",
        "\
usage: unwasm bytes <module.wasm> <offset> <length>

The bytes at an offset *into the wasm file*, and whether that sequence is
unique. For an address in guest memory, use `unwasm data`.",
    ),
    (
        "constants",
        "\
usage: unwasm constants <module.wasm> <value> [--data]

Every site that pushes a value, with the file offset and encoded length of
each. `--data` also lists every address in the data segments that holds those
four bytes — which is where a function pointer installed in a vtable lives, and
no instruction ever pushes it.",
    ),
    (
        "patch",
        "\
usage: unwasm patch <module.wasm> <offset> <value> -o <patched.wasm>

Rewrites the `i32.const` at a file offset, keeping the encoding the same length
so nothing else in the module moves.",
    ),
    (
        "inspect",
        "\
usage: unwasm inspect <module.wasm>

What the module is, in a screen: counts, memory, imports, the stack pointer,
frames, embind registrations and exports.",
    ),
];

fn help_for(command: &str) -> Result<String, String> {
    HELP.iter()
        .find(|(name, _)| *name == command)
        .map(|(_, text)| format!("{text}\n"))
        .ok_or_else(|| format!("no help for `{command}`"))
}

fn read(path: &str) -> Result<Module, String> {
    let bytes =
        std::fs::read(Path::new(path)).map_err(|error| format!("reading {path}: {error}"))?;
    Module::parse(&bytes).map_err(|error| error.to_string())
}
