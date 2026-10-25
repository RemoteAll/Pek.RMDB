//! 内存数据库门面（对应 C# `cdb` + `MemoryDB`）。
//!
//! - **多实例**：`MemoryDb::new(base_dir)` 可创建相互隔离的库（测试/多租户）；
//! - **全局单例**：`global()` 提供 `cdb` 风格的静态入口，配合 `configure_global` 设置目录；
//! - **惰性建表**：按 `CacheObject::type_name()` 自动创建表并加载 `Data/{type_name}.json`；
//! - **读多写少**：读操作并发无阻塞，写操作单写者保证主存储与索引一致；
//! - **后台持久化**：写操作只置脏并唤醒后台线程，不在调用线程做磁盘 IO。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock, Weak};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::error::{Result, RmdbError};
use crate::model::{
    CacheObject, DbStatistics, PagedResult,
};
use crate::persistence::PersistHub;
use crate::table::Table;
use crate::value::FieldValue;

/// 持久化错误回调（参数：类型名、错误）。
pub type PersistErrorHandler = Arc<dyn Fn(&str, &RmdbError) + Send + Sync>;

/// 数据库选项。
#[derive(Clone)]
pub struct DbOptions {
    /// 数据根目录（数据文件写入 `{base_dir}/Data/{type_name}.json`）。
    pub base_dir: PathBuf,
    /// 是否启用后台持久化线程（关闭后仍可用 [`MemoryDb::flush`] 手动落盘）。
    pub auto_persist: bool,
    /// 最小写盘间隔（写合并窗口；默认 500ms，与 C# 版一致）。
    pub min_write_interval: Duration,
    /// 是否输出缩进换行的 JSON（默认 `true`，对齐 C# `SimpleJsonString` 可读格式）。
    pub pretty: bool,
    /// `MemoryDb` 释放时是否同步写盘（默认 `true`，避免丢尾部修改）。
    pub flush_on_drop: bool,
    /// 持久化失败回调（默认打印到 stderr）。
    pub on_error: Option<PersistErrorHandler>,
}

impl Default for DbOptions {
    fn default() -> Self {
        Self {
            base_dir: default_base_dir(),
            auto_persist: true,
            min_write_interval: Duration::from_millis(500),
            pretty: true,
            flush_on_drop: true,
            on_error: None,
        }
    }
}

/// 默认数据根目录：可执行文件所在目录（对齐 C# `AppDomain.BaseDirectory`）。
pub fn default_base_dir() -> PathBuf {
    dhrust::io::exe_dir()
}

/// 内存数据库。
pub struct MemoryDb {
    inner: Arc<Inner>,
}

pub(crate) struct Inner {
    pub(crate) options: DbOptions,
    pub(crate) tables: RwLock<HashMap<&'static str, Arc<Table>>>,
    pub(crate) persist: Arc<PersistHub>,
    pub(crate) worker: Mutex<Option<JoinHandle<()>>>,
    pub(crate) stop: AtomicBool,
}

impl Inner {
    fn report_error(&self, table: &str, error: &RmdbError) {
        match &self.options.on_error {
            Some(handler) => handler(table, error),
            None => eprintln!("[pek-rmdb] 持久化失败（{table}）：{error}"),
        }
    }

    fn table_path(&self, type_name: &str) -> Option<PathBuf> {
        if self.options.base_dir.as_os_str().is_empty() {
            None
        } else {
            Some(table_path(&self.options.base_dir, type_name))
        }
    }
}

impl MemoryDb {
    /// 使用默认选项创建（数据目录 = 可执行文件目录）。
    pub fn new(base_dir: impl Into<PathBuf>) -> Self {
        Self::with_options(DbOptions {
            base_dir: base_dir.into(),
            ..DbOptions::default()
        })
    }

    /// 纯内存实例（不读写磁盘）。
    pub fn in_memory() -> Self {
        Self::with_options(DbOptions {
            base_dir: PathBuf::new(),
            auto_persist: false,
            flush_on_drop: false,
            ..DbOptions::default()
        })
    }

    /// 使用自定义选项创建。
    pub fn with_options(options: DbOptions) -> Self {
        let persist = Arc::new(PersistHub::new());
        let inner = Arc::new(Inner {
            options,
            tables: RwLock::new(HashMap::new()),
            persist: persist.clone(),
            worker: Mutex::new(None),
            stop: AtomicBool::new(false),
        });
        if inner.options.auto_persist
            && let Some(handle) = spawn_worker(Arc::downgrade(&inner), persist)
        {
            *inner.worker.lock().unwrap_or_else(|e| e.into_inner()) = Some(handle);
        }
        Self { inner }
    }

    /// 数据根目录。
    pub fn base_dir(&self) -> &Path {
        &self.inner.options.base_dir
    }

    /// 指定实体的数据文件路径（纯内存模式返回空路径）。
    pub fn data_file<T: CacheObject>(&self) -> PathBuf {
        self.inner.table_path(T::type_name()).unwrap_or_default()
    }

    /// 已加载（注册）的表名。
    pub fn loaded_tables(&self) -> Vec<&'static str> {
        let tables = self.inner.tables.read().unwrap_or_else(|e| e.into_inner());
        let mut names: Vec<&'static str> = tables.keys().copied().collect();
        names.sort_unstable();
        names
    }

    /// 取表（首次访问时创建并加载）。
    fn table<T: CacheObject>(&self) -> Arc<Table> {
        let type_name = T::type_name();
        if let Some(table) = self
            .inner
            .tables
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(type_name)
            .cloned()
        {
            return table;
        }

        let mut tables = self.inner.tables.write().unwrap_or_else(|e| e.into_inner());
        if let Some(table) = tables.get(type_name).cloned() {
            return table;
        }
        let table = Arc::new(Table::new::<T>());
        if !table.is_in_memory()
            && let Some(path) = self.inner.table_path(type_name)
        {
            let report = |message: String| eprintln!("[pek-rmdb] {message}");
            if let Err(e) = table.load(&path, &report) {
                report(format!("加载 {type_name} 失败（将使用空表）：{e}"));
            }
        }
        tables.insert(type_name, table.clone());
        table
    }

    // ------------------------------------------------------------------
    // 写操作
    // ------------------------------------------------------------------

    /// 插入实体（分配并回写 ID，返回新 ID）。
    pub fn insert<T: CacheObject>(&self, obj: T) -> Result<i64> {
        let table = self.table::<T>();
        let id = table.insert(obj, None)?;
        if table.mark_dirty() {
            self.inner.persist.notify();
        }
        Ok(id)
    }

    /// 插入实体，只对指定字段建立普通索引（对应 C# `InsertByIndex`）。
    pub fn insert_by_index<T: CacheObject>(&self, obj: T, fields: &[&str]) -> Result<i64> {
        let table = self.table::<T>();
        let id = table.insert(obj, Some(fields))?;
        if table.mark_dirty() {
            self.inner.persist.notify();
        }
        Ok(id)
    }

    /// 批量插入（写锁内一次完成；唯一约束冲突时**已插入的行保留**，与 C# 语义一致）。
    pub fn insert_batch<T: CacheObject>(&self, objects: Vec<T>) -> Result<Vec<i64>> {
        if objects.is_empty() {
            return Ok(Vec::new());
        }
        let table = self.table::<T>();
        let ids = table.insert_batch(objects)?;
        if table.mark_dirty() {
            self.inner.persist.notify();
        }
        Ok(ids)
    }

    /// 更新实体（按 `obj.id()` 匹配；同时更新普通索引与唯一约束索引）。
    pub fn update<T: CacheObject>(&self, obj: T) -> Result<()> {
        let table = self.table::<T>();
        table.update(obj)?;
        if table.mark_dirty() {
            self.inner.persist.notify();
        }
        Ok(())
    }

    /// 删除实体（不存在返回 `false`）。
    pub fn delete<T: CacheObject>(&self, id: i64) -> bool {
        let table = self.table::<T>();
        let deleted = table.delete::<T>(id);
        if deleted && table.mark_dirty() {
            self.inner.persist.notify();
        }
        deleted
    }

    /// 清空指定类型的全部数据（并随后持久化为空表）。
    pub fn clear<T: CacheObject>(&self) {
        let table = self.table::<T>();
        table.clear();
        if table.mark_dirty() {
            self.inner.persist.notify();
        }
    }

    /// 清空全部已加载表。
    pub fn clear_all(&self) {
        let tables: Vec<Arc<Table>> = {
            let tables = self.inner.tables.read().unwrap_or_else(|e| e.into_inner());
            tables.values().cloned().collect()
        };
        let mut changed = false;
        for table in tables {
            table.clear();
            changed |= table.mark_dirty();
        }
        if changed {
            self.inner.persist.notify();
        }
    }

    /// 同步写盘所有脏表（后台线程未启用时同样可用）。
    pub fn flush(&self) -> Result<()> {
        let tables: Vec<Arc<Table>> = {
            let tables = self.inner.tables.read().unwrap_or_else(|e| e.into_inner());
            tables.values().cloned().collect()
        };
        for table in tables {
            if table.is_in_memory() || !table.is_dirty() {
                continue;
            }
            let Some(path) = self.inner.table_path(table.type_name) else {
                continue;
            };
            table.save_to(&path, self.inner.options.pretty)?;
        }
        Ok(())
    }

    // ------------------------------------------------------------------
    // 读操作
    // ------------------------------------------------------------------

    /// 按主键查找。
    pub fn find_by_id<T: CacheObject>(&self, id: i64) -> Option<Arc<T>> {
        self.table::<T>().find_by_id::<T>(id)
    }

    /// 全表查找（按 ID 升序）。
    pub fn find_all<T: CacheObject>(&self) -> Vec<Arc<T>> {
        self.table::<T>().find_all::<T>()
    }

    /// 精确查找（走索引）。
    pub fn find_by<T: CacheObject>(
        &self,
        field: &str,
        value: impl Into<FieldValue>,
    ) -> Vec<Arc<T>> {
        self.table::<T>().find_by::<T>(field, value.into())
    }

    /// 范围查找（数值/时间有序索引；文本按小写字典序）。
    pub fn find_by_range<T: CacheObject>(
        &self,
        field: &str,
        min: impl Into<FieldValue>,
        max: impl Into<FieldValue>,
    ) -> Vec<Arc<T>> {
        self.table::<T>()
            .find_by_range::<T>(field, min.into(), max.into())
    }

    /// 模式匹配（文本 `*` 通配、布尔智能值、时间年/月/日）。
    pub fn find_by_like<T: CacheObject>(&self, field: &str, pattern: &str) -> Vec<Arc<T>> {
        self.table::<T>().find_by_like::<T>(field, pattern)
    }

    /// 多条件交集查找。
    pub fn find_by_multiple<T: CacheObject>(
        &self,
        conditions: &[(&str, FieldValue)],
    ) -> Vec<Arc<T>> {
        self.table::<T>().find_by_multiple::<T>(conditions)
    }

    /// 分页查找（指定排序字段时优先走索引顺序，避免全表排序）。
    pub fn find_by_page<T: CacheObject>(
        &self,
        sort_field: Option<&str>,
        page_index: usize,
        page_size: usize,
        ascending: bool,
    ) -> PagedResult<Arc<T>> {
        self.table::<T>()
            .find_by_page::<T>(sort_field, page_index, page_size, ascending)
    }

    /// 唯一字段查找。
    pub fn find_by_unique<T: CacheObject>(
        &self,
        field: &str,
        value: impl Into<FieldValue>,
    ) -> Option<Arc<T>> {
        self.table::<T>().find_by_unique::<T>(field, value.into())
    }

    /// 复合唯一约束查找。
    pub fn find_by_composite_unique<T: CacheObject>(
        &self,
        group: &str,
        values: &[(&str, FieldValue)],
    ) -> Option<Arc<T>> {
        self.table::<T>().find_by_composite_unique::<T>(group, values)
    }

    /// 唯一值是否已存在（更新时可排除自身）。
    pub fn is_unique_value_exists<T: CacheObject>(
        &self,
        field: &str,
        value: impl Into<FieldValue>,
        exclude_id: Option<i64>,
    ) -> bool {
        self.table::<T>()
            .is_unique_value_exists(field, value.into(), exclude_id)
    }

    /// 全库统计。
    pub fn statistics(&self) -> DbStatistics {
        let tables: Vec<Arc<Table>> = {
            let tables = self.inner.tables.read().unwrap_or_else(|e| e.into_inner());
            tables.values().cloned().collect()
        };
        let mut stats: Vec<_> = tables.iter().map(|table| table.statistics()).collect();
        stats.sort_by(|a, b| a.type_name.cmp(&b.type_name));
        let total_rows = stats.iter().map(|s| s.rows).sum();
        DbStatistics {
            table_count: stats.len(),
            total_rows,
            tables: stats,
        }
    }
}

impl Drop for MemoryDb {
    fn drop(&mut self) {
        self.inner.stop.store(true, Ordering::Release);
        self.inner.persist.stop();
        if let Some(handle) = self
            .inner
            .worker
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
        {
            let _ = handle.join();
        }
        if self.inner.options.flush_on_drop {
            let _ = self.flush();
        }
    }
}

/// 后台持久化线程：等待脏标记 → 合并写盘；无脏数据时零轮询。
fn spawn_worker(inner: Weak<Inner>, hub: Arc<PersistHub>) -> Option<JoinHandle<()>> {
    thread::Builder::new()
        .name("pek-rmdb-persist".to_string())
        .spawn(move || {
            loop {
                let Some(db) = inner.upgrade() else {
                    return;
                };
                if db.stop.load(Ordering::Acquire) || hub.is_stopped() {
                    return;
                }
                hub.wait();
                if db.stop.load(Ordering::Acquire) || hub.is_stopped() {
                    return;
                }
                hub.take_pending();
                hub.wait_min_interval(db.options.min_write_interval);
                if db.stop.load(Ordering::Acquire) {
                    return;
                }

                let dirty: Vec<Arc<Table>> = {
                    let tables = db.tables.read().unwrap_or_else(|e| e.into_inner());
                    tables
                        .values()
                        .filter(|table| !table.is_in_memory() && table.is_dirty())
                        .cloned()
                        .collect()
                };
                for table in dirty {
                    if db.stop.load(Ordering::Acquire) {
                        return;
                    }
                    let Some(path) = db.table_path(table.type_name) else {
                        continue;
                    };
                    match table.save_to(&path, db.options.pretty) {
                        Ok(_) => hub.mark_written(),
                        Err(e) => db.report_error(table.type_name, &e),
                    }
                }
                drop(db);
            }
        })
        .ok()
}

/// 数据文件路径（类型名经 `dhrust::io::sanitize_file_name` 清洗，防目录穿越）。
pub(crate) fn table_path(base_dir: &Path, type_name: &str) -> PathBuf {
    base_dir
        .join("Data")
        .join(format!("{}.json", dhrust::io::sanitize_file_name(type_name)))
}

// ----------------------------------------------------------------------
// 全局单例（cdb 风格）
// ----------------------------------------------------------------------

static GLOBAL: OnceLock<MemoryDb> = OnceLock::new();

/// 全局数据库实例（首次调用时按 [`DbOptions::default`] 创建）。
pub fn global() -> &'static MemoryDb {
    GLOBAL.get_or_init(MemoryDb::default)
}

/// 配置全局实例（必须在首次使用 `global()` 之前调用；已初始化则返回 `false`）。
pub fn configure_global(options: DbOptions) -> bool {
    GLOBAL.set(MemoryDb::with_options(options)).is_ok()
}

impl Default for MemoryDb {
    fn default() -> Self {
        Self::with_options(DbOptions::default())
    }
}

/// 全局实例：全表查找。
pub fn find_all<T: CacheObject>() -> Vec<Arc<T>> {
    global().find_all::<T>()
}

/// 全局实例：按主键查找。
pub fn find_by_id<T: CacheObject>(id: i64) -> Option<Arc<T>> {
    global().find_by_id::<T>(id)
}

/// 全局实例：精确查找。
pub fn find_by<T: CacheObject>(field: &str, value: impl Into<FieldValue>) -> Vec<Arc<T>> {
    global().find_by::<T>(field, value)
}

/// 全局实例：范围查找。
pub fn find_by_range<T: CacheObject>(
    field: &str,
    min: impl Into<FieldValue>,
    max: impl Into<FieldValue>,
) -> Vec<Arc<T>> {
    global().find_by_range::<T>(field, min, max)
}

/// 全局实例：模式匹配。
pub fn find_by_like<T: CacheObject>(field: &str, pattern: &str) -> Vec<Arc<T>> {
    global().find_by_like::<T>(field, pattern)
}

/// 全局实例：多条件查找。
pub fn find_by_multiple<T: CacheObject>(conditions: &[(&str, FieldValue)]) -> Vec<Arc<T>> {
    global().find_by_multiple::<T>(conditions)
}

/// 全局实例：分页。
pub fn find_by_page<T: CacheObject>(
    sort_field: Option<&str>,
    page_index: usize,
    page_size: usize,
    ascending: bool,
) -> PagedResult<Arc<T>> {
    global().find_by_page::<T>(sort_field, page_index, page_size, ascending)
}

/// 全局实例：唯一字段查找。
pub fn find_by_unique<T: CacheObject>(
    field: &str,
    value: impl Into<FieldValue>,
) -> Option<Arc<T>> {
    global().find_by_unique::<T>(field, value)
}

/// 全局实例：复合唯一约束查找。
pub fn find_by_composite_unique<T: CacheObject>(
    group: &str,
    values: &[(&str, FieldValue)],
) -> Option<Arc<T>> {
    global().find_by_composite_unique::<T>(group, values)
}

/// 全局实例：唯一值是否存在。
pub fn is_unique_value_exists<T: CacheObject>(
    field: &str,
    value: impl Into<FieldValue>,
    exclude_id: Option<i64>,
) -> bool {
    global().is_unique_value_exists::<T>(field, value, exclude_id)
}

/// 全局实例：插入。
pub fn insert<T: CacheObject>(obj: T) -> Result<i64> {
    global().insert(obj)
}

/// 全局实例：按指定字段建索引插入。
pub fn insert_by_index<T: CacheObject>(obj: T, fields: &[&str]) -> Result<i64> {
    global().insert_by_index(obj, fields)
}

/// 全局实例：批量插入。
pub fn insert_batch<T: CacheObject>(objects: Vec<T>) -> Result<Vec<i64>> {
    global().insert_batch(objects)
}

/// 全局实例：更新。
pub fn update<T: CacheObject>(obj: T) -> Result<()> {
    global().update(obj)
}

/// 全局实例：删除。
pub fn delete<T: CacheObject>(id: i64) -> bool {
    global().delete::<T>(id)
}

/// 全局实例：清空指定类型。
pub fn clear<T: CacheObject>() {
    global().clear::<T>()
}

/// 全局实例：同步写盘。
pub fn flush() -> Result<()> {
    global().flush()
}

/// 全局实例：统计。
pub fn statistics() -> DbStatistics {
    global().statistics()
}
