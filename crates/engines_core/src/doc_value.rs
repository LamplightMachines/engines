//! The doc value tree (ADR 0054 P3): `serde_json::Value`'s shape and API, except that a number may
//! be non-finite, so a `-Infinity` gain (silence, schema v0.9) survives every parse, edit and write.
//! `serde_json::Value` turns a non-finite float into `null`, which would make a silent voice play at
//! its default.
//!
//! - **Reading:** `DocValue` deserializes from the `json5` parser (which hands `-inf` to a visitor)
//!   or from any other serde source; [`from_doc_value`] deserializes a type out of one.
//! - **Editing:** [`to_doc_value`] serializes a type into one, so a doc's JSONPath edits
//!   (`LoopDoc::with_param`, song blocks) round-trip it without loss.
//! - **Writing:** [`to_string`] / [`to_string_pretty`] write JSON exactly as `serde_json` does, byte
//!   for byte, except that `-inf` is written `-Infinity` (valid JSON5, and Python's `json` reads it).
//!   `+Infinity` and `NaN` are refused: neither means anything in a doc.
//!
//! Strict JSON channels (`status.json`, the `engines-live` protocol, the C ABI's telemetry) keep using
//! `serde_json`: TypeScript's `JSON.parse` doesn't accept `-Infinity`.
//!
//! Like `serde_json` without `preserve_order`, an object is a `BTreeMap`: keys iterate sorted.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;

mod de;
mod emit;
mod ser;
#[cfg(test)]
mod tests;

pub use de::from_doc_value;
pub use emit::{to_string, to_string_pretty, to_writer, to_writer_pretty};
pub use ser::to_doc_value;

/// An object's members, sorted by key.
pub type Map = BTreeMap<String, DocValue>;

/// A JSON5 value whose numbers may be non-finite. See the [module docs](self).
#[derive(Clone, Debug, Default, PartialEq)]
pub enum DocValue {
    #[default]
    Null,
    Bool(bool),
    Number(Number),
    String(String),
    Array(Vec<DocValue>),
    Object(Map),
}

/// A number as the parser handed it over: an integer (`u64`/`i64`, like `serde_json::Number`) or a
/// float, which may be non-finite.
#[derive(Clone, Copy, PartialEq)]
pub struct Number(N);

#[derive(Clone, Copy, PartialEq)]
enum N {
    PosInt(u64),
    /// Always negative.
    NegInt(i64),
    Float(f64),
}

impl Number {
    pub fn as_f64(&self) -> Option<f64> {
        Some(match self.0 {
            N::PosInt(u) => u as f64,
            N::NegInt(i) => i as f64,
            N::Float(f) => f,
        })
    }
    pub fn as_u64(&self) -> Option<u64> {
        match self.0 {
            N::PosInt(u) => Some(u),
            _ => None,
        }
    }
    pub fn as_i64(&self) -> Option<i64> {
        match self.0 {
            N::PosInt(u) => i64::try_from(u).ok(),
            N::NegInt(i) => Some(i),
            N::Float(_) => None,
        }
    }
    pub fn is_u64(&self) -> bool {
        matches!(self.0, N::PosInt(_))
    }
    pub fn is_i64(&self) -> bool {
        match self.0 {
            N::PosInt(u) => u <= i64::MAX as u64,
            N::NegInt(_) => true,
            N::Float(_) => false,
        }
    }
    pub fn is_f64(&self) -> bool {
        matches!(self.0, N::Float(_))
    }
    /// False only for `±Infinity` and `NaN`.
    pub fn is_finite(&self) -> bool {
        match self.0 {
            N::Float(f) => f.is_finite(),
            _ => true,
        }
    }
}

impl From<u64> for Number {
    fn from(u: u64) -> Self {
        Number(N::PosInt(u))
    }
}
impl From<i64> for Number {
    fn from(i: i64) -> Self {
        Number(if i < 0 { N::NegInt(i) } else { N::PosInt(i as u64) })
    }
}
impl From<f64> for Number {
    fn from(f: f64) -> Self {
        Number(N::Float(f))
    }
}

impl fmt::Debug for Number {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Number({self})")
    }
}

/// As JSON5: `serde_json`'s text for a finite number, else `-Infinity`, `Infinity` or `NaN`.
impl fmt::Display for Number {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            N::PosInt(u) => write!(f, "{u}"),
            N::NegInt(i) => write!(f, "{i}"),
            N::Float(x) if x.is_nan() => f.write_str("NaN"),
            N::Float(x) if x.is_infinite() => f.write_str(if x < 0.0 { "-Infinity" } else { "Infinity" }),
            N::Float(x) => f.write_str(&serde_json::Number::from_f64(x).expect("finite").to_string()),
        }
    }
}

static NULL: DocValue = DocValue::Null;

impl DocValue {
    pub fn is_null(&self) -> bool {
        matches!(self, DocValue::Null)
    }
    pub fn is_boolean(&self) -> bool {
        matches!(self, DocValue::Bool(_))
    }
    pub fn is_number(&self) -> bool {
        matches!(self, DocValue::Number(_))
    }
    pub fn is_string(&self) -> bool {
        matches!(self, DocValue::String(_))
    }
    pub fn is_array(&self) -> bool {
        matches!(self, DocValue::Array(_))
    }
    pub fn is_object(&self) -> bool {
        matches!(self, DocValue::Object(_))
    }
    pub fn is_u64(&self) -> bool {
        matches!(self, DocValue::Number(n) if n.is_u64())
    }
    pub fn is_i64(&self) -> bool {
        matches!(self, DocValue::Number(n) if n.is_i64())
    }
    pub fn is_f64(&self) -> bool {
        matches!(self, DocValue::Number(n) if n.is_f64())
    }
    pub fn as_null(&self) -> Option<()> {
        self.is_null().then_some(())
    }
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            DocValue::Bool(b) => Some(*b),
            _ => None,
        }
    }
    /// The number as `f64` — `-inf` included, which is the point of this type.
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            DocValue::Number(n) => n.as_f64(),
            _ => None,
        }
    }
    pub fn as_u64(&self) -> Option<u64> {
        match self {
            DocValue::Number(n) => n.as_u64(),
            _ => None,
        }
    }
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            DocValue::Number(n) => n.as_i64(),
            _ => None,
        }
    }
    pub fn as_number(&self) -> Option<&Number> {
        match self {
            DocValue::Number(n) => Some(n),
            _ => None,
        }
    }
    pub fn as_str(&self) -> Option<&str> {
        match self {
            DocValue::String(s) => Some(s),
            _ => None,
        }
    }
    pub fn as_array(&self) -> Option<&Vec<DocValue>> {
        match self {
            DocValue::Array(a) => Some(a),
            _ => None,
        }
    }
    pub fn as_array_mut(&mut self) -> Option<&mut Vec<DocValue>> {
        match self {
            DocValue::Array(a) => Some(a),
            _ => None,
        }
    }
    pub fn as_object(&self) -> Option<&Map> {
        match self {
            DocValue::Object(m) => Some(m),
            _ => None,
        }
    }
    pub fn as_object_mut(&mut self) -> Option<&mut Map> {
        match self {
            DocValue::Object(m) => Some(m),
            _ => None,
        }
    }
    /// An object's member or an array's item (`None` for anything else, like `serde_json`).
    pub fn get<I: Index>(&self, index: I) -> Option<&DocValue> {
        index.index_into(self)
    }
    pub fn get_mut<I: Index>(&mut self, index: I) -> Option<&mut DocValue> {
        index.index_into_mut(self)
    }
    /// The node at an RFC 6901 pointer (`/instruments/pad/sends`, `/steps/0`), like `serde_json`.
    pub fn pointer(&self, pointer: &str) -> Option<&DocValue> {
        if pointer.is_empty() {
            return Some(self);
        }
        let rest = pointer.strip_prefix('/')?;
        rest.split('/').map(|t| t.replace("~1", "/").replace("~0", "~")).try_fold(self, |v, t| match v {
            DocValue::Object(m) => m.get(&t),
            DocValue::Array(a) => t.parse::<usize>().ok().and_then(|i| a.get(i)),
            _ => None,
        })
    }
    /// Replace with `Null`, returning what was there.
    pub fn take(&mut self) -> DocValue {
        std::mem::take(self)
    }
    /// A number that isn't finite (`±Infinity`, `NaN`).
    pub fn is_non_finite(&self) -> bool {
        matches!(self, DocValue::Number(n) if !n.is_finite())
    }

    /// The same tree as a `serde_json::Value` — **lossy**: a non-finite number becomes `null`, as
    /// `serde_json` itself would make it. For strict-JSON consumers only.
    pub fn to_json(&self) -> serde_json::Value {
        use serde_json::Value;
        match self {
            DocValue::Null => Value::Null,
            DocValue::Bool(b) => Value::Bool(*b),
            DocValue::Number(Number(N::PosInt(u))) => Value::Number((*u).into()),
            DocValue::Number(Number(N::NegInt(i))) => Value::Number((*i).into()),
            DocValue::Number(Number(N::Float(f))) => serde_json::Number::from_f64(*f).map_or(Value::Null, Value::Number),
            DocValue::String(s) => Value::String(s.clone()),
            DocValue::Array(a) => Value::Array(a.iter().map(DocValue::to_json).collect()),
            DocValue::Object(m) => Value::Object(m.iter().map(|(k, v)| (k.clone(), v.to_json())).collect()),
        }
    }
}

/// What [`DocValue::get`] takes: a key (`&str`, `String`) or an array index (`usize`).
pub trait Index {
    #[doc(hidden)]
    fn index_into<'v>(&self, v: &'v DocValue) -> Option<&'v DocValue>;
    #[doc(hidden)]
    fn index_into_mut<'v>(&self, v: &'v mut DocValue) -> Option<&'v mut DocValue>;
}
impl Index for usize {
    fn index_into<'v>(&self, v: &'v DocValue) -> Option<&'v DocValue> {
        v.as_array()?.get(*self)
    }
    fn index_into_mut<'v>(&self, v: &'v mut DocValue) -> Option<&'v mut DocValue> {
        v.as_array_mut()?.get_mut(*self)
    }
}
impl Index for str {
    fn index_into<'v>(&self, v: &'v DocValue) -> Option<&'v DocValue> {
        v.as_object()?.get(self)
    }
    fn index_into_mut<'v>(&self, v: &'v mut DocValue) -> Option<&'v mut DocValue> {
        v.as_object_mut()?.get_mut(self)
    }
}
impl Index for String {
    fn index_into<'v>(&self, v: &'v DocValue) -> Option<&'v DocValue> {
        self.as_str().index_into(v)
    }
    fn index_into_mut<'v>(&self, v: &'v mut DocValue) -> Option<&'v mut DocValue> {
        self.as_str().index_into_mut(v)
    }
}
impl<T: Index + ?Sized> Index for &T {
    fn index_into<'v>(&self, v: &'v DocValue) -> Option<&'v DocValue> {
        (**self).index_into(v)
    }
    fn index_into_mut<'v>(&self, v: &'v mut DocValue) -> Option<&'v mut DocValue> {
        (**self).index_into_mut(v)
    }
}

/// `v["key"]` / `v[0]`: `Null` when absent, like `serde_json`.
impl<I: Index> std::ops::Index<I> for DocValue {
    type Output = DocValue;
    fn index(&self, index: I) -> &DocValue {
        index.index_into(self).unwrap_or(&NULL)
    }
}

/// `v["key"] = …` inserts into an object (a `Null` becomes one), like `serde_json`; panics on
/// anything else.
impl std::ops::IndexMut<&str> for DocValue {
    fn index_mut(&mut self, key: &str) -> &mut DocValue {
        if self.is_null() {
            *self = DocValue::Object(Map::new());
        }
        match self {
            DocValue::Object(m) => m.entry(key.to_string()).or_default(),
            other => panic!("cannot index a {} with a key", kind(other)),
        }
    }
}

fn kind(v: &DocValue) -> &'static str {
    match v {
        DocValue::Null => "null",
        DocValue::Bool(_) => "boolean",
        DocValue::Number(_) => "number",
        DocValue::String(_) => "string",
        DocValue::Array(_) => "array",
        DocValue::Object(_) => "object",
    }
}

/// Compact JSON5: `serde_json`'s text with `-Infinity` (and `Infinity`/`NaN`, which a writer refuses)
/// spelled out — for messages and debugging.
impl fmt::Display for DocValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DocValue::Null => f.write_str("null"),
            DocValue::Bool(b) => write!(f, "{b}"),
            DocValue::Number(n) => write!(f, "{n}"),
            DocValue::String(s) => f.write_str(&serde_json::Value::String(s.clone()).to_string()),
            DocValue::Array(a) => {
                f.write_str("[")?;
                for (i, v) in a.iter().enumerate() {
                    if i > 0 {
                        f.write_str(",")?;
                    }
                    write!(f, "{v}")?;
                }
                f.write_str("]")
            }
            DocValue::Object(m) => {
                f.write_str("{")?;
                for (i, (k, v)) in m.iter().enumerate() {
                    if i > 0 {
                        f.write_str(",")?;
                    }
                    write!(f, "{}:{v}", serde_json::Value::String(k.clone()))?;
                }
                f.write_str("}")
            }
        }
    }
}

// ── conversions ──

impl From<serde_json::Value> for DocValue {
    fn from(v: serde_json::Value) -> Self {
        use serde_json::Value;
        match v {
            Value::Null => DocValue::Null,
            Value::Bool(b) => DocValue::Bool(b),
            Value::Number(n) => DocValue::Number(match (n.as_u64(), n.as_i64()) {
                (Some(u), _) => u.into(),
                (None, Some(i)) => i.into(),
                _ => n.as_f64().unwrap_or(f64::NAN).into(),
            }),
            Value::String(s) => DocValue::String(s),
            Value::Array(a) => DocValue::Array(a.into_iter().map(DocValue::from).collect()),
            Value::Object(m) => DocValue::Object(m.into_iter().map(|(k, v)| (k, DocValue::from(v))).collect()),
        }
    }
}
impl From<&serde_json::Value> for DocValue {
    fn from(v: &serde_json::Value) -> Self {
        DocValue::from(v.clone())
    }
}
impl From<bool> for DocValue {
    fn from(b: bool) -> Self {
        DocValue::Bool(b)
    }
}
impl From<f64> for DocValue {
    fn from(f: f64) -> Self {
        DocValue::Number(f.into())
    }
}
/// Widened like `serde_json` widens an `f32` (`0.3f32` is `0.30000001192092896`).
impl From<f32> for DocValue {
    fn from(f: f32) -> Self {
        DocValue::Number((f as f64).into())
    }
}
macro_rules! from_int {
    ($($t:ty => $via:ty),*) => {$(
        impl From<$t> for DocValue {
            fn from(n: $t) -> Self {
                DocValue::Number((n as $via).into())
            }
        }
    )*};
}
from_int!(u8 => u64, u16 => u64, u32 => u64, u64 => u64, usize => u64, i8 => i64, i16 => i64, i32 => i64, i64 => i64, isize => i64);
impl From<&str> for DocValue {
    fn from(s: &str) -> Self {
        DocValue::String(s.to_string())
    }
}
impl From<String> for DocValue {
    fn from(s: String) -> Self {
        DocValue::String(s)
    }
}
impl From<Vec<DocValue>> for DocValue {
    fn from(a: Vec<DocValue>) -> Self {
        DocValue::Array(a)
    }
}
impl From<Map> for DocValue {
    fn from(m: Map) -> Self {
        DocValue::Object(m)
    }
}
impl From<Number> for DocValue {
    fn from(n: Number) -> Self {
        DocValue::Number(n)
    }
}

impl PartialEq<serde_json::Value> for DocValue {
    fn eq(&self, other: &serde_json::Value) -> bool {
        *self == DocValue::from(other)
    }
}
impl PartialEq<DocValue> for serde_json::Value {
    fn eq(&self, other: &DocValue) -> bool {
        other == self
    }
}
impl PartialEq<str> for DocValue {
    fn eq(&self, other: &str) -> bool {
        self.as_str() == Some(other)
    }
}
impl PartialEq<&str> for DocValue {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == Some(*other)
    }
}
impl PartialEq<String> for DocValue {
    fn eq(&self, other: &String) -> bool {
        self.as_str() == Some(other.as_str())
    }
}
impl PartialEq<bool> for DocValue {
    fn eq(&self, other: &bool) -> bool {
        self.as_bool() == Some(*other)
    }
}
impl PartialEq<f64> for DocValue {
    fn eq(&self, other: &f64) -> bool {
        self.as_f64() == Some(*other)
    }
}
impl PartialEq<i64> for DocValue {
    fn eq(&self, other: &i64) -> bool {
        self.as_i64() == Some(*other)
    }
}

// ── serde ──

/// The same calls `serde_json::Value` makes, so serializing either tree writes the same text.
impl Serialize for DocValue {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::{SerializeMap, SerializeSeq};
        match self {
            DocValue::Null => s.serialize_unit(),
            DocValue::Bool(b) => s.serialize_bool(*b),
            DocValue::Number(Number(N::PosInt(u))) => s.serialize_u64(*u),
            DocValue::Number(Number(N::NegInt(i))) => s.serialize_i64(*i),
            DocValue::Number(Number(N::Float(f))) => s.serialize_f64(*f),
            DocValue::String(st) => s.serialize_str(st),
            DocValue::Array(a) => {
                let mut seq = s.serialize_seq(Some(a.len()))?;
                for v in a {
                    seq.serialize_element(v)?;
                }
                seq.end()
            }
            DocValue::Object(m) => {
                let mut map = s.serialize_map(Some(m.len()))?;
                for (k, v) in m {
                    map.serialize_entry(k, v)?;
                }
                map.end()
            }
        }
    }
}

/// Built like `serde_json::Value` (a repeated key keeps its last value), except that a non-finite
/// float stays a number.
impl<'de> Deserialize<'de> for DocValue {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        d.deserialize_any(de::ValueVisitor)
    }
}

/// The error of [`to_doc_value`], [`from_doc_value`] and the writers: a message, like a
/// `serde_json::Error` built from a value (no line or column).
#[derive(Clone, Debug, PartialEq)]
pub struct Error(String);

impl Error {
    fn msg(m: impl fmt::Display) -> Self {
        Error(m.to_string())
    }
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for Error {}
impl serde::de::Error for Error {
    fn custom<T: fmt::Display>(msg: T) -> Self {
        Error::msg(msg)
    }
}
impl serde::ser::Error for Error {
    fn custom<T: fmt::Display>(msg: T) -> Self {
        Error::msg(msg)
    }
}
impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::msg(e)
    }
}
