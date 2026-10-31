//! The doc writer: any `Serialize` type → JSON text **byte-identical to `serde_json`'s** (compact and
//! pretty, the same float and string formatting, struct fields in declaration order), except that
//! `-inf` is written `-Infinity` where `serde_json` writes `null`. That's valid JSON5 (every doc
//! reader takes it) and Python's `json` reads it. `+Infinity` and `NaN` are refused.

use super::Error;
use serde::ser::{self, Serialize};
use std::io::Write;

/// Compact text, like `serde_json::to_string`.
pub fn to_string<T: Serialize + ?Sized>(v: &T) -> Result<String, Error> {
    let mut out = Vec::new();
    to_writer(&mut out, v)?;
    Ok(String::from_utf8(out).expect("the writer writes UTF-8"))
}

/// Pretty text (two-space indent), like `serde_json::to_string_pretty`.
pub fn to_string_pretty<T: Serialize + ?Sized>(v: &T) -> Result<String, Error> {
    let mut out = Vec::new();
    to_writer_pretty(&mut out, v)?;
    Ok(String::from_utf8(out).expect("the writer writes UTF-8"))
}

/// Compact text into `w`, like `serde_json::to_writer`. An I/O error keeps its message.
pub fn to_writer<W: Write, T: Serialize + ?Sized>(w: W, v: &T) -> Result<(), Error> {
    v.serialize(&mut Emitter { w, fmt: Fmt { pretty: false, indent: 0, has_value: false } })
}

/// Pretty text into `w`, like `serde_json::to_writer_pretty`.
pub fn to_writer_pretty<W: Write, T: Serialize + ?Sized>(w: W, v: &T) -> Result<(), Error> {
    v.serialize(&mut Emitter { w, fmt: Fmt { pretty: true, indent: 0, has_value: false } })
}

/// `serde_json`'s `CompactFormatter` and `PrettyFormatter` in one, with the same single `has_value`
/// flag the pretty one keeps (it decides whether a closing bracket goes on its own line).
struct Fmt {
    pretty: bool,
    indent: usize,
    has_value: bool,
}

impl Fmt {
    fn newline<W: Write>(&self, w: &mut W) -> std::io::Result<()> {
        w.write_all(b"\n")?;
        for _ in 0..self.indent {
            w.write_all(b"  ")?;
        }
        Ok(())
    }
    fn begin<W: Write>(&mut self, w: &mut W, open: &[u8]) -> std::io::Result<()> {
        if self.pretty {
            self.indent += 1;
            self.has_value = false;
        }
        w.write_all(open)
    }
    fn end<W: Write>(&mut self, w: &mut W, close: &[u8]) -> std::io::Result<()> {
        if self.pretty {
            self.indent -= 1;
            if self.has_value {
                self.newline(w)?;
            }
        }
        w.write_all(close)
    }
    /// Before an array item or an object key.
    fn item<W: Write>(&mut self, w: &mut W, first: bool) -> std::io::Result<()> {
        if !first {
            w.write_all(b",")?;
        }
        if self.pretty {
            self.newline(w)?;
        }
        Ok(())
    }
    fn colon<W: Write>(&mut self, w: &mut W) -> std::io::Result<()> {
        w.write_all(if self.pretty { b": " } else { b":" })
    }
    fn item_done(&mut self) {
        self.has_value = true;
    }
}

struct Emitter<W: Write> {
    w: W,
    fmt: Fmt,
}

fn non_finite(f: f64) -> Result<&'static str, Error> {
    if f.is_nan() {
        Err(Error::msg("NaN can't be written in a doc"))
    } else if f > 0.0 {
        Err(Error::msg("Infinity can't be written in a doc (only -Infinity, silence, can)"))
    } else {
        Ok("-Infinity")
    }
}

impl<W: Write> Emitter<W> {
    fn raw(&mut self, s: &str) -> Result<(), Error> {
        self.w.write_all(s.as_bytes()).map_err(Error::from)
    }
    fn json<T: Serialize + ?Sized>(&mut self, v: &T) -> Result<(), Error> {
        serde_json::to_writer(&mut self.w, v).map_err(Error::msg)
    }
    fn begin_variant(&mut self, variant: &str) -> Result<(), Error> {
        self.fmt.begin(&mut self.w, b"{")?;
        self.fmt.item(&mut self.w, true)?;
        self.json(variant)?;
        self.fmt.colon(&mut self.w)?;
        Ok(())
    }
    fn end_variant(&mut self) -> Result<(), Error> {
        self.fmt.item_done();
        self.fmt.end(&mut self.w, b"}").map_err(Error::from)
    }
}

#[derive(PartialEq)]
enum State {
    Empty,
    First,
    Rest,
}

struct Compound<'a, W: Write> {
    e: &'a mut Emitter<W>,
    state: State,
    /// An enum variant's payload: closing it also closes the `{ "variant": … }` wrapper.
    variant: bool,
}

impl<'a, W: Write> ser::Serializer for &'a mut Emitter<W> {
    type Ok = ();
    type Error = Error;
    type SerializeSeq = Compound<'a, W>;
    type SerializeTuple = Compound<'a, W>;
    type SerializeTupleStruct = Compound<'a, W>;
    type SerializeTupleVariant = Compound<'a, W>;
    type SerializeMap = Compound<'a, W>;
    type SerializeStruct = Compound<'a, W>;
    type SerializeStructVariant = Compound<'a, W>;

    fn serialize_bool(self, v: bool) -> Result<(), Error> {
        self.raw(if v { "true" } else { "false" })
    }
    fn serialize_i8(self, v: i8) -> Result<(), Error> {
        self.raw(&v.to_string())
    }
    fn serialize_i16(self, v: i16) -> Result<(), Error> {
        self.raw(&v.to_string())
    }
    fn serialize_i32(self, v: i32) -> Result<(), Error> {
        self.raw(&v.to_string())
    }
    fn serialize_i64(self, v: i64) -> Result<(), Error> {
        self.raw(&v.to_string())
    }
    fn serialize_i128(self, v: i128) -> Result<(), Error> {
        self.raw(&v.to_string())
    }
    fn serialize_u8(self, v: u8) -> Result<(), Error> {
        self.raw(&v.to_string())
    }
    fn serialize_u16(self, v: u16) -> Result<(), Error> {
        self.raw(&v.to_string())
    }
    fn serialize_u32(self, v: u32) -> Result<(), Error> {
        self.raw(&v.to_string())
    }
    fn serialize_u64(self, v: u64) -> Result<(), Error> {
        self.raw(&v.to_string())
    }
    fn serialize_u128(self, v: u128) -> Result<(), Error> {
        self.raw(&v.to_string())
    }
    fn serialize_f32(self, v: f32) -> Result<(), Error> {
        if v.is_finite() {
            self.json(&v)
        } else {
            self.raw(non_finite(v as f64)?)
        }
    }
    fn serialize_f64(self, v: f64) -> Result<(), Error> {
        if v.is_finite() {
            self.json(&v)
        } else {
            self.raw(non_finite(v)?)
        }
    }
    fn serialize_char(self, v: char) -> Result<(), Error> {
        self.json(&v)
    }
    fn serialize_str(self, v: &str) -> Result<(), Error> {
        self.json(v)
    }
    fn serialize_bytes(self, v: &[u8]) -> Result<(), Error> {
        use ser::SerializeSeq;
        let mut seq = self.serialize_seq(Some(v.len()))?;
        for b in v {
            seq.serialize_element(b)?;
        }
        seq.end()
    }
    fn serialize_none(self) -> Result<(), Error> {
        self.raw("null")
    }
    fn serialize_some<T: Serialize + ?Sized>(self, v: &T) -> Result<(), Error> {
        v.serialize(self)
    }
    fn serialize_unit(self) -> Result<(), Error> {
        self.raw("null")
    }
    fn serialize_unit_struct(self, _name: &'static str) -> Result<(), Error> {
        self.raw("null")
    }
    fn serialize_unit_variant(self, _name: &'static str, _index: u32, variant: &'static str) -> Result<(), Error> {
        self.json(variant)
    }
    fn serialize_newtype_struct<T: Serialize + ?Sized>(self, _name: &'static str, v: &T) -> Result<(), Error> {
        v.serialize(self)
    }
    fn serialize_newtype_variant<T: Serialize + ?Sized>(self, _name: &'static str, _index: u32, variant: &'static str, v: &T) -> Result<(), Error> {
        self.begin_variant(variant)?;
        v.serialize(&mut *self)?;
        self.end_variant()
    }
    fn serialize_seq(self, len: Option<usize>) -> Result<Compound<'a, W>, Error> {
        self.fmt.begin(&mut self.w, b"[")?;
        let state = if len == Some(0) {
            self.fmt.end(&mut self.w, b"]")?;
            State::Empty
        } else {
            State::First
        };
        Ok(Compound { e: self, state, variant: false })
    }
    fn serialize_tuple(self, len: usize) -> Result<Compound<'a, W>, Error> {
        self.serialize_seq(Some(len))
    }
    fn serialize_tuple_struct(self, _name: &'static str, len: usize) -> Result<Compound<'a, W>, Error> {
        self.serialize_seq(Some(len))
    }
    fn serialize_tuple_variant(self, _name: &'static str, _index: u32, variant: &'static str, len: usize) -> Result<Compound<'a, W>, Error> {
        self.begin_variant(variant)?;
        let mut c = self.serialize_seq(Some(len))?;
        c.variant = true;
        Ok(c)
    }
    fn serialize_map(self, len: Option<usize>) -> Result<Compound<'a, W>, Error> {
        self.fmt.begin(&mut self.w, b"{")?;
        let state = if len == Some(0) {
            self.fmt.end(&mut self.w, b"}")?;
            State::Empty
        } else {
            State::First
        };
        Ok(Compound { e: self, state, variant: false })
    }
    fn serialize_struct(self, _name: &'static str, len: usize) -> Result<Compound<'a, W>, Error> {
        self.serialize_map(Some(len))
    }
    fn serialize_struct_variant(self, _name: &'static str, _index: u32, variant: &'static str, len: usize) -> Result<Compound<'a, W>, Error> {
        self.begin_variant(variant)?;
        let mut c = self.serialize_map(Some(len))?;
        c.variant = true;
        Ok(c)
    }
}

impl<W: Write> Compound<'_, W> {
    fn element<T: Serialize + ?Sized>(&mut self, v: &T) -> Result<(), Error> {
        self.e.fmt.item(&mut self.e.w, self.state == State::First)?;
        self.state = State::Rest;
        v.serialize(&mut *self.e)?;
        self.e.fmt.item_done();
        Ok(())
    }
    fn close(self, bracket: &[u8]) -> Result<(), Error> {
        if self.state != State::Empty {
            self.e.fmt.end(&mut self.e.w, bracket)?;
        }
        if self.variant {
            self.e.end_variant()?;
        }
        Ok(())
    }
    fn key<T: Serialize + ?Sized>(&mut self, k: &T) -> Result<(), Error> {
        self.e.fmt.item(&mut self.e.w, self.state == State::First)?;
        self.state = State::Rest;
        let text = k.serialize(super::ser::KeySerializer)?;
        self.e.json(&text)
    }
    fn value<T: Serialize + ?Sized>(&mut self, v: &T) -> Result<(), Error> {
        self.e.fmt.colon(&mut self.e.w)?;
        v.serialize(&mut *self.e)?;
        self.e.fmt.item_done();
        Ok(())
    }
}

impl<W: Write> ser::SerializeSeq for Compound<'_, W> {
    type Ok = ();
    type Error = Error;
    fn serialize_element<T: Serialize + ?Sized>(&mut self, v: &T) -> Result<(), Error> {
        self.element(v)
    }
    fn end(self) -> Result<(), Error> {
        self.close(b"]")
    }
}
impl<W: Write> ser::SerializeTuple for Compound<'_, W> {
    type Ok = ();
    type Error = Error;
    fn serialize_element<T: Serialize + ?Sized>(&mut self, v: &T) -> Result<(), Error> {
        self.element(v)
    }
    fn end(self) -> Result<(), Error> {
        self.close(b"]")
    }
}
impl<W: Write> ser::SerializeTupleStruct for Compound<'_, W> {
    type Ok = ();
    type Error = Error;
    fn serialize_field<T: Serialize + ?Sized>(&mut self, v: &T) -> Result<(), Error> {
        self.element(v)
    }
    fn end(self) -> Result<(), Error> {
        self.close(b"]")
    }
}
impl<W: Write> ser::SerializeTupleVariant for Compound<'_, W> {
    type Ok = ();
    type Error = Error;
    fn serialize_field<T: Serialize + ?Sized>(&mut self, v: &T) -> Result<(), Error> {
        self.element(v)
    }
    fn end(self) -> Result<(), Error> {
        self.close(b"]")
    }
}
impl<W: Write> ser::SerializeMap for Compound<'_, W> {
    type Ok = ();
    type Error = Error;
    fn serialize_key<T: Serialize + ?Sized>(&mut self, k: &T) -> Result<(), Error> {
        self.key(k)
    }
    fn serialize_value<T: Serialize + ?Sized>(&mut self, v: &T) -> Result<(), Error> {
        self.value(v)
    }
    fn end(self) -> Result<(), Error> {
        self.close(b"}")
    }
}
impl<W: Write> ser::SerializeStruct for Compound<'_, W> {
    type Ok = ();
    type Error = Error;
    fn serialize_field<T: Serialize + ?Sized>(&mut self, key: &'static str, v: &T) -> Result<(), Error> {
        self.key(key)?;
        self.value(v)
    }
    fn end(self) -> Result<(), Error> {
        self.close(b"}")
    }
}
impl<W: Write> ser::SerializeStructVariant for Compound<'_, W> {
    type Ok = ();
    type Error = Error;
    fn serialize_field<T: Serialize + ?Sized>(&mut self, key: &'static str, v: &T) -> Result<(), Error> {
        self.key(key)?;
        self.value(v)
    }
    fn end(self) -> Result<(), Error> {
        self.close(b"}")
    }
}
