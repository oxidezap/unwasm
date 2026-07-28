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
  unwasm inspect   <module.wasm>

  -o <out>       a path ending in .rs writes one file; any other path is a
                 directory, written as mod.rs plus part0.rs, part1.rs, …
                 Without -o, the Rust goes to stdout.
  --split <n>    functions per part file. Implies a directory. Without it, a
                 directory gets the layout the module's size calls for.

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
            Some(functions_per_file) => codegen::Layout::Split { functions_per_file },
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
    match module.memory {
        Some(memory) => out.push_str(&format!(
            "memory: {} pages initial{}\n",
            memory.min_pages,
            memory
                .max_pages
                .map_or(String::new(), |max| format!(", {max} maximum"))
        )),
        None => out.push_str("memory: none\n"),
    }
    if !module.func_imports.is_empty() {
        out.push_str("\nimports:\n");
        for import in &module.func_imports {
            out.push_str(&format!("  {}::{}\n", import.module, import.field));
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
