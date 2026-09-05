# NOTICE

## What this project is

`unwasm` is a WebAssembly decompiler. The decompiler, its runtime and its tests
are original work, licensed under `LICENSE-MIT` and `LICENSE-APACHE`.

## What it is tested against, and what that means

Two of the test tiers — `tests/captured.rs` and the `#[ignore]`d tests in
`host.rs`, `names.rs`, `embind.rs` and `trampolines.rs` — run against wasm
modules served by WhatsApp Web. They are there because a decompiler is judged by
shipped modules rather than by fixtures: they found bugs no hand-written fixture
reached, and each of them is recorded in the source next to the test that
caught it.

Those modules are **not** part of this repository and are **not** covered by its
licence. They are somebody else's build output. `cargo xt fetch-captures`
downloads them at test time from the public
[`oxidezap/whatspec`](https://github.com/oxidezap/whatspec) archive, into
`fixtures/wasm`, which is git-ignored.

This project is independent and is not affiliated with, endorsed by, or
sponsored by WhatsApp or Meta. WhatsApp is a trademark of its respective owner.

## Dependencies

One: [`wasmparser`](https://crates.io/crates/wasmparser), from the Bytecode
Alliance's `wasm-tools`, dual-licensed Apache-2.0-WITH-LLVM-exception / MIT.

The test harness shells out to `node`, `clang`, `wasm-tools` and `rustc`, and
the Emscripten tier to `emcc`. None of them are linked into anything this
project produces.
