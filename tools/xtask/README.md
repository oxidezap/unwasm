# Rust tasks

Run `cargo xt --help` from this workspace. The alias builds the host-only task
crate in release mode; it is outside default-members and adds no dependency to
the decompiler or codec runtime.

| Task | Purpose |
|---|---|
| `cargo xt fetch-captures [destination]` | Verify/fetch the decompiler capture manifest |
| `cargo xt sha256 FILE` / `cargo xt sha256 --hex HEX` | Reproducible content hashes |

Capture restoration is implemented by whatspec's commit-pinned `wa-store`.
The task contains no WhatsApp host, VoIP recipe, HTTP client or archive parser.
Its Rust 1.89 floor follows the current pure-Rust TLS provider; the decompiler
crates retain their independent Rust 1.88 floor.
