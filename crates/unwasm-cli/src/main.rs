//! The command line.
//!
//! Argument parsing is a dozen lines here rather than a dependency: the tool
//! has two verbs, and a decompiler that takes ten seconds to rebuild is a
//! decompiler nobody runs twice.

use std::path::Path;
use std::process::ExitCode;

use unwasm_core::{Module, codegen};

const USAGE: &str = "\
unwasm — a WebAssembly decompiler whose output compiles

usage:
  unwasm decompile <module.wasm> [-o <out>] [--split <n>]
  unwasm host      <module.wasm> [-o <host.rs>]
  unwasm inspect   <module.wasm>

  -o <out>       a path ending in .rs writes one file; any other path is a
                 directory, written as mod.rs plus part0.rs, part1.rs, …
                 Without -o, the Rust goes to stdout.
  --split <n>    roughly how many lines to put in each part file. Implies a
                 directory. Without it, a directory gets the layout the
                 module's size calls for.

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
            print!("{output}");
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
        Some("inspect") => inspect(&arguments[1..]),
        Some("-h" | "--help" | "help") | None => Ok(USAGE.to_string()),
        Some(other) => Err(format!("unknown command `{other}`\n\n{USAGE}")),
    }
}

fn decompile(arguments: &[String]) -> Result<String, String> {
    let path = arguments.first().ok_or("decompile needs a module path")?;
    let mut destination = None;
    let mut split = None;
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
            other => return Err(format!("unexpected argument `{other}`")),
        }
    }

    let module = read(path)?;
    let Some(destination) = destination else {
        if split.is_some() {
            return Err("--split writes several files, so it needs -o <directory>".to_string());
        }
        return codegen::generate(&module).map_err(|error| error.to_string());
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

    let files = codegen::generate_files(&module, layout).map_err(|error| error.to_string())?;
    let lines: usize = files.iter().map(|file| file.contents.lines().count()).sum();

    if files.len() == 1 && single_file {
        std::fs::write(&destination, &files[0].contents)
            .map_err(|error| format!("writing {destination}: {error}"))?;
        return Ok(format!(
            "wrote {destination} ({lines} lines, {} functions)\n",
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
        "wrote {destination}/ ({} files, {lines} lines, {} functions)\n",
        files.len(),
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
            Ok(format!(
                "wrote {destination} ({} methods to implement)\n",
                skeleton.matches("    fn ").count()
            ))
        }
        None => Ok(skeleton),
    }
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
            .filter(|registration| registration.name.is_some())
            .count();
        out.push_str(&format!(
            "embind: {} registrations, {named} of them named\n",
            analysis.registrations.len()
        ));
        for registration in &analysis.registrations {
            if let Some(name) = &registration.name {
                out.push_str(&format!(
                    "  {} {name}\n",
                    registration.kind.trim_start_matches("_embind_register_")
                ));
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
