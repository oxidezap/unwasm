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
    let id = common::captures::VOIP;
    let Some(bytes) = common::captured(id) else {
        panic!("{}", common::missing_capture(id));
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
        "{id}: {} embind registrations, {} named",
        registrations.len(),
        names.len()
    );
}

/// The types, not just the names.
///
/// embind passes them as a pointer to an array of type ids, and each id is the
/// address of a `std::type_info` that some other registration names. Both
/// halves are static.
#[test]
fn a_registered_function_comes_back_with_its_c_plus_plus_signature() {
    let wasm = common::assemble(
        "embind-types",
        r#"(module
            (import "env" "_embind_register_void" (func $reg_void (param i32 i32)))
            (import "env" "_embind_register_bool"
                (func $reg_bool (param i32 i32 i32 i32 i32)))
            (import "env" "_embind_register_std_string" (func $reg_string (param i32 i32)))
            (import "env" "_embind_register_function"
                (func $reg_fn (param i32 i32 i32 i32 i32 i32 i32)))
            (memory (export "memory") 1)
            ;; type ids, and the names they are registered under
            (data (i32.const 100) "void\00")
            (data (i32.const 110) "bool\00")
            (data (i32.const 120) "std::string\00")
            (data (i32.const 140) "startVoipCall\00")
            ;; the argument type array: [void, std::string, bool]
            (data (i32.const 200) "\01\00\00\00\03\00\00\00\02\00\00\00")
            (func (export "__wasm_call_ctors")
                ;; register the types: id 1 is void, 2 is bool, 3 is std::string
                i32.const 1 i32.const 100 call $reg_void
                i32.const 2 i32.const 110 i32.const 1 i32.const 1 i32.const 0 call $reg_bool
                i32.const 3 i32.const 120 call $reg_string
                ;; register the function taking (std::string, bool) and returning void
                i32.const 140 i32.const 3 i32.const 200
                i32.const 0 i32.const 0 i32.const 0 i32.const 0
                call $reg_fn))"#,
    );
    let module = Module::parse(&wasm).expect("valid");
    let registrations = analysis::analyse(&module).registrations;
    let function = registrations
        .iter()
        .find(|registration| registration.name.as_deref() == Some("startVoipCall"))
        .expect("registered");
    assert_eq!(
        function.signature.as_deref(),
        Some("void startVoipCall(std::string, bool)")
    );
}

#[test]
fn a_type_registered_after_it_is_used_is_still_resolved() {
    // The order is whatever the module's initialisers happen to run in, and
    // nothing says the types come first.
    let wasm = common::assemble(
        "embind-order",
        r#"(module
            (import "env" "_embind_register_integer"
                (func $reg_int (param i32 i32 i32 i32 i32)))
            (import "env" "_embind_register_function"
                (func $reg_fn (param i32 i32 i32 i32 i32 i32 i32)))
            (memory (export "memory") 1)
            (data (i32.const 100) "int\00")
            (data (i32.const 140) "twice\00")
            (data (i32.const 200) "\01\00\00\00\01\00\00\00")
            (func (export "__wasm_call_ctors")
                i32.const 140 i32.const 2 i32.const 200
                i32.const 0 i32.const 0 i32.const 0 i32.const 0
                call $reg_fn
                ;; the type is registered afterwards — and `int` is three
                ;; characters, which a general text read would refuse
                i32.const 1 i32.const 100 i32.const 4 i32.const 0 i32.const 0
                call $reg_int))"#,
    );
    let module = Module::parse(&wasm).expect("valid");
    let registrations = analysis::analyse(&module).registrations;
    let function = registrations
        .iter()
        .find(|registration| registration.name.as_deref() == Some("twice"))
        .expect("registered");
    assert_eq!(function.signature.as_deref(), Some("int twice(int)"));
}

/// A class, its constructor and a method — the shape embind's generated code
/// has, in the order it has it.
const CLASS_WITH_METHOD: &str = r#"(module
            (import "env" "_embind_register_integer"
                (func $reg_int (param i32 i32 i32 i32 i32)))
            (import "env" "_embind_register_class"
                (func $reg_class
                    (param i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32)))
            (import "env" "_embind_register_class_function"
                (func $reg_method (param i32 i32 i32 i32 i32 i32 i32 i32 i32)))
            (import "env" "_embind_register_class_constructor"
                (func $reg_ctor (param i32 i32 i32 i32 i32 i32)))
            (memory (export "memory") 1)
            (data (i32.const 100) "int\00")
            (data (i32.const 110) "Uint8List\00")
            (data (i32.const 130) "push_back\00")
            (data (i32.const 200) "\01\00\00\00\01\00\00\00")
            (data (i32.const 220) "\0a\00\00\00")
            (func (export "__wasm_call_ctors")
                i32.const 1 i32.const 100 i32.const 4 i32.const 0 i32.const 0
                call $reg_int
                ;; class id 9, pointer id 10, const pointer id 11
                i32.const 9 i32.const 10 i32.const 11 i32.const 0 i32.const 0
                i32.const 0 i32.const 0 i32.const 0 i32.const 0 i32.const 0
                i32.const 110
                i32.const 0 i32.const 0
                call $reg_class
                ;; its constructor, which returns a pointer to the class
                i32.const 9 i32.const 1 i32.const 220 i32.const 0 i32.const 0 i32.const 0
                call $reg_ctor
                ;; and a method
                i32.const 9 i32.const 130 i32.const 2 i32.const 200
                i32.const 0 i32.const 0 i32.const 0 i32.const 0 i32.const 0
                call $reg_method))"#;

#[test]
fn a_method_belongs_to_the_class_registered_before_it() {
    let wasm = common::assemble("embind-method", CLASS_WITH_METHOD);
    let module = Module::parse(&wasm).expect("valid");
    let registrations = analysis::analyse(&module).registrations;

    let method = registrations
        .iter()
        .find(|registration| registration.name.as_deref() == Some("push_back"))
        .expect("registered");
    assert_eq!(method.class.as_deref(), Some("Uint8List"));
    assert_eq!(method.signature.as_deref(), Some("int push_back(int)"));

    // A constructor is named after its class rather than returning an address.
    let constructor = registrations
        .iter()
        .find(|registration| registration.kind.ends_with("class_constructor"))
        .expect("registered");
    assert_eq!(constructor.signature.as_deref(), Some("Uint8List()"));
    assert_eq!(constructor.class.as_deref(), Some("Uint8List"));
}

#[test]
fn a_type_nothing_names_is_shown_as_its_address_rather_than_guessed_at() {
    let wasm = common::assemble(
        "embind-unknown-type",
        r#"(module
            (import "env" "_embind_register_function"
                (func $reg_fn (param i32 i32 i32 i32 i32 i32 i32)))
            (memory (export "memory") 1)
            (data (i32.const 140) "mystery\00")
            (data (i32.const 200) "\07\00\00\00")
            (func (export "__wasm_call_ctors")
                i32.const 140 i32.const 1 i32.const 200
                i32.const 0 i32.const 0 i32.const 0 i32.const 0
                call $reg_fn))"#,
    );
    let module = Module::parse(&wasm).expect("valid");
    let registrations = analysis::analyse(&module).registrations;
    assert_eq!(
        registrations[0].signature.as_deref(),
        Some("type@7 mystery()"),
        "an id nothing registered is an address, and says so"
    );
}

#[test]
fn the_signatures_reach_the_top_of_the_generated_module() {
    // The same module as the signature test above: a registration with types
    // to resolve, rather than one with the type arguments zeroed.
    let wasm = common::assemble(
        "embind-signature-output",
        r#"(module
            (import "env" "_embind_register_std_string" (func $reg_string (param i32 i32)))
            (import "env" "_embind_register_function"
                (func $reg_fn (param i32 i32 i32 i32 i32 i32 i32)))
            (memory (export "memory") 1)
            (data (i32.const 120) "std::string\00")
            (data (i32.const 140) "startVoipCall\00")
            (data (i32.const 200) "\03\00\00\00\03\00\00\00")
            (func (export "__wasm_call_ctors")
                i32.const 3 i32.const 120 call $reg_string
                i32.const 140 i32.const 2 i32.const 200
                i32.const 0 i32.const 0 i32.const 0 i32.const 0
                call $reg_fn))"#,
    );
    let code = common::decompile(&wasm);
    assert!(
        code.contains("The API this module registers with embind"),
        "{code}"
    );
    assert!(
        code.contains("//   function: std::string startVoipCall(std::string)"),
        "the types reach the reader, not just the name:\n{code}"
    );
}

#[test]
fn a_method_reaches_the_output_with_its_class() {
    let wasm = common::assemble("embind-method-output", CLASS_WITH_METHOD);
    let code = common::decompile(&wasm);
    assert!(
        code.contains("//   class function: Uint8List::int push_back(int)"),
        "a method is printed under the class it belongs to:\n{code}"
    );
    assert!(
        code.contains("//   class constructor: Uint8List::Uint8List()"),
        "{code}"
    );
}

#[test]
fn a_type_array_outside_static_memory_reads_as_unknown() {
    // The pointer is a number the module never wrote anything at.
    let wasm = common::assemble(
        "embind-far-types",
        r#"(module
            (import "env" "_embind_register_function"
                (func $reg_fn (param i32 i32 i32 i32 i32 i32 i32)))
            (memory (export "memory") 1)
            (data (i32.const 140) "far\00")
            (func (export "__wasm_call_ctors")
                i32.const 140 i32.const 1 i32.const 999999
                i32.const 0 i32.const 0 i32.const 0 i32.const 0
                call $reg_fn))"#,
    );
    let module = Module::parse(&wasm).expect("valid");
    let registrations = analysis::analyse(&module).registrations;
    assert_eq!(
        registrations[0].signature.as_deref(),
        Some("? far()"),
        "a type that could not be read is unknown, not invented"
    );
}

#[test]
fn a_type_definition_with_nothing_readable_defines_nothing() {
    // The name argument points nowhere, so there is no name to register the
    // type under — and the registration is still reported.
    let wasm = common::assemble(
        "embind-nameless-type",
        r#"(module
            (import "env" "_embind_register_integer"
                (func $reg_int (param i32 i32 i32 i32 i32)))
            (import "env" "_embind_register_function"
                (func $reg_fn (param i32 i32 i32 i32 i32 i32 i32)))
            (memory (export "memory") 1)
            (data (i32.const 140) "uses_it\00")
            (data (i32.const 200) "\01\00\00\00")
            (func (export "__wasm_call_ctors")
                i32.const 1 i32.const 888888 i32.const 4 i32.const 0 i32.const 0
                call $reg_int
                i32.const 140 i32.const 1 i32.const 200
                i32.const 0 i32.const 0 i32.const 0 i32.const 0
                call $reg_fn))"#,
    );
    let module = Module::parse(&wasm).expect("valid");
    let registrations = analysis::analyse(&module).registrations;
    assert_eq!(registrations[0].name, None, "the type registered no name");
    assert_eq!(
        registrations[1].signature.as_deref(),
        Some("type@1 uses_it()"),
        "so the id stays an id"
    );
}

#[test]
fn static_memory_is_read_out_of_a_placed_segment_too() {
    // A threaded module's segments are passive; the type arrays live in them
    // like everything else.
    let wasm = common::assemble(
        "embind-placed-types",
        r#"(module
            (import "env" "_embind_register_std_string" (func $reg_string (param i32 i32)))
            (import "env" "_embind_register_function"
                (func $reg_fn (param i32 i32 i32 i32 i32 i32 i32)))
            (memory (export "memory") 1)
            (data $names "std::string\00placed_call\00")
            (data $types "\03\00\00\00\03\00\00\00")
            (func (export "__wasm_call_ctors")
                i32.const 4096 i32.const 0 i32.const 24 memory.init $names
                i32.const 8192 i32.const 0 i32.const 8 memory.init $types
                i32.const 3 i32.const 4096 call $reg_string
                i32.const 4108 i32.const 2 i32.const 8192
                i32.const 0 i32.const 0 i32.const 0 i32.const 0
                call $reg_fn))"#,
    );
    let module = Module::parse(&wasm).expect("valid");
    let registrations = analysis::analyse(&module).registrations;
    let call = registrations
        .iter()
        .find(|registration| registration.name.as_deref() == Some("placed_call"))
        .expect("registered");
    assert_eq!(
        call.signature.as_deref(),
        Some("std::string placed_call(std::string)")
    );
}

/// Registered, then called — from outside, through what the registration said.
///
/// This is the whole point of reading the registrations: a module built with
/// embind exports almost nothing directly, and what it does export is the
/// bindings' own machinery. The API it means to publish is a list of table
/// slots, and this is that list being used.
#[test]
fn something_the_module_registers_can_then_be_called() {
    let wasm = common::assemble(
        "embind-call",
        r#"(module
            (import "env" "_embind_register_function"
                (func $register (param i32 i32 i32 i32 i32 i32 i32 i32)))
            (memory (export "memory") 1)
            (data (i32.const 64) "doubler\00")
            (type $invoker (func (param i32 i32) (result i32)))
            (type $target (func (param i32) (result i32)))
            ;; What embind generates: an invoker that takes the function
            ;; pointer and the arguments, and calls through the table.
            (func $invoke (type $invoker)
                local.get 1 local.get 0 call_indirect (type $target))
            (func $double (type $target) local.get 0 i32.const 2 i32.mul)
            (func (export "register")
                i32.const 64   ;; name
                i32.const 2    ;; argCount, counting the return type
                i32.const 0    ;; argTypes
                i32.const 0    ;; signature
                i32.const 1    ;; invoker: table slot 1
                i32.const 2    ;; function: table slot 2
                i32.const 0 i32.const 0
                call $register)
            (table 3 funcref)
            ;; The import is function 0, so the two defined ones are 1 and 2.
            (elem (i32.const 1) func 1 2))"#,
    );
    let module = Module::parse(&wasm).expect("valid");
    let generated = codegen::generate(&module).expect("generates");
    let host = codegen::generate_host(&module).expect("generates a host");

    const DRIVER: &str = r#"
mod generated;
mod host;
fn main() {
    let mut instance = generated::Instance::with_host(host::Host::default());
    instance.register();

    // Everything from here on is the module describing itself: the name it
    // published, the slot to call, and what to pass first.
    let call = {
        let embind = &instance.host.embind;
        let registered = embind.function("doubler").expect("the module registered it");
        registered.call().expect("a function is callable")
    };
    println!("invoker {} context {} arity {}", call.invoker, call.context, call.arity);
    let answer = instance.invoke_slot(call.invoker, &[i64::from(call.context), 21]);
    println!("{answer:?}");
}
"#;
    let output = common::run_with_host("embind-call", &generated, &host, DRIVER);
    let lines: Vec<&str> = output.trim().lines().collect();
    assert_eq!(lines[0], "invoker 1 context 2 arity 2");
    assert_eq!(
        lines[1], "Ok(Some(42))",
        "twice 21, through a function this side only ever knew as a number"
    );
}
