use crate::diagnostic::SourceSpan;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TypeRef {
    pub name: String,
    pub span: SourceSpan,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NamedArg {
    pub name: String,
    pub name_span: SourceSpan,
    pub value: Expr,
}

impl std::ops::Deref for TypeRef {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        &self.name
    }
}

impl std::fmt::Display for TypeRef {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.name)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Expr {
    Literal(Literal, usize),
    Name(String, SourceSpan),
    List(Vec<Expr>, usize),
    Record(Vec<(String, Expr)>, usize),
    Unary {
        op: String,
        expr: Box<Expr>,
        line: usize,
    },
    Binary {
        left: Box<Expr>,
        op: String,
        right: Box<Expr>,
        line: usize,
    },
    Call {
        receiver: Box<Expr>,
        name: Option<String>,
        name_span: Option<SourceSpan>,
        args: Vec<Expr>,
        named: Vec<NamedArg>,
        line: usize,
    },
    Attr {
        receiver: Box<Expr>,
        name: String,
        name_span: SourceSpan,
        line: usize,
    },
    Index {
        receiver: Box<Expr>,
        index: Box<Expr>,
        line: usize,
    },
    Function {
        name: Option<String>,
        declaration_span: SourceSpan,
        params: Vec<Param>,
        body: Vec<Stmt>,
        type_name: Option<TypeRef>,
        line: usize,
    },
}

impl Expr {
    pub fn line(&self) -> usize {
        match self {
            Self::Literal(_, l) | Self::List(_, l) | Self::Record(_, l) => *l,
            Self::Name(_, span) => span.start.line,
            Self::Unary { line, .. }
            | Self::Binary { line, .. }
            | Self::Call { line, .. }
            | Self::Attr { line, .. }
            | Self::Index { line, .. }
            | Self::Function { line, .. } => *line,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Literal {
    Number(#[serde(with = "rust_decimal::serde::str")] Decimal),
    Text(String),
    Bool(bool),
    Null,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Param {
    pub name: String,
    pub name_span: SourceSpan,
    pub default: Option<Expr>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum MatchPattern {
    Value(Expr),
    Type(TypeRef),
    Bind { name: String, span: SourceSpan },
}

impl MatchPattern {
    /// 分支隐式`当前值`绑定的身份范围。隐式绑定没有自己的源码文本，所以折叠到模式起点：
    /// 语义索引、编译器和解释器都调用这里，三方对同一分支得到同一身份，
    /// 也不会与写在同一位置的绑定名或类型引用争夺范围。
    pub fn implicit_binding_span(&self) -> SourceSpan {
        SourceSpan::collapsed(match self {
            Self::Bind { span, .. } => span.start,
            Self::Type(reference) => reference.span.start,
            Self::Value(value) => SourceSpan::line(value.line()).start,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MatchArm {
    pub pattern: MatchPattern,
    pub guard: Option<Expr>,
    pub body: Vec<Stmt>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TestHookStage {
    SuiteSetup,
    BeforeEach,
    AfterEach,
    SuiteTeardown,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Stmt {
    /// Module visibility wrapper. Top-level declarations are public by default;
    /// `内部` hides the wrapped binding.
    Export {
        public: bool,
        statement: Box<Stmt>,
        line: usize,
    },
    TaskGroup {
        body: Vec<Stmt>,
        line: usize,
    },
    Define {
        name: String,
        name_span: SourceSpan,
        expr: Expr,
        type_constraint: Option<TypeRef>,
        line: usize,
    },
    Assign {
        target: Expr,
        expr: Expr,
        line: usize,
    },
    Expr {
        expr: Expr,
        line: usize,
    },
    If {
        branches: Vec<(Expr, Vec<Stmt>)>,
        otherwise: Vec<Stmt>,
        line: usize,
    },
    While {
        loop_id: LoopId,
        condition: Expr,
        body: Vec<Stmt>,
        line: usize,
    },
    For {
        loop_id: LoopId,
        iterable: Expr,
        name: String,
        name_span: SourceSpan,
        body: Vec<Stmt>,
        line: usize,
    },
    Function {
        name: String,
        name_span: SourceSpan,
        params: Vec<Param>,
        body: Vec<Stmt>,
        type_name: Option<TypeRef>,
        line: usize,
    },
    Return {
        expr: Option<Expr>,
        line: usize,
    },
    Struct {
        name: String,
        name_span: SourceSpan,
        fields: Vec<String>,
        field_spans: Vec<SourceSpan>,
        line: usize,
    },
    Import {
        module: String,
        names: Option<Vec<String>>,
        binding_spans: Vec<SourceSpan>,
        line: usize,
    },
    Global {
        names: Vec<String>,
        name_spans: Vec<SourceSpan>,
        line: usize,
    },
    Break {
        target: Option<LoopId>,
        line: usize,
    },
    Continue {
        target: Option<LoopId>,
        line: usize,
    },
    Try {
        body: Vec<Stmt>,
        error_name: Option<String>,
        error_span: Option<SourceSpan>,
        catch_body: Vec<Stmt>,
        finally_body: Vec<Stmt>,
        line: usize,
    },
    Throw {
        expr: Expr,
        line: usize,
    },
    Require {
        expr: Expr,
        type_constraint: Option<TypeRef>,
        message: Option<Expr>,
        line: usize,
    },
    Match {
        expr: Expr,
        arms: Vec<MatchArm>,
        otherwise: Vec<Stmt>,
        line: usize,
    },
    Test {
        name: String,
        body: Vec<Stmt>,
        line: usize,
    },
    TestHook {
        stage: TestHookStage,
        body: Vec<Stmt>,
        line: usize,
    },
}

impl Stmt {
    pub fn line(&self) -> usize {
        match self {
            Self::Export { line, .. }
            | Self::TaskGroup { line, .. }
            | Self::Define { line, .. }
            | Self::Assign { line, .. }
            | Self::Expr { line, .. }
            | Self::If { line, .. }
            | Self::While { line, .. }
            | Self::For { line, .. }
            | Self::Function { line, .. }
            | Self::Return { line, .. }
            | Self::Struct { line, .. }
            | Self::Import { line, .. }
            | Self::Global { line, .. }
            | Self::Break { line, .. }
            | Self::Continue { line, .. }
            | Self::Try { line, .. }
            | Self::Throw { line, .. }
            | Self::Require { line, .. }
            | Self::Match { line, .. }
            | Self::Test { line, .. }
            | Self::TestHook { line, .. } => *line,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct BindingId(pub u32);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BindingOperand {
    pub id: BindingId,
    pub name: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct LoopId(pub u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ScopeId(pub u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct TypeId {
    pub module: u32,
    pub local: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct MethodId {
    pub owner: TypeId,
    pub local: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TypeOperand {
    pub id: Option<TypeId>,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MethodOperand {
    pub candidates: Vec<MethodId>,
    pub name: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FieldId {
    pub owner: TypeId,
    pub local: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SymbolId {
    Binding(BindingId),
    Type(TypeId),
    Method(MethodId),
    Field(FieldId),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SymbolKind {
    Value,
    Type,
    Method,
    Field,
    Implicit,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SymbolOccurrence {
    pub name: String,
    pub span: crate::diagnostic::SourceSpan,
    pub identity: SymbolId,
    pub kind: SymbolKind,
    pub declaration: bool,
    pub scope: ScopeId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SymbolScope {
    pub id: ScopeId,
    pub parent: Option<ScopeId>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SemanticType {
    pub id: TypeId,
    pub name: String,
    pub span: SourceSpan,
    pub scope: ScopeId,
    pub fields: Vec<(FieldId, String, SourceSpan)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SemanticMethod {
    pub id: MethodId,
    pub owner: TypeId,
    pub name: String,
    pub span: SourceSpan,
    pub public: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapturedBinding {
    pub id: BindingId,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SemanticFunction {
    pub declaration_span: SourceSpan,
    pub scope: ScopeId,
    pub captures: Vec<CapturedBinding>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ModuleInterface {
    pub public_values: Vec<BindingId>,
    pub internal_values: Vec<BindingId>,
    pub public_types: Vec<TypeId>,
    pub internal_types: Vec<TypeId>,
    pub public_methods: Vec<MethodId>,
    pub internal_methods: Vec<MethodId>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SymbolIndex {
    pub occurrences: Vec<SymbolOccurrence>,
    pub scopes: Vec<SymbolScope>,
    pub types: Vec<SemanticType>,
    pub methods: Vec<SemanticMethod>,
    pub functions: Vec<SemanticFunction>,
    pub interface: ModuleInterface,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Program {
    statements: Vec<Stmt>,
    pub symbols: SymbolIndex,
}

impl Program {
    pub fn new(statements: Vec<Stmt>, symbols: SymbolIndex) -> Self {
        Self {
            statements,
            symbols,
        }
    }

    pub(crate) fn binding_operands(&self) -> Vec<(SourceSpan, bool, BindingOperand)> {
        self.symbols
            .occurrences
            .iter()
            .filter_map(|occurrence| match occurrence.identity {
                SymbolId::Binding(id) => Some((
                    occurrence.span,
                    occurrence.declaration,
                    BindingOperand {
                        id,
                        name: occurrence.name.clone(),
                    },
                )),
                _ => None,
            })
            .collect()
    }

    pub(crate) fn set_module_identity(&mut self, module: u32) {
        let rebase_type = |id: &mut TypeId| id.module = module;
        for semantic_type in &mut self.symbols.types {
            rebase_type(&mut semantic_type.id);
            for (field, _, _) in &mut semantic_type.fields {
                rebase_type(&mut field.owner);
            }
        }
        for method in &mut self.symbols.methods {
            rebase_type(&mut method.id.owner);
            rebase_type(&mut method.owner);
        }
        for occurrence in &mut self.symbols.occurrences {
            match &mut occurrence.identity {
                SymbolId::Type(id) => rebase_type(id),
                SymbolId::Method(id) => rebase_type(&mut id.owner),
                SymbolId::Field(id) => rebase_type(&mut id.owner),
                SymbolId::Binding(_) => {}
            }
        }
        for id in self
            .symbols
            .interface
            .public_types
            .iter_mut()
            .chain(&mut self.symbols.interface.internal_types)
        {
            rebase_type(id);
        }
        for id in self
            .symbols
            .interface
            .public_methods
            .iter_mut()
            .chain(&mut self.symbols.interface.internal_methods)
        {
            rebase_type(&mut id.owner);
        }
    }
}

impl std::ops::Deref for Program {
    type Target = [Stmt];

    fn deref(&self) -> &Self::Target {
        &self.statements
    }
}

impl AsRef<[Stmt]> for Program {
    fn as_ref(&self) -> &[Stmt] {
        &self.statements
    }
}

pub(crate) fn statement_bindings(statement: &Stmt) -> Vec<String> {
    match statement {
        Stmt::Export { statement, .. } => statement_bindings(statement),
        Stmt::Define { name, .. } | Stmt::Struct { name, .. } => vec![name.clone()],
        Stmt::Function {
            name,
            type_name: None,
            ..
        } => vec![name.clone()],
        Stmt::Import { module, names, .. } => names
            .clone()
            .unwrap_or_else(|| vec![module.rsplit('/').next().unwrap_or(module).to_string()]),
        _ => Vec::new(),
    }
}
