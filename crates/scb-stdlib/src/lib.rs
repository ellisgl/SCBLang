//! SCB core library (SPEC "Core library (POC minimum)").
//!
//! Native implementations of the functions and methods declared in the stdlib stubs
//! (`crates/scb-types/stdlib/*.scb`), registered under the names the checker gives
//! them: `Type.method` for instance methods (`list.push`), `Type::method` for static
//! ones (`i8::wrap`), `Ns\function` for functions (`Io\print`).
//!
//! Every function reaches the outside world only through the isolate it runs in
//! (`it.iso`), never through `std::io` or `std::env` (CLAUDE.md architecture rule 2).

mod bytes;
mod list;
mod map;
mod numeric;
mod range;
mod string;
mod system;
mod throwable;
mod util;

use scb_runtime::Natives;

/// Every native function, by name.
pub fn natives() -> Natives {
    let mut n = Natives::new();
    list::register(&mut n);
    map::register(&mut n);
    string::register(&mut n);
    bytes::register(&mut n);
    range::register(&mut n);
    numeric::register(&mut n);
    system::register(&mut n);
    throwable::register(&mut n);
    n
}
