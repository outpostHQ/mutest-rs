//! NOTE: The runtime writes its own JSON because the `PartialEq<Value>` impls of `serde_json` for the primitive types
//!       make comparisons like `assert_eq!(1_u64, "1".parse().unwrap())` ambiguous in every crate linked with it.

use std::fmt::{self, Display};
use std::io;
use std::mem;

use serde::de::value::{MapDeserializer, SeqDeserializer};
use serde::de::{self, DeserializeOwned, IntoDeserializer, Visitor};
use serde::ser::{self, Serialize};

#[derive(Debug)]
pub(crate) struct Error(String);

impl Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

impl ser::Error for Error {
    fn custom<T: Display>(msg: T) -> Self {
        Self(msg.to_string())
    }
}

impl de::Error for Error {
    fn custom<T: Display>(msg: T) -> Self {
        Self(msg.to_string())
    }
}

/// Writes the value as compact JSON, as `serde_json::to_vec` does.
pub(crate) fn to_vec<T: Serialize + ?Sized>(value: &T) -> io::Result<Vec<u8>> {
    let mut out = vec![];
    value.serialize(Writer { out: &mut out }).map_err(io::Error::other)?;
    Ok(out)
}

/// A JSON object, written one field at a time.
pub(crate) struct Object {
    out: Vec<u8>,
    error: Option<Error>,
}

impl Object {
    pub(crate) fn new() -> Self {
        Self { out: vec![b'{'], error: None }
    }

    pub(crate) fn field<T: Serialize + ?Sized>(mut self, key: &str, value: &T) -> Self {
        if self.error.is_some() { return self; }
        if self.out.len() > 1 { self.out.push(b','); }
        write_str(&mut self.out, key);
        self.out.push(b':');
        self.error = value.serialize(Writer { out: &mut self.out }).err();
        self
    }

    pub(crate) fn into_vec(mut self) -> io::Result<Vec<u8>> {
        if let Some(error) = self.error { return Err(io::Error::other(error)); }
        self.out.push(b'}');
        Ok(self.out)
    }
}

fn write_str(out: &mut Vec<u8>, s: &str) {
    out.push(b'"');
    for c in s.chars() {
        match c {
            '"' => out.extend_from_slice(b"\\\""),
            '\\' => out.extend_from_slice(b"\\\\"),
            '\n' => out.extend_from_slice(b"\\n"),
            '\r' => out.extend_from_slice(b"\\r"),
            '\t' => out.extend_from_slice(b"\\t"),
            c if c < ' ' => out.extend_from_slice(format!("\\u{:04x}", c as u32).as_bytes()),
            c => out.extend_from_slice(c.encode_utf8(&mut [0; 4]).as_bytes()),
        }
    }
    out.push(b'"');
}

fn write_display(out: &mut Vec<u8>, value: impl Display) -> Result<(), Error> {
    out.extend_from_slice(value.to_string().as_bytes());
    Ok(())
}

/// Finite floats in their shortest round-trip form, the others as `null`, as `serde_json` writes them.
fn write_float(out: &mut Vec<u8>, value: f64, shortest: impl fmt::Debug) -> Result<(), Error> {
    match value.is_finite() {
        true => write_display(out, format_args!("{shortest:?}")),
        false => write_display(out, "null"),
    }
}

struct Writer<'a> {
    out: &'a mut Vec<u8>,
}

impl<'a> Writer<'a> {
    fn compound(self, open: &[u8], end: &'static [u8]) -> Compound<'a> {
        self.out.extend_from_slice(open);
        Compound { out: self.out, first: true, end }
    }

    /// Opens the `{"variant":` wrapper of an externally tagged enum variant.
    fn variant(self, variant: &str) -> Self {
        self.out.push(b'{');
        write_str(self.out, variant);
        self.out.push(b':');
        self
    }
}

impl<'a> ser::Serializer for Writer<'a> {
    type Ok = ();
    type Error = Error;
    type SerializeSeq = Compound<'a>;
    type SerializeTuple = Compound<'a>;
    type SerializeTupleStruct = Compound<'a>;
    type SerializeTupleVariant = Compound<'a>;
    type SerializeMap = Compound<'a>;
    type SerializeStruct = Compound<'a>;
    type SerializeStructVariant = Compound<'a>;

    fn serialize_bool(self, v: bool) -> Result<(), Error> { write_display(self.out, v) }
    fn serialize_i8(self, v: i8) -> Result<(), Error> { write_display(self.out, v) }
    fn serialize_i16(self, v: i16) -> Result<(), Error> { write_display(self.out, v) }
    fn serialize_i32(self, v: i32) -> Result<(), Error> { write_display(self.out, v) }
    fn serialize_i64(self, v: i64) -> Result<(), Error> { write_display(self.out, v) }
    fn serialize_i128(self, v: i128) -> Result<(), Error> { write_display(self.out, v) }
    fn serialize_u8(self, v: u8) -> Result<(), Error> { write_display(self.out, v) }
    fn serialize_u16(self, v: u16) -> Result<(), Error> { write_display(self.out, v) }
    fn serialize_u32(self, v: u32) -> Result<(), Error> { write_display(self.out, v) }
    fn serialize_u64(self, v: u64) -> Result<(), Error> { write_display(self.out, v) }
    fn serialize_u128(self, v: u128) -> Result<(), Error> { write_display(self.out, v) }
    fn serialize_f32(self, v: f32) -> Result<(), Error> { write_float(self.out, v.into(), v) }
    fn serialize_f64(self, v: f64) -> Result<(), Error> { write_float(self.out, v, v) }
    fn serialize_char(self, v: char) -> Result<(), Error> { self.serialize_str(v.encode_utf8(&mut [0; 4])) }

    fn serialize_str(self, v: &str) -> Result<(), Error> {
        write_str(self.out, v);
        Ok(())
    }

    fn serialize_bytes(self, v: &[u8]) -> Result<(), Error> {
        let mut seq = self.compound(b"[", b"]");
        v.iter().try_for_each(|byte| seq.element(byte))?;
        seq.end()
    }

    fn serialize_none(self) -> Result<(), Error> { self.serialize_unit() }
    fn serialize_some<T: Serialize + ?Sized>(self, value: &T) -> Result<(), Error> { value.serialize(self) }
    fn serialize_unit(self) -> Result<(), Error> { write_display(self.out, "null") }
    fn serialize_unit_struct(self, _name: &'static str) -> Result<(), Error> { self.serialize_unit() }

    fn serialize_unit_variant(self, _name: &'static str, _index: u32, variant: &'static str) -> Result<(), Error> {
        self.serialize_str(variant)
    }

    fn serialize_newtype_struct<T: Serialize + ?Sized>(self, _name: &'static str, value: &T) -> Result<(), Error> {
        value.serialize(self)
    }

    fn serialize_newtype_variant<T: Serialize + ?Sized>(self, _name: &'static str, _index: u32, variant: &'static str, value: &T) -> Result<(), Error> {
        let writer = self.variant(variant);
        let out = &mut *writer.out;
        value.serialize(Writer { out: &mut *out })?;
        out.push(b'}');
        Ok(())
    }

    fn serialize_seq(self, _len: Option<usize>) -> Result<Compound<'a>, Error> { Ok(self.compound(b"[", b"]")) }
    fn serialize_tuple(self, _len: usize) -> Result<Compound<'a>, Error> { Ok(self.compound(b"[", b"]")) }
    fn serialize_tuple_struct(self, _name: &'static str, _len: usize) -> Result<Compound<'a>, Error> { Ok(self.compound(b"[", b"]")) }

    fn serialize_tuple_variant(self, _name: &'static str, _index: u32, variant: &'static str, _len: usize) -> Result<Compound<'a>, Error> {
        Ok(self.variant(variant).compound(b"[", b"]}"))
    }

    fn serialize_map(self, _len: Option<usize>) -> Result<Compound<'a>, Error> { Ok(self.compound(b"{", b"}")) }
    fn serialize_struct(self, _name: &'static str, _len: usize) -> Result<Compound<'a>, Error> { Ok(self.compound(b"{", b"}")) }

    fn serialize_struct_variant(self, _name: &'static str, _index: u32, variant: &'static str, _len: usize) -> Result<Compound<'a>, Error> {
        Ok(self.variant(variant).compound(b"{", b"}}"))
    }
}

/// An array or object being written, with the bytes that close it.
struct Compound<'a> {
    out: &'a mut Vec<u8>,
    first: bool,
    end: &'static [u8],
}

impl Compound<'_> {
    fn separate(&mut self) {
        if !mem::take(&mut self.first) { self.out.push(b','); }
    }

    fn element<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Error> {
        self.separate();
        value.serialize(Writer { out: self.out })
    }

    fn field<T: Serialize + ?Sized>(&mut self, key: &str, value: &T) -> Result<(), Error> {
        self.separate();
        write_str(self.out, key);
        self.out.push(b':');
        value.serialize(Writer { out: self.out })
    }

    fn end(self) -> Result<(), Error> {
        self.out.extend_from_slice(self.end);
        Ok(())
    }
}

macro_rules! impl_compound {
    ($($trait:ident :: $method:ident),+) => {
        $(
            impl ser::$trait for Compound<'_> {
                type Ok = ();
                type Error = Error;

                fn $method<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Error> { self.element(value) }
                fn end(self) -> Result<(), Error> { Compound::end(self) }
            }
        )+
    };
}

impl_compound!(SerializeSeq::serialize_element, SerializeTuple::serialize_element, SerializeTupleStruct::serialize_field, SerializeTupleVariant::serialize_field);

impl ser::SerializeMap for Compound<'_> {
    type Ok = ();
    type Error = Error;

    /// Like `serde_json`, writes integer keys as strings.
    fn serialize_key<T: Serialize + ?Sized>(&mut self, key: &T) -> Result<(), Error> {
        self.separate();
        let mut key_json = vec![];
        key.serialize(Writer { out: &mut key_json })?;
        match key_json.first() {
            Some(b'"') => self.out.extend_from_slice(&key_json),
            Some(b'-' | b'0'..=b'9') => self.out.extend_from_slice(&[b"\"", &key_json[..], b"\""].concat()),
            _ => return Err(Error("a map key must be a string or an integer".to_owned())),
        }
        self.out.push(b':');
        Ok(())
    }

    fn serialize_value<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Error> {
        value.serialize(Writer { out: self.out })
    }

    fn end(self) -> Result<(), Error> { Compound::end(self) }
}

impl ser::SerializeStruct for Compound<'_> {
    type Ok = ();
    type Error = Error;

    fn serialize_field<T: Serialize + ?Sized>(&mut self, key: &'static str, value: &T) -> Result<(), Error> { self.field(key, value) }
    fn end(self) -> Result<(), Error> { Compound::end(self) }
}

impl ser::SerializeStructVariant for Compound<'_> {
    type Ok = ();
    type Error = Error;

    fn serialize_field<T: Serialize + ?Sized>(&mut self, key: &'static str, value: &T) -> Result<(), Error> { self.field(key, value) }
    fn end(self) -> Result<(), Error> { Compound::end(self) }
}

/// A JSON value of the kinds that the runtime writes, where every number is an unsigned integer.
enum Value {
    Null,
    Bool(bool),
    Number(u64),
    String(String),
    Array(Vec<Value>),
    Object(Vec<(String, Value)>),
}

/// Reads the text, which must hold exactly one JSON value, into `T`.
pub(crate) fn from_str<T: DeserializeOwned>(text: &str) -> io::Result<T> {
    let mut parser = Parser { bytes: text.as_bytes(), pos: 0 };
    let value = parser.value().and_then(|value| parser.end().map(|()| value)).map_err(io::Error::other)?;
    T::deserialize(value).map_err(io::Error::other)
}

struct Parser<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl Parser<'_> {
    fn error(&self, expected: &str) -> Error {
        Error(format!("expected {expected} at byte {}", self.pos))
    }

    fn skip_whitespace(&mut self) {
        while let Some(b' ' | b'\t' | b'\n' | b'\r') = self.bytes.get(self.pos) { self.pos += 1; }
    }

    fn next(&mut self) -> Option<u8> {
        let byte = self.bytes.get(self.pos).copied();
        self.pos += 1;
        byte
    }

    fn keyword(&mut self, keyword: &str, value: Value) -> Result<Value, Error> {
        if !self.bytes[self.pos..].starts_with(keyword.as_bytes()) { return Err(self.error(keyword)); }
        self.pos += keyword.len();
        Ok(value)
    }

    fn end(&mut self) -> Result<(), Error> {
        self.skip_whitespace();
        match self.pos == self.bytes.len() {
            true => Ok(()),
            false => Err(self.error("the end of the text")),
        }
    }

    fn value(&mut self) -> Result<Value, Error> {
        self.skip_whitespace();
        match self.bytes.get(self.pos) {
            Some(b'n') => self.keyword("null", Value::Null),
            Some(b't') => self.keyword("true", Value::Bool(true)),
            Some(b'f') => self.keyword("false", Value::Bool(false)),
            Some(b'0'..=b'9') => self.number().map(Value::Number),
            Some(b'"') => self.string().map(Value::String),
            Some(b'[') => {
                let mut items = vec![];
                self.list(b']', |parser| { items.push(parser.value()?); Ok(()) })?;
                Ok(Value::Array(items))
            }
            Some(b'{') => {
                let mut fields = vec![];
                self.list(b'}', |parser| { fields.push(parser.field()?); Ok(()) })?;
                Ok(Value::Object(fields))
            }
            _ => Err(self.error("a value")),
        }
    }

    fn number(&mut self) -> Result<u64, Error> {
        let start = self.pos;
        while let Some(b'0'..=b'9') = self.bytes.get(self.pos) { self.pos += 1; }
        let digits = std::str::from_utf8(&self.bytes[start..self.pos]).map_err(|_| self.error("digits"))?;
        digits.parse().map_err(|_| self.error("a number that fits 64 bits"))
    }

    fn string(&mut self) -> Result<String, Error> {
        self.pos += 1;
        let mut string = String::new();
        loop {
            let start = self.pos;
            while !matches!(self.bytes.get(self.pos), Some(b'"' | b'\\') | None) { self.pos += 1; }
            string.push_str(std::str::from_utf8(&self.bytes[start..self.pos]).map_err(|_| self.error("UTF-8 text"))?);
            match self.next() {
                Some(b'"') => return Ok(string),
                Some(b'\\') => string.push(self.escape()?),
                _ => return Err(self.error("the end of the string")),
            }
        }
    }

    /// The escapes that `write_str` writes, and `\u` escapes of other characters outside the surrogate range.
    fn escape(&mut self) -> Result<char, Error> {
        match self.next() {
            Some(b'"') => Ok('"'),
            Some(b'\\') => Ok('\\'),
            Some(b'/') => Ok('/'),
            Some(b'n') => Ok('\n'),
            Some(b'r') => Ok('\r'),
            Some(b't') => Ok('\t'),
            Some(b'u') => {
                let hex = self.bytes.get(self.pos..self.pos + 4).and_then(|hex| std::str::from_utf8(hex).ok());
                self.pos += 4;
                hex.and_then(|hex| u32::from_str_radix(hex, 16).ok()).and_then(char::from_u32).ok_or_else(|| self.error("a character code"))
            }
            _ => Err(self.error("an escape")),
        }
    }

    fn field(&mut self) -> Result<(String, Value), Error> {
        self.skip_whitespace();
        if self.bytes.get(self.pos) != Some(&b'"') { return Err(self.error("a key")); }
        let key = self.string()?;
        self.skip_whitespace();
        if self.next() != Some(b':') { return Err(self.error("`:`")); }
        Ok((key, self.value()?))
    }

    /// Reads the items of an array or object, from its opening to its closing byte.
    fn list(&mut self, close: u8, mut item: impl FnMut(&mut Self) -> Result<(), Error>) -> Result<(), Error> {
        self.pos += 1;
        self.skip_whitespace();
        if self.bytes.get(self.pos) == Some(&close) {
            self.pos += 1;
            return Ok(());
        }
        loop {
            item(self)?;
            self.skip_whitespace();
            match self.next() {
                Some(b',') => {}
                Some(byte) if byte == close => return Ok(()),
                _ => return Err(self.error("`,` or the end of the list")),
            }
        }
    }
}

impl<'de> de::Deserializer<'de> for Value {
    type Error = Error;

    fn deserialize_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        match self {
            Value::Null => visitor.visit_unit(),
            Value::Bool(v) => visitor.visit_bool(v),
            Value::Number(v) => visitor.visit_u64(v),
            Value::String(v) => visitor.visit_string(v),
            Value::Array(items) => {
                let mut seq = SeqDeserializer::new(items.into_iter());
                let value = visitor.visit_seq(&mut seq)?;
                seq.end().map(|()| value)
            }
            Value::Object(fields) => {
                let mut map = MapDeserializer::new(fields.into_iter());
                let value = visitor.visit_map(&mut map)?;
                map.end().map(|()| value)
            }
        }
    }

    fn deserialize_option<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        match self {
            Value::Null => visitor.visit_none(),
            value => visitor.visit_some(value),
        }
    }

    serde::forward_to_deserialize_any! {
        bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string bytes byte_buf unit unit_struct
        newtype_struct seq tuple tuple_struct map struct enum identifier ignored_any
    }
}

impl IntoDeserializer<'_, Error> for Value {
    type Deserializer = Self;

    fn into_deserializer(self) -> Self {
        self
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::time::Duration;

    use serde::{Deserialize, Serialize};

    #[derive(Serialize)]
    #[serde(tag = "event", rename_all = "snake_case")]
    enum Event {
        Start { name: String, timeout: Option<Duration> },
        Score { score: Option<f64>, scores: Vec<f32> },
    }

    #[derive(Serialize)]
    enum Shape {
        Unit,
        Newtype(u8),
        Tuple(i32, bool),
        Struct { c: char },
    }

    #[test]
    fn writes_the_json_that_serde_json_writes() {
        let values = (
            [Event::Start { name: "a\"\\\n\u{1}é😀".to_owned(), timeout: Some(Duration::from_millis(1500)) }, Event::Score { score: None, scores: vec![0.5, -1.0, 1e-7, f32::NAN] }],
            [Shape::Unit, Shape::Newtype(7), Shape::Tuple(-3, true), Shape::Struct { c: 'x' }],
            BTreeMap::from([(2_u32, "b"), (10, "c")]),
            (u128::MAX, i64::MIN, (), Some(f64::INFINITY), b"\x00\xff".as_slice()),
        );
        assert_eq!(String::from_utf8(super::to_vec(&values).unwrap()).unwrap(), serde_json::to_string(&values).unwrap());
    }

    #[test]
    fn objects_are_written_one_field_at_a_time() {
        let object = super::Object::new().field("event", "phase").field("ids", &[1, 2]).field("timeout", &None::<u64>);
        assert_eq!(object.into_vec().unwrap(), br#"{"event":"phase","ids":[1,2],"timeout":null}"#);
        assert_eq!(super::Object::new().into_vec().unwrap(), b"{}");
    }

    #[derive(Deserialize, Debug, PartialEq)]
    #[serde(deny_unknown_fields)]
    struct Row {
        name: String,
        ids: Vec<u32>,
        pairs: Vec<(bool, Option<String>)>,
    }

    #[test]
    fn reads_back_what_it_writes_and_refuses_the_rest() {
        let row = super::from_str::<Row>(" {\"name\" : \"a\\\"\\\\\\n\\t\\u00e9/\\/\", \"ids\":[ 0 ,4294967295], \"pairs\":[[true,null],[false,\"x\"]]} ").unwrap();
        assert_eq!(row, Row { name: "a\"\\\n\té//".to_owned(), ids: vec![0, u32::MAX], pairs: vec![(true, None), (false, Some("x".to_owned()))] });
        for invalid in ["", "{", "[1,]", "[1 2]", "01x", "1.5", "-1", "\"\\x\"", "\"\\ud800\"", "\"a", "nul", "{\"a\" 1}", "{1:2}", "[] []", "18446744073709551616"] {
            assert!(super::from_str::<serde::de::IgnoredAny>(invalid).is_err(), "accepted {invalid}");
        }
    }
}
