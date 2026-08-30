use super::{Interpreter, RuntimeError};
use crate::ast::{Program, SymbolId};
use crate::diagnostic::SourceSpan;
use crate::resource::{read_string_limited, MAX_SOURCE_BYTES};
use crate::value::{Env, EnvRef, Module, Value};
use crate::{checker, parser};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;

/// 主解释器与所有模块子解释器共用的模块注册表：`loaded`按物理身份缓存已执行的模块，
/// `loading`按同一身份记录进行中的加载栈，因此菱形导入只执行一次、循环依赖仍能报出书写名。
#[derive(Default)]
pub(crate) struct ModuleRegistry {
    loaded: BTreeMap<String, Rc<Module>>,
    loading: Vec<(String, String)>,
}

struct ResolvedModule {
    identity: String,
    location: ModuleLocation,
}

enum ModuleLocation {
    Virtual(Box<Program>),
    File(PathBuf),
}

impl Interpreter {
    pub(super) fn do_import(
        &mut self,
        module: &str,
        names: Option<&[String]>,
        binding_spans: &[SourceSpan],
        env: &EnvRef,
        line: usize,
    ) -> Result<(), RuntimeError> {
        let value = self.load_module(module, line)?;
        if let Some(names) = names {
            for (name, span) in names.iter().zip(binding_spans) {
                let binding = self.binding(name, *span, true).ok_or_else(|| {
                    RuntimeError::new(line, format!("名称“{name}”缺少已解析的语义绑定身份"))
                })?;
                Env::define_resolved(
                    env,
                    &binding,
                    value.public_value(name).cloned().ok_or_else(|| {
                        RuntimeError::from(self.err(format!("模块“{module}”没有“{name}”")))
                            .with_fallback_line(line)
                    })?,
                    None,
                )?;
            }
            Ok(())
        } else {
            let name = module.rsplit('/').next().unwrap_or(module);
            let span = binding_spans
                .first()
                .copied()
                .ok_or_else(|| RuntimeError::new(line, "模块导入缺少绑定范围".into()))?;
            let binding = self.binding(name, span, true).ok_or_else(|| {
                RuntimeError::new(line, format!("名称“{name}”缺少已解析的语义绑定身份"))
            })?;
            Ok(Env::define_resolved(
                env,
                &binding,
                Value::Module(value),
                None,
            )?)
        }
    }

    fn load_module(&mut self, name: &str, line: usize) -> Result<Rc<Module>, RuntimeError> {
        if let Some(value) = self.modules.get(name) {
            return Ok(value.clone());
        }
        if let Some(value) = self.builtin_module(name) {
            self.modules.insert(name.into(), value.clone());
            return Ok(value);
        }
        let source = self.resolve_module_source(name, line)?;
        if let Some(value) = self.module_registry.borrow().loaded.get(&source.identity) {
            let value = value.clone();
            self.modules.insert(name.into(), value.clone());
            return Ok(value);
        }
        {
            let registry = self.module_registry.borrow();
            if registry.loading.len() >= self.permissions.module_depth_limit() {
                return Err(RuntimeError::new(line, "模块加载深度超过资源限制".into())
                    .with_category("资源错误"));
            }
            if let Some(index) = registry
                .loading
                .iter()
                .position(|(identity, _)| identity == &source.identity)
            {
                let mut cycle = registry.loading[index..]
                    .iter()
                    .map(|(_, display)| display.clone())
                    .collect::<Vec<_>>();
                cycle.push(name.into());
                drop(registry);
                return Err(self
                    .err(format!("模块循环依赖：{}", cycle.join(" -> ")))
                    .into());
            }
        }
        self.module_registry
            .borrow_mut()
            .loading
            .push((source.identity.clone(), name.into()));
        let result = self.load_new_module(name, source, line);
        self.module_registry.borrow_mut().loading.pop();
        result
    }

    /// 把书写的导入名解析成唯一的物理模块身份：虚拟模块用名称，磁盘模块用规范化路径。
    /// 只有先确定身份才能让菱形导入在三条路径上都只执行一次模块顶层。
    fn resolve_module_source(
        &self,
        name: &str,
        line: usize,
    ) -> Result<ResolvedModule, RuntimeError> {
        if let Some(program) = self.virtual_modules.get(name) {
            return Ok(ResolvedModule {
                identity: format!("虚拟:{name}"),
                location: ModuleLocation::Virtual(Box::new(program.clone())),
            });
        }
        let mut path = self.base_dir.join(name);
        if path.extension().is_none() {
            path.set_extension("代码");
        }
        if !path.exists() {
            if let Some(dependency_path) = crate::project::resolve_module(&self.base_dir, name) {
                path = dependency_path;
            }
        }
        let canonical = path
            .canonicalize()
            .map_err(|_| RuntimeError::new(line, format!("找不到模块“{name}”。")))?;
        Ok(ResolvedModule {
            identity: canonical.to_string_lossy().into_owned(),
            location: ModuleLocation::File(canonical),
        })
    }

    fn load_new_module(
        &mut self,
        name: &str,
        source: ResolvedModule,
        line: usize,
    ) -> Result<Rc<Module>, RuntimeError> {
        let (mut program, base_dir) = match source.location {
            ModuleLocation::Virtual(program) => (*program, self.base_dir.clone()),
            ModuleLocation::File(path) => {
                let text = read_string_limited(
                    &path,
                    self.permissions.buffer_limit().min(MAX_SOURCE_BYTES),
                )
                .map_err(|error| RuntimeError::new(line, error.to_string()))?;
                let program = parser::parse_diagnostic(&text).map_err(|diagnostic| {
                    RuntimeError::new(line, format!("模块“{name}”解析失败。"))
                        .with_category("模块错误")
                        .with_cause(RuntimeError::from_diagnostic(diagnostic))
                })?;
                checker::check_diagnostic(&program).map_err(|diagnostic| {
                    RuntimeError::new(line, format!("模块“{name}”静态检查失败。"))
                        .with_category("模块错误")
                        .with_cause(RuntimeError::from_diagnostic(diagnostic))
                })?;
                let base_dir = path.parent().unwrap_or(Path::new(".")).to_path_buf();
                (program, base_dir)
            }
        };
        let mut child = Self::new(base_dir, self.print_output);
        program.set_module_identity(child.env.borrow().module_id as u32);
        child.permissions = self.permissions.clone();
        child.program_args = self.program_args.clone();
        child.cancellation = self.cancellation.clone();
        child.started_at = self.started_at;
        child.virtual_modules = self.virtual_modules.clone();
        child.module_registry = self.module_registry.clone();
        child.output_bytes = self.output_bytes;
        let result = child.run_structured(&program);
        self.absorb_module_output(&mut child);
        result?;
        Ok(self.register_loaded_module(name, &source.identity, &program, child))
    }

    fn absorb_module_output(&mut self, child: &mut Interpreter) {
        self.output_bytes = child.output_bytes;
        self.output.append(&mut child.output);
    }

    fn register_loaded_module(
        &mut self,
        name: &str,
        identity: &str,
        program: &Program,
        child: Interpreter,
    ) -> Rc<Module> {
        child.environments.protect(&child.env);
        let module = Rc::new(Module::new(
            name,
            module_exports(program, &child.env),
            Some(child.env.clone()),
        ));
        self.module_registry
            .borrow_mut()
            .loaded
            .insert(identity.into(), module.clone());
        self.modules.insert(name.into(), module.clone());
        module
    }

    pub(super) fn builtin_module(&self, name: &str) -> Option<Rc<Module>> {
        let functions = builtin_module_functions(name)?;
        Some(Rc::new(Module::new(
            name,
            functions
                .iter()
                .map(|function| {
                    (
                        (*function).into(),
                        Value::Native(format!("{name}.{function}")),
                    )
                })
                .collect(),
            None,
        )))
    }
}

/// 内建模块的唯一名称表，供回归测试遍历整个内建表面。
#[cfg(test)]
pub(crate) const BUILTIN_MODULE_NAMES: &[&str] = &[
    "数学",
    "文本处理",
    "表格处理",
    "统计分析",
    "日期时间",
    "文件系统",
    "路径处理",
    "编码",
    "JSON",
    "TOML",
    "环境",
    "进程",
    "正则",
    "压缩",
    "网络",
    "数据库",
    "密码",
    "日志",
    "网页",
    "交付",
    "隐私",
    "对账",
    "并发",
    "财务",
    "图表",
    "报告",
    "人工智能",
];

pub(crate) fn builtin_module_functions(name: &str) -> Option<&'static [&'static str]> {
    let functions: &[&str] = match name {
        "数学" => &["平方根", "随机数"],
        "文本处理" => &["大写", "小写", "替换", "分割"],
        "表格处理" => &[
            "读取表格",
            "读取CSV流",
            "流式多维汇总",
            "把记录整理成表格",
            "写入表格",
            "分区写出",
        ],
        "统计分析" => &[
            "数组",
            "数值数组",
            "从表格",
            "线性回归",
            "读取CSV分块",
            "读取Parquet",
            "读取Parquet流",
            "分块聚合",
            "单样本T检验",
            "独立双样本t检验",
            "夏皮罗威尔克检验",
            "正态分布概率",
            "正态分布分位点",
            "单因素方差分析",
            "卡方检验",
            "曼惠特尼U检验",
            "配对样本t检验",
            "Spearman相关",
            "Fisher精确检验",
            "多重检验校正",
            "均值置信区间",
            "逻辑回归",
            "时间序列分解",
            "线性预测",
            "异常值检测",
            "排除异常值",
            "缩尾异常值",
            "预测准确度",
            "需求稳定性",
            "需求预测",
            "安全库存",
            "批量需求预测",
            "线性规划",
            "数据概览",
            "期间比较",
            "贡献分析",
            "Pareto分析",
            "队列分析",
            "漏斗分析",
        ],
        "人工智能" => &[
            "配置",
            "请求",
            "模拟响应",
            "分析结论",
            "生成报告",
            "提取结构化",
        ],
        _ => {
            return crate::stdlib::module_functions(name)
                .or_else(|| crate::concurrency::module_functions(name))
                .or_else(|| crate::finance::module_functions(name))
                .or_else(|| crate::report::module_functions(name))
        }
    };
    Some(functions)
}

fn module_exports(program: &Program, env: &EnvRef) -> BTreeMap<String, Value> {
    let public = program
        .symbols
        .interface
        .public_values
        .iter()
        .copied()
        .collect::<BTreeSet<_>>();
    program
        .symbols
        .occurrences
        .iter()
        .filter_map(|occurrence| match occurrence.identity {
            SymbolId::Binding(id) if occurrence.declaration && public.contains(&id) => env
                .borrow()
                .resolved_values
                .get(&id)
                .map(|(_, value)| (occurrence.name.clone(), value.clone())),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{builtin_module_functions, BUILTIN_MODULE_NAMES};
    use crate::runtime::interpreter::{execute, execute_with_permissions};
    use crate::runtime::test_support::run;
    use crate::security::PermissionSet;
    use std::fs;
    use std::path::Path;

    #[test]
    fn robust_statistics_forecasting_and_inventory_are_available_in_language() {
        let source = r#"引入 统计分析
定义 数据 为 统计分析 数组([10, 11, 9, 10, 100])
输出(数据 四分位距())
定义 异常 为 统计分析 异常值检测(数据)
输出(异常 的 异常数量)
定义 稳定 为 统计分析 需求稳定性([5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5])
输出(稳定 的 稳定, 稳定 的 需求类型)
定义 预测 为 统计分析 需求预测([10, 11, 12, 13, 14, 15, 16, 17], 步数 为 2, 验证期 为 2)
输出(长度(预测 的 预测))
定义 库存 为 统计分析 安全库存([10, 11, 12, 13, 14, 15], 预测需求 为 预测 的 预测)
输出(库存 的 库存上限 >= 库存 的 库存下限)"#;
        assert_eq!(run(source), vec!["1", "1", "是 平稳需求", "2", "是"]);
    }

    #[test]
    fn internal_structure_methods_are_module_private() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(
            directory.path().join("类型模块.代码"),
            "公开 结构 客户：\n姓名\n结束\n内部 定义功能 客户 私有（自己）：\n返回 自己 的 姓名\n结束\n公开 定义功能 客户 可见（自己）：\n返回 自己 私有()\n结束\n",
        )
        .unwrap();
        let visible = execute(
            "引入 类型模块\n定义 甲 为 类型模块 的 客户(姓名 为 \"甲\")\n输出(甲 可见())",
            directory.path(),
            false,
        )
        .expect("模块公开方法应该能调用同模块内部方法");
        assert_eq!(visible.output, ["甲"]);
        let error = execute(
            "引入 类型模块\n定义 甲 为 类型模块 的 客户(姓名 为 \"甲\")\n输出(甲 私有())",
            directory.path(),
            false,
        )
        .err()
        .expect("模块外访问内部方法应该失败");
        assert!(error.contains("仅限所属模块内部访问"), "{error}");
    }

    #[test]
    fn module_depth_budget_is_enforced_before_loading_next_module() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("甲.代码"), "引入 乙\n").unwrap();
        fs::write(directory.path().join("乙.代码"), "输出(1)\n").unwrap();
        let mut permissions = PermissionSet::for_source(Path::new("."));
        permissions.max_module_depth = Some(1);
        let error = execute_with_permissions(
            "引入 甲\n",
            directory.path(),
            false,
            Vec::new(),
            permissions,
        )
        .err()
        .expect("模块深度应该触发资源限制");
        assert!(error.contains("模块加载深度超过资源限制"), "{error}");
    }

    #[test]
    fn source_module_cycles_are_reported_without_recursing() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("甲.代码"), "引入 乙\n").unwrap();
        fs::write(directory.path().join("乙.代码"), "引入 甲\n").unwrap();
        let error = execute("引入 甲", directory.path(), false).err().unwrap();
        assert!(error.contains("模块循环依赖"));
        assert!(error.contains("甲 -> 乙 -> 甲"));
    }

    #[test]
    fn builtin_module_functions_reject_wrong_argument_counts_without_panicking() {
        let directory = tempfile::tempdir().unwrap();
        let mut panics = Vec::new();
        for module in BUILTIN_MODULE_NAMES {
            // 网页模块会启动真实浏览器，参数契约由 stdlib 契约表统一校验。
            if *module == "网页" {
                continue;
            }
            let functions = builtin_module_functions(module)
                .unwrap_or_else(|| panic!("内建模块“{module}”缺少功能表"));
            for function in functions {
                for count in 0..=3 {
                    let arguments = vec!["\"衡枢不存在的测试目标\""; count].join("，");
                    let source = format!("引入 {module}\n{module} {function}（{arguments}）");
                    let outcome = std::panic::catch_unwind(|| {
                        execute_with_permissions(
                            &source,
                            directory.path(),
                            false,
                            Vec::new(),
                            PermissionSet::for_source(directory.path()),
                        )
                        .map(|_| ())
                    });
                    if outcome.is_err() {
                        panics.push(format!("{module}.{function} 收到 {count} 个参数时崩溃"));
                    }
                    // 权限根是临时目录，任何落到进程当前目录的文件都意味着绕过了文件权限。
                    let escaped = std::path::Path::new("衡枢不存在的测试目标");
                    if escaped.exists() {
                        panics.push(format!(
                            "{module}.{function} 收到 {count} 个参数时绕过文件权限写出文件"
                        ));
                        std::fs::remove_file(escaped).unwrap();
                    }
                }
            }
        }
        assert!(panics.is_empty(), "{}", panics.join("\n"));
    }

    /// 文件读写只能经由受权限约束的`文件系统`模块。曾经并存的内建`文件`模块直接把
    /// 原始字符串交给`fs::write`，既不授权也不按源码目录解析，必须整体消失。
    #[test]
    fn builtin_module_surface_has_no_unauthorized_file_entry() {
        assert!(builtin_module_functions("文件").is_none());
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let target = outside.path().join("逃逸.txt");
        let source = format!(
            "引入 文件\n文件 写入文本(\"{}\", \"逃出沙箱\")\n",
            target.display()
        );
        let error = execute_with_permissions(
            &source,
            root.path(),
            false,
            Vec::new(),
            PermissionSet::for_source(root.path()),
        )
        .err()
        .expect("内建文件模块必须不存在");
        assert!(error.contains("找不到模块“文件”"), "{error}");
        assert!(!target.exists());
    }
}
