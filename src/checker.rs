use crate::ast::{Expr, MatchPattern, Program, Stmt, SymbolId, SymbolKind, TestHookStage, TypeRef};
use crate::diagnostic::{Diagnostic, DiagnosticStage, SourceSpan};
use crate::value::BUILTIN_TYPE_NAMES;
use std::collections::{BTreeMap, BTreeSet};

pub fn check(program: &Program) -> Result<(), String> {
    check_diagnostic(program).map_err(|error| error.to_string())
}

pub(crate) fn check_diagnostic(program: &Program) -> Result<(), Diagnostic> {
    let mut root_bindings = builtins();
    for statement in program.iter() {
        root_bindings.extend(crate::ast::statement_bindings(statement));
    }
    let mut checker = Checker {
        program,
        scopes: vec![builtins()],
        function_depth: 0,
        loop_depth: 0,
        function_scope_starts: Vec::new(),
        function_globals: Vec::new(),
        root_bindings,
        methods: BTreeMap::new(),
        test_names: BTreeSet::new(),
        test_hooks: BTreeSet::new(),
        type_scopes: vec![BUILTIN_TYPE_NAMES
            .iter()
            .map(|name| (*name).to_string())
            .collect()],
        unit_identities: BTreeMap::new(),
    };
    checker.block(program)
}

struct Checker<'a> {
    program: &'a Program,
    scopes: Vec<BTreeSet<String>>,
    function_depth: usize,
    loop_depth: usize,
    function_scope_starts: Vec<usize>,
    function_globals: Vec<BTreeSet<String>>,
    root_bindings: BTreeSet<String>,
    methods: BTreeMap<String, BTreeSet<String>>,
    test_names: BTreeSet<String>,
    test_hooks: BTreeSet<u8>,
    type_scopes: Vec<BTreeSet<String>>,
    unit_identities: BTreeMap<String, StaticUnit>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct StaticUnit {
    dimension: String,
    basis: StaticBasis,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum StaticBasis {
    Unknown,
    None,
    Named(String),
}

fn static_basis(value: Option<&Expr>) -> StaticBasis {
    match value {
        None | Some(Expr::Literal(crate::ast::Literal::Null, _)) => StaticBasis::None,
        Some(Expr::Literal(crate::ast::Literal::Text(value), _)) => {
            StaticBasis::Named(value.clone())
        }
        _ => StaticBasis::Unknown,
    }
}

fn static_unit_from_record(fields: &[(String, Expr)]) -> Option<StaticUnit> {
    let dimension =
        fields
            .iter()
            .find(|(key, _)| key == "维度")
            .and_then(|(_, value)| match value {
                Expr::Literal(crate::ast::Literal::Text(text), _) => Some(text.clone()),
                _ => None,
            })?;
    let basis = fields
        .iter()
        .find(|(key, _)| key == "口径")
        .map_or(StaticBasis::None, |(_, value)| static_basis(Some(value)));
    Some(StaticUnit { dimension, basis })
}

fn builtins() -> BTreeSet<String> {
    [
        "输出",
        "参数化输出",
        "输入",
        "长度",
        "四舍五入",
        "绝对值",
        "最大值",
        "最小值",
        "求和",
        "平均值",
        "排序",
        "去重",
        "包含",
        "转数字",
        "转文本",
        "转逻辑",
        "类型是",
        "字节",
        "集合",
        "路径",
        "持续时间",
        "范围",
        "成功",
        "失败",
        "错误",
        "流",
        "断言",
        "断言相等",
        "断言不相等",
        "断言包含",
        "断言为空",
        "断言不为空",
        "断言接近",
        "断言会失败",
    ]
    .into_iter()
    .map(str::to_string)
    .collect()
}

impl Checker<'_> {
    fn block(&mut self, statements: &[Stmt]) -> Result<(), Diagnostic> {
        for statement in statements {
            self.statement(statement)?;
        }
        Ok(())
    }

    fn statement(&mut self, statement: &Stmt) -> Result<(), Diagnostic> {
        match statement {
            Stmt::Export {
                statement, line, ..
            } => {
                if !self.at_file_top_level() {
                    return Err(error(*line, "公开或内部只能出现在模块文件顶层。"));
                }
                if !matches!(
                    statement.as_ref(),
                    Stmt::Define { .. }
                        | Stmt::Function { .. }
                        | Stmt::Struct { .. }
                        | Stmt::Import { .. }
                ) {
                    return Err(error(
                        *line,
                        "公开或内部只能修饰定义、功能、结构或引入语句。",
                    ));
                }
                self.statement(statement)?;
            }
            Stmt::TaskGroup { body, .. } => self.scoped(body)?,
            Stmt::Define {
                name,
                name_span,
                expr,
                type_constraint,
                ..
            } => {
                self.expr(expr)?;
                if let Some(identity) = self.infer_unit_identity(expr) {
                    self.unit_identities.insert(name.clone(), identity);
                }
                if let Some(expected) = type_constraint {
                    self.check_type(expected)?;
                }
                self.define(name, *name_span)?;
            }
            Stmt::Assign { target, expr, .. } => {
                self.expr(expr)?;
                if let Expr::Name(name, _) = target {
                    let Some(scope_index) = self.resolve_scope(name) else {
                        return Err(error_at(
                            name_span(target),
                            format!("变量“{name}”尚未定义，不能赋值。"),
                        ));
                    };
                    if let Some(function_start) = self.function_scope_starts.last().copied() {
                        let global = self
                            .function_globals
                            .last()
                            .is_some_and(|globals| globals.contains(name));
                        if scope_index < function_start && !global {
                            return Err(error_at(
                                name_span(target),
                                format!("功能内修改外部绑定“{name}”前必须声明“全局 {name}”。"),
                            ));
                        }
                    }
                    if let Some(identity) = self.infer_unit_identity(expr) {
                        self.unit_identities.insert(name.clone(), identity);
                    } else {
                        self.unit_identities.remove(name);
                    }
                } else {
                    self.expr(target)?;
                }
            }
            Stmt::Expr { expr, .. } | Stmt::Throw { expr, .. } => self.expr(expr)?,
            Stmt::If {
                branches,
                otherwise,
                ..
            } => {
                for (condition, body) in branches {
                    self.expr(condition)?;
                    self.scoped(body)?;
                }
                self.scoped(otherwise)?;
            }
            Stmt::While {
                condition, body, ..
            } => {
                self.expr(condition)?;
                self.loop_depth += 1;
                let result = self.scoped(body);
                self.loop_depth -= 1;
                result?;
            }
            Stmt::For {
                iterable,
                name,
                name_span,
                body,
                ..
            } => {
                self.expr(iterable)?;
                self.loop_depth += 1;
                self.push_scope(BTreeSet::new());
                self.define(name, *name_span)?;
                let result = self.block(body);
                self.pop_scope();
                self.loop_depth -= 1;
                result?;
            }
            Stmt::Function {
                name,
                name_span,
                params,
                body,
                type_name,
                ..
            } => {
                if type_name.is_none() {
                    self.define(name, *name_span)?;
                } else if let Some(type_name) = type_name {
                    if !self.at_file_top_level() {
                        return Err(error_at(type_name.span, "方法只能声明在模块文件顶层。"));
                    }
                    self.check_type(type_name)?;
                    if BUILTIN_TYPE_NAMES.contains(&type_name.name.as_str())
                        || matches!(type_name.name.as_str(), "任意" | "结构实例")
                    {
                        return Err(error_at(
                            type_name.span,
                            format!("不能为内建或抽象类型“{type_name}”声明用户方法。"),
                        ));
                    }
                    let names = self.methods.entry(type_name.name.clone()).or_default();
                    if !names.insert(name.clone()) {
                        return Err(error_at(
                            *name_span,
                            format!("类型“{type_name}”的方法“{name}”已经定义过了。"),
                        ));
                    }
                    if params.first().map(|param| param.name.as_str()) != Some("自己") {
                        let span = params.first().map_or(*name_span, |param| param.name_span);
                        return Err(error_at(span, "结构方法的第一个参数必须是“自己”。"));
                    }
                }
                self.check_function(params, body)?;
            }
            Stmt::Return { expr, line } => {
                if self.function_depth == 0 {
                    return Err(error(*line, "返回只能出现在功能内部。"));
                }
                if let Some(expr) = expr {
                    self.expr(expr)?;
                }
            }
            Stmt::Struct {
                name, name_span, ..
            } => {
                self.define(name, *name_span)?;
                if let Some(types) = self.type_scopes.last_mut() {
                    types.insert(name.clone());
                }
            }
            Stmt::Import {
                module,
                names,
                binding_spans,
                line,
            } => {
                if let Some(names) = names {
                    for (index, name) in names.iter().enumerate() {
                        let span = binding_spans
                            .get(index)
                            .copied()
                            .unwrap_or_else(|| SourceSpan::line(*line));
                        self.define(name, span)?;
                        if let Some(types) = self.type_scopes.last_mut() {
                            types.insert(name.clone());
                        }
                    }
                } else {
                    let span = binding_spans
                        .first()
                        .copied()
                        .unwrap_or_else(|| SourceSpan::line(*line));
                    self.define(module.rsplit('/').next().unwrap_or(module), span)?;
                }
            }
            Stmt::Global {
                names, name_spans, ..
            } => {
                let Some(globals) = self.function_globals.last() else {
                    return Err(error_at(
                        name_spans
                            .first()
                            .copied()
                            .unwrap_or_else(|| SourceSpan::line(statement.line())),
                        "全局声明只能出现在功能内部。",
                    ));
                };
                for (name, span) in names.iter().zip(name_spans) {
                    if !globals.contains(name) {
                        return Err(error_at(*span, format!("全局声明“{name}”未解析。")));
                    }
                }
            }
            Stmt::Break { line, .. } | Stmt::Continue { line, .. } => {
                if self.loop_depth == 0 {
                    return Err(error(*line, "循环控制语句只能出现在循环内部。"));
                }
            }
            Stmt::Try {
                body,
                error_name,
                catch_body,
                finally_body,
                ..
            } => {
                self.scoped(body)?;
                self.push_scope(error_name.iter().cloned().collect::<BTreeSet<String>>());
                let result = self.block(catch_body);
                self.pop_scope();
                result?;
                self.scoped(finally_body)?;
            }
            Stmt::Require {
                expr,
                type_constraint,
                message,
                ..
            } => {
                self.expr(expr)?;
                if let Some(expected) = type_constraint {
                    self.check_type(expected)?;
                }
                if let Some(message) = message {
                    self.expr(message)?;
                }
            }
            Stmt::Match {
                expr,
                arms,
                otherwise,
                ..
            } => {
                self.expr(expr)?;
                for arm in arms {
                    let mut names = BTreeSet::from(["当前值".into()]);
                    match &arm.pattern {
                        MatchPattern::Value(expr) => self.expr(expr)?,
                        MatchPattern::Bind { name, .. } => {
                            if name == "当前值" {
                                return Err(error(
                                    expr.line(),
                                    "匹配分支已经隐式定义“当前值”，不能再次记作同名绑定。",
                                ));
                            }
                            names.insert(name.clone());
                        }
                        MatchPattern::Type(expected) => self.check_type(expected)?,
                    }
                    self.push_scope(names);
                    if let Some(guard) = &arm.guard {
                        self.expr(guard)?;
                    }
                    let result = self.block(&arm.body);
                    self.pop_scope();
                    result?;
                }
                self.scoped(otherwise)?;
            }
            Stmt::Test { name, body, line } => {
                if self.scopes.len() != 1 {
                    return Err(error(*line, "测试只能写在测试文件的顶层。"));
                }
                if !self.test_names.insert(name.clone()) {
                    return Err(error(*line, format!("测试“{name}”已经定义过了。")));
                }
                self.scoped(body)?;
            }
            Stmt::TestHook { stage, body, line } => {
                if self.scopes.len() != 1 {
                    return Err(error(*line, "测试准备和收尾只能写在测试文件的顶层。"));
                }
                let key = match stage {
                    TestHookStage::SuiteSetup => 0,
                    TestHookStage::BeforeEach => 1,
                    TestHookStage::AfterEach => 2,
                    TestHookStage::SuiteTeardown => 3,
                };
                if !self.test_hooks.insert(key) {
                    let name = match stage {
                        TestHookStage::SuiteSetup => "测试准备",
                        TestHookStage::BeforeEach => "每项测试前",
                        TestHookStage::AfterEach => "每项测试后",
                        TestHookStage::SuiteTeardown => "测试收尾",
                    };
                    return Err(error(*line, format!("“{name}”在同一文件中只能定义一次。")));
                }
                self.scoped(body)?;
            }
        }
        Ok(())
    }

    fn expr(&mut self, expr: &Expr) -> Result<(), Diagnostic> {
        match expr {
            Expr::Literal(..) => {}
            Expr::Name(name, span) => {
                if !self.defined(name) {
                    return Err(error_at(*span, format!("变量“{name}”尚未定义。")));
                }
            }
            Expr::List(items, _) => {
                for item in items {
                    self.expr(item)?;
                }
            }
            Expr::Record(items, _) => {
                for (_, value) in items {
                    self.expr(value)?;
                }
            }
            Expr::Unary { expr, .. } => self.expr(expr)?,
            Expr::Binary { left, right, .. } => {
                self.expr(left)?;
                self.expr(right)?;
            }
            Expr::Call {
                receiver,
                name,
                args,
                named,
                ..
            } => {
                self.expr(receiver)?;
                for arg in args {
                    self.expr(arg)?;
                }
                for argument in named {
                    self.expr(&argument.value)?;
                }
                self.check_unit_call(receiver, name.as_deref(), args)?;
            }
            Expr::Attr { receiver, .. } | Expr::Index { receiver, .. } => self.expr(receiver)?,
            Expr::Function { params, body, .. } => self.check_function(params, body)?,
        }
        if let Expr::Index { index, .. } = expr {
            self.expr(index)?;
        }
        Ok(())
    }

    fn check_unit_call(
        &self,
        receiver: &Expr,
        name: Option<&str>,
        args: &[Expr],
    ) -> Result<(), Diagnostic> {
        let Expr::Name(module, _) = receiver else {
            return Ok(());
        };
        let Some(name) = name else { return Ok(()) };
        if module != "单位" {
            return Ok(());
        }
        if name == "换算" || name == "税转换" {
            let source = args.first().and_then(|value| match value {
                Expr::Record(fields, _) => static_unit_from_record(fields),
                Expr::Name(variable, _) => self.unit_identities.get(variable).cloned(),
                _ => None,
            });
            let target = args.get(2).and_then(|dimension| match dimension {
                Expr::Literal(crate::ast::Literal::Text(dimension), _) => Some(StaticUnit {
                    dimension: dimension.clone(),
                    basis: static_basis(args.get(4)),
                }),
                _ => None,
            });
            if let (Some(source), Some(target)) = (source, target) {
                let basis_mismatch = args.get(5).is_none()
                    && source.basis != StaticBasis::Unknown
                    && target.basis != StaticBasis::Unknown
                    && source.basis != target.basis;
                if source.dimension != target.dimension || basis_mismatch {
                    let span = args.get(2).map_or_else(
                        || match receiver {
                            Expr::Name(_, span) => *span,
                            _ => SourceSpan::line(1),
                        },
                        |expr| match expr {
                            Expr::Literal(_, line)
                            | Expr::List(_, line)
                            | Expr::Record(_, line) => SourceSpan::line(*line),
                            Expr::Name(_, span) => *span,
                            _ => SourceSpan::line(expr.line()),
                        },
                    );
                    return Err(error_at(
                        span,
                        format!(
                            "单位/口径静态不兼容：{} -> {}",
                            source.dimension, target.dimension
                        ),
                    ));
                }
            }
            if let Some(Expr::Literal(crate::ast::Literal::Number(value), line)) = args.get(5) {
                if *value <= rust_decimal::Decimal::ZERO {
                    return Err(error_at(
                        SourceSpan::line(*line),
                        "税率或汇率必须是有限正数",
                    ));
                }
            }
        }
        if name == "舍入" {
            if let Some(Expr::Literal(crate::ast::Literal::Number(value), line)) = args.get(1) {
                if value.fract() != rust_decimal::Decimal::ZERO
                    || *value < rust_decimal::Decimal::ZERO
                    || *value > rust_decimal::Decimal::from(28)
                {
                    return Err(error_at(
                        SourceSpan::line(*line),
                        "单位舍入精度必须是 0 到 28 的整数",
                    ));
                }
            }
        }
        Ok(())
    }

    fn infer_unit_identity(&self, expr: &Expr) -> Option<StaticUnit> {
        let Expr::Call {
            receiver,
            name: Some(name),
            args,
            ..
        } = expr
        else {
            return None;
        };
        if !matches!(receiver.as_ref(), Expr::Name(module, _) if module == "单位") || name != "创建"
        {
            return None;
        }
        match args.get(2) {
            Some(Expr::Literal(crate::ast::Literal::Text(value), _)) => Some(StaticUnit {
                dimension: value.clone(),
                basis: static_basis(args.get(4)),
            }),
            _ => None,
        }
    }

    fn scoped(&mut self, body: &[Stmt]) -> Result<(), Diagnostic> {
        self.push_scope(BTreeSet::new());
        let result = self.block(body);
        self.pop_scope();
        result
    }

    fn check_function(
        &mut self,
        params: &[crate::ast::Param],
        body: &[Stmt],
    ) -> Result<(), Diagnostic> {
        let mut globals = BTreeMap::new();
        let mut locals = BTreeMap::new();
        collect_function_declarations(body, &mut globals, &mut locals)?;
        for param in params {
            locals.insert(param.name.clone(), param.name_span);
        }
        for (name, span) in &globals {
            if !self.root_bindings.contains(name) {
                return Err(error_at(
                    *span,
                    format!("全局目标“{name}”在模块或程序根作用域中不存在。"),
                ));
            }
            if let Some(local_span) = locals.get(name) {
                return Err(error_at(
                    *local_span,
                    format!("局部声明“{name}”与全局声明冲突。"),
                ));
            }
        }

        self.function_depth += 1;
        let outer_loop_depth = std::mem::replace(&mut self.loop_depth, 0);
        self.push_scope(BTreeSet::new());
        self.function_scope_starts.push(self.scopes.len() - 1);
        self.function_globals
            .push(globals.keys().cloned().collect());
        let result = (|| {
            for param in params {
                if let Some(default) = &param.default {
                    self.expr(default)?;
                }
                self.define(&param.name, param.name_span)?;
            }
            self.block(body)
        })();
        self.function_globals.pop();
        self.function_scope_starts.pop();
        self.pop_scope();
        self.loop_depth = outer_loop_depth;
        self.function_depth -= 1;
        result
    }

    fn define(&mut self, name: &str, span: SourceSpan) -> Result<(), Diagnostic> {
        if self
            .function_globals
            .last()
            .is_some_and(|globals| globals.contains(name))
        {
            return Err(error_at(
                span,
                format!("局部声明“{name}”与本功能的全局声明冲突。"),
            ));
        }
        let scope = self
            .scopes
            .last_mut()
            .ok_or_else(|| error_at(span, "检查器作用域栈为空。"))?;
        if scope.contains(name) {
            return Err(error_at(span, format!("变量“{name}”已经定义过了。")));
        }
        scope.insert(name.into());
        Ok(())
    }

    fn defined(&self, name: &str) -> bool {
        self.scopes.iter().rev().any(|scope| scope.contains(name))
    }

    fn resolve_scope(&self, name: &str) -> Option<usize> {
        self.scopes
            .iter()
            .enumerate()
            .rev()
            .find_map(|(index, scope)| scope.contains(name).then_some(index))
    }

    fn check_type(&self, reference: &TypeRef) -> Result<(), Diagnostic> {
        let name = reference.name.as_str();
        let resolved = BUILTIN_TYPE_NAMES.contains(&name)
            || matches!(name, "任意")
            || self
                .type_scopes
                .iter()
                .rev()
                .any(|scope| scope.contains(name))
            || self.program.symbols.occurrences.iter().any(|occurrence| {
                occurrence.span == reference.span
                    && occurrence.kind == SymbolKind::Type
                    && matches!(occurrence.identity, SymbolId::Type(_))
            })
            || name
                .split_once('.')
                .is_some_and(|(module, ty)| !ty.is_empty() && self.defined(module));
        if resolved {
            Ok(())
        } else {
            Err(error_at(reference.span, format!("未知类型“{name}”。")))
        }
    }

    fn at_file_top_level(&self) -> bool {
        self.function_depth == 0 && self.scopes.len() == 1
    }

    fn push_scope(&mut self, names: BTreeSet<String>) {
        self.scopes.push(names);
        self.type_scopes.push(BTreeSet::new());
    }

    fn pop_scope(&mut self) {
        self.scopes.pop();
        self.type_scopes.pop();
    }
}

fn collect_function_declarations(
    statements: &[Stmt],
    globals: &mut BTreeMap<String, SourceSpan>,
    locals: &mut BTreeMap<String, SourceSpan>,
) -> Result<(), Diagnostic> {
    for statement in statements {
        match statement {
            Stmt::Export { statement, .. } => {
                collect_function_declarations(
                    std::slice::from_ref(statement.as_ref()),
                    globals,
                    locals,
                )?;
            }
            Stmt::Define {
                name, name_span, ..
            }
            | Stmt::Struct {
                name, name_span, ..
            } => {
                locals.entry(name.clone()).or_insert(*name_span);
            }
            Stmt::Function {
                name,
                name_span,
                type_name: None,
                ..
            } => {
                locals.entry(name.clone()).or_insert(*name_span);
            }
            Stmt::Import {
                module,
                names,
                binding_spans,
                ..
            } => {
                if let Some(names) = names {
                    for (name, span) in names.iter().zip(binding_spans) {
                        locals.entry(name.clone()).or_insert(*span);
                    }
                } else if let Some(span) = binding_spans.first() {
                    locals
                        .entry(module.rsplit('/').next().unwrap_or(module).to_string())
                        .or_insert(*span);
                }
            }
            Stmt::Global {
                names, name_spans, ..
            } => {
                for (name, span) in names.iter().zip(name_spans) {
                    if globals.insert(name.clone(), *span).is_some() {
                        return Err(error_at(
                            *span,
                            format!("全局目标“{name}”在同一功能中重复声明。"),
                        ));
                    }
                }
            }
            Stmt::For {
                name,
                name_span,
                body,
                ..
            } => {
                locals.entry(name.clone()).or_insert(*name_span);
                collect_function_declarations(body, globals, locals)?;
            }
            Stmt::If {
                branches,
                otherwise,
                ..
            } => {
                for (_, body) in branches {
                    collect_function_declarations(body, globals, locals)?;
                }
                collect_function_declarations(otherwise, globals, locals)?;
            }
            Stmt::While { body, .. }
            | Stmt::TaskGroup { body, .. }
            | Stmt::Test { body, .. }
            | Stmt::TestHook { body, .. } => {
                collect_function_declarations(body, globals, locals)?;
            }
            Stmt::Try {
                body,
                error_name,
                error_span,
                catch_body,
                finally_body,
                ..
            } => {
                collect_function_declarations(body, globals, locals)?;
                if let (Some(name), Some(span)) = (error_name, error_span) {
                    locals.entry(name.clone()).or_insert(*span);
                }
                collect_function_declarations(catch_body, globals, locals)?;
                collect_function_declarations(finally_body, globals, locals)?;
            }
            Stmt::Match {
                arms, otherwise, ..
            } => {
                for arm in arms {
                    if let MatchPattern::Bind { name, span } = &arm.pattern {
                        locals.entry(name.clone()).or_insert(*span);
                    }
                    collect_function_declarations(&arm.body, globals, locals)?;
                }
                collect_function_declarations(otherwise, globals, locals)?;
            }
            Stmt::Function { .. }
            | Stmt::Assign { .. }
            | Stmt::Expr { .. }
            | Stmt::Return { .. }
            | Stmt::Break { .. }
            | Stmt::Continue { .. }
            | Stmt::Throw { .. }
            | Stmt::Require { .. } => {}
        }
    }
    Ok(())
}

fn error(line: usize, message: impl AsRef<str>) -> Diagnostic {
    error_at(SourceSpan::line(line), message)
}

fn error_at(span: SourceSpan, message: impl AsRef<str>) -> Diagnostic {
    Diagnostic::new(DiagnosticStage::Check, span, message.as_ref())
}

fn name_span(expr: &Expr) -> SourceSpan {
    match expr {
        Expr::Name(_, span) => *span,
        _ => SourceSpan::line(expr.line()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn checked(source: &str) -> Result<(), String> {
        check(&crate::parser::parse(source)?)
    }

    #[test]
    fn rejects_nested_and_duplicate_tests() {
        let nested = "如果 是：\n测试 \"嵌套\"：\n断言(是)\n结束\n结束";
        assert!(checked(nested)
            .unwrap_err()
            .contains("只能写在测试文件的顶层"));

        let duplicate = "测试 \"重名\"：\n结束\n测试 \"重名\"：\n结束";
        assert!(checked(duplicate).unwrap_err().contains("已经定义过"));
    }

    #[test]
    fn rejects_nested_and_duplicate_test_hooks() {
        let nested = "定义功能 工作（）：\n每项测试前：\n结束\n结束";
        assert!(checked(nested)
            .unwrap_err()
            .contains("只能写在测试文件的顶层"));

        let duplicate = "测试准备：\n结束\n测试准备：\n结束";
        assert!(checked(duplicate).unwrap_err().contains("只能定义一次"));

        let exported = "公开 测试 \"不可导出\"：\n结束";
        assert!(checked(exported)
            .unwrap_err()
            .contains("只能修饰定义、功能、结构或引入语句"));
    }

    #[test]
    fn rejects_unknown_types_invalid_visibility_and_forward_default_references() {
        assert!(checked("定义 值 为 1，类型限定为 不存在")
            .unwrap_err()
            .contains("未知类型“不存在”"));
        assert!(checked("公开 输出(1)")
            .unwrap_err()
            .contains("只能修饰定义、功能、结构或引入语句"));
        assert!(checked("定义功能 计算（甲 为 乙，乙 为 1）为 甲")
            .unwrap_err()
            .contains("变量“乙”尚未定义"));
    }

    #[test]
    fn accepts_structure_types_and_defaults_that_reference_previous_parameters() {
        checked("结构 客户：\n姓名\n结束\n定义 值 为 客户(姓名 为 \"甲\")，类型限定为 客户\n定义功能 计算（甲，乙 为 甲 + 1）为 乙")
            .unwrap();
    }

    #[test]
    fn rejects_cross_function_loop_control_and_invalid_globals() {
        let named = "当 是 时重复：\n定义功能 非法（）：\n跳出循环\n结束\n结束";
        assert!(checked(named).unwrap_err().contains("只能出现在循环内部"));

        let anonymous = "当 是 时重复：\n定义 临时 为 定义功能（）：\n继续下一轮\n结束\n结束";
        assert!(checked(anonymous)
            .unwrap_err()
            .contains("只能出现在循环内部"));

        assert!(checked("全局 值")
            .unwrap_err()
            .contains("只能出现在功能内部"));
        assert!(checked("定义功能 改（）：\n全局 不存在\n结束")
            .unwrap_err()
            .contains("根作用域中不存在"));
        assert!(checked("定义 根 为 1\n定义功能 改（根）：\n全局 根\n结束")
            .unwrap_err()
            .contains("与全局声明冲突"));
        assert!(
            checked("定义 根 为 1\n定义功能 改（）：\n全局 根\n定义 根 为 2\n结束")
                .unwrap_err()
                .contains("与全局声明冲突")
        );
        assert!(checked("定义 根 为 1\n定义功能 改（）：\n根 为 2\n结束")
            .unwrap_err()
            .contains("必须声明“全局 根”"));
        assert!(checked(
            "定义 根 为 1\n定义功能 改（）：\n如果 是：\n全局 根\n根 为 2\n结束\n结束"
        )
        .is_ok());
    }

    #[test]
    fn rejects_invalid_method_declarations_visibility_and_match_binding() {
        let duplicate = "结构 客户：\n姓名\n结束\n定义功能 客户 问候（自己）：\n结束\n定义功能 客户 问候（自己）：\n结束";
        assert!(checked(duplicate).unwrap_err().contains("已经定义过"));

        let builtin = "定义功能 数字 扩展（自己）：\n结束";
        assert!(checked(builtin)
            .unwrap_err()
            .contains("不能为内建或抽象类型"));

        let nested = "结构 客户：\n姓名\n结束\n如果 是：\n定义功能 客户 问候（自己）：\n结束\n结束";
        assert!(checked(nested).unwrap_err().contains("模块文件顶层"));

        let visibility = "如果 是：\n公开 定义 值 为 1\n结束";
        assert!(checked(visibility).unwrap_err().contains("模块文件顶层"));

        let current = "匹配 1：\n当 记作 当前值：\n结束\n结束";
        assert!(checked(current).unwrap_err().contains("隐式定义“当前值”"));
    }

    #[test]
    fn rejects_literal_unit_dimension_mismatch_before_runtime() {
        let source = "引入 单位\n单位 换算({值: 1，单位: \"元\"，维度: \"货币\"，比例: 1}，\"人\"，\"人数\"，1)\n";
        assert!(checked(source).unwrap_err().contains("单位/口径静态不兼容"));
        assert!(checked(
            "引入 单位\n单位 舍入({值: 1，单位: \"元\"，维度: \"货币\"，比例: 1}，29)\n"
        )
        .unwrap_err()
        .contains("舍入精度"));
        let variable = "引入 单位\n定义 金额 为 单位 创建(1，\"元\"，\"货币\"，1)\n单位 换算(金额，\"人\"，\"人数\"，1)\n";
        assert!(checked(variable)
            .unwrap_err()
            .contains("单位/口径静态不兼容"));
        let basis = "引入 单位\n定义 金额 为 单位 创建(1，\"元\"，\"货币\"，1，\"CNY:未税\")\n单位 换算(金额，\"元\"，\"货币\"，1，\"CNY:含税\")\n";
        assert!(checked(basis).unwrap_err().contains("单位/口径静态不兼容"));
        let zero_rate = "引入 单位\n定义 金额 为 单位 创建(1，\"元\"，\"货币\"，1，\"CNY:未税\")\n单位 税转换(金额，\"元\"，\"货币\"，1，\"CNY:含税\"，0)\n";
        assert!(checked(zero_rate).unwrap_err().contains("有限正数"));
        let assignment = "引入 单位\n定义 数量 为 单位 创建(1，\"人\"，\"人数\"，1)\n数量 为 单位 创建(1，\"元\"，\"货币\"，1)\n单位 换算(数量，\"千克\"，\"重量\"，1)\n";
        assert!(checked(assignment)
            .unwrap_err()
            .contains("单位/口径静态不兼容"));
    }
}
