//! 并发：多读并发、读写混合与唯一约束竞争。

mod common;

use common::{user, User};
use std::sync::Arc;
use std::thread;

#[test]
fn concurrent_reads_are_consistent() {
    let db = Arc::new(common::memory_db());
    for index in 1..=200 {
        db.insert(user(index)).unwrap();
    }

    let mut handles = Vec::new();
    for _ in 0..8 {
        let db = Arc::clone(&db);
        handles.push(thread::spawn(move || {
            for index in 1..=200 {
                assert!(db.find_by_id::<User>(index).is_some());
                assert_eq!(db.find_by::<User>("Status", if index % 2 == 0 { "Active" } else { "Disabled" }).len(), 100);
            }
            for index in 1..=100 {
                assert!(db.find_by_unique::<User>("Username", format!("user{index}")).is_some());
            }
        }));
    }
    for handle in handles {
        handle.join().unwrap();
    }
}

#[test]
fn concurrent_writers_do_not_lose_rows() {
    let db = Arc::new(common::memory_db());
    let mut handles = Vec::new();
    for worker in 0..4 {
        let db = Arc::clone(&db);
        handles.push(thread::spawn(move || {
            for index in 0..100 {
                let mut entity = user(worker * 100 + index + 1);
                entity.username = format!("w{worker}-{index}");
                entity.email = format!("w{worker}-{index}@example.com");
                db.insert(entity).unwrap();
            }
        }));
    }
    for handle in handles {
        handle.join().unwrap();
    }
    assert_eq!(db.find_all::<User>().len(), 400);
    // ID 唯一且连续（1..=400）
    let mut ids: Vec<i64> = db.find_all::<User>().iter().map(|item| item.id).collect();
    ids.sort_unstable();
    assert_eq!(ids, (1..=400).collect::<Vec<i64>>());
}

#[test]
fn concurrent_unique_conflict_has_exactly_one_winner() {
    let db = Arc::new(common::memory_db());
    let mut handles = Vec::new();
    for worker in 0..8 {
        let db = Arc::clone(&db);
        handles.push(thread::spawn(move || {
            let mut entity = user(worker + 1);
            entity.username = "same-name".into();
            entity.email = format!("same{worker}@example.com");
            db.insert(entity).is_ok()
        }));
    }
    let winners = handles
        .into_iter()
        .filter(|_| true)
        .map(|handle| handle.join().unwrap())
        .filter(|ok| *ok)
        .count();
    assert_eq!(winners, 1, "并发写同一唯一值只能有一个成功");
    assert_eq!(db.find_all::<User>().len(), 1);
}

#[test]
fn read_write_mix_stays_consistent() {
    let db = Arc::new(common::memory_db());
    for index in 1..=50 {
        db.insert(user(index)).unwrap();
    }

    let writer = {
        let db = Arc::clone(&db);
        thread::spawn(move || {
            for index in 51..=150 {
                db.insert(user(index)).unwrap();
            }
        })
    };
    let readers: Vec<_> = (0..4)
        .map(|_| {
            let db = Arc::clone(&db);
            thread::spawn(move || {
                for _ in 0..500 {
                    let all = db.find_all::<User>();
                    // 快照内部一致：每行都能按 ID 查回
                    for item in all.iter().take(10) {
                        assert!(db.find_by_id::<User>(item.id).is_some());
                    }
                }
            })
        })
        .collect();

    writer.join().unwrap();
    for reader in readers {
        reader.join().unwrap();
    }
    assert_eq!(db.find_all::<User>().len(), 150);
}
