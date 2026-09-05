# Rust tasks

Run `cargo xt --help` from this workspace. The alias builds the host-only task
crate in release mode; it is outside default-members and adds no dependency to
the decompiler or codec runtime.

| Task | Purpose |
|---|---|
| `cargo xt fetch-wasm [destination]` | Verify/fetch the oracle lock, CDN first and release archives second |
| `cargo xt fetch-captures [destination]` | Verify/fetch the decompiler capture manifest |
| `cargo xt mlow specs --check` | Detect drift between trace recipes and generated specs |
| `cargo xt mlow verify` | Execute, hash-check and assemble both captures |
| `cargo xt mlow verify --out DIR --from-derived` | Validate every cached output before assembling |
| `cargo xt mlow spec KIND --out FILE` | Generate an individual trace spec |
| `cargo xt mlow assemble KIND --run DIR --out FILE` | Assemble an individual trace |
| `cargo xt export-globals SOURCE DESTINATION COUNT` | Add global exports without rewriting other bytes |
| `cargo xt force-offer-guard SOURCE DESTINATION` | Diagnostic offer-guard patch |
| `cargo xt tag-offer-error-sites SOURCE_DIR DESTINATION_DIR` | Diagnostic pinned error-site tags |
| `cargo xt neutralize-thread-profiler SOURCE_DIR DESTINATION_DIR` | Diagnostic pinned profiler guard |
| `cargo xt sha256 FILE` / `cargo xt sha256 --hex HEX` | Reproducible content hashes |

`KIND` is `fe`, `signal`, `kernel`, `postfilter`, `params` or `gennoise`.
Kernel/postfilter assembly accepts `--secondary` for the second output file.
`--refresh-spec-hashes` is only for serialization migrations: it preserves all
previous module/output identities. `--update-lock` deliberately records changed
oracle outputs and is never used by CI.

`xtask-support` holds hashes, checked binary decoding, atomic writes, process
execution, descriptors, capture downloads and canonical CBOR/zstd. Its optional
features keep download/compression dependencies out of callers that only need
metadata or descriptors. GitHub credentials never go to a CDN or redirect.
