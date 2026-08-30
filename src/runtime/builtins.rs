use super::eval_expr::checked_decimal;
use super::{native_arity, native_named_args, Interpreter, RuntimeError};
use crate::contracts::{
    decimal_to_i64, decimal_to_u32, decimal_to_u8, duration_milliseconds, validate_arity,
    validate_named_args,
};
use crate::value::*;
use rust_decimal::Decimal;
use std::cell::RefCell;
use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::rc::Rc;

impl Interpreter {
    pub(super) fn call_native(
        &mut self,
        name: &str,
        args: Vec<Value>,
        named: BTreeMap<String, Value>,
        line: usize,
        env: &EnvRef,
    ) -> Result<Value, RuntimeError> {
        validate_arity(name, args.len(), native_arity(name))?;
        validate_named_args(name, &named, native_named_args(name))?;
        match name {
            "输出" => {
                for value in &args {
                    if let Value::Table(table) = value {
                        crate::privacy::guard_endpoint(
                            crate::privacy::Endpoint::Stdout,
                            &table.borrow().sensitivity(),
                        )
                        .map_err(|error| self.err(error))?;
                    }
                }
                self.emit(
                    args.iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join(" "),
                )?;
                Ok(Value::Null)
            }
            "参数化输出" => {
                let Value::Text(template) = &args[0] else {
                    return Err(self.err("参数化输出的模板必须是文本").into());
                };
                let text =
                    render_parameterized_output(template, env, self.permissions.output_limit())
                        .map_err(|error| self.err(error))?;
                self.emit(text)?;
                Ok(Value::Null)
            }
            "输入" => {
                use std::io::{self, Write};
                if !self.interactive_input {
                    return Err(self.err("当前宿主已禁用交互式标准输入").into());
                }
                if let Some(p) = args.first() {
                    print!("{p}");
                    io::stdout().flush().ok();
                }
                let s = match super::input::read_line(self.remaining_execution_time(line)?)
                    .map_err(|error| self.err(error))?
                {
                    super::input::ReadLine::Line(text) => text,
                    super::input::ReadLine::TimedOut => {
                        return Err(
                            self.blocking_timeout_error(line, "等待标准输入超过程序执行时间限制")
                        )
                    }
                };
                Ok(Value::Text(s.trim_end().into()))
            }
            "长度" => Ok(Value::Number(Decimal::from(
                args.first()
                    .ok_or_else(|| self.err("长度缺少参数"))?
                    .iter_values_limited(self.permissions.collection_limit())?
                    .len(),
            ))),
            "绝对值" => {
                let value = args[0].as_decimal()?;
                Ok(Value::Number(if value.is_sign_negative() {
                    checked_decimal(Decimal::ZERO.checked_sub(value), "绝对值")?
                } else {
                    value
                }))
            }
            "最大值" | "最小值" => {
                let values = if args.len() == 1 {
                    match args[0].iter_values_limited(self.permissions.collection_limit()) {
                        Ok(values) => values,
                        Err(error) if error.ends_with("不能被遍历") => args,
                        Err(error) => return Err(self.err(error).into()),
                    }
                } else {
                    args
                };
                let mut best = values
                    .first()
                    .cloned()
                    .ok_or_else(|| self.err("数据不能为空"))?;
                for (processed, v) in values.iter().enumerate().skip(1) {
                    self.check_native_loop(processed)?;
                    let ord = self.compare(v, &best)?;
                    if (name == "最大值" && ord == Ordering::Greater)
                        || (name == "最小值" && ord == Ordering::Less)
                    {
                        best = v.clone();
                    }
                }
                Ok(best)
            }
            "求和" => {
                let values = args[0].iter_values_limited(self.permissions.collection_limit())?;
                Ok(Value::Number(values.iter().enumerate().try_fold(
                    Decimal::ZERO,
                    |a, (processed, v)| {
                        self.check_native_loop(processed)?;
                        checked_decimal(a.checked_add(v.as_decimal()?), "求和")
                    },
                )?))
            }
            "平均值" => {
                let values = args[0].iter_values_limited(self.permissions.collection_limit())?;
                if values.is_empty() {
                    Ok(Value::Number(Decimal::ZERO))
                } else {
                    Ok(Value::Number(
                        values
                            .iter()
                            .enumerate()
                            .try_fold(Decimal::ZERO, |a, (processed, v)| {
                                self.check_native_loop(processed)?;
                                checked_decimal(a.checked_add(v.as_decimal()?), "平均值")
                            })?
                            .checked_div(Decimal::from(values.len()))
                            .ok_or_else(|| self.err("平均值运算溢出"))?,
                    ))
                }
            }
            "四舍五入" => {
                let digits = decimal_to_u32(
                    args.get(1)
                        .or_else(|| named.get("位数"))
                        .map(Value::as_decimal)
                        .transpose()?
                        .unwrap_or(Decimal::ZERO),
                    "小数位数",
                )?;
                Ok(Value::Number(args[0].as_decimal()?.round_dp(digits)))
            }
            "排序" => {
                let mut values =
                    args[0].iter_values_limited(self.permissions.collection_limit())?;
                let desc = named.get("顺序").is_some_and(|v| v.to_string() == "降序");
                let mut comparison_error = None;
                let mut compared = 0_usize;
                values.sort_by(|a, b| {
                    compared += 1;
                    if let Err(limit_error) = self.check_native_loop(compared) {
                        comparison_error = Some(limit_error);
                        return Ordering::Equal;
                    }
                    match self.compare(a, b) {
                        Ok(ordering) => ordering,
                        Err(error_value) => {
                            comparison_error = Some(error_value);
                            Ordering::Equal
                        }
                    }
                });
                if let Some(error_value) = comparison_error {
                    return Err(error_value.into());
                }
                if desc {
                    values.reverse()
                }
                Ok(Value::list(values))
            }
            "去重" => {
                let mut out = Vec::new();
                for (processed, v) in args[0]
                    .iter_values_limited(self.permissions.collection_limit())?
                    .into_iter()
                    .enumerate()
                {
                    self.check_native_loop(processed)?;
                    if !out.iter().any(|x| values_equal(x, &v)) {
                        out.push(v)
                    }
                }
                Ok(Value::list(out))
            }
            "包含" => {
                let values = args[0].iter_values_limited(self.permissions.collection_limit())?;
                for (processed, v) in values.iter().enumerate() {
                    self.check_native_loop(processed)?;
                    if values_equal(v, &args[1]) {
                        return Ok(Value::Bool(true));
                    }
                }
                Ok(Value::Bool(false))
            }
            "转数字" => Ok(Value::Number(
                args[0]
                    .as_decimal()
                    .map_err(|_| self.err("无法转换为数字"))?,
            )),
            "转文本" => Ok(Value::Text(args[0].to_string())),
            "转逻辑" => Ok(Value::Bool(args[0].truth())),
            "类型是" => Ok(Value::Bool(Env::value_matches_dynamic_type(
                env,
                &args[0],
                &args[1].to_string(),
            )?)),
            "字节" => {
                let bytes = match &args[0] {
                    Value::Text(value) => value.as_bytes().to_vec(),
                    Value::Bytes(value) => value.borrow().clone(),
                    other => other
                        .iter_values_limited(self.permissions.collection_limit())?
                        .iter()
                        .map(|value| decimal_to_u8(value.as_decimal()?, "字节值"))
                        .collect::<Result<Vec<_>, _>>()?,
                };
                Ok(Value::Bytes(Rc::new(RefCell::new(bytes))))
            }
            "集合" => {
                let mut values = Vec::new();
                for (processed, value) in args[0]
                    .iter_values_limited(self.permissions.collection_limit())?
                    .into_iter()
                    .enumerate()
                {
                    self.check_native_loop(processed)?;
                    if !values.iter().any(|current| values_equal(current, &value)) {
                        values.push(value);
                    }
                }
                Ok(Value::Set(Rc::new(RefCell::new(values))))
            }
            "路径" => Ok(Value::Path(Rc::new(PathBuf::from(args[0].to_string())))),
            "持续时间" => {
                let milliseconds = duration_milliseconds(&args[0], "持续时间")?;
                Ok(Value::Duration(chrono::Duration::milliseconds(
                    milliseconds,
                )))
            }
            "范围" => {
                let start = decimal_to_i64(args[0].as_decimal()?, "范围起点")?;
                let end = decimal_to_i64(args[1].as_decimal()?, "范围终点")?;
                let step = args
                    .get(2)
                    .or_else(|| named.get("步长"))
                    .map(Value::as_decimal)
                    .transpose()?
                    .map(|value| decimal_to_i64(value, "范围步长"))
                    .transpose()?
                    .unwrap_or(1);
                if step == 0 {
                    return Err(self.err("范围步长不能为零").into());
                }
                Ok(Value::Range(Rc::new(RangeValue {
                    start,
                    end,
                    inclusive: named.get("包含终点").is_some_and(Value::truth),
                    step,
                })))
            }
            "成功" => Ok(Value::Outcome(Rc::new(OutcomeValue::Success(
                args.first().cloned().unwrap_or(Value::Null),
            )))),
            "失败" => {
                let issue = match args.first() {
                    Some(Value::Error(issue)) => issue.as_ref().clone(),
                    Some(value) => RuntimeIssue {
                        category: "失败".into(),
                        message: value.to_string(),
                        line,
                        definition_span: None,
                        stack: Vec::new(),
                        cause: None,
                    },
                    None => RuntimeIssue {
                        category: "失败".into(),
                        message: String::new(),
                        line,
                        definition_span: None,
                        stack: Vec::new(),
                        cause: None,
                    },
                };
                Ok(Value::Outcome(Rc::new(OutcomeValue::Failure(issue))))
            }
            "错误" => Ok(Value::Error(Rc::new(RuntimeIssue {
                category: named
                    .get("类别")
                    .map(ToString::to_string)
                    .unwrap_or_else(|| "运行错误".into()),
                message: args.first().map(ToString::to_string).unwrap_or_default(),
                line,
                definition_span: None,
                stack: Vec::new(),
                cause: None,
            }))),
            "流" => Ok(Value::Stream(Rc::new(RefCell::new(StreamValue {
                source: StreamSource::Values(
                    args[0]
                        .iter_values_limited(self.permissions.collection_limit())?
                        .into(),
                ),
                operations: Vec::new(),
            })))),
            "断言" => {
                if !args[0].truth() {
                    return Err(self
                        .err(
                            args.get(1)
                                .map(ToString::to_string)
                                .unwrap_or_else(|| "断言没有通过".into()),
                        )
                        .into());
                }
                Ok(Value::Null)
            }
            "断言相等" => {
                if !values_equal(&args[0], &args[1]) {
                    return Err(self
                        .err(assertion_message(
                            &args,
                            2,
                            format!("断言相等没有通过：实际为 {}，预期为 {}", args[0], args[1]),
                        ))
                        .into());
                }
                Ok(Value::Null)
            }
            "断言不相等" => {
                if values_equal(&args[0], &args[1]) {
                    return Err(self
                        .err(assertion_message(
                            &args,
                            2,
                            format!("断言不相等没有通过：实际值与不应值均为 {}", args[0]),
                        ))
                        .into());
                }
                Ok(Value::Null)
            }
            "断言包含" => {
                if !assertion_contains(&args[0], &args[1], self.permissions.collection_limit())? {
                    return Err(self
                        .err(assertion_message(
                            &args,
                            2,
                            format!("断言包含没有通过：{} 中没有 {}", args[0], args[1]),
                        ))
                        .into());
                }
                Ok(Value::Null)
            }
            "断言为空" | "断言不为空" => {
                let empty = assertion_is_empty(&args[0]);
                let passed = if name == "断言为空" {
                    empty
                } else {
                    !empty
                };
                if !passed {
                    let default = if empty {
                        "断言不为空没有通过：实际值为空".into()
                    } else {
                        format!("断言为空没有通过：实际为 {}", args[0])
                    };
                    return Err(self.err(assertion_message(&args, 1, default)).into());
                }
                Ok(Value::Null)
            }
            "断言接近" => {
                let actual = args[0]
                    .as_f64()
                    .map_err(|_| self.err("断言接近的实际值必须是数字"))?;
                let expected = args[1]
                    .as_f64()
                    .map_err(|_| self.err("断言接近的预期值必须是数字"))?;
                let tolerance = named
                    .get("允许误差")
                    .map(Value::as_f64)
                    .transpose()
                    .map_err(|_| self.err("断言接近的允许误差必须是数字"))?
                    .unwrap_or(1e-9);
                if !actual.is_finite() || !expected.is_finite() || !tolerance.is_finite() {
                    return Err(self.err("断言接近只接受有限数字").into());
                }
                if tolerance < 0.0 {
                    return Err(self.err("断言接近的允许误差不能小于零").into());
                }
                if (actual - expected).abs() > tolerance {
                    return Err(self
                        .err(format!(
                            "断言接近没有通过：实际为 {}，预期为 {}，允许误差为 {}",
                            args[0], args[1], tolerance
                        ))
                        .into());
                }
                Ok(Value::Null)
            }
            "断言会失败" => {
                let expected = match named.get("信息包含") {
                    Some(Value::Text(text)) => Some(text.as_str()),
                    Some(_) => return Err(self.err("信息包含必须是文本").into()),
                    None => None,
                };
                match self.call_in_env(args[0].clone(), Vec::new(), BTreeMap::new(), line, env) {
                    Ok(value) => Err(self
                        .err(format!("断言会失败没有通过：功能正常返回了 {value}"))
                        .into()),
                    Err(error) => {
                        if let Some(fragment) = expected {
                            if !error.message.contains(fragment) {
                                return Err(self
                                    .err(format!(
                                        "断言会失败捕获了错误，但信息中不包含“{fragment}”：{error}"
                                    ))
                                    .into());
                            }
                        }
                        Ok(Value::Null)
                    }
                }
            }
            n if n.starts_with("数学.") => Ok(self.math_native(&n["数学.".len()..], args)?),
            n if n.starts_with("文本处理.") => {
                Ok(self.text_native(&n["文本处理.".len()..], args)?)
            }
            n if n.starts_with("日期时间.") => Ok(crate::stdlib::call(
                n,
                args,
                named,
                crate::stdlib::ExecutionContext {
                    base_dir: &self.base_dir,
                    program_args: &self.program_args,
                    permissions: &self.permissions,
                    execution_budget: self.remaining_execution_time(line)?,
                },
            )?),
            n if n.starts_with("表格处理.") => Ok(crate::table::table_native(
                self,
                &n["表格处理.".len()..],
                args,
                named,
            )?),
            n if n.starts_with("对账.") => Ok(crate::stdlib::reconcile::reconcile_call(
                self,
                &n["对账.".len()..],
                args,
            )?),
            n if n.starts_with("统计分析.") => Ok(crate::analysis::analysis_native(
                self,
                &n["统计分析.".len()..],
                args,
                named,
            )?),
            n if n.starts_with("并发.") => {
                crate::concurrency::call(self, &n["并发.".len()..], args, named, line)
            }
            n if n.starts_with("财务.") => Ok(crate::finance::call(
                &n["财务.".len()..],
                args,
                named,
                self.permissions.collection_limit(),
            )?),
            n if crate::report::module_functions(
                n.split_once('.').map(|value| value.0).unwrap_or(""),
            )
            .is_some() =>
            {
                let (module, function) = n
                    .split_once('.')
                    .ok_or_else(|| self.err("报表功能名称无效"))?;
                Ok(crate::report::call(
                    module,
                    function,
                    args,
                    named,
                    &self.base_dir,
                    &self.permissions,
                )?)
            }
            n if crate::stdlib::module_functions(n.split_once('.').map(|v| v.0).unwrap_or(""))
                .is_some() =>
            {
                Ok(crate::stdlib::call(
                    n,
                    args,
                    named,
                    crate::stdlib::ExecutionContext {
                        base_dir: &self.base_dir,
                        program_args: &self.program_args,
                        permissions: &self.permissions,
                        execution_budget: self.remaining_execution_time(line)?,
                    },
                )?)
            }
            _ => Err(self.err(format!("未知内置功能“{name}”")).into()),
        }
    }
    fn math_native(&self, name: &str, args: Vec<Value>) -> Result<Value, String> {
        match name {
            "平方根" => {
                let value = args[0].as_f64()?;
                if !value.is_finite() || value < 0.0 {
                    return Err(self.err("平方根要求非负有限数字"));
                }
                Ok(Value::Float(value.sqrt()))
            }
            "随机数" => {
                use std::time::{SystemTime, UNIX_EPOCH};
                crate::evidence::observe_external(
                    "随机",
                    "数学.随机数",
                    crate::evidence::ExternalDependencyLevel::FingerprintOnly,
                );
                let n = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map_err(|error| self.err(error.to_string()))?
                    .subsec_nanos();
                Ok(Value::Float(n as f64 / 1_000_000_000.0))
            }
            _ => Err(self.err("未知数学功能")),
        }
    }
    fn text_native(&self, name: &str, args: Vec<Value>) -> Result<Value, String> {
        let s = args[0].to_string();
        match name {
            "大写" => Ok(Value::Text(s.to_uppercase())),
            "小写" => Ok(Value::Text(s.to_lowercase())),
            "替换" => Ok(Value::Text(
                s.replace(&args[1].to_string(), &args[2].to_string()),
            )),
            "分割" => Ok(Value::list(
                s.split(&args[1].to_string())
                    .map(|x| Value::Text(x.into()))
                    .collect(),
            )),
            _ => Err(self.err("未知文本功能")),
        }
    }
}

fn assertion_message(args: &[Value], index: usize, default: String) -> String {
    args.get(index).map(ToString::to_string).unwrap_or(default)
}

fn assertion_contains(container: &Value, member: &Value, limit: usize) -> Result<bool, String> {
    match (container, member) {
        (Value::Text(text), Value::Text(fragment)) => Ok(text.contains(fragment)),
        (Value::Record(record), Value::Text(key)) => Ok(record.borrow().contains_key(key)),
        _ => Ok(container
            .iter_values_limited(limit)?
            .iter()
            .any(|value| values_equal(value, member))),
    }
}

fn assertion_is_empty(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::Text(value) => value.is_empty(),
        Value::List(value) | Value::Set(value) => value.borrow().is_empty(),
        Value::Record(value) => value.borrow().is_empty(),
        Value::Table(value) => value.borrow().rows.is_empty(),
        Value::NumericArray(value) => value.data.is_empty(),
        Value::Bytes(value) => value.borrow().is_empty(),
        _ => false,
    }
}

fn render_parameterized_output(
    template: &str,
    env: &EnvRef,
    output_limit: usize,
) -> Result<String, String> {
    let chars = template.chars().collect::<Vec<_>>();
    let mut output = String::with_capacity(template.len());
    let mut index = 0;
    while index < chars.len() {
        match chars[index] {
            '{' if chars.get(index + 1) == Some(&'{') => {
                output.push('{');
                index += 2;
            }
            '{' => {
                let start = index + 1;
                let Some(relative_end) = chars[start..]
                    .iter()
                    .position(|character| *character == '}')
                else {
                    return Err("参数化输出模板缺少结束括号“}”".into());
                };
                let end = start + relative_end;
                let expression = chars[start..end].iter().collect::<String>();
                let (name, format) = expression
                    .split_once(':')
                    .map_or((expression.as_str(), None), |(name, format)| {
                        (name, Some(format))
                    });
                let name = name.trim();
                if name.is_empty() {
                    return Err("参数化输出占位符缺少变量名".into());
                }
                let value = Env::get(env, name)
                    .map_err(|_| format!("参数化输出引用的变量“{name}”尚未定义"))?;
                output.push_str(&format_parameterized_value(&value, format, name)?);
                index = end + 1;
            }
            '}' if chars.get(index + 1) == Some(&'}') => {
                output.push('}');
                index += 2;
            }
            '}' => return Err("参数化输出模板出现未配对的结束括号“}”".into()),
            character => {
                output.push(character);
                index += 1;
            }
        }
        if output.len() > output_limit {
            return Err("参数化输出超过最大输出字节限制".into());
        }
    }
    Ok(output)
}

fn format_parameterized_value(
    value: &Value,
    format: Option<&str>,
    name: &str,
) -> Result<String, String> {
    let Some(format) = format else {
        return Ok(value.to_string());
    };
    let mut grouping = false;
    let mut decimals = None;
    for option in format.split([',', '，']) {
        let option = option.trim();
        if option.is_empty() {
            continue;
        }
        if option == "千分位" {
            grouping = true;
        } else if let Some(digits) = option.strip_suffix("位小数") {
            let digits = digits
                .parse::<u32>()
                .map_err(|_| format!("参数“{name}”的小数位格式无效"))?;
            if digits > 28 {
                return Err(format!("参数“{name}”的小数位不能超过 28"));
            }
            decimals = Some(digits);
        } else {
            return Err(format!("参数“{name}”不支持格式选项“{option}”"));
        }
    }
    if !grouping && decimals.is_none() {
        return Err(format!("参数“{name}”缺少有效数字格式"));
    }
    let decimal = value
        .as_decimal()
        .map_err(|_| format!("参数“{name}”不是可格式化数字"))?;
    let raw = if let Some(decimals) = decimals {
        let rounded = decimal.round_dp(decimals);
        if rounded.is_zero() {
            format!("{:.*}", decimals as usize, Decimal::ZERO)
        } else {
            format!("{:.*}", decimals as usize, rounded)
        }
    } else {
        decimal.normalize().to_string()
    };
    Ok(group_decimal_text(&raw, grouping))
}

fn group_decimal_text(raw: &str, grouping: bool) -> String {
    if !grouping {
        return raw.into();
    }
    let (negative, unsigned) = raw
        .strip_prefix('-')
        .map_or((false, raw), |value| (true, value));
    let (integer, fraction) = unsigned.split_once('.').unwrap_or((unsigned, ""));
    let mut grouped = String::with_capacity(raw.len() + integer.len() / 3);
    if negative {
        grouped.push('-');
    }
    for (index, character) in integer.chars().enumerate() {
        if index > 0 && (integer.len() - index) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(character);
    }
    if !fraction.is_empty() {
        grouped.push('.');
        grouped.push_str(fraction);
    }
    grouped
}

#[cfg(test)]
mod tests {
    use crate::runtime::interpreter::execute;
    use crate::runtime::test_support::run;

    #[test]
    fn parameterized_output_resolves_scope_variables_and_formats_numbers() {
        assert_eq!(
            run("定义 订单数 为 1234567\n定义 销售额 为 -1234567.8\n参数化输出(\"订单数：{订单数:千分位,0位小数}，销售额：{销售额:千分位,2位小数}，原值：{订单数}\")"),
            vec!["订单数：1,234,567，销售额：-1,234,567.80，原值：1234567"]
        );
        assert_eq!(
            run("定义 数量 为 12\n参数化输出(\"{{数量}}：{数量:0位小数}\")"),
            vec!["{数量}：12"]
        );
    }

    #[test]
    fn parameterized_output_reports_template_errors() {
        for (source, message) in [
            ("参数化输出(\"{不存在}\")", "尚未定义"),
            (
                "定义 文本 为 \"甲\"\n参数化输出(\"{文本:千分位}\")",
                "不是可格式化数字",
            ),
            ("参数化输出(\"{数量\")", "缺少结束括号"),
            ("参数化输出(\"}\")", "未配对"),
            (
                "定义 数量 为 1\n参数化输出(\"{数量}\", 数量 为 数量)",
                "没有命名参数",
            ),
        ] {
            let error = match execute(source, ".", false) {
                Ok(_) => panic!("{source} should fail"),
                Err(error) => error,
            };
            assert!(error.contains(message), "{source}: {error}");
        }
    }

    #[test]
    fn parameterized_output_resolves_function_scope_variables() {
        assert_eq!(
            run("定义功能 打印订单数（订单数）：\n参数化输出(\"订单数：{订单数:千分位,0位小数}\")\n结束\n打印订单数(1234567)"),
            vec!["订单数：1,234,567"]
        );
    }
}
