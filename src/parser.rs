use crate::ast::*;
use crate::diagnostic::{Diagnostic, DiagnosticStage};
use crate::lexer::{lex_diagnostic, Token, TokenKind};
use std::collections::BTreeSet;

type FunctionParts = (
    Option<String>,
    Option<crate::diagnostic::SourceSpan>,
    crate::diagnostic::SourceSpan,
    Option<TypeRef>,
    Vec<Param>,
    Vec<Stmt>,
    usize,
);
type ParseResult<T> = Result<T, Diagnostic>;
/// 语法深度预算与运行时资源预算写在同一处配置，解析、语义检查和编译共用。
pub(crate) const MAX_SYNTAX_DEPTH: usize = crate::resource::DEFAULT_MAX_SYNTAX_DEPTH;

pub fn parse(source: &str) -> Result<Program, String> {
    parse_diagnostic(source).map_err(|error| error.to_string())
}

pub(crate) fn parse_diagnostic(source: &str) -> ParseResult<Program> {
    Parser::new(source)?.parse()
}

struct Parser {
    tokens: Vec<Token>,
    i: usize,
    statement_depth: usize,
    block_depth: usize,
    expression_depth: usize,
    unary_depth: usize,
}

impl Parser {
    fn new(source: &str) -> ParseResult<Self> {
        Ok(Self {
            tokens: lex_diagnostic(source)?,
            i: 0,
            statement_depth: 0,
            block_depth: 0,
            expression_depth: 0,
            unary_depth: 0,
        })
    }
    fn token(&self) -> &Token {
        &self.tokens[self.i.min(self.tokens.len() - 1)]
    }
    fn text(&self) -> String {
        self.token().text()
    }
    fn peek(&self, value: &str) -> bool {
        token_matches_syntax(self.token(), value)
    }
    fn peek_at(&self, offset: usize, value: &str) -> bool {
        self.tokens
            .get(self.i + offset)
            .is_some_and(|token| token_matches_syntax(token, value))
    }
    fn take(&mut self, value: Option<&str>) -> ParseResult<Token> {
        let token = self.token().clone();
        if let Some(expected) = value {
            if !token_matches_syntax(&token, expected) {
                return self.error(format!("需要“{expected}”，实际是“{}”", token.text()));
            }
        }
        self.i += 1;
        Ok(token)
    }
    fn error<T>(&self, message: String) -> ParseResult<T> {
        Err(Diagnostic::new(
            DiagnosticStage::Parse,
            self.token().span,
            message,
        ))
    }
    fn depth_error<T>(&self) -> ParseResult<T> {
        self.error(format!("语法嵌套深度超过 {MAX_SYNTAX_DEPTH} 层"))
    }
    /// 左结合链条不增加解析递归，却让语法树同样变深，必须与当前嵌套深度合并计量。
    fn check_chain_depth(&self, chain: usize) -> ParseResult<()> {
        if self.expression_depth + chain > MAX_SYNTAX_DEPTH {
            return self.depth_error();
        }
        Ok(())
    }
    fn skip_lines(&mut self) {
        while matches!(self.token().kind, TokenKind::Newline) || self.peek(";") {
            self.i += 1;
        }
    }
    fn skip_continuation_lines(&mut self) {
        while matches!(self.token().kind, TokenKind::Newline) {
            self.i += 1;
        }
    }
    fn end_of_statement(&mut self, stops: &[&str]) -> ParseResult<()> {
        if matches!(self.token().kind, TokenKind::Newline | TokenKind::Eof)
            || self.peek(";")
            || stops.iter().any(|stop| self.peek(stop))
        {
            return Ok(());
        }
        self.error(format!(
            "一行只能写一条语句，“{}”之前需要换行或分号",
            self.token().text()
        ))
    }
    fn type_ref(&mut self) -> ParseResult<TypeRef> {
        let first = self.ident_token(false)?;
        let mut name = first.text();
        let mut span = first.span;
        if self.peek(".") {
            self.take(None)?;
            let second = self.ident_token(false)?;
            name.push('.');
            name.push_str(&second.text());
            span.end = second.span.end;
        }
        Ok(TypeRef { name, span })
    }
    fn ident_token(&mut self, allow_self: bool) -> ParseResult<Token> {
        if !matches!(self.token().kind, TokenKind::Word(_)) {
            return self.error("需要标识符".into());
        }
        let name = self.text();
        if is_keyword(&name) && !(allow_self && name == "自己") {
            return self.error(format!("关键字“{name}”不能作为标识符"));
        }
        self.take(None)
    }
    fn parse(mut self) -> ParseResult<Program> {
        let mut out = Vec::new();
        self.skip_lines();
        while !matches!(self.token().kind, TokenKind::Eof) {
            out.push(self.statement()?);
            self.end_of_statement(&[])?;
            self.skip_lines();
        }
        Ok(crate::symbols::build(out))
    }
    fn statement(&mut self) -> ParseResult<Stmt> {
        if self.statement_depth >= MAX_SYNTAX_DEPTH {
            return self.depth_error();
        }
        self.statement_depth += 1;
        let result = self.statement_inner();
        self.statement_depth -= 1;
        result
    }
    fn statement_inner(&mut self) -> ParseResult<Stmt> {
        let line = self.token().line;
        match self.text().as_str() {
            "公开" | "内部" => {
                let public = self.text() == "公开";
                self.take(None)?;
                let statement = self.statement()?;
                Ok(Stmt::Export {
                    public,
                    statement: Box::new(statement),
                    line,
                })
            }
            "任务组" => {
                self.take(Some("任务组"))?;
                self.take(Some(":"))?;
                let body = self.block(&["结束"])?;
                self.take(Some("结束"))?;
                Ok(Stmt::TaskGroup { body, line })
            }
            "定义" => {
                self.take(Some("定义"))?;
                let name_token = self.ident_token(false)?;
                let name = name_token.text();
                self.take(Some("为"))?;
                let expr = self.expression(0)?;
                let type_constraint = if self.peek(",") {
                    self.take(None)?;
                    self.take(Some("类型限定为"))?;
                    Some(self.type_ref()?)
                } else {
                    None
                };
                Ok(Stmt::Define {
                    name,
                    name_span: name_token.span,
                    expr,
                    type_constraint,
                    line,
                })
            }
            "如果" => self.if_stmt(),
            "当" => self.while_stmt(),
            "遍历" => self.for_stmt(),
            "尝试" => self.try_stmt(),
            "要求" => self.require_stmt(),
            "匹配" => self.match_stmt(),
            "测试" => self.test_stmt(),
            "测试准备" => self.test_hook_stmt(TestHookStage::SuiteSetup),
            "每项测试前" => self.test_hook_stmt(TestHookStage::BeforeEach),
            "每项测试后" => self.test_hook_stmt(TestHookStage::AfterEach),
            "测试收尾" => self.test_hook_stmt(TestHookStage::SuiteTeardown),
            "定义功能" => self.function_stmt(),
            "功能" => self.error("旧的“功能”声明已移除，请改用“定义功能”".into()),
            "只要" => self.error("旧的条件循环已移除，请改用“当 条件 时重复：”".into()),
            "结构" => self.struct_stmt(),
            "返回" => {
                self.take(None)?;
                let expr = if matches!(self.token().kind, TokenKind::Newline | TokenKind::Eof) {
                    None
                } else {
                    Some(self.expression(0)?)
                };
                Ok(Stmt::Return { expr, line })
            }
            "跳出循环" => {
                self.take(None)?;
                Ok(Stmt::Break { target: None, line })
            }
            "继续下一轮" => {
                self.take(None)?;
                Ok(Stmt::Continue { target: None, line })
            }
            "抛出" => {
                self.take(None)?;
                Ok(Stmt::Throw {
                    expr: self.expression(0)?,
                    line,
                })
            }
            "全局" => {
                self.take(None)?;
                let (names, name_spans) = self.ident_list()?;
                Ok(Stmt::Global {
                    names,
                    name_spans,
                    line,
                })
            }
            "引入" => {
                self.take(None)?;
                let (module, binding_span) = self.module_name()?;
                Ok(Stmt::Import {
                    module,
                    names: None,
                    binding_spans: vec![binding_span],
                    line,
                })
            }
            "从" => {
                self.take(None)?;
                let (module, _) = self.module_name()?;
                self.take(Some("引入"))?;
                let (names, binding_spans) = self.ident_list()?;
                Ok(Stmt::Import {
                    module,
                    names: Some(names),
                    binding_spans,
                    line,
                })
            }
            _ => {
                let expr = self.expression(0)?;
                if self.peek("中的每个") {
                    return self.error("旧的遍历语法已移除，请改用“遍历 集合，记作 项：”".into());
                }
                if self.peek("为") {
                    if !matches!(
                        expr,
                        Expr::Name(_, _) | Expr::Attr { .. } | Expr::Index { .. }
                    ) {
                        return self.error("赋值目标只能是变量、属性或索引".into());
                    }
                    self.take(None)?;
                    return Ok(Stmt::Assign {
                        target: expr,
                        expr: self.expression(0)?,
                        line,
                    });
                }
                Ok(Stmt::Expr { expr, line })
            }
        }
    }
    fn try_stmt(&mut self) -> ParseResult<Stmt> {
        let line = self.take(Some("尝试"))?.line;
        self.take(Some(":"))?;
        let body = self.block(&["捕获", "最终", "结束"])?;
        let mut error_name = None;
        let mut error_span = None;
        let mut catch_body = Vec::new();
        if self.peek("捕获") {
            self.take(None)?;
            if !self.peek(":") {
                let token = self.ident_token(false)?;
                error_name = Some(token.text());
                error_span = Some(token.span);
            }
            self.take(Some(":"))?;
            catch_body = self.block(&["最终", "结束"])?;
        }
        let finally_body = if self.peek("最终") {
            self.take(None)?;
            self.take(Some(":"))?;
            self.block(&["结束"])?
        } else {
            Vec::new()
        };
        self.take(Some("结束"))?;
        if error_name.is_none() && catch_body.is_empty() && finally_body.is_empty() {
            return self.error("尝试语句至少需要捕获或最终分支".into());
        }
        Ok(Stmt::Try {
            body,
            error_name,
            error_span,
            catch_body,
            finally_body,
            line,
        })
    }
    fn require_stmt(&mut self) -> ParseResult<Stmt> {
        let line = self.take(Some("要求"))?.line;
        let expr = self.expression(0)?;
        let type_constraint = if self.peek("的类型是") {
            self.take(None)?;
            Some(self.type_ref()?)
        } else {
            None
        };
        let message = if self.peek(",") {
            self.take(None)?;
            self.take(Some("否则说明"))?;
            Some(self.expression(0)?)
        } else {
            None
        };
        Ok(Stmt::Require {
            expr,
            type_constraint,
            message,
            line,
        })
    }
    fn match_stmt(&mut self) -> ParseResult<Stmt> {
        let line = self.take(Some("匹配"))?.line;
        let expr = self.expression(0)?;
        self.take(Some(":"))?;
        self.skip_lines();
        let mut arms = Vec::new();
        while self.peek("当") {
            self.take(None)?;
            let pattern = if self.peek("类型是") {
                self.take(None)?;
                MatchPattern::Type(self.type_ref()?)
            } else if self.peek("记作") {
                self.take(None)?;
                let token = self.ident_token(false)?;
                MatchPattern::Bind {
                    name: token.text(),
                    span: token.span,
                }
            } else {
                MatchPattern::Value(self.expression(0)?)
            };
            let guard = if self.peek(",") {
                self.take(None)?;
                self.take(Some("并且满足"))?;
                Some(self.expression(0)?)
            } else {
                None
            };
            self.take(Some(":"))?;
            let body = self.match_arm_block()?;
            arms.push(MatchArm {
                pattern,
                guard,
                body,
            });
        }
        let otherwise = if self.peek("其他") {
            self.take(None)?;
            self.take(Some(":"))?;
            self.block(&["结束"])?
        } else {
            Vec::new()
        };
        self.take(Some("结束"))?;
        if arms.is_empty() {
            return self.error("匹配语句至少需要一个“当”分支".into());
        }
        Ok(Stmt::Match {
            expr,
            arms,
            otherwise,
            line,
        })
    }
    fn test_stmt(&mut self) -> ParseResult<Stmt> {
        let line = self.take(Some("测试"))?.line;
        let name = match self.token().kind.clone() {
            TokenKind::String(name) | TokenKind::Word(name) => {
                self.take(None)?;
                name
            }
            _ => return self.error("测试需要名称".into()),
        };
        self.take(Some(":"))?;
        let body = self.block(&["结束"])?;
        self.take(Some("结束"))?;
        Ok(Stmt::Test { name, body, line })
    }
    fn test_hook_stmt(&mut self, stage: TestHookStage) -> ParseResult<Stmt> {
        let keyword = self.text();
        let line = self.take(Some(&keyword))?.line;
        self.take(Some(":"))?;
        let body = self.block(&["结束"])?;
        self.take(Some("结束"))?;
        Ok(Stmt::TestHook { stage, body, line })
    }
    fn ident_list(&mut self) -> ParseResult<(Vec<String>, Vec<crate::diagnostic::SourceSpan>)> {
        let first = self.ident_token(false)?;
        let mut out = vec![first.text()];
        let mut spans = vec![first.span];
        while self.peek(",") {
            self.take(None)?;
            let token = self.ident_token(false)?;
            out.push(token.text());
            spans.push(token.span);
        }
        Ok((out, spans))
    }
    fn module_name(&mut self) -> ParseResult<(String, crate::diagnostic::SourceSpan)> {
        let first = self.ident_token(false)?;
        let mut out = first.text();
        let mut binding_span = first.span;
        while self.peek("/") {
            self.take(None)?;
            out.push('/');
            let token = self.ident_token(false)?;
            out.push_str(&token.text());
            binding_span = token.span;
        }
        Ok((out, binding_span))
    }
    fn block(&mut self, stops: &[&str]) -> ParseResult<Vec<Stmt>> {
        if self.block_depth >= MAX_SYNTAX_DEPTH {
            return self.depth_error();
        }
        self.block_depth += 1;
        let result = self.block_inner(stops);
        self.block_depth -= 1;
        result
    }
    fn block_inner(&mut self, stops: &[&str]) -> ParseResult<Vec<Stmt>> {
        let mut out = Vec::new();
        self.skip_lines();
        while !matches!(self.token().kind, TokenKind::Eof) && !stops.iter().any(|s| self.peek(s)) {
            out.push(self.statement()?);
            self.end_of_statement(stops)?;
            self.skip_lines();
        }
        Ok(out)
    }
    fn match_arm_block(&mut self) -> ParseResult<Vec<Stmt>> {
        let mut out = Vec::new();
        // 空分支同样需要终止符，否则同一行后面的“当”会被静默并进上一分支。
        if self.starts_match_arm() {
            self.end_of_statement(&["其他", "结束"])?;
        }
        self.skip_lines();
        while !matches!(self.token().kind, TokenKind::Eof)
            && !self.peek("其他")
            && !self.peek("结束")
            && !self.starts_match_arm()
        {
            out.push(self.statement()?);
            self.end_of_statement(&["其他", "结束"])?;
            self.skip_lines();
        }
        Ok(out)
    }
    fn starts_match_arm(&self) -> bool {
        self.peek("当") && !self.current_line_contains("时重复")
    }
    fn current_line_contains(&self, expected: &str) -> bool {
        self.tokens[self.i..]
            .iter()
            .take_while(|token| !matches!(token.kind, TokenKind::Newline | TokenKind::Eof))
            .any(|token| token_matches_syntax(token, expected))
    }
    fn if_stmt(&mut self) -> ParseResult<Stmt> {
        let line = self.take(Some("如果"))?.line;
        let condition = self.expression(0)?;
        self.take(Some(":"))?;
        let mut branches = vec![(condition, self.block(&["结束", "否则", "否则如果"])?)];
        while self.peek("否则") && self.peek_at(1, ",") && self.peek_at(2, "如果") {
            self.take(Some("否则"))?;
            self.take(Some(","))?;
            self.take(Some("如果"))?;
            let c = self.expression(0)?;
            self.take(Some(":"))?;
            branches.push((c, self.block(&["结束", "否则", "否则如果"])?));
        }
        if self.peek("否则如果") {
            return self.error("旧的“否则如果”已移除，请改用“否则，如果”".into());
        }
        let otherwise = if self.peek("否则") {
            self.take(None)?;
            self.take(Some(":"))?;
            self.block(&["结束"])?
        } else {
            Vec::new()
        };
        self.take(Some("结束"))?;
        Ok(Stmt::If {
            branches,
            otherwise,
            line,
        })
    }
    fn while_stmt(&mut self) -> ParseResult<Stmt> {
        let line = self.take(Some("当"))?.line;
        let condition = self.expression(0)?;
        self.take(Some("时重复"))?;
        self.take(Some(":"))?;
        let body = self.block(&["结束"])?;
        self.take(Some("结束"))?;
        Ok(Stmt::While {
            loop_id: LoopId(0),
            condition,
            body,
            line,
        })
    }
    fn for_stmt(&mut self) -> ParseResult<Stmt> {
        let line = self.take(Some("遍历"))?.line;
        let iterable = self.expression(0)?;
        self.take(Some(","))?;
        self.take(Some("记作"))?;
        let name_token = self.ident_token(false)?;
        let name = name_token.text();
        self.take(Some(":"))?;
        let body = self.block(&["结束"])?;
        self.take(Some("结束"))?;
        Ok(Stmt::For {
            loop_id: LoopId(0),
            iterable,
            name,
            name_span: name_token.span,
            body,
            line,
        })
    }
    fn params(&mut self, method: bool) -> ParseResult<Vec<Param>> {
        let mut out = Vec::new();
        let mut names = BTreeSet::new();
        self.skip_continuation_lines();
        if self.peek(")") {
            return Ok(out);
        }
        loop {
            let name_token = self.ident_token(method && out.is_empty())?;
            let name = name_token.text();
            if !names.insert(name.clone()) {
                return self.error(format!("形式参数“{name}”重复定义"));
            }
            let default = if self.peek("为") {
                self.take(None)?;
                self.skip_continuation_lines();
                Some(self.expression(0)?)
            } else {
                None
            };
            out.push(Param {
                name,
                name_span: name_token.span,
                default,
            });
            self.skip_continuation_lines();
            if !self.peek(",") {
                break;
            }
            self.take(None)?;
            self.skip_continuation_lines();
        }
        Ok(out)
    }
    fn function_parts(&mut self) -> ParseResult<FunctionParts> {
        let keyword = self.take(Some("定义功能"))?;
        let line = keyword.line;
        let (name, name_span, type_name) = if self.peek("(") {
            (None, None, None)
        } else {
            let first = self.ident_token(false)?;
            if self.peek("(") {
                (Some(first.text()), Some(first.span), None)
            } else {
                let name = self.ident_token(false)?;
                (
                    Some(name.text()),
                    Some(name.span),
                    Some(TypeRef {
                        name: first.text(),
                        span: first.span,
                    }),
                )
            }
        };
        self.take(Some("("))?;
        let params = self.params(type_name.is_some())?;
        self.take(Some(")"))?;
        let body = if self.peek("为") {
            self.take(None)?;
            vec![Stmt::Return {
                expr: Some(self.expression(0)?),
                line,
            }]
        } else {
            self.take(Some(":"))?;
            let body = self.block(&["结束"])?;
            self.take(Some("结束"))?;
            body
        };
        Ok((name, name_span, keyword.span, type_name, params, body, line))
    }
    fn function_stmt(&mut self) -> ParseResult<Stmt> {
        let (name, name_span, _, type_name, params, body, line) = self.function_parts()?;
        let Some(name) = name else {
            return self.error("具名功能缺少名称".into());
        };
        Ok(Stmt::Function {
            name,
            name_span: name_span.unwrap_or_else(|| crate::diagnostic::SourceSpan::line(line)),
            params,
            body,
            type_name,
            line,
        })
    }
    fn struct_stmt(&mut self) -> ParseResult<Stmt> {
        let line = self.take(Some("结构"))?.line;
        let name_token = self.ident_token(false)?;
        let name = name_token.text();
        self.take(Some(":"))?;
        self.skip_lines();
        let mut fields = Vec::new();
        let mut field_spans = Vec::new();
        let mut names = BTreeSet::new();
        while !matches!(self.token().kind, TokenKind::Eof) && !self.peek("结束") {
            let token = self.ident_token(false)?;
            let field = token.text();
            if !names.insert(field.clone()) {
                return self.error(format!("结构字段“{field}”重复定义"));
            }
            fields.push(field);
            field_spans.push(token.span);
            self.end_of_statement(&["结束"])?;
            self.skip_lines();
        }
        self.take(Some("结束"))?;
        Ok(Stmt::Struct {
            name,
            name_span: name_token.span,
            fields,
            field_spans,
            line,
        })
    }
    fn precedence(op: &str) -> Option<u8> {
        Some(match op {
            "或" => 1,
            "且" => 2,
            "=" | "==" | "!=" | ">" | "<" | ">=" | "<=" => 3,
            "属于" | "不属于" => 4,
            "+" | "-" => 5,
            "*" | "/" | "%" => 6,
            "^" => 7,
            _ => return None,
        })
    }
    fn expression(&mut self, min: u8) -> ParseResult<Expr> {
        if self.expression_depth >= MAX_SYNTAX_DEPTH {
            return self.depth_error();
        }
        self.expression_depth += 1;
        let result = self.expression_inner(min);
        self.expression_depth -= 1;
        result
    }
    fn expression_inner(&mut self, min: u8) -> ParseResult<Expr> {
        let mut left = self.unary()?;
        let mut chain = 0;
        loop {
            let op = self.text();
            let Some(p) = Self::precedence(&op) else {
                break;
            };
            if p < min {
                break;
            }
            chain += 1;
            self.check_chain_depth(chain)?;
            self.take(None)?;
            let next = if op == "^" { p } else { p + 1 };
            self.skip_continuation_lines();
            let right = self.expression(next)?;
            let line = left.line();
            left = Expr::Binary {
                left: Box::new(left),
                op,
                right: Box::new(right),
                line,
            };
        }
        Ok(left)
    }
    fn unary(&mut self) -> ParseResult<Expr> {
        if self.unary_depth >= MAX_SYNTAX_DEPTH {
            return self.depth_error();
        }
        self.unary_depth += 1;
        let result = self.unary_inner();
        self.unary_depth -= 1;
        result
    }
    fn unary_inner(&mut self) -> ParseResult<Expr> {
        if self.peek("非") || self.peek("-") {
            let t = self.take(None)?;
            return Ok(Expr::Unary {
                op: t.text(),
                expr: Box::new(self.unary()?),
                line: t.line,
            });
        }
        self.postfix()
    }
    fn postfix(&mut self) -> ParseResult<Expr> {
        let mut value = self.primary()?;
        let mut chain = 0;
        loop {
            chain += 1;
            self.check_chain_depth(chain)?;
            if self.peek("的") || self.peek("之") {
                let line = self.take(None)?.line;
                let token = self.ident_token(false)?;
                let name = token.text();
                value = Expr::Attr {
                    receiver: Box::new(value),
                    name,
                    name_span: token.span,
                    line,
                };
                continue;
            }
            if self.peek("[") {
                let line = self.take(None)?.line;
                let index = self.expression(0)?;
                self.take(Some("]"))?;
                value = Expr::Index {
                    receiver: Box::new(value),
                    index: Box::new(index),
                    line,
                };
                continue;
            }
            if matches!(self.token().kind, TokenKind::Word(_)) && self.peek_at(1, "(") {
                let t = self.take(None)?;
                self.take(Some("("))?;
                let (args, named) = self.arguments()?;
                self.take(Some(")"))?;
                value = Expr::Call {
                    receiver: Box::new(value),
                    name: Some(t.text()),
                    name_span: Some(t.span),
                    args,
                    named,
                    line: t.line,
                };
                continue;
            }
            if self.peek("(") {
                let line = self.take(None)?.line;
                let (args, named) = self.arguments()?;
                self.take(Some(")"))?;
                value = Expr::Call {
                    receiver: Box::new(value),
                    name: None,
                    name_span: None,
                    args,
                    named,
                    line,
                };
                continue;
            }
            break;
        }
        Ok(value)
    }
    fn arguments(&mut self) -> ParseResult<(Vec<Expr>, Vec<NamedArg>)> {
        let mut args = Vec::new();
        let mut named = Vec::new();
        let mut named_names = BTreeSet::new();
        let mut saw_named = false;
        self.skip_continuation_lines();
        if self.peek(")") {
            return Ok((args, named));
        }
        loop {
            if matches!(self.token().kind, TokenKind::Word(_)) && self.peek_at(1, "为") {
                saw_named = true;
                let token = self.take(None)?;
                let n = token.text();
                self.take(None)?;
                let value = self.expression(0)?;
                if !named_names.insert(n.clone()) {
                    return self.error(format!("具名实参“{n}”重复提供"));
                }
                named.push(NamedArg {
                    name: n,
                    name_span: token.span,
                    value,
                });
            } else {
                if saw_named {
                    return self.error("位置实参不能出现在具名实参之后".into());
                }
                args.push(self.expression(0)?);
            }
            self.skip_continuation_lines();
            if !self.peek(",") {
                break;
            }
            self.take(None)?;
            self.skip_continuation_lines();
        }
        Ok((args, named))
    }
    fn primary(&mut self) -> ParseResult<Expr> {
        let token = self.token().clone();
        match token.kind {
            TokenKind::Number(v) => {
                self.take(None)?;
                Ok(Expr::Literal(Literal::Number(v), token.line))
            }
            TokenKind::String(v) => {
                self.take(None)?;
                Ok(Expr::Literal(Literal::Text(v), token.line))
            }
            TokenKind::Word(ref v) if matches!(v.as_str(), "是" | "否" | "空") => {
                self.take(None)?;
                Ok(Expr::Literal(
                    match v.as_str() {
                        "是" => Literal::Bool(true),
                        "否" => Literal::Bool(false),
                        _ => Literal::Null,
                    },
                    token.line,
                ))
            }
            TokenKind::Symbol(ref v) if v == "(" => {
                self.take(None)?;
                self.skip_continuation_lines();
                let value = self.expression(0)?;
                self.skip_continuation_lines();
                self.take(Some(")"))?;
                Ok(value)
            }
            TokenKind::Symbol(ref v) if v == "[" => {
                self.take(None)?;
                self.skip_continuation_lines();
                let mut items = Vec::new();
                if !self.peek("]") {
                    loop {
                        items.push(self.expression(0)?);
                        self.skip_continuation_lines();
                        if !self.peek(",") {
                            break;
                        }
                        self.take(None)?;
                        self.skip_continuation_lines();
                    }
                }
                self.skip_continuation_lines();
                self.take(Some("]"))?;
                Ok(Expr::List(items, token.line))
            }
            TokenKind::Symbol(ref v) if v == "{" => {
                self.take(None)?;
                self.skip_continuation_lines();
                let mut items = Vec::new();
                let mut keys = BTreeSet::new();
                if !self.peek("}") {
                    loop {
                        let key = self.take(None)?;
                        let key = match key.kind {
                            TokenKind::String(key) => key,
                            TokenKind::Word(key) if !is_keyword(&key) => key,
                            _ => return self.error("记录的键必须是标识符或文本".into()),
                        };
                        if !keys.insert(key.clone()) {
                            return self.error(format!("记录键“{key}”重复定义"));
                        }
                        self.take(Some(":"))?;
                        self.skip_continuation_lines();
                        items.push((key, self.expression(0)?));
                        self.skip_continuation_lines();
                        if !self.peek(",") {
                            break;
                        }
                        self.take(None)?;
                        self.skip_continuation_lines();
                    }
                }
                self.skip_continuation_lines();
                self.take(Some("}"))?;
                Ok(Expr::Record(items, token.line))
            }
            TokenKind::Word(ref v) if v == "定义功能" => {
                let (name, _, declaration_span, type_name, params, body, line) =
                    self.function_parts()?;
                if name.is_some() || type_name.is_some() {
                    return self
                        .error("表达式位置只允许“定义功能（参数）：...结束”匿名形式".into());
                }
                Ok(Expr::Function {
                    name,
                    declaration_span,
                    params,
                    body,
                    type_name,
                    line,
                })
            }
            TokenKind::Word(ref v) if v == "功能" => {
                self.error("旧的“功能”表达式已移除，请改用“定义功能”".into())
            }
            TokenKind::Word(v) => {
                self.take(None)?;
                Ok(Expr::Name(v, token.span))
            }
            _ => self.error(format!("无法解析“{}”", token.text())),
        }
    }
}

fn token_matches_syntax(token: &Token, value: &str) -> bool {
    matches!(&token.kind, TokenKind::Word(text) | TokenKind::Symbol(text) if text == value)
}

pub(crate) const CURRENT_KEYWORDS: &[&str] = &[
    "公开",
    "内部",
    "任务组",
    "定义",
    "为",
    "类型限定为",
    "如果",
    "否则",
    "遍历",
    "记作",
    "当",
    "时重复",
    "尝试",
    "捕获",
    "最终",
    "要求",
    "否则说明",
    "匹配",
    "其他",
    "并且满足",
    "测试",
    "测试准备",
    "每项测试前",
    "每项测试后",
    "测试收尾",
    "定义功能",
    "结构",
    "返回",
    "跳出循环",
    "继续下一轮",
    "抛出",
    "全局",
    "引入",
    "从",
    "结束",
    "类型是",
    "的类型是",
    "且",
    "或",
    "非",
    "属于",
    "不属于",
    "的",
    "之",
    "输出",
    "参数化输出",
    "输入",
    "自己",
    "是",
    "否",
    "空",
];

pub(crate) const REMOVED_KEYWORDS: &[&str] = &[
    "否则如果",
    "只要",
    "就一直进行",
    "中的每个",
    "都进行",
    "功能",
];

pub(crate) fn is_keyword(word: &str) -> bool {
    CURRENT_KEYWORDS.contains(&word) || REMOVED_KEYWORDS.contains(&word)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_core_program() {
        let source = "定义 总计 为 0\n当 总计 < 3 时重复：\n总计 为 总计 + 1\n结束\n输出(总计)";
        assert_eq!(parse(source).unwrap().len(), 3);
    }
    #[test]
    fn parses_method_chain_and_named_args() {
        parse("定义 汇总 为 表 分组(\"部门\") 求和(\"金额\", 精度 为 2)").unwrap();
    }
    #[test]
    fn parses_natural_contract_try_and_match() {
        parse(
            "定义 金额 为 1，类型限定为 数字\n要求 金额 的类型是 数字\n尝试：\n抛出 \"错\"\n捕获 异常：\n输出(异常 的 信息)\n最终：\n输出(\"完成\")\n结束\n匹配 金额：\n当 类型是 数字，并且满足 当前值 > 0：\n输出(当前值)\n其他：\n输出(0)\n结束",
        )
        .unwrap();
    }

    #[test]
    fn parses_multiline_collections_calls_and_continued_expression() {
        let source = r#"
定义 数据 为 [
    1,
    2,
    3
]
定义 记录 为 {
    "名称": "衡枢",
    "数量": 3
}
输出(
    1 +
    2
)
输出(长度(
    数据
))
"#;
        let program = parse(source).unwrap();
        assert_eq!(program.len(), 4);
    }

    #[test]
    fn parses_test_lifecycle_and_multiline_failure_assertion() {
        let source = r#"
测试准备：
    定义 基准 为 [1]
结束
每项测试前：
    基准 加入（2）
结束
测试 "中文断言"：
    断言会失败（
        定义功能（）：
            抛出 "预期错误"
        结束，
        信息包含 为 "预期"
    ）
结束
每项测试后：
    断言不为空（基准）
结束
测试收尾：
    断言相等（长度（基准），1）
结束
"#;
        let program = parse(source).unwrap();
        assert_eq!(program.len(), 5);
        assert!(matches!(program[0], Stmt::TestHook { .. }));
        assert!(matches!(program[2], Stmt::Test { .. }));
    }

    #[test]
    fn preserves_newline_as_statement_boundary_outside_delimiters() {
        let program = parse("输出(1)\n输出(2)").unwrap();
        assert_eq!(program.len(), 2);
    }

    #[test]
    fn requires_a_statement_terminator_between_statements_on_one_line() {
        for source in [
            "输出 1",
            "定义 金额 为 1\n参数化输出 \"{金额}\"",
            "定义功能 甲（）：\n输出 1\n结束\n甲()",
            "匹配 1：\n当 类型是 数字：\n输出 1\n结束",
        ] {
            let error = parse(source).unwrap_err();
            assert!(error.contains("一行只能写一条语句"), "{source}：{error}");
        }
        parse("输出(1); 输出(2)").unwrap();
        parse("定义 甲 为 定义功能（）：返回 1 结束\n输出(甲())").unwrap();
    }

    #[test]
    fn rejects_invalid_assignment_targets_and_keyword_identifiers() {
        assert!(parse("1 为 2").unwrap_err().contains("赋值目标"));
        assert!(parse("定义 如果 为 1").unwrap_err().contains("关键字"));
        for keyword in ["的", "之", "输出", "参数化输出", "输入", "自己"] {
            assert!(
                parse(&format!("定义 {keyword} 为 1"))
                    .unwrap_err()
                    .contains("关键字"),
                "应拒绝关键字 {keyword}"
            );
        }
        assert!(parse("定义功能 普通（自己）：\n返回 自己\n结束")
            .unwrap_err()
            .contains("关键字"));
        parse("定义功能 客户 升级（自己）：\n返回 自己\n结束").unwrap();
    }

    #[test]
    fn rejects_duplicate_parameters_named_arguments_record_keys_and_struct_fields() {
        for (source, expected) in [
            (
                "定义功能 取值（甲，甲）：\n返回 甲\n结束",
                "形式参数“甲”重复定义",
            ),
            (
                "定义功能 取值（甲）：\n返回 甲\n结束\n取值（甲 为 1，甲 为 2）",
                "具名实参“甲”重复提供",
            ),
            ("定义 记录值 为 {名称: 1，名称: 2}", "记录键“名称”重复定义"),
            ("结构 客户：\n姓名\n姓名\n结束", "结构字段“姓名”重复定义"),
        ] {
            assert!(parse(source).unwrap_err().contains(expected), "{source}");
        }
    }

    #[test]
    fn preserves_named_argument_order_and_rejects_invalid_expression_functions() {
        let program = parse("调用(乙 为 1, 甲 为 2)").unwrap();
        let Stmt::Expr {
            expr: Expr::Call { named, .. },
            ..
        } = &program[0]
        else {
            panic!("应解析为调用表达式");
        };
        assert_eq!(
            named
                .iter()
                .map(|argument| argument.name.as_str())
                .collect::<Vec<_>>(),
            ["乙", "甲"]
        );
        assert!(parse("调用(甲 为 1, 2)")
            .unwrap_err()
            .contains("位置实参不能出现在具名实参之后"));
        assert!(parse("定义 值 为 定义功能 具名（）：\n结束")
            .unwrap_err()
            .contains("表达式位置只允许"));
        assert!(parse("定义 值 为 定义功能 客户 方法（自己）：\n结束")
            .unwrap_err()
            .contains("表达式位置只允许"));
    }

    #[test]
    fn record_keys_accept_identifiers_and_text() {
        let program = parse("定义 记录值 为 {名称: \"衡枢\"，\"版本\": 0.9}").unwrap();
        let Stmt::Define { expr, .. } = &program[0] else {
            panic!("需要定义语句");
        };
        let Expr::Record(items, _) = expr else {
            panic!("需要记录表达式");
        };
        assert_eq!(items[0].0, "名称");
        assert_eq!(items[1].0, "版本");
    }

    #[test]
    fn removed_control_syntax_reports_the_replacement() {
        for (source, replacement) in [
            ("功能 计算（）：\n结束", "定义功能"),
            ("如果 是：\n否则如果 否：\n结束", "否则，如果"),
            ("[1] 中的每个 项 都进行：\n结束", "遍历 集合，记作 项"),
            ("只要 是 就一直进行：\n结束", "当 条件 时重复"),
        ] {
            let error = parse(source).unwrap_err();
            assert!(error.contains(replacement), "源码：{source}\n错误：{error}");
        }
    }

    #[test]
    fn string_contents_are_not_treated_as_syntax_tokens() {
        for content in [")", "]", "}", ",", ":", "为", "结束"] {
            let source = format!("输出(\"{content}\")");
            assert!(parse(&source).is_ok(), "应解析字符串内容 {content:?}");
        }
    }

    #[test]
    fn public_keyword_manifests_stay_synchronized() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let api: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(root.join("skills/hengshu/api.json")).unwrap(),
        )
        .unwrap();
        let api_keywords = api["keywords"]
            .as_array()
            .unwrap()
            .iter()
            .map(|value| value.as_str().unwrap())
            .collect::<Vec<_>>();
        let removed = api["removed_keywords"]
            .as_array()
            .unwrap()
            .iter()
            .map(|value| value.as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(api_keywords, CURRENT_KEYWORDS);
        assert_eq!(removed, REMOVED_KEYWORDS);
        assert_eq!(api["version"], env!("CARGO_PKG_VERSION"));

        let modules = api["modules"].as_object().unwrap();
        for name in [
            "文件系统",
            "路径处理",
            "编码",
            "JSON",
            "TOML",
            "日期时间",
            "环境",
            "进程",
            "正则",
            "压缩",
            "网络",
            "网页",
            "数据库",
            "密码",
            "单位",
            "交付",
            "隐私",
            "对账",
            "日志",
        ] {
            let baseline = modules
                .get(name)
                .unwrap_or_else(|| panic!("API 基线缺少模块 {name}"))
                .as_array()
                .unwrap()
                .iter()
                .map(|value| value.as_str().unwrap())
                .collect::<Vec<_>>();
            for function in crate::stdlib::module_functions(name).unwrap() {
                assert!(
                    baseline.contains(function),
                    "API 基线的模块 {name} 缺少函数 {function}"
                );
            }
        }

        let grammar: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(root.join("editors/vscode/syntaxes/hengshu.tmLanguage.json"))
                .unwrap(),
        )
        .unwrap();
        let highlighted = grammar["patterns"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|pattern| {
                pattern["name"].as_str().is_some_and(|name| {
                    name.starts_with("keyword.") || name == "constant.language.hengshu"
                })
            })
            .filter_map(|pattern| pattern["match"].as_str())
            .flat_map(|pattern| {
                pattern
                    .trim_start_matches("(?:")
                    .trim_end_matches(')')
                    .split('|')
            })
            .collect::<std::collections::BTreeSet<_>>();
        for keyword in CURRENT_KEYWORDS {
            assert!(
                highlighted.contains(keyword),
                "VS Code 缺少关键字 {keyword}"
            );
        }
        for keyword in REMOVED_KEYWORDS {
            assert!(
                !highlighted.contains(keyword),
                "VS Code 不应把已移除词 {keyword} 作为现行关键字"
            );
        }

        let reference = std::fs::read_to_string(root.join("docs/语言参考.md")).unwrap();
        for keyword in CURRENT_KEYWORDS {
            assert!(
                reference.contains(&format!("`{keyword}`")),
                "语言参考缺少关键字 {keyword}"
            );
        }
    }

    /// 预算级深度的解析必须在大栈线程上进行，与所有实际入口一致。
    fn parse_on_compile_stack(source: &str) -> Result<Program, String> {
        crate::resource::on_compile_stack_scoped(|| parse(source)).unwrap()
    }

    #[test]
    fn rejects_expression_nesting_beyond_limit_without_panicking() {
        let source = format!(
            "输出({})",
            std::iter::repeat_n("(", MAX_SYNTAX_DEPTH + 8)
                .chain(std::iter::once("1"))
                .chain(std::iter::repeat_n(")", MAX_SYNTAX_DEPTH + 8))
                .collect::<String>()
        );
        let result = std::panic::catch_unwind(|| parse_on_compile_stack(&source));
        assert!(result.is_ok());
        assert!(result.unwrap().is_err());
    }

    #[test]
    fn accepts_reasonable_expression_nesting() {
        let depth = 8;
        let source = format!(
            "输出({})",
            std::iter::repeat_n("(", depth)
                .chain(std::iter::once("1"))
                .chain(std::iter::repeat_n(")", depth))
                .collect::<String>()
        );
        assert!(parse(&source).is_ok());
    }

    #[test]
    fn rejects_deep_unary_power_and_block_nesting() {
        let unary = format!(
            "输出({}1)",
            std::iter::repeat_n("非 ", MAX_SYNTAX_DEPTH + 1).collect::<String>()
        );
        let power = format!(
            "输出({})",
            std::iter::repeat_n("1 ^ ", MAX_SYNTAX_DEPTH + 1).collect::<String>() + "1"
        );
        let blocks = format!(
            "{}输出(1)\n{}",
            std::iter::repeat_n("如果 是：\n", MAX_SYNTAX_DEPTH + 1).collect::<String>(),
            std::iter::repeat_n("结束\n", MAX_SYNTAX_DEPTH + 1).collect::<String>()
        );
        for source in [unary, power, blocks] {
            let result = std::panic::catch_unwind(|| parse_on_compile_stack(&source));
            assert!(result.is_ok());
            assert!(result.unwrap().is_err());
        }
    }

    #[test]
    fn rejects_flat_chains_that_build_a_deep_syntax_tree() {
        let sum = format!(
            "输出({})",
            std::iter::repeat_n("1", MAX_SYNTAX_DEPTH + 2)
                .collect::<Vec<_>>()
                .join(" + ")
        );
        let attributes = format!(
            "输出(值{})",
            std::iter::repeat_n(" 的 项", MAX_SYNTAX_DEPTH + 2).collect::<String>()
        );
        for source in [sum, attributes] {
            let error = parse_on_compile_stack(&source).unwrap_err();
            assert!(
                error.contains(&format!("语法嵌套深度超过 {MAX_SYNTAX_DEPTH} 层")),
                "{error}"
            );
        }
        let accepted = format!(
            "输出({})",
            std::iter::repeat_n("1", MAX_SYNTAX_DEPTH / 2)
                .collect::<Vec<_>>()
                .join(" + ")
        );
        assert!(parse_on_compile_stack(&accepted).is_ok());
    }

    #[test]
    fn requires_terminators_in_struct_fields_and_empty_match_arms() {
        for source in [
            "结构 客户：\n姓名 等级\n结束",
            "定义 值 为 1\n匹配 值：\n当 1： 当 2：\n输出(2)\n结束\n结束",
        ] {
            let error = parse(source).unwrap_err();
            assert!(error.contains("一行只能写一条语句"), "{error}");
        }
        assert!(parse("结构 客户：\n姓名\n等级\n结束").is_ok());
        assert!(parse("定义 值 为 1\n匹配 值：\n当 1：\n当 2：\n输出(2)\n结束").is_ok());
    }
}
