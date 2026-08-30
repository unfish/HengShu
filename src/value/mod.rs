//! 运行时值：值枚举与基础值类型。
//!
//! 环境（`env`）、显示（`display`）、相等与排序（`equality`）、集合限制（`limits`）、
//! 可移植编解码（`portable`）和资源持有类型（`resources`）各自成模块，
//! 使本文件只回答“值是什么”，不再兼任所有运行时资源的注册表。

use crate::ast::{BindingOperand, CapturedBinding, MethodId, Param, Stmt, TypeId};
use crate::bytecode::BytecodeProgram;
use chrono::{DateTime, Duration, FixedOffset};
use rust_decimal::Decimal;
use std::cell::RefCell;
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fmt;
use std::path::PathBuf;
use std::rc::Rc;

mod display;
mod env;
mod equality;
mod limits;
mod portable;
mod resources;

pub type Rows = Vec<BTreeMap<String, Value>>;

pub use crate::stream::{StreamOperation, StreamSource, StreamValue};
pub use env::{Env, EnvRef};
pub use equality::{
    ensure_acyclic_insertion, float_order, mixed_numeric_message, money_order, values_equal,
};
pub use limits::{ensure_collection_limit, ensure_collection_limit_with_depth};
pub use portable::{PortableCodec, PortableValue};
pub use resources::{
    ChannelValue, DatabaseValue, HttpBody, HttpResponseValue, SocketValue, TaskValue,
};

const MAX_VALUE_NESTING: usize = 256;
type ContainerKey = (usize, u8);

pub(crate) const BUILTIN_TYPE_NAMES: &[&str] = &[
    "任意",
    "空",
    "逻辑",
    "数字",
    "浮点数",
    "文本",
    "列表",
    "记录",
    "功能",
    "结构",
    "结构实例",
    "模块",
    "表格",
    "分组",
    "数值数组",
    "回归结果",
    "字节",
    "集合",
    "路径",
    "日期时间",
    "持续时间",
    "范围",
    "错误",
    "结果",
    "网络响应",
    "数据库连接",
    "网络套接字",
    "任务",
    "通道",
    "流",
    "金额",
];

thread_local! {
    static DISPLAY_CONTAINERS: RefCell<HashSet<ContainerKey>> = RefCell::new(HashSet::new());
}

#[derive(Clone)]
pub enum Value {
    Null,
    Bool(bool),
    Number(Decimal),
    Float(f64),
    Text(String),
    List(Rc<RefCell<Vec<Value>>>),
    Record(Rc<RefCell<BTreeMap<String, Value>>>),
    Function(Rc<UserFunction>),
    BytecodeFunction(Rc<BytecodeClosure>),
    Native(String),
    StructType(Rc<StructType>),
    StructInstance(Rc<RefCell<StructInstance>>),
    Module(Rc<Module>),
    Table(Rc<RefCell<Table>>),
    Grouped(Rc<Grouped>),
    NumericArray(Rc<NumericArray>),
    Regression(Rc<RegressionResult>),
    Bytes(Rc<RefCell<Vec<u8>>>),
    Set(Rc<RefCell<Vec<Value>>>),
    Path(Rc<PathBuf>),
    DateTime(DateTime<FixedOffset>),
    Duration(Duration),
    Range(Rc<RangeValue>),
    Error(Rc<RuntimeIssue>),
    Outcome(Rc<OutcomeValue>),
    HttpResponse(Rc<HttpResponseValue>),
    Database(Rc<DatabaseValue>),
    Socket(Rc<RefCell<SocketValue>>),
    Task(Rc<TaskValue>),
    Channel(Rc<ChannelValue>),
    Stream(Rc<RefCell<StreamValue>>),
    Money(Rc<MoneyValue>),
}

#[derive(Clone)]
pub struct RuntimeIssue {
    pub category: String,
    pub message: String,
    pub line: usize,
    pub definition_span: Option<Box<crate::diagnostic::SourceSpan>>,
    pub stack: Vec<String>,
    pub cause: Option<Box<RuntimeIssue>>,
}

#[derive(Clone)]
pub enum OutcomeValue {
    Success(Value),
    Failure(RuntimeIssue),
}

#[derive(Clone)]
pub struct RangeValue {
    pub start: i64,
    pub end: i64,
    pub inclusive: bool,
    pub step: i64,
}

#[derive(Clone, Debug)]
pub struct MoneyValue {
    pub amount: Decimal,
    pub currency: String,
    pub scale: u32,
    pub rounding: String,
    pub audit: Vec<AuditEntry>,
}

/// 审计记录条目：同一动作按次数折叠，使金额值的审计链不随运算次数线性追加。
#[derive(Clone, Debug)]
pub struct AuditEntry {
    pub action: String,
    pub count: usize,
}

impl AuditEntry {
    pub fn new(action: impl Into<String>) -> Self {
        Self {
            action: action.into(),
            count: 1,
        }
    }
}

impl fmt::Display for AuditEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.count > 1 {
            write!(f, "{}（{}次）", self.action, self.count)
        } else {
            write!(f, "{}", self.action)
        }
    }
}

#[derive(Clone)]
pub struct UserFunction {
    pub name: String,
    pub params: Vec<Param>,
    pub body: Vec<Stmt>,
    pub closure: EnvRef,
    pub captures: Vec<CapturedBinding>,
    pub semantic_bindings: Vec<(crate::diagnostic::SourceSpan, bool, BindingOperand)>,
    pub semantic_types: Vec<crate::ast::SemanticType>,
    pub semantic_methods: Vec<crate::ast::SemanticMethod>,
    pub self_binding: Option<BindingOperand>,
}

#[derive(Clone)]
pub struct BytecodeClosure {
    pub name: String,
    pub function: u32,
    pub program: Rc<BytecodeProgram>,
    pub closure: EnvRef,
}

#[derive(Clone)]
pub struct StructType {
    pub id: TypeId,
    pub name: String,
    pub fields: Vec<String>,
    pub methods: RefCell<BTreeMap<MethodId, UserMethod>>,
    pub method_names: RefCell<BTreeMap<String, MethodId>>,
    pub module_id: usize,
}

#[derive(Clone)]
pub struct UserMethod {
    pub value: Value,
    pub public: bool,
}

#[derive(Clone)]
pub struct StructInstance {
    pub struct_type: Rc<StructType>,
    pub fields: BTreeMap<String, Value>,
}

#[derive(Clone)]
pub struct Module {
    pub name: String,
    pub exports: BTreeMap<String, Value>,
    pub interface: ModuleInterface,
    pub environment: Option<EnvRef>,
}

#[derive(Clone, Default)]
pub struct ModuleInterface {
    pub public_values: BTreeSet<String>,
    pub public_types: BTreeSet<String>,
    pub public_methods: BTreeSet<String>,
}

impl Module {
    pub fn new(
        name: impl Into<String>,
        exports: BTreeMap<String, Value>,
        environment: Option<EnvRef>,
    ) -> Self {
        let mut interface = ModuleInterface::default();
        for (name, value) in &exports {
            interface.public_values.insert(name.clone());
            if let Value::StructType(struct_type) = value {
                interface.public_types.insert(name.clone());
                for (method_name, method_id) in struct_type.method_names.borrow().iter() {
                    if struct_type
                        .methods
                        .borrow()
                        .get(method_id)
                        .is_some_and(|definition| definition.public)
                    {
                        interface
                            .public_methods
                            .insert(format!("{name}.{method_name}"));
                    }
                }
            }
        }
        Self {
            name: name.into(),
            exports,
            interface,
            environment,
        }
    }

    pub fn public_value(&self, name: &str) -> Option<&Value> {
        self.interface
            .public_values
            .contains(name)
            .then(|| self.exports.get(name))
            .flatten()
    }
}

impl Drop for Module {
    fn drop(&mut self) {
        if let Some(environment) = self.environment.take() {
            let mut environment = environment.borrow_mut();
            environment.values.clear();
            environment.resolved_values.clear();
            environment.parent = None;
        }
    }
}

#[derive(Clone, Default)]
pub struct Table {
    pub rows: Rows,
    pub metadata: crate::execution::Metadata,
    pub column_metadata: BTreeMap<String, crate::execution::Metadata>,
}

impl Table {
    /// 表级与列级敏感标签的并集：任何一列敏感，整表外发就要授权。
    /// 只有所有带标签的来源都已脱敏或假名化，整表才算已处理。
    pub fn sensitivity(&self) -> crate::execution::Sensitivity {
        let mut sensitivity = self.metadata.sensitivity.clone();
        let mut contributors = Vec::new();
        if !sensitivity.labels.is_empty() {
            contributors.push(&self.metadata.sensitivity);
        }
        for metadata in self.column_metadata.values() {
            if metadata.sensitivity.labels.is_empty() {
                continue;
            }
            sensitivity
                .labels
                .extend(metadata.sensitivity.labels.iter().cloned());
            contributors.push(&metadata.sensitivity);
        }
        if !contributors.is_empty() {
            sensitivity.masked = contributors.iter().all(|entry| entry.masked);
            sensitivity.pseudonymized = contributors.iter().all(|entry| entry.pseudonymized);
        }
        sensitivity
    }
}

#[derive(Clone)]
pub struct Grouped {
    pub rows: Rows,
    pub column: String,
}

#[derive(Clone)]
pub struct NumericArray {
    pub data: Vec<f64>,
    pub rows: usize,
    pub cols: usize,
}

/// OLS 影响诊断：只在线性回归路径产生，复用同一个 `(XᵗX)⁻¹`。
#[derive(Clone)]
pub struct RegressionDiagnostics {
    pub standardized_residuals: Vec<f64>,
    pub leverage: Vec<f64>,
    pub cooks_distance: Vec<f64>,
    /// 按预测变量顺序给出，不含截距项。
    pub variance_inflation: Vec<f64>,
}

/// 逻辑回归的分类质量摘要：只在逻辑回归路径产生。
#[derive(Clone)]
pub struct ClassificationMetrics {
    pub log_loss: f64,
    pub accuracy: f64,
    pub true_positive: usize,
    pub false_positive: usize,
    pub true_negative: usize,
    pub false_negative: usize,
    pub roc_auc: f64,
}

#[derive(Clone)]
pub struct RegressionResult {
    pub coefficients: Vec<f64>,
    pub r_squared: f64,
    pub adjusted_r_squared: f64,
    pub p_values: Vec<f64>,
    pub standard_errors: Vec<f64>,
    pub fitted: Vec<f64>,
    pub residuals: Vec<f64>,
    pub rmse: f64,
    pub mae: f64,
    pub diagnostics: Option<RegressionDiagnostics>,
    pub classification: Option<ClassificationMetrics>,
}

impl Value {
    pub fn list(values: Vec<Value>) -> Self {
        Self::List(Rc::new(RefCell::new(values)))
    }
    pub fn record(values: BTreeMap<String, Value>) -> Self {
        Self::Record(Rc::new(RefCell::new(values)))
    }
    pub fn type_name(&self) -> &'static str {
        match self {
            Self::Null => "空",
            Self::Bool(_) => "逻辑",
            Self::Number(_) => "数字",
            Self::Float(_) => "浮点数",
            Self::Text(_) => "文本",
            Self::List(_) => "列表",
            Self::Record(_) => "记录",
            Self::Function(_) | Self::BytecodeFunction(_) | Self::Native(_) => "功能",
            Self::StructType(_) => "结构",
            Self::StructInstance(_) => "结构实例",
            Self::Module(_) => "模块",
            Self::Table(_) => "表格",
            Self::Grouped(_) => "分组",
            Self::NumericArray(_) => "数值数组",
            Self::Regression(_) => "回归结果",
            Self::Bytes(_) => "字节",
            Self::Set(_) => "集合",
            Self::Path(_) => "路径",
            Self::DateTime(_) => "日期时间",
            Self::Duration(_) => "持续时间",
            Self::Range(_) => "范围",
            Self::Error(_) => "错误",
            Self::Outcome(_) => "结果",
            Self::HttpResponse(_) => "网络响应",
            Self::Database(_) => "数据库连接",
            Self::Socket(_) => "网络套接字",
            Self::Task(_) => "任务",
            Self::Channel(_) => "通道",
            Self::Stream(_) => "流",
            Self::Money(_) => "金额",
        }
    }
    pub fn actual_type_display(&self, expected: &str) -> String {
        match self {
            Self::StructInstance(instance) => {
                let name = instance.borrow().struct_type.name.clone();
                if name == expected {
                    format!("另一处声明的{name}")
                } else {
                    name
                }
            }
            _ => self.type_name().to_string(),
        }
    }
    pub fn truth(&self) -> bool {
        match self {
            Self::Null => false,
            Self::Bool(v) => *v,
            Self::Number(v) => !v.is_zero(),
            Self::Float(v) => *v != 0.0 && !v.is_nan(),
            Self::Text(v) => !v.is_empty(),
            Self::List(v) => !v.borrow().is_empty(),
            Self::Bytes(v) => !v.borrow().is_empty(),
            Self::Set(v) => !v.borrow().is_empty(),
            Self::Outcome(v) => matches!(v.as_ref(), OutcomeValue::Success(_)),
            _ => true,
        }
    }
    pub fn as_f64(&self) -> Result<f64, String> {
        match self {
            Self::Number(v) => v.to_string().parse().map_err(|_| "无法转换为数字".into()),
            Self::Float(v) => Ok(*v),
            Self::Bool(v) => Ok(if *v { 1.0 } else { 0.0 }),
            Self::Text(v) => v.parse().map_err(|_| "无法转换为数字".into()),
            Self::Money(v) => v
                .amount
                .to_string()
                .parse()
                .map_err(|_| "无法转换为数字".into()),
            _ => Err("需要数字".into()),
        }
    }
    pub fn as_decimal(&self) -> Result<Decimal, String> {
        match self {
            Self::Number(v) => Ok(*v),
            Self::Float(v) => {
                Decimal::from_str_exact(&v.to_string()).map_err(|_| "无法转换为数字".into())
            }
            Self::Bool(v) => Ok(Decimal::from(if *v { 1 } else { 0 })),
            Self::Text(v) => v.parse().map_err(|_| "无法转换为数字".into()),
            Self::Money(v) => Ok(v.amount),
            _ => Err("需要数字".into()),
        }
    }
    pub fn iter_values(&self) -> Result<Vec<Value>, String> {
        self.iter_values_limited(crate::resource::DEFAULT_MAX_COLLECTION_ITEMS)
    }

    pub fn iter_values_limited(&self, limit: usize) -> Result<Vec<Value>, String> {
        match self {
            Self::List(v) => limited_clone(&v.borrow(), limit),
            Self::Text(v) => collect_limited(v.chars().map(|c| Self::Text(c.to_string())), limit),
            Self::Table(v) => {
                collect_limited(v.borrow().rows.iter().cloned().map(Self::record), limit)
            }
            Self::NumericArray(v) => collect_limited(v.data.iter().map(|x| Self::Float(*x)), limit),
            Self::Bytes(v) => collect_limited(
                v.borrow().iter().map(|x| Self::Number(Decimal::from(*x))),
                limit,
            ),
            Self::Set(v) => limited_clone(&v.borrow(), limit),
            Self::Range(v) => {
                if v.step == 0 {
                    return Err("范围步长不能为零".into());
                }
                let mut current = v.start;
                let mut values = Vec::new();
                let keep = |n: i64| {
                    if v.step > 0 {
                        if v.inclusive {
                            n <= v.end
                        } else {
                            n < v.end
                        }
                    } else if v.inclusive {
                        n >= v.end
                    } else {
                        n > v.end
                    }
                };
                while keep(current) {
                    if values.len() >= limit {
                        return Err("集合项数超过资源限制".into());
                    }
                    values.push(Self::Number(Decimal::from(current)));
                    if current == v.end && v.inclusive {
                        break;
                    }
                    let Some(next) = current.checked_add(v.step) else {
                        break;
                    };
                    current = next;
                }
                Ok(values)
            }
            _ => Err(format!("{}不能被遍历", self.type_name())),
        }
    }
}

fn limited_clone(values: &[Value], limit: usize) -> Result<Vec<Value>, String> {
    if values.len() > limit {
        Err("集合项数超过资源限制".into())
    } else {
        Ok(values.to_vec())
    }
}

fn collect_limited(
    values: impl IntoIterator<Item = Value>,
    limit: usize,
) -> Result<Vec<Value>, String> {
    let mut output = Vec::new();
    for value in values {
        if output.len() >= limit {
            return Err("集合项数超过资源限制".into());
        }
        output.push(value);
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn endpoint_ranges_terminate_and_containers_are_reflexive() {
        let maximum = Value::Range(Rc::new(RangeValue {
            start: i64::MAX - 1,
            end: i64::MAX,
            inclusive: true,
            step: 1,
        }));
        assert_eq!(maximum.iter_values().unwrap().len(), 2);
        let minimum = Value::Range(Rc::new(RangeValue {
            start: i64::MIN + 1,
            end: i64::MIN,
            inclusive: true,
            step: -1,
        }));
        assert_eq!(minimum.iter_values().unwrap().len(), 2);

        let list = Value::list(vec![Value::Number(Decimal::ONE)]);
        let record = Value::record(BTreeMap::from([("值".into(), list.clone())]));
        let set = Value::Set(Rc::new(RefCell::new(vec![record.clone()])));
        assert!(values_equal(&list, &list));
        assert!(values_equal(&record, &record));
        assert!(values_equal(&set, &set));
    }

    #[test]
    fn table_sensitivity_unions_column_labels() {
        use crate::execution::SensitiveLabel;
        let labeled = |label: SensitiveLabel, masked: bool| {
            let mut metadata = crate::execution::Metadata::default();
            metadata.sensitivity.labels.insert(label);
            metadata.sensitivity.masked = masked;
            metadata
        };
        let mut table = Table::default();
        assert!(table.sensitivity().labels.is_empty());
        table
            .column_metadata
            .insert("工资".into(), labeled(SensitiveLabel::Financial, true));
        assert_eq!(
            table.sensitivity().labels,
            BTreeSet::from([SensitiveLabel::Financial])
        );
        table
            .column_metadata
            .insert("身份证".into(), labeled(SensitiveLabel::Identity, false));
        let sensitivity = table.sensitivity();
        assert_eq!(
            sensitivity.labels,
            BTreeSet::from([SensitiveLabel::Financial, SensitiveLabel::Identity])
        );
        assert!(!sensitivity.masked);
    }
}
