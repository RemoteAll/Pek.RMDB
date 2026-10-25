//! 唯一约束：单字段、复合、空值语义、更新排除自身、批量与查找。

mod common;

use common::{user, Employee, User};
use pek_rmdb::{ConstraintType, FieldValue};

fn employee(company: i64, number: &str, id_card: &str) -> Employee {
    Employee {
        id: 0,
        name: format!("员工{number}"),
        company_id: company,
        employee_number: number.to_string(),
        id_card_number: id_card.to_string(),
        full_name: format!("姓名{number}"),
    }
}

#[test]
fn single_field_unique_rejects_duplicates() {
    let db = common::memory_db();
    db.insert(user(1)).unwrap();

    let mut duplicate = user(2);
    duplicate.username = "user1".into();
    let error = db.insert(duplicate).unwrap_err();
    let violation = error.unique().expect("应为唯一约束错误");
    assert_eq!(violation.constraint_type, ConstraintType::Single);
    assert_eq!(violation.property_name, "Username");
    assert_eq!(violation.conflicting_id, Some(1));
    assert!(violation.message.contains("用户名已存在"));
    assert_eq!(db.find_all::<User>().len(), 1, "冲突插入不应落地");
}

#[test]
fn nullable_unique_allows_multiple_nulls() {
    let db = common::memory_db();
    db.insert(user(1)).unwrap(); // phone 空
    db.insert(user(2)).unwrap(); // phone 空
    assert_eq!(db.find_all::<User>().len(), 2);
}

#[test]
fn required_unique_rejects_null() {
    let db = common::memory_db();
    let mut entity = user(1);
    entity.email = String::new(); // 空串 → Null
    let error = db.insert(entity).unwrap_err();
    let violation = error.unique().unwrap();
    assert_eq!(violation.property_name, "Email");
    assert_eq!(violation.constraint_type, ConstraintType::Single);
    assert_eq!(violation.conflicting_id, None);
    assert!(violation.message.contains("不允许为空值"));
    assert!(db.find_all::<User>().is_empty());
}

#[test]
fn find_by_unique_and_exists() {
    let db = common::memory_db();
    let id = db.insert(user(1)).unwrap();
    assert_eq!(db.find_by_unique::<User>("Email", "user1@example.com").unwrap().id, id);
    assert!(db.find_by_unique::<User>("Email", "none@example.com").is_none());

    assert!(db.is_unique_value_exists::<User>("Username", "user1", None));
    assert!(!db.is_unique_value_exists::<User>("Username", "user1", Some(id)));
    assert!(db.is_unique_value_exists::<User>("Username", "user1", Some(id + 1)));
    assert!(!db.is_unique_value_exists::<User>("Username", "other", None));
}

#[test]
fn composite_unique_checks_combination() {
    let db = common::memory_db();
    db.insert(employee(100, "E001", "ID-1")).unwrap();

    // 同公司同工号 → 冲突
    let error = db.insert(employee(100, "E001", "ID-2")).unwrap_err();
    let violation = error.unique().unwrap();
    assert_eq!(violation.constraint_type, ConstraintType::Composite);
    assert_eq!(violation.property_name, "CompanyId, EmployeeNumber");
    assert!(violation.message.contains("同一公司内工号必须唯一"));

    // 同公司不同工号 → 允许
    db.insert(employee(100, "E002", "ID-3")).unwrap();
    // 不同公司同工号 → 允许
    db.insert(employee(200, "E001", "ID-4")).unwrap();

    assert_eq!(db.find_all::<Employee>().len(), 3);

    // 复合查找（值顺序无关）
    let found = db
        .find_by_composite_unique::<Employee>(
            "CompanyUnique",
            &[
                ("EmployeeNumber", FieldValue::Text("E001".into())),
                ("CompanyId", FieldValue::Int(200)),
            ],
        )
        .expect("应能按复合键找到");
    assert_eq!(found.id_card_number, "ID-4");
    assert!(db
        .find_by_composite_unique::<Employee>(
            "UnknownGroup",
            &[("CompanyId", FieldValue::Int(200))]
        )
        .is_none());
}

#[test]
fn update_excludes_self_but_detects_others() {
    let db = common::memory_db();
    let id1 = db.insert(user(1)).unwrap();
    let id2 = db.insert(user(2)).unwrap();

    // 非唯一字段更新 + 唯一字段不变 → 成功
    let mut first = (*db.find_by_id::<User>(id1).unwrap()).clone();
    first.age = 50;
    db.update(first.clone()).unwrap();

    // 改成 user2 的邮箱 → 冲突
    let mut conflict = first.clone();
    conflict.email = "user2@example.com".into();
    let error = db.update(conflict).unwrap_err();
    assert_eq!(error.unique().unwrap().conflicting_id, Some(id2));

    // 原对象与索引保持不变
    let reloaded = db.find_by_id::<User>(id1).unwrap();
    assert_eq!(reloaded.email, "user1@example.com");
    assert_eq!(db.find_by_unique::<User>("Email", "user1@example.com").unwrap().id, id1);
    assert_eq!(db.find_by::<User>("Age", 50).len(), 1);
}

#[test]
fn delete_releases_unique_value() {
    let db = common::memory_db();
    let id = db.insert(user(1)).unwrap();
    assert!(db.delete::<User>(id));
    assert!(!db.is_unique_value_exists::<User>("Username", "user1", None));
    db.insert(user(1)).unwrap(); // 可再次使用同一唯一值
}

#[test]
fn batch_conflict_keeps_previous_rows() {
    let db = common::memory_db();
    let mut duplicate = user(2);
    duplicate.username = "user1".into();
    let error = db.insert_batch(vec![user(1), duplicate, user(3)]).unwrap_err();
    assert!(error.is_unique_violation());
    // 与 C# 一致：批量非事务，冲突前的行保留
    assert_eq!(db.find_all::<User>().len(), 1);
    assert_eq!(db.find_by_unique::<User>("Username", "user1").unwrap().id, 1);
}

#[test]
fn normalized_values_compare_across_numeric_types() {
    let db = common::memory_db();
    db.insert(employee(100, "E001", "ID-1")).unwrap();
    // i32 查询 i64 字段
    let found = db.find_by_composite_unique::<Employee>(
        "CompanyUnique",
        &[
            ("CompanyId", FieldValue::Int(100)),
            ("EmployeeNumber", FieldValue::Text("E001".into())),
        ],
    );
    assert!(found.is_some());
}
