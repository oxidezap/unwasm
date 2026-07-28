//! The real modules.
//!
//! Fixtures are what a decompiler is developed against; shipped modules are what
//! it is judged by. These run over the WhatsApp Web captures — minified,
//! Emscripten-built, up to 9 MiB — and they found the two bugs that fixtures did
//! not: a value spilled inside a block and consumed after it, and an `Imports`
//! trait emitted without return types.
//!
//! They are `#[ignore]`d because they are slow, not because they are optional:
//!
//! ```sh
//! cargo test --test captured -- --ignored --nocapture
//! ```
//!
//! The captures live in the whatsapp-rust checkout and are not copied here — a
//! second copy drifts from the one the protocol notes refer to. `WA_WASM_DIR`
//! points somewhere else.

mod common;

use unwasm_core::{Module, codegen};

/// Every capture, with what is known about it.
const CAPTURES: &[(&str, &str)] = &[
    ("COs9e0Kj0ic", "VOPRF / crypto, 236 KiB"),
    ("php8T1oSIZM", "mozjpeg, 376 KiB"),
    ("9Nbh3eMuVjD", "2.9 MiB"),
    ("ayqr5HQtlkb", "2.0 MiB"),
    ("rogm88TRRiw", "2.1 MiB"),
    ("D5pLH9sfOOl", "VoIP / PJSIP, 9.4 MiB — imports its memory"),
];

#[test]
#[ignore = "reads the capture directory and decompiles several megabytes"]
fn every_capture_either_decompiles_or_says_why_not() {
    let mut seen = 0;
    for (id, description) in CAPTURES {
        let Some(bytes) = common::captured(id) else {
            eprintln!("skipping: {id} unavailable (set WA_WASM_DIR)");
            continue;
        };
        seen += 1;
        match Module::parse(&bytes) {
            Ok(module) => {
                let code = codegen::generate(&module).unwrap_or_else(|error| {
                    panic!("{id} ({description}) parsed but did not generate: {error}")
                });
                assert!(code.contains("pub struct Instance"));
                eprintln!(
                    "{id}: {} functions, {} lines of Rust — {description}",
                    module.funcs.len(),
                    code.lines().count()
                );
            }
            Err(error) => {
                // A refusal is a result, as long as it names the construct. The
                // VoIP module imports its memory, which this version does not
                // model; what must never happen is a module that decompiles
                // into something that does not mean the same thing.
                let message = error.to_string();
                assert!(message.contains("unsupported"), "{id}: {message}");
                eprintln!("{id}: refused — {message}");
            }
        }
    }
    assert!(
        seen > 0,
        "no captures were found; this test compared nothing. Set WA_WASM_DIR."
    );
}

/// The smallest capture, taken all the way: decompiled, compiled by rustc, and
/// instantiated. Instantiation runs the module's own `__wasm_call_ctors` and
/// every static initialiser with it, which is a far broader exercise of the
/// generated code than any fixture.
#[test]
#[ignore = "compiles 70k lines of generated Rust"]
fn the_smallest_capture_compiles_and_instantiates() {
    let Some(bytes) = common::captured("COs9e0Kj0ic") else {
        panic!("COs9e0Kj0ic is not available; set WA_WASM_DIR");
    };
    const DRIVER: &str = r#"
mod generated;
fn main() {
    let instance = generated::Instance::new();
    println!("{}", instance.memory.data.len());
}
"#;
    let output = common::run_with_driver("capture-crypto", &bytes, DRIVER);
    let pages: usize = output.trim().parse().expect("a memory size");
    assert!(pages > 0, "the module instantiated with an empty memory");
    assert_eq!(pages % 65536, 0, "memory is a whole number of pages");
}

/// A real module in the split layout, compiled and instantiated.
///
/// The smallest capture is 478 functions, so the default layout already splits
/// it — which makes this the test that the part files hold together on
/// something that was not written to be easy.
#[test]
#[ignore = "compiles 70k lines of generated Rust"]
fn a_capture_compiles_in_the_split_layout() {
    let Some(bytes) = common::captured("COs9e0Kj0ic") else {
        panic!("COs9e0Kj0ic is not available; set WA_WASM_DIR");
    };
    let module = Module::parse(&bytes).expect("it parses");
    let layout = codegen::Layout::for_module(&module);
    assert!(
        matches!(layout, codegen::Layout::Split { .. }),
        "478 functions should split by default, got {layout:?}"
    );
    let files = codegen::generate_files(&module, layout).expect("it generates");
    assert!(files.len() > 1, "the layout produced no parts");

    const DRIVER: &str = r#"
mod generated;
fn main() {
    let instance = generated::Instance::new();
    println!("{}", instance.memory.data.len());
}
"#;
    let output = common::run_with_driver("capture-split", &bytes, DRIVER);
    let bytes_of_memory: usize = output.trim().parse().expect("a memory size");
    assert_eq!(bytes_of_memory % 65536, 0);
    eprintln!(
        "COs9e0Kj0ic: {} files, {} bytes of memory after instantiation",
        files.len(),
        bytes_of_memory
    );
}
