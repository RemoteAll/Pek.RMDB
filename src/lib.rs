//! # Pek.RMDB —— 高性能内存数据库（Rust 版）
//!
//! C# [`Pek.MDB`](https://github.com/RemoteAll/Pek.MDB) 的 Rust 实现：面向
//! **“读多写少、内存为主、JSON 异步持久化”** 场景，提供唯一约束（单字段/复合）、
//! 类型感知索引、分页/范围/模糊查询与批量操作。
//!
//! ## 快速开始
//!
//! ```
//! use std::sync::Arc;
//! use pek_rmdb::{CacheObject, Field, FieldValue, MemoryDb};
//! use pek_rmdb::value::FieldKind;
//!
//! #[derive(Clone, Debug, Default)]
//! struct User {
//!     id: i64,
//!     name: String,
//!     username: String,
//!     email: String,
//!     age: i64,
//! }
//!
//! static FIELDS: &[Field] = &[
//!     Field::text("Username").unique("用户名已存在"),
//!     Field::text("Email").unique("邮箱已被注册"),
//!     Field::int("Age"),
//! ];
//!
//! impl CacheObject for User {
//!     fn type_name() -> &'static str { "Demo.User" }
//!     fn id(&self) -> i64 { self.id }
//!     fn set_id(&mut self, id: i64) { self.id = id; }
//!     fn name(&self) -> &str { &self.name }
//!     fn set_name(&mut self, name: &str) { self.name = name.to_string(); }
//!     fn fields() -> &'static [Field] { FIELDS }
//!     fn get_field(&self, field: &str) -> Option<FieldValue> {
//!         match field {
//!             "Username" => Some(self.username.clone().into()),
//!             "Email" => Some(self.email.clone().into()),
//!             "Age" => Some(self.age.into()),
//!             _ => None,
//!         }
//!     }
//!     fn set_field(&mut self, field: &str, value: &FieldValue) -> bool {
//!         match field {
//!             "Username" => { self.username = value.to_text().unwrap_or_default(); true }
//!             "Email" => { self.email = value.to_text().unwrap_or_default(); true }
//!             "Age" => { self.age = value.to_i64().unwrap_or_default(); true }
//!             _ => false,
//!         }
//!     }
//! }
//!
//! let db = MemoryDb::in_memory();
//! let id = db.insert(User {
//!     id: 0,
//!     name: "张三".into(),
//!     username: "zhangsan".into(),
//!     email: "zhangsan@example.com".into(),
//!     age: 30,
//! }).unwrap();
//!
//! let user = db.find_by_id::<User>(id).unwrap();
//! assert_eq!(user.name, "张三");
//! assert_eq!(db.find_by_unique::<User>("Username", "zhangsan").unwrap().id, id);
//! assert_eq!(db.find_by::<User>("Age", 30).len(), 1);
//! ```
//!
//! ## 持久化
//!
//! 数据文件位于 `{base_dir}/Data/{type_name}.json`，格式与 C# 版**双向兼容**
//! （裸键对象、`\r\n` + `\t` 缩进）。写盘由后台线程合并执行（默认 500ms 窗口），
//! 调用线程不阻塞；[`MemoryDb::flush`] 可手动同步落盘。
//!
//! ## 设计文档
//!
//! - `docs/与C#对照.md`：API/语义映射与有意差异；
//! - `docs/审计报告.md`：性能与资源审计（含实测数据与 C# 缺陷分析）。

mod db;
mod error;
pub mod json;
mod hash;
mod index;
mod model;
mod persistence;
mod table;
mod unique;
pub mod value;

pub use db::{
    DbOptions, MemoryDb, PersistErrorHandler, clear, configure_global, default_base_dir, delete,
    find_all, find_by, find_by_composite_unique, find_by_id, find_by_like, find_by_multiple,
    find_by_page, find_by_range, find_by_unique, flush, global, insert, insert_batch,
    insert_by_index, is_unique_value_exists, statistics, update,
};
pub use error::{ConstraintType, Result, RmdbError, UniqueViolation};
pub use model::{
    CacheObject, CompositeUniqueRule, DbStatistics, Field, IndexStatistics, PagedResult,
    TableStatistics, UniqueRule,
};
pub use value::{FieldKind, FieldValue};
