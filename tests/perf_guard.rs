//! 性能基准（默认忽略；发版前用 release 跑一次，数据写入 `docs/审计报告.md`）。
//!
//! ```powershell
//! cargo test --release --test perf_guard -- --ignored --nocapture --test-threads=1
//! ```

mod common;

use std::sync::Arc;
use std::time::Instant;

use common::{user, User};

fn ms(start: Instant) -> f64 {
    start.elapsed().as_secs_f64() * 1000.0
}

#[test]
#[ignore]
fn bench_hot_paths() {
    const N: i64 = 100_000;
    let db = common::memory_db();

    // 1. 批量插入（每批 1000 行）
    let start = Instant::now();
    for chunk in 0..(N / 1000) {
        let batch: Vec<User> = ((chunk * 1000 + 1)..=(chunk + 1) * 1000).map(user).collect();
        db.insert_batch(batch).unwrap();
    }
    let elapsed = ms(start);
    println!(
        "插入 {N} 行：{elapsed:.1} ms（{:.0} 行/秒，{:.0} ns/行）",
        N as f64 / (elapsed / 1000.0),
        elapsed * 1e6 / N as f64
    );

    // 2. 按 ID 查找
    let iterations = 100_000;
    let start = Instant::now();
    let mut hits = 0usize;
    for i in 0..iterations {
        if db.find_by_id::<User>((i % N) + 1).is_some() {
            hits += 1;
        }
    }
    let elapsed = ms(start);
    println!(
        "find_by_id {iterations} 次：{elapsed:.1} ms（{:.0} ns/次，命中 {hits}）",
        elapsed * 1e6 / iterations as f64
    );

    // 3. 唯一索引查找
    let start = Instant::now();
    for i in 0..iterations {
        let name = format!("user{}", (i % N) + 1);
        assert!(db.find_by_unique::<User>("Username", name).is_some());
    }
    let elapsed = ms(start);
    println!(
        "find_by_unique {iterations} 次：{elapsed:.1} ms（{:.0} ns/次，含字符串构造）",
        elapsed * 1e6 / iterations as f64
    );

    // 4. 字段精确查找（Age 只有 30 个取值）
    let start = Instant::now();
    let mut total = 0usize;
    for i in 0..10_000 {
        total += db.find_by::<User>("Age", 18 + (i % 30)).len();
    }
    let elapsed = ms(start);
    println!(
        "find_by(Age) 1 万次：{elapsed:.1} ms（{:.0} ns/次，平均返回 {:.0} 行）",
        elapsed * 1e6 / 10_000.0,
        total as f64 / 10_000.0
    );

    // 5. 范围查询
    let start = Instant::now();
    let mut rows = 0usize;
    for _ in 0..1_000 {
        rows += db.find_by_range::<User>("Age", 20, 24).len();
    }
    let elapsed = ms(start);
    println!(
        "find_by_range 1 千次：{elapsed:.1} ms（{:.0} ns/次，平均返回 {:.0} 行）",
        elapsed * 1e6 / 1_000.0,
        rows as f64 / 1_000.0
    );

    // 6. 分页（走索引顺序，不排序）
    let start = Instant::now();
    for page in 0..1_000 {
        let result = db.find_by_page::<User>(Some("Age"), page % 100, 50, true);
        assert_eq!(result.items.len(), 50);
    }
    let elapsed = ms(start);
    println!(
        "find_by_page(索引) 1 千次：{elapsed:.1} ms（{:.0} μs/次）",
        elapsed * 1e3 / 1_000.0
    );

    // 6.1 无索引字段分页（回退：partial select）
    let start = Instant::now();
    for page in 0..100 {
        let result = db.find_by_page::<User>(Some("Remark"), page % 10, 100, true);
        assert_eq!(result.items.len(), 100);
    }
    let elapsed = ms(start);
    println!(
        "find_by_page(无索引字段) 100 次：{elapsed:.1} ms（{:.0} μs/次）",
        elapsed * 1e3 / 100.0
    );

    // 6.2 无排序字段分页（按 ID）
    let start = Instant::now();
    for page in 0..1_000 {
        let result = db.find_by_page::<User>(None, page % 100, 50, true);
        assert_eq!(result.items.len(), 50);
    }
    let elapsed = ms(start);
    println!(
        "find_by_page(无排序字段) 1 千次：{elapsed:.1} ms（{:.0} μs/次）",
        elapsed * 1e3 / 1_000.0
    );

    // 7. 更新 1 万行（索引全量重建）
    let start = Instant::now();
    for i in 0..10_000 {
        let mut entity = (*db.find_by_id::<User>((i % N) + 1).unwrap()).clone();
        entity.age = (entity.age + 1) % 60;
        db.update(entity).unwrap();
    }
    let elapsed = ms(start);
    println!(
        "update 1 万行：{elapsed:.1} ms（{:.0} ns/行，含读取/克隆/索引重建）",
        elapsed * 1e6 / 10_000.0
    );

    // 8. 并发读（8 线程 × 10 万次）
    let db = Arc::new(db);
    let start = Instant::now();
    let mut handles = Vec::new();
    for _ in 0..8 {
        let db = Arc::clone(&db);
        handles.push(std::thread::spawn(move || {
            let mut hits = 0usize;
            for i in 0..100_000i64 {
                if db.find_by_id::<User>((i % N) + 1).is_some() {
                    hits += 1;
                }
            }
            hits
        }));
    }
    let hits: usize = handles.into_iter().map(|h| h.join().unwrap()).sum();
    let elapsed = ms(start);
    println!(
        "并发 find_by_id 8 线程 × 10 万次：{elapsed:.1} ms（总 {:.0} 万次/秒，命中 {hits}）",
        800_000.0 / (elapsed / 1000.0) / 10_000.0
    );

    // 统计快照
    let stats = db.statistics();
    println!(
        "表数 {}，总行数 {}，索引数 {}",
        stats.table_count,
        stats.total_rows,
        stats.tables.iter().map(|t| t.indexes.len()).sum::<usize>()
    );
}

/// 全量快照写盘基准（10 万行；用于审计报告的资源数据）。
#[test]
#[ignore]
fn bench_persist_snapshot() {
    const N: i64 = 100_000;
    let dir = tempfile::tempdir().unwrap();
    let db = pek_rmdb::MemoryDb::with_options(pek_rmdb::DbOptions {
        base_dir: dir.path().to_path_buf(),
        auto_persist: false,
        min_write_interval: std::time::Duration::ZERO,
        pretty: true,
        flush_on_drop: false,
        on_error: None,
    });
    for chunk in 0..(N / 1000) {
        let batch: Vec<User> = ((chunk * 1000 + 1)..=(chunk + 1) * 1000).map(user).collect();
        db.insert_batch(batch).unwrap();
    }
    let start = Instant::now();
    db.flush().unwrap();
    let elapsed = ms(start);
    let stats = db.statistics();
    let table = &stats.tables[0];
    println!(
        "写盘 {N} 行：{elapsed:.1} ms，{:.2} MB（{} 字节），写盘次数 {}，最近耗时 {} ms",
        table.persisted_bytes as f64 / 1_048_576.0,
        table.persisted_bytes,
        table.persist_count,
        table.last_persist_ms
    );
}
