//! 唯一约束索引与校验（对应 C# `UniqueConstraintManager`）。
//!
//! 接口按“字段描述 + 取值器”设计（而非泛型实体），这样泛型写路径与
//! 类型擦除的加载/持久化路径可以共用同一套索引逻辑。
//!
//! 与 C# 版差异：
//! - 键值先按字段类型规范化（`i32` 与 `i64` 的 1 视为同值），比 `object` 相等更符合直觉；
//! - 数据文件加载时若发现冲突，**跳过该对象**（C# 版只记警告但对象仍在表中，
//!   会造成“对象存在、唯一索引缺失”的不一致）。

use std::collections::HashMap;

use crate::error::{ConstraintType, Result, RmdbError, UniqueViolation};
use crate::model::{CompositeUniqueRule, Field};
use crate::value::FieldValue;

/// 取值器：按字段名读取当前对象的值。
pub(crate) type ValueGetter<'a> = &'a dyn Fn(&str) -> Option<FieldValue>;

/// 唯一约束索引。
#[derive(Default)]
pub(crate) struct UniqueSet {
    /// 单字段：字段名 → 值 → 对象 ID。
    fields: HashMap<&'static str, HashMap<FieldValue, i64>>,
    /// 复合：约束组名 → 组合键 → 对象 ID。
    composites: HashMap<&'static str, HashMap<Vec<FieldValue>, i64>>,
}

impl UniqueSet {
    /// 校验对象的全部唯一约束（`exclude` 为更新时排除的自身 ID）。
    pub(crate) fn validate(
        &self,
        fields: &'static [Field],
        composites: &'static [CompositeUniqueRule],
        get: ValueGetter<'_>,
        exclude: Option<i64>,
    ) -> Result<()> {
        for field in fields {
            let Some(rule) = field.unique else {
                continue;
            };
            let value = get(field.name).unwrap_or(FieldValue::Null);
            if value.is_null() {
                if !rule.allow_null {
                    return Err(RmdbError::Unique(UniqueViolation {
                        property_name: field.name.to_string(),
                        value: None,
                        constraint_type: ConstraintType::Single,
                        conflicting_id: None,
                        message: format!("字段 {} 不允许为空值。{}", field.name, rule.message),
                    }));
                }
                continue;
            }
            let Some(key) = value.normalize(field.kind) else {
                continue;
            };
            if let Some(existing) = self
                .fields
                .get(field.name)
                .and_then(|index| index.get(&key))
                && exclude != Some(*existing)
            {
                return Err(RmdbError::Unique(UniqueViolation {
                    property_name: field.name.to_string(),
                    value: Some(key.clone()),
                    constraint_type: ConstraintType::Single,
                    conflicting_id: Some(*existing),
                    message: format!(
                        "字段 {} 的值 '{}' 已存在。{}",
                        field.name, key, rule.message
                    ),
                }));
            }
        }

        for rule in composites {
            let (key, has_null) = composite_key(fields, rule, get)?;
            if has_null {
                if rule.allow_null {
                    continue;
                }
                return Err(RmdbError::Unique(UniqueViolation {
                    property_name: rule.fields.join(", "),
                    value: Some(FieldValue::List(key)),
                    constraint_type: ConstraintType::Composite,
                    conflicting_id: None,
                    message: format!("复合约束 {} 不允许包含空值。{}", rule.group, rule.message),
                }));
            }
            if let Some(existing) = self
                .composites
                .get(rule.group)
                .and_then(|index| index.get(&key))
                && exclude != Some(*existing)
            {
                return Err(RmdbError::Unique(UniqueViolation {
                    property_name: rule.fields.join(", "),
                    value: Some(FieldValue::List(key)),
                    constraint_type: ConstraintType::Composite,
                    conflicting_id: Some(*existing),
                    message: format!("复合约束 {} 的组合值已存在。{}", rule.group, rule.message),
                }));
            }
        }
        Ok(())
    }

    /// 写入对象的唯一约束索引（两阶段：先全部校验，再统一写入；失败时索引保持不变）。
    pub(crate) fn add(
        &mut self,
        fields: &'static [Field],
        composites: &'static [CompositeUniqueRule],
        obj_id: i64,
        get: ValueGetter<'_>,
    ) -> Result<()> {
        let mut pending_fields: Vec<(&'static str, FieldValue)> = Vec::new();
        for field in fields {
            let Some(rule) = field.unique else {
                continue;
            };
            let value = get(field.name).unwrap_or(FieldValue::Null);
            if value.is_null() {
                if !rule.allow_null {
                    return Err(RmdbError::Unique(UniqueViolation {
                        property_name: field.name.to_string(),
                        value: None,
                        constraint_type: ConstraintType::Single,
                        conflicting_id: None,
                        message: format!("字段 {} 不允许为空值。{}", field.name, rule.message),
                    }));
                }
                continue;
            }
            let Some(key) = value.normalize(field.kind) else {
                continue;
            };
            if let Some(existing) = self
                .fields
                .get(field.name)
                .and_then(|index| index.get(&key))
            {
                return Err(RmdbError::Unique(UniqueViolation {
                    property_name: field.name.to_string(),
                    value: Some(key.clone()),
                    constraint_type: ConstraintType::Single,
                    conflicting_id: Some(*existing),
                    message: rule.message.to_string(),
                }));
            }
            pending_fields.push((field.name, key));
        }

        let mut pending_composites: Vec<(&'static str, Vec<FieldValue>)> = Vec::new();
        for rule in composites {
            let (key, has_null) = composite_key(fields, rule, get)?;
            if has_null {
                if rule.allow_null {
                    continue;
                }
                return Err(RmdbError::Unique(UniqueViolation {
                    property_name: rule.fields.join(", "),
                    value: Some(FieldValue::List(key)),
                    constraint_type: ConstraintType::Composite,
                    conflicting_id: None,
                    message: rule.message.to_string(),
                }));
            }
            if let Some(existing) = self
                .composites
                .get(rule.group)
                .and_then(|index| index.get(&key))
            {
                return Err(RmdbError::Unique(UniqueViolation {
                    property_name: rule.fields.join(", "),
                    value: Some(FieldValue::List(key)),
                    constraint_type: ConstraintType::Composite,
                    conflicting_id: Some(*existing),
                    message: rule.message.to_string(),
                }));
            }
            pending_composites.push((rule.group, key));
        }

        // 校验全部通过，写入不会失败
        for (name, key) in pending_fields {
            self.fields.entry(name).or_default().insert(key, obj_id);
        }
        for (group, key) in pending_composites {
            self.composites.entry(group).or_default().insert(key, obj_id);
        }
        Ok(())
    }

    /// 移除对象的唯一约束索引（对未写入的键同样安全）。
    pub(crate) fn remove(
        &mut self,
        fields: &'static [Field],
        composites: &'static [CompositeUniqueRule],
        get: ValueGetter<'_>,
    ) {
        for field in fields {
            if field.unique.is_none() {
                continue;
            }
            let value = get(field.name).unwrap_or(FieldValue::Null);
            let Some(key) = value.normalize(field.kind) else {
                continue;
            };
            if let Some(index) = self.fields.get_mut(field.name) {
                index.remove(&key);
                if index.is_empty() {
                    self.fields.remove(field.name);
                }
            }
        }

        for rule in composites {
            let Ok((key, has_null)) = composite_key(fields, rule, get) else {
                continue;
            };
            if has_null {
                continue;
            }
            if let Some(index) = self.composites.get_mut(rule.group) {
                index.remove(&key);
                if index.is_empty() {
                    self.composites.remove(rule.group);
                }
            }
        }
    }

    /// 单字段唯一值 → 对象 ID。
    pub(crate) fn find_value(&self, field: &str, key: &FieldValue) -> Option<i64> {
        self.fields
            .get(field)
            .and_then(|index| index.get(key))
            .copied()
    }

    /// 复合约束组合 → 对象 ID。
    pub(crate) fn find_composite(&self, group: &str, key: &[FieldValue]) -> Option<i64> {
        self.composites
            .get(group)
            .and_then(|index| index.get(key))
            .copied()
    }

    pub(crate) fn clear(&mut self) {
        self.fields.clear();
        self.composites.clear();
    }
}

/// 构造复合键（按约束声明的字段顺序；任一值为空时 `has_null = true`）。
fn composite_key(
    fields: &'static [Field],
    rule: &CompositeUniqueRule,
    get: ValueGetter<'_>,
) -> Result<(Vec<FieldValue>, bool)> {
    let mut key = Vec::with_capacity(rule.fields.len());
    let mut has_null = false;
    for name in rule.fields {
        let field = fields
            .iter()
            .find(|f| f.name == *name)
            .ok_or_else(|| RmdbError::InvalidField((*name).to_string()))?;
        let value = get(name).unwrap_or(FieldValue::Null);
        match value.normalize(field.kind) {
            Some(v) => key.push(v),
            None => {
                has_null = true;
                key.push(FieldValue::Null);
            }
        }
    }
    Ok((key, has_null))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::UniqueRule;

    static RULES: &[CompositeUniqueRule] = &[CompositeUniqueRule::new(
        "CompanyUnique",
        &["CompanyId", "EmployeeNumber"],
        "同一公司内工号必须唯一",
    )];

    static FIELDS: &[Field] = &[
        Field::text("Username").unique("用户名已存在"),
        Field::int("CompanyId"),
        Field::text("EmployeeNumber"),
    ];

    fn employee<'a>(
        username: &'a str,
        company: &'a i64,
        number: &'a str,
    ) -> impl Fn(&str) -> Option<FieldValue> + 'a {
        move |name| match name {
            "Username" => Some(username.into()),
            "CompanyId" => Some((*company).into()),
            "EmployeeNumber" => Some(number.into()),
            _ => None,
        }
    }

    #[test]
    fn rejects_duplicate_single_and_composite() {
        let mut set = UniqueSet::default();
        set.add(FIELDS, RULES, 1, &employee("zhangsan", &100, "E001"))
            .unwrap();

        let dup_name = employee("zhangsan", &100, "E002");
        assert!(set.validate(FIELDS, RULES, &dup_name, None).is_err());

        let dup_composite = employee("lisi", &100, "E001");
        let error = set
            .validate(FIELDS, RULES, &dup_composite, None)
            .unwrap_err();
        assert_eq!(
            error.unique().map(|v| v.constraint_type),
            Some(ConstraintType::Composite)
        );

        // 换公司后同工号允许
        let other_company = employee("lisi", &200, "E001");
        assert!(set.validate(FIELDS, RULES, &other_company, None).is_ok());
    }

    #[test]
    fn update_excludes_self() {
        let mut set = UniqueSet::default();
        let first = employee("zhangsan", &100, "E001");
        set.add(FIELDS, RULES, 1, &first).unwrap();
        assert!(set.validate(FIELDS, RULES, &first, Some(1)).is_ok());
        assert!(set.validate(FIELDS, RULES, &first, None).is_err());
        assert!(set.validate(FIELDS, RULES, &first, Some(2)).is_err());
    }

    #[test]
    fn allow_null_semantics() {
        static NULLABLE: &[Field] = &[
            Field::text("A").unique("A 已存在"),
            Field::text("B").unique_required("B 不允许为空"),
        ];

        let mut set = UniqueSet::default();
        let empty = |name: &str| -> Option<FieldValue> {
            match name {
                "A" | "B" => Some(FieldValue::Null),
                _ => None,
            }
        };
        assert!(set.add(NULLABLE, &[], 0, &empty).is_err(), "B 不允许为空");

        let with_b1 = |name: &str| -> Option<FieldValue> {
            match name {
                "A" => Some(FieldValue::Null),
                "B" => Some("b1".into()),
                _ => None,
            }
        };
        let with_b2 = |name: &str| -> Option<FieldValue> {
            match name {
                "A" => Some(FieldValue::Null),
                "B" => Some("b2".into()),
                _ => None,
            }
        };
        set.add(NULLABLE, &[], 1, &with_b1).unwrap();
        // A 允许空且不参与唯一校验，两行空 A 可以共存
        set.add(NULLABLE, &[], 2, &with_b2).unwrap();
    }

    #[test]
    fn unique_rule_defaults_allow_null() {
        static F: &[Field] = &[Field::text("A").unique("x")];
        assert_eq!(
            F[0].unique,
            Some(UniqueRule {
                message: "x",
                allow_null: true
            })
        );
    }

    #[test]
    fn null_composite_is_skipped_when_allowed() {
        static F: &[Field] = &[Field::int("CompanyId"), Field::text("EmployeeNumber")];
        static R: &[CompositeUniqueRule] = &[CompositeUniqueRule::new(
            "G",
            &["CompanyId", "EmployeeNumber"],
            "必须唯一",
        )];
        let mut set = UniqueSet::default();
        let first = |name: &str| -> Option<FieldValue> {
            match name {
                "CompanyId" => Some(FieldValue::Null),
                "EmployeeNumber" => Some("E1".into()),
                _ => None,
            }
        };
        set.add(F, R, 1, &first).unwrap();
        set.add(F, R, 2, &first).unwrap(); // 含空值：允许重复
    }
}
