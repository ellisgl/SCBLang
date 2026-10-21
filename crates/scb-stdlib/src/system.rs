//! `Io`, `Env`, `Process` and `Math` (SPEC "Core library", "Source files and program
//! entry"). All I/O goes through the isolate's handles.

use scb_runtime::value::Value;
use scb_runtime::{Flow, Interp, NativeCall, Natives, R};

pub fn register(n: &mut Natives) {
    let fns: &[(&str, scb_runtime::NativeFn)] = &[
        ("Io\\print", print),
        ("Io\\eprint", eprint),
        ("Io\\readLine", read_line),
        ("Io\\readAll", read_all),
        ("Env\\get", env_get),
        ("Process\\exit", exit),
        ("Process\\abort", abort),
        ("Process\\name", name),
        ("Math\\floorDiv", floor_div),
        ("Math\\mod", modulo),
    ];
    for (name, f) in fns {
        n.insert((*name).to_string(), *f);
    }
}

fn io_error(it: &mut Interp<'_>, e: std::io::Error, c: &NativeCall) -> Flow {
    it.error("IoException", e.to_string(), c.span)
}

fn print(it: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    let text = c.args[0].as_str().to_string();
    it.iso
        .write_stdout(text.as_bytes())
        .map_err(|e| io_error(it, e, c))?;
    Ok(Value::Null)
}

fn eprint(it: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    let text = c.args[0].as_str().to_string();
    it.iso
        .write_stderr(text.as_bytes())
        .map_err(|e| io_error(it, e, c))?;
    Ok(Value::Null)
}

/// The next line without `\n` or `\r\n` (a lone `\r` is kept), `null` at end of
/// input; invalid UTF-8 throws `EncodingException`.
fn read_line(it: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    let Some(mut line) = it.iso.read_line_raw().map_err(|e| io_error(it, e, c))? else {
        return Ok(Value::Null);
    };
    if line.last() == Some(&b'\n') {
        line.pop();
        if line.last() == Some(&b'\r') {
            line.pop();
        }
    }
    match String::from_utf8(line) {
        Ok(s) => Ok(Value::str(s)),
        Err(e) => Err(it.error(
            "EncodingException",
            format!(
                "stdin line is not valid UTF-8 (byte {})",
                e.utf8_error().valid_up_to()
            ),
            c.span,
        )),
    }
}

fn read_all(it: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    let data = it.iso.read_all().map_err(|e| io_error(it, e, c))?;
    Ok(Value::Bytes(std::rc::Rc::new(data)))
}

fn env_get(it: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    let name = c.args[0].as_str().to_string();
    match it.iso.env_var(&name) {
        None => Ok(Value::Null),
        Some(raw) => match String::from_utf8(raw) {
            Ok(s) => Ok(Value::str(s)),
            Err(_) => Err(it.error(
                "EncodingException",
                format!("environment variable `{name}` is not valid UTF-8"),
                c.span,
            )),
        },
    }
}

fn exit(it: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    Err(it.exit_with(c.args[0].as_i64(), c.span))
}

fn abort(_: &mut Interp<'_>, _: &mut NativeCall) -> R<Value> {
    Err(Flow::Abort)
}

fn name(it: &mut Interp<'_>, _: &mut NativeCall) -> R<Value> {
    Ok(Value::str(it.iso.program_name()))
}

fn operands(it: &mut Interp<'_>, c: &NativeCall) -> R<(scb_types::ty::IntTy, i128, i128)> {
    let t = c.args[0].int_ty().unwrap_or(scb_types::ty::IntTy::I64);
    let (a, b) = (
        c.args[0].as_i128().unwrap_or(0),
        c.args[1].as_i128().unwrap_or(0),
    );
    if b == 0 {
        return Err(it.error("DivisionByZeroError", "division by zero", c.span));
    }
    Ok((t, a, b))
}

/// Division rounding toward negative infinity.
fn floor_div(it: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    let (t, a, b) = operands(it, c)?;
    let mut q = a / b;
    if a % b != 0 && (a < 0) != (b < 0) {
        q -= 1;
    }
    if q < t.min() || q > t.max() {
        return Err(it.error(
            "OverflowError",
            format!("Math\\floorDiv({a}, {b}) overflows"),
            c.span,
        ));
    }
    Ok(Value::int(t, q))
}

/// The remainder of floor division: non-negative for a positive divisor.
fn modulo(it: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    let (t, a, b) = operands(it, c)?;
    let mut r = a % b;
    if r != 0 && (r < 0) != (b < 0) {
        r += b;
    }
    Ok(Value::int(t, r))
}
