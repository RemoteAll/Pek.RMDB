//! 完整示例：唯一约束 + 索引查询 + 分页 + 复合唯一 + 持久化。
//!
//! ```powershell
//! cargo run --example basic
//! ```

use chrono::NaiveDate;
use pek_rmdb::{
    CacheObject, CompositeUniqueRule, DbOptions, Field, FieldValue, MemoryDb, RmdbError,
};

#[derive(Clone, Debug, Default)]
struct User {
    id: i64,
    name: String,
    username: String,
    email: String,
    age: i64,
    status: String,
    create_time: Option<chrono::NaiveDateTime>,
}

static USER_FIELDS: &[Field] = &[
    Field::text("Username").unique("用户名已存在"),
    Field::text("Email").unique_required("邮箱已被注册"),
    Field::int("Age"),
    Field::text("Status"),
    Field::time("CreateTime"),
];

impl CacheObject for User {
    fn type_name() -> &'static str {
        "Demo.User"
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
            "Email" => Some(self.email.clone().into()),
            "Age" => Some(self.age.into()),
            "Status" => Some(self.status.clone().into()),
            "CreateTime" => Some(self.create_time.into()),
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
            "Age" => {
                self.age = value.to_i64().unwrap_or_default();
                true
            }
            "Status" => {
                self.status = value.to_text().unwrap_or_default();
                true
            }
            "CreateTime" => {
                self.create_time = value.to_time();
                true
            }
            _ => false,
        }
    }
}

#[derive(Clone, Debug, Default)]
struct Employee {
    id: i64,
    name: String,
    company_id: i64,
    employee_number: String,
    id_card_number: String,
}

static EMPLOYEE_FIELDS: &[Field] = &[
    Field::int("CompanyId"),
    Field::text("EmployeeNumber"),
    Field::text("IdCardNumber").unique("身份证号已存在"),
];

static EMPLOYEE_RULES: &[CompositeUniqueRule] = &[CompositeUniqueRule::new(
    "CompanyUnique",
    &["CompanyId", "EmployeeNumber"],
    "同一公司内工号必须唯一",
)];

impl CacheObject for Employee {
    fn type_name() -> &'static str {
        "Demo.Employee"
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
        EMPLOYEE_RULES
    }
    fn get_field(&self, field: &str) -> Option<FieldValue> {
        match field {
            "CompanyId" => Some(self.company_id.into()),
            "EmployeeNumber" => Some(self.employee_number.clone().into()),
            "IdCardNumber" => Some(self.id_card_number.clone().into()),
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
            _ => false,
        }
    }
}

fn user(index: i64) -> User {
    User {
        id: 0,
        name: format!("用户{index}"),
        username: format!("user{index}"),
        email: format!("user{index}@example.com"),
        age: 18 + (index % 30),
        status: if index % 2 == 0 { "Active" } else { "Disabled" }.to_string(),
        create_time: NaiveDate::from_ymd_opt(2024, 1, ((index % 28) + 1) as u32)
            .and_then(|d| d.and_hms_opt(8, 30, 0)),
    }
}

fn main() -> Result<(), RmdbError> {
    let dir = std::env::current_dir()
        .unwrap()
        .join("target")
        .join("example-data");
    let db = MemoryDb::with_options(DbOptions {
        base_dir: dir.clone(),
        min_write_interval: std::time::Duration::from_millis(200),
        ..DbOptions::default()
    });

    // 批量插入
    let ids = db.insert_batch((1..=100).map(user).collect())?;
    println!("插入 {} 行，首个 ID = {}", ids.len(), ids[0]);

    // 主键 / 唯一 / 范围 / 模糊 / 分页
    println!("ID 42：{}", db.find_by_id::<User>(42).unwrap().username);
    println!(
        "唯一查找 user7：ID {}",
        db.find_by_unique::<User>("Username", "user7").unwrap().id
    );
    println!(
        "年龄 20~22：{} 行",
        db.find_by_range::<User>("Age", 20, 22).len()
    );
    println!(
        "状态前缀 act*：{} 行",
        db.find_by_like::<User>("Status", "act*").len()
    );
    let page = db.find_by_page::<User>(Some("Age"), 0, 10, true);
    println!(
        "按年龄分页：第 {} 页 / 共 {} 页，当前 {} 行",
        page.page_index + 1,
        page.total_pages,
        page.items.len()
    );

    // 唯一约束冲突
    let mut duplicate = user(1);
    duplicate.username = "user1".into();
    match db.insert(duplicate) {
        Err(error) if error.is_unique_violation() => {
            println!("唯一约束生效：{error}");
        }
        other => panic!("应触发唯一约束：{other:?}"),
    }

    // 更新
    let mut first = (*db.find_by_id::<User>(1).unwrap()).clone();
    first.age = 60;
    db.update(first)?;
    println!("更新后年龄：{}", db.find_by_id::<User>(1).unwrap().age);

    // 复合唯一
    db.insert(Employee {
        id: 0,
        name: "张三".into(),
        company_id: 100,
        employee_number: "E001".into(),
        id_card_number: "ID-1".into(),
    })?;
    let dup = db.insert(Employee {
        id: 0,
        name: "李四".into(),
        company_id: 100,
        employee_number: "E001".into(),
        id_card_number: "ID-2".into(),
    });
    println!("复合唯一冲突：{}", dup.unwrap_err());
    let found = db
        .find_by_composite_unique::<Employee>(
            "CompanyUnique",
            &[
                ("CompanyId", FieldValue::Int(100)),
                ("EmployeeNumber", FieldValue::Text("E001".into())),
            ],
        )
        .expect("应命中复合键");
    println!("复合键命中：{}", found.name);

    // 同步落盘（后台线程也会在窗口内自动写）
    db.flush()?;
    println!(
        "数据文件：{}（{} 字节）",
        db.data_file::<User>().display(),
        std::fs::metadata(db.data_file::<User>()).map(|m| m.len()).unwrap_or(0)
    );
    let stats = db.statistics();
    println!(
        "统计：{} 张表 / {} 行 / 写盘 {} 次 / {} 字节",
        stats.table_count, stats.total_rows,
        stats.tables.iter().map(|t| t.persist_count).sum::<u64>(),
        stats.tables.iter().map(|t| t.persisted_bytes).sum::<u64>(),
    );
    Ok(())
}
