//! JSON 序列化与**宽松解析**。
//!
//! # 为什么不用 serde
//!
//! C# 版持久化格式是 `SimpleJsonString` 产出的**裸键**对象（`{ Id:1, Name:"x", Age:30 }`），
//! 标准 JSON 解析器无法读取。本模块提供一个零依赖的递归下降解析器：
//! - 同时接受裸键与带引号键、`//` 行注释、尾逗号、空数组元素（对齐 C# `JsonParser` 行为）；
//! - 写入端仍输出 **C# 兼容格式**（裸键 + `\r\n` + `\t` 缩进），保证 C# 版能直接读取 Rust 写出的文件；
//! - 复杂值（列表/映射）按 `JsonString` 口径输出。
//!
//! 解析结果先成为 [`JsonValue`] 树，再按 [`FieldKind`] 强制转换为 [`FieldValue`]。

use std::any::Any;
use std::sync::Arc;

use crate::model::CacheObject;
use crate::value::{write_quoted, FieldKind, FieldValue};

/// JSON 值（解析中间表示）。
#[derive(Clone, Debug, PartialEq)]
pub enum JsonValue {
    /// null
    Null,
    /// 布尔
    Bool(bool),
    /// 整数
    Int(i64),
    /// 浮点
    Float(f64),
    /// 文本
    Text(String),
    /// 数组
    Array(Vec<JsonValue>),
    /// 对象（保持出现顺序）
    Object(Vec<(String, JsonValue)>),
}

impl JsonValue {
    /// 按键取值（大小写不敏感；C# 属性名大小写不一也能容错）。
    pub fn get(&self, key: &str) -> Option<&JsonValue> {
        match self {
            JsonValue::Object(entries) => entries
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(key))
                .map(|(_, v)| v),
            _ => None,
        }
    }

    /// 转为 `FieldValue`（结构值保留为 List/Map）。
    pub fn to_field_value(&self) -> FieldValue {
        match self {
            JsonValue::Null => FieldValue::Null,
            JsonValue::Bool(v) => FieldValue::Bool(*v),
            JsonValue::Int(v) => FieldValue::Int(*v),
            JsonValue::Float(v) => FieldValue::Float(*v),
            JsonValue::Text(v) => FieldValue::Text(v.clone()),
            JsonValue::Array(items) => {
                FieldValue::List(items.iter().map(|v| v.to_field_value()).collect())
            }
            JsonValue::Object(entries) => FieldValue::Map(
                entries
                    .iter()
                    .map(|(k, v)| (k.clone(), v.to_field_value()))
                    .collect(),
            ),
        }
    }

    /// 按字段类型转换为字段值。
    pub fn coerce(&self, kind: FieldKind) -> Option<FieldValue> {
        let generic = self.to_field_value();
        match kind {
            FieldKind::Text => {
                if generic.is_null() {
                    None
                } else {
                    generic.to_text().map(FieldValue::Text)
                }
            }
            FieldKind::Int => generic.to_i64().map(FieldValue::Int),
            FieldKind::Float => generic.to_f64().map(FieldValue::Float),
            FieldKind::Bool => generic.to_bool().map(FieldValue::Bool),
            FieldKind::Time => generic.to_time().map(FieldValue::Time),
            FieldKind::Json => {
                if generic.is_null() {
                    None
                } else {
                    Some(generic)
                }
            }
        }
    }
}

/// 宽松 JSON 解析（C# 兼容）。
pub fn parse(text: &str) -> Result<JsonValue, String> {
    let mut parser = Parser {
        bytes: text.as_bytes(),
        pos: 0,
    };
    parser.skip_ws()?;
    let value = parser.parse_value()?;
    parser.skip_ws()?;
    if parser.pos != parser.bytes.len() {
        return Err(format!("位置 {} 之后存在多余内容", parser.pos));
    }
    Ok(value)
}

/// 反序列化实体列表（顶层允许数组或单个对象）。
pub(crate) fn parse_entities<T: CacheObject>(text: &str) -> Result<Vec<T>, String> {
    let value = parse(text)?;
    let objects: Vec<&JsonValue> = match &value {
        JsonValue::Array(items) => items.iter().collect(),
        JsonValue::Object(_) => vec![&value],
        JsonValue::Null => return Ok(Vec::new()),
        _ => return Err("顶层应为对象数组".to_string()),
    };

    let fields = T::fields();
    let mut results = Vec::with_capacity(objects.len());
    for object in objects {
        let JsonValue::Object(entries) = object else {
            return Err("数组元素应为对象".to_string());
        };
        let mut entity = T::default();
        for (key, value) in entries {
            if key.eq_ignore_ascii_case("Id") {
                if let Some(id) = value.to_field_value().to_i64() {
                    entity.set_id(id);
                }
                continue;
            }
            if key.eq_ignore_ascii_case("Name") {
                if let Some(name) = value.to_field_value().to_text() {
                    entity.set_name(&name);
                }
                continue;
            }
            let Some(field) = fields
                .iter()
                .find(|f| f.name == key)
                .or_else(|| fields.iter().find(|f| f.name.eq_ignore_ascii_case(key)))
            else {
                continue;
            };
            if let Some(converted) = value.coerce(field.kind) {
                entity.set_field(field.name, &converted);
            }
        }
        results.push(entity);
    }
    Ok(results)
}

/// 序列化实体快照（C# `SimpleJsonString.ConvertList` 口径）。
pub(crate) fn serialize_snapshot<T: CacheObject>(
    items: &[Arc<dyn Any + Send + Sync>],
    pretty: bool,
) -> String {
    let mut out = String::with_capacity(items.len() * 96 + 4);
    if pretty {
        out.push_str("[\r\n");
    } else {
        out.push_str("[ ");
    }

    let mut first = true;
    for item in items {
        let Ok(entity) = item.clone().downcast::<T>() else {
            continue;
        };
        if pretty {
            out.push('\t');
        } else if !first {
            out.push_str(", ");
        }
        write_entity(&mut out, entity.as_ref());
        if pretty {
            out.push_str(",\r\n");
        }
        first = false;
    }
    if pretty {
        if !first {
            // 去掉最后一行的逗号（C# 写法：最后一个对象后不加逗号）
            out.truncate(out.len() - 3);
        }
        out.push(']');
    } else {
        out.push_str(" ]");
    }
    out
}

/// 写单个实体（`{ Id:1, Name:"x", Field:value }`）。
fn write_entity(out: &mut String, entity: &dyn CacheObjectDyn) {
    out.push_str("{ ");
    out.push_str("Id:");
    out.push_str(&entity.dyn_id().to_string());
    out.push_str(", Name:");
    write_quoted(out, entity.dyn_name());

    // 字段顺序固定为 fields() 声明顺序（保证输出稳定，便于 diff）
    for field in entity.dyn_fields() {
        // Id/Name 由框架特殊处理（与 C# 序列化一致），字段清单里重复声明时跳过
        if field.name.eq_ignore_ascii_case("Id") || field.name.eq_ignore_ascii_case("Name") {
            continue;
        }
        if !field.save {
            continue;
        }
        let Some(value) = entity.dyn_get_field(field.name) else {
            continue;
        };
        if value.is_null() {
            continue;
        }
        out.push_str(", ");
        out.push_str(field.name);
        out.push(':');
        write_value(out, &value);
    }
    out.push_str(" }");
}

/// 写值（C# `JsonString` 口径）。
fn write_value(out: &mut String, value: &FieldValue) {
    match value {
        FieldValue::Null => out.push_str("null"),
        FieldValue::Bool(v) => out.push_str(if *v { "true" } else { "false" }),
        FieldValue::Int(v) => out.push_str(&v.to_string()),
        FieldValue::Float(v) => out.push_str(&FieldValue::Float(*v).to_json_text()),
        FieldValue::Text(s) => write_quoted(out, s),
        FieldValue::Time(t) => {
            write_quoted(out, &t.format("%Y-%m-%d %H:%M:%S").to_string())
        }
        FieldValue::List(items) => {
            out.push_str("[ ");
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                write_value(out, item);
            }
            out.push_str(" ]");
        }
        FieldValue::Map(entries) => {
            out.push_str("{ ");
            for (i, (key, value)) in entries.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                write_quoted(out, key);
                out.push(':');
                write_value(out, value);
            }
            out.push_str(" }");
        }
    }
}

/// 类型擦除后可序列化的实体视图（避免 `serialize_snapshot` 依赖具体类型）。
trait CacheObjectDyn {
    fn dyn_id(&self) -> i64;
    fn dyn_name(&self) -> &str;
    fn dyn_fields(&self) -> &'static [crate::model::Field];
    fn dyn_get_field(&self, name: &str) -> Option<FieldValue>;
}

impl<T: CacheObject> CacheObjectDyn for T {
    fn dyn_id(&self) -> i64 {
        self.id()
    }
    fn dyn_name(&self) -> &str {
        self.name()
    }
    fn dyn_fields(&self) -> &'static [crate::model::Field] {
        T::fields()
    }
    fn dyn_get_field(&self, name: &str) -> Option<FieldValue> {
        self.get_field(name)
    }
}

// ---------------------------------------------------------------------------
// 递归下降解析器
// ---------------------------------------------------------------------------

struct Parser<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn skip_ws(&mut self) -> Result<(), String> {
        loop {
            while self.pos < self.bytes.len() && self.bytes[self.pos].is_ascii_whitespace() {
                self.pos += 1;
            }
            // `//` 行注释（对齐 C# CharSource.clearComment）
            if self.pos + 1 < self.bytes.len()
                && self.bytes[self.pos] == b'/'
                && self.bytes[self.pos + 1] == b'/'
            {
                while self.pos < self.bytes.len() && self.bytes[self.pos] != b'\n' {
                    self.pos += 1;
                }
                continue;
            }
            break;
        }
        Ok(())
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn bump(&mut self) -> Result<u8, String> {
        let b = self
            .peek()
            .ok_or_else(|| format!("位置 {} 处意外结束", self.pos))?;
        self.pos += 1;
        Ok(b)
    }

    fn expect(&mut self, expected: u8) -> Result<(), String> {
        let b = self.bump()?;
        if b != expected {
            return Err(format!(
                "位置 {} 期望 '{}'，实际 '{}'",
                self.pos - 1,
                expected as char,
                b as char
            ));
        }
        Ok(())
    }

    fn parse_value(&mut self) -> Result<JsonValue, String> {
        self.skip_ws()?;
        match self.peek() {
            Some(b'{') => self.parse_object(),
            Some(b'[') => self.parse_array(),
            Some(b'"') => self.parse_string().map(JsonValue::Text),
            Some(b't') => {
                self.expect_literal("true")?;
                Ok(JsonValue::Bool(true))
            }
            Some(b'f') => {
                self.expect_literal("false")?;
                Ok(JsonValue::Bool(false))
            }
            Some(b'n') => {
                self.expect_literal("null")?;
                Ok(JsonValue::Null)
            }
            Some(b'-') | Some(b'0'..=b'9') => self.parse_number(),
            Some(other) => Err(format!(
                "位置 {} 处出现非法字符 '{}'",
                self.pos, other as char
            )),
            None => Err("意外的输入结束".to_string()),
        }
    }

    fn expect_literal(&mut self, literal: &str) -> Result<(), String> {
        let bytes = literal.as_bytes();
        if self.bytes.len() < self.pos + bytes.len()
            || &self.bytes[self.pos..self.pos + bytes.len()] != bytes
        {
            return Err(format!("位置 {} 处期望 {literal}", self.pos));
        }
        self.pos += bytes.len();
        Ok(())
    }

    fn parse_object(&mut self) -> Result<JsonValue, String> {
        self.expect(b'{')?;
        let mut entries = Vec::new();
        loop {
            self.skip_ws()?;
            if self.peek() == Some(b'}') {
                self.pos += 1;
                break;
            }
            if !entries.is_empty() {
                self.expect(b',')?;
                self.skip_ws()?;
                // 容忍尾逗号
                if self.peek() == Some(b'}') {
                    self.pos += 1;
                    break;
                }
            }
            let key = if self.peek() == Some(b'"') {
                self.parse_string()?
            } else {
                self.parse_bare_key()?
            };
            self.skip_ws()?;
            self.expect(b':')?;
            let value = self.parse_value()?;
            entries.push((key, value));
        }
        Ok(JsonValue::Object(entries))
    }

    /// 裸键（C# `SimpleJsonString` 格式，如 `Id` / `CreateTime`）。
    fn parse_bare_key(&mut self) -> Result<String, String> {
        let start = self.pos;
        while let Some(b) = self.peek() {
            if b.is_ascii_whitespace() || b == b':' || b == b',' || b == b'}' {
                break;
            }
            self.pos += 1;
        }
        if start == self.pos {
            return Err(format!("位置 {} 处缺少对象键", self.pos));
        }
        String::from_utf8(self.bytes[start..self.pos].to_vec())
            .map_err(|e| format!("键名不是合法 UTF-8：{e}"))
    }

    fn parse_array(&mut self) -> Result<JsonValue, String> {
        self.expect(b'[')?;
        let mut items = Vec::new();
        loop {
            self.skip_ws()?;
            if self.peek() == Some(b']') {
                self.pos += 1;
                break;
            }
            if !items.is_empty() {
                self.expect(b',')?;
                self.skip_ws()?;
                // C# ArrayJsonParser 对连续逗号插入 null；这里容忍尾逗号
                if self.peek() == Some(b']') {
                    self.pos += 1;
                    break;
                }
                if self.peek() == Some(b',') {
                    items.push(JsonValue::Null);
                    continue;
                }
            }
            items.push(self.parse_value()?);
        }
        Ok(JsonValue::Array(items))
    }

    fn parse_string(&mut self) -> Result<String, String> {
        self.expect(b'"')?;
        let mut out = String::new();
        loop {
            let b = self.bump()?;
            match b {
                b'"' => break,
                b'\\' => {
                    let esc = self.bump()?;
                    match esc {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{0008}'),
                        b'f' => out.push('\u{000C}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let first = self.parse_hex4()?;
                            // 代理对：\uD83D\uDE00
                            if (0xD800..=0xDBFF).contains(&first) {
                                let save = self.pos;
                                if self.peek() == Some(b'\\') {
                                    self.pos += 1;
                                    if self.peek() == Some(b'u') {
                                        self.pos += 1;
                                        let second = self.parse_hex4()?;
                                        if (0xDC00..=0xDFFF).contains(&second) {
                                            let code =
                                                0x10000 + ((first - 0xD800) << 10) + (second - 0xDC00);
                                            if let Some(ch) = char::from_u32(code) {
                                                out.push(ch);
                                                continue;
                                            }
                                        }
                                    }
                                }
                                self.pos = save;
                                out.push('\u{FFFD}');
                            } else if let Some(ch) = char::from_u32(first) {
                                out.push(ch);
                            } else {
                                out.push('\u{FFFD}');
                            }
                        }
                        other => {
                            return Err(format!(
                                "位置 {} 处非法转义 \\{}",
                                self.pos - 1,
                                other as char
                            ));
                        }
                    }
                }
                _ => {
                    // UTF-8 多字节：按原始字节收集后统一解码（`"`/`\` 均为 ASCII，不会落入续字节）
                    let start = self.pos - 1;
                    self.pos = start;
                    while let Some(b) = self.peek() {
                        if b == b'"' || b == b'\\' {
                            break;
                        }
                        self.pos += 1;
                    }
                    let chunk = std::str::from_utf8(&self.bytes[start..self.pos])
                        .map_err(|e| format!("位置 {start} 处字符串不是合法 UTF-8：{e}"))?;
                    // C# 版写入时会去掉 \r\n\t；读取端保留原样更安全
                    out.push_str(chunk);
                }
            }
        }
        Ok(out)
    }

    fn parse_hex4(&mut self) -> Result<u32, String> {
        let mut value = 0u32;
        for _ in 0..4 {
            let b = self.bump()?;
            let digit = (b as char)
                .to_digit(16)
                .ok_or_else(|| format!("位置 {} 处非法十六进制字符", self.pos - 1))?;
            value = (value << 4) | digit;
        }
        Ok(value)
    }

    fn parse_number(&mut self) -> Result<JsonValue, String> {
        let start = self.pos;
        if self.peek() == Some(b'-') {
            self.pos += 1;
        }
        while let Some(b) = self.peek() {
            if b.is_ascii_digit() || matches!(b, b'.' | b'e' | b'E' | b'+' | b'-') {
                self.pos += 1;
            } else {
                break;
            }
        }
        let text = std::str::from_utf8(&self.bytes[start..self.pos])
            .map_err(|e| format!("数字不是合法 UTF-8：{e}"))?;
        let is_float = text.contains('.') || text.contains('e') || text.contains('E');
        if !is_float && let Ok(v) = text.parse::<i64>() {
            return Ok(JsonValue::Int(v));
        }
        text.parse::<f64>()
            .map(JsonValue::Float)
            .map_err(|_| format!("位置 {start} 处数字非法：{text}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Field;

    #[derive(Clone, Debug, Default, PartialEq)]
    struct Demo {
        id: i64,
        name: String,
        age: i64,
        tags: Vec<String>,
    }

    static DEMO_FIELDS: &[Field] = &[
        Field::text("Name"),
        Field::int("Age"),
        Field::json("Tags"),
    ];

    impl CacheObject for Demo {
        fn type_name() -> &'static str {
            "Test.Demo"
        }
        fn id(&self) -> i64 {
            self.id
        }
        fn set_id(&mut self, id: i64) {
            self.id = id;
        }
        fn name(&self) -> &str {
            &self.name
        }
        fn set_name(&mut self, name: &str) {
            self.name = name.to_string();
        }
        fn fields() -> &'static [Field] {
            DEMO_FIELDS
        }
        fn get_field(&self, name: &str) -> Option<FieldValue> {
            match name {
                "Name" => Some(self.name.clone().into()),
                "Age" => Some(self.age.into()),
                "Tags" => Some(self.tags.clone().into()),
                _ => None,
            }
        }
        fn set_field(&mut self, name: &str, value: &FieldValue) -> bool {
            match name {
                "Name" => {
                    self.name = value.to_text().unwrap_or_default();
                    true
                }
                "Age" => {
                    self.age = value.to_i64().unwrap_or_default();
                    true
                }
                "Tags" => {
                    if let FieldValue::List(items) = value {
                        self.tags = items
                            .iter()
                            .filter_map(|v| v.to_text())
                            .collect::<Vec<_>>();
                        true
                    } else {
                        false
                    }
                }
                _ => false,
            }
        }
    }

    #[test]
    fn parses_bare_keys_and_comments() {
        let text = r#"
        // 这是 C# 版写出的格式
        [
            { Id:1, Name:"张三", Age:30 },
            { Id:2, Name:"a\"b", Age:18, },
        ]
        "#;
        let value = parse(text).unwrap();
        let JsonValue::Array(items) = value else {
            panic!()
        };
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].get("Id"), Some(&JsonValue::Int(1)));
        assert_eq!(items[1].get("Name"), Some(&JsonValue::Text("a\"b".into())));
    }

    #[test]
    fn parses_standard_json_and_surrogate_pairs() {
        let value = parse(r#"{"s":"\uD83D\uDE00","n":[1,2.5,true,null]}"#).unwrap();
        assert_eq!(value.get("s"), Some(&JsonValue::Text("😀".into())));
        match value.get("n") {
            Some(JsonValue::Array(items)) => {
                assert_eq!(items[0], JsonValue::Int(1));
                assert_eq!(items[1], JsonValue::Float(2.5));
                assert_eq!(items[2], JsonValue::Bool(true));
                assert_eq!(items[3], JsonValue::Null);
            }
            _ => panic!(),
        }
    }

    #[test]
    fn entities_roundtrip_with_field_kind_coercion() {
        let mut demo = Demo {
            id: 7,
            name: "七".into(),
            age: 30,
            tags: vec!["a".into(), "b".into()],
        };
        demo.set_id(7);
        let items: Vec<Arc<dyn Any + Send + Sync>> = vec![Arc::new(demo)];
        let text = serialize_snapshot::<Demo>(&items, true);
        assert!(text.contains("Id:7"), "实际输出：{text}");
        assert!(text.contains("Name:\"七\""), "实际输出：{text}");
        assert!(text.contains("Tags:[ \"a\", \"b\" ]"), "实际输出：{text}");

        let parsed = parse_entities::<Demo>(&text).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].id, 7);
        assert_eq!(parsed[0].name, "七");
        assert_eq!(parsed[0].age, 30);
        assert_eq!(parsed[0].tags, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn writes_csharp_compatible_bare_keys() {
        let items: Vec<Arc<dyn Any + Send + Sync>> = vec![Arc::new(Demo {
            id: 1,
            name: "x".into(),
            age: 1,
            tags: Vec::new(),
        })];
        let text = serialize_snapshot::<Demo>(&items, false);
        assert_eq!(text, "[ { Id:1, Name:\"x\", Age:1, Tags:[  ] } ]");
    }
}
