# Pek.RMDB · 项目指令

> 全项目通用规则见用户级《跨项目接续协议》（本机 `%APPDATA%\Code\User\prompts\跨项目接续协议.instructions.md`；模板在 `tools/continuity/templates/`）。
> **开工先读（存在则读，按序）**：`.github/copilot-instructions.md` → `docs/项目接续指南.md` → `docs/用户档案.md` → `docs/项目记忆.md` → `docs/决策记录.md` 最新节。
> **机器自愈（有则快跑）**：`tools/continuity/install-global-instruction.ps1`（幂等；打开项目时 `.vscode/tasks.json` 也会自动跑）。

## 项目定位

`Pek.RMDB`（crate `pek-rmdb`）是 C# `Pek.MDB` 的 Rust 对应实现：**高性能内存数据库库**
（非服务、无面板），面向读多写少、内存为主、JSON 异步持久化场景，供生态内 Rust 项目以 path 依赖复用。

参照源码：`G:\Code\Pek.FrameWork\Pek.MDB`（`Data/Cache/MemoryDB.cs`、`TypedIndex/*`、
`UniqueConstraintManager.cs`、`Serialization/SimpleJsonString.cs`、`_ding/cdb.cs`）。

## 依赖约定（不要重复造轮子）

| 能力 | 复用来源 | 说明 |
|---|---|---|
| 时间解析/格式化 | `dhrust::times` | C# 兼容的多格式时间解析（`parse_datetime`） |
| JSON 宽松解析/转义 | `dhrust::json` | `JsonValue`/`parse_lax`（裸键/注释/尾逗号）、`write_quoted`（C# `ClearNewLine` 口径） |
| 原子替换文件 | `dhrust::io::safe_replace_file` | 先写临时文件再替换，防半截文件 |
| 时间类型 | `chrono` | 与 dhrust 同源 |
| 临时目录（测试） | `tempfile`（dev-dependency） | 集成测试隔离数据目录 |

- 不引入 async 运行时：库提供同步 API；需要异步的消费方自行 `spawn_blocking`。
- 不引入 serde：C# 持久化格式是**裸键** JSON，标准解析器读不了，库内自带宽松解析器（`src/json.rs`）。

## 硬约束

- **持久化格式必须与 C# 双向兼容**：写入 `SimpleJsonString` 风格（裸键、`\r\n` + `\t`、`Id`/`Name` 在前）；
  读取必须容忍注释、尾逗号、斜杠日期（`2024/1/5 14:30:00`）与带引号键。
- **读多写少锁策略**：整表 `RwLock<State>`；读路径只取读锁，写路径单写者；
  索引值集合用 `BTreeSet<i64>`（插入/删除 `O(log k)`、迭代有序），**禁止**改成 `Vec` + 线性去重。
- **索引值先规范化**（按 `FieldKind`），跨类型查询（`i32` 查 `i64`）必须命中。
- **唯一约束两阶段写入**：先全部校验、再统一落索引；失败不得改变任何索引状态。
- **加载时修正 ID 计数器**为 `max(id)+1`（C# 版未修正，加载后新增会从 1 覆盖数据）。
- **加载时唯一冲突跳过对象**（C# 版对象留在表中但唯一索引缺失，状态不一致）。
- **周期行为变化驱动**：后台持久化线程无脏数据时无限等待（零轮询）；写盘按最小间隔合并。
- `fields()` 不要包含 `Id`/`Name`（框架自动索引与序列化；重复声明会被跳过）。

## 目录约定

```
src/{lib,error,value,model,index,unique,table,db,persistence,json}.rs
tests/{common,crud,index,unique,persistence,concurrency,perf_guard}.rs
docs/{项目接续指南,用户档案,项目记忆,决策记录,审计报告,与C#对照}.md
tools/continuity/*
```

## 每轮收尾（务必）

① `docs/决策记录.md` 新增一节（**含用户原话**）；② 运行 `tools/continuity/sync-memory.ps1` 同步记忆（需在项目根目录运行或传 `-ProjectPath`）；③ 跑回归（`cargo test` + `cargo clippy --all-targets -- -D warnings`，必要时跑 perf_guard）并如实汇报；④ 提交并推送。
