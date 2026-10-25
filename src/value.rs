//! 字段值（索引键）统一表示。
//!
//! 设计要点（对应 C# `Pek.MDB` 的 `object` 装箱索引值）：
//! - 用枚举代替装箱，插入/查询热路径零堆分配（`Int`/`Bool`/`Time`）；
//! - 实现 `Hash`/`Ord` 以支持精确索引（`HashMap`）与范围索引（`BTreeMap`）；
//! - `Float` 按 `f64::total_cmp` 比较，NaN 归一化后参与哈希，保证一致性；
//! - 值写入索引前一律按 [`FieldKind`] **规范化**，跨类型查询（如 `i32` 查 `i64` 字段）也能命中，
//!   这比 C# 用 `object` 相等更符合直觉（详见 `docs/与C#对照.md`）。

use std::cmp::Ordering;
use std::fmt;
use std::hash::{Hash, Hasher};

use chrono::{NaiveDate, NaiveDateTime};
use dhrust::json::{format_number, write_quoted};

/// 字段值类型（内存数据库中的“万能值”）。
#[derive(Clone, Debug)]
pub enum FieldValue {
    /// 空值（不建索引；单字段唯一约束的 `allow_null` 语义依据此值）。
    Null,
    /// 布尔。
    Bool(bool),
    /// 整数（i8~i64、u8~u32 等统一收敛到 i64）。
    Int(i64),
    /// 浮点数。
    Float(f64),
    /// 文本。
    Text(String),
    /// 时间（本地时间语义；持久化为 `yyyy-MM-dd HH:mm:ss`）。
    Time(NaiveDateTime),
    /// 列表（不参与索引，仅用于持久化/展示）。
    List(Vec<FieldValue>),
    /// 键值映射（不参与索引，仅用于持久化/展示）。
    Map(Vec<(String, FieldValue)>),
}

impl FieldValue {
    /// 是否为空值。
    pub fn is_null(&self) -> bool {
        matches!(self, FieldValue::Null)
    }

    /// 转为文本（用于文本索引/展示）。
    pub fn to_text(&self) -> Option<String> {
        match self {
            FieldValue::Null => None,
            FieldValue::Text(s) => Some(s.clone()),
            FieldValue::Bool(v) => Some(v.to_string()),
            FieldValue::Int(v) => Some(v.to_string()),
            FieldValue::Float(v) => Some(v.to_string()),
            FieldValue::Time(t) => Some(t.format("%Y-%m-%d %H:%M:%S").to_string()),
            FieldValue::List(_) | FieldValue::Map(_) => Some(self.to_json_text()),
        }
    }

    /// 转为整数（宽松，对齐 C# `cvt.ToLong` 口径；无法转换返回 `None`）。
    pub fn to_i64(&self) -> Option<i64> {
        match self {
            FieldValue::Int(v) => Some(*v),
            FieldValue::Float(v) if v.is_finite() => Some(*v as i64),
            FieldValue::Bool(v) => Some(i64::from(*v)),
            FieldValue::Text(s) => s.trim().parse::<i64>().ok().or_else(|| {
                s.trim()
                    .parse::<f64>()
                    .ok()
                    .filter(|v| v.is_finite())
                    .map(|v| v as i64)
            }),
            // 时间 → Unix 毫秒（按本地时间解释，与 `to_time` 的时间戳分支对称）
            FieldValue::Time(t) => Some(t.and_utc().timestamp_millis()),
            _ => None,
        }
    }

    /// 转为浮点（宽松）。
    pub fn to_f64(&self) -> Option<f64> {
        match self {
            FieldValue::Float(v) => Some(*v),
            FieldValue::Int(v) => Some(*v as f64),
            FieldValue::Bool(v) => Some(if *v { 1.0 } else { 0.0 }),
            FieldValue::Text(s) => s.trim().parse::<f64>().ok(),
            _ => None,
        }
    }

    /// 转为布尔（智能转换，对齐 C# `BooleanIndex`：true/1/yes/on/是、false/0/no/off/否）。
    pub fn to_bool(&self) -> Option<bool> {
        match self {
            FieldValue::Bool(v) => Some(*v),
            FieldValue::Int(v) => Some(*v != 0),
            FieldValue::Float(v) => Some(*v != 0.0),
            FieldValue::Text(s) => dhrust::text::parse_bool(s),
            _ => None,
        }
    }

    /// 转为时间（兼容 `yyyy-MM-dd HH:mm:ss`、ISO 8601、`yyyy/M/d` 等 C# 常见格式，
    /// 以及 Unix 秒/毫秒数字时间戳）。
    pub fn to_time(&self) -> Option<NaiveDateTime> {
        match self {
            FieldValue::Time(t) => Some(*t),
            FieldValue::Text(s) => dhrust::times::parse_datetime_lax(s),
            FieldValue::Int(v) => dhrust::times::from_unix_timestamp(*v),
            FieldValue::Float(v) if v.is_finite() => {
                dhrust::times::from_unix_timestamp(*v as i64)
            }
            _ => None,
        }
    }

    /// 按字段类型规范化（建索引/查询前调用）。返回 `None` 表示该值无法作为该字段的索引键
    /// （例如空值、无法解析的文本），调用方应跳过建索引或返回空结果。
    ///
    /// 按值接收：索引键直接移动进 `HashMap`/`BTreeMap`，热路径少一次字符串克隆。
    pub fn normalize(self, kind: FieldKind) -> Option<FieldValue> {
        match kind {
            FieldKind::Text => match self {
                FieldValue::Null => None,
                FieldValue::Text(s) => Some(FieldValue::Text(s)),
                other => other.to_text().map(FieldValue::Text),
            },
            FieldKind::Int => self.to_i64().map(FieldValue::Int),
            FieldKind::Float => self.to_f64().map(FieldValue::Float),
            FieldKind::Bool => self.to_bool().map(FieldValue::Bool),
            FieldKind::Time => self.to_time().map(FieldValue::Time),
            FieldKind::Json => match self {
                FieldValue::Null => None,
                FieldValue::Text(s) => Some(FieldValue::Text(s)),
                other => Some(FieldValue::Text(other.to_json_text())),
            },
        }
    }

    /// 序列化为 JSON 文本（C# `JsonString` 口径：转义 `\` 与 `"`，去掉 `\r`/`\n`/`\t`）。
    pub fn to_json_text(&self) -> String {
        let mut out = String::new();
        self.write_json_text(&mut out);
        out
    }

    fn write_json_text(&self, out: &mut String) {
        match self {
            FieldValue::Null => out.push_str("null"),
            FieldValue::Bool(v) => out.push_str(if *v { "true" } else { "false" }),
            FieldValue::Int(v) => out.push_str(&v.to_string()),
            FieldValue::Float(v) => {
                let text = format_number(*v);
                if v.is_finite() {
                    out.push_str(&text);
                } else {
                    write_quoted(out, &text);
                }
            }
            FieldValue::Text(s) => write_quoted(out, s),
            FieldValue::Time(t) => write_quoted(out, &t.format("%Y-%m-%d %H:%M:%S").to_string()),
            FieldValue::List(items) => {
                out.push_str("[ ");
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    item.write_json_text(out);
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
                    value.write_json_text(out);
                }
                out.push_str(" }");
            }
        }
    }
}

impl PartialEq for FieldValue {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (FieldValue::Null, FieldValue::Null) => true,
            (FieldValue::Bool(a), FieldValue::Bool(b)) => a == b,
            (FieldValue::Int(a), FieldValue::Int(b)) => a == b,
            (FieldValue::Float(a), FieldValue::Float(b)) => {
                (a.is_nan() && b.is_nan()) || a.total_cmp(b) == Ordering::Equal
            }
            (FieldValue::Text(a), FieldValue::Text(b)) => a == b,
            (FieldValue::Time(a), FieldValue::Time(b)) => a == b,
            (FieldValue::List(a), FieldValue::List(b)) => a == b,
            (FieldValue::Map(a), FieldValue::Map(b)) => a == b,
            _ => false,
        }
    }
}

impl Eq for FieldValue {}

impl Hash for FieldValue {
    fn hash<H: Hasher>(&self, state: &mut H) {
        match self {
            FieldValue::Null => 0u8.hash(state),
            FieldValue::Bool(v) => {
                1u8.hash(state);
                v.hash(state);
            }
            FieldValue::Int(v) => {
                2u8.hash(state);
                v.hash(state);
            }
            FieldValue::Float(v) => {
                3u8.hash(state);
                let bits = if v.is_nan() { f64::NAN.to_bits() } else { v.to_bits() };
                bits.hash(state);
            }
            FieldValue::Text(v) => {
                4u8.hash(state);
                v.hash(state);
            }
            FieldValue::Time(v) => {
                5u8.hash(state);
                v.hash(state);
            }
            FieldValue::List(v) => {
                6u8.hash(state);
                v.hash(state);
            }
            FieldValue::Map(v) => {
                7u8.hash(state);
                v.hash(state);
            }
        }
    }
}

impl PartialOrd for FieldValue {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for FieldValue {
    fn cmp(&self, other: &Self) -> Ordering {
        fn rank(v: &FieldValue) -> u8 {
            match v {
                FieldValue::Null => 0,
                FieldValue::Bool(_) => 1,
                FieldValue::Int(_) => 2,
                FieldValue::Float(_) => 3,
                FieldValue::Text(_) => 4,
                FieldValue::Time(_) => 5,
                FieldValue::List(_) => 6,
                FieldValue::Map(_) => 7,
            }
        }
        match (self, other) {
            (FieldValue::Null, FieldValue::Null) => Ordering::Equal,
            (FieldValue::Bool(a), FieldValue::Bool(b)) => a.cmp(b),
            (FieldValue::Int(a), FieldValue::Int(b)) => a.cmp(b),
            (FieldValue::Float(a), FieldValue::Float(b)) => {
                if a.is_nan() && b.is_nan() {
                    Ordering::Equal
                } else {
                    a.total_cmp(b)
                }
            }
            (FieldValue::Text(a), FieldValue::Text(b)) => a.cmp(b),
            (FieldValue::Time(a), FieldValue::Time(b)) => a.cmp(b),
            (FieldValue::List(a), FieldValue::List(b)) => a.cmp(b),
            (FieldValue::Map(a), FieldValue::Map(b)) => a.cmp(b),
            _ => rank(self).cmp(&rank(other)),
        }
    }
}

impl fmt::Display for FieldValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_json_text())
    }
}

impl From<bool> for FieldValue {
    fn from(v: bool) -> Self {
        FieldValue::Bool(v)
    }
}

impl From<i8> for FieldValue {
    fn from(v: i8) -> Self {
        FieldValue::Int(i64::from(v))
    }
}

impl From<i16> for FieldValue {
    fn from(v: i16) -> Self {
        FieldValue::Int(i64::from(v))
    }
}

impl From<i32> for FieldValue {
    fn from(v: i32) -> Self {
        FieldValue::Int(i64::from(v))
    }
}

impl From<i64> for FieldValue {
    fn from(v: i64) -> Self {
        FieldValue::Int(v)
    }
}

impl From<u8> for FieldValue {
    fn from(v: u8) -> Self {
        FieldValue::Int(i64::from(v))
    }
}

impl From<u16> for FieldValue {
    fn from(v: u16) -> Self {
        FieldValue::Int(i64::from(v))
    }
}

impl From<u32> for FieldValue {
    fn from(v: u32) -> Self {
        FieldValue::Int(i64::from(v))
    }
}

impl From<usize> for FieldValue {
    fn from(v: usize) -> Self {
        FieldValue::Int(v as i64)
    }
}

impl From<f32> for FieldValue {
    fn from(v: f32) -> Self {
        FieldValue::Float(f64::from(v))
    }
}

impl From<f64> for FieldValue {
    fn from(v: f64) -> Self {
        FieldValue::Float(v)
    }
}

impl From<String> for FieldValue {
    fn from(v: String) -> Self {
        FieldValue::Text(v)
    }
}

impl From<&str> for FieldValue {
    fn from(v: &str) -> Self {
        FieldValue::Text(v.to_string())
    }
}

impl From<NaiveDateTime> for FieldValue {
    fn from(v: NaiveDateTime) -> Self {
        FieldValue::Time(v)
    }
}

impl From<NaiveDate> for FieldValue {
    fn from(v: NaiveDate) -> Self {
        FieldValue::Time(v.and_hms_opt(0, 0, 0).unwrap_or_default())
    }
}

impl<T: Into<FieldValue>> From<Option<T>> for FieldValue {
    fn from(v: Option<T>) -> Self {
        match v {
            Some(v) => v.into(),
            None => FieldValue::Null,
        }
    }
}

impl From<Vec<String>> for FieldValue {
    fn from(v: Vec<String>) -> Self {
        FieldValue::List(v.into_iter().map(FieldValue::Text).collect())
    }
}

impl From<Vec<i64>> for FieldValue {
    fn from(v: Vec<i64>) -> Self {
        FieldValue::List(v.into_iter().map(FieldValue::Int).collect())
    }
}

/// 字段类型（决定索引策略与持久化文本形态）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldKind {
    /// 文本：精确索引 + 大小写不敏感模糊/范围查询。
    Text,
    /// 整数：有序索引，范围查询 `O(log n + k)`。
    Int,
    /// 浮点：有序索引。
    Float,
    /// 布尔：双桶索引 + 智能模式查询。
    Bool,
    /// 时间：有序索引 + 年/月/日模式查询。
    Time,
    /// 复杂对象（列表/映射）：按 JSON 文本建精确索引。
    Json,
}

impl FieldKind {
    /// 索引类型名称（统计展示用）。
    pub fn index_type_name(self) -> &'static str {
        match self {
            FieldKind::Text => "StringIndex",
            FieldKind::Int | FieldKind::Float => "NumericIndex",
            FieldKind::Bool => "BooleanIndex",
            FieldKind::Time => "DateTimeIndex",
            FieldKind::Json => "GenericIndex",
        }
    }

    /// 是否使用有序索引（支持范围查询与按索引分页）。
    pub fn is_ordered(self) -> bool {
        matches!(self, FieldKind::Int | FieldKind::Float | FieldKind::Time)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn float_hash_and_order_are_consistent() {
        let nan_a = FieldValue::Float(f64::NAN);
        let nan_b = FieldValue::Float(-f64::NAN);
        assert_eq!(nan_a, nan_b);
        assert_eq!(nan_a.cmp(&nan_b), Ordering::Equal);

        let mut map = std::collections::HashMap::new();
        map.insert(nan_a, 1);
        assert_eq!(map.get(&nan_b), Some(&1));

        assert_eq!(
            FieldValue::Float(1.0).cmp(&FieldValue::Float(2.0)),
            Ordering::Less
        );
        assert_eq!(
            FieldValue::Float(f64::NEG_INFINITY).cmp(&FieldValue::Float(0.0)),
            Ordering::Less
        );
    }

    #[test]
    fn normalize_converts_across_types() {
        assert_eq!(
            FieldValue::Text("42".into()).normalize(FieldKind::Int),
            Some(FieldValue::Int(42))
        );
        assert_eq!(
            FieldValue::Int(1).normalize(FieldKind::Bool),
            Some(FieldValue::Bool(true))
        );
        assert_eq!(
            FieldValue::Text("是".into()).normalize(FieldKind::Bool),
            Some(FieldValue::Bool(true))
        );
        assert_eq!(FieldValue::Null.normalize(FieldKind::Text), None);
        assert_eq!(
            FieldValue::Text("3.5".into()).normalize(FieldKind::Float),
            Some(FieldValue::Float(3.5))
        );
    }

    #[test]
    fn time_parse_covers_csharp_formats() {
        let expect = NaiveDate::from_ymd_opt(2024, 1, 5)
            .unwrap()
            .and_hms_opt(14, 30, 0)
            .unwrap();
        let parse = |text: &str| FieldValue::Text(text.to_string()).to_time();
        assert_eq!(parse("2024-01-05 14:30:00"), Some(expect));
        assert_eq!(parse("2024-01-05T14:30:00"), Some(expect));
        assert_eq!(parse("2024/1/5 14:30:00"), Some(expect));
        assert_eq!(
            parse("2024-01-05"),
            Some(NaiveDate::from_ymd_opt(2024, 1, 5).unwrap().and_hms_opt(0, 0, 0).unwrap())
        );
        // 数字时间戳同样可识别（与共享解析器同一口径）
        let secs = expect.and_utc().timestamp();
        assert_eq!(
            FieldValue::Int(secs).to_time(),
            dhrust::times::from_unix_timestamp(secs)
        );
    }

    #[test]
    fn json_text_escapes_like_csharp() {
        let v = FieldValue::Text("a\"b\\c\r\nd\t".into());
        assert_eq!(v.to_json_text(), "\"a\\\"b\\\\cd\"");
    }
}
