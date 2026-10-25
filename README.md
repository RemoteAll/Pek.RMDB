# Pek.RMDB · 高性能内存数据库（Rust 版）

> C# [`Pek.MDB`](../Pek.FrameWork/Pek.MDB) 的 Rust 实现：面向 **“读多写少、内存为主、JSON 异步持久化”** 场景的轻量级内存数据库。
> crate：`pek-rmdb`（库，无面板/无服务），供生态内 Rust 项目以 path 依赖复用。

## 特性

| 能力 | 说明 |
|---|---|
| 类型化实体 | `CacheObject` trait + `const fn` 字段描述，**无反射、无装箱** |
| 类型感知索引 | 文本（精确 + 大小写不敏感模糊 `*`/`?`）、整数/浮点/时间（`BTreeMap` 有序范围）、布尔（双桶 + 智能值）、JSON（文本键） |
| 唯一约束 | 单字段 `[Unique]` 与复合 `[CompositeUnique]`，支持 `allow_null`、更新排除自身、批量校验 |
| 查询 | 按 ID / 字段 / 范围 / 通配符 / 多条件交集 / 唯一值 / 复合键 / 分页 |
| 异步持久化 | 后台线程合并写盘（默认 500ms 窗口）、原子替换、`flush()` 同步落盘 |
| C# 互通 | 读写 `Data/{type_name}.json`，格式与 C# `SimpleJsonString` 双向兼容（裸键 + 注释 + 斜杠日期均可解析） |
| 多实例/全局 | `MemoryDb::new(dir)` 多实例隔离；`pek_rmdb::global()` 提供 `cdb` 风格静态入口 |
| 零依赖框架 | 仅依赖 `chrono` 与生态公共库 `dhrust`（时间解析/原子替换） |

## 快速开始

```rust
use pek_rmdb::{CacheObject, Field, FieldValue, MemoryDb};

#[derive(Clone, Debug, Default)]
struct User {
    id: i64,
    name: String,
    username: String,
    email: String,
    age: i64,
}

static FIELDS: &[Field] = &[
    Field::text("Username").unique("用户名已存在"),
    Field::text("Email").unique_required("邮箱已被注册"),
    Field::int("Age"),
];

impl CacheObject for User {
    fn type_name() -> &'static str { "Demo.User" }          // 对应 Data/Demo.User.json
    fn id(&self) -> i64 { self.id }
    fn set_id(&mut self, id: i64) { self.id = id; }
    fn name(&self) -> &str { &self.name }
    fn set_name(&mut self, name: &str) { self.name = name.to_string(); }
    fn fields() -> &'static [Field] { FIELDS }

    fn get_field(&self, field: &str) -> Option<FieldValue> {
        match field {
            "Username" => Some(self.username.clone().into()),
            "Email" => Some(self.email.clone().into()),
            "Age" => Some(self.age.into()),
            _ => None,
        }
    }

    fn set_field(&mut self, field: &str, value: &FieldValue) -> bool {
        match field {
            "Username" => { self.username = value.to_text().unwrap_or_default(); true }
            "Email" => { self.email = value.to_text().unwrap_or_default(); true }
            "Age" => { self.age = value.to_i64().unwrap_or_default(); true }
            _ => false,
        }
    }
}

let db = MemoryDb::new("./AppData");
let id = db.insert(User {
    id: 0,
    name: "张三".into(),
    username: "zhangsan".into(),
    email: "zhangsan@example.com".into(),
    age: 30,
})?;

let user = db.find_by_id::<User>(id).unwrap();
let same = db.find_by_unique::<User>("Username", "zhangsan").unwrap();
let adults = db.find_by_range::<User>("Age", 18, 60);
let page = db.find_by_page::<User>(Some("Age"), 0, 20, true);
assert_eq!(user.username, same.username);
assert_eq!(adults.len(), 1);
assert_eq!(page.total_count, 1);
# Ok::<(), pek_rmdb::RmdbError>(())
```

复合唯一约束：

```rust
use pek_rmdb::{CompositeUniqueRule, Field};

static EMPLOYEE_FIELDS: &[Field] = &[
    Field::int("CompanyId"),
    Field::text("EmployeeNumber"),
    Field::text("IdCardNumber").unique("身份证号已存在"),
];

static EMPLOYEE_RULES: &[CompositeUniqueRule] = &[CompositeUniqueRule::new(
    "CompanyUnique",
    &["CompanyId", "EmployeeNumber"],
    "同一公司内工号必须唯一",
)];
// impl CacheObject for Employee { ...
//     fn composite_uniques() -> &'static [CompositeUniqueRule] { EMPLOYEE_RULES }
// }
```

## 字段与索引

| 构造器 | 类型 | 索引能力 |
|---|---|---|
| `Field::text(name)` | 文本 | 精确（区分大小写）、`find_by_like`（大小写不敏感：`*`/`?` 通配、前缀、包含、后缀）、范围（小写字典序） |
| `Field::int(name)` / `Field::float(name)` | 数值 | 精确、范围 `O(log n + k)` |
| `Field::bool(name)` | 布尔 | 精确、智能模式（`true/1/yes/on/是`、`false/0/no/off/否`） |
| `Field::time(name)` | 时间 | 精确、范围、年/年月/年月日（`find_by_like("2024-01")`） |
| `Field::json(name)` | 列表/映射 | 按 JSON 文本精确索引 |

修饰器：`.unique(msg)`（允许空）、`.unique_required(msg)`（不允许空）、`.not_indexed()`（不建普通索引）、`.not_save()`（不落盘）。
`Id` 与 `Name` 由框架自动索引，不需要写进 `fields()`。

## 常用 API

| 操作 | 实例方法 | 全局（`cdb` 风格） |
|---|---|---|
| 插入 | `insert` / `insert_batch` / `insert_by_index` | `pek_rmdb::insert(..)` |
| 更新 | `update` | `update(..)` |
| 删除 | `delete::<T>(id)` | `delete::<T>(id)` |
| 主键/全表 | `find_by_id` / `find_all` | 同名自由函数 |
| 字段/范围/模糊 | `find_by` / `find_by_range` / `find_by_like` | 同名自由函数 |
| 多条件/分页 | `find_by_multiple` / `find_by_page` | 同名自由函数 |
| 唯一键 | `find_by_unique` / `find_by_composite_unique` / `is_unique_value_exists` | 同名自由函数 |
| 维护 | `clear::<T>` / `clear_all` / `flush` / `statistics` | `clear` / `flush` / `statistics` |

写操作返回 [`RmdbError`]（唯一冲突可用 `error.unique()` 取详情）；读操作返回 `Arc<T>`（零拷贝，可直接跨线程共享）。

## 持久化

- 路径：`{base_dir}/Data/{type_name}.json`；纯内存库用 `MemoryDb::in_memory()`（`base_dir` 为空即不落盘）。
- 格式：与 C# 版一致（裸键、`\r\n` + `\t`、`Name` 在前）；**Rust 写出的文件可被 C# 版直接读取，C# 写出的文件也可被 Rust 读取**（含 `//` 注释与 `2024/1/5 14:30:00` 日期）。
- 后台线程：写操作只置脏并唤醒线程；默认 500ms 内的多次修改合并为一次序列化 + 一次原子写；无脏数据时线程在条件变量上无限等待（零轮询）。
- `flush()` 可随时同步落盘；`MemoryDb` 释放时默认也会 flush（`DbOptions::flush_on_drop`）。
- 整体类型不落盘：实现 `CacheObject::not_save() -> true`（对应 C# 类级 `[NotSave]`）。

## 性能（Release，Windows / i7 级桌面 CPU，详见 `docs/审计报告-第三轮-下沉与性能-2026-10-25.md`）

| 操作 | 第二轮 | 第三轮（FxHash） | 备注 |
|---|---|---|---|
| 插入 10 万行（11 索引 + 3 唯一） | 6.8~6.9 µs/行 | **6.81 µs/行**（14.7 万行/秒） | 瓶颈在唯一/索引维护 |
| `find_by_id` | 120~142 ns | **107 ns** | 读锁 + FxHash 查 + `Arc` 克隆 |
| `find_by_unique` | 429~436 ns | **393 ns** | 含查询字符串构造 |
| `find_by(Age)`（低基数，均返 3333 行） | 362~369 µs | **163.7 µs** | 33 万次 `by_id` 查找提速 |
| `find_by_range`（均返 16670 行） | 834~861 µs | **410.3 µs** | 同上 |
| `update` | 3.05~3.23 µs/行 | **3.07 µs/行** | 按字段 diff，只重建变化索引 |
| `find_by_page`（索引顺序） | 8~9 µs | **7 µs** | 不排序，按页取 |
| `find_by_page`（无索引字段 / 无排序） | 19.8 ms / 1.35 ms | 20.3 ms / 1.32 ms | 全量键 + 部分选择（10 万行） |
| 并发 `find_by_id` 8 线程 | 668~740 万次/秒 | **817 万次/秒** | 读读并发无阻塞 |
| 全量写盘（10 万行） | 126~128 ms / 20.3 MB | **124.5 ms / 20.3 MB** | 500ms 合并、原子替换 |

C# 版 README 基准（FindById ≈1 µs、Insert ≈10 µs、唯一查找 ≈1 µs）：Rust 版在同等/更多索引下分别约为 **9.3× / 1.5× / 2.5×** 更快。

## 构建与测试

```powershell
cargo check
cargo test
cargo clippy --all-targets -- -D warnings
cargo test --release --test perf_guard -- --ignored --nocapture --test-threads=1
```

依赖下载走项目内 `.cargo/config.toml`（rsproxy 镜像）；日常构建可用 `cargo build --profile fast`。

公共能力（不重复实现）：`dhrust::json`（宽松解析/转义/数字格式化）、`dhrust::text::parse_bool`、`dhrust::times`（时间解析 + 日期模式区间 `date_range_of`）、`dhrust::io`（原子写/通配匹配/`sanitize_file_name`/`exe_dir`）、`dhrust::web::Paged`（分页结构）。

哈希容器：内部使用 FxHash（`src/hash.rs`）而非标准 `SipHash`——键来自可信业务实体、无需抗碰撞 DoS，读路径因此快 5~15%。

## 设计文档

- `docs/与C#对照.md`：API/特性映射、有意差异与 C# 缺陷修正清单；
- `docs/审计报告.md`：首轮性能与资源审计；
- `docs/审计报告-第二轮-下沉与性能-2026-10-25.md`：下沉候选矩阵、update diff 优化（3.5×）与全量审计；
- `docs/审计报告-第三轮-下沉与性能-2026-10-25.md`：第三轮下沉（4 项）、FxHash 容器优化与全量审计；
- `docs/决策记录.md`：每轮决策（含用户原话）；
- `docs/项目接续指南.md` / `docs/用户档案.md` / `docs/项目记忆.md`：跨会话接续。

## License

MIT
