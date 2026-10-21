//! SCB core library (SPEC "Core library (POC minimum)").
//!
//! Implemented in M7. Every stdlib function reaches the outside world only through
//! the [`scb_runtime::Isolate`] it runs in, never through `std::io` or `std::env`.
