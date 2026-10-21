//! SCB runtime: the [`Isolate`], the value representation and the interpreter.
//!
//! The core library's native functions live in `scb-stdlib`; they are written
//! against the [`Interp`] API here and handed to an isolate by the embedder
//! ([`Isolate::with_natives`]), so this crate does not depend on the stdlib.

pub mod interp;
mod isolate;
pub mod value;

pub use interp::{Flow, Interp, NativeCall, NativeFn, Natives, R};
pub use isolate::{Env, Isolate, IsolateIo, MapEnv, RunOutcome};
