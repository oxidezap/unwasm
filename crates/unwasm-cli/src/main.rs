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
  unwasm decompile <module.wasm> [-o <out.rs>]   translate to Rust
  unwasm inspect   <module.wasm>                 what the module contains

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
    let code = codegen::generate(&module).map_err(|error| error.to_string())?;
    match destination {
        Some(destination) => {
            std::fs::write(&destination, &code)
                .map_err(|error| format!("writing {destination}: {error}"))?;
            Ok(format!(
                "wrote {destination} ({} lines, {} functions)\n",
                code.lines().count(),
                module.funcs.len()
            ))
        }
        None => Ok(code),
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
