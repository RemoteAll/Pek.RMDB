//! 持久化：C# 格式兼容、重启恢复、ID 计数器修复、写合并、原子写、NotSave。

mod common;

use common::{user, User};
use pek_rmdb::{DbOptions, MemoryDb};
use std::time::Duration;

fn write_file(path: &std::path::Path, content: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

#[test]
fn background_persist_writes_csharp_compatible_file() {
    let (_dir, db) = common::temp_db();
    db.insert(user(1)).unwrap();
    let path = db.data_file::<User>();
    assert!(common::wait_file(&path), "后台线程应写出数据文件");
    // 等待内容落稳
    std::thread::sleep(Duration::from_millis(50));

    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("Id:1"), "裸键格式（C# 兼容）：{text}");
    assert!(text.contains("Name:\"用户1\""), "{text}");
    assert!(text.contains("CreateTime:\"2024-01-02 08:30:00\""), "{text}");
    assert!(!text.contains("\"Id\":"), "不应输出标准 JSON 带引号键：{text}");
    assert!(!text.contains("Cache"), "NotSave 字段不落盘：{text}");
    assert!(text.starts_with("[\r\n"), "{text}");
}

#[test]
fn reload_restores_data_indexes_and_unique_indexes() {
    let (dir, db) = common::temp_db();
    let id = db.insert(user(1)).unwrap();
    db.insert(user(2)).unwrap();
    db.flush().unwrap();
    drop(db);

    let db2 = MemoryDb::new(dir.path());
    assert_eq!(db2.find_all::<User>().len(), 2);
    assert_eq!(db2.find_by_id::<User>(id).unwrap().username, "user1");
    // 普通索引恢复
    assert_eq!(db2.find_by::<User>("Status", "Active").len(), 1);
    assert_eq!(db2.find_by_range::<User>("Age", 18, 100).len(), 2);
    // 唯一索引恢复
    assert_eq!(
        db2.find_by_unique::<User>("Username", "user1")
            .unwrap()
            .id,
        id
    );
    // 唯一约束仍然生效
    let mut duplicate = user(9);
    duplicate.username = "user1".into();
    assert!(db2.insert(duplicate).is_err());
    // NotSave 字段不恢复（默认值）
    assert!(db2.find_by_id::<User>(id).unwrap().cache.is_empty());
}

#[test]
fn reload_fixes_id_counter_to_max_plus_one() {
    let (dir, db) = common::temp_db();
    for index in 1..=5 {
        db.insert(user(index)).unwrap();
    }
    db.flush().unwrap();
    drop(db);

    let db2 = MemoryDb::new(dir.path());
    let id = db2.insert(user(6)).unwrap();
    assert_eq!(id, 6, "加载后新增应延续现有最大 ID（C# 版会从 1 覆盖）");
    assert_eq!(db2.find_all::<User>().len(), 6);
    assert_eq!(db2.find_by_id::<User>(1).unwrap().username, "user1");
}

#[test]
fn reads_csharp_legacy_file() {
    let dir = tempfile::tempdir().unwrap();
    let db = MemoryDb::new(dir.path());
    let path = db.data_file::<User>();
    write_file(
        &path,
        r#"// C# SimpleJsonString 风格（裸键 + 注释 + 斜杠日期）
[
	{ Id:7, Name:"老数据", Username:"old", Email:"old@example.com", Age:30, Status:"Active", Enabled:true, Tags:[ "a", "b" ], CreateTime:"2024/1/5 14:30:00" }
]
"#,
    );

    let found = db.find_by_id::<User>(7).expect("应能读取 C# 文件");
    assert_eq!(found.name, "老数据");
    assert_eq!(found.age, 30);
    assert!(found.enabled);
    assert_eq!(found.tags, vec!["a".to_string(), "b".to_string()]);
    let time = found.create_time.unwrap();
    assert_eq!(time.format("%Y-%m-%d %H:%M:%S").to_string(), "2024-01-05 14:30:00");
    assert_eq!(db.find_by_unique::<User>("Username", "old").unwrap().id, 7);

    // 新增行 ID 从 8 开始
    assert_eq!(db.insert(user(8)).unwrap(), 8);
}

#[test]
fn duplicated_unique_rows_are_skipped_on_load() {
    let dir = tempfile::tempdir().unwrap();
    let db = MemoryDb::new(dir.path());
    let path = db.data_file::<User>();
    write_file(
        &path,
        r#"[
	{ Id:1, Name:"a", Username:"dup", Email:"a@example.com", Age:20, Status:"Active", Enabled:true, CreateTime:"2024-01-01 00:00:00" },
	{ Id:2, Name:"b", Username:"dup", Email:"b@example.com", Age:20, Status:"Active", Enabled:true, CreateTime:"2024-01-02 00:00:00" }
]
"#,
    );

    let all = db.find_all::<User>();
    assert_eq!(all.len(), 1, "唯一冲突的第二行应跳过");
    assert_eq!(all[0].id, 1);
    assert_eq!(db.find_by_unique::<User>("Username", "dup").unwrap().id, 1);
    // 冲突对象未进入普通索引
    assert_eq!(db.find_by_id::<User>(2), None);
}

#[test]
fn manual_flush_works_without_background_thread() {
    let dir = tempfile::tempdir().unwrap();
    let db = MemoryDb::with_options(DbOptions {
        base_dir: dir.path().to_path_buf(),
        auto_persist: false,
        min_write_interval: Duration::ZERO,
        pretty: true,
        flush_on_drop: false,
        on_error: None,
    });
    db.insert(user(1)).unwrap();
    assert!(!db.data_file::<User>().exists());
    db.flush().unwrap();
    assert!(db.data_file::<User>().exists());
}

#[test]
fn write_merging_limits_persist_count() {
    let (_dir, db) = common::temp_db();
    for index in 1..=100 {
        db.insert(user(index)).unwrap();
    }
    // 等待合并窗口
    std::thread::sleep(Duration::from_millis(150));
    db.flush().unwrap();
    let stats = db.statistics();
    let count = stats.tables[0].persist_count;
    assert!(count >= 1, "至少应有 1 次写盘");
    assert!(count < 100, "写合并应显著减少写盘次数，实际 {count}");

    // 重启后数据完整
    let text = std::fs::read_to_string(db.data_file::<User>()).unwrap();
    assert!(text.contains("Id:100"), "最后一次修改必须落盘");
}

#[test]
fn atomic_write_leaves_no_temp_files() {
    let (_dir, db) = common::temp_db();
    for index in 1..=10 {
        db.insert(user(index)).unwrap();
    }
    db.flush().unwrap();
    let data_dir = db.data_file::<User>().parent().unwrap().to_path_buf();
    let leftovers: Vec<String> = std::fs::read_dir(&data_dir)
        .unwrap()
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".tmp"))
        .collect();
    assert!(leftovers.is_empty(), "残留临时文件：{leftovers:?}");
}

#[test]
fn drop_flushes_pending_changes() {
    let dir = tempfile::tempdir().unwrap();
    {
        let db = MemoryDb::new(dir.path());
        db.insert(user(1)).unwrap();
        // 不显式 flush，依赖 Drop
    }
    let db2 = MemoryDb::new(dir.path());
    assert_eq!(db2.find_all::<User>().len(), 1);
}

#[test]
fn corrupted_json_is_reported_but_service_stays_usable() {
    let dir = tempfile::tempdir().unwrap();
    let db = MemoryDb::new(dir.path());
    let path = db.data_file::<User>();
    write_file(&path, "{ this is not json");

    // 加载失败不 panic，表为空但可继续使用
    assert!(db.find_all::<User>().is_empty());
    assert_eq!(db.insert(user(1)).unwrap(), 1);
}

#[test]
fn in_memory_db_writes_nothing() {
    let db = MemoryDb::in_memory();
    db.insert(user(1)).unwrap();
    db.flush().unwrap();
    assert!(db.base_dir().as_os_str().is_empty());
    assert_eq!(db.find_all::<User>().len(), 1);
}
