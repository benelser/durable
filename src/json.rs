//! Minimal JSON parser and serializer — no dependencies.
//!
//! Provides a `Value` enum, a recursive-descent parser, and a serializer.
//! Enough for the runtime's wire protocol and storage needs.

use std::collections::BTreeMap;
use std::fmt;

/// A JSON value.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Array(Vec<Value>),
    Object(BTreeMap<String, Value>),
}

impl Value {
    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Value::Number(n) => Some(*n),
            _ => None,
        }
    }

    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Value::Number(n) => {
                let i = *n as i64;
                if (i as f64 - *n).abs() < f64::EPSILON {
                    Some(i)
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    pub fn as_u64(&self) -> Option<u64> {
        self.as_i64().and_then(|i| u64::try_from(i).ok())
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&[Value]> {
        match self {
            Value::Array(a) => Some(a),
            _ => None,
        }
    }

    pub fn as_array_mut(&mut self) -> Option<&mut Vec<Value>> {
        match self {
            Value::Array(a) => Some(a),
            _ => None,
        }
    }

    pub fn as_object(&self) -> Option<&BTreeMap<String, Value>> {
        match self {
            Value::Object(o) => Some(o),
            _ => None,
        }
    }

    pub fn as_object_mut(&mut self) -> Option<&mut BTreeMap<String, Value>> {
        match self {
            Value::Object(o) => Some(o),
            _ => None,
        }
    }

    /// Get a value by key (for objects) or index (for arrays).
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Object(map) => map.get(key),
            _ => None,
        }
    }

    pub fn get_idx(&self, idx: usize) -> Option<&Value> {
        match self {
            Value::Array(arr) => arr.get(idx),
            _ => None,
        }
    }
}

// -- json! macro for ergonomic JSON construction --

/// Construct a JSON `Value` using JSON-like syntax.
///
/// # Examples
/// ```
/// use delite_core::json;
/// let v = json!({
///     "name": "Alice",
///     "age": 30,
///     "active": true,
///     "tags": ["admin", "user"],
///     "address": null
/// });
/// ```
#[macro_export]
macro_rules! json {
    // null
    (null) => { $crate::json::Value::Null };
    // bool
    (true) => { $crate::json::Value::Bool(true) };
    (false) => { $crate::json::Value::Bool(false) };
    // array
    ([ $($elem:tt),* $(,)? ]) => {
        $crate::json::Value::Array(vec![ $( json!($elem) ),* ])
    };
    // object
    ({ $($key:tt : $val:tt),* $(,)? }) => {{
        let mut map = ::std::collections::BTreeMap::new();
        $( map.insert($key.to_string(), json!($val)); )*
        $crate::json::Value::Object(map)
    }};
    // expression fallback (numbers, variables, function calls)
    ($e:expr) => {
        $crate::json::IntoValue::into_value($e)
    };
}

/// Trait for converting Rust values into `Value` inside the `json!` macro.
pub trait IntoValue {
    fn into_value(self) -> Value;
}

impl IntoValue for Value {
    fn into_value(self) -> Value {
        self
    }
}

impl IntoValue for &str {
    fn into_value(self) -> Value {
        Value::String(self.to_string())
    }
}

impl IntoValue for String {
    fn into_value(self) -> Value {
        Value::String(self)
    }
}

impl IntoValue for &String {
    fn into_value(self) -> Value {
        Value::String(self.clone())
    }
}

impl IntoValue for bool {
    fn into_value(self) -> Value {
        Value::Bool(self)
    }
}

impl IntoValue for f64 {
    fn into_value(self) -> Value {
        Value::Number(self)
    }
}

impl IntoValue for i32 {
    fn into_value(self) -> Value {
        Value::Number(self as f64)
    }
}

impl IntoValue for i64 {
    fn into_value(self) -> Value {
        Value::Number(self as f64)
    }
}

impl IntoValue for u32 {
    fn into_value(self) -> Value {
        Value::Number(self as f64)
    }
}

impl IntoValue for u64 {
    fn into_value(self) -> Value {
        Value::Number(self as f64)
    }
}

impl IntoValue for usize {
    fn into_value(self) -> Value {
        Value::Number(self as f64)
    }
}

// -- Typed accessors for ergonomic field extraction --

impl Value {
    /// Get a string field by key, returning an error if missing or wrong type.
    pub fn string(&self, key: &str) -> Result<&str, crate::core::error::DurableError> {
        self.get(key)
            .and_then(|v| v.as_str())
            .ok_or_else(|| crate::core::error::DurableError::Serialization(
                format!("expected string field '{}'", key),
            ))
    }

    /// Get a number field by key, returning an error if missing or wrong type.
    pub fn number(&self, key: &str) -> Result<f64, crate::core::error::DurableError> {
        self.get(key)
            .and_then(|v| v.as_f64())
            .ok_or_else(|| crate::core::error::DurableError::Serialization(
                format!("expected number field '{}'", key),
            ))
    }

    /// Get a boolean field by key, returning an error if missing or wrong type.
    pub fn boolean(&self, key: &str) -> Result<bool, crate::core::error::DurableError> {
        self.get(key)
            .and_then(|v| v.as_bool())
            .ok_or_else(|| crate::core::error::DurableError::Serialization(
                format!("expected boolean field '{}'", key),
            ))
    }

    /// Get an integer field by key, returning an error if missing or wrong type.
    pub fn integer(&self, key: &str) -> Result<i64, crate::core::error::DurableError> {
        self.get(key)
            .and_then(|v| v.as_i64())
            .ok_or_else(|| crate::core::error::DurableError::Serialization(
                format!("expected integer field '{}'", key),
            ))
    }
}

// -- Convenience constructors --

pub fn json_null() -> Value {
    Value::Null
}

pub fn json_bool(b: bool) -> Value {
    Value::Bool(b)
}

pub fn json_num(n: f64) -> Value {
    Value::Number(n)
}

pub fn json_str(s: &str) -> Value {
    Value::String(s.to_string())
}

pub fn json_string(s: String) -> Value {
    Value::String(s)
}

pub fn json_array(v: Vec<Value>) -> Value {
    Value::Array(v)
}

pub fn json_object(entries: Vec<(&str, Value)>) -> Value {
    let mut map = BTreeMap::new();
    for (k, v) in entries {
        map.insert(k.to_string(), v);
    }
    Value::Object(map)
}

// -- Serialization --

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        serialize(self, f)
    }
}

fn serialize(val: &Value, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match val {
        Value::Null => write!(f, "null"),
        Value::Bool(b) => write!(f, "{}", if *b { "true" } else { "false" }),
        Value::Number(n) => {
            if n.fract() == 0.0 && n.abs() < (i64::MAX as f64) {
                write!(f, "{}", *n as i64)
            } else {
                write!(f, "{}", n)
            }
        }
        Value::String(s) => write_json_string(s, f),
        Value::Array(arr) => {
            write!(f, "[")?;
            for (i, v) in arr.iter().enumerate() {
                if i > 0 {
                    write!(f, ",")?;
                }
                serialize(v, f)?;
            }
            write!(f, "]")
        }
        Value::Object(map) => {
            write!(f, "{{")?;
            for (i, (k, v)) in map.iter().enumerate() {
                if i > 0 {
                    write!(f, ",")?;
                }
                write_json_string(k, f)?;
                write!(f, ":")?;
                serialize(v, f)?;
            }
            write!(f, "}}")
        }
    }
}

fn write_json_string(s: &str, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "\"")?;
    for ch in s.chars() {
        match ch {
            '"' => write!(f, "\\\"")?,
            '\\' => write!(f, "\\\\")?,
            '\n' => write!(f, "\\n")?,
            '\r' => write!(f, "\\r")?,
            '\t' => write!(f, "\\t")?,
            c if (c as u32) < 0x20 => write!(f, "\\u{:04x}", c as u32)?,
            c => write!(f, "{}", c)?,
        }
    }
    write!(f, "\"")
}

/// Serialize a Value to a String.
pub fn to_string(val: &Value) -> String {
    format!("{}", val)
}

/// Serialize a Value to a pretty-printed String.
pub fn to_string_pretty(val: &Value) -> String {
    let mut out = String::new();
    pretty_write(val, &mut out, 0);
    out
}

fn pretty_write(val: &Value, out: &mut String, indent: usize) {
    match val {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => {
            if n.fract() == 0.0 && n.abs() < (i64::MAX as f64) {
                out.push_str(&format!("{}", *n as i64));
            } else {
                out.push_str(&format!("{}", n));
            }
        }
        Value::String(s) => {
            out.push('"');
            for ch in s.chars() {
                match ch {
                    '"' => out.push_str("\\\""),
                    '\\' => out.push_str("\\\\"),
                    '\n' => out.push_str("\\n"),
                    '\r' => out.push_str("\\r"),
                    '\t' => out.push_str("\\t"),
                    c if (c as u32) < 0x20 => {
                        out.push_str(&format!("\\u{:04x}", c as u32));
                    }
                    c => out.push(c),
                }
            }
            out.push('"');
        }
        Value::Array(arr) => {
            if arr.is_empty() {
                out.push_str("[]");
                return;
            }
            out.push_str("[\n");
            for (i, v) in arr.iter().enumerate() {
                push_indent(out, indent + 1);
                pretty_write(v, out, indent + 1);
                if i + 1 < arr.len() {
                    out.push(',');
                }
                out.push('\n');
            }
            push_indent(out, indent);
            out.push(']');
        }
        Value::Object(map) => {
            if map.is_empty() {
                out.push_str("{}");
                return;
            }
            out.push_str("{\n");
            let len = map.len();
            for (i, (k, v)) in map.iter().enumerate() {
                push_indent(out, indent + 1);
                out.push('"');
                out.push_str(k);
                out.push_str("\": ");
                pretty_write(v, out, indent + 1);
                if i + 1 < len {
                    out.push(',');
                }
                out.push('\n');
            }
            push_indent(out, indent);
            out.push('}');
        }
    }
}

fn push_indent(out: &mut String, level: usize) {
    for _ in 0..level {
        out.push_str("  ");
    }
}

// -- Parser --

/// Parse a JSON string into a Value.
pub fn parse(input: &str) -> Result<Value, ParseError> {
    let mut parser = Parser::new(input);
    let val = parser.parse_value()?;
    parser.skip_whitespace();
    if parser.pos < parser.input.len() {
        return Err(ParseError::TrailingData(parser.pos));
    }
    Ok(val)
}

#[derive(Debug, Clone)]
pub enum ParseError {
    UnexpectedEof,
    UnexpectedChar(usize, char),
    InvalidNumber(usize),
    InvalidEscape(usize),
    InvalidUnicode(usize),
    ExpectedColon(usize),
    ExpectedCommaOrEnd(usize),
    TrailingData(usize),
    DepthExceeded(usize),
    StringTooLong(usize),
    TooManyElements(usize),
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParseError::UnexpectedEof => write!(f, "unexpected end of input"),
            ParseError::UnexpectedChar(pos, ch) => {
                write!(f, "unexpected character '{}' at position {}", ch, pos)
            }
            ParseError::InvalidNumber(pos) => write!(f, "invalid number at position {}", pos),
            ParseError::InvalidEscape(pos) => write!(f, "invalid escape at position {}", pos),
            ParseError::InvalidUnicode(pos) => {
                write!(f, "invalid unicode escape at position {}", pos)
            }
            ParseError::ExpectedColon(pos) => write!(f, "expected ':' at position {}", pos),
            ParseError::ExpectedCommaOrEnd(pos) => {
                write!(f, "expected ',' or closing bracket at position {}", pos)
            }
            ParseError::TrailingData(pos) => {
                write!(f, "trailing data at position {}", pos)
            }
            ParseError::DepthExceeded(depth) => {
                write!(f, "nesting depth {} exceeds maximum", depth)
            }
            ParseError::StringTooLong(len) => {
                write!(f, "string length {} exceeds maximum", len)
            }
            ParseError::TooManyElements(count) => {
                write!(f, "element count {} exceeds maximum", count)
            }
        }
    }
}

impl std::error::Error for ParseError {}

/// Safety limits for JSON parsing.
const MAX_DEPTH: usize = 128;
const MAX_STRING_LEN: usize = 10 * 1024 * 1024; // 10 MB
const MAX_ELEMENTS: usize = 100_000;

struct Parser<'a> {
    input: &'a [u8],
    pos: usize,
    depth: usize,
}

impl<'a> Parser<'a> {
    fn new(input: &'a str) -> Self {
        Self {
            input: input.as_bytes(),
            pos: 0,
            depth: 0,
        }
    }

    fn peek(&self) -> Option<u8> {
        self.input.get(self.pos).copied()
    }

    fn advance(&mut self) -> Option<u8> {
        let b = self.input.get(self.pos).copied()?;
        self.pos += 1;
        Some(b)
    }

    fn skip_whitespace(&mut self) {
        while let Some(b) = self.peek() {
            if b == b' ' || b == b'\t' || b == b'\n' || b == b'\r' {
                self.pos += 1;
            } else {
                break;
            }
        }
    }

    fn expect(&mut self, expected: u8) -> Result<(), ParseError> {
        match self.advance() {
            Some(b) if b == expected => Ok(()),
            Some(b) => Err(ParseError::UnexpectedChar(self.pos - 1, b as char)),
            None => Err(ParseError::UnexpectedEof),
        }
    }

    fn parse_value(&mut self) -> Result<Value, ParseError> {
        self.skip_whitespace();
        match self.peek() {
            None => Err(ParseError::UnexpectedEof),
            Some(b'"') => self.parse_string().map(Value::String),
            Some(b'{') => {
                if self.depth >= MAX_DEPTH {
                    return Err(ParseError::DepthExceeded(self.depth));
                }
                self.depth += 1;
                let r = self.parse_object();
                self.depth -= 1;
                r
            }
            Some(b'[') => {
                if self.depth >= MAX_DEPTH {
                    return Err(ParseError::DepthExceeded(self.depth));
                }
                self.depth += 1;
                let r = self.parse_array();
                self.depth -= 1;
                r
            }
            Some(b't') => self.parse_literal(b"true", Value::Bool(true)),
            Some(b'f') => self.parse_literal(b"false", Value::Bool(false)),
            Some(b'n') => self.parse_literal(b"null", Value::Null),
            Some(b) if b == b'-' || b.is_ascii_digit() => self.parse_number(),
            Some(b) => Err(ParseError::UnexpectedChar(self.pos, b as char)),
        }
    }

    fn parse_literal(&mut self, expected: &[u8], val: Value) -> Result<Value, ParseError> {
        for &b in expected {
            self.expect(b)?;
        }
        Ok(val)
    }

    fn parse_number(&mut self) -> Result<Value, ParseError> {
        let start = self.pos;
        if self.peek() == Some(b'-') {
            self.pos += 1;
        }
        // Integer part
        match self.peek() {
            Some(b'0') => {
                self.pos += 1;
            }
            Some(b) if b.is_ascii_digit() => {
                while let Some(b) = self.peek() {
                    if b.is_ascii_digit() {
                        self.pos += 1;
                    } else {
                        break;
                    }
                }
            }
            _ => return Err(ParseError::InvalidNumber(start)),
        }
        // Fractional part
        if self.peek() == Some(b'.') {
            self.pos += 1;
            let frac_start = self.pos;
            while let Some(b) = self.peek() {
                if b.is_ascii_digit() {
                    self.pos += 1;
                } else {
                    break;
                }
            }
            if self.pos == frac_start {
                return Err(ParseError::InvalidNumber(start));
            }
        }
        // Exponent
        if matches!(self.peek(), Some(b'e') | Some(b'E')) {
            self.pos += 1;
            if matches!(self.peek(), Some(b'+') | Some(b'-')) {
                self.pos += 1;
            }
            let exp_start = self.pos;
            while let Some(b) = self.peek() {
                if b.is_ascii_digit() {
                    self.pos += 1;
                } else {
                    break;
                }
            }
            if self.pos == exp_start {
                return Err(ParseError::InvalidNumber(start));
            }
        }
        let s = std::str::from_utf8(&self.input[start..self.pos]).unwrap();
        let n: f64 = s.parse().map_err(|_| ParseError::InvalidNumber(start))?;
        Ok(Value::Number(n))
    }

    fn parse_string(&mut self) -> Result<String, ParseError> {
        self.expect(b'"')?;
        let mut s = String::new();
        loop {
            if s.len() > MAX_STRING_LEN {
                return Err(ParseError::StringTooLong(s.len()));
            }
            match self.advance() {
                None => return Err(ParseError::UnexpectedEof),
                Some(b'"') => return Ok(s),
                Some(b'\\') => {
                    let esc = self.advance().ok_or(ParseError::UnexpectedEof)?;
                    match esc {
                        b'"' => s.push('"'),
                        b'\\' => s.push('\\'),
                        b'/' => s.push('/'),
                        b'n' => s.push('\n'),
                        b'r' => s.push('\r'),
                        b't' => s.push('\t'),
                        b'b' => s.push('\u{0008}'),
                        b'f' => s.push('\u{000C}'),
                        b'u' => {
                            let cp = self.parse_hex4()?;
                            // Handle surrogate pairs
                            if (0xD800..=0xDBFF).contains(&cp) {
                                self.expect(b'\\')?;
                                self.expect(b'u')?;
                                let lo = self.parse_hex4()?;
                                if !(0xDC00..=0xDFFF).contains(&lo) {
                                    return Err(ParseError::InvalidUnicode(self.pos));
                                }
                                let combined =
                                    0x10000 + ((cp as u32 - 0xD800) << 10) + (lo as u32 - 0xDC00);
                                s.push(
                                    char::from_u32(combined)
                                        .ok_or(ParseError::InvalidUnicode(self.pos))?,
                                );
                            } else {
                                s.push(
                                    char::from_u32(cp as u32)
                                        .ok_or(ParseError::InvalidUnicode(self.pos))?,
                                );
                            }
                        }
                        _ => return Err(ParseError::InvalidEscape(self.pos - 1)),
                    }
                }
                Some(b) => s.push(b as char),
            }
        }
    }

    fn parse_hex4(&mut self) -> Result<u16, ParseError> {
        let mut val: u16 = 0;
        for _ in 0..4 {
            let b = self.advance().ok_or(ParseError::UnexpectedEof)?;
            let digit = match b {
                b'0'..=b'9' => b - b'0',
                b'a'..=b'f' => 10 + b - b'a',
                b'A'..=b'F' => 10 + b - b'A',
                _ => return Err(ParseError::InvalidUnicode(self.pos - 1)),
            } as u16;
            val = (val << 4) | digit;
        }
        Ok(val)
    }

    fn parse_array(&mut self) -> Result<Value, ParseError> {
        self.expect(b'[')?;
        self.skip_whitespace();
        let mut arr = Vec::new();
        if self.peek() == Some(b']') {
            self.pos += 1;
            return Ok(Value::Array(arr));
        }
        loop {
            if arr.len() >= MAX_ELEMENTS {
                return Err(ParseError::TooManyElements(arr.len()));
            }
            arr.push(self.parse_value()?);
            self.skip_whitespace();
            match self.peek() {
                Some(b',') => {
                    self.pos += 1;
                }
                Some(b']') => {
                    self.pos += 1;
                    return Ok(Value::Array(arr));
                }
                _ => return Err(ParseError::ExpectedCommaOrEnd(self.pos)),
            }
        }
    }

    fn parse_object(&mut self) -> Result<Value, ParseError> {
        self.expect(b'{')?;
        self.skip_whitespace();
        let mut map = BTreeMap::new();
        if self.peek() == Some(b'}') {
            self.pos += 1;
            return Ok(Value::Object(map));
        }
        loop {
            if map.len() >= MAX_ELEMENTS {
                return Err(ParseError::TooManyElements(map.len()));
            }
            self.skip_whitespace();
            let key = self.parse_string()?;
            self.skip_whitespace();
            self.expect(b':')?;
            let val = self.parse_value()?;
            map.insert(key, val);
            self.skip_whitespace();
            match self.peek() {
                Some(b',') => {
                    self.pos += 1;
                }
                Some(b'}') => {
                    self.pos += 1;
                    return Ok(Value::Object(map));
                }
                _ => return Err(ParseError::ExpectedCommaOrEnd(self.pos)),
            }
        }
    }
}

// -- ToJson / FromJson traits --

/// Convert a Rust type to a JSON Value.
pub trait ToJson {
    fn to_json(&self) -> Value;
}

/// Construct a Rust type from a JSON Value.
pub trait FromJson: Sized {
    fn from_json(val: &Value) -> Result<Self, String>;
}

impl ToJson for Value {
    fn to_json(&self) -> Value {
        self.clone()
    }
}

impl FromJson for Value {
    fn from_json(val: &Value) -> Result<Self, String> {
        Ok(val.clone())
    }
}

impl ToJson for String {
    fn to_json(&self) -> Value {
        Value::String(self.clone())
    }
}

impl FromJson for String {
    fn from_json(val: &Value) -> Result<Self, String> {
        val.as_str()
            .map(|s| s.to_string())
            .ok_or_else(|| "expected string".to_string())
    }
}

impl ToJson for &str {
    fn to_json(&self) -> Value {
        Value::String(self.to_string())
    }
}

impl ToJson for bool {
    fn to_json(&self) -> Value {
        Value::Bool(*self)
    }
}

impl FromJson for bool {
    fn from_json(val: &Value) -> Result<Self, String> {
        val.as_bool().ok_or_else(|| "expected bool".to_string())
    }
}

impl ToJson for f64 {
    fn to_json(&self) -> Value {
        Value::Number(*self)
    }
}

impl FromJson for f64 {
    fn from_json(val: &Value) -> Result<Self, String> {
        val.as_f64().ok_or_else(|| "expected number".to_string())
    }
}

impl ToJson for i64 {
    fn to_json(&self) -> Value {
        Value::Number(*self as f64)
    }
}

impl FromJson for i64 {
    fn from_json(val: &Value) -> Result<Self, String> {
        val.as_i64().ok_or_else(|| "expected integer".to_string())
    }
}

impl ToJson for u64 {
    fn to_json(&self) -> Value {
        Value::Number(*self as f64)
    }
}

impl FromJson for u64 {
    fn from_json(val: &Value) -> Result<Self, String> {
        val.as_u64()
            .ok_or_else(|| "expected unsigned integer".to_string())
    }
}

impl ToJson for usize {
    fn to_json(&self) -> Value {
        Value::Number(*self as f64)
    }
}

impl FromJson for usize {
    fn from_json(val: &Value) -> Result<Self, String> {
        val.as_u64()
            .map(|n| n as usize)
            .ok_or_else(|| "expected unsigned integer".to_string())
    }
}

impl<T: ToJson> ToJson for Vec<T> {
    fn to_json(&self) -> Value {
        Value::Array(self.iter().map(|v| v.to_json()).collect())
    }
}

impl<T: FromJson> FromJson for Vec<T> {
    fn from_json(val: &Value) -> Result<Self, String> {
        val.as_array()
            .ok_or_else(|| "expected array".to_string())?
            .iter()
            .map(T::from_json)
            .collect()
    }
}

impl<T: ToJson> ToJson for Option<T> {
    fn to_json(&self) -> Value {
        match self {
            Some(v) => v.to_json(),
            None => Value::Null,
        }
    }
}

impl<T: FromJson> FromJson for Option<T> {
    fn from_json(val: &Value) -> Result<Self, String> {
        if val.is_null() {
            Ok(None)
        } else {
            T::from_json(val).map(Some)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_primitives() {
        assert_eq!(parse("null").unwrap(), Value::Null);
        assert_eq!(parse("true").unwrap(), Value::Bool(true));
        assert_eq!(parse("false").unwrap(), Value::Bool(false));
        assert_eq!(parse("42").unwrap(), Value::Number(42.0));
        assert_eq!(parse("-3.14").unwrap(), Value::Number(-3.14));
        assert_eq!(
            parse("\"hello\"").unwrap(),
            Value::String("hello".to_string())
        );
    }

    #[test]
    fn test_parse_array() {
        let v = parse("[1, 2, 3]").unwrap();
        assert_eq!(
            v,
            Value::Array(vec![
                Value::Number(1.0),
                Value::Number(2.0),
                Value::Number(3.0)
            ])
        );
    }

    #[test]
    fn test_parse_object() {
        let v = parse("{\"a\": 1, \"b\": \"two\"}").unwrap();
        let obj = v.as_object().unwrap();
        assert_eq!(obj.get("a"), Some(&Value::Number(1.0)));
        assert_eq!(obj.get("b"), Some(&Value::String("two".to_string())));
    }

    #[test]
    fn test_roundtrip() {
        let input = "{\"arr\":[1,null,true],\"nested\":{\"x\":\"y\"}}";
        let parsed = parse(input).unwrap();
        let serialized = to_string(&parsed);
        let reparsed = parse(&serialized).unwrap();
        assert_eq!(parsed, reparsed);
    }

    #[test]
    fn test_escape_roundtrip() {
        let input = r#""hello\nworld\t\"quoted\"""#;
        let parsed = parse(input).unwrap();
        assert_eq!(parsed, Value::String("hello\nworld\t\"quoted\"".to_string()));
        let serialized = to_string(&parsed);
        let reparsed = parse(&serialized).unwrap();
        assert_eq!(parsed, reparsed);
    }

    #[test]
    fn test_json_macro_primitives() {
        assert_eq!(json!(null), Value::Null);
        assert_eq!(json!(true), Value::Bool(true));
        assert_eq!(json!(false), Value::Bool(false));
        assert_eq!(json!(42), Value::Number(42.0));
        assert_eq!(json!(3.14), Value::Number(3.14));
        assert_eq!(json!("hello"), Value::String("hello".to_string()));
    }

    #[test]
    fn test_json_macro_array() {
        let v = json!([1, 2, "three", null, true]);
        assert_eq!(
            v,
            Value::Array(vec![
                Value::Number(1.0),
                Value::Number(2.0),
                Value::String("three".to_string()),
                Value::Null,
                Value::Bool(true),
            ])
        );
    }

    #[test]
    fn test_json_macro_object() {
        let v = json!({
            "name": "Alice",
            "age": 30,
            "active": true,
            "scores": [95, 87],
            "address": null
        });
        assert_eq!(v.string("name").unwrap(), "Alice");
        assert_eq!(v.number("age").unwrap(), 30.0);
        assert_eq!(v.boolean("active").unwrap(), true);
        assert!(v.get("scores").unwrap().as_array().is_some());
        assert!(v.get("address").unwrap().is_null());
    }

    #[test]
    fn test_json_macro_nested() {
        let v = json!({
            "user": {
                "name": "Bob",
                "tags": ["admin", "user"]
            }
        });
        let user = v.get("user").unwrap();
        assert_eq!(user.string("name").unwrap(), "Bob");
    }

    #[test]
    fn test_json_macro_with_variables() {
        let name = "Charlie";
        let age = 25_i32;
        let v = json!({
            "name": name,
            "age": age
        });
        assert_eq!(v.string("name").unwrap(), "Charlie");
        assert_eq!(v.number("age").unwrap(), 25.0);
    }

    #[test]
    fn test_value_typed_accessors() {
        let v = json!({
            "name": "Alice",
            "age": 30,
            "active": true
        });
        assert_eq!(v.string("name").unwrap(), "Alice");
        assert_eq!(v.number("age").unwrap(), 30.0);
        assert_eq!(v.boolean("active").unwrap(), true);
        assert_eq!(v.integer("age").unwrap(), 30);

        // Missing fields return errors
        assert!(v.string("missing").is_err());
        assert!(v.number("name").is_err());
    }
}
