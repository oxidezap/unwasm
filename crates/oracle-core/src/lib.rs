//! Loads WhatsApp Web's shipped WebAssembly modules so they can be inspected
//! and, eventually, executed as a behavioural oracle.
//!
//! The point of the crate is to replace *reading* a decompilation with
//! *running* the original artifact: a captured module answers questions about
//! wire behaviour that static analysis can only guess at.

pub mod abi;
pub mod call;
pub mod carry;
pub mod catalog;
pub mod cxa;
pub mod data;
pub mod embind;
pub mod emscripten;
pub mod emval;
pub mod exports;
pub mod host;
pub mod inspect;
pub mod patch;
pub mod runtime;
pub mod schedule;
pub mod shared;
pub mod state;
pub mod threads;
pub mod wasi;

pub use call::{Handle, Value};
pub use embind::{EmbindClass, EmbindFunction, EmbindRegistry};
pub use runtime::Runtime;
pub use state::{HostCall, HostState, ThreadPolicy};

pub use catalog::{CapturedModule, Catalog};
pub use data::{DataReport, DataSegment, DataString};
pub use inspect::{ExportEntry, ImportEntry, Inspection, Requirement, SectionEntry, Toolchain};
