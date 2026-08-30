use crate::security::PermissionSet;
use crate::value::Value;
#[cfg(test)]
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::Path;
#[cfg(test)]
use std::rc::Rc;
use std::time::Duration as StdDuration;

mod ai;
mod codec;
mod database;
mod delivery;
mod http;
mod local;
mod network;
mod path;
mod privacy;
mod process;
pub(crate) mod reconcile;
mod tools;
mod units;
mod webpage;

use ai::ai_call;
use codec::{encoding_call, json_call, toml_call};
use database::{database_call, database_method_limited};
use delivery::delivery_call;
use http::{response_attr, response_method};
use local::{datetime_call, environment_call, file_call, logging_call};
use network::{network_call, socket_method};
use path::{path_call, resolve};
use privacy::privacy_call;
use process::process_call;
use tools::{compression_call, crypto_call, regex_call};
use units::unit_call;
use webpage::webpage_call;

pub fn module_functions(name: &str) -> Option<&'static [&'static str]> {
    Some(match name {
        "文件系统" => &[
            "读取文本",
            "读取字节",
            "写入文本",
            "写入字节",
            "追加文本",
            "存在",
            "是文件",
            "是目录",
            "创建目录",
            "复制",
            "移动",
            "删除",
            "遍历",
            "递归遍历",
            "元数据",
            "逐行读取",
        ],
        "路径处理" => &["拼接", "绝对路径", "规范化", "当前目录"],
        "编码" => &[
            "转字节",
            "转文本",
            "Base64编码",
            "Base64解码",
            "十六进制编码",
            "十六进制解码",
        ],
        "JSON" => &["解析", "生成", "读取行", "写入行"],
        "TOML" => &["解析", "生成"],
        "日期时间" => &["现在", "今天", "解析", "格式化", "增加", "相差"],
        "环境" => &[
            "读取",
            "设置",
            "全部",
            "当前目录",
            "切换目录",
            "程序参数",
            "平台",
        ],
        "进程" => &["执行"],
        "正则" => &["是否匹配", "查找", "查找全部", "替换", "分割"],
        "压缩" => &["Gzip压缩", "Gzip解压"],
        "网络" => &[
            "解析URL",
            "拼接URL",
            "请求",
            "连接TCP",
            "监听TCP",
            "绑定UDP",
        ],
        "数据库" => &["打开SQLite", "驱动能力"],
        "密码" => &["安全随机字节", "SHA256", "HMAC_SHA256", "恒定时间相等"],
        "单位" => &["创建", "换算", "税转换", "舍入", "值", "名称", "维度"],
        "交付" => &["Word转PDF", "模板替换"],
        "隐私" => &["掩码", "假名化", "分箱"],
        "对账" => &["比对", "统计"],
        "日志" => &["记录", "调试", "信息", "警告", "错误"],
        "网页" => &["取出文本", "取出HTML", "取出属性", "按字段采集", "逐页采集"],
        "人工智能" => &[
            "配置",
            "请求",
            "模拟响应",
            "分析结论",
            "生成报告",
            "提取结构化",
        ],
        _ => return None,
    })
}

pub struct ExecutionContext<'a> {
    pub base_dir: &'a Path,
    pub program_args: &'a [String],
    pub permissions: &'a PermissionSet,
    pub execution_budget: Option<StdDuration>,
}

#[derive(Clone, Copy)]
struct FunctionContract {
    arity: crate::contracts::Arity,
    named: &'static [&'static str],
    permission: PermissionRule,
}

#[derive(Clone, Copy)]
enum PermissionRule {
    None,
    FileFirst,
    FileFirstAndSecond,
    EnvironmentFirst,
    EnvironmentAll,
    Process,
    Http,
    NetworkFirst,
    Sqlite,
    LogFile,
}

pub fn call(
    name: &str,
    args: Vec<Value>,
    named: BTreeMap<String, Value>,
    context: ExecutionContext<'_>,
) -> Result<Value, String> {
    let ExecutionContext {
        base_dir,
        program_args,
        permissions,
        execution_budget,
    } = context;
    let (module, function) = name
        .split_once('.')
        .ok_or_else(|| error("标准库功能名称无效"))?;
    let contract = stdlib_contract(module, function);
    crate::contracts::validate_arity(name, args.len(), contract.map(|value| value.arity))?;
    crate::contracts::validate_named_args(name, &named, contract.map_or(&[], |value| value.named))?;
    authorize(module, function, &args, &named, base_dir, permissions)?;
    observe(module, function, &args, base_dir);
    match module {
        "文件系统" => file_call(function, args, named, base_dir, permissions.buffer_limit()),
        "路径处理" => path_call(function, args, base_dir),
        "编码" => encoding_call(function, args, named, permissions.collection_limit()),
        "JSON" => json_call(
            function,
            args,
            named,
            base_dir,
            permissions.buffer_limit(),
            permissions.collection_limit(),
        ),
        "TOML" => toml_call(function, args),
        "日期时间" => datetime_call(function, args, named),
        "环境" => environment_call(function, args, program_args, base_dir),
        "进程" => process_call(
            function,
            args,
            named,
            base_dir,
            execution_budget,
            permissions,
        ),
        "正则" => regex_call(function, args),
        "压缩" => compression_call(function, args, permissions.buffer_limit()),
        "网络" => network_call(
            function,
            args,
            named,
            base_dir,
            permissions,
            execution_budget,
        ),
        "数据库" => database_call(function, args, base_dir),
        "密码" => crypto_call(function, args, permissions.buffer_limit()),
        "单位" => unit_call(function, args),
        "交付" => delivery_call(function, args, base_dir, permissions),
        "隐私" => privacy_call(function, args, named),
        "日志" => logging_call(function, args, named, base_dir),
        "网页" => webpage_call(function, args, named, permissions, execution_budget),
        "人工智能" => ai_call(function, args, named, base_dir, permissions),
        _ => Err(error(format!("未知标准库模块“{module}”"))),
    }
}

pub(crate) fn named_args(name: &str) -> Option<&'static [&'static str]> {
    let (module, function) = name.split_once('.')?;
    stdlib_contract(module, function).map(|contract| contract.named)
}

fn stdlib_contract(module: &str, function: &str) -> Option<FunctionContract> {
    let arity = match (module, function) {
        ("文件系统", "写入文本" | "写入字节" | "追加文本" | "复制" | "移动") => {
            (2, Some(2))
        }
        ("文件系统", _) => (1, Some(1)),
        ("路径处理", "当前目录") => (0, Some(0)),
        ("路径处理", "拼接") => (1, None),
        ("路径处理", _) => (1, Some(1)),
        ("编码", _) | ("TOML", _) | ("压缩", _) => (1, Some(1)),
        ("JSON", "写入行") => (2, Some(2)),
        ("JSON", _) => (1, Some(1)),
        ("日期时间", "现在" | "今天") => (0, Some(0)),
        ("日期时间", "增加" | "相差") => (2, Some(2)),
        ("日期时间", "格式化") => (1, Some(2)),
        ("日期时间", _) => (1, Some(1)),
        ("环境", "全部" | "当前目录" | "程序参数" | "平台") => (0, Some(0)),
        ("环境", "设置") => (2, Some(2)),
        ("环境", _) => (1, Some(1)),
        ("进程", "执行") => (1, Some(2)),
        ("正则", "替换") => (3, Some(3)),
        ("正则", _) => (2, Some(2)),
        ("网络", "拼接URL" | "请求") => (2, Some(2)),
        ("网络", _) => (1, Some(1)),
        ("数据库", "驱动能力") => (0, Some(0)),
        ("数据库", "打开SQLite") => (1, Some(1)),
        ("密码", "HMAC_SHA256" | "恒定时间相等") => (2, Some(2)),
        ("密码", _) => (1, Some(1)),
        ("单位", "创建") => (4, Some(5)),
        ("单位", "换算") => (4, Some(6)),
        ("单位", "税转换") => (6, Some(6)),
        ("单位", "舍入") => (2, Some(3)),
        ("单位", "值" | "名称" | "维度") => (1, Some(1)),
        ("交付", "Word转PDF") => (2, Some(3)),
        ("交付", "模板替换") => (3, Some(3)),
        ("隐私", "掩码") => (1, Some(1)),
        ("隐私", "假名化" | "分箱") => (2, Some(2)),
        ("日志", "记录") => (2, Some(2)),
        ("日志", _) => (1, Some(1)),
        ("网页", "取出文本" | "取出HTML") => (2, Some(2)),
        ("网页", "取出属性" | "按字段采集") => (3, Some(3)),
        ("网页", "逐页采集") => (4, Some(4)),
        ("人工智能", "配置" | "模拟响应") => (0, Some(1)),
        ("人工智能", "分析结论") => (1, Some(3)),
        ("人工智能", "生成报告") => (1, Some(2)),
        ("人工智能", "提取结构化") => (2, Some(2)),
        ("人工智能", "请求") => (1, Some(2)),
        _ => return None,
    };
    let named: &'static [&'static str] = match (module, function) {
        ("文件系统", "读取文本" | "写入文本" | "追加文本") => &["编码"],
        ("文件系统", "删除") => &["递归"],
        ("编码", "转字节" | "转文本") => &["编码"],
        ("JSON", "生成") => &["美化"],
        ("日期时间", "解析") => &["格式", "时区"],
        ("进程", "执行") => &["工作目录", "环境", "最长等待"],
        ("网络", "连接TCP") => &["最长等待"],
        ("网络", "请求") => &[
            "查询参数",
            "最长等待",
            "最大重定向",
            "代理",
            "重试次数",
            "再次尝试前等待",
            "请求头",
            "取消文件",
            "JSON",
            "表单",
            "文件上传",
            "正文",
        ],
        ("隐私", "掩码") => &["保留前", "保留后"],
        ("日志", _) => &["字段", "文件", "轮换字节", "保留份数"],
        ("网页", "逐页采集") => &[
            "最多采集页数",
            "翻页后至少等待",
            "翻页后最多等待",
            "页面最长等待",
            "内容加载完成标志",
            "浏览器位置",
            "隐藏浏览器窗口",
        ],
        ("人工智能", "请求") => &[
            "模式",
            "响应文件",
            "记录文件",
            "输入图片",
            "输入PDF",
            "细节",
            "PDF细节",
            "最长等待",
            "重试次数",
            "再次尝试前等待",
        ],
        ("人工智能", "分析结论") => &[
            "模式",
            "响应文件",
            "记录文件",
            "输入图片",
            "输入PDF",
            "细节",
            "PDF细节",
            "最长等待",
            "重试次数",
            "再次尝试前等待",
        ],
        ("人工智能", "提取结构化") => &[
            "模式",
            "响应文件",
            "记录文件",
            "输入图片",
            "输入PDF",
            "细节",
            "PDF细节",
            "最长等待",
            "重试次数",
            "再次尝试前等待",
        ],
        ("人工智能", "生成报告") => &[
            "模式",
            "响应文件",
            "记录文件",
            "输入图片",
            "输入PDF",
            "细节",
            "PDF细节",
            "标题",
            "主题",
            "章节顺序",
            "HTML模板",
            "最长等待",
            "重试次数",
            "再次尝试前等待",
        ],
        _ => &[],
    };
    let permission = match (module, function) {
        ("文件系统", "复制" | "移动") => PermissionRule::FileFirstAndSecond,
        ("文件系统", _) | ("路径处理", "绝对路径") => PermissionRule::FileFirst,
        ("JSON", "读取行" | "写入行") | ("环境", "切换目录") => {
            PermissionRule::FileFirst
        }
        ("环境", "读取" | "设置") => PermissionRule::EnvironmentFirst,
        ("环境", "全部") => PermissionRule::EnvironmentAll,
        ("进程", "执行") => PermissionRule::Process,
        ("网络", "请求") => PermissionRule::Http,
        ("网络", "连接TCP" | "监听TCP" | "绑定UDP") => PermissionRule::NetworkFirst,
        ("数据库", "打开SQLite") => PermissionRule::Sqlite,
        ("日志", _) => PermissionRule::LogFile,
        ("交付", "Word转PDF" | "模板替换") => PermissionRule::FileFirstAndSecond,
        ("网页", "逐页采集") => PermissionRule::None,
        _ => PermissionRule::None,
    };
    Some(FunctionContract {
        arity,
        named,
        permission,
    })
}

pub fn attr(value: &Value, name: &str) -> Option<Result<Value, String>> {
    match value {
        Value::HttpResponse(response) => response_attr(response, name),
        Value::Database(database) => Some(match name {
            "驱动" => Ok(Value::Text(database.driver.clone())),
            _ => return None,
        }),
        _ => None,
    }
}

pub fn method(
    value: &Value,
    name: &str,
    args: Vec<Value>,
    named: BTreeMap<String, Value>,
    context: ExecutionContext<'_>,
) -> Option<Result<Value, String>> {
    let ExecutionContext {
        base_dir,
        permissions,
        execution_budget,
        ..
    } = context;
    match value {
        Value::HttpResponse(response) => {
            response_method(response, name, args, named, base_dir, permissions)
        }
        Value::Database(database) => Some(database_method_limited(
            database,
            name,
            args,
            named,
            permissions.collection_limit(),
        )),
        Value::Socket(socket) => Some({
            if name == "发送到" && args.len() > 1 {
                if let Err(permission_error) = permissions.check_network(&args[1].to_string()) {
                    return Some(Err(permission_error));
                }
            }
            socket_method(socket, name, args, named, execution_budget)
        }),
        _ => None,
    }
}

fn authorize(
    module: &str,
    function: &str,
    args: &[Value],
    named: &BTreeMap<String, Value>,
    base_dir: &Path,
    permissions: &PermissionSet,
) -> Result<(), String> {
    let file = |value: &Value| permissions.check_file(&resolve(base_dir, value));
    let permission = stdlib_contract(module, function)
        .map(|contract| contract.permission)
        .unwrap_or(PermissionRule::None);
    match permission {
        PermissionRule::FileFirstAndSecond => {
            file(&args[0])?;
            file(&args[1])
        }
        PermissionRule::FileFirst => file(&args[0]),
        PermissionRule::EnvironmentFirst => permissions.check_environment(&args[0].to_string()),
        PermissionRule::EnvironmentAll => permissions.check_environment("*"),
        PermissionRule::Process => {
            permissions.check_process()?;
            if let Some(directory) = named.get("工作目录") {
                file(directory)?;
            }
            Ok(())
        }
        PermissionRule::Http => {
            permissions.check_network(&args[1].to_string())?;
            if let Some(Value::Record(files)) = named.get("文件上传") {
                for path in files.borrow().values() {
                    file(path)?;
                }
            }
            if let Some(path) = named.get("取消文件") {
                file(path)?;
            }
            Ok(())
        }
        PermissionRule::NetworkFirst => permissions.check_network(&args[0].to_string()),
        PermissionRule::Sqlite if args[0].to_string() != ":memory:" => file(&args[0]),
        PermissionRule::LogFile => named.get("文件").map_or(Ok(()), file),
        PermissionRule::None | PermissionRule::Sqlite => Ok(()),
    }
}

/// 权限通过后、真实 I/O 之前登记运行观察，供存证记录输入指纹和外部依赖等级。
fn observe(module: &str, function: &str, args: &[Value], base_dir: &Path) {
    let first_path = matches!(
        module,
        "文件系统" | "路径处理" | "数据库" | "压缩" | "JSON" | "TOML"
    )
    .then(|| args.first().map(|value| resolve(base_dir, value)))
    .flatten();
    let identity = match module {
        "网络" | "网页" => args
            .get(1)
            .or(args.first())
            .map(|value| value.to_string())
            .unwrap_or_default(),
        _ => args
            .first()
            .map(|value| value.to_string())
            .unwrap_or_default(),
    };
    // 逻辑名相对基准目录，凭证因此不携带构建机器的绝对路径。
    let logical_name = first_path
        .as_ref()
        .map(|path| {
            path.strip_prefix(base_dir)
                .unwrap_or(path)
                .display()
                .to_string()
        })
        .unwrap_or_default();
    crate::evidence::observe_stdlib_boundary(
        module,
        function,
        first_path.as_deref(),
        &logical_name,
        &identity,
    );
}

fn error(message: impl AsRef<str>) -> String {
    message.as_ref().to_string()
}

#[cfg(test)]
#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::SocketValue;
    use std::net::TcpListener;

    #[test]
    fn stdlib_contracts_accept_supported_named_args_and_reject_unknown_ones() {
        assert_eq!(named_args("编码.转字节"), Some(&["编码"][..]));
        let permissions = PermissionSet::for_source(Path::new("."));
        let context = || ExecutionContext {
            base_dir: Path::new("."),
            program_args: &[],
            permissions: &permissions,
            execution_budget: None,
        };
        assert!(call(
            "编码.转字节",
            vec![Value::Text("甲".into())],
            BTreeMap::from([("编码".into(), Value::Text("UTF-8".into()))]),
            context(),
        )
        .is_ok());
        assert!(call(
            "编码.转字节",
            vec![Value::Text("甲".into())],
            BTreeMap::from([("拼错".into(), Value::Null)]),
            context(),
        )
        .unwrap_err()
        .contains("没有命名参数"));
    }

    #[cfg(unix)]
    #[test]
    fn process_and_socket_waits_obey_execution_budget() {
        let permissions = PermissionSet::for_source(Path::new("."));
        let started = std::time::Instant::now();
        let error = process_call(
            "执行",
            vec![
                Value::Text("/bin/sh".into()),
                Value::list(vec![
                    Value::Text("-c".into()),
                    Value::Text("sleep 1".into()),
                ]),
            ],
            BTreeMap::from([(
                "最长等待".into(),
                Value::Duration(chrono::Duration::seconds(5)),
            )]),
            Path::new("."),
            Some(StdDuration::from_millis(20)),
            &permissions,
        )
        .unwrap_err();
        assert!(error.contains("超时"), "{error}");
        assert!(started.elapsed() < StdDuration::from_millis(300));

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let socket = Rc::new(RefCell::new(SocketValue::TcpListener(listener)));
        let started = std::time::Instant::now();
        let error = socket_method(
            &socket,
            "接受",
            Vec::new(),
            BTreeMap::new(),
            Some(StdDuration::from_millis(20)),
        )
        .unwrap_err();
        assert!(error.contains("时间限制"), "{error}");
        assert!(started.elapsed() < StdDuration::from_millis(100));
    }
}
