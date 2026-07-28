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
  unwasm decompile <module.wasm> [-o <out>] [--split <n>] [--only <indices>]
  unwasm host      <module.wasm> [-o <host.rs>]
  unwasm table     <module.wasm> [--type <signature>]
  unwasm calls     <module.wasm> <index>
  unwasm signatures <module.wasm> [-o <sigs.txt>]
  unwasm frames    <module.wasm> [--outside]
  unwasm bytes     <module.wasm> <offset> <length>
  unwasm inspect   <module.wasm>

  -o <out>       a path ending in .rs writes one file; any other path is a
                 directory, written as mod.rs plus part0.rs, part1.rs, …
                 Without -o, the Rust goes to stdout.
  --split <n>    roughly how many lines to put in each part file. Implies a
                 directory. Without it, a directory gets the layout the
                 module's size calls for.
  --signatures <file>  name library code using a catalogue from `signatures`.
  --only <list>  decompile only these function indices
  --with-callees  and everything they call directly
  --instrument-stores  route every memory write through the watchpoint runtime,
                 so `instance.memory.watch(addr, len)` reports who wrote it
  --offsets      also write offsets.json: which wasm bytes made each line, comma-separated. The
                 rest keep their signatures and become `unimplemented!()`, so
                 the result still compiles - for reading three functions out of
                 thirteen thousand without producing 365 MB.

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
        Some("frames") => frames(&arguments[1..]),
        Some("bytes") => bytes(&arguments[1..]),
        Some("inspect") => inspect(&arguments[1..]),
        Some("-h" | "--help" | "help") | None => Ok(USAGE.to_string()),
        Some(other) => Err(format!("unknown command `{other}`\n\n{USAGE}")),
    }
}

fn decompile(arguments: &[String]) -> Result<String, String> {
    let path = arguments.first().ok_or("decompile needs a module path")?;
    let mut destination = None;
    let mut split = None;
    let mut only: std::collections::BTreeSet<u32> = std::collections::BTreeSet::new();
    let mut catalogue: Option<codegen::Signatures> = None;
    let mut with_callees = false;
    let mut instrument = false;
    let mut map_offsets = false;
    let mut rest = arguments[1..].iter();
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
            "--instrument-stores" => instrument = true,
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
            other => return Err(format!("unexpected argument `{other}`")),
        }
    }

    let module = read(path)?;
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
        if only.is_empty() && catalogue.is_none() && !instrument {
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
        },
    )
    .map_err(|error| error.to_string())?;
    let lines: usize = files.iter().map(|file| file.contents.lines().count()).sum();

    // A single-file destination gets the Rust; the index needs a directory to
    // live beside it.
    if single_file {
        std::fs::write(&destination, &files[0].contents)
            .map_err(|error| format!("writing {destination}: {error}"))?;
        return Ok(format!(
            "wrote {destination} ({} lines, {} functions)\n",
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
        "wrote {destination}/ ({} Rust files plus names.json, {lines} lines, {} functions)\n",
        files.len() - 1,
        module.funcs.len()
    ))
}

fn host(arguments: &[String]) -> Result<String, String> {
    let path = arguments.first().ok_or("host needs a module path")?;
    let mut destination = None;
    let mut rest = arguments[1..].iter();
    while let Some(argument) = rest.next() {
        match argument.as_str() {
            "-o" | "--output" => {
                destination = Some(rest.next().ok_or("-o needs a path")?.clone());
            }
            other => return Err(format!("unexpected argument `{other}`")),
        }
    }

    let module = read(path)?;
    let skeleton = codegen::generate_host(&module).map_err(|error| error.to_string())?;
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

/// Prints the bytes at an offset, and says whether they are unique.
///
/// The two questions a hand-written patch asks, and the two that were being
/// answered by computing LEB encodings by hand: what is actually there, and
/// will the pattern I am about to search for match one place or forty.
fn bytes(arguments: &[String]) -> Result<String, String> {
    let path = arguments.first().ok_or("bytes needs a module path")?;
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
    if let Some(extra) = arguments.get(3) {
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

fn frames(arguments: &[String]) -> Result<String, String> {
    let path = arguments.first().ok_or("frames needs a module path")?;
    let mut only_outside = false;
    for argument in &arguments[1..] {
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
    let path = arguments.first().ok_or("signatures needs a module path")?;
    let mut destination = None;
    let mut rest = arguments[1..].iter();
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
    let path = arguments.first().ok_or("calls needs a module path")?;
    let index = arguments
        .get(1)
        .ok_or("calls needs a function index")?
        .parse::<u32>()
        .map_err(|_| "calls takes a function index".to_string())?;
    if let Some(extra) = arguments.get(2) {
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
    let path = arguments.first().ok_or("table needs a module path")?;
    let mut wanted = None;
    let mut rest = arguments[1..].iter();
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
    let path = arguments.first().ok_or("inspect needs a module path")?;
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

fn read(path: &str) -> Result<Module, String> {
    let bytes =
        std::fs::read(Path::new(path)).map_err(|error| format!("reading {path}: {error}"))?;
    Module::parse(&bytes).map_err(|error| error.to_string())
}
