//! unwasm: a WebAssembly decompiler whose output compiles.

pub mod codegen;
pub mod error;
pub mod module;
pub mod ops;
pub mod rt;

pub use error::{Error, Result};
pub use module::{Module, ValType};
