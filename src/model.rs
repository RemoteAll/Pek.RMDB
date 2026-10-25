//! 实体模型描述：字段元数据、唯一约束、`CacheObject` 契约与查询结果结构。
//!
//! Rust 没有运行时反射，因此实体通过 [`CacheObject`] 显式描述：
//! - `fields()`：字段清单（名称/类型/是否建索引/是否持久化/单字段唯一约束）；
//! - `composite_uniques()`：类级复合唯一约束（对应 C# `[CompositeUnique]`）；
//! - `get_field` / `set_field`：按字段名读写（由实体实现，通常一个 `match` 即可）。
//!
//! 字段名建议使用 **PascalCase**，与 C# 属性名保持一致，便于直接读取 C# 版写出的
//! `Data/*.json`（解析器也兼容大小写不敏感匹配）。

use crate::value::{FieldKind, FieldValue};

/// 单字段唯一约束（对应 C# `[Unique(ErrorMessage, AllowNull)]`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UniqueRule {
    /// 冲突时的业务错误消息。
    pub message: &'static str,
    /// 是否允许空值（默认允许；不允许时空值也视为冲突）。
    pub allow_null: bool,
}

/// 复合字段唯一约束（对应 C# `[CompositeUnique(groupName, fields...)]`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CompositeUniqueRule {
    /// 约束组名（同类型多个复合约束时用于区分索引）。
    pub group: &'static str,
    /// 参与约束的字段名（至少 2 个）。
    pub fields: &'static [&'static str],
    /// 冲突时的业务错误消息。
    pub message: &'static str,
    /// 组合中包含空值时是否允许（默认允许）。
    pub allow_null: bool,
}

impl CompositeUniqueRule {
    /// 创建复合唯一约束（允许空值，系统默认）。
    pub const fn new(
        group: &'static str,
        fields: &'static [&'static str],
        message: &'static str,
    ) -> Self {
        Self {
            group,
            fields,
            message,
            allow_null: true,
        }
    }

    /// 创建复合唯一约束（含空值即视为冲突）。
    pub const fn required(
        group: &'static str,
        fields: &'static [&'static str],
        message: &'static str,
    ) -> Self {
        Self {
            group,
            fields,
            message,
            allow_null: false,
        }
    }
}

/// 字段元数据。
///
/// 使用 `const fn` 构造器，可直接放进 `static` 数组（零运行时初始化）：
///
/// ```ignore
/// static FIELDS: &[Field] = &[
///     Field::text("Username").unique("用户名已存在").unique_required(),
///     Field::int("Age"),
///     Field::time("CreateTime").not_indexed(),
///     Field::text("Cache").not_save(),
/// ];
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Field {
    /// 字段名（建议 PascalCase，与 C# 属性名一致）。
    pub name: &'static str,
    /// 字段类型（决定索引策略）。
    pub kind: FieldKind,
    /// 是否建立普通索引（默认 `true`；对应 C# `[NotIndexed]` 时置 `false`）。
    pub indexed: bool,
    /// 是否持久化（默认 `true`；对应 C# `[NotSave]` 时置 `false`）。
    pub save: bool,
    /// 单字段唯一约束。
    pub unique: Option<UniqueRule>,
}

impl Field {
    /// 创建字段（默认建索引、持久化、无唯一约束）。
    pub const fn new(name: &'static str, kind: FieldKind) -> Self {
        Self {
            name,
            kind,
            indexed: true,
            save: true,
            unique: None,
        }
    }

    /// 文本字段。
    pub const fn text(name: &'static str) -> Self {
        Self::new(name, FieldKind::Text)
    }

    /// 整数字段。
    pub const fn int(name: &'static str) -> Self {
        Self::new(name, FieldKind::Int)
    }

    /// 浮点字段。
    pub const fn float(name: &'static str) -> Self {
        Self::new(name, FieldKind::Float)
    }

    /// 布尔字段。
    pub const fn bool(name: &'static str) -> Self {
        Self::new(name, FieldKind::Bool)
    }

    /// 时间字段。
    pub const fn time(name: &'static str) -> Self {
        Self::new(name, FieldKind::Time)
    }

    /// 复杂对象字段（列表/映射；按 JSON 文本建精确索引）。
    pub const fn json(name: &'static str) -> Self {
        Self::new(name, FieldKind::Json)
    }

    /// 添加单字段唯一约束（允许空值）。
    pub const fn unique(mut self, message: &'static str) -> Self {
        self.unique = Some(UniqueRule {
            message,
            allow_null: true,
        });
        self
    }

    /// 添加单字段唯一约束（不允许空值）。
    pub const fn unique_required(mut self, message: &'static str) -> Self {
        self.unique = Some(UniqueRule {
            message,
            allow_null: false,
        });
        self
    }

    /// 不建立普通索引（大文本/低频查询字段）。
    pub const fn not_indexed(mut self) -> Self {
        self.indexed = false;
        self
    }

    /// 不持久化（运行时缓存字段）。
    pub const fn not_save(mut self) -> Self {
        self.save = false;
        self
    }
}

/// 内存数据库实体契约。
///
/// 实现要求：
/// - `Clone + Default + Send + Sync + 'static`（`Default` 供反序列化构造空实体）；
/// - `type_name()` 决定持久化文件名 `Data/{type_name}.json`，**不含路径分隔符**；
/// - `fields()` 返回 `'static` 字段描述（可用 `static FIELDS: &[Field] = &[..]`）。
///
/// `Id` / `Name` 由框架维护（主键与名称），无需放进 `fields()`；
/// 框架会自动为它们建立索引，`find_by("Name", ..)` / `find_by("Id", ..)` 都可用。
pub trait CacheObject: Clone + Default + Send + Sync + 'static {
    /// 类型名（持久化文件名；对应 C# `Type.FullName`，建议 `模块.类型` 形式）。
    fn type_name() -> &'static str;

    /// 主键。插入时由框架分配并回写。
    fn id(&self) -> i64;

    /// 设置主键（框架在插入/加载时调用）。
    fn set_id(&mut self, id: i64);

    /// 名称（对应 C# `CacheObject.Name`，默认为空）。
    fn name(&self) -> &str {
        ""
    }

    /// 设置名称（加载/外部赋值时调用）。
    fn set_name(&mut self, _name: &str) {}

    /// 字段清单（建议 PascalCase 名称）。
    fn fields() -> &'static [Field] {
        &[]
    }

    /// 复合唯一约束清单。
    fn composite_uniques() -> &'static [CompositeUniqueRule] {
        &[]
    }

    /// 按字段名读取值（字段不存在返回 `None`）。
    fn get_field(&self, name: &str) -> Option<FieldValue>;

    /// 按字段名写入值（字段不存在或类型不符返回 `false`）。
    fn set_field(&mut self, name: &str, value: &FieldValue) -> bool;

    /// 整个类型是否常驻内存、不落盘（对应 C# 类级 `[NotSave]`）。
    fn not_save() -> bool {
        false
    }
}

/// 分页结果（对应 C# `PagedResult<T>`）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PagedResult<T> {
    /// 当前页数据。
    pub items: Vec<T>,
    /// 页索引（从 0 开始）。
    pub page_index: usize,
    /// 页大小。
    pub page_size: usize,
    /// 总记录数。
    pub total_count: usize,
    /// 总页数。
    pub total_pages: usize,
}

impl<T> PagedResult<T> {
    /// 是否为空页。
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

/// 单个索引的统计信息（对应 C# `IndexStatistics`）。
#[derive(Clone, Debug, PartialEq)]
pub struct IndexStatistics {
    /// 字段名。
    pub property_name: String,
    /// 索引类型（`StringIndex` / `NumericIndex` / `BooleanIndex` / `DateTimeIndex` / `GenericIndex`）。
    pub index_type: String,
    /// 查询次数。
    pub query_count: u64,
    /// 命中次数。
    pub hit_count: u64,
    /// 唯一值数量。
    pub unique_value_count: usize,
    /// 索引中的 ID 总数（含重复值对应的多行）。
    pub id_count: usize,
}

impl IndexStatistics {
    /// 命中率（0~1）。
    pub fn hit_rate(&self) -> f64 {
        if self.query_count == 0 {
            0.0
        } else {
            self.hit_count as f64 / self.query_count as f64
        }
    }
}

/// 单表统计信息。
#[derive(Clone, Debug, PartialEq)]
pub struct TableStatistics {
    /// 类型名。
    pub type_name: String,
    /// 当前行数。
    pub rows: usize,
    /// 索引统计。
    pub indexes: Vec<IndexStatistics>,
    /// 单字段唯一约束数量。
    pub unique_fields: usize,
    /// 复合唯一约束数量。
    pub unique_composites: usize,
    /// 持久化次数。
    pub persist_count: u64,
    /// 累计写盘字节数。
    pub persisted_bytes: u64,
    /// 最近一次持久化耗时（毫秒）。
    pub last_persist_ms: u64,
    /// 是否常驻内存（不落盘）。
    pub in_memory: bool,
}

/// 全库统计信息。
#[derive(Clone, Debug, PartialEq)]
pub struct DbStatistics {
    /// 各表统计（按类型名排序）。
    pub tables: Vec<TableStatistics>,
    /// 表总数。
    pub table_count: usize,
    /// 行数合计。
    pub total_rows: usize,
}
