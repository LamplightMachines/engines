//! Any `Serialize` type → a [`DocValue`] ([`to_doc_value`]): `serde_json::to_value`, except that a
//! non-finite float stays a number instead of becoming `null`.

use super::{DocValue, Error, Map};
use serde::ser::{self, Impossible, Serialize};

/// Serialize `v` into a tree (a doc's JSONPath edits, a song block's merge).
pub fn to_doc_value<T: Serialize + ?Sized>(v: &T) -> Result<DocValue, Error> {
    v.serialize(Serializer)
}

struct Serializer;

impl ser::Serializer for Serializer {
    type Ok = DocValue;
    type Error = Error;
    type SerializeSeq = Seq;
    type SerializeTuple = Seq;
    type SerializeTupleStruct = Seq;
    type SerializeTupleVariant = TupleVariant;
    type SerializeMap = Object;
    type SerializeStruct = Object;
    type SerializeStructVariant = StructVariant;

    fn serialize_bool(self, v: bool) -> Result<DocValue, Error> {
        Ok(v.into())
    }
    fn serialize_i8(self, v: i8) -> Result<DocValue, Error> {
        Ok(v.into())
    }
    fn serialize_i16(self, v: i16) -> Result<DocValue, Error> {
        Ok(v.into())
    }
    fn serialize_i32(self, v: i32) -> Result<DocValue, Error> {
        Ok(v.into())
    }
    fn serialize_i64(self, v: i64) -> Result<DocValue, Error> {
        Ok(v.into())
    }
    fn serialize_i128(self, v: i128) -> Result<DocValue, Error> {
        if let Ok(u) = u64::try_from(v) {
            Ok(u.into())
        } else if let Ok(i) = i64::try_from(v) {
            Ok(i.into())
        } else {
            Err(Error::msg("number out of range"))
        }
    }
    fn serialize_u8(self, v: u8) -> Result<DocValue, Error> {
        Ok(v.into())
    }
    fn serialize_u16(self, v: u16) -> Result<DocValue, Error> {
        Ok(v.into())
    }
    fn serialize_u32(self, v: u32) -> Result<DocValue, Error> {
        Ok(v.into())
    }
    fn serialize_u64(self, v: u64) -> Result<DocValue, Error> {
        Ok(v.into())
    }
    fn serialize_u128(self, v: u128) -> Result<DocValue, Error> {
        u64::try_from(v).map(DocValue::from).map_err(|_| Error::msg("number out of range"))
    }
    fn serialize_f32(self, v: f32) -> Result<DocValue, Error> {
        Ok(v.into())
    }
    fn serialize_f64(self, v: f64) -> Result<DocValue, Error> {
        Ok(v.into())
    }
    fn serialize_char(self, v: char) -> Result<DocValue, Error> {
        Ok(DocValue::String(v.to_string()))
    }
    fn serialize_str(self, v: &str) -> Result<DocValue, Error> {
        Ok(v.into())
    }
    fn serialize_bytes(self, v: &[u8]) -> Result<DocValue, Error> {
        Ok(DocValue::Array(v.iter().map(|&b| b.into()).collect()))
    }
    fn serialize_none(self) -> Result<DocValue, Error> {
        Ok(DocValue::Null)
    }
    fn serialize_some<T: Serialize + ?Sized>(self, v: &T) -> Result<DocValue, Error> {
        v.serialize(self)
    }
    fn serialize_unit(self) -> Result<DocValue, Error> {
        Ok(DocValue::Null)
    }
    fn serialize_unit_struct(self, _name: &'static str) -> Result<DocValue, Error> {
        Ok(DocValue::Null)
    }
    fn serialize_unit_variant(self, _name: &'static str, _index: u32, variant: &'static str) -> Result<DocValue, Error> {
        Ok(variant.into())
    }
    fn serialize_newtype_struct<T: Serialize + ?Sized>(self, _name: &'static str, v: &T) -> Result<DocValue, Error> {
        v.serialize(self)
    }
    fn serialize_newtype_variant<T: Serialize + ?Sized>(self, _name: &'static str, _index: u32, variant: &'static str, v: &T) -> Result<DocValue, Error> {
        Ok(DocValue::Object(Map::from([(variant.to_string(), to_doc_value(v)?)])))
    }
    fn serialize_seq(self, len: Option<usize>) -> Result<Seq, Error> {
        Ok(Seq(Vec::with_capacity(len.unwrap_or(0))))
    }
    fn serialize_tuple(self, len: usize) -> Result<Seq, Error> {
        self.serialize_seq(Some(len))
    }
    fn serialize_tuple_struct(self, _name: &'static str, len: usize) -> Result<Seq, Error> {
        self.serialize_seq(Some(len))
    }
    fn serialize_tuple_variant(self, _name: &'static str, _index: u32, variant: &'static str, len: usize) -> Result<TupleVariant, Error> {
        Ok(TupleVariant { variant, items: Vec::with_capacity(len) })
    }
    fn serialize_map(self, _len: Option<usize>) -> Result<Object, Error> {
        Ok(Object { map: Map::new(), key: None })
    }
    fn serialize_struct(self, _name: &'static str, len: usize) -> Result<Object, Error> {
        self.serialize_map(Some(len))
    }
    fn serialize_struct_variant(self, _name: &'static str, _index: u32, variant: &'static str, _len: usize) -> Result<StructVariant, Error> {
        Ok(StructVariant { variant, map: Map::new() })
    }
}

struct Seq(Vec<DocValue>);

impl ser::SerializeSeq for Seq {
    type Ok = DocValue;
    type Error = Error;
    fn serialize_element<T: Serialize + ?Sized>(&mut self, v: &T) -> Result<(), Error> {
        self.0.push(to_doc_value(v)?);
        Ok(())
    }
    fn end(self) -> Result<DocValue, Error> {
        Ok(DocValue::Array(self.0))
    }
}
impl ser::SerializeTuple for Seq {
    type Ok = DocValue;
    type Error = Error;
    fn serialize_element<T: Serialize + ?Sized>(&mut self, v: &T) -> Result<(), Error> {
        ser::SerializeSeq::serialize_element(self, v)
    }
    fn end(self) -> Result<DocValue, Error> {
        ser::SerializeSeq::end(self)
    }
}
impl ser::SerializeTupleStruct for Seq {
    type Ok = DocValue;
    type Error = Error;
    fn serialize_field<T: Serialize + ?Sized>(&mut self, v: &T) -> Result<(), Error> {
        ser::SerializeSeq::serialize_element(self, v)
    }
    fn end(self) -> Result<DocValue, Error> {
        ser::SerializeSeq::end(self)
    }
}

struct TupleVariant {
    variant: &'static str,
    items: Vec<DocValue>,
}

impl ser::SerializeTupleVariant for TupleVariant {
    type Ok = DocValue;
    type Error = Error;
    fn serialize_field<T: Serialize + ?Sized>(&mut self, v: &T) -> Result<(), Error> {
        self.items.push(to_doc_value(v)?);
        Ok(())
    }
    fn end(self) -> Result<DocValue, Error> {
        Ok(DocValue::Object(Map::from([(self.variant.to_string(), DocValue::Array(self.items))])))
    }
}

struct Object {
    map: Map,
    key: Option<String>,
}

impl ser::SerializeMap for Object {
    type Ok = DocValue;
    type Error = Error;
    fn serialize_key<T: Serialize + ?Sized>(&mut self, k: &T) -> Result<(), Error> {
        self.key = Some(k.serialize(KeySerializer)?);
        Ok(())
    }
    fn serialize_value<T: Serialize + ?Sized>(&mut self, v: &T) -> Result<(), Error> {
        let k = self.key.take().ok_or_else(|| Error::msg("serialize_value called before serialize_key"))?;
        self.map.insert(k, to_doc_value(v)?);
        Ok(())
    }
    fn end(self) -> Result<DocValue, Error> {
        Ok(DocValue::Object(self.map))
    }
}
impl ser::SerializeStruct for Object {
    type Ok = DocValue;
    type Error = Error;
    fn serialize_field<T: Serialize + ?Sized>(&mut self, key: &'static str, v: &T) -> Result<(), Error> {
        self.map.insert(key.to_string(), to_doc_value(v)?);
        Ok(())
    }
    fn end(self) -> Result<DocValue, Error> {
        Ok(DocValue::Object(self.map))
    }
}

struct StructVariant {
    variant: &'static str,
    map: Map,
}

impl ser::SerializeStructVariant for StructVariant {
    type Ok = DocValue;
    type Error = Error;
    fn serialize_field<T: Serialize + ?Sized>(&mut self, key: &'static str, v: &T) -> Result<(), Error> {
        self.map.insert(key.to_string(), to_doc_value(v)?);
        Ok(())
    }
    fn end(self) -> Result<DocValue, Error> {
        Ok(DocValue::Object(Map::from([(self.variant.to_string(), DocValue::Object(self.map))])))
    }
}

/// An object key as text: a string, or a number, bool or char written as one (as `serde_json` does).
pub(super) struct KeySerializer;

fn key_error() -> Error {
    Error::msg("key must be a string")
}

impl ser::Serializer for KeySerializer {
    type Ok = String;
    type Error = Error;
    type SerializeSeq = Impossible<String, Error>;
    type SerializeTuple = Impossible<String, Error>;
    type SerializeTupleStruct = Impossible<String, Error>;
    type SerializeTupleVariant = Impossible<String, Error>;
    type SerializeMap = Impossible<String, Error>;
    type SerializeStruct = Impossible<String, Error>;
    type SerializeStructVariant = Impossible<String, Error>;

    fn serialize_bool(self, v: bool) -> Result<String, Error> {
        Ok(v.to_string())
    }
    fn serialize_i8(self, v: i8) -> Result<String, Error> {
        Ok(v.to_string())
    }
    fn serialize_i16(self, v: i16) -> Result<String, Error> {
        Ok(v.to_string())
    }
    fn serialize_i32(self, v: i32) -> Result<String, Error> {
        Ok(v.to_string())
    }
    fn serialize_i64(self, v: i64) -> Result<String, Error> {
        Ok(v.to_string())
    }
    fn serialize_i128(self, v: i128) -> Result<String, Error> {
        Ok(v.to_string())
    }
    fn serialize_u8(self, v: u8) -> Result<String, Error> {
        Ok(v.to_string())
    }
    fn serialize_u16(self, v: u16) -> Result<String, Error> {
        Ok(v.to_string())
    }
    fn serialize_u32(self, v: u32) -> Result<String, Error> {
        Ok(v.to_string())
    }
    fn serialize_u64(self, v: u64) -> Result<String, Error> {
        Ok(v.to_string())
    }
    fn serialize_u128(self, v: u128) -> Result<String, Error> {
        Ok(v.to_string())
    }
    fn serialize_f32(self, _v: f32) -> Result<String, Error> {
        Err(key_error())
    }
    fn serialize_f64(self, _v: f64) -> Result<String, Error> {
        Err(key_error())
    }
    fn serialize_char(self, v: char) -> Result<String, Error> {
        Ok(v.to_string())
    }
    fn serialize_str(self, v: &str) -> Result<String, Error> {
        Ok(v.to_string())
    }
    fn serialize_bytes(self, _v: &[u8]) -> Result<String, Error> {
        Err(key_error())
    }
    fn serialize_none(self) -> Result<String, Error> {
        Err(key_error())
    }
    fn serialize_some<T: Serialize + ?Sized>(self, _v: &T) -> Result<String, Error> {
        Err(key_error())
    }
    fn serialize_unit(self) -> Result<String, Error> {
        Err(key_error())
    }
    fn serialize_unit_struct(self, _name: &'static str) -> Result<String, Error> {
        Err(key_error())
    }
    fn serialize_unit_variant(self, _name: &'static str, _index: u32, variant: &'static str) -> Result<String, Error> {
        Ok(variant.to_string())
    }
    fn serialize_newtype_struct<T: Serialize + ?Sized>(self, _name: &'static str, v: &T) -> Result<String, Error> {
        v.serialize(self)
    }
    fn serialize_newtype_variant<T: Serialize + ?Sized>(self, _name: &'static str, _index: u32, _variant: &'static str, _v: &T) -> Result<String, Error> {
        Err(key_error())
    }
    fn serialize_seq(self, _len: Option<usize>) -> Result<Self::SerializeSeq, Error> {
        Err(key_error())
    }
    fn serialize_tuple(self, _len: usize) -> Result<Self::SerializeTuple, Error> {
        Err(key_error())
    }
    fn serialize_tuple_struct(self, _name: &'static str, _len: usize) -> Result<Self::SerializeTupleStruct, Error> {
        Err(key_error())
    }
    fn serialize_tuple_variant(self, _name: &'static str, _index: u32, _variant: &'static str, _len: usize) -> Result<Self::SerializeTupleVariant, Error> {
        Err(key_error())
    }
    fn serialize_map(self, _len: Option<usize>) -> Result<Self::SerializeMap, Error> {
        Err(key_error())
    }
    fn serialize_struct(self, _name: &'static str, _len: usize) -> Result<Self::SerializeStruct, Error> {
        Err(key_error())
    }
    fn serialize_struct_variant(self, _name: &'static str, _index: u32, _variant: &'static str, _len: usize) -> Result<Self::SerializeStructVariant, Error> {
        Err(key_error())
    }
}
