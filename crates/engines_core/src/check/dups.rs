//! JSON5 → a [`DocValue`] exactly like `json5::from_str::<DocValue>`, also noting every repeated
//! key: a map keeps the last one silently, so a copy-pasted route or instrument whose id wasn't
//! renamed would drop the original without a word.

use serde::de::{Deserialize, DeserializeSeed, Deserializer, MapAccess, SeqAccess, Visitor};
use crate::doc_value::{DocValue, Map};
use std::cell::RefCell;
use std::fmt;

/// Parse `src`, returning the value and the JSONPath of each repeated key (in source order).
pub(super) fn parse(src: &str) -> Result<(DocValue, Vec<String>), json5::Error> {
    json5::from_str::<Parsed>(src).map(|p| (p.value, p.dups))
}

struct Parsed {
    value: DocValue,
    dups: Vec<String>,
}

impl<'de> Deserialize<'de> for Parsed {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let dups = RefCell::new(Vec::new());
        let value = Seed { path: String::new(), dups: &dups }.deserialize(d)?;
        Ok(Parsed { value, dups: dups.into_inner() })
    }
}

struct Seed<'a> {
    path: String,
    dups: &'a RefCell<Vec<String>>,
}

impl<'de> DeserializeSeed<'de> for Seed<'_> {
    type Value = DocValue;
    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<DocValue, D::Error> {
        d.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for Seed<'_> {
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
    /// A non-finite float (`-Infinity`) stays a number, where `serde_json` would make it `null`.
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
        self.deserialize(d)
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<DocValue, A::Error> {
        let mut out = Vec::new();
        while let Some(v) = seq.next_element_seed(Seed { path: format!("{}[{}]", self.path, out.len()), dups: self.dups })? {
            out.push(v);
        }
        Ok(DocValue::Array(out))
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<DocValue, A::Error> {
        let mut out = Map::new();
        while let Some(k) = map.next_key::<String>()? {
            let path = if self.path.is_empty() { k.clone() } else { format!("{}.{k}", self.path) };
            if out.contains_key(&k) {
                self.dups.borrow_mut().push(path.clone());
            }
            let v = map.next_value_seed(Seed { path, dups: self.dups })?;
            out.insert(k, v);
        }
        Ok(DocValue::Object(out))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_like_json5_and_finds_every_repeat() {
        let src = r#"{ a: 1, b: { c: 0.5, c: 0.9, d: [ { e: 1, e: 2 } ] }, a: "x", n: null, f: -2.5e3, h: 0x10, t: true, g: -Infinity, }"#;
        let (v, dups) = parse(src).unwrap();
        assert_eq!(v, json5::from_str::<DocValue>(src).unwrap(), "the value is json5's own (last key wins)");
        assert_eq!(dups, ["b.c", "b.d[0].e", "a"]);
        assert_eq!(v["g"].as_f64(), Some(f64::NEG_INFINITY), "-Infinity stays a number");
        assert!(parse("{ a: 1, a: 1 }").unwrap().1 == ["a"]);
        assert!(parse("{ a: }").is_err());
    }
}
