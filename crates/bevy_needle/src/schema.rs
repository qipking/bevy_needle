//! 工具 JSON Schema 的构建与校验。
//!
//! Needle 的解码器按声明的 schema 生成字节级语法约束：schema 描述得越
//! 精确（枚举、上下界），模型的输出就越可靠。本模块提供：
//!
//! - [`normalize_tool_schema`]: 校验并规范化 `ToolSpec` 中的 parameters；
//! - [`ParametersBuilder`]: 便于游戏代码声明参数的构建器（枚举/区间/长度）。

use serde_json::{json, Map, Value};

#[derive(Debug, thiserror::Error)]
#[error("工具 schema 无效: {0}")]
/// `ToolSchemaError`（见类型级与模块级文档）。
pub struct ToolSchemaError(pub String);

/// 规范化并校验单个工具的 parameters。
///
/// - `type` 必须为 `"object"`（缺省时补全）；
/// - `properties` 必须是对象（缺省时补全为空对象）;
/// - `required` 中的每个名字必须出现在 `properties` 里。
pub fn normalize_tool_schema(parameters: &Value) -> Result<Value, ToolSchemaError> {
    let mut params = parameters.clone();
    if !params.is_object() {
        return Err(ToolSchemaError(format!(
            "parameters 必须是 JSON 对象，得到: {params}"
        )));
    }
    let obj = params.as_object_mut().expect("checked above");
    match obj.get("type") {
        Some(Value::String(t)) if t == "object" => {}
        Some(other) => {
            return Err(ToolSchemaError(format!(
                "parameters.type 必须是 \"object\"，得到: {other}"
            )))
        }
        None => {
            obj.insert("type".into(), json!("object"));
        }
    }
    match obj.get_mut("properties") {
        Some(Value::Object(_)) => {}
        Some(_) => return Err(ToolSchemaError("properties 必须是 JSON 对象".into())),
        None => {
            obj.insert("properties".into(), Value::Object(Map::new()));
        }
    }
    // 先拷贝 required 名单，避免与 properties 的不可变借用冲突
    let required_names: Vec<String> = obj
        .get("required")
        .and_then(|r| r.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    if let Some(raw_required) = obj.get("required") {
        if !raw_required.is_array() {
            return Err(ToolSchemaError("required 必须是字符串数组".into()));
        }
    }
    for name in &required_names {
        let defined = obj.get("properties").and_then(|p| p.get(name)).is_some();
        if !defined {
            return Err(ToolSchemaError(format!(
                "required 中的 {name:?} 没有对应的 property"
            )));
        }
    }
    Ok(params)
}

/// 参数构建器：为工具声明 ergonomics 的 object schema。
#[derive(Debug, Default, Clone)]
pub struct ParametersBuilder {
    properties: Map<String, Value>,
    required: Vec<String>,
}

impl ParametersBuilder {
    /// 构造/执行入口（错误经 `Result` 返回，不 panic）。
    pub fn new() -> Self {
        Self::default()
    }

    fn push(&mut self, name: &str, mut schema: Value, description: &str, required: bool) -> &mut Self {
        if !description.is_empty() {
            schema["description"] = Value::String(description.to_string());
        }
        self.properties.insert(name.to_string(), schema);
        if required {
            self.required.push(name.to_string());
        }
        self
    }

    /// 字符串参数（可带枚举选择，枚举会被编译进解码语法）。
    pub fn str_enum(mut self, name: &str, choices: &[&str], description: &str) -> Self {
        let schema = json!({ "type": "string", "enum": choices });
        self.push(name, schema, description, true);
        self
    }

    /// 构造/执行入口（错误经 `Result` 返回，不 panic）。
    pub fn string(mut self, name: &str, description: &str) -> Self {
        let schema = json!({ "type": "string" });
        self.push(name, schema, description, true);
        self
    }

    /// 可选字符串（未出现证据时模型会省略该字段）。
    pub fn optional_string(mut self, name: &str, description: &str) -> Self {
        let schema = json!({ "type": "string" });
        self.push(name, schema, description, false);
        self
    }

    /// 有界整数（minimum/maximum 进入解码语法）。
    pub fn int_range(mut self, name: &str, min: i64, max: i64, description: &str) -> Self {
        let schema = json!({ "type": "integer", "minimum": min, "maximum": max });
        self.push(name, schema, description, true);
        self
    }

    /// 构造/执行入口（错误经 `Result` 返回，不 panic）。
    pub fn integer(mut self, name: &str, description: &str) -> Self {
        let schema = json!({ "type": "integer" });
        self.push(name, schema, description, true);
        self
    }

    /// 构造/执行入口（错误经 `Result` 返回，不 panic）。
    pub fn number(mut self, name: &str, description: &str) -> Self {
        let schema = json!({ "type": "number" });
        self.push(name, schema, description, true);
        self
    }

    /// 构造/执行入口（错误经 `Result` 返回，不 panic）。
    pub fn boolean(mut self, name: &str, description: &str) -> Self {
        let schema = json!({ "type": "boolean" });
        self.push(name, schema, description, true);
        self
    }

    /// 构造/执行入口（错误经 `Result` 返回，不 panic）。
    pub fn optional_boolean(mut self, name: &str, description: &str) -> Self {
        let schema = json!({ "type": "boolean" });
        self.push(name, schema, description, false);
        self
    }

    /// 构造/执行入口（错误经 `Result` 返回，不 panic）。
    pub fn required(mut self, names: &[&str]) -> Self {
        for name in names {
            if !self.required.iter().any(|existing| existing == name) && self.properties.contains_key(*name) {
                self.required.push((*name).to_string());
            }
        }
        self
    }

    /// 构造/执行入口（错误经 `Result` 返回，不 panic）。
    pub fn build(self) -> Value {
        json!({
            "type": "object",
            "properties": Value::Object(self.properties),
            "required": self.required,
        })
    }
}

/// 把一组规范化后的 schema 序列化成 `needle_init` 需要的 JSON 数组字符串。
pub fn tools_json(schemas: &[Value]) -> Result<String, ToolSchemaError> {
    serde_json::to_string(schemas)
        .map_err(|e| ToolSchemaError(format!("序列化失败: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn fills_missing_type_and_properties() {
        let raw = json!({});
        let normalized = normalize_tool_schema(&raw).unwrap();
        assert_eq!(normalized["type"], "object");
        assert!(normalized["properties"].is_object());
    }

    #[test]
    fn rejects_required_outside_properties() {
        let raw = json!({ "type": "object", "required": ["missing"] });
        assert!(normalize_tool_schema(&raw).is_err());
    }

    #[test]
    fn builder_emits_constrained_schema() {
        let params = ParametersBuilder::new()
            .str_enum("target", &["title", "status"], "要修改的标签")
            .int_range("value", 0, 100, "百分比值")
            .optional_boolean("on", "开关")
            .build();
        let normalized = normalize_tool_schema(&params).unwrap();
        assert_eq!(normalized["required"], json!(["target", "value"]));
        assert_eq!(normalized["properties"]["target"]["enum"], json!(["title", "status"]));
    }
}
