//! What the module registers with embind.
//!
//! embind is how a C++ module tells JavaScript what it exposes. The
//! registration runs at startup, but its arguments are constants in the code —
//! including the pointer to each name — so it can be read without running
//! anything.
//!
//! Everything else this crate recovers is what the compiler left behind. This
//! is what the author published, and on a stripped module it is the only such
//! thing there is: the VoIP module names `initVoipStack` and
//! `handleIncomingSignalingOffer` here and nowhere else.

mod common;

use unwasm_core::{Module, analysis, codegen};

/// A module that registers a class and a function, the way Emscripten does.
const REGISTERING: &str = r#"(module
    (import "env" "_embind_register_function"
        (func $register_function (param i32 i32 i32 i32 i32 i32 i32)))
    (import "env" "_embind_register_class"
        (func $register_class
            (param i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32)))
    (import "env" "_embind_register_class_function"
        (func $register_method (param i32 i32 i32 i32 i32 i32 i32 i32 i32)))
    (memory (export "memory") 1)
    (data (i32.const 100) "startVoipCall\00")
    (data (i32.const 200) "Uint8List\00")
    (data (i32.const 300) "push_back\00")
    (func (export "__wasm_call_ctors")
        ;; _embind_register_function(name = 100, ..)
        i32.const 100
        i32.const 0 i32.const 0 i32.const 0 i32.const 0 i32.const 0 i32.const 0
        call $register_function
        ;; _embind_register_class(.., className = 200 at argument 10, ..)
        i32.const 0 i32.const 0 i32.const 0 i32.const 0 i32.const 0
        i32.const 0 i32.const 0 i32.const 0 i32.const 0 i32.const 0
        i32.const 200
        i32.const 0 i32.const 0
        call $register_class
        ;; _embind_register_class_function(classType, methodName = 300, ..)
        i32.const 0
        i32.const 300
        i32.const 0 i32.const 0 i32.const 0 i32.const 0 i32.const 0 i32.const 0 i32.const 0
        call $register_method))"#;

#[test]
fn the_registered_api_is_read_without_running_anything() {
    let wasm = common::assemble("embind", REGISTERING);
    let module = Module::parse(&wasm).expect("valid");
    let registrations = analysis::analyse(&module).registrations;

    assert_eq!(registrations.len(), 3);
    let named: Vec<(&str, &str)> = registrations
        .iter()
        .filter_map(|registration| {
            registration
                .name
                .as_deref()
                .map(|name| (registration.kind.as_str(), name))
        })
        .collect();
    assert_eq!(
        named,
        [
            ("_embind_register_function", "startVoipCall"),
            ("_embind_register_class", "Uint8List"),
            ("_embind_register_class_function", "push_back"),
        ]
    );
}

#[test]
fn the_api_reaches_the_top_of_the_generated_module() {
    let wasm = common::assemble("embind-output", REGISTERING);
    let code = common::decompile(&wasm);
    assert!(
        code.contains("The API this module registers with embind"),
        "{}",
        &code[..code.len().min(1500)]
    );
    assert!(code.contains("//   function: startVoipCall"));
    assert!(code.contains("//   class: Uint8List"));
    assert!(code.contains("//   class function: push_back"));
    // It is a comment: nothing about the module's behaviour changed.
    common::assert_compiles("embind-output", &wasm);
}

#[test]
fn a_registration_assembled_at_run_time_is_reported_without_a_name() {
    // The name comes from a local rather than a constant: that it registered
    // something is still worth knowing, and what it called it is not knowable
    // from here.
    let wasm = common::assemble(
        "embind-computed",
        r#"(module
            (import "env" "_embind_register_function"
                (func $register (param i32 i32 i32 i32 i32 i32 i32)))
            (memory 1)
            (data (i32.const 100) "computed\00")
            (func (export "register_it") (param i32)
                local.get 0
                i32.const 0 i32.const 0 i32.const 0 i32.const 0 i32.const 0 i32.const 0
                call $register))"#,
    );
    let module = Module::parse(&wasm).expect("valid");
    let registrations = analysis::analyse(&module).registrations;
    assert_eq!(registrations.len(), 1, "the call is still reported");
    assert_eq!(registrations[0].name, None, "and not given a made-up name");
}

#[test]
fn a_name_pointing_at_nothing_readable_stays_unnamed() {
    let wasm = common::assemble(
        "embind-badname",
        r#"(module
            (import "env" "_embind_register_function"
                (func $register (param i32 i32 i32 i32 i32 i32 i32)))
            (memory 1)
            (func (export "register_it")
                i32.const 999999
                i32.const 0 i32.const 0 i32.const 0 i32.const 0 i32.const 0 i32.const 0
                call $register))"#,
    );
    let module = Module::parse(&wasm).expect("valid");
    let registrations = analysis::analyse(&module).registrations;
    assert_eq!(registrations[0].name, None);
}

#[test]
fn a_registration_with_no_name_argument_is_still_recorded() {
    // `_embind_register_class_constructor` names nothing — it belongs to the
    // class registered before it.
    let wasm = common::assemble(
        "embind-constructor",
        r#"(module
            (import "env" "_embind_register_class_constructor"
                (func $register (param i32 i32 i32 i32 i32 i32)))
            (memory 1)
            (func (export "register_it")
                i32.const 0 i32.const 0 i32.const 0 i32.const 0 i32.const 0 i32.const 0
                call $register))"#,
    );
    let module = Module::parse(&wasm).expect("valid");
    let registrations = analysis::analyse(&module).registrations;
    assert_eq!(registrations.len(), 1);
    assert_eq!(registrations[0].kind, "_embind_register_class_constructor");
    assert_eq!(registrations[0].name, None);
}

#[test]
fn a_module_that_registers_nothing_says_nothing() {
    let wasm = common::assemble(
        "embind-none",
        "(module (func (export \"f\") (result i32) i32.const 1))",
    );
    let module = Module::parse(&wasm).expect("valid");
    assert!(analysis::analyse(&module).registrations.is_empty());
    let code = codegen::generate(&module).expect("generates");
    assert!(!code.contains("embind"));
}

/// The module this was for.
#[test]
#[ignore = "reads the capture directory"]
fn the_voip_modules_own_api_comes_back() {
    let Some(bytes) = common::captured("D5pLH9sfOOl") else {
        panic!("D5pLH9sfOOl is not available; set WA_WASM_DIR");
    };
    let module = Module::parse(&bytes).expect("parses");
    let registrations = analysis::analyse(&module).registrations;
    let names: Vec<&str> = registrations
        .iter()
        .filter_map(|registration| registration.name.as_deref())
        .collect();

    // The entry points wa-wasm-oracle recovers by running the module. These
    // come out of the bytes.
    for expected in [
        "initVoipStack",
        "handleIncomingSignalingOffer",
        "startVoipCall",
        "endCall",
        "Uint8List",
    ] {
        assert!(names.contains(&expected), "{expected} was not recovered");
    }
    eprintln!(
        "D5pLH9sfOOl: {} embind registrations, {} named",
        registrations.len(),
        names.len()
    );
}
