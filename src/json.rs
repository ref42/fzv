//! A small JSON reader.
//!
//! The Zig download index is the only JSON fzv reads, but it needs exact
//! structural access: a version's tarball URL, its checksum and the master
//! snapshot version. Scanning the text for quoted substrings could do none of
//! those reliably, so this self-contained parser replaces it.

use std::collections::BTreeMap;

/// Guards against a pathological document exhausting the stack.
const MAX_DEPTH: usize = 64;

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    /// Kept as written so large integers and exotic forms survive untouched.
    Number(String),
    String(String),
    Array(Vec<Value>),
    Object(BTreeMap<String, Value>),
}

impl Value {
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.as_object()?.get(key)
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(text) => Some(text),
            _ => None,
        }
    }

    pub fn as_object(&self) -> Option<&BTreeMap<String, Value>> {
        match self {
            Value::Object(entries) => Some(entries),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&[Value]> {
        match self {
            Value::Array(items) => Some(items),
            _ => None,
        }
    }

    /// The keys of an object, in sorted order.
    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.as_object()
            .into_iter()
            .flat_map(|entries| entries.keys().map(String::as_str))
    }

    /// The string stored at `key`, or `None` when it is absent or not a string.
    pub fn string_at(&self, key: &str) -> Option<&str> {
        self.get(key).and_then(Value::as_str)
    }

    /// The number stored at `key`, written either way.
    ///
    /// The Zig index quotes its sizes (`"size": "97217739"`) while GitHub
    /// writes them bare, so both forms are read.
    pub fn u64_at(&self, key: &str) -> Option<u64> {
        match self.get(key)? {
            Value::Number(text) | Value::String(text) => text.parse().ok(),
            _ => None,
        }
    }
}

pub fn parse(text: &str) -> Result<Value, String> {
    let mut parser = Parser {
        chars: text.chars().collect(),
        at: 0,
        depth: 0,
    };
    parser.skip_whitespace();
    let value = parser.value()?;
    parser.skip_whitespace();
    if parser.at != parser.chars.len() {
        return Err(parser.error("trailing data after the top-level value"));
    }
    Ok(value)
}

struct Parser {
    chars: Vec<char>,
    at: usize,
    depth: usize,
}

impl Parser {
    fn error(&self, message: &str) -> String {
        format!("{message} (at character {})", self.at)
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.at).copied()
    }

    fn bump(&mut self) -> Option<char> {
        let character = self.peek()?;
        self.at += 1;
        Some(character)
    }

    fn skip_whitespace(&mut self) {
        while matches!(self.peek(), Some(' ' | '\t' | '\n' | '\r')) {
            self.at += 1;
        }
    }

    fn expect(&mut self, wanted: char) -> Result<(), String> {
        match self.bump() {
            Some(found) if found == wanted => Ok(()),
            Some(found) => Err(self.error(&format!("expected '{wanted}', found '{found}'"))),
            None => Err(self.error(&format!("expected '{wanted}', found end of input"))),
        }
    }

    fn literal(&mut self, word: &str, value: Value) -> Result<Value, String> {
        for wanted in word.chars() {
            if self.bump() != Some(wanted) {
                return Err(self.error(&format!("invalid literal, expected '{word}'")));
            }
        }
        Ok(value)
    }

    fn value(&mut self) -> Result<Value, String> {
        match self.peek() {
            Some('{') => self.object(),
            Some('[') => self.array(),
            Some('"') => Ok(Value::String(self.string()?)),
            Some('t') => self.literal("true", Value::Bool(true)),
            Some('f') => self.literal("false", Value::Bool(false)),
            Some('n') => self.literal("null", Value::Null),
            Some(character) if character == '-' || character.is_ascii_digit() => self.number(),
            Some(character) => Err(self.error(&format!("unexpected character '{character}'"))),
            None => Err(self.error("unexpected end of input")),
        }
    }

    fn enter(&mut self) -> Result<(), String> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err(self.error("document nests too deeply"));
        }
        Ok(())
    }

    fn object(&mut self) -> Result<Value, String> {
        self.enter()?;
        self.expect('{')?;
        let mut entries = BTreeMap::new();
        self.skip_whitespace();
        if self.peek() == Some('}') {
            self.at += 1;
            self.depth -= 1;
            return Ok(Value::Object(entries));
        }
        loop {
            self.skip_whitespace();
            let key = self.string()?;
            self.skip_whitespace();
            self.expect(':')?;
            self.skip_whitespace();
            let value = self.value()?;
            entries.insert(key, value);
            self.skip_whitespace();
            match self.bump() {
                Some(',') => continue,
                Some('}') => break,
                Some(found) => {
                    return Err(self.error(&format!("expected ',' or '}}', found '{found}'")));
                }
                None => return Err(self.error("unterminated object")),
            }
        }
        self.depth -= 1;
        Ok(Value::Object(entries))
    }

    fn array(&mut self) -> Result<Value, String> {
        self.enter()?;
        self.expect('[')?;
        let mut items = Vec::new();
        self.skip_whitespace();
        if self.peek() == Some(']') {
            self.at += 1;
            self.depth -= 1;
            return Ok(Value::Array(items));
        }
        loop {
            self.skip_whitespace();
            items.push(self.value()?);
            self.skip_whitespace();
            match self.bump() {
                Some(',') => continue,
                Some(']') => break,
                Some(found) => {
                    return Err(self.error(&format!("expected ',' or ']', found '{found}'")));
                }
                None => return Err(self.error("unterminated array")),
            }
        }
        self.depth -= 1;
        Ok(Value::Array(items))
    }

    fn string(&mut self) -> Result<String, String> {
        self.expect('"')?;
        let mut text = String::new();
        loop {
            match self.bump() {
                None => return Err(self.error("unterminated string")),
                Some('"') => return Ok(text),
                Some('\\') => match self.bump() {
                    Some('"') => text.push('"'),
                    Some('\\') => text.push('\\'),
                    Some('/') => text.push('/'),
                    Some('b') => text.push('\u{8}'),
                    Some('f') => text.push('\u{c}'),
                    Some('n') => text.push('\n'),
                    Some('r') => text.push('\r'),
                    Some('t') => text.push('\t'),
                    Some('u') => text.push(self.escape()?),
                    Some(found) => return Err(self.error(&format!("invalid escape '\\{found}'"))),
                    None => return Err(self.error("unterminated escape")),
                },
                Some(character) => text.push(character),
            }
        }
    }

    fn escape(&mut self) -> Result<char, String> {
        let first = self.hex4()?;
        // A high surrogate must be followed by its low surrogate.
        if (0xD800..0xDC00).contains(&first) {
            if self.bump() != Some('\\') || self.bump() != Some('u') {
                return Err(self.error("high surrogate without a low surrogate"));
            }
            let second = self.hex4()?;
            if !(0xDC00..0xE000).contains(&second) {
                return Err(self.error("invalid low surrogate"));
            }
            let combined = 0x10000 + ((first - 0xD800) << 10) + (second - 0xDC00);
            return char::from_u32(combined).ok_or_else(|| self.error("invalid surrogate pair"));
        }
        char::from_u32(first).ok_or_else(|| self.error("invalid unicode escape"))
    }

    fn hex4(&mut self) -> Result<u32, String> {
        let mut value = 0u32;
        for _ in 0..4 {
            let digit = self
                .bump()
                .and_then(|character| character.to_digit(16))
                .ok_or_else(|| self.error("invalid unicode escape"))?;
            value = value * 16 + digit;
        }
        Ok(value)
    }

    fn number(&mut self) -> Result<Value, String> {
        let start = self.at;
        if self.peek() == Some('-') {
            self.at += 1;
        }
        while matches!(self.peek(), Some(character) if character.is_ascii_digit() || matches!(character, '.' | 'e' | 'E' | '+' | '-'))
        {
            self.at += 1;
        }
        let text: String = self.chars[start..self.at].iter().collect();
        if text.is_empty() || text == "-" || text.parse::<f64>().is_err() {
            return Err(self.error(&format!("invalid number '{text}'")));
        }
        Ok(Value::Number(text))
    }
}

#[cfg(test)]
mod tests {
    use super::{Value, parse};
    use std::collections::BTreeMap;

    fn object(pairs: &[(&str, Value)]) -> Value {
        Value::Object(
            pairs
                .iter()
                .map(|(key, value)| ((*key).to_string(), value.clone()))
                .collect::<BTreeMap<_, _>>(),
        )
    }

    #[test]
    fn parses_nested_structures() {
        let text = r#"{"a": {"b": [1, 2, {"c": "x"}]}, "d": true, "e": null}"#;
        let value = parse(text).unwrap();
        assert_eq!(value.string_at("missing"), None);
        assert_eq!(value.get("d"), Some(&Value::Bool(true)));
        assert_eq!(value.get("e"), Some(&Value::Null));
        let array = value.get("a").unwrap().get("b").unwrap();
        assert_eq!(
            array,
            &Value::Array(vec![
                Value::Number("1".into()),
                Value::Number("2".into()),
                object(&[("c", Value::String("x".into()))]),
            ])
        );
    }

    #[test]
    fn reads_index_style_entries() {
        let text = r#"{"master": {"version": "0.17.0-dev.2228+955228b68"},
                       "0.13.0": {"x86_64-windows": {"tarball": "https://x/zig-windows-x86_64-0.13.0.zip", "shasum": "d85999", "size": 79163968}}}"#;
        let index = parse(text).unwrap();
        assert_eq!(index.string_at("master"), None);
        assert_eq!(
            index.get("master").unwrap().string_at("version"),
            Some("0.17.0-dev.2228+955228b68")
        );
        let entry = index.get("0.13.0").unwrap().get("x86_64-windows").unwrap();
        assert_eq!(entry.string_at("shasum"), Some("d85999"));
        assert_eq!(entry.string_at("size"), None);
        assert_eq!(entry.get("size"), Some(&Value::Number("79163968".into())));
        // Both spellings of a number are read the same way.
        assert_eq!(entry.u64_at("size"), Some(79163968));
        assert_eq!(entry.u64_at("shasum"), None);
        assert_eq!(index.u64_at("master"), None);
        let mut keys: Vec<_> = index.keys().collect();
        keys.sort_unstable();
        assert_eq!(keys, ["0.13.0", "master"]);
    }

    #[test]
    fn decodes_escapes_including_surrogate_pairs() {
        let value = parse(r#""a\u00e9b\ud83d\ude00c\"\/\\\n""#).unwrap();
        assert_eq!(value.as_str(), Some("a\u{e9}b\u{1f600}c\"/\\\n"));
    }

    #[test]
    fn rejects_malformed_documents() {
        assert!(parse("").is_err());
        assert!(parse("{").is_err());
        assert!(parse("{}extra").is_err());
        assert!(parse(r#"{"a": }"#).is_err());
        assert!(parse(r#""unterminated"#).is_err());
        assert!(parse(r#""\ud83d""#).is_err());
        assert!(parse("{\"a\": 1,}").is_err());
        let deep = format!("{}{}", "[".repeat(200), "]".repeat(200));
        assert!(parse(&deep).is_err());
    }

    #[test]
    fn keeps_large_integers_exact() {
        let value = parse(r#"{"size": 82229343}"#).unwrap();
        assert_eq!(value.string_at("size"), None);
        assert_eq!(
            value.get("size").unwrap(),
            &Value::Number("82229343".into())
        );
    }

    #[test]
    fn reads_sizes_in_both_spellings() {
        let value =
            parse(r#"{"quoted": "97217739", "bare": 97217739, "text": "unknown"}"#).unwrap();
        assert_eq!(value.u64_at("quoted"), Some(97217739));
        assert_eq!(value.u64_at("bare"), Some(97217739));
        assert_eq!(value.u64_at("text"), None);
        assert_eq!(value.u64_at("missing"), None);
    }
}
