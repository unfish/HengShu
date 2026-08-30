use super::*;

fn number_display(value: &Decimal) -> String {
    let mut s = value.normalize().to_string();
    if s == "-0" {
        s = "0".into();
    }
    s
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Null => write!(f, "空"),
            Self::Bool(v) => write!(f, "{}", if *v { "是" } else { "否" }),
            Self::Number(v) => write!(f, "{}", number_display(v)),
            Self::Float(v) => {
                if v.is_nan() {
                    write!(f, "空")
                } else {
                    write!(f, "{v}")
                }
            }
            Self::Text(v) => write!(f, "{v}"),
            Self::List(v) => display_container(f, (Rc::as_ptr(v) as usize, 1), || {
                format!(
                    "[{}]",
                    v.borrow()
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            }),
            Self::Record(v) => display_container(f, (Rc::as_ptr(v) as usize, 2), || {
                format!(
                    "{{{}}}",
                    v.borrow()
                        .iter()
                        .map(|(k, v)| format!("\"{k}\": {v}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            }),
            Self::Function(v) => write!(f, "功能 {}", v.name),
            Self::BytecodeFunction(v) => write!(f, "功能 {}", v.name),
            Self::Native(v) => write!(f, "内置功能 {v}"),
            Self::StructType(v) => write!(f, "结构 {}", v.name),
            Self::StructInstance(v) => display_container(f, (Rc::as_ptr(v) as usize, 3), || {
                let value = v.borrow();
                format!(
                    "{}({})",
                    value.struct_type.name,
                    Value::record(value.fields.clone())
                )
            }),
            Self::Module(v) => write!(f, "模块 {}", v.name),
            Self::Table(v) => display_container(f, (Rc::as_ptr(v) as usize, 5), || {
                format!(
                    "[{}]",
                    v.borrow()
                        .rows
                        .iter()
                        .cloned()
                        .map(Value::record)
                        .map(|v| v.to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            }),
            Self::Grouped(_) => write!(f, "分组结果"),
            Self::NumericArray(v) => write!(
                f,
                "[{}]",
                v.data
                    .iter()
                    .map(|x| if x.is_nan() {
                        "空".into()
                    } else {
                        x.to_string()
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Self::Regression(v) => write!(f, "回归结果(R平方={})", v.r_squared),
            Self::Bytes(v) => write!(
                f,
                "字节[{}]",
                v.borrow()
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect::<Vec<_>>()
                    .join("")
            ),
            Self::Set(v) => display_container(f, (Rc::as_ptr(v) as usize, 4), || {
                format!(
                    "集合{{{}}}",
                    v.borrow()
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            }),
            Self::Path(v) => write!(f, "{}", v.display()),
            Self::DateTime(v) => write!(f, "{}", v.to_rfc3339()),
            Self::Duration(v) => write!(f, "{}毫秒", v.num_milliseconds()),
            Self::Range(v) => write!(f, "范围({}, {}, 步长为 {})", v.start, v.end, v.step),
            Self::Error(v) => write!(f, "{}：{}", v.category, v.message),
            Self::Outcome(v) => match v.as_ref() {
                OutcomeValue::Success(value) => write!(f, "成功({value})"),
                OutcomeValue::Failure(error) => {
                    write!(f, "失败({}：{})", error.category, error.message)
                }
            },
            Self::HttpResponse(v) => write!(f, "网络响应({})", v.status),
            Self::Database(v) => write!(f, "数据库连接({})", v.driver),
            Self::Socket(_) => write!(f, "网络套接字"),
            Self::Task(_) => write!(f, "并发任务"),
            Self::Channel(_) => write!(f, "通道"),
            Self::Stream(_) => write!(f, "数据流"),
            Self::Money(v) => write!(f, "{} {}", v.currency, v.amount.round_dp(v.scale)),
        }
    }
}

fn display_container(
    f: &mut fmt::Formatter<'_>,
    key: ContainerKey,
    render: impl FnOnce() -> String,
) -> fmt::Result {
    let state = DISPLAY_CONTAINERS.with(|containers| {
        let mut containers = containers.borrow_mut();
        if containers.contains(&key) {
            return 0;
        }
        if containers.len() >= MAX_VALUE_NESTING {
            return 1;
        }
        containers.insert(key);
        2
    });
    match state {
        0 => write!(f, "<循环引用>"),
        1 => write!(f, "<嵌套过深>"),
        _ => {
            let output = render();
            DISPLAY_CONTAINERS.with(|containers| {
                containers.borrow_mut().remove(&key);
            });
            write!(f, "{output}")
        }
    }
}

impl fmt::Debug for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self}")
    }
}
