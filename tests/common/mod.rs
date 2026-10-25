//! 集成测试共享实体与辅助函数。
#![allow(dead_code)]

use chrono::NaiveDateTime;
use pek_rmdb::{
    CacheObject, CompositeUniqueRule, DbOptions, Field, FieldValue, MemoryDb, UniqueRule,
};
use std::time::Duration;

/// 用户实体（覆盖文本/整数/时间/JSON 字段、单字段唯一、非持久化字段）。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct User {
    pub id: i64,
    pub name: String,
    pub username: String,
    pub email: String,
    /// 空串视为 Null（配合 `allow_null` 语义）。
    pub phone: String,
    pub age: i64,
    pub status: String,
    pub enabled: bool,
    pub tags: Vec<String>,
    pub create_time: Option<NaiveDateTime>,
    /// 未建索引的大文本字段。
    pub remark: String,
    /// 运行期缓存字段（不落盘）。
    pub cache: String,
}

pub static USER_FIELDS: &[Field] = &[
    Field::text("Username").unique("用户名已存在"),
    Field::text("Email").unique_required("邮箱已被注册"),
    Field::text("Phone").unique("手机号已被使用"),
    Field::int("Age"),
    Field::text("Status"),
    Field::bool("Enabled"),
    Field::json("Tags"),
    Field::time("CreateTime"),
    Field::text("Remark").not_indexed(),
    Field::text("Cache").not_save(),
];

impl CacheObject for User {
    fn type_name() -> &'static str {
        "Test.User"
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
        USER_FIELDS
    }

    fn get_field(&self, field: &str) -> Option<FieldValue> {
        match field {
            "Username" => Some(self.username.clone().into()),
            "Email" => {
                if self.email.is_empty() {
                    Some(FieldValue::Null)
                } else {
                    Some(self.email.clone().into())
                }
            }
            "Phone" => {
                if self.phone.is_empty() {
                    Some(FieldValue::Null)
                } else {
                    Some(self.phone.clone().into())
                }
            }
            "Age" => Some(self.age.into()),
            "Status" => Some(self.status.clone().into()),
            "Enabled" => Some(self.enabled.into()),
            "Tags" => Some(self.tags.clone().into()),
            "CreateTime" => Some(self.create_time.into()),
            "Remark" => Some(self.remark.clone().into()),
            "Cache" => Some(self.cache.clone().into()),
            _ => None,
        }
    }

    fn set_field(&mut self, field: &str, value: &FieldValue) -> bool {
        match field {
            "Username" => {
                self.username = value.to_text().unwrap_or_default();
                true
            }
            "Email" => {
                self.email = value.to_text().unwrap_or_default();
                true
            }
            "Phone" => {
                self.phone = value.to_text().unwrap_or_default();
                true
            }
            "Age" => {
                self.age = value.to_i64().unwrap_or_default();
                true
            }
            "Status" => {
                self.status = value.to_text().unwrap_or_default();
                true
            }
            "Enabled" => {
                self.enabled = value.to_bool().unwrap_or_default();
                true
            }
            "Tags" => {
                if let FieldValue::List(items) = value {
                    self.tags = items.iter().filter_map(|v| v.to_text()).collect();
                    true
                } else {
                    false
                }
            }
            "CreateTime" => {
                self.create_time = value.to_time();
                true
            }
            "Remark" => {
                self.remark = value.to_text().unwrap_or_default();
                true
            }
            "Cache" => {
                self.cache = value.to_text().unwrap_or_default();
                true
            }
            _ => false,
        }
    }
}

/// 员工实体（复合唯一约束）。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Employee {
    pub id: i64,
    pub name: String,
    pub company_id: i64,
    pub employee_number: String,
    pub id_card_number: String,
    pub full_name: String,
}

pub static EMPLOYEE_FIELDS: &[Field] = &[
    Field::int("CompanyId"),
    Field::text("EmployeeNumber"),
    Field::text("IdCardNumber").unique("身份证号已存在"),
];

pub static EMPLOYEE_COMPOSITES: &[CompositeUniqueRule] = &[CompositeUniqueRule::new(
    "CompanyUnique",
    &["CompanyId", "EmployeeNumber"],
    "同一公司内工号必须唯一",
)];

impl CacheObject for Employee {
    fn type_name() -> &'static str {
        "Test.Employee"
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
        EMPLOYEE_FIELDS
    }

    fn composite_uniques() -> &'static [CompositeUniqueRule] {
        EMPLOYEE_COMPOSITES
    }

    fn get_field(&self, field: &str) -> Option<FieldValue> {
        match field {
            "CompanyId" => Some(self.company_id.into()),
            "EmployeeNumber" => Some(self.employee_number.clone().into()),
            "IdCardNumber" => Some(self.id_card_number.clone().into()),
            "FullName" => Some(self.full_name.clone().into()),
            _ => None,
        }
    }

    fn set_field(&mut self, field: &str, value: &FieldValue) -> bool {
        match field {
            "CompanyId" => {
                self.company_id = value.to_i64().unwrap_or_default();
                true
            }
            "EmployeeNumber" => {
                self.employee_number = value.to_text().unwrap_or_default();
                true
            }
            "IdCardNumber" => {
                self.id_card_number = value.to_text().unwrap_or_default();
                true
            }
            "FullName" => {
                self.full_name = value.to_text().unwrap_or_default();
                true
            }
            _ => false,
        }
    }
}

/// 只读缓存实体（类级 `not_save`：不读写磁盘）。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Setting {
    pub id: i64,
    pub name: String,
    pub value: String,
}

pub static SETTING_FIELDS: &[Field] = &[Field::text("Value")];

impl CacheObject for Setting {
    fn type_name() -> &'static str {
        "Test.Setting"
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
        SETTING_FIELDS
    }

    fn get_field(&self, field: &str) -> Option<FieldValue> {
        match field {
            "Value" => Some(self.value.clone().into()),
            _ => None,
        }
    }

    fn set_field(&mut self, field: &str, value: &FieldValue) -> bool {
        match field {
            "Value" => {
                self.value = value.to_text().unwrap_or_default();
                true
            }
            _ => false,
        }
    }

    fn not_save() -> bool {
        true
    }
}

/// 造一个用户。
pub fn user(index: i64) -> User {
    User {
        id: 0,
        name: format!("用户{index}"),
        username: format!("user{index}"),
        email: format!("user{index}@example.com"),
        phone: String::new(),
        age: 18 + (index % 30),
        status: if index % 2 == 0 { "Active" } else { "Disabled" }.to_string(),
        enabled: index % 2 == 0,
        tags: vec!["tag".to_string(), format!("tag{index}")],
        create_time: NaiveDateTime::parse_from_str(
            &format!("2024-01-{:02} 08:30:00", (index % 28) + 1),
            "%Y-%m-%d %H:%M:%S",
        )
        .ok(),
        remark: format!("备注{index}"),
        cache: format!("cache-{index}"),
    }
}

/// 临时目录 + 快速写盘的数据库（最小间隔 10ms，便于测持久化）。
pub fn temp_db() -> (tempfile::TempDir, MemoryDb) {
    let dir = tempfile::tempdir().expect("创建临时目录");
    let db = MemoryDb::with_options(DbOptions {
        base_dir: dir.path().to_path_buf(),
        auto_persist: true,
        min_write_interval: Duration::from_millis(10),
        pretty: true,
        flush_on_drop: true,
        on_error: None,
    });
    (dir, db)
}

/// 纯内存数据库。
pub fn memory_db() -> MemoryDb {
    MemoryDb::in_memory()
}

/// 等待后台持久化完成（轮询文件出现，最多 3 秒）。
pub fn wait_file(path: &std::path::Path) -> bool {
    for _ in 0..300 {
        if path.exists() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    false
}

/// 唯一约束消息（断言用）。
pub fn unique_rule(message: &'static str) -> UniqueRule {
    UniqueRule {
        message,
        allow_null: true,
    }
}
