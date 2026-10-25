//! 类型感知索引。
//!
//! # 数据结构选择（性能关键）
//!
//! 每个索引值对应的 ID 集合使用 **`BTreeSet<i64>`**：
//! - 插入/删除 `O(log k)`，避免 C# 版 `ConcurrentHashSet` 之外的线性去重/删除（`Vec::contains` 在低基数字段上会退化
//!   为 `O(k)`——10 万行、`Status` 只有 2 个取值时，每行插入要扫 5 万个 ID）；
//! - 迭代天然按 ID 升序，分页/范围返回顺序稳定；`BTreeSet` 的每节点分摊内存也低于哈希表。
//!
//! 与 C# 版 `TypedIndex` 的其他差异（详见 `docs/审计报告.md`）：
//! - 值用 [`FieldValue`] 枚举而非 `object` 装箱；
//! - 数值/时间范围查询走 `BTreeMap::range`（`O(log n + k)`），C# 版为遍历 `ConcurrentDictionary`（`O(n)`）；
//! - 字符串前缀查询走 `BTreeMap` 有序范围，包含/后缀/复杂通配才遍历键；
//! - 索引计数用 `AtomicU64`，读路径无锁（外层表读锁为读写锁，多读并发）。

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::model::IndexStatistics;
use crate::value::{FieldKind, FieldValue, TimePattern};

/// 单个字段的索引槽。
pub(crate) struct IndexSlot {
    pub field: &'static str,
    pub kind: FieldKind,
    index: TypedIndex,
    query_count: AtomicU64,
    hit_count: AtomicU64,
    id_count: usize,
}

impl IndexSlot {
    fn new(field: &'static str, kind: FieldKind) -> Self {
        Self {
            field,
            kind,
            index: TypedIndex::new(kind),
            query_count: AtomicU64::new(0),
            hit_count: AtomicU64::new(0),
            id_count: 0,
        }
    }

    fn add(&mut self, key: FieldValue, id: i64) {
        if self.index.add(&key, id) {
            self.id_count += 1;
        }
    }

    fn remove(&mut self, key: &FieldValue, id: i64) {
        if self.index.remove(key, id) {
            self.id_count = self.id_count.saturating_sub(1);
        }
    }

    fn find(&self, key: &FieldValue) -> Vec<i64> {
        self.query_count.fetch_add(1, Ordering::Relaxed);
        let ids = self.index.find(key);
        if !ids.is_empty() {
            self.hit_count.fetch_add(1, Ordering::Relaxed);
        }
        ids
    }

    fn range(&self, min: &FieldValue, max: &FieldValue) -> Vec<i64> {
        self.query_count.fetch_add(1, Ordering::Relaxed);
        let ids = self.index.range(min, max);
        if !ids.is_empty() {
            self.hit_count.fetch_add(1, Ordering::Relaxed);
        }
        ids
    }

    fn pattern(&self, pattern: &str) -> Vec<i64> {
        self.query_count.fetch_add(1, Ordering::Relaxed);
        let ids = match self.kind {
            // 时间字段：年 / 年月 / 年月日（对齐 C# DateTimeIndex.GetByPattern）
            FieldKind::Time => match TimePattern::parse(pattern).and_then(|p| p.range()) {
                Some((start, end)) => {
                    self.index.range(&FieldValue::Time(start), &FieldValue::Time(end))
                }
                None => Vec::new(),
            },
            _ => self.index.pattern(pattern),
        };
        if !ids.is_empty() {
            self.hit_count.fetch_add(1, Ordering::Relaxed);
        }
        ids
    }

    fn statistics(&self) -> IndexStatistics {
        IndexStatistics {
            property_name: self.field.to_string(),
            index_type: self.kind.index_type_name().to_string(),
            query_count: self.query_count.load(Ordering::Relaxed),
            hit_count: self.hit_count.load(Ordering::Relaxed),
            unique_value_count: self.index.unique_value_count(),
            id_count: self.id_count,
        }
    }
}

/// 索引值容器。
enum TypedIndex {
    /// 布尔：双桶（无哈希开销）。
    Bool([BTreeSet<i64>; 2]),
    /// 整数/浮点/时间：有序索引。
    Ordered(BTreeMap<FieldValue, BTreeSet<i64>>),
    /// 文本：精确 + 大小写不敏感有序索引。
    Text(TextIndex),
    /// 复杂值：按 JSON 文本精确索引。
    Exact(HashMap<FieldValue, BTreeSet<i64>>),
}

struct TextIndex {
    exact: HashMap<String, BTreeSet<i64>>,
    lower: BTreeMap<String, BTreeSet<i64>>,
}

impl TypedIndex {
    fn new(kind: FieldKind) -> Self {
        match kind {
            FieldKind::Bool => TypedIndex::Bool([BTreeSet::new(), BTreeSet::new()]),
            FieldKind::Int | FieldKind::Float | FieldKind::Time => {
                TypedIndex::Ordered(BTreeMap::new())
            }
            FieldKind::Text => TypedIndex::Text(TextIndex {
                exact: HashMap::new(),
                lower: BTreeMap::new(),
            }),
            FieldKind::Json => TypedIndex::Exact(HashMap::new()),
        }
    }

    /// 返回是否新增了一条 ID（同值同 ID 不重复计数）。
    fn add(&mut self, key: &FieldValue, id: i64) -> bool {
        match self {
            TypedIndex::Bool(index) => {
                let slot = usize::from(matches!(key, FieldValue::Bool(true)));
                index[slot].insert(id)
            }
            TypedIndex::Ordered(map) => map.entry(key.clone()).or_default().insert(id),
            TypedIndex::Exact(map) => map.entry(key.clone()).or_default().insert(id),
            TypedIndex::Text(index) => {
                let text = key.to_text().unwrap_or_default();
                let exact_added = index.exact.entry(text.clone()).or_default().insert(id);
                let lower_added = index.lower.entry(text.to_lowercase()).or_default().insert(id);
                exact_added || lower_added
            }
        }
    }

    fn remove(&mut self, key: &FieldValue, id: i64) -> bool {
        match self {
            TypedIndex::Bool(index) => {
                let slot = usize::from(matches!(key, FieldValue::Bool(true)));
                index[slot].remove(&id)
            }
            TypedIndex::Ordered(map) => remove_from_map(map, key, id),
            TypedIndex::Exact(map) => remove_from_hash_map(map, key, id),
            TypedIndex::Text(index) => {
                let text = key.to_text().unwrap_or_default();
                let removed = remove_from_string_map(&mut index.exact, &text, id);
                let lower_key = text.to_lowercase();
                if let Some(ids) = index.lower.get_mut(&lower_key) {
                    ids.remove(&id);
                    if ids.is_empty() {
                        index.lower.remove(&lower_key);
                    }
                }
                removed
            }
        }
    }

    fn find(&self, key: &FieldValue) -> Vec<i64> {
        match self {
            TypedIndex::Bool(index) => {
                let slot = usize::from(matches!(key, FieldValue::Bool(true)));
                index[slot].iter().copied().collect()
            }
            TypedIndex::Ordered(map) => map
                .get(key)
                .map(|ids| ids.iter().copied().collect())
                .unwrap_or_default(),
            TypedIndex::Exact(map) => map
                .get(key)
                .map(|ids| ids.iter().copied().collect())
                .unwrap_or_default(),
            TypedIndex::Text(index) => {
                let text = key.to_text().unwrap_or_default();
                index
                    .exact
                    .get(&text)
                    .map(|ids| ids.iter().copied().collect())
                    .unwrap_or_default()
            }
        }
    }

    fn range(&self, min: &FieldValue, max: &FieldValue) -> Vec<i64> {
        match self {
            TypedIndex::Ordered(map) => collect_range(map, min, max),
            TypedIndex::Text(index) => {
                let start = min.to_text().unwrap_or_default().to_lowercase();
                let end = max.to_text().unwrap_or_default().to_lowercase();
                if start > end {
                    return Vec::new();
                }
                index
                    .lower
                    .range(start..=end)
                    .flat_map(|(_, ids)| ids.iter().copied())
                    .collect()
            }
            _ => Vec::new(),
        }
    }

    fn pattern(&self, pattern: &str) -> Vec<i64> {
        if pattern.is_empty() {
            return Vec::new();
        }
        match self {
            TypedIndex::Text(index) => text_pattern(index, pattern),
            TypedIndex::Bool(index) => {
                let Some(value) = parse_bool_pattern(pattern) else {
                    return Vec::new();
                };
                index[usize::from(value)].iter().copied().collect()
            }
            TypedIndex::Ordered(_) => Vec::new(),
            TypedIndex::Exact(_) => Vec::new(),
        }
    }

    fn unique_value_count(&self) -> usize {
        match self {
            TypedIndex::Bool(index) => index
                .iter()
                .filter(|ids| !ids.is_empty())
                .count(),
            TypedIndex::Ordered(map) => map.len(),
            TypedIndex::Exact(map) => map.len(),
            TypedIndex::Text(index) => index.exact.len(),
        }
    }

    /// 按索引顺序取前 `limit` 个 ID（升/降序）；不支持的索引类型返回 `None`。
    fn take_ordered(&self, ascending: bool, limit: usize) -> Option<Vec<i64>> {
        fn take<'a, I>(iter: I, limit: usize) -> Vec<i64>
        where
            I: Iterator<Item = &'a BTreeSet<i64>>,
        {
            let mut out = Vec::with_capacity(limit.min(64));
            for ids in iter {
                for id in ids {
                    if out.len() >= limit {
                        return out;
                    }
                    out.push(*id);
                }
            }
            out
        }

        match self {
            TypedIndex::Ordered(map) => Some(if ascending {
                take(map.values(), limit)
            } else {
                take(map.values().rev(), limit)
            }),
            TypedIndex::Text(index) => Some(if ascending {
                take(index.lower.values(), limit)
            } else {
                take(index.lower.values().rev(), limit)
            }),
            _ => None,
        }
    }
}

fn remove_from_map(
    map: &mut BTreeMap<FieldValue, BTreeSet<i64>>,
    key: &FieldValue,
    id: i64,
) -> bool {
    let Some(ids) = map.get_mut(key) else {
        return false;
    };
    let removed = ids.remove(&id);
    if ids.is_empty() {
        map.remove(key);
    }
    removed
}

fn remove_from_hash_map(
    map: &mut HashMap<FieldValue, BTreeSet<i64>>,
    key: &FieldValue,
    id: i64,
) -> bool {
    let Some(ids) = map.get_mut(key) else {
        return false;
    };
    let removed = ids.remove(&id);
    if ids.is_empty() {
        map.remove(key);
    }
    removed
}

fn remove_from_string_map(map: &mut HashMap<String, BTreeSet<i64>>, key: &str, id: i64) -> bool {
    let Some(ids) = map.get_mut(key) else {
        return false;
    };
    let removed = ids.remove(&id);
    if ids.is_empty() {
        map.remove(key);
    }
    removed
}

fn collect_range(
    map: &BTreeMap<FieldValue, BTreeSet<i64>>,
    min: &FieldValue,
    max: &FieldValue,
) -> Vec<i64> {
    if min > max {
        return Vec::new();
    }
    map.range(min.clone()..=max.clone())
        .flat_map(|(_, ids)| ids.iter().copied())
        .collect()
}

fn parse_bool_pattern(pattern: &str) -> Option<bool> {
    match pattern.trim().to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "on" | "是" => Some(true),
        "false" | "0" | "no" | "off" | "否" => Some(false),
        _ => None,
    }
}

fn text_pattern(index: &TextIndex, pattern: &str) -> Vec<i64> {
    let lower_pattern = pattern.to_lowercase();
    if !pattern.contains('*') {
        // 精确（大小写不敏感，对齐 C# StringIndex.GetByPattern）
        return index
            .lower
            .get(&lower_pattern)
            .map(|ids| ids.iter().copied().collect())
            .unwrap_or_default();
    }

    let mut out = Vec::new();
    if pattern.starts_with('*') && pattern.ends_with('*') {
        // *abc* —— 包含
        let needle = lower_pattern.trim_matches('*').to_string();
        if needle.is_empty() {
            return index
                .lower
                .values()
                .flat_map(|ids| ids.iter().copied())
                .collect();
        }
        for (key, ids) in &index.lower {
            if key.contains(&needle) {
                out.extend(ids.iter().copied());
            }
        }
    } else if pattern.ends_with('*') {
        // abc* —— 前缀（有序范围，避免全量遍历）
        let prefix = lower_pattern.trim_end_matches('*').to_string();
        if prefix.is_empty() {
            return index
                .lower
                .values()
                .flat_map(|ids| ids.iter().copied())
                .collect();
        }
        for (key, ids) in index.lower.range(prefix.clone()..) {
            if !key.starts_with(&prefix) {
                break;
            }
            out.extend(ids.iter().copied());
        }
    } else if pattern.starts_with('*') {
        // *abc —— 后缀
        let suffix = lower_pattern.trim_start_matches('*').to_string();
        for (key, ids) in &index.lower {
            if key.ends_with(&suffix) {
                out.extend(ids.iter().copied());
            }
        }
    } else {
        // 复杂通配（如 a*b*c）：遍历 + 逐字符匹配
        for (key, ids) in &index.lower {
            if wildcard_match(key, &lower_pattern) {
                out.extend(ids.iter().copied());
            }
        }
    }
    out
}

/// 简单通配符匹配（支持 `*`；比 C# 版修正了“模式短于文本也判真”的缺陷）。
pub(crate) fn wildcard_match(text: &str, pattern: &str) -> bool {
    let text: Vec<char> = text.chars().collect();
    let pattern: Vec<char> = pattern.chars().collect();
    let mut ti = 0usize;
    let mut pi = 0usize;
    let mut star: Option<usize> = None;
    let mut mark = 0usize;

    while ti < text.len() {
        if pi < pattern.len() && pattern[pi] == text[ti] {
            ti += 1;
            pi += 1;
        } else if pi < pattern.len() && pattern[pi] == '*' {
            star = Some(pi);
            pi += 1;
            mark = ti;
        } else if let Some(star_pos) = star {
            pi = star_pos + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    while pi < pattern.len() && pattern[pi] == '*' {
        pi += 1;
    }
    pi == pattern.len()
}

/// 字段索引集合。
#[derive(Default)]
pub(crate) struct IndexSet {
    slots: HashMap<&'static str, IndexSlot>,
}

impl IndexSet {
    pub(crate) fn new() -> Self {
        Self {
            slots: HashMap::new(),
        }
    }

    pub(crate) fn ensure(&mut self, field: &'static str, kind: FieldKind) {
        self.slots
            .entry(field)
            .or_insert_with(|| IndexSlot::new(field, kind));
    }

    pub(crate) fn add(&mut self, field: &'static str, kind: FieldKind, key: FieldValue, id: i64) {
        self.ensure(field, kind);
        if let Some(slot) = self.slots.get_mut(field) {
            slot.add(key, id);
        }
    }

    pub(crate) fn remove(&mut self, field: &str, key: &FieldValue, id: i64) {
        if let Some(slot) = self.slots.get_mut(field) {
            slot.remove(key, id);
        }
    }

    pub(crate) fn find(&self, field: &str, key: &FieldValue) -> Vec<i64> {
        self.slots
            .get(field)
            .map(|slot| slot.find(key))
            .unwrap_or_default()
    }

    pub(crate) fn range(&self, field: &str, min: &FieldValue, max: &FieldValue) -> Vec<i64> {
        self.slots
            .get(field)
            .map(|slot| slot.range(min, max))
            .unwrap_or_default()
    }

    pub(crate) fn pattern(&self, field: &str, pattern: &str) -> Vec<i64> {
        self.slots
            .get(field)
            .map(|slot| slot.pattern(pattern))
            .unwrap_or_default()
    }

    pub(crate) fn id_count(&self, field: &str) -> usize {
        self.slots.get(field).map(|slot| slot.id_count).unwrap_or(0)
    }

    /// 按索引顺序取前 `limit` 个 ID（分页优化）；索引缺失/不支持返回 `None`。
    pub(crate) fn take_ordered(&self, field: &str, ascending: bool, limit: usize) -> Option<Vec<i64>> {
        self.slots
            .get(field)
            .and_then(|slot| slot.index.take_ordered(ascending, limit))
    }

    pub(crate) fn clear(&mut self) {
        self.slots.clear();
    }

    pub(crate) fn statistics(&self) -> Vec<IndexStatistics> {
        let mut stats: Vec<_> = self
            .slots
            .values()
            .map(|slot| slot.statistics())
            .collect();
        stats.sort_by(|a, b| a.property_name.cmp(&b.property_name));
        stats
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wildcard_matches_csharp_cases() {
        assert!(wildcard_match("zhangsan", "zhang*"));
        assert!(wildcard_match("zhangsan", "*san"));
        assert!(wildcard_match("zhangsan", "*hang*"));
        assert!(wildcard_match("zhangsan", "z*n"));
        assert!(wildcard_match("abc", "*"));
        assert!(!wildcard_match("abc", "ab"));
        assert!(!wildcard_match("abc", "b*"));
    }

    #[test]
    fn numeric_range_uses_ordered_index() {
        let mut set = IndexSet::new();
        set.add("Age", FieldKind::Int, FieldValue::Int(10), 1);
        set.add("Age", FieldKind::Int, FieldValue::Int(20), 2);
        set.add("Age", FieldKind::Int, FieldValue::Int(30), 3);
        assert_eq!(
            set.range("Age", &FieldValue::Int(15), &FieldValue::Int(30)),
            vec![2, 3]
        );
        assert_eq!(set.id_count("Age"), 3);
        assert_eq!(set.take_ordered("Age", false, 2), Some(vec![3, 2]));

        // 删除中间项后计数与查询一致
        set.remove("Age", &FieldValue::Int(20), 2);
        assert_eq!(set.id_count("Age"), 2);
        assert!(set.find("Age", &FieldValue::Int(20)).is_empty());
    }

    #[test]
    fn text_patterns_cover_prefix_contains_suffix() {
        let mut set = IndexSet::new();
        set.add("Name", FieldKind::Text, FieldValue::Text("ZhangSan".into()), 1);
        set.add("Name", FieldKind::Text, FieldValue::Text("LiSi".into()), 2);
        set.add("Name", FieldKind::Text, FieldValue::Text("ZhangSanFeng".into()), 3);

        assert_eq!(set.pattern("Name", "zhang*"), vec![1, 3], "前缀大小写不敏感");
        assert_eq!(set.pattern("Name", "*sifeng"), Vec::<i64>::new());
        assert_eq!(set.pattern("Name", "*lis*"), vec![2]);
        assert_eq!(set.pattern("Name", "zhangsan"), vec![1]);
        // 删除后再查
        set.remove("Name", &FieldValue::Text("LiSi".into()), 2);
        assert!(set.pattern("Name", "*lis*").is_empty());
    }

    #[test]
    fn time_pattern_year_month_day() {
        use chrono::NaiveDate;
        let mut set = IndexSet::new();
        let mut push = |y, m, d, id| {
            let t = NaiveDate::from_ymd_opt(y, m, d)
                .unwrap()
                .and_hms_opt(12, 0, 0)
                .unwrap();
            set.add("CreateTime", FieldKind::Time, FieldValue::Time(t), id);
        };
        push(2024, 1, 5, 1);
        push(2024, 2, 5, 2);
        push(2023, 12, 31, 3);

        assert_eq!(set.pattern("CreateTime", "2024"), vec![1, 2]);
        assert_eq!(set.pattern("CreateTime", "2024-02"), vec![2]);
        assert_eq!(set.pattern("CreateTime", "2023-12-31"), vec![3]);
    }

    #[test]
    fn duplicate_add_does_not_inflate_count() {
        let mut set = IndexSet::new();
        set.add("Status", FieldKind::Text, FieldValue::Text("Active".into()), 1);
        set.add("Status", FieldKind::Text, FieldValue::Text("Active".into()), 1);
        assert_eq!(set.id_count("Status"), 1);
        assert_eq!(set.find("Status", &FieldValue::Text("Active".into())), vec![1]);
    }
}
