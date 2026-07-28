//! The differential harness: the module against its decompilation.
//!
//! A decompiler can only claim to be faithful if something checks. Here the
//! check is execution: the same calls run twice, once on the wasm module in a
//! real engine and once on the Rust this crate emitted, and every result — the
//! returned value, the trap, and the whole of linear memory — has to match.
//!
//! The engine is node's, reached through a small driver script. Using an engine
//! we did not write is the point: an interpreter of our own would share our
//! misreadings, and agreeing with yourself proves nothing.
//!
//! Nothing here is skipped when a tool is missing. A harness that quietly
//! degrades to "no comparison ran" reports the same green as one that compared
//! everything, and that is how a decompiler ships a wrong answer.

#![allow(dead_code, reason = "each test binary uses a different part of this")]

use std::path::{Path, PathBuf};
use std::process::Command;

use unwasm_core::{Module, codegen};

/// An argument to a call under test.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Arg {
    I32(i32),
    I64(i64),
    F32(f32),
    F64(f64),
}

impl Arg {
    fn rust_literal(self) -> String {
        match self {
            Self::I32(value) if value == i32::MIN => "i32::MIN".to_string(),
            Self::I64(value) if value == i64::MIN => "i64::MIN".to_string(),
            Self::I32(value) => format!("{value}i32"),
            Self::I64(value) => format!("{value}i64"),
            Self::F32(value) => format!("f32::from_bits({}u32)", value.to_bits()),
            Self::F64(value) => format!("f64::from_bits({}u64)", value.to_bits()),
        }
    }

    /// How the value is written into the node driver's JSON.
    fn json(self) -> String {
        match self {
            Self::I32(value) => format!("{{\"kind\":\"i32\",\"value\":{value}}}"),
            // i64 goes through as a decimal string: JSON numbers lose the low
            // bits of a 64-bit value, and losing them here would make the two
            // sides agree on a number neither of them ran.
            Self::I64(value) => format!("{{\"kind\":\"i64\",\"value\":\"{value}\"}}"),
            // Float bits travel as strings for the same reason: JSON's number
            // is a double, so the bit pattern of `f64::MAX` parses back as the
            // bits of infinity — and the two sides then agree about a value
            // neither of them was asked to compute.
            Self::F32(value) => format!("{{\"kind\":\"f32\",\"bits\":\"{}\"}}", value.to_bits()),
            Self::F64(value) => format!("{{\"kind\":\"f64\",\"bits\":\"{}\"}}", value.to_bits()),
        }
    }
}

/// One exported function, called with one set of arguments.
#[derive(Debug, Clone)]
pub struct Call {
    pub export: String,
    pub args: Vec<Arg>,
}

/// Builds a call.
pub fn call(export: &str, args: &[Arg]) -> Call {
    Call {
        export: export.to_string(),
        args: args.to_vec(),
    }
}

/// Where scratch files for a run go. Under `target/`, so `cargo clean` clears
/// them and two test binaries running at once cannot collide.
fn workspace_scratch(name: &str) -> PathBuf {
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.push("../../target/unwasm-tests");
    path.push(name);
    std::fs::create_dir_all(&path).expect("creating the scratch directory");
    path
}

fn tool(name: &str) -> Command {
    // A missing tool fails the test rather than skipping it: the alternative is
    // a green run that compared nothing.
    assert!(
        Command::new(name)
            .arg("--version")
            .output()
            .is_ok_and(|output| output.status.success()),
        "`{name}` is required by the differential harness and is not runnable"
    );
    Command::new(name)
}

/// Assembles a `.wat` source into a module, via wasm-tools.
pub fn assemble(name: &str, wat: &str) -> Vec<u8> {
    let scratch = workspace_scratch(name);
    let source = scratch.join("module.wat");
    let binary = scratch.join("module.wasm");
    std::fs::write(&source, wat).expect("writing the wat source");
    let output = tool("wasm-tools")
        .arg("parse")
        .arg(&source)
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("running wasm-tools");
    assert!(
        output.status.success(),
        "wasm-tools rejected the fixture:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    std::fs::read(&binary).expect("reading the assembled module")
}

/// Locates `emcc`.
///
/// The Arch package puts it in `/usr/lib/emscripten` and adds that to `PATH`
/// through `/etc/profile.d`, which only applies to shells started afterwards —
/// so a `cargo test` in an older shell would not find it. Looking in the known
/// location as well means the tests do not depend on which shell ran them.
fn emcc() -> Option<PathBuf> {
    if Command::new("emcc").arg("--version").output().is_ok() {
        return Some(PathBuf::from("emcc"));
    }
    let packaged = PathBuf::from("/usr/lib/emscripten/emcc");
    packaged.exists().then_some(packaged)
}

/// Compiles a C or C++ fixture with Emscripten.
///
/// `-sSTANDALONE_WASM` keeps the module runnable without the JavaScript glue,
/// which is what lets the differential harness call into it. The code inside is
/// the real thing regardless: Emscripten's libc, its `malloc`, its vtables and
/// its shadow stack.
///
/// # Panics
///
/// Panics if `emcc` is not installed. Every caller is `#[ignore]`d for that
/// reason — but once it runs, a missing toolchain must fail rather than pass
/// having compiled nothing.
pub fn compile_emscripten(name: &str, source: &str, extension: &str, flags: &[&str]) -> Vec<u8> {
    let scratch = workspace_scratch(name);
    let file = scratch.join(format!("fixture.{extension}"));
    let output_js = scratch.join("fixture.js");
    std::fs::write(&file, source).expect("writing the fixture");

    let emcc = emcc().expect("emcc is required by this test; install the `emscripten` package");
    let output = Command::new(emcc)
        .arg(&file)
        .args(["-sSTANDALONE_WASM", "--no-entry"])
        .args(flags)
        .arg("-o")
        .arg(&output_js)
        .output()
        .expect("running emcc");
    assert!(
        output.status.success(),
        "emcc rejected the fixture:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    std::fs::read(scratch.join("fixture.wasm")).expect("reading the compiled module")
}

/// Compiles a C fixture to wasm with clang.
///
/// clang's wasm32 target is the same LLVM backend Emscripten drives, so the
/// generated code — the shadow stack, the `br_table` dispatch, the way locals
/// are promoted — has the shape the real modules have, without waiting on an
/// emsdk install for every iteration.
pub fn compile_c(name: &str, source: &str, optimisation: &str) -> Vec<u8> {
    let scratch = workspace_scratch(name);
    let file = scratch.join("fixture.c");
    let binary = scratch.join("fixture.wasm");
    std::fs::write(&file, source).expect("writing the C fixture");
    let output = tool("clang")
        .args(["--target=wasm32", "-nostdlib", optimisation])
        // `--export-all` rather than per-function attributes: the fixtures stay
        // plain C, and a module that exports its internals is closer to what a
        // debug build ships anyway.
        .args([
            "-Wl,--no-entry",
            "-Wl,--export-all",
            "-Wl,--allow-undefined",
        ])
        .arg("-o")
        .arg(&binary)
        .arg(&file)
        .output()
        .expect("running clang");
    assert!(
        output.status.success(),
        "clang rejected the fixture:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    std::fs::read(&binary).expect("reading the compiled module")
}

/// Runs the calls in node, against the wasm module itself.
fn run_in_node(name: &str, wasm: &[u8], calls: &[Call], module: &Module) -> Vec<String> {
    let scratch = workspace_scratch(name);
    let binary = scratch.join("subject.wasm");
    std::fs::write(&binary, wasm).expect("writing the module for node");
    let driver = scratch.join("driver.js");
    std::fs::write(&driver, NODE_DRIVER).expect("writing the node driver");

    let plan = calls
        .iter()
        .map(|call| {
            let args: Vec<String> = call.args.iter().map(|arg| arg.json()).collect();
            format!(
                "{{\"export\":\"{}\",\"args\":[{}],\"result\":\"{}\"}}",
                call.export,
                args.join(","),
                result_kind(module, &call.export)
            )
        })
        .collect::<Vec<_>>()
        .join(",");

    let output = tool("node")
        .arg(&driver)
        .arg(&binary)
        .arg(format!("[{plan}]"))
        .output()
        .expect("running node");
    assert!(
        output.status.success(),
        "the node driver failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::to_string)
        .collect()
}

/// The result type of an exported function, as the drivers need to print it.
fn result_kind(module: &Module, export: &str) -> String {
    let index = module
        .exports
        .iter()
        .find(|candidate| candidate.name == export)
        .unwrap_or_else(|| panic!("the module exports no `{export}`"))
        .index;
    let ty = module.func_type(index).expect("the export has a signature");
    match ty.results.first() {
        None => "void".to_string(),
        Some(result) => result.rust_name().to_string(),
    }
}

/// Writes the decompilation to the scratch directory in the given layout.
///
/// Both layouts land at `mod generated;`: Rust resolves that to `generated.rs`
/// or to `generated/mod.rs`, so the driver is identical either way and the two
/// layouts are compared by exactly the same tests.
fn write_generated(scratch: &Path, module: &Module, layout: codegen::Layout) {
    let files = codegen::generate_files(module, layout).expect("generating Rust");
    let single = scratch.join("generated.rs");
    let directory = scratch.join("generated");
    // Clear whichever form a previous run left, or `mod generated;` becomes
    // ambiguous and the compile fails for a reason that has nothing to do with
    // the test.
    let _ = std::fs::remove_file(&single);
    let _ = std::fs::remove_dir_all(&directory);

    if files.len() == 1 {
        std::fs::write(&single, &files[0].contents).expect("writing the generated module");
        return;
    }
    std::fs::create_dir_all(&directory).expect("creating the module directory");
    for file in &files {
        std::fs::write(directory.join(&file.name), &file.contents).expect("writing a part");
    }
}

/// Runs the same calls against the generated Rust.
fn run_in_rust(name: &str, module: &Module, calls: &[Call]) -> Vec<String> {
    run_in_rust_with_layout(name, module, calls, codegen::Layout::Single)
}

fn run_in_rust_with_layout(
    name: &str,
    module: &Module,
    calls: &[Call],
    layout: codegen::Layout,
) -> Vec<String> {
    let scratch = workspace_scratch(name);
    write_generated(&scratch, module, layout);

    let mut main = String::from(
        "mod generated;\n\
         \n\
         fn checksum(bytes: &[u8]) -> String {\n\
         \x20   let mut hash: u64 = 0xcbf2_9ce4_8422_2325;\n\
         \x20   for byte in bytes {\n\
         \x20       hash ^= u64::from(*byte);\n\
         \x20       hash = hash.wrapping_mul(0x0000_0100_0000_01b3);\n\
         \x20   }\n\
         \x20   format!(\"{:016x}:{}\", hash, bytes.len())\n\
         }\n\
         \n\
         fn main() {\n\
         \x20   std::panic::set_hook(Box::new(|_| {}));\n\
         \x20   let mut instance = generated::Instance::new();\n",
    );
    for call in calls {
        let arguments: Vec<String> = call.args.iter().map(|arg| arg.rust_literal()).collect();
        let kind = result_kind(module, &call.export);
        let printer = match kind.as_str() {
            "void" => "String::from(\"void\")",
            // Floats are compared by their bits: two engines can print the same
            // value differently, and NaN prints the same as another NaN with a
            // different payload.
            "f32" | "f64" => {
                "if value.is_nan() { String::from(\"nan\") } else { format!(\"{}\", value.to_bits()) }"
            }
            _ => "format!(\"{}\", value)",
        };
        // Each call runs inside `catch_unwind` so a trap is a comparable
        // outcome rather than the end of the run: the engine reports one as an
        // exception and carries on, and so must this side.
        main.push_str(&format!(
            "    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {{\n\
             \x20       let value = instance.{}({});\n\
             \x20       {printer}\n\
             \x20   }}));\n\
             \x20   println!(\"{{}}\", outcome.unwrap_or_else(|_| String::from(\"trap\")));\n",
            export_ident(module, &call.export),
            arguments.join(", ")
        ));
    }
    // The engine can only be asked for memory the module exports, so the two
    // sides compare memory exactly when the module publishes it.
    let exports_memory = module
        .exports
        .iter()
        .any(|export| export.kind == unwasm_core::module::ExportKind::Memory);
    if exports_memory {
        main.push_str("    println!(\"memory {}\", checksum(&instance.memory.data));\n}\n");
    } else {
        main.push_str("    println!(\"memory none\");\n}\n");
    }
    std::fs::write(scratch.join("main.rs"), &main).expect("writing the driver");

    let binary = scratch.join("driver");
    let output = tool("rustc")
        .args(["--edition", "2024"])
        .arg("-o")
        .arg(&binary)
        .arg(scratch.join("main.rs"))
        .output()
        .expect("running rustc");
    assert!(
        output.status.success(),
        "the generated Rust does not compile — which is the one thing it must always do:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let run = Command::new(&binary).output().expect("running the driver");
    assert!(
        run.status.success(),
        "the generated driver crashed:\n{}",
        String::from_utf8_lossy(&run.stderr)
    );
    String::from_utf8_lossy(&run.stdout)
        .lines()
        .map(str::to_string)
        .collect()
}

/// How an export's name reaches the generated code.
///
/// Asks the backend rather than reimplementing its sanitising: a second copy of
/// that logic would drift, and the drift would look like a decompilation bug.
fn export_ident(module: &Module, name: &str) -> String {
    codegen::exported_functions(module)
        .into_iter()
        .find(|(export, _)| export.name == name)
        .map(|(_, ident)| ident)
        .unwrap_or_else(|| panic!("the module exports no `{name}`"))
}

/// Runs `calls` on both sides and asserts every line agrees.
///
/// # Panics
///
/// Panics with the first disagreement, naming the call that produced it.
pub fn assert_agrees(name: &str, wasm: &[u8], calls: &[Call]) {
    assert_agrees_with_layout(name, wasm, calls, codegen::Layout::Single);
}

/// As [`assert_agrees`], for a chosen layout.
///
/// Splitting the output across modules must not change what it does. It moves
/// functions between files and makes them `pub(crate)`; if that ever changed a
/// result, this is what would say so.
pub fn assert_agrees_with_layout(name: &str, wasm: &[u8], calls: &[Call], layout: codegen::Layout) {
    let module = Module::parse(wasm).expect("parsing the module under test");
    let engine = run_in_node(name, wasm, calls, &module);
    let ours = run_in_rust_with_layout(name, &module, calls, layout);

    assert_eq!(
        engine.len(),
        ours.len(),
        "the two sides produced different numbers of lines\nengine: {engine:#?}\nours: {ours:#?}"
    );
    for (at, (expected, actual)) in engine.iter().zip(ours.iter()).enumerate() {
        let what = calls
            .get(at)
            .map_or_else(|| "linear memory".to_string(), |call| format!("{call:?}"));
        assert_eq!(
            expected, actual,
            "the decompilation disagrees with the engine on {what}"
        );
    }
}

/// Decompiles and compiles, without running. For modules with no callable
/// entry point worth driving, where the claim under test is only that the
/// output builds.
pub fn assert_compiles(name: &str, wasm: &[u8]) {
    let module = Module::parse(wasm).expect("parsing the module under test");
    run_in_rust(name, &module, &[]);
}

/// Compiles the decompilation against a driver written by the caller, and
/// returns what it printed.
///
/// This is how the generated `Imports` trait gets exercised: a host that
/// answers is something only the caller can supply.
pub fn run_with_driver(name: &str, wasm: &[u8], driver: &str) -> String {
    let module = Module::parse(wasm).expect("parsing the module under test");
    let scratch = workspace_scratch(name);
    write_generated(&scratch, &module, codegen::Layout::for_module(&module));
    std::fs::write(scratch.join("main.rs"), driver).expect("writing the driver");

    let binary = scratch.join("driver");
    let output = tool("rustc")
        .args(["--edition", "2024"])
        .arg("-o")
        .arg(&binary)
        .arg(scratch.join("main.rs"))
        .output()
        .expect("running rustc");
    assert!(
        output.status.success(),
        "the generated Rust does not compile:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let run = Command::new(&binary).output().expect("running the driver");
    assert!(
        run.status.success(),
        "the driver failed:\n{}",
        String::from_utf8_lossy(&run.stderr)
    );
    String::from_utf8_lossy(&run.stdout).to_string()
}

/// Decompiles a module to Rust, for tests that read the output rather than run
/// it.
pub fn decompile(wasm: &[u8]) -> String {
    let module = Module::parse(wasm).expect("parsing the module under test");
    codegen::generate(&module).expect("generating Rust")
}

/// Reads a module from the wasm captures, if they are present.
pub fn captured(id: &str) -> Option<Vec<u8>> {
    let directory = std::env::var("WA_WASM_DIR").unwrap_or_else(|_| {
        format!(
            "{}/projects/whatsapp-rust/docs/captured-js/wasm",
            std::env::var("HOME").unwrap_or_default()
        )
    });
    let path = Path::new(&directory).join(format!("{id}.wasm"));
    std::fs::read(path).ok()
}

const NODE_DRIVER: &str = r#"
// Runs a plan of calls against a wasm module and prints one line per call, in
// the same canonical form the Rust driver prints.
const fs = require('fs');
const bytes = fs.readFileSync(process.argv[2]);
const plan = JSON.parse(process.argv[3]);

const instance = new WebAssembly.Instance(new WebAssembly.Module(bytes), {});
const f32bits = new DataView(new ArrayBuffer(8));

for (const step of plan) {
  const target = instance.exports[step.export];
  if (typeof target !== 'function') {
    console.log('missing export');
    continue;
  }
  const args = step.args.map((arg) => {
    if (arg.kind === 'i64') return BigInt(arg.value);
    if (arg.kind === 'f32') { f32bits.setUint32(0, Number(arg.bits)); return f32bits.getFloat32(0); }
    if (arg.kind === 'f64') { f32bits.setBigUint64(0, BigInt(arg.bits)); return f32bits.getFloat64(0); }
    return arg.value;
  });
  try {
    const value = target(...args);
    if (step.result === 'void') {
      console.log('void');
    } else if (step.result === 'f32') {
      // A NaN is reported as `nan`, not as its bits: wasm leaves the payload
      // and sign of a propagated NaN unspecified, so comparing them would be
      // comparing something the spec does not decide. That a NaN came back at
      // all is still compared.
      if (Number.isNaN(value)) { console.log('nan'); }
      else { f32bits.setFloat32(0, value); console.log(f32bits.getUint32(0).toString()); }
    } else if (step.result === 'f64') {
      if (Number.isNaN(value)) { console.log('nan'); }
      else { f32bits.setFloat64(0, value); console.log(f32bits.getBigUint64(0).toString()); }
    } else {
      console.log(value.toString());
    }
  } catch (error) {
    // Every wasm trap arrives as a RuntimeError; anything else is the harness
    // failing and must not be reported as agreement.
    if (error instanceof WebAssembly.RuntimeError) { console.log('trap'); }
    else { throw error; }
  }
}

const memory = instance.exports.memory;
if (memory) {
  const data = new Uint8Array(memory.buffer);
  let hash = 0xcbf29ce484222325n;
  const mask = 0xffffffffffffffffn;
  for (const byte of data) {
    hash ^= BigInt(byte);
    hash = (hash * 0x100000001b3n) & mask;
  }
  console.log('memory ' + hash.toString(16).padStart(16, '0') + ':' + data.length);
} else {
  console.log('memory none');
}
"#;
