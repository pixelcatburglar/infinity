//! CCP `blue.marshal` codec for the Infinity client (carbon-blue v2.6.5,
//! Python 2.7, `PY27_COMPATIBILITY_MODE`).
//!
//! Source of truth: carbonengine/blue `Marshal.cpp` / `Marshal.h` /
//! `PyRowSet.cpp`, cross-checked by parsing files written by the client's own
//! `blue.dll` (byte-identical between the Infinity and Serenity builds):
//!
//! * Python 2 `str` is written as `TY_BUFFER` (0x13), or the empty / single
//!   char / string-table short forms. `TY_STR*` long forms are only read.
//! * `unicode` is written as `TY_UTF8` (0x2E). `TY_UNICODE` (0x12) is read as
//!   UTF-8, matching carbonengine.
//! * Integers outside the 32-bit C `long` range are Python longs: `TY_LONG`.
//! * Stream header is version 0: `0x7E`, i32 shared count, objects, then the
//!   shared-object index map at the very end of the buffer.
//!
//! Wire widths, sign masks, tuple arities and size sentinels follow
//! `Marshal.cpp` / `Marshal.h`; they are codec-specific, not general constants.
//! The long representation here is limited to little-endian two's complement;
//! a different wire format requires changing both encoder and decoder.
//! `Debug` output decodes embedded streams at most `DEBUG_STREAM_DEPTH`
//! levels along one path and prints `Stream(N bytes)` beyond that.

mod dbrow;
mod decode;
mod encode;
pub mod strings;

pub use dbrow::{DbRowDescriptor, DbType};
pub use decode::loads;
pub use encode::dumps;

use std::fmt;

/// Server codec design: embedded streams are decoded for display only this
/// many levels deep along one path, which keeps repeated decoding of nested
/// streams linear in the input size.
const DEBUG_STREAM_DEPTH: usize = 4;

/// `Marshal::WriteLong` / `_PyLong_FromByteArray`: redundant sign extension.
fn redundant_sign_byte(last: u8, prev: u8) -> bool {
    (last == 0 && prev & 0x80 == 0) || (last == 0xFF && prev & 0x80 != 0)
}

/// Type tags (`PYTYPES` in Marshal.h).
pub mod tag {
    /// `Marshal.h`: wire signature `SIGNATURE`.
    pub const SIGNATURE: u8 = 0x7E;
    /// `Marshal.h`: wire signature `SIGNATURE2`.
    pub const SIGNATURE2: u8 = 0x7D;
    /// `Marshal.h`: wire tag or flag `NONE`.
    pub const NONE: u8 = 1;
    /// `Marshal.h`: wire tag or flag `GLOBAL`.
    pub const GLOBAL: u8 = 2;
    /// `Marshal.h`: wire tag or flag `INT64`.
    pub const INT64: u8 = 3;
    /// `Marshal.h`: wire tag or flag `INT32`.
    pub const INT32: u8 = 4;
    /// `Marshal.h`: wire tag or flag `INT16`.
    pub const INT16: u8 = 5;
    /// `Marshal.h`: wire tag or flag `INT8`.
    pub const INT8: u8 = 6;
    /// `Marshal.h`: wire tag or flag `INT_N1`.
    pub const INT_N1: u8 = 7;
    /// `Marshal.h`: wire tag or flag `INT_0`.
    pub const INT_0: u8 = 8;
    /// `Marshal.h`: wire tag or flag `INT_1`.
    pub const INT_1: u8 = 9;
    /// `Marshal.h`: wire tag or flag `FLOAT`.
    pub const FLOAT: u8 = 10;
    /// `Marshal.h`: wire tag or flag `FLOAT_0`.
    pub const FLOAT_0: u8 = 11;
    /// `Marshal.h`: wire tag or flag `COMPLEX`.
    pub const COMPLEX: u8 = 12;
    /// `Marshal.h`: wire tag or flag `STR`.
    pub const STR: u8 = 13;
    /// `Marshal.h`: wire tag or flag `STR_EMPTY`.
    pub const STR_EMPTY: u8 = 14;
    /// `Marshal.h`: wire tag or flag `STR_CHAR`.
    pub const STR_CHAR: u8 = 15;
    /// `Marshal.h`: wire tag or flag `STR_SHORT`.
    pub const STR_SHORT: u8 = 16;
    /// `Marshal.h`: wire tag or flag `STR_TABLE`.
    pub const STR_TABLE: u8 = 17;
    /// `Marshal.h`: wire tag or flag `UNICODE`.
    pub const UNICODE: u8 = 18;
    /// `Marshal.h`: wire tag or flag `BUFFER`.
    pub const BUFFER: u8 = 19;
    /// `Marshal.h`: wire tag or flag `TUPLE`.
    pub const TUPLE: u8 = 20;
    /// `Marshal.h`: wire tag or flag `LIST`.
    pub const LIST: u8 = 21;
    /// `Marshal.h`: wire tag or flag `DICT`.
    pub const DICT: u8 = 22;
    /// `Marshal.h`: wire tag or flag `INSTANCE`.
    pub const INSTANCE: u8 = 23;
    /// `Marshal.h`: wire tag or flag `CALLBACK`.
    pub const CALLBACK: u8 = 25;
    /// `Marshal.h`: wire tag or flag `PICKLE`.
    pub const PICKLE: u8 = 26;
    /// `Marshal.h`: wire tag or flag `REFERENCE`.
    pub const REFERENCE: u8 = 27;
    /// `Marshal.h`: wire tag or flag `CRC_CHECK`.
    pub const CRC_CHECK: u8 = 28;
    /// `Marshal.h`: wire tag or flag `TRUE`.
    pub const TRUE: u8 = 31;
    /// `Marshal.h`: wire tag or flag `FALSE`.
    pub const FALSE: u8 = 32;
    /// `Marshal.h`: wire tag or flag `PICKLER`.
    pub const PICKLER: u8 = 33;
    /// `Marshal.h`: wire tag or flag `REDUCE`.
    pub const REDUCE: u8 = 34;
    /// `Marshal.h`: wire tag or flag `NEWOBJ`.
    pub const NEWOBJ: u8 = 35;
    /// `Marshal.h`: wire tag or flag `TUPLE0`.
    pub const TUPLE0: u8 = 36;
    /// `Marshal.h`: wire tag or flag `TUPLE1`.
    pub const TUPLE1: u8 = 37;
    /// `Marshal.h`: wire tag or flag `LIST0`.
    pub const LIST0: u8 = 38;
    /// `Marshal.h`: wire tag or flag `LIST1`.
    pub const LIST1: u8 = 39;
    /// `Marshal.h`: wire tag or flag `UNICODE_0`.
    pub const UNICODE_0: u8 = 40;
    /// `Marshal.h`: wire tag or flag `UNICODE_1`.
    pub const UNICODE_1: u8 = 41;
    /// `Marshal.h`: wire tag or flag `DBROW`.
    pub const DBROW: u8 = 42;
    /// `Marshal.h`: wire tag or flag `WSTREAM`.
    pub const WSTREAM: u8 = 43;
    /// `Marshal.h`: wire tag or flag `TUPLE2`.
    pub const TUPLE2: u8 = 44;
    /// `Marshal.h`: wire tag or flag `MARK`.
    pub const MARK: u8 = 45;
    /// `Marshal.h`: wire tag or flag `UTF8`.
    pub const UTF8: u8 = 46;
    /// `Marshal.h`: wire tag or flag `LONG`.
    pub const LONG: u8 = 47;
    /// `Marshal.h`: wire tag or flag `SHARED_FLAG`.
    pub const SHARED_FLAG: u8 = 0x40;
    /// `Marshal.h`: wire tag or flag `TYPE_MASK`.
    pub const TYPE_MASK: u8 = 0x3F;
}

/// A decoded Python object.
#[derive(Clone, PartialEq)]
pub enum Value {
    None,
    Bool(bool),
    Int(i64),
    /// Python long that does not fit in i64: little-endian two's complement.
    BigLong(Vec<u8>),
    Float(f64),
    /// Python 2 `str` (byte string).
    Bytes(Vec<u8>),
    Unicode(String),
    Tuple(Vec<Value>),
    List(Vec<Value>),
    /// Insertion-ordered key/value pairs.
    Dict(Vec<(Value, Value)>),
    /// Class or function referenced by `module.name`.
    Global(String),
    /// Old-style class instance: `(class name, state)`.
    Instance(Box<Value>, Box<Value>),
    /// `__reduce__` protocol: `data` is `(callable, args[, state])`.
    Reduce { data: Box<Value>, list: Vec<Value>, dict: Vec<(Value, Value)> },
    /// `copy_reg.__newobj__` form: `data` is `((cls, *args)[, state])`.
    NewObj { data: Box<Value>, list: Vec<Value>, dict: Vec<(Value, Value)> },
    /// `blue.DBRow`: descriptor object, unpacked fixed-size data, trailing objects.
    DbRow { descriptor: Box<Value>, data: Vec<u8>, objects: Vec<Value> },
    /// `blue.MarshalStream`: an embedded, already-marshalled stream.
    SubStream(Vec<u8>),
    /// `TY_CALLBACK`: the marshal callback's substitute for an object, e.g. a
    /// machoNet bound-object reference (`ObjectCallGPCS.GetObjectID`).
    Callback(Box<Value>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    Truncated { at: usize, wanted: usize },
    BadSignature(u8),
    BadTag { tag: u8, at: usize },
    BadStringIndex(u8),
    BadReference(i32),
    BadSharedMap(String),
    BadUtf8 { at: usize },
    BadRle,
    TooDeep,
    TooLarge,
    Unsupported(&'static str),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Truncated { at, wanted } => write!(f, "truncated stream: wanted {wanted} bytes at {at}"),
            Error::BadSignature(b) => write!(f, "bad marshal signature 0x{b:02x}"),
            Error::BadTag { tag, at } => write!(f, "invalid type tag {tag} at offset {at}"),
            Error::BadStringIndex(i) => write!(f, "invalid string table index {i}"),
            Error::BadReference(i) => write!(f, "invalid TY_REFERENCE {i}"),
            Error::BadSharedMap(m) => write!(f, "bad shared object map: {m}"),
            Error::BadUtf8 { at } => write!(f, "invalid UTF-8 at offset {at}"),
            Error::BadRle => write!(f, "invalid DBRow RLE data"),
            Error::TooDeep => write!(f, "recursion limit exceeded"),
            Error::TooLarge => write!(f, "decoded size limit exceeded"),
            Error::Unsupported(what) => write!(f, "unsupported marshal feature: {what}"),
        }
    }
}

impl std::error::Error for Error {}

// ---------------------------------------------------------------- builders
impl Value {
    /// Python 2 `str`.
    pub fn str(s: impl AsRef<[u8]>) -> Value {
        Value::Bytes(s.as_ref().to_vec())
    }
    pub fn unicode(s: impl Into<String>) -> Value {
        Value::Unicode(s.into())
    }
    pub fn tuple(items: impl IntoIterator<Item = Value>) -> Value {
        Value::Tuple(items.into_iter().collect())
    }
    pub fn list(items: impl IntoIterator<Item = Value>) -> Value {
        Value::List(items.into_iter().collect())
    }
    /// Dict with Python 2 `str` keys, the common case on the wire.
    pub fn dict<K: AsRef<[u8]>>(items: impl IntoIterator<Item = (K, Value)>) -> Value {
        Value::Dict(items.into_iter().map(|(k, v)| (Value::str(k), v)).collect())
    }
    pub fn global(name: impl Into<String>) -> Value {
        Value::Global(name.into())
    }
    /// Old-style class instance with a dict state.
    pub fn instance<K: AsRef<[u8]>>(class: &str, state: impl IntoIterator<Item = (K, Value)>) -> Value {
        Value::Instance(Box::new(Value::str(class)), Box::new(Value::dict(state)))
    }
    /// `callable(*args)` via the reduce protocol, optionally followed by `__setstate__(state)`.
    pub fn reduce(callable: &str, args: Vec<Value>, state: Option<Value>) -> Value {
        let mut data = vec![Value::global(callable), Value::Tuple(args)];
        data.extend(state);
        Value::Reduce { data: Box::new(Value::Tuple(data)), list: vec![], dict: vec![] }
    }
}

// ---------------------------------------------------------------- accessors
impl Value {
    pub fn as_int(&self) -> Option<i64> {
        match self {
            Value::Int(i) => Some(*i),
            Value::Bool(b) => Some(*b as i64),
            _ => None,
        }
    }
    pub fn as_float(&self) -> Option<f64> {
        match self {
            Value::Float(f) => Some(*f),
            Value::Int(i) => Some(*i as f64),
            _ => None,
        }
    }
    /// Text from either string flavour (lossy for non-UTF-8 byte strings).
    pub fn as_text(&self) -> Option<std::borrow::Cow<'_, str>> {
        match self {
            Value::Bytes(b) => Some(String::from_utf8_lossy(b)),
            Value::Unicode(s) => Some(s.as_str().into()),
            _ => None,
        }
    }
    pub fn as_bytes(&self) -> Option<&[u8]> {
        match self {
            Value::Bytes(b) | Value::SubStream(b) => Some(b),
            _ => None,
        }
    }
    /// Elements of a tuple, list, or Python set (pickled as `Reduce(set, ([items],))`).
    pub fn items(&self) -> Option<&[Value]> {
        match self {
            Value::Tuple(v) | Value::List(v) => Some(v),
            Value::Reduce { data, .. } => match data.items()? {
                [Value::Global(g), args] if g == "__builtin__.set" || g == "__builtin__.frozenset" => match args.items()? {
                    [inner] => inner.items(),
                    [] => Some(&[]),
                    _ => None,
                },
                _ => None,
            },
            _ => None,
        }
    }
    pub fn is_none(&self) -> bool {
        matches!(self, Value::None)
    }
    /// Dict lookup by string key (either flavour).
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Dict(pairs) => pairs.iter().find(|(k, _)| k.as_text().is_some_and(|t| t == key)).map(|(_, v)| v),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------- repr
/// `fmt::Debug` for `Value` with the embedded-stream depth still available:
/// containers pass the remaining depth through, each decoded stream spends one.
struct Repr<'a>(&'a Value, usize);

impl fmt::Debug for Repr<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fn seq(f: &mut fmt::Formatter<'_>, open: &str, items: &[Value], close: &str, depth: usize) -> fmt::Result {
            f.write_str(open)?;
            for (i, v) in items.iter().enumerate() {
                if i > 0 {
                    f.write_str(", ")?;
                }
                write!(f, "{:?}", Repr(v, depth))?;
            }
            if open == "(" && items.len() == 1 {
                f.write_str(",")?;
            }
            f.write_str(close)
        }
        fn pairs(f: &mut fmt::Formatter<'_>, items: &[(Value, Value)], depth: usize) -> fmt::Result {
            f.write_str("{")?;
            for (i, (k, v)) in items.iter().enumerate() {
                if i > 0 {
                    f.write_str(", ")?;
                }
                write!(f, "{:?}: {:?}", Repr(k, depth), Repr(v, depth))?;
            }
            f.write_str("}")
        }
        let depth = self.1;
        match self.0 {
            Value::None => f.write_str("None"),
            Value::Bool(true) => f.write_str("True"),
            Value::Bool(false) => f.write_str("False"),
            Value::Int(i) => write!(f, "{i}"),
            Value::BigLong(b) => write!(f, "long(0x{})", b.iter().rev().map(|x| format!("{x:02x}")).collect::<String>()),
            Value::Float(x) => write!(f, "{x:?}"),
            Value::Bytes(b) => match std::str::from_utf8(b) {
                Ok(s) if b.iter().all(|c| (0x20..0x7f).contains(c)) => write!(f, "'{s}'"),
                _ => write!(f, "'{}'", b.escape_ascii()),
            },
            Value::Unicode(s) => write!(f, "u{s:?}"),
            Value::Tuple(v) => seq(f, "(", v, ")", depth),
            Value::List(v) => seq(f, "[", v, "]", depth),
            Value::Dict(p) => pairs(f, p, depth),
            Value::Global(g) => write!(f, "<{g}>"),
            Value::Instance(c, s) => write!(f, "Instance({:?}, {:?})", Repr(c, depth), Repr(s, depth)),
            Value::Reduce { data, list, dict } | Value::NewObj { data, list, dict } => {
                let kind = if matches!(self.0, Value::Reduce { .. }) { "Reduce" } else { "NewObj" };
                write!(f, "{kind}{:?}", Repr(data, depth))?;
                if !list.is_empty() {
                    seq(f, "+", list, "", depth)?;
                }
                if !dict.is_empty() {
                    f.write_str("+")?;
                    pairs(f, dict, depth)?;
                }
                Ok(())
            }
            Value::DbRow { descriptor, data, objects } => {
                write!(f, "DBRow({:?}, {} bytes, ", Repr(descriptor, depth), data.len())?;
                seq(f, "[", objects, "])", depth)
            }
            Value::Callback(v) => write!(f, "Callback({:?})", Repr(v, depth)),
            Value::SubStream(b) => match depth.checked_sub(1).and_then(|d| loads(b).ok().map(|v| (v, d))) {
                Some((v, d)) => write!(f, "Stream({:?})", Repr(&v, d)),
                None => write!(f, "Stream({} bytes)", b.len()),
            },
        }
    }
}

impl fmt::Debug for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        Repr(self, DEBUG_STREAM_DEPTH).fmt(f)
    }
}

#[cfg(test)]
mod tests;
