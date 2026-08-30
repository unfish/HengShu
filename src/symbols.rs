use crate::ast::{
    BindingId, CapturedBinding, Expr, FieldId, LoopId, MatchPattern, MethodId, ModuleInterface,
    Param, Program, ScopeId, SemanticFunction, SemanticMethod, SemanticType, Stmt, SymbolId,
    SymbolIndex, SymbolKind, SymbolOccurrence, SymbolScope, TypeId, TypeRef,
};
use crate::diagnostic::SourceSpan;
use std::collections::BTreeMap;

pub(crate) fn build(mut statements: Vec<Stmt>) -> Program {
    let mut next_loop = 1;
    resolve_control_targets(&mut statements, &mut Vec::new(), &mut next_loop);
    let mut builder = Builder::new();
    builder.statements(&statements, true);
    builder.resolve_member_references();
    builder.resolve_function_captures();
    Program::new(
        statements,
        SymbolIndex {
            occurrences: builder.occurrences,
            scopes: builder
                .parents
                .into_iter()
                .enumerate()
                .map(|(id, parent)| SymbolScope {
                    id: ScopeId(id as u32),
                    parent,
                })
                .collect(),
            types: builder.types,
            methods: builder.methods,
            functions: builder.functions,
            interface: builder.interface,
        },
    )
}

fn resolve_control_targets(statements: &mut [Stmt], loops: &mut Vec<LoopId>, next_loop: &mut u32) {
    for statement in statements {
        match statement {
            Stmt::Export { statement, .. } => {
                resolve_control_targets(std::slice::from_mut(statement), loops, next_loop)
            }
            Stmt::TaskGroup { body, .. }
            | Stmt::Test { body, .. }
            | Stmt::TestHook { body, .. } => resolve_control_targets(body, loops, next_loop),
            Stmt::If {
                branches,
                otherwise,
                ..
            } => {
                for (condition, body) in branches {
                    resolve_expr_control_targets(condition, next_loop);
                    resolve_control_targets(body, loops, next_loop);
                }
                resolve_control_targets(otherwise, loops, next_loop);
            }
            Stmt::While {
                loop_id,
                condition,
                body,
                ..
            } => {
                resolve_expr_control_targets(condition, next_loop);
                *loop_id = LoopId(*next_loop);
                *next_loop += 1;
                loops.push(*loop_id);
                resolve_control_targets(body, loops, next_loop);
                loops.pop();
            }
            Stmt::For {
                loop_id,
                iterable,
                body,
                ..
            } => {
                resolve_expr_control_targets(iterable, next_loop);
                *loop_id = LoopId(*next_loop);
                *next_loop += 1;
                loops.push(*loop_id);
                resolve_control_targets(body, loops, next_loop);
                loops.pop();
            }
            Stmt::Function { body, params, .. } => {
                for param in params {
                    if let Some(default) = &mut param.default {
                        resolve_expr_control_targets(default, next_loop);
                    }
                }
                resolve_control_targets(body, &mut Vec::new(), next_loop);
            }
            Stmt::Try {
                body,
                catch_body,
                finally_body,
                ..
            } => {
                resolve_control_targets(body, loops, next_loop);
                resolve_control_targets(catch_body, loops, next_loop);
                resolve_control_targets(finally_body, loops, next_loop);
            }
            Stmt::Match {
                expr,
                arms,
                otherwise,
                ..
            } => {
                resolve_expr_control_targets(expr, next_loop);
                for arm in arms {
                    if let MatchPattern::Value(pattern) = &mut arm.pattern {
                        resolve_expr_control_targets(pattern, next_loop);
                    }
                    if let Some(guard) = &mut arm.guard {
                        resolve_expr_control_targets(guard, next_loop);
                    }
                    resolve_control_targets(&mut arm.body, loops, next_loop);
                }
                resolve_control_targets(otherwise, loops, next_loop);
            }
            Stmt::Break { target, .. } | Stmt::Continue { target, .. } => {
                *target = loops.last().copied();
            }
            Stmt::Define { expr, .. } | Stmt::Expr { expr, .. } | Stmt::Throw { expr, .. } => {
                resolve_expr_control_targets(expr, next_loop)
            }
            Stmt::Assign { target, expr, .. } => {
                resolve_expr_control_targets(expr, next_loop);
                resolve_expr_control_targets(target, next_loop);
            }
            Stmt::Return { expr, .. } => {
                if let Some(expr) = expr {
                    resolve_expr_control_targets(expr, next_loop);
                }
            }
            Stmt::Require { expr, message, .. } => {
                resolve_expr_control_targets(expr, next_loop);
                if let Some(message) = message {
                    resolve_expr_control_targets(message, next_loop);
                }
            }
            Stmt::Struct { .. } | Stmt::Import { .. } | Stmt::Global { .. } => {}
        }
    }
}

fn resolve_expr_control_targets(expr: &mut Expr, next_loop: &mut u32) {
    match expr {
        Expr::List(values, _) => {
            for value in values {
                resolve_expr_control_targets(value, next_loop);
            }
        }
        Expr::Record(values, _) => {
            for (_, value) in values {
                resolve_expr_control_targets(value, next_loop);
            }
        }
        Expr::Unary { expr, .. } => resolve_expr_control_targets(expr, next_loop),
        Expr::Binary { left, right, .. } => {
            resolve_expr_control_targets(left, next_loop);
            resolve_expr_control_targets(right, next_loop);
        }
        Expr::Call {
            receiver,
            args,
            named,
            ..
        } => {
            resolve_expr_control_targets(receiver, next_loop);
            for arg in args {
                resolve_expr_control_targets(arg, next_loop);
            }
            for arg in named {
                resolve_expr_control_targets(&mut arg.value, next_loop);
            }
        }
        Expr::Attr { receiver, .. } => resolve_expr_control_targets(receiver, next_loop),
        Expr::Index {
            receiver, index, ..
        } => {
            resolve_expr_control_targets(receiver, next_loop);
            resolve_expr_control_targets(index, next_loop);
        }
        Expr::Function { body, params, .. } => {
            for param in params {
                if let Some(default) = &mut param.default {
                    resolve_expr_control_targets(default, next_loop);
                }
            }
            resolve_control_targets(body, &mut Vec::new(), next_loop);
        }
        Expr::Literal(..) | Expr::Name(..) => {}
    }
}

struct MemberReference {
    name: String,
    span: SourceSpan,
    scope: ScopeId,
    kind: SymbolKind,
}

struct Builder {
    value_scopes: Vec<BTreeMap<String, BindingId>>,
    type_scopes: Vec<BTreeMap<String, TypeId>>,
    next_binding: u32,
    next_scope: u32,
    next_type: u32,
    next_method: u32,
    occurrences: Vec<SymbolOccurrence>,
    parents: Vec<Option<ScopeId>>,
    scope_ids: Vec<ScopeId>,
    types: Vec<SemanticType>,
    methods: Vec<SemanticMethod>,
    functions: Vec<SemanticFunction>,
    interface: ModuleInterface,
    method_names: BTreeMap<String, Vec<MethodId>>,
    field_names: BTreeMap<String, Vec<FieldId>>,
    member_references: Vec<MemberReference>,
}

impl Builder {
    fn new() -> Self {
        Self {
            value_scopes: vec![BTreeMap::new()],
            type_scopes: vec![BTreeMap::new()],
            next_binding: 1,
            next_scope: 1,
            next_type: 1,
            next_method: 1,
            occurrences: Vec::new(),
            parents: vec![None],
            scope_ids: vec![ScopeId(0)],
            types: Vec::new(),
            methods: Vec::new(),
            functions: Vec::new(),
            interface: ModuleInterface::default(),
            method_names: BTreeMap::new(),
            field_names: BTreeMap::new(),
            member_references: Vec::new(),
        }
    }

    fn statements(&mut self, statements: &[Stmt], top_level: bool) {
        for statement in statements {
            self.statement(statement, top_level, top_level);
        }
    }

    fn statement(&mut self, statement: &Stmt, top_level: bool, public: bool) {
        match statement {
            Stmt::Export {
                public: exported,
                statement,
                ..
            } => self.statement(statement, top_level, *exported),
            Stmt::TaskGroup { body, .. } => self.scoped(body),
            Stmt::Define {
                name,
                name_span,
                expr,
                type_constraint,
                ..
            } => {
                self.expr(expr);
                self.type_reference(type_constraint.as_ref());
                self.declare_value(
                    name,
                    *name_span,
                    top_level.then_some(public),
                    SymbolKind::Value,
                );
            }
            Stmt::Assign { target, expr, .. } => {
                self.expr(expr);
                self.expr(target);
            }
            Stmt::Expr { expr, .. } | Stmt::Throw { expr, .. } => self.expr(expr),
            Stmt::If {
                branches,
                otherwise,
                ..
            } => {
                for (condition, body) in branches {
                    self.expr(condition);
                    self.scoped(body);
                }
                self.scoped(otherwise);
            }
            Stmt::While {
                condition, body, ..
            } => {
                self.expr(condition);
                self.scoped(body);
            }
            Stmt::For {
                iterable,
                name,
                name_span,
                body,
                ..
            } => {
                self.expr(iterable);
                self.push_scope();
                self.declare_value(name, *name_span, None, SymbolKind::Value);
                self.statements(body, false);
                self.pop_scope();
            }
            Stmt::Function {
                name,
                name_span,
                params,
                body,
                type_name,
                ..
            } => {
                if let Some(owner_ref) = type_name {
                    let owner = self.resolve_type_reference(owner_ref);
                    if let Some(owner) = owner {
                        self.declare_method(owner, name, *name_span, top_level.then_some(public));
                    }
                } else {
                    self.declare_value(
                        name,
                        *name_span,
                        top_level.then_some(public),
                        SymbolKind::Value,
                    );
                }
                self.function(*name_span, params, body);
            }
            Stmt::Return { expr, .. } => {
                if let Some(expr) = expr {
                    self.expr(expr);
                }
            }
            Stmt::Struct {
                name,
                name_span,
                fields,
                field_spans,
                ..
            } => {
                self.declare_value(
                    name,
                    *name_span,
                    top_level.then_some(public),
                    SymbolKind::Value,
                );
                self.declare_type(
                    name,
                    *name_span,
                    fields,
                    field_spans,
                    top_level.then_some(public),
                );
            }
            Stmt::Import {
                module,
                names,
                binding_spans,
                ..
            } => {
                if let Some(names) = names {
                    for (name, span) in names.iter().zip(binding_spans) {
                        self.declare_value(
                            name,
                            *span,
                            top_level.then_some(public),
                            SymbolKind::Value,
                        );
                    }
                } else {
                    let name = module.rsplit('/').next().unwrap_or(module);
                    if let Some(span) = binding_spans.first() {
                        self.declare_value(
                            name,
                            *span,
                            top_level.then_some(public),
                            SymbolKind::Value,
                        );
                    }
                }
            }
            Stmt::Global {
                names, name_spans, ..
            } => {
                for (name, span) in names.iter().zip(name_spans) {
                    if let Some(binding) = self
                        .value_scopes
                        .first()
                        .and_then(|scope| scope.get(name))
                        .copied()
                    {
                        self.record(
                            name,
                            *span,
                            SymbolId::Binding(binding),
                            SymbolKind::Value,
                            false,
                        );
                    }
                }
            }
            Stmt::Break { .. } | Stmt::Continue { .. } => {}
            Stmt::Try {
                body,
                error_name,
                error_span,
                catch_body,
                finally_body,
                ..
            } => {
                self.scoped(body);
                self.push_scope();
                if let (Some(name), Some(span)) = (error_name, error_span) {
                    self.declare_value(name, *span, None, SymbolKind::Value);
                }
                self.statements(catch_body, false);
                self.pop_scope();
                self.scoped(finally_body);
            }
            Stmt::Require {
                expr,
                type_constraint,
                message,
                ..
            } => {
                self.expr(expr);
                self.type_reference(type_constraint.as_ref());
                if let Some(message) = message {
                    self.expr(message);
                }
            }
            Stmt::Match {
                expr,
                arms,
                otherwise,
                ..
            } => {
                self.expr(expr);
                for arm in arms {
                    if let MatchPattern::Value(pattern) = &arm.pattern {
                        self.expr(pattern);
                    }
                    self.push_scope();
                    let implicit_span = arm.pattern.implicit_binding_span();
                    self.declare_value("当前值", implicit_span, None, SymbolKind::Implicit);
                    match &arm.pattern {
                        MatchPattern::Bind { name, span } => {
                            self.declare_value(name, *span, None, SymbolKind::Value);
                        }
                        MatchPattern::Type(reference) => self.type_reference(Some(reference)),
                        MatchPattern::Value(_) => {}
                    }
                    if let Some(guard) = &arm.guard {
                        self.expr(guard);
                    }
                    self.statements(&arm.body, false);
                    self.pop_scope();
                }
                self.scoped(otherwise);
            }
            Stmt::Test { body, .. } | Stmt::TestHook { body, .. } => self.scoped(body),
        }
    }

    fn function(&mut self, declaration_span: SourceSpan, params: &[Param], body: &[Stmt]) {
        self.push_scope();
        let scope = self.current_scope();
        for param in params {
            if let Some(default) = &param.default {
                self.expr(default);
            }
            self.declare_value(&param.name, param.name_span, None, SymbolKind::Value);
        }
        self.statements(body, false);
        self.functions.push(SemanticFunction {
            declaration_span,
            scope,
            captures: Vec::new(),
        });
        self.pop_scope();
    }

    fn expr(&mut self, expr: &Expr) {
        match expr {
            Expr::Literal(..) => {}
            Expr::Name(name, span) => {
                if let Some(binding) = self.resolve_value(name) {
                    self.record(
                        name,
                        *span,
                        SymbolId::Binding(binding),
                        SymbolKind::Value,
                        false,
                    );
                }
            }
            Expr::List(items, _) => items.iter().for_each(|item| self.expr(item)),
            Expr::Record(items, _) => items.iter().for_each(|(_, value)| self.expr(value)),
            Expr::Unary { expr, .. } => self.expr(expr),
            Expr::Binary { left, right, .. } => {
                self.expr(left);
                self.expr(right);
            }
            Expr::Call {
                receiver,
                name,
                name_span,
                args,
                named,
                ..
            } => {
                self.expr(receiver);
                args.iter().for_each(|arg| self.expr(arg));
                named.iter().for_each(|argument| self.expr(&argument.value));
                if let (Some(name), Some(span)) = (name, name_span) {
                    self.member_references.push(MemberReference {
                        name: name.clone(),
                        span: *span,
                        scope: self.current_scope(),
                        kind: SymbolKind::Method,
                    });
                }
            }
            Expr::Attr {
                receiver,
                name,
                name_span,
                ..
            } => {
                self.expr(receiver);
                self.member_references.push(MemberReference {
                    name: name.clone(),
                    span: *name_span,
                    scope: self.current_scope(),
                    kind: SymbolKind::Field,
                });
            }
            Expr::Index {
                receiver, index, ..
            } => {
                self.expr(receiver);
                self.expr(index);
            }
            Expr::Function {
                declaration_span,
                params,
                body,
                type_name,
                ..
            } => {
                self.type_reference(type_name.as_ref());
                self.function(*declaration_span, params, body);
            }
        }
    }

    fn scoped(&mut self, body: &[Stmt]) {
        self.push_scope();
        self.statements(body, false);
        self.pop_scope();
    }

    fn push_scope(&mut self) {
        let parent = Some(self.current_scope());
        let id = ScopeId(self.next_scope);
        self.next_scope += 1;
        self.parents.push(parent);
        self.scope_ids.push(id);
        self.value_scopes.push(BTreeMap::new());
        self.type_scopes.push(BTreeMap::new());
    }

    fn pop_scope(&mut self) {
        self.value_scopes.pop();
        self.type_scopes.pop();
        self.scope_ids.pop();
    }

    fn current_scope(&self) -> ScopeId {
        self.scope_ids.last().copied().unwrap_or(ScopeId(0))
    }

    fn declare_value(
        &mut self,
        name: &str,
        span: SourceSpan,
        visibility: Option<bool>,
        kind: SymbolKind,
    ) -> BindingId {
        let binding = BindingId(self.next_binding);
        self.next_binding += 1;
        if let Some(scope) = self.value_scopes.last_mut() {
            scope.insert(name.to_string(), binding);
        }
        self.record(name, span, SymbolId::Binding(binding), kind, true);
        match visibility {
            Some(true) => self.interface.public_values.push(binding),
            Some(false) => self.interface.internal_values.push(binding),
            None => {}
        }
        binding
    }

    fn declare_type(
        &mut self,
        name: &str,
        span: SourceSpan,
        fields: &[String],
        field_spans: &[SourceSpan],
        visibility: Option<bool>,
    ) -> TypeId {
        let id = TypeId {
            module: 0,
            local: self.next_type,
        };
        self.next_type += 1;
        if let Some(scope) = self.type_scopes.last_mut() {
            scope.insert(name.to_string(), id);
        }
        self.record(name, span, SymbolId::Type(id), SymbolKind::Type, true);
        let semantic_fields = fields
            .iter()
            .zip(field_spans)
            .enumerate()
            .map(|(index, (field, field_span))| {
                let field_id = FieldId {
                    owner: id,
                    local: index as u32,
                };
                self.field_names
                    .entry(field.clone())
                    .or_default()
                    .push(field_id);
                self.record(
                    field,
                    *field_span,
                    SymbolId::Field(field_id),
                    SymbolKind::Field,
                    true,
                );
                (field_id, field.clone(), *field_span)
            })
            .collect();
        self.types.push(SemanticType {
            id,
            name: name.to_string(),
            span,
            scope: self.current_scope(),
            fields: semantic_fields,
        });
        match visibility {
            Some(true) => self.interface.public_types.push(id),
            Some(false) => self.interface.internal_types.push(id),
            None => {}
        }
        id
    }

    fn declare_method(
        &mut self,
        owner: TypeId,
        name: &str,
        span: SourceSpan,
        visibility: Option<bool>,
    ) {
        let id = MethodId {
            owner,
            local: self.next_method,
        };
        self.next_method += 1;
        self.record(name, span, SymbolId::Method(id), SymbolKind::Method, true);
        self.method_names
            .entry(name.to_string())
            .or_default()
            .push(id);
        self.methods.push(SemanticMethod {
            id,
            owner,
            name: name.to_string(),
            span,
            public: visibility.unwrap_or(false),
        });
        match visibility {
            Some(true) => self.interface.public_methods.push(id),
            Some(false) => self.interface.internal_methods.push(id),
            None => {}
        }
    }

    fn resolve_value(&self, name: &str) -> Option<BindingId> {
        self.value_scopes
            .iter()
            .rev()
            .find_map(|scope| scope.get(name).copied())
    }

    fn resolve_type(&self, name: &str) -> Option<TypeId> {
        self.type_scopes
            .iter()
            .rev()
            .find_map(|scope| scope.get(name).copied())
    }

    fn resolve_type_reference(&mut self, reference: &TypeRef) -> Option<TypeId> {
        let id = reference
            .name
            .split_once('.')
            .and(None)
            .or_else(|| self.resolve_type(&reference.name));
        if let Some(id) = id {
            self.record(
                &reference.name,
                reference.span,
                SymbolId::Type(id),
                SymbolKind::Type,
                false,
            );
        }
        id
    }

    fn type_reference(&mut self, reference: Option<&TypeRef>) {
        if let Some(reference) = reference {
            self.resolve_type_reference(reference);
        }
    }

    fn resolve_member_references(&mut self) {
        for reference in std::mem::take(&mut self.member_references) {
            let identity = match reference.kind {
                SymbolKind::Method => self
                    .method_names
                    .get(&reference.name)
                    .filter(|ids| ids.len() == 1)
                    .map(|ids| SymbolId::Method(ids[0])),
                SymbolKind::Field => self
                    .field_names
                    .get(&reference.name)
                    .filter(|ids| ids.len() == 1)
                    .map(|ids| SymbolId::Field(ids[0])),
                _ => None,
            };
            if let Some(identity) = identity {
                self.occurrences.push(SymbolOccurrence {
                    name: reference.name,
                    span: reference.span,
                    identity,
                    kind: reference.kind,
                    declaration: false,
                    scope: reference.scope,
                });
            }
        }
    }

    fn resolve_function_captures(&mut self) {
        let declarations = self
            .occurrences
            .iter()
            .filter_map(
                |occurrence| match (occurrence.declaration, occurrence.identity) {
                    (true, SymbolId::Binding(binding)) => Some((binding, occurrence.scope)),
                    _ => None,
                },
            )
            .collect::<BTreeMap<_, _>>();
        let parents = self
            .parents
            .iter()
            .enumerate()
            .map(|(id, parent)| (ScopeId(id as u32), *parent))
            .collect::<BTreeMap<_, _>>();
        for function in &mut self.functions {
            let mut captures = BTreeMap::new();
            let self_binding = self.occurrences.iter().find_map(|occurrence| {
                match (
                    occurrence.declaration,
                    occurrence.span == function.declaration_span,
                    occurrence.identity,
                ) {
                    (true, true, SymbolId::Binding(binding)) => Some(binding),
                    _ => None,
                }
            });
            for occurrence in &self.occurrences {
                let SymbolId::Binding(binding) = occurrence.identity else {
                    continue;
                };
                if Some(binding) == self_binding
                    || occurrence.declaration
                    || !scope_is_within(occurrence.scope, function.scope, &parents)
                {
                    continue;
                }
                let Some(declaration_scope) = declarations.get(&binding).copied() else {
                    continue;
                };
                if !scope_is_within(declaration_scope, function.scope, &parents) {
                    captures
                        .entry(binding)
                        .or_insert_with(|| occurrence.name.clone());
                }
            }
            function.captures = captures
                .into_iter()
                .map(|(id, name)| CapturedBinding { id, name })
                .collect();
        }
    }

    fn record(
        &mut self,
        name: &str,
        span: SourceSpan,
        identity: SymbolId,
        kind: SymbolKind,
        declaration: bool,
    ) {
        self.occurrences.push(SymbolOccurrence {
            name: name.to_string(),
            span,
            identity,
            kind,
            declaration,
            scope: self.current_scope(),
        });
    }
}

fn scope_is_within(
    scope: ScopeId,
    ancestor: ScopeId,
    parents: &BTreeMap<ScopeId, Option<ScopeId>>,
) -> bool {
    let mut current = Some(scope);
    while let Some(scope) = current {
        if scope == ancestor {
            return true;
        }
        current = parents.get(&scope).copied().flatten();
    }
    false
}

#[cfg(test)]
mod tests {
    use crate::ast::SymbolId;

    #[test]
    fn module_interface_records_public_and_internal_semantic_identities() {
        let program = crate::parser::parse(
            "公开 定义 公开值 为 1\n内部 定义 内部值 为 2\n公开 结构 客户：\n姓名\n结束\n内部 结构 凭证：\n编号\n结束\n公开 定义功能 客户 标签（自己）：\n返回 自己 的 姓名\n结束\n内部 定义功能 客户 内部标签（自己）：\n返回 自己 的 姓名\n结束",
        )
        .unwrap();
        let interface = &program.symbols.interface;
        assert_eq!(interface.public_values.len(), 2);
        assert_eq!(interface.internal_values.len(), 2);
        assert_eq!(interface.public_types.len(), 1);
        assert_eq!(interface.internal_types.len(), 1);
        assert_eq!(interface.public_methods.len(), 1);
        assert_eq!(interface.internal_methods.len(), 1);
        let internal_names = program
            .symbols
            .occurrences
            .iter()
            .filter_map(|occurrence| match occurrence.identity {
                SymbolId::Binding(id)
                    if occurrence.declaration && interface.internal_values.contains(&id) =>
                {
                    Some(occurrence.name.as_str())
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(internal_names, ["内部值", "凭证"]);
    }

    #[test]
    fn lexical_reads_and_assignments_resolve_to_binding_and_scope_identities() {
        let program = crate::parser::parse(
            "定义 值 为 1\n如果 是：\n定义 值 为 2\n值 为 值 + 1\n输出(值)\n结束\n值 为 值 + 1\n输出(值)",
        )
        .unwrap();
        let occurrences = program
            .symbols
            .occurrences
            .iter()
            .filter(|occurrence| occurrence.name == "值")
            .collect::<Vec<_>>();
        let declarations = occurrences
            .iter()
            .filter(|occurrence| occurrence.declaration)
            .map(|occurrence| occurrence.identity)
            .collect::<Vec<_>>();
        assert_eq!(declarations.len(), 2);
        assert_ne!(declarations[0], declarations[1]);
        for occurrence in occurrences {
            let expected = if (3..=5).contains(&occurrence.span.start.line) {
                declarations[1]
            } else {
                declarations[0]
            };
            assert_eq!(occurrence.identity, expected);
        }
        assert!(program
            .symbols
            .scopes
            .iter()
            .any(|scope| scope.parent.is_some()));
    }
}
