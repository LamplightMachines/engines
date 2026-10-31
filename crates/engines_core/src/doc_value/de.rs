//! Reading a [`DocValue`]: its visitor (any serde source → a tree), and the tree as a serde
//! `Deserializer` ([`from_doc_value`]) — the calls `serde_json::Value`'s own deserializer makes, so a
//! struct reads the same from either tree, with the same error messages.

use super::{DocValue, Error, Map, Number, N};
use serde::de::value::{MapDeserializer, SeqDeserializer};
use serde::de::{self, DeserializeOwned, Deserializer, EnumAccess, IntoDeserializer, MapAccess, SeqAccess, Unexpected, VariantAccess, Visitor};
use std::fmt;

/// Deserialize `T` from a tree (`LoopDoc::from_value`, a song block, a dither move).
pub fn from_doc_value<T: DeserializeOwned>(v: DocValue) -> Result<T, Error> {
    T::deserialize(v)
}

pub(super) struct ValueVisitor;

impl<'de> Visitor<'de> for ValueVisitor {
    type Value = DocValue;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("any JSON5 value")
    }
    fn visit_bool<E>(self, b: bool) -> Result<DocValue, E> {
        Ok(DocValue::Bool(b))
    }
    fn visit_i64<E>(self, n: i64) -> Result<DocValue, E> {
        Ok(DocValue::Number(n.into()))
    }
    fn visit_u64<E>(self, n: u64) -> Result<DocValue, E> {
        Ok(DocValue::Number(n.into()))
    }
    fn visit_f64<E>(self, n: f64) -> Result<DocValue, E> {
        Ok(DocValue::Number(n.into()))
    }
    fn visit_str<E>(self, s: &str) -> Result<DocValue, E> {
        Ok(DocValue::String(s.to_string()))
    }
    fn visit_string<E>(self, s: String) -> Result<DocValue, E> {
        Ok(DocValue::String(s))
    }
    fn visit_unit<E>(self) -> Result<DocValue, E> {
        Ok(DocValue::Null)
    }
    fn visit_none<E>(self) -> Result<DocValue, E> {
        Ok(DocValue::Null)
    }
    fn visit_some<D: Deserializer<'de>>(self, d: D) -> Result<DocValue, D::Error> {
        d.deserialize_any(self)
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<DocValue, A::Error> {
        let mut out = Vec::new();
        while let Some(v) = seq.next_element()? {
            out.push(v);
        }
        Ok(DocValue::Array(out))
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<DocValue, A::Error> {
        let mut out = Map::new();
        while let Some((k, v)) = map.next_entry::<String, DocValue>()? {
            out.insert(k, v);
        }
        Ok(DocValue::Object(out))
    }
}

impl DocValue {
    fn unexpected(&self) -> Unexpected<'_> {
        match self {
            DocValue::Null => Unexpected::Unit,
            DocValue::Bool(b) => Unexpected::Bool(*b),
            DocValue::Number(Number(N::PosInt(u))) => Unexpected::Unsigned(*u),
            DocValue::Number(Number(N::NegInt(i))) => Unexpected::Signed(*i),
            DocValue::Number(Number(N::Float(f))) => Unexpected::Float(*f),
            DocValue::String(s) => Unexpected::Str(s),
            DocValue::Array(_) => Unexpected::Seq,
            DocValue::Object(_) => Unexpected::Map,
        }
    }
}

fn visit_array<'de, V: Visitor<'de>>(a: Vec<DocValue>, visitor: V) -> Result<V::Value, Error> {
    let mut seq = SeqDeserializer::new(a.into_iter());
    let v = visitor.visit_seq(&mut seq)?;
    seq.end()?;
    Ok(v)
}

fn visit_object<'de, V: Visitor<'de>>(m: Map, visitor: V) -> Result<V::Value, Error> {
    let mut map = MapDeserializer::new(m.into_iter().map(|(k, v)| (Key(k), v)));
    let v = visitor.visit_map(&mut map)?;
    map.end()?;
    Ok(v)
}

impl<'de> Deserializer<'de> for DocValue {
    type Error = Error;

    fn deserialize_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        match self {
            DocValue::Null => visitor.visit_unit(),
            DocValue::Bool(b) => visitor.visit_bool(b),
            DocValue::Number(Number(N::PosInt(u))) => visitor.visit_u64(u),
            DocValue::Number(Number(N::NegInt(i))) => visitor.visit_i64(i),
            DocValue::Number(Number(N::Float(f))) => visitor.visit_f64(f),
            DocValue::String(s) => visitor.visit_string(s),
            DocValue::Array(a) => visit_array(a, visitor),
            DocValue::Object(m) => visit_object(m, visitor),
        }
    }

    fn deserialize_option<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        match self {
            DocValue::Null => visitor.visit_none(),
            other => visitor.visit_some(other),
        }
    }

    fn deserialize_enum<V: Visitor<'de>>(self, _name: &'static str, _variants: &'static [&'static str], visitor: V) -> Result<V::Value, Error> {
        match self {
            DocValue::String(s) => visitor.visit_enum(s.into_deserializer()),
            DocValue::Object(m) => {
                let mut it = m.into_iter();
                match (it.next(), it.next()) {
                    (Some((variant, value)), None) => visitor.visit_enum(Variant { variant, value }),
                    _ => Err(de::Error::invalid_value(Unexpected::Map, &"map with a single key")),
                }
            }
            other => Err(de::Error::invalid_type(other.unexpected(), &"string or map")),
        }
    }

    fn deserialize_newtype_struct<V: Visitor<'de>>(self, _name: &'static str, visitor: V) -> Result<V::Value, Error> {
        visitor.visit_newtype_struct(self)
    }

    fn deserialize_ignored_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        drop(self);
        visitor.visit_unit()
    }

    serde::forward_to_deserialize_any! {
        bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string bytes byte_buf unit
        unit_struct seq tuple tuple_struct map struct identifier
    }
}

impl<'de> IntoDeserializer<'de, Error> for DocValue {
    type Deserializer = DocValue;
    fn into_deserializer(self) -> DocValue {
        self
    }
}

/// An object key: a string, which a numeric-keyed map reads as its number (as `serde_json` does).
struct Key(String);

impl<'de> IntoDeserializer<'de, Error> for Key {
    type Deserializer = Key;
    fn into_deserializer(self) -> Key {
        self
    }
}

macro_rules! key_number {
    ($($method:ident => $visit:ident: $t:ty),*) => {$(
        fn $method<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
            match self.0.parse::<$t>() {
                Ok(n) => visitor.$visit(n),
                Err(_) => visitor.visit_string(self.0),
            }
        }
    )*};
}

impl<'de> Deserializer<'de> for Key {
    type Error = Error;

    fn deserialize_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        visitor.visit_string(self.0)
    }

    key_number! {
        deserialize_i8 => visit_i8: i8, deserialize_i16 => visit_i16: i16, deserialize_i32 => visit_i32: i32,
        deserialize_i64 => visit_i64: i64, deserialize_u8 => visit_u8: u8, deserialize_u16 => visit_u16: u16,
        deserialize_u32 => visit_u32: u32, deserialize_u64 => visit_u64: u64
    }

    fn deserialize_option<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        visitor.visit_some(self)
    }

    fn deserialize_newtype_struct<V: Visitor<'de>>(self, _name: &'static str, visitor: V) -> Result<V::Value, Error> {
        visitor.visit_newtype_struct(self)
    }

    fn deserialize_enum<V: Visitor<'de>>(self, _name: &'static str, _variants: &'static [&'static str], visitor: V) -> Result<V::Value, Error> {
        visitor.visit_enum(self.0.into_deserializer())
    }

    serde::forward_to_deserialize_any! {
        bool i128 u128 f32 f64 char str string bytes byte_buf unit unit_struct seq tuple tuple_struct map
        struct identifier ignored_any
    }
}

/// An externally tagged enum's `{ "variant": value }`.
struct Variant {
    variant: String,
    value: DocValue,
}

impl<'de> EnumAccess<'de> for Variant {
    type Error = Error;
    type Variant = DocValue;
    fn variant_seed<S: de::DeserializeSeed<'de>>(self, seed: S) -> Result<(S::Value, DocValue), Error> {
        let v = seed.deserialize(IntoDeserializer::<Error>::into_deserializer(self.variant))?;
        Ok((v, self.value))
    }
}

impl<'de> VariantAccess<'de> for DocValue {
    type Error = Error;
    fn unit_variant(self) -> Result<(), Error> {
        match self {
            DocValue::Null => Ok(()),
            other => Err(de::Error::invalid_type(other.unexpected(), &"unit variant")),
        }
    }
    fn newtype_variant_seed<S: de::DeserializeSeed<'de>>(self, seed: S) -> Result<S::Value, Error> {
        seed.deserialize(self)
    }
    fn tuple_variant<V: Visitor<'de>>(self, _len: usize, visitor: V) -> Result<V::Value, Error> {
        match self {
            DocValue::Array(a) => visit_array(a, visitor),
            other => Err(de::Error::invalid_type(other.unexpected(), &"tuple variant")),
        }
    }
    fn struct_variant<V: Visitor<'de>>(self, _fields: &'static [&'static str], visitor: V) -> Result<V::Value, Error> {
        match self {
            DocValue::Object(m) => visit_object(m, visitor),
            other => Err(de::Error::invalid_type(other.unexpected(), &"struct variant")),
        }
    }
}
