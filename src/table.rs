//! 单表存储：主存储（ID → 对象）+ 类型感知索引 + 唯一约束索引。
//!
//! # 锁策略（读多写少）
//!
//! 整表状态放在一把 `RwLock<State>` 中：
//! - **读**（按 ID/字段/范围/分页查找）取读锁，多线程无阻塞并发；
//! - **写**（插入/更新/删除/批量）取写锁，单写者保证“主存储 + 普通索引 + 唯一索引”原子一致。
//!
//! 相比 C# 版的 `lock` + `ConcurrentDictionary`：
//! - 读路径没有 `ConcurrentDictionary` 的分段锁开销（一次 `RwLock` 读 + `HashMap` 查）；
//! - 写路径只在真正修改时持锁，索引更新在锁内一次完成，避免多处并发结构的不一致窗口。

use std::any::Any;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::time::Instant;

use crate::error::{Result, RmdbError};
use crate::index::IndexSet;
use crate::json;
use crate::model::{
    CacheObject, CompositeUniqueRule, Field, PagedResult, TableStatistics,
};
use dhrust::io::write_all_text_atomic;
use crate::unique::{UniqueSet, ValueGetter};
use crate::value::{FieldKind, FieldValue};

/// 类型擦除后的实体指针（保存 `Arc<T>`，可按类型 `downcast` 回去）。
pub(crate) type Erased = Arc<dyn Any + Send + Sync>;

/// 类型擦除操作集（加载/持久化路径使用；避免把泛型参数带进后台线程）。
pub(crate) struct TableOps {
    pub id: fn(&Erased) -> i64,
    pub name: fn(&Erased) -> String,
    pub get: fn(&Erased, &str) -> Option<FieldValue>,
    pub serialize: fn(&[Erased], bool) -> String,
    pub parse: fn(&str) -> std::result::Result<Vec<Erased>, String>,
}

/// 单表。
pub(crate) struct Table {
    pub type_name: &'static str,
    fields: &'static [Field],
    composites: &'static [CompositeUniqueRule],
    not_save: bool,
    ops: TableOps,
    state: RwLock<State>,
    dirty: AtomicBool,
    save_mutex: Mutex<()>,
    persist_count: AtomicU64,
    persisted_bytes: AtomicU64,
    last_persist_ms: AtomicU64,
}

struct State {
    by_id: HashMap<i64, Erased>,
    indexes: IndexSet,
    unique: UniqueSet,
    next_id: i64,
    loaded: bool,
}

impl Table {
    /// 按实体类型创建空表。
    pub(crate) fn new<T: CacheObject>() -> Self {
        // 预建全部字段的索引槽（Id/Name + 声明为 indexed 的字段），热路径不再做 entry 查找
        let indexes = IndexSet::new_with_fields(T::fields());

        Self {
            type_name: T::type_name(),
            fields: T::fields(),
            composites: T::composite_uniques(),
            not_save: T::not_save(),
            ops: ops_of::<T>(),
            state: RwLock::new(State {
                by_id: HashMap::new(),
                indexes,
                unique: UniqueSet::default(),
                next_id: 1,
                loaded: false,
            }),
            dirty: AtomicBool::new(false),
            save_mutex: Mutex::new(()),
            persist_count: AtomicU64::new(0),
            persisted_bytes: AtomicU64::new(0),
            last_persist_ms: AtomicU64::new(0),
        }
    }

    pub(crate) fn is_in_memory(&self) -> bool {
        self.not_save
    }

    fn read(&self) -> RwLockReadGuard<'_, State> {
        self.state.read().unwrap_or_else(|e| e.into_inner())
    }

    fn write(&self) -> RwLockWriteGuard<'_, State> {
        self.state.write().unwrap_or_else(|e| e.into_inner())
    }

    /// 表中是否有未落盘修改。
    pub(crate) fn is_dirty(&self) -> bool {
        self.dirty.load(Ordering::Acquire)
    }

    /// 标记脏数据（后台持久化线程据此写盘）。
    ///
    /// 返回 `true` 表示本次从“干净”变为“脏”（调用方据此决定是否唤醒后台线程，
    /// 连续写入时只在第一次唤醒，减少调度锁竞争）。
    pub(crate) fn mark_dirty(&self) -> bool {
        if self.not_save {
            return false;
        }
        !self.dirty.swap(true, Ordering::AcqRel)
    }

    /// 从磁盘加载（首次访问时调用一次；文件不存在视为空表）。
    ///
    /// 兼容 C# 版 `Data/{Type.FullName}.json`：
    /// - 加载时**修正 ID 计数器**为 `max(id)+1`（C# 版未修正，加载后新增会从 1 开始覆盖旧数据）；
    /// - 唯一冲突的对象直接跳过并回调警告（C# 版对象留在表中但唯一索引缺失，状态不一致）。
    pub(crate) fn load(&self, path: &Path, warn: &dyn Fn(String)) -> Result<()> {
        {
            let mut state = self.write();
            if state.loaded {
                return Ok(());
            }
            state.loaded = true;
        }
        if self.not_save {
            return Ok(());
        }

        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(RmdbError::io(path, e)),
        };
        if text.trim().is_empty() {
            return Ok(());
        }
        let items = (self.ops.parse)(&text).map_err(|message| RmdbError::json(path, message))?;

        let mut state = self.write();
        for item in items {
            let id = (self.ops.id)(&item);
            if id <= 0 {
                warn(format!(
                    "{}：发现无效对象 ID {id}，已跳过（数据文件：{}）",
                    self.type_name,
                    path.display()
                ));
                continue;
            }
            if state.by_id.contains_key(&id) {
                warn(format!(
                    "{}：发现重复对象 ID {id}，已跳过（数据文件：{}）",
                    self.type_name,
                    path.display()
                ));
                continue;
            }

            let name = (self.ops.name)(&item);
            {
                let get = |field: &str| (self.ops.get)(&item, field);
                if let Err(e) = state.unique.add(self.fields, self.composites, id, &get) {
                    warn(format!(
                        "{}：加载时发现唯一约束冲突，已跳过对象 ID {id}：{e}",
                        self.type_name
                    ));
                    continue;
                }
                index_entity(&mut state, self.fields, id, &name, &get, None);
            }
            state.by_id.insert(id, item);
            state.next_id = state.next_id.max(id.saturating_add(1));
        }
        Ok(())
    }

    /// 序列化快照（按 ID 升序，输出稳定便于 diff）。
    pub(crate) fn snapshot_text(&self, pretty: bool) -> String {
        let items = self.snapshot();
        (self.ops.serialize)(&items, pretty)
    }

    fn snapshot(&self) -> Vec<Erased> {
        let state = self.read();
        let mut items: Vec<(i64, Erased)> = state
            .by_id
            .iter()
            .map(|(id, item)| (*id, item.clone()))
            .collect();
        items.sort_by_key(|(id, _)| *id);
        items.into_iter().map(|(_, item)| item).collect()
    }

    /// 保存到磁盘（原子替换；后台线程与 `flush` 共用）。
    pub(crate) fn save_to(&self, path: &Path, pretty: bool) -> Result<u64> {
        if self.not_save {
            return Ok(0);
        }
        let _guard = self.save_mutex.lock().unwrap_or_else(|e| e.into_inner());
        // 先“取走”脏标记：写入期间的新修改会重新置脏，由下一轮补写
        self.dirty.store(false, Ordering::Release);
        let start = Instant::now();
        let text = self.snapshot_text(pretty);
        match write_all_text_atomic(path, &text) {
            Ok(()) => {
                let bytes = text.len() as u64;
                self.persist_count.fetch_add(1, Ordering::Relaxed);
                self.persisted_bytes.fetch_add(bytes, Ordering::Relaxed);
                self.last_persist_ms
                    .store(start.elapsed().as_millis() as u64, Ordering::Relaxed);
                Ok(bytes)
            }
            Err(e) => {
                self.dirty.store(true, Ordering::Release);
                Err(RmdbError::io(path, e))
            }
        }
    }

    /// 插入实体并分配 ID（返回新 ID）。
    pub(crate) fn insert<T: CacheObject>(&self, mut obj: T, only: Option<&[&str]>) -> Result<i64> {
        let mut state = self.write();
        let id = state.next_id;
        obj.set_id(id);

        let name = obj.name().to_string();
        {
            let get = |field: &str| obj.get_field(field);
            // 唯一索引两阶段原子写入（内部完成校验；失败不改变状态）
            state.unique.add(self.fields, self.composites, id, &get)?;
            index_entity(&mut state, self.fields, id, &name, &get, only);
        }
        state.next_id = id.saturating_add(1);
        state.by_id.insert(id, Arc::new(obj));
        Ok(id)
    }

    /// 批量插入（同类型；写锁内一次完成，索引只更新一次，返回分配的 ID）。
    pub(crate) fn insert_batch<T: CacheObject>(&self, objects: Vec<T>) -> Result<Vec<i64>> {
        let mut state = self.write();
        let mut ids = Vec::with_capacity(objects.len());
        for mut obj in objects {
            let id = state.next_id;
            obj.set_id(id);
            let name = obj.name().to_string();
            {
                let get = |field: &str| obj.get_field(field);
                state.unique.add(self.fields, self.composites, id, &get)?;
                index_entity(&mut state, self.fields, id, &name, &get, None);
            }
            state.next_id = id.saturating_add(1);
            state.by_id.insert(id, Arc::new(obj));
            ids.push(id);
        }
        Ok(ids)
    }

    /// 更新实体（按 `obj.id()` 匹配；不存在或类型不符返回错误）。
    ///
    /// **按字段 diff**：只重建真正变化的普通索引/唯一键（典型“改 1 个字段”的更新
    /// 不再全量删除 + 重建全部索引）。
    pub(crate) fn update<T: CacheObject>(&self, obj: T) -> Result<()> {
        let mut state = self.write();
        let id = obj.id();
        let old = state
            .by_id
            .get(&id)
            .cloned()
            .ok_or_else(|| RmdbError::NotFound(format!("{}（ID {id}）", self.type_name)))?;
        let old_ref = old.downcast_ref::<T>().ok_or(RmdbError::TypeMismatch {
            expected: self.type_name,
            got: "其他实体类型",
        })?;

        // 1. 全量校验（排除自身）；失败不改变任何状态
        {
            let get = |field: &str| obj.get_field(field);
            state
                .unique
                .validate(self.fields, self.composites, &get, Some(id))?;
        }

        // 2. 唯一索引按字段/复合键 diff（内部两阶段，失败不改变状态）
        {
            let old_get = |field: &str| old_ref.get_field(field);
            let new_get = |field: &str| obj.get_field(field);
            state
                .unique
                .update_entity(self.fields, self.composites, id, &old_get, &new_get)?;
        }

        // 3. 普通索引按字段 diff（Name 单独处理）
        let old_name = old_ref.name();
        let new_name = obj.name();
        if old_name != new_name {
            state
                .indexes
                .remove("Name", &FieldValue::Text(old_name.to_string()), id);
            state.indexes.add(
                "Name",
                FieldKind::Text,
                FieldValue::Text(new_name.to_string()),
                id,
            );
        }
        for field in self.fields {
            if !field.indexed {
                continue;
            }
            if field.name == "Id" || field.name == "Name" {
                continue;
            }
            let old_key = old_ref
                .get_field(field.name)
                .and_then(|value| value.normalize(field.kind));
            let new_key = obj
                .get_field(field.name)
                .and_then(|value| value.normalize(field.kind));
            if old_key == new_key {
                continue;
            }
            if let Some(key) = old_key {
                state.indexes.remove(field.name, &key, id);
            }
            if let Some(key) = new_key {
                state.indexes.add(field.name, field.kind, key, id);
            }
        }

        state.by_id.insert(id, Arc::new(obj));
        Ok(())
    }

    /// 删除实体（不存在返回 `false`）。
    pub(crate) fn delete<T: CacheObject>(&self, id: i64) -> bool {
        let mut state = self.write();
        let Some(old) = state.by_id.get(&id).cloned() else {
            return false;
        };
        let Some(old_ref) = old.downcast_ref::<T>() else {
            return false;
        };
        {
            let get = |field: &str| old_ref.get_field(field);
            remove_index_entity(&mut state, self.fields, id, old_ref.name(), &get);
            state.unique.remove(self.fields, self.composites, &get);
        }
        state.by_id.remove(&id);
        true
    }

    /// 清空表（内存与后续持久化都为空表）。
    pub(crate) fn clear(&self) {
        let mut state = self.write();
        state.by_id.clear();
        state.indexes = IndexSet::new_with_fields(self.fields);
        state.unique.clear();
        state.next_id = 1;
        state.loaded = true;
    }

    /// 主键查找。
    pub(crate) fn find_by_id<T: CacheObject>(&self, id: i64) -> Option<Arc<T>> {
        let state = self.read();
        state.by_id.get(&id)?.clone().downcast::<T>().ok()
    }

    /// 全表查找（按 ID 升序）。
    pub(crate) fn find_all<T: CacheObject>(&self) -> Vec<Arc<T>> {
        let state = self.read();
        let mut items: Vec<(i64, Erased)> = state
            .by_id
            .iter()
            .map(|(id, item)| (*id, item.clone()))
            .collect();
        items.sort_by_key(|(id, _)| *id);
        items
            .into_iter()
            .filter_map(|(_, item)| item.downcast::<T>().ok())
            .collect()
    }

    /// 精确查找（走索引）。
    pub(crate) fn find_by<T: CacheObject>(&self, field: &str, value: FieldValue) -> Vec<Arc<T>> {
        let Some(kind) = self.field_kind(field) else {
            return Vec::new();
        };
        let Some(key) = value.normalize(kind) else {
            return Vec::new();
        };
        let state = self.read();
        let ids = state.indexes.find(field, &key);
        collect_by_ids(&state, ids)
    }

    /// 范围查找（数值/时间有序索引；文本按小写字典序）。
    pub(crate) fn find_by_range<T: CacheObject>(
        &self,
        field: &str,
        min: FieldValue,
        max: FieldValue,
    ) -> Vec<Arc<T>> {
        let Some(kind) = self.field_kind(field) else {
            return Vec::new();
        };
        let (Some(min), Some(max)) = (min.normalize(kind), max.normalize(kind)) else {
            return Vec::new();
        };
        let state = self.read();
        let ids = state.indexes.range(field, &min, &max);
        collect_by_ids(&state, ids)
    }

    /// 模式匹配（文本 `*` 通配、布尔智能值、时间年/月/日）。
    pub(crate) fn find_by_like<T: CacheObject>(&self, field: &str, pattern: &str) -> Vec<Arc<T>> {
        if self.field_kind(field).is_none() {
            return Vec::new();
        }
        let state = self.read();
        let ids = state.indexes.pattern(field, pattern);
        collect_by_ids(&state, ids)
    }

    /// 多条件交集查找。
    pub(crate) fn find_by_multiple<T: CacheObject>(
        &self,
        conditions: &[(&str, FieldValue)],
    ) -> Vec<Arc<T>> {
        if conditions.is_empty() {
            return Vec::new();
        }
        let state = self.read();
        // 先查候选集最小的条件（减少交集规模）
        let mut ordered: Vec<&(&str, FieldValue)> = conditions.iter().collect();
        ordered.sort_by_key(|(field, _)| state.indexes.id_count(field));
        let mut result: Option<HashSet<i64>> = None;
        for (field, value) in ordered {
            let Some(kind) = self.field_kind(field) else {
                return Vec::new();
            };
            let Some(key) = value.clone().normalize(kind) else {
                return Vec::new();
            };
            let ids: HashSet<i64> = state.indexes.find(field, &key).into_iter().collect();
            result = Some(match result {
                None => ids,
                Some(previous) => previous.intersection(&ids).copied().collect(),
            });
            if result.as_ref().is_some_and(HashSet::is_empty) {
                return Vec::new();
            }
        }
        let mut ids: Vec<i64> = result.unwrap_or_default().into_iter().collect();
        ids.sort_unstable();
        collect_by_ids(&state, ids)
    }

    /// 唯一字段查找。
    pub(crate) fn find_by_unique<T: CacheObject>(
        &self,
        field: &str,
        value: FieldValue,
    ) -> Option<Arc<T>> {
        let kind = self.field_kind(field)?;
        let key = value.normalize(kind)?;
        let state = self.read();
        let id = state.unique.find_value(field, &key)?;
        state.by_id.get(&id)?.clone().downcast::<T>().ok()
    }

    /// 复合唯一约束查找（按约束声明的字段顺序取值）。
    pub(crate) fn find_by_composite_unique<T: CacheObject>(
        &self,
        group: &str,
        values: &[(&str, FieldValue)],
    ) -> Option<Arc<T>> {
        let rule = self.composites.iter().find(|r| r.group == group)?;
        let mut key = Vec::with_capacity(rule.fields.len());
        for name in rule.fields {
            let (_, value) = values
                .iter()
                .find(|(field, _)| field.eq_ignore_ascii_case(name))?;
            let kind = self.field_kind(name)?;
            key.push(value.clone().normalize(kind)?);
        }
        let state = self.read();
        let id = state.unique.find_composite(group, &key)?;
        state.by_id.get(&id)?.clone().downcast::<T>().ok()
    }

    /// 唯一值是否已存在（更新场景可排除自身）。
    pub(crate) fn is_unique_value_exists(
        &self,
        field: &str,
        value: FieldValue,
        exclude_id: Option<i64>,
    ) -> bool {
        let Some(kind) = self.field_kind(field) else {
            return false;
        };
        let Some(key) = value.normalize(kind) else {
            return false;
        };
        let state = self.read();
        match state.unique.find_value(field, &key) {
            Some(id) => exclude_id != Some(id),
            None => false,
        }
    }

    /// 分页（优先按索引顺序取页，避免全表排序）。
    pub(crate) fn find_by_page<T: CacheObject>(
        &self,
        sort_field: Option<&str>,
        page_index: usize,
        page_size: usize,
        ascending: bool,
    ) -> PagedResult<Arc<T>> {
        let page_size = page_size.max(1);
        let skip = page_index.saturating_mul(page_size);
        let take_end = skip.saturating_add(page_size);
        let state = self.read();
        let total_count = state.by_id.len();
        let total_pages = total_count.div_ceil(page_size);

        let mut items: Vec<Arc<T>> = Vec::new();
        let mut used_index = false;
        if let Some(field) = sort_field {
            // 索引覆盖全表时按索引顺序取前 take_end 条（含空值字段不建索引，退回全量排序）
            if total_count > 0
                && state.indexes.id_count(field) == total_count
                && let Some(ids) = state.indexes.take_ordered(field, ascending, take_end)
            {
                items = ids
                    .into_iter()
                    .skip(skip)
                    .filter_map(|id| state.by_id.get(&id)?.clone().downcast::<T>().ok())
                    .collect();
                used_index = true;
            }
        }

        if !used_index {
            match sort_field {
                None => {
                    // 无排序字段：按 ID 升序/降序取页（整数排序，不做 FieldValue 比较）
                    let mut ids: Vec<i64> = state.by_id.keys().copied().collect();
                    ids.sort_unstable();
                    if !ascending {
                        ids.reverse();
                    }
                    items = ids
                        .into_iter()
                        .skip(skip)
                        .take(page_size)
                        .filter_map(|id| state.by_id.get(&id)?.clone().downcast::<T>().ok())
                        .collect();
                }
                Some(field) => {
                    let kind = self.field_kind(field);
                    let mut rows: Vec<(FieldValue, i64)> = Vec::with_capacity(total_count);
                    for (id, item) in &state.by_id {
                        let key = kind
                            .and_then(|kind| {
                                item.downcast_ref::<T>()?
                                    .get_field(field)
                                    .and_then(|value| value.normalize(kind))
                            })
                            .unwrap_or(FieldValue::Null);
                        rows.push((key, *id));
                    }
                    let cmp = |a: &(FieldValue, i64), b: &(FieldValue, i64)| {
                        let ordering = if ascending {
                            a.0.cmp(&b.0)
                        } else {
                            b.0.cmp(&a.0)
                        };
                        ordering.then(a.1.cmp(&b.1))
                    };
                    // 只选目标页边界（O(n)），再排序前 take_end 个（O(k log k)）
                    if take_end < rows.len() {
                        rows.select_nth_unstable_by(take_end - 1, cmp);
                        rows.truncate(take_end);
                    }
                    rows.sort_by(cmp);
                    items = rows
                        .into_iter()
                        .skip(skip)
                        .take(page_size)
                        .filter_map(|(_, id)| state.by_id.get(&id)?.clone().downcast::<T>().ok())
                        .collect();
                }
            }
        }

        PagedResult {
            items,
            page_index,
            page_size,
            total_count,
            total_pages,
        }
    }

    /// 表统计。
    pub(crate) fn statistics(&self) -> TableStatistics {
        let state = self.read();
        TableStatistics {
            type_name: self.type_name.to_string(),
            rows: state.by_id.len(),
            indexes: state.indexes.statistics(),
            unique_fields: self.fields.iter().filter(|field| field.unique.is_some()).count(),
            unique_composites: self.composites.len(),
            persist_count: self.persist_count.load(Ordering::Relaxed),
            persisted_bytes: self.persisted_bytes.load(Ordering::Relaxed),
            last_persist_ms: self.last_persist_ms.load(Ordering::Relaxed),
            in_memory: self.not_save,
        }
    }

    fn field_kind(&self, field: &str) -> Option<FieldKind> {
        if field.eq_ignore_ascii_case("Id") {
            return Some(FieldKind::Int);
        }
        if field.eq_ignore_ascii_case("Name") {
            return Some(FieldKind::Text);
        }
        self.fields.iter().find(|f| f.name == field).map(|f| f.kind)
    }
}

fn collect_by_ids<T: CacheObject>(state: &State, ids: Vec<i64>) -> Vec<Arc<T>> {
    let mut out = Vec::with_capacity(ids.len());
    for id in ids {
        if let Some(item) = state.by_id.get(&id)
            && let Ok(item) = item.clone().downcast::<T>()
        {
            out.push(item);
        }
    }
    out
}

/// 建立普通索引（`only = None` 表示全部；`Some` 时仅指定字段，对应 C# `InsertByIndex`）。
fn index_entity(
    state: &mut State,
    fields: &'static [Field],
    id: i64,
    name: &str,
    get: ValueGetter<'_>,
    only: Option<&[&str]>,
) {
    let wanted = |field: &str| match only {
        None => true,
        Some(list) => list.contains(&field),
    };

    if wanted("Id") {
        state
            .indexes
            .add("Id", FieldKind::Int, FieldValue::Int(id), id);
    }
    if wanted("Name") {
        state.indexes.add(
            "Name",
            FieldKind::Text,
            FieldValue::Text(name.to_string()),
            id,
        );
    }
    for field in fields {
        if !field.indexed || !wanted(field.name) {
            continue;
        }
        if field.name == "Id" || field.name == "Name" {
            continue;
        }
        if let Some(value) = get(field.name)
            && let Some(key) = value.normalize(field.kind)
        {
            state.indexes.add(field.name, field.kind, key, id);
        }
    }
}

fn remove_index_entity(
    state: &mut State,
    fields: &'static [Field],
    id: i64,
    name: &str,
    get: ValueGetter<'_>,
) {
    state.indexes.remove("Id", &FieldValue::Int(id), id);
    state
        .indexes
        .remove("Name", &FieldValue::Text(name.to_string()), id);
    for field in fields {
        if !field.indexed {
            continue;
        }
        if field.name == "Id" || field.name == "Name" {
            continue;
        }
        if let Some(value) = get(field.name)
            && let Some(key) = value.normalize(field.kind)
        {
            state.indexes.remove(field.name, &key, id);
        }
    }
}

fn ops_of<T: CacheObject>() -> TableOps {
    TableOps {
        id: |item| item.downcast_ref::<T>().map(|entity| entity.id()).unwrap_or(0),
        name: |item| {
            item.downcast_ref::<T>()
                .map(|entity| entity.name().to_string())
                .unwrap_or_default()
        },
        get: |item, field| item.downcast_ref::<T>().and_then(|entity| entity.get_field(field)),
        serialize: json::serialize_snapshot::<T>,
        parse: |text| {
            json::parse_entities::<T>(text).map(|items| {
                items
                    .into_iter()
                    .map(|entity| Arc::new(entity) as Erased)
                    .collect()
            })
        },
    }
}
