//! 基础 CRUD、分页与统计。

mod common;

use common::{user, User};
use pek_rmdb::{FieldValue, RmdbError};

#[test]
fn insert_assigns_sequential_ids_and_find_by_id() {
    let db = common::memory_db();
    let id1 = db.insert(user(1)).unwrap();
    let id2 = db.insert(user(2)).unwrap();
    assert_eq!((id1, id2), (1, 2));

    let found = db.find_by_id::<User>(id1).unwrap();
    assert_eq!(found.id, id1);
    assert_eq!(found.username, "user1");
    assert_eq!(found.name, "用户1");
    assert!(db.find_by_id::<User>(999).is_none());
}

#[test]
fn insert_overwrites_preexisting_id() {
    let db = common::memory_db();
    let mut entity = user(1);
    entity.id = 500; // C# 版同样会重新分配
    let id = db.insert(entity).unwrap();
    assert_eq!(id, 1);
    assert!(db.find_by_id::<User>(500).is_none());
}

#[test]
fn update_replaces_object_and_indexes() {
    let db = common::memory_db();
    let id = db.insert(user(1)).unwrap();

    let mut entity = (*db.find_by_id::<User>(id).unwrap()).clone();
    entity.username = "renamed".into();
    entity.email = "renamed@example.com".into();
    entity.age = 99;
    db.update(entity).unwrap();

    assert!(db.find_by_unique::<User>("Username", "user1").is_none());
    assert_eq!(
        db.find_by_unique::<User>("Username", "renamed")
            .unwrap()
            .id,
        id
    );
    assert_eq!(db.find_by::<User>("Age", 99).len(), 1);
    assert!(db.find_by::<User>("Age", 19).is_empty());
    assert_eq!(db.find_by_id::<User>(id).unwrap().username, "renamed");
}

#[test]
fn update_missing_returns_not_found() {
    let db = common::memory_db();
    let mut entity = user(1);
    entity.id = 42;
    let error = db.update(entity).unwrap_err();
    assert!(matches!(error, RmdbError::NotFound(_)), "实际：{error}");
}

#[test]
fn delete_removes_from_all_indexes() {
    let db = common::memory_db();
    let id = db.insert(user(1)).unwrap();
    assert!(db.delete::<User>(id));
    assert!(!db.delete::<User>(id), "重复删除返回 false");

    assert!(db.find_by_id::<User>(id).is_none());
    assert!(db.find_by::<User>("Age", 19).is_empty());
    assert!(db.find_by_unique::<User>("Username", "user1").is_none());
    assert!(db.find_all::<User>().is_empty());

    // 唯一值被释放，可以重新使用
    let again = db.insert(user(1)).unwrap();
    assert_eq!(again, 2, "删除后 ID 不复用，新行拿下一个自增 ID");
    assert_eq!(db.find_by_unique::<User>("Username", "user1").unwrap().id, again);
}

#[test]
fn find_all_is_sorted_by_id() {
    let db = common::memory_db();
    for index in 1..=5 {
        db.insert(user(index)).unwrap();
    }
    let all = db.find_all::<User>();
    let ids: Vec<i64> = all.iter().map(|item| item.id).collect();
    assert_eq!(ids, vec![1, 2, 3, 4, 5]);
}

#[test]
fn find_by_name_and_id_work_without_declaring_them_in_fields() {
    let db = common::memory_db();
    let id = db.insert(user(1)).unwrap();
    assert_eq!(db.find_by::<User>("Name", "用户1").len(), 1);
    assert_eq!(db.find_by::<User>("Id", id).len(), 1);
}

#[test]
fn pagination_with_ordered_index() {
    let db = common::memory_db();
    for index in 1..=25 {
        db.insert(user(index)).unwrap();
    }

    let page = db.find_by_page::<User>(Some("Age"), 0, 10, true);
    assert_eq!(page.total_count, 25);
    assert_eq!(page.total_pages, 3);
    assert_eq!(page.items.len(), 10);
    assert!(page.items.windows(2).all(|pair| pair[0].age <= pair[1].age));

    let last = db.find_by_page::<User>(Some("Age"), 2, 10, true);
    assert_eq!(last.items.len(), 5);

    let desc = db.find_by_page::<User>(Some("Age"), 0, 5, false);
    assert_eq!(desc.items.len(), 5);
    assert!(desc.items.windows(2).all(|pair| pair[0].age >= pair[1].age));

    // 无排序字段：按 ID 升序
    let plain = db.find_by_page::<User>(None, 0, 3, true);
    let ids: Vec<i64> = plain.items.iter().map(|item| item.id).collect();
    assert_eq!(ids, vec![1, 2, 3]);
}

#[test]
fn pagination_falls_back_to_full_sort_when_index_incomplete() {
    let db = common::memory_db();
    // 缺少 CreateTime 的行不会进入时间索引（Null 不建索引），此时分页退回全量排序
    let mut missing = user(1);
    missing.create_time = None;
    db.insert(missing).unwrap();
    for index in 2..=5 {
        db.insert(user(index)).unwrap();
    }

    let page = db.find_by_page::<User>(Some("CreateTime"), 0, 10, true);
    assert_eq!(page.total_count, 5);
    assert_eq!(page.items.len(), 5);
    assert_eq!(page.items[0].id, 1, "空值排最前");

    // 索引完整时按值顺序（Active 在 Disabled 之前，同值按 ID）
    let page = db.find_by_page::<User>(Some("Status"), 0, 10, true);
    let ids: Vec<i64> = page.items.iter().map(|item| item.id).collect();
    assert_eq!(ids, vec![2, 4, 1, 3, 5]);
}

#[test]
fn find_by_multiple_intersects_conditions() {
    let db = common::memory_db();
    for index in 1..=6 {
        db.insert(user(index)).unwrap();
    }
    let results = db.find_by_multiple::<User>(&[
        ("Status", FieldValue::Text("Active".into())),
        ("Age", FieldValue::Int(20)),
    ]);
    let ids: Vec<i64> = results.iter().map(|item| item.id).collect();
    // index%2==0 与 age==20（18 + index%30）同时满足：index=2（20）、index=4（22 否）……
    assert_eq!(ids, vec![2]);

    assert!(db
        .find_by_multiple::<User>(&[("Status", FieldValue::Text("Nope".into()))])
        .is_empty());
    assert!(db.find_by_multiple::<User>(&[]).is_empty());
}

#[test]
fn insert_by_index_only_builds_given_indexes() {
    let db = common::memory_db();
    let id = db.insert_by_index(user(1), &["Status"]).unwrap();
    assert_eq!(id, 1);
    assert_eq!(db.find_by::<User>("Status", "Disabled").len(), 1);
    // Age 未建索引
    assert!(db.find_by::<User>("Age", 19).is_empty());
    // 主键仍可查
    assert!(db.find_by_id::<User>(id).is_some());
}

#[test]
fn insert_batch_assigns_ids_and_indexes() {
    let db = common::memory_db();
    let objects: Vec<User> = (1..=10).map(user).collect();
    let ids = db.insert_batch(objects).unwrap();
    assert_eq!(ids, (1..=10).collect::<Vec<i64>>());
    assert_eq!(db.find_all::<User>().len(), 10);
    assert_eq!(db.find_by::<User>("Status", "Active").len(), 5);

    // 空批次
    assert!(db.insert_batch(Vec::<User>::new()).unwrap().is_empty());
}

#[test]
fn clear_resets_data_and_ids() {
    let db = common::memory_db();
    db.insert(user(1)).unwrap();
    db.insert(user(2)).unwrap();
    db.clear::<User>();
    assert!(db.find_all::<User>().is_empty());
    assert_eq!(db.insert(user(3)).unwrap(), 1);
}

#[test]
fn statistics_report_rows_indexes_and_unique() {
    let db = common::memory_db();
    db.insert(user(1)).unwrap();
    db.insert(user(2)).unwrap();

    let stats = db.statistics();
    assert_eq!(stats.table_count, 1);
    assert_eq!(stats.total_rows, 2);
    let table = &stats.tables[0];
    assert_eq!(table.type_name, "Test.User");
    assert_eq!(table.rows, 2);
    assert_eq!(table.unique_fields, 3);
    assert_eq!(table.unique_composites, 0);
    assert!(!table.in_memory);
    let age = table
        .indexes
        .iter()
        .find(|index| index.property_name == "Age")
        .expect("Age 索引");
    assert_eq!(age.index_type, "NumericIndex");
    assert_eq!(age.id_count, 2);
}

#[test]
fn not_save_entity_stays_in_memory() {
    let (_dir, db) = common::temp_db();
    use common::Setting;
    db.insert(Setting {
        id: 0,
        name: "s".into(),
        value: "v".into(),
    })
    .unwrap();
    db.flush().unwrap();
    assert!(!db.data_file::<Setting>().exists(), "NotSave 实体不落盘");
    assert_eq!(db.find_all::<Setting>().len(), 1);
    let stats = db.statistics();
    assert!(stats.tables.iter().any(|table| table.in_memory));
}
