//! The stdlib declaration stubs (`crates/scb-types/stdlib/*.scb`), embedded in the
//! binary, plus the generated stubs of the numeric types.
//!
//! The stubs declare the core library's API in SCB syntax; the checker type-checks
//! calls against them. Bodiless functions are implemented natively by the runtime.

use std::fmt::Write as _;

use crate::ty::IntTy;

/// `namespace Prelude;` in a stub means the root namespace.
pub const PRELUDE_NAMESPACE: &str = "Prelude";

/// Every top-level stdlib namespace, including those not implemented in the POC: user
/// code may not declare them (SPEC "Modules and packages", `reserved-namespace`).
pub const RESERVED_ROOTS: &[&str] = &[
    "Io", "Env", "Math", "Str", "Process", "Test", "Task", "Time", "Thread", "Sync", "Net", "Http",
    "Json", "Html", "Float", "Crypto", "Prelude",
];

/// Stub file name and source.
pub fn stubs() -> Vec<(&'static str, String)> {
    vec![
        (
            "prelude.scb",
            include_str!("../stdlib/prelude.scb").to_string(),
        ),
        ("numeric.scb", numeric_stub()),
        ("io.scb", include_str!("../stdlib/io.scb").to_string()),
        ("env.scb", include_str!("../stdlib/env.scb").to_string()),
        ("math.scb", include_str!("../stdlib/math.scb").to_string()),
        (
            "process.scb",
            include_str!("../stdlib/process.scb").to_string(),
        ),
    ]
}

/// Static methods and constants of the numeric types (SPEC "Casts and conversions":
/// `i8::wrap($x)`, `i32::checkedAdd($a, $b)`, `f64::NAN`, `int::parse($s)`, …).
fn numeric_stub() -> String {
    let mut s = String::from("namespace Prelude;\n\n");
    for i in IntTy::ALL {
        let t = i.name();
        let _ = write!(
            s,
            "class {t}\n{{\n\
             public const MIN: {t} = {min};\n\
             public const MAX: {t} = {max};\n\
             public static function wrap<N of __Integer>($x: N): {t};\n\
             public static function saturate<N of __Integer>($x: N): {t};\n\
             public static function tryFrom<N of __Integer>($x: N): ?{t};\n\
             public static function parse($s: string): {t};\n\
             public static function tryParse($s: string): ?{t};\n\
             public static function round($f: f64, $mode: \\Math\\Rounding = \\Math\\Rounding::HALF_EVEN): {t};\n",
            min = i.min(),
            max = i.max(),
        );
        for op in ["Add", "Sub", "Mul"] {
            let _ = write!(
                s,
                "public static function wrapping{op}($a: {t}, $b: {t}): {t};\n\
                 public static function saturating{op}($a: {t}, $b: {t}): {t};\n\
                 public static function checked{op}($a: {t}, $b: {t}): ?{t};\n"
            );
        }
        let _ = write!(
            s,
            "public static function wrappingPow($a: {t}, $n: u32): {t};\n\
             public static function checkedPow($a: {t}, $n: u32): ?{t};\n\
             public static function wrappingNeg($a: {t}): {t};\n\
             public static function wrappingShl($a: {t}, $n: u32): {t};\n\
             public static function wrappingShr($a: {t}, $n: u32): {t};\n}}\n\n"
        );
    }
    for t in ["f32", "f64"] {
        let max = if t == "f32" {
            "3.4028234663852886e38"
        } else {
            "1.7976931348623157e308"
        };
        let _ = write!(
            s,
            "class {t}\n{{\n\
             public const NAN: {t} = 0.0 / 0.0;\n\
             public const INFINITY: {t} = 1.0 / 0.0;\n\
             public const MAX: {t} = {max};\n\
             public const MIN: {t} = -{max};\n\
             public static function exactFrom<N of __Integer>($i: N): ?{t};\n\
             public static function parse($s: string): {t};\n\
             public static function tryParse($s: string): ?{t};\n\
             public static function isNan($f: {t}): bool;\n}}\n\n"
        );
    }
    s
}
