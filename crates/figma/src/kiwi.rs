//! A generic decoder for the [kiwi](https://github.com/evanw/kiwi) binary format.
//!
//! Figma's clipboard embeds its kiwi schema next to the message, so nothing about Figma's
//! types is hard-coded here: the schema is decoded first and then drives decoding of the
//! message into dynamic [`Value`]s.
//!
//! Wire format (little endian):
//! - `uint`: LEB128, 7 bits per byte (at most 5 bytes); `int`: zig-zag over `uint`.
//! - `uint64`/`int64`: the same with up to 10 bytes.
//! - `float`: a single `0` byte for 0.0, else 4 bytes holding the f32 bits rotated so the
//!   exponent comes first (`bits = (b << 23) | (b >> 9)` restores them).
//! - `string`: UTF-8, NUL terminated. `byte[]`: length (`uint`) + raw bytes.
//! - arrays: length (`uint`) + elements. enums: `uint`. structs: every field in order.
//!   messages: `(field id: uint, value)*` terminated by id 0.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq)]
pub struct Error(pub String);

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

pub type Result<T, E = Error> = std::result::Result<T, E>;

fn err<T>(msg: impl Into<String>) -> Result<T> {
    Err(Error(msg.into()))
}

struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn byte(&mut self) -> Result<u8> {
        let b = *self
            .data
            .get(self.pos)
            .ok_or_else(|| Error(format!("unexpected end of data at {}", self.pos)))?;
        self.pos += 1;
        Ok(b)
    }

    fn bytes(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self.pos.checked_add(n).filter(|e| *e <= self.data.len());
        let Some(end) = end else {
            return err(format!("byte array of {n} overruns the data at {}", self.pos));
        };
        let s = &self.data[self.pos..end];
        self.pos = end;
        Ok(s)
    }

    fn var_uint(&mut self) -> Result<u32> {
        let mut v: u32 = 0;
        let mut shift = 0;
        loop {
            let b = self.byte()?;
            v |= ((b & 127) as u32) << shift;
            shift += 7;
            if b & 128 == 0 || shift >= 35 {
                return Ok(v);
            }
        }
    }

    fn var_int(&mut self) -> Result<i32> {
        let v = self.var_uint()?;
        Ok(((v >> 1) as i32) ^ -((v & 1) as i32))
    }

    fn var_uint64(&mut self) -> Result<u64> {
        let mut v: u64 = 0;
        let mut shift = 0;
        loop {
            let b = self.byte()?;
            v |= ((b & 127) as u64) << shift;
            shift += 7;
            if b & 128 == 0 || shift >= 70 {
                return Ok(v);
            }
        }
    }

    fn var_float(&mut self) -> Result<f32> {
        let first = self.byte()?;
        if first == 0 {
            return Ok(0.0);
        }
        let rest = self.bytes(3)?;
        let bits = u32::from_le_bytes([first, rest[0], rest[1], rest[2]]);
        Ok(f32::from_bits(bits.rotate_left(23)))
    }

    fn string(&mut self) -> Result<String> {
        let start = self.pos;
        let len = self.data[start..]
            .iter()
            .position(|b| *b == 0)
            .ok_or_else(|| Error("unterminated string".into()))?;
        self.pos = start + len + 1;
        Ok(String::from_utf8_lossy(&self.data[start..start + len]).into_owned())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DefKind {
    Enum,
    Struct,
    Message,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldType {
    Bool,
    Byte,
    Int,
    Uint,
    Float,
    String,
    Int64,
    Uint64,
    /// Index into [`Schema::defs`].
    Def(usize),
}

#[derive(Debug, Clone)]
pub struct Field {
    pub name: Arc<str>,
    pub ty: FieldType,
    pub array: bool,
    /// Field id (messages) or enum value.
    pub value: u32,
}

#[derive(Debug, Clone)]
pub struct Def {
    pub name: String,
    pub kind: DefKind,
    pub fields: Vec<Field>,
}

#[derive(Debug, Clone)]
pub struct Schema {
    pub defs: Vec<Def>,
}

impl Schema {
    /// Decodes a binary schema (kiwi's `encodeBinarySchema` output).
    pub fn decode(data: &[u8]) -> Result<Schema> {
        let mut r = Reader::new(data);
        let n = r.var_uint()? as usize;
        let mut defs = Vec::with_capacity(n.min(4096));
        for _ in 0..n {
            let name = r.string()?;
            let kind = match r.byte()? {
                0 => DefKind::Enum,
                1 => DefKind::Struct,
                2 => DefKind::Message,
                k => return err(format!("definition `{name}` has unknown kind {k}")),
            };
            let count = r.var_uint()? as usize;
            let mut fields = Vec::with_capacity(count.min(4096));
            for _ in 0..count {
                let fname = r.string()?;
                let t = r.var_int()?;
                let array = r.byte()? & 1 == 1;
                let value = r.var_uint()?;
                let ty = match t {
                    -1 => FieldType::Bool,
                    -2 => FieldType::Byte,
                    -3 => FieldType::Int,
                    -4 => FieldType::Uint,
                    -5 => FieldType::Float,
                    -6 => FieldType::String,
                    -7 => FieldType::Int64,
                    -8 => FieldType::Uint64,
                    t if t >= 0 => FieldType::Def(t as usize),
                    t => return err(format!("field `{fname}` has unknown type {t}")),
                };
                fields.push(Field { name: fname.into(), ty, array, value });
            }
            defs.push(Def { name, kind, fields });
        }
        for d in &defs {
            for f in &d.fields {
                if let FieldType::Def(i) = f.ty
                    && i >= defs.len()
                {
                    return err(format!("field `{}.{}` references missing type {i}", d.name, f.name));
                }
            }
        }
        Ok(Schema { defs })
    }

    pub fn def(&self, name: &str) -> Option<usize> {
        self.defs.iter().position(|d| d.name == name)
    }

    /// Decodes `data` as the definition named `root` (Figma: `Message`).
    pub fn decode_message(&self, root: &str, data: &[u8]) -> Result<Value> {
        let idx = self.def(root).ok_or_else(|| Error(format!("schema has no `{root}`")))?;
        let mut r = Reader::new(data);
        self.def_value(idx, &mut r, 0)
    }

    fn def_value(&self, idx: usize, r: &mut Reader, depth: usize) -> Result<Value> {
        if depth > 256 {
            return err("nesting too deep");
        }
        let d = &self.defs[idx];
        match d.kind {
            DefKind::Enum => {
                let v = r.var_uint()?;
                Ok(match d.fields.iter().find(|f| f.value == v) {
                    Some(f) => Value::Enum(f.name.clone()),
                    None => Value::Int(v as i64),
                })
            }
            DefKind::Struct => {
                let mut o = Obj::new();
                for f in &d.fields {
                    o.insert(f.name.clone(), self.field(f, r, depth)?);
                }
                Ok(Value::Obj(o))
            }
            DefKind::Message => {
                let mut o = Obj::new();
                loop {
                    let id = r.var_uint()?;
                    if id == 0 {
                        return Ok(Value::Obj(o));
                    }
                    let Some(f) = d.fields.iter().find(|f| f.value == id) else {
                        return err(format!("unknown field {id} in `{}` at byte {}", d.name, r.pos));
                    };
                    o.insert(f.name.clone(), self.field(f, r, depth)?);
                }
            }
        }
    }

    fn field(&self, f: &Field, r: &mut Reader, depth: usize) -> Result<Value> {
        if !f.array {
            return self.scalar(f.ty, r, depth);
        }
        let n = r.var_uint()? as usize;
        if f.ty == FieldType::Byte {
            return Ok(Value::Bytes(r.bytes(n)?.to_vec()));
        }
        // Every element takes at least one byte: a cheap guard against corrupt lengths.
        if n > r.data.len() - r.pos {
            return err(format!("array `{}` of {n} overruns the data", f.name));
        }
        let mut a = Vec::with_capacity(n);
        for _ in 0..n {
            a.push(self.scalar(f.ty, r, depth)?);
        }
        Ok(Value::Array(a))
    }

    fn scalar(&self, t: FieldType, r: &mut Reader, depth: usize) -> Result<Value> {
        Ok(match t {
            FieldType::Bool => Value::Bool(r.byte()? != 0),
            FieldType::Byte => Value::Int(r.byte()? as i64),
            FieldType::Int => Value::Int(r.var_int()? as i64),
            FieldType::Uint => Value::Int(r.var_uint()? as i64),
            FieldType::Float => Value::Float(r.var_float()? as f64),
            FieldType::String => Value::Str(r.string()?),
            FieldType::Int64 => {
                let v = r.var_uint64()?;
                Value::Int(((v >> 1) as i64) ^ -((v & 1) as i64))
            }
            FieldType::Uint64 => Value::Int(r.var_uint64()? as i64),
            FieldType::Def(i) => self.def_value(i, r, depth + 1)?,
        })
    }
}

pub type Obj = BTreeMap<Arc<str>, Value>;

/// A decoded kiwi value.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    Bytes(Vec<u8>),
    Enum(Arc<str>),
    Array(Vec<Value>),
    Obj(Obj),
}

static NULL_ARRAY: Vec<Value> = Vec::new();

impl Value {
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Obj(o) => o.get(key),
            _ => None,
        }
    }

    pub fn obj(&self) -> Option<&Obj> {
        match self {
            Value::Obj(o) => Some(o),
            _ => None,
        }
    }

    pub fn num(&self) -> Option<f64> {
        match self {
            Value::Int(i) => Some(*i as f64),
            Value::Float(f) => Some(*f),
            _ => None,
        }
    }

    pub fn str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            Value::Enum(s) => Some(s),
            _ => None,
        }
    }

    pub fn bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }

    /// The elements of an array (empty for anything else).
    pub fn items(&self) -> &[Value] {
        match self {
            Value::Array(a) => a,
            _ => &NULL_ARRAY,
        }
    }

    pub fn bytes(&self) -> Option<&[u8]> {
        match self {
            Value::Bytes(b) => Some(b),
            _ => None,
        }
    }

    /// `self.a.b.c` for a dotted path.
    pub fn at(&self, path: &str) -> Option<&Value> {
        path.split('.').try_fold(self, |v, k| v.get(k))
    }

    pub fn f(&self, path: &str) -> Option<f64> {
        self.at(path).and_then(Value::num)
    }

    pub fn s(&self, path: &str) -> Option<&str> {
        self.at(path).and_then(Value::str)
    }

    pub fn b(&self, path: &str) -> Option<bool> {
        self.at(path).and_then(Value::bool)
    }

    /// Debug view as JSON (bytes as their length).
    pub fn to_json(&self) -> serde_json::Value {
        use serde_json::Value as J;
        match self {
            Value::Bool(b) => J::Bool(*b),
            Value::Int(i) => J::from(*i),
            Value::Float(f) => serde_json::Number::from_f64(*f).map_or(J::Null, J::Number),
            Value::Str(s) => J::String(s.clone()),
            Value::Bytes(b) => J::String(format!("<{} bytes>", b.len())),
            Value::Enum(s) => J::String(s.to_string()),
            Value::Array(a) => J::Array(a.iter().map(Value::to_json).collect()),
            Value::Obj(o) => J::Object(o.iter().map(|(k, v)| (k.to_string(), v.to_json())).collect()),
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Minimal kiwi encoder for tests.
    #[derive(Default)]
    pub struct Writer(pub Vec<u8>);

    impl Writer {
        pub fn uint(&mut self, mut v: u32) -> &mut Self {
            loop {
                let b = (v & 127) as u8;
                v >>= 7;
                if v == 0 {
                    self.0.push(b);
                    return self;
                }
                self.0.push(b | 128);
            }
        }
        pub fn int(&mut self, v: i32) -> &mut Self {
            self.uint(((v << 1) ^ (v >> 31)) as u32)
        }
        pub fn float(&mut self, v: f32) -> &mut Self {
            if v == 0.0 {
                self.0.push(0);
                return self;
            }
            let bits = v.to_bits().rotate_right(23);
            self.0.extend_from_slice(&bits.to_le_bytes());
            self
        }
        pub fn str(&mut self, s: &str) -> &mut Self {
            self.0.extend_from_slice(s.as_bytes());
            self.0.push(0);
            self
        }
        pub fn byte(&mut self, b: u8) -> &mut Self {
            self.0.push(b);
            self
        }
    }

    #[test]
    fn primitives_round_trip() {
        let mut w = Writer::default();
        w.uint(0).uint(300).int(-5).int(7).float(1.5).float(0.0).float(-1234.25).str("héllo");
        let mut r = Reader::new(&w.0);
        assert_eq!(r.var_uint().unwrap(), 0);
        assert_eq!(r.var_uint().unwrap(), 300);
        assert_eq!(r.var_int().unwrap(), -5);
        assert_eq!(r.var_int().unwrap(), 7);
        assert_eq!(r.var_float().unwrap(), 1.5);
        assert_eq!(r.var_float().unwrap(), 0.0);
        assert_eq!(r.var_float().unwrap(), -1234.25);
        assert_eq!(r.string().unwrap(), "héllo");
        assert!(r.byte().is_err());
    }

    #[test]
    fn schema_drives_message_decoding() {
        // enum Kind { A = 0; B = 1; }  struct P { float x; float y; }
        // message M { string name = 1; Kind kind = 2; P[] pts = 3; byte[] raw = 4; }
        let mut s = Writer::default();
        s.uint(3);
        s.str("Kind").byte(0).uint(2);
        s.str("A").int(-4).byte(0).uint(0);
        s.str("B").int(-4).byte(0).uint(1);
        s.str("P").byte(1).uint(2);
        s.str("x").int(-5).byte(0).uint(1);
        s.str("y").int(-5).byte(0).uint(2);
        s.str("M").byte(2).uint(4);
        s.str("name").int(-6).byte(0).uint(1);
        s.str("kind").int(0).byte(0).uint(2);
        s.str("pts").int(1).byte(1).uint(3);
        s.str("raw").int(-2).byte(1).uint(4);
        let schema = Schema::decode(&s.0).unwrap();
        assert_eq!(schema.defs.len(), 3);

        let mut m = Writer::default();
        m.uint(1).str("hi");
        m.uint(2).uint(1);
        m.uint(3).uint(2).float(1.0).float(2.0).float(3.0).float(0.0);
        m.uint(4).uint(3).byte(9).byte(8).byte(7);
        m.uint(0);
        let v = schema.decode_message("M", &m.0).unwrap();
        assert_eq!(v.s("name"), Some("hi"));
        assert_eq!(v.s("kind"), Some("B"));
        assert_eq!(v.get("pts").unwrap().items().len(), 2);
        assert_eq!(v.get("pts").unwrap().items()[1].f("x"), Some(3.0));
        assert_eq!(v.get("raw").unwrap().bytes(), Some(&[9u8, 8, 7][..]));

        // Unknown field ids are an error, not silently skipped.
        let mut bad = Writer::default();
        bad.uint(9).uint(1).uint(0);
        assert!(schema.decode_message("M", &bad.0).is_err());
        // Truncated data is an error.
        assert!(schema.decode_message("M", &m.0[..5]).is_err());
    }
}
