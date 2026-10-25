# Pek.RMDB 与 C# Pek.MDB 对照

> 参照源码：`G:\Code\Pek.FrameWork\Pek.MDB`
> 原则：**功能与语义对齐；Rust 无反射/无装箱的限制用显式契约替代；发现 C# 缺陷时在 Rust 版修正并记录。**

## 1. 类型映射

| C# | Rust | 说明 |
|---|---|---|
| `CacheObject`（基类） | `CacheObject` trait | `Id`/`Name` 由框架维护；`Default` 供反序列化构造 |
| `[Unique("msg", AllowNull=false)]` | `Field::text("X").unique("msg")` / `.unique_required("msg")` | 单字段唯一 |
| `[CompositeUnique("G", "A", "B")]` | `CompositeUniqueRule::new("G", &["A","B"], "msg")` | 类级复合唯一 |
| `[NotSave]`（属性） | `Field::not_save()` | 字段不持久化 |
| `[NotSave]`（类） | `CacheObject::not_save() -> true` | 整表常驻内存 |
| `[NotIndexed]` | `Field::not_indexed()` | 不建普通索引（C# 版未实际生效，见 §5） |
| `object` 属性值 | `FieldValue` 枚举 | 无装箱；`Int/Float/Text/Time/Bool/List/Map` |
| `ConcurrentDictionary<long, CacheObject>` | `HashMap<i64, Arc<dyn Any + Send + Sync>>` | 主存储；`Arc<T>` 支持零拷贝读 |
| `Type.FullName` | `CacheObject::type_name()` | 决定 `Data/{type_name}.json` |
| `cdb`（静态类） | 自由函数（`pek_rmdb::insert` 等）+ `MemoryDb` 实例 | 多实例隔离 + 全局单例 |
| `Result`（`_ding/Result.cs`） | `Result<(), RmdbError>` | Rust 用 `Result` 类型，无 HTML/JSON 错误格式 |

## 2. API 映射

| C# `cdb` | Rust 实例方法 | Rust 全局函数 |
|---|---|---|
| `FindAll<T>()` | `find_all::<T>()` | `find_all::<T>()` |
| `FindById<T>(id)` | `find_by_id::<T>(id)` | `find_by_id::<T>(id)` |
| `FindBy<T>(prop, val)` | `find_by::<T>(field, value)` | `find_by::<T>(..)` |
| `FindByRange<T>(prop, min, max)` | `find_by_range::<T>(..)` | `find_by_range::<T>(..)` |
| `FindByLike<T>(prop, pattern)` | `find_by_like::<T>(..)` | `find_by_like::<T>(..)` |
| `FindByMultiple<T>(dict)` | `find_by_multiple::<T>(&[(&str, FieldValue)])` | `find_by_multiple::<T>(..)` |
| `FindByPage<T>(prop, i, size, asc)` | `find_by_page::<T>(Option<&str>, i, size, asc)` | `find_by_page::<T>(..)` |
| `FindByUnique<T>(prop, val)` | `find_by_unique::<T>(..)` | `find_by_unique::<T>(..)` |
| `FindByCompositeUnique<T>(group, dict)` | `find_by_composite_unique::<T>(group, &[(&str, FieldValue)])` | 同 |
| `IsUniqueValueExists<T>(prop, val, exclude)` | `is_unique_value_exists::<T>(..)` | 同 |
| `Insert(obj)` | `insert(obj) -> Result<i64>` | `insert(obj)` |
| `InsertByIndex(obj, prop, val)` | `insert_by_index(obj, &["Field"])` | 同 |
| `InsertBatch(list)` | `insert_batch(Vec<T>) -> Result<Vec<i64>>` | 同 |
| `Update(obj)` | `update(obj) -> Result<()>` | `update(obj)` |
| `Delete(obj)` | `delete::<T>(id) -> bool` | `delete::<T>(id)` |
| `GetUniqueConstraintStatistics()` | `statistics()`（表级 `unique_fields/unique_composites`） | `statistics()` |
| `CacheObject.FindBy/Insert/Update/Delete` | 用 `MemoryDb` / 自由函数替代 | — |
| `cdb.FindNameById<T>(id)` | `find_by_id::<T>(id).map(|x| x.name().to_string())` | — |

## 3. 索引映射

| C# 索引 | Rust 实现 | 能力与优化 |
|---|---|---|
| `StringIndex` | `TextIndex { exact: HashMap, lower: BTreeMap }` | 精确区分大小写；`find_by_like` 大小写不敏感；前缀用 `BTreeMap::range`；范围按小写字典序 |
| `NumericIndex` | `Ordered(BTreeMap<FieldValue, BTreeSet<i64>>)` | 范围 `O(log n + k)`（C# 遍历 `O(n)`） |
| `DateTimeIndex` | 同上 + `TimePattern` | 范围 + 年/年月/年月日（用日期区间 range，C# 用字符串分组字典） |
| `BooleanIndex` | `Bool([BTreeSet<i64>; 2])` | 智能值（true/1/yes/on/是、false/0/no/off/否） |
| `GenericIndex` | `Exact(HashMap<FieldValue, BTreeSet<i64>>)` | JSON 字段按文本精确索引 |

- 值先按 `FieldKind` 规范化：`i32/i64/文本数字` 都命中同一数值索引（C# 用 `object` 相等，类型不同即不命中）。
- ID 集合 `BTreeSet<i64>`：插入/删除 `O(log k)`，迭代有序（C# `ConcurrentHashSet` 无序且每次查询 `ToHashSet()` 复制）。
- `Id`/`Name` 自动索引；`Name` 空串也建索引（C# 跳过空串，导致该行不参与分页/范围）。

## 4. 持久化映射

| 方面 | C# | Rust |
|---|---|---|
| 文件 | `{BaseDirectory}/Data/{FullName}.json` | `{base_dir}/Data/{type_name}.json` |
| 格式 | `SimpleJsonString`：`[\r\n\t{ Id:1, Name:"x", A:1 }\r\n]` | 相同（裸键、`\r\n` + `\t`、`Id`/`Name` 在前、字段按 `fields()` 顺序） |
| 解析 | 自带 `JsonParser`（注释/裸键） | 复用 `dhrust::json::parse_lax`（注释/裸键/引号键/尾逗号/Unicode；C# 兼容下沉实现） |
| 写入 | `Task.Run` 异步 + 500ms 跳过 | 后台线程 + 500ms 合并（**不丢尾写**）+ 原子替换 |
| 转义 | `JsonString.ClearNewLine`（`\`/`"`、去换行/Tab） | 相同（`Name` 也转义，C# 特殊路径未转义） |
| 时间 | `DateTime.ToString()`（随文化，如 `2024/1/5 14:30:00`） | 写 `yyyy-MM-dd HH:mm:ss`；读兼容斜杠/ISO/Unix 时间戳 |
| 加载 | 首次 `FindAll/FindBy` 时按需加载 | 首次访问该表时加载（含 `FindById`） |

> Rust 写出的文件可被 C# 版直接读取；C# 写出的旧文件可直接放入 `Data/` 由 Rust 加载（测试 `reads_csharp_legacy_file` 覆盖）。

## 5. 有意差异（含理由）

| 差异 | C# 行为 | Rust 行为 | 理由 |
|---|---|---|---|
| 实体描述 | 反射属性 | `fields()` + `get_field/set_field` | Rust 无反射；静态描述零运行时开销 |
| 读返回值 | 对象引用（可变） | `Arc<T>`（不可变共享） | 并发安全；更新需提交完整对象 |
| 更新 | 修改同一实例后 `Update()` | `let mut x = (*find_by_id(id)?).clone(); ...; update(x)` | Rust 没有共享可变引用 |
| 删除 | `Delete(obj)` | `delete::<T>(id)` | ID 唯一标识，避免类型擦除 |
| 排序分页 | 反射 `GetValue` + `OrderBy` 全量排序 | 优先按索引顺序取页；索引不全才全量排序 | 性能（9µs/页） |
| 分页无排序字段 | 字典遍历顺序 | 按 ID 升序 | 结果稳定可复现 |
| `[NotIndexed]` | 索引系统未使用该特性（所有属性都建索引） | `not_indexed()` 真正跳过索引 | 尊重声明、节省内存 |
| 通配符 | `"ab"` 可匹配 `"abc"`（未消耗完文本） | 必须完整匹配 | 修正缺陷 |
| 加载唯一冲突 | 记警告，对象仍入表但唯一索引缺失 | 跳过该对象（含普通索引） | 保持主存储与索引一致 |
| ID 计数器 | 加载后不修正，新增从 1 覆盖旧数据 | 加载后取 `max(id)+1` | 修正数据损坏缺陷 |
| `clear` | 清空后下次 `FindAll` 重新从文件加载 | 清空并写空表 | 语义更直观（“清空”即持久清空） |
| `insert` 已有 ID | 重新分配（覆盖原值） | 相同 | 语义一致 |

## 6. C# 缺陷清单与 Rust 修正

| # | 位置 | 问题 | Rust 处理 |
|---|---|---|---|
| 1 | `MemoryDB.GetNextIdAtomic` | 从 1 开始且加载不修正 → 新插入覆盖已有行 | 加载时 `next_id = max(id)+1`（测试 `reload_fixes_id_counter_to_max_plus_one`） |
| 2 | `GetListWithIndex` | 唯一冲突仅 `XTrace`，对象仍在表中、唯一索引缺失 | 跳过对象并警告（测试 `duplicated_unique_rows_are_skipped_on_load`） |
| 3 | `MemoryDB.FindById` | 未触发文件加载，直接调用可能返回 null | 首次访问表即加载 |
| 4 | `NumericIndex.GetRange` / `StringIndex.GetRange` | 遍历全部键 `O(n)` | `BTreeMap::range` `O(log n + k)` |
| 5 | `DateTimeIndex` | `SortedDictionary` + `ReaderWriterLockSlim` + `Where` 遍历 | 表写锁内 `BTreeMap::range`；年/月/日直接用日期区间 |
| 6 | `TypedIndexBase.GetIds` | 每次查询 `idSet.ToHashSet()` 复制 | 返回 `Vec<i64>`（BTreeSet 迭代），无中间集合 |
| 7 | `StringIndex` | 精确索引 + 小写索引两份 `ConcurrentDictionary`；范围遍历 | 保留两份（能力需要），小写侧改 `BTreeMap` 支持前缀/范围 |
| 8 | `cdb.FindByPage` | 反射 `GetValue` + `Count()` 两次 + 全量排序 | 索引顺序取页；无索引才排序（且只排序一次） |
| 9 | `StartAsyncPersistence` | 500ms 内直接 return，期间最后一次修改可能永不落盘 | 合并写：写前取走脏标记，写期间的新修改下轮补写 |
| 10 | `StringIndex.IsWildcardMatch` | 模式短于文本也判真（`ab` ≈ `abc`） | 完整匹配算法 |
| 11 | `SimpleJsonString` | `Name` 未走 `EncodeQuoteAndClearLine`，含引号会破坏文件 | 统一转义 |
| 12 | `SimpleJsonString.ConvertList` | null 元素跳过但逗号按原索引判断（边界可能多/少逗号） | 只对实际输出项加分隔符 |
| 13 | `_hasCheckedFileDB` 双检锁 | 加载完成后才置标记，并发首次可重复加载 | `loaded` 标记在写锁内先置位（失败不重试） |

## 7. 未实现（有意）

- `cdb.FindNameById`（一行组合即可，不单独提供）。
- C# `Result` 的 HTML/JSON 错误格式（Rust 用 `Result` + `Display`）。
- 反射式 `CacheObject.FindBy/Insert/Update/Delete` 实例方法（Rust 用 `MemoryDb`/自由函数）。
- `JsonObject`/`JSON.ConvertList` 等通用 JSON 工具（库内只保留持久化所需的读写）。
