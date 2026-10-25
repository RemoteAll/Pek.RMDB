//! 错误类型。

use std::fmt;
use std::io;
use std::path::PathBuf;

use crate::value::FieldValue;

/// 约束类型（对齐 C# `UniqueConstraintViolationException.ConstraintType`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConstraintType {
    /// 单字段唯一约束。
    Single,
    /// 复合字段唯一约束。
    Composite,
}

impl ConstraintType {
    /// 文本名称（与 C# 一致）。
    pub fn as_str(self) -> &'static str {
        match self {
            ConstraintType::Single => "Single",
            ConstraintType::Composite => "Composite",
        }
    }
}

impl fmt::Display for ConstraintType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 唯一约束冲突详情（对齐 C# `UniqueConstraintViolationException`）。
#[derive(Clone, Debug)]
pub struct UniqueViolation {
    /// 违反约束的字段名（复合约束时为字段列表，如 `CompanyId, EmployeeNumber`）。
    pub property_name: String,
    /// 违反约束的值。
    pub value: Option<FieldValue>,
    /// 约束类型。
    pub constraint_type: ConstraintType,
    /// 冲突对象 ID（复合约束或批量预检时可能未知）。
    pub conflicting_id: Option<i64>,
    /// 业务错误消息（来自 `Field::unique` / `CompositeUniqueRule::message`）。
    pub message: String,
}

impl fmt::Display for UniqueViolation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)?;
        if let Some(id) = self.conflicting_id {
            write!(f, "（冲突对象 ID：{id}）")?;
        }
        Ok(())
    }
}

/// 内存数据库错误。
#[derive(Debug)]
pub enum RmdbError {
    /// 唯一约束冲突。
    Unique(UniqueViolation),
    /// 对象不存在（更新/删除时按 ID 未找到）。
    NotFound(String),
    /// 字段不存在或不可写。
    InvalidField(String),
    /// 字段值类型不匹配。
    TypeMismatch {
        /// 期望的类型。
        expected: &'static str,
        /// 实际类型。
        got: &'static str,
    },
    /// 持久化 IO 错误。
    Io {
        /// 文件路径。
        path: PathBuf,
        /// 底层错误。
        source: io::Error,
    },
    /// JSON 解析错误。
    Json {
        /// 文件路径（空表示内存字符串解析）。
        path: PathBuf,
        /// 错误消息。
        message: String,
    },
}

impl RmdbError {
    /// 唯一约束冲突详情（非唯一冲突返回 `None`）。
    pub fn unique(&self) -> Option<&UniqueViolation> {
        match self {
            RmdbError::Unique(v) => Some(v),
            _ => None,
        }
    }

    /// 是否为唯一约束冲突。
    pub fn is_unique_violation(&self) -> bool {
        matches!(self, RmdbError::Unique(_))
    }

    pub(crate) fn io(path: impl Into<PathBuf>, source: io::Error) -> Self {
        RmdbError::Io {
            path: path.into(),
            source,
        }
    }

    pub(crate) fn json(path: impl Into<PathBuf>, message: impl Into<String>) -> Self {
        RmdbError::Json {
            path: path.into(),
            message: message.into(),
        }
    }
}

impl fmt::Display for RmdbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RmdbError::Unique(v) => write!(f, "唯一约束校验失败：{v}"),
            RmdbError::NotFound(name) => write!(f, "对象不存在：{name}"),
            RmdbError::InvalidField(name) => write!(f, "字段不存在或不可写：{name}"),
            RmdbError::TypeMismatch { expected, got } => {
                write!(f, "字段值类型不匹配：期望 {expected}，实际 {got}")
            }
            RmdbError::Io { path, source } => {
                write!(f, "持久化 IO 失败：{}：{source}", path.display())
            }
            RmdbError::Json { path, message } => {
                if path.as_os_str().is_empty() {
                    write!(f, "JSON 解析失败：{message}")
                } else {
                    write!(f, "JSON 解析失败：{}：{message}", path.display())
                }
            }
        }
    }
}

impl std::error::Error for RmdbError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            RmdbError::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// 统一结果类型。
pub type Result<T> = std::result::Result<T, RmdbError>;
