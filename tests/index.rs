//! 类型感知索引：精确/范围/模糊/布尔/时间/JSON/多条件。

mod common;

use chrono::NaiveDate;
use common::{user, User};
use pek_rmdb::FieldValue;

fn seed(count: i64) -> pek_rmdb::MemoryDb {
    let db = common::memory_db();
    for index in 1..=count {
        db.insert(user(index)).unwrap();
    }
    db
}

#[test]
fn exact_text_is_case_sensitive_but_like_is_not() {
    let db = seed(4);
    assert_eq!(db.find_by::<User>("Status", "Active").len(), 2);
    assert_eq!(
        db.find_by::<User>("Status", "active").len(),
        0,
        "精确查找区分大小写（对齐 C# StringIndex 基索引）"
    );
    assert_eq!(db.find_by_like::<User>("Status", "active").len(), 2);
    assert_eq!(db.find_by_like::<User>("Status", "Act*").len(), 2);
    assert_eq!(db.find_by_like::<User>("Status", "*tive").len(), 2);
    assert_eq!(db.find_by_like::<User>("Status", "*cti*").len(), 2);
    assert_eq!(db.find_by_like::<User>("Status", "A*e").len(), 2);
    assert_eq!(db.find_by_like::<User>("Status", "D*").len(), 2);
    assert!(db.find_by_like::<User>("Status", "Nope*").is_empty());
}

#[test]
fn text_range_is_lexicographic_case_insensitive() {
    let db = seed(5);
    // user1..user5（单字符差异，字典序与数值序一致）
    let names: Vec<String> = db
        .find_by_range::<User>("Username", "user2", "user4")
        .into_iter()
        .map(|item| item.username.clone())
        .collect();
    let mut names = names;
    names.sort();
    assert_eq!(names, vec!["user2", "user3", "user4"]);

    // 大小写不敏感范围
    let names = db.find_by_range::<User>("Username", "USER2", "USER4");
    assert_eq!(names.len(), 3);
}

#[test]
fn numeric_range_and_cross_type_query() {
    let db = seed(10);
    // age = 18 + index%30 → 19..28
    let mut ages: Vec<i64> = db
        .find_by_range::<User>("Age", 20, 22)
        .into_iter()
        .map(|item| item.age)
        .collect();
    ages.sort_unstable();
    assert_eq!(ages, vec![20, 21, 22]);

    // 字符串数字也能命中整数索引（按字段类型规范化）
    assert_eq!(db.find_by_range::<User>("Age", "20", "22").len(), 3);
    // 浮点查询整数范围
    assert_eq!(
        db.find_by::<User>("Age", FieldValue::Float(20.0)).len(),
        1
    );
    // 无结果
    assert!(db.find_by_range::<User>("Age", 100, 200).is_empty());
    assert!(db.find_by_range::<User>("Age", 200, 100).is_empty());
}

#[test]
fn time_range_and_year_month_day_patterns() {
    let db = seed(5);
    // user(1) → 2024-01-02，user(2) → 01-03，……
    let start = NaiveDate::from_ymd_opt(2024, 1, 3)
        .unwrap()
        .and_hms_opt(0, 0, 0)
        .unwrap();
    let end = NaiveDate::from_ymd_opt(2024, 1, 4)
        .unwrap()
        .and_hms_opt(23, 59, 59)
        .unwrap();
    let ids: Vec<i64> = db
        .find_by_range::<User>("CreateTime", start, end)
        .into_iter()
        .map(|item| item.id)
        .collect();
    assert_eq!(ids, vec![2, 3]);

    assert_eq!(db.find_by_like::<User>("CreateTime", "2024").len(), 5);
    assert_eq!(db.find_by_like::<User>("CreateTime", "2024-01").len(), 5);
    assert_eq!(db.find_by_like::<User>("CreateTime", "2024-01-03").len(), 1);
    assert_eq!(db.find_by_like::<User>("CreateTime", "2023").len(), 0);
    // 字符串时间也能查询
    assert_eq!(
        db.find_by::<User>("CreateTime", "2024-01-03 08:30:00").len(),
        1
    );
}

#[test]
fn boolean_patterns_are_smart() {
    let db = seed(4);
    assert_eq!(db.find_by::<User>("Enabled", true).len(), 2);
    assert_eq!(db.find_by_like::<User>("Enabled", "true").len(), 2);
    assert_eq!(db.find_by_like::<User>("Enabled", "yes").len(), 2);
    assert_eq!(db.find_by_like::<User>("Enabled", "1").len(), 2);
    assert_eq!(db.find_by_like::<User>("Enabled", "false").len(), 2);
    assert_eq!(db.find_by_like::<User>("Enabled", "否").len(), 2);
    assert!(db.find_by_like::<User>("Enabled", "maybe").is_empty());
}

#[test]
fn json_field_indexes_by_text_form() {
    let db = seed(3);
    let tags = ["tag".to_string(), "tag2".to_string()];
    let found = db.find_by::<User>("Tags", FieldValue::List(tags.iter().cloned().map(FieldValue::Text).collect::<Vec<_>>()));
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].id, 2);
}

#[test]
fn not_indexed_field_returns_empty() {
    let db = seed(2);
    assert!(db.find_by::<User>("Remark", "备注1").is_empty());
    // 但对象本身可读
    assert_eq!(db.find_by_id::<User>(1).unwrap().remark, "备注1");
}

#[test]
fn unknown_field_returns_empty() {
    let db = seed(2);
    assert!(db.find_by::<User>("NotExists", 1).is_empty());
    assert!(db.find_by_range::<User>("NotExists", 1, 2).is_empty());
    assert!(db.find_by_like::<User>("NotExists", "*").is_empty());
    assert!(db.find_by_unique::<User>("NotExists", 1).is_none());
    assert!(!db.is_unique_value_exists::<User>("NotExists", 1, None));
}

#[test]
fn index_statistics_count_queries_and_hits() {
    let db = seed(3);
    let _ = db.find_by::<User>("Age", 19);
    let _ = db.find_by::<User>("Age", 19);
    let _ = db.find_by::<User>("Age", 999);

    let stats = db.statistics();
    let age = stats.tables[0]
        .indexes
        .iter()
        .find(|index| index.property_name == "Age")
        .unwrap();
    assert_eq!(age.query_count, 3);
    assert_eq!(age.hit_count, 2);
    assert!((age.hit_rate() - 2.0 / 3.0).abs() < 1e-9);
}
