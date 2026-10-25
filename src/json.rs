//! 实体 JSON 序列化与反序列化（持久化格式）。
//!
//! # 职责边界
//!
//! 通用能力已下沉到 `dhrust::json`（宽松解析 + C# 口径转义）：
//! - [`JsonValue`] / [`parse`] → `dhrust::json::{JsonValue, parse_lax}`；
//! - 字符串转义 → `dhrust::json::write_quoted`。
//!
//! 本模块只保留**与 `CacheObject`/`FieldValue` 强耦合**的部分：
//! - 实体列表序列化（C# `SimpleJsonString` 风格：裸键、`\r\n` + `\t`、`Id`/`Name` 在前）；
//! - 实体反序列化（按 `fields()` 的 `FieldKind` 强制转换并写回实体）。

use std::any::Any;
use std::sync::Arc;

use dhrust::json::{format_number, write_quoted};

use crate::model::{CacheObject, Field};
use crate::value::{FieldKind, FieldValue};

// 保持 `pek_rmdb::json::JsonValue` 路径稳定（实体为公共库 dhrust 的类型）
pub use dhrust::json::JsonValue;

/// 宽松 JSON 解析（C# 兼容；实现见 `dhrust::json::parse_lax`）。
pub fn parse(text: &str) -> Result<JsonValue, String> {
    dhrust::json::parse_lax(text)
}

/// `JsonValue` → `FieldValue`（结构值保留为 List/Map）。
fn json_to_field_value(value: &JsonValue) -> FieldValue {
    match value {
        JsonValue::Null => FieldValue::Null,
        JsonValue::Bool(flag) => FieldValue::Bool(*flag),
        JsonValue::Int(number) => FieldValue::Int(*number),
        JsonValue::Float(number) => FieldValue::Float(*number),
        JsonValue::Text(text) => FieldValue::Text(text.clone()),
        JsonValue::Array(items) => {
            FieldValue::List(items.iter().map(json_to_field_value).collect())
        }
        JsonValue::Object(entries) => FieldValue::Map(
            entries
                .iter()
                .map(|(key, value)| (key.clone(), json_to_field_value(value)))
                .collect(),
        ),
    }
}

/// 按字段类型把 JSON 值转换为字段值。
fn coerce_json(value: &JsonValue, kind: FieldKind) -> Option<FieldValue> {
    let generic = json_to_field_value(value);
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
                if let Some(id) = value.to_i64() {
                    entity.set_id(id);
                }
                continue;
            }
            if key.eq_ignore_ascii_case("Name") {
                if let Some(name) = value.to_text() {
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
            if let Some(converted) = coerce_json(value, field.kind) {
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
    let mut out = String::with_capacity(items.len() * 192 + 64);
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
        FieldValue::Bool(flag) => out.push_str(if *flag { "true" } else { "false" }),
        FieldValue::Int(number) => out.push_str(&number.to_string()),
        FieldValue::Float(number) => {
            let text = format_number(*number);
            if number.is_finite() {
                out.push_str(&text);
            } else {
                write_quoted(out, &text);
            }
        }
        FieldValue::Text(text) => write_quoted(out, text),
        FieldValue::Time(time) => {
            write_quoted(out, &time.format("%Y-%m-%d %H:%M:%S").to_string())
        }
        FieldValue::List(items) => {
            out.push_str("[ ");
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push_str(", ");
                }
                write_value(out, item);
            }
            out.push_str(" ]");
        }
        FieldValue::Map(entries) => {
            out.push_str("{ ");
            for (index, (key, value)) in entries.iter().enumerate() {
                if index > 0 {
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
    fn dyn_fields(&self) -> &'static [Field];
    fn dyn_get_field(&self, name: &str) -> Option<FieldValue>;
}

impl<T: CacheObject> CacheObjectDyn for T {
    fn dyn_id(&self) -> i64 {
        self.id()
    }
    fn dyn_name(&self) -> &str {
        self.name()
    }
    fn dyn_fields(&self) -> &'static [Field] {
        T::fields()
    }
    fn dyn_get_field(&self, name: &str) -> Option<FieldValue> {
        self.get_field(name)
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

    #[test]
    fn parses_legacy_csharp_values_through_public_api() {
        let text = r#"[
	{ Id:7, Name:"老数据", Age:30, Tags:[ "a", "b" ] }
]"#;
        let parsed = parse_entities::<Demo>(text).unwrap();
        assert_eq!(parsed[0].id, 7);
        assert_eq!(parsed[0].name, "老数据");
        assert_eq!(parsed[0].tags, vec!["a".to_string(), "b".to_string()]);
    }
}
