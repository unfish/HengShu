use crate::value::RuntimeIssue;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RuntimeError {
    pub span: crate::diagnostic::SourceSpan,
    pub category: String,
    pub message: String,
    pub stack: Box<[String]>,
    pub cause: Option<Box<RuntimeError>>,
    pub definition_span: Option<Box<crate::diagnostic::SourceSpan>>,
    pub termination: crate::execution::RunTermination,
}

impl RuntimeError {
    pub(crate) fn new(line: usize, message: String) -> Self {
        let (category, message, termination) =
            if let Some(reason) = message.strip_prefix(super::CANCELLED_ERROR_MARK) {
                (
                    "资源错误",
                    reason.to_string(),
                    crate::execution::RunTermination::Cancelled,
                )
            } else if let Some(reason) = message.strip_prefix(super::TIMED_OUT_ERROR_MARK) {
                (
                    "资源错误",
                    reason.to_string(),
                    crate::execution::RunTermination::TimedOut,
                )
            } else if let Some(reason) = message.strip_prefix(super::RESOURCE_ERROR_MARK) {
                (
                    "资源错误",
                    reason.to_string(),
                    crate::execution::RunTermination::Failed,
                )
            } else {
                (
                    "运行错误",
                    message,
                    crate::execution::RunTermination::Failed,
                )
            };
        Self {
            span: crate::diagnostic::SourceSpan::line(line),
            category: category.into(),
            message,
            stack: Box::new([]),
            cause: None,
            definition_span: None,
            termination,
        }
    }

    pub(crate) fn from_issue(issue: &RuntimeIssue, fallback_line: usize) -> Self {
        Self {
            span: crate::diagnostic::SourceSpan::line(if issue.line == 0 {
                fallback_line
            } else {
                issue.line
            }),
            category: issue.category.clone(),
            message: issue.message.clone(),
            stack: issue.stack.clone().into_boxed_slice(),
            cause: issue
                .cause
                .as_deref()
                .map(|cause| Box::new(Self::from_issue(cause, fallback_line))),
            definition_span: issue.definition_span.clone(),
            termination: crate::execution::RunTermination::Failed,
        }
    }

    pub(crate) fn from_diagnostic(diagnostic: crate::diagnostic::Diagnostic) -> Self {
        Self {
            span: diagnostic.span,
            category: diagnostic.stage.category().into(),
            message: diagnostic.message,
            stack: Box::new([]),
            cause: None,
            definition_span: None,
            termination: crate::execution::RunTermination::Failed,
        }
    }

    pub(crate) fn with_category(mut self, category: impl Into<String>) -> Self {
        self.category = category.into();
        self
    }

    pub(crate) fn with_termination(
        mut self,
        termination: crate::execution::RunTermination,
    ) -> Self {
        self.termination = termination;
        self
    }

    pub(crate) fn render(&self) -> String {
        let mut output =
            crate::diagnostic::render_location(self.span, &self.category, &self.message);
        if let Some(cause) = self.cause.as_deref() {
            output.push_str("\n原因：");
            output.push_str(&cause.render());
        }
        output
    }

    pub(crate) fn report(&self) -> crate::diagnostic::DiagnosticReport {
        crate::diagnostic::DiagnosticReport {
            category: Some(self.category.clone()),
            span: Some(self.span),
            message: self.message.clone(),
            stack: self.stack.to_vec(),
            definition_span: self.definition_span.as_deref().copied(),
            cause: self.cause.as_deref().map(|cause| Box::new(cause.report())),
        }
    }

    pub(crate) fn with_fallback_line(mut self, line: usize) -> Self {
        if self.span.start.line == 0 {
            self.span = crate::diagnostic::SourceSpan::line(line);
        }
        self
    }

    pub(crate) fn into_issue(self, stack: Vec<String>) -> RuntimeIssue {
        RuntimeIssue {
            category: self.category,
            message: self.message,
            line: self.span.start.line,
            definition_span: self.definition_span,
            cause: self
                .cause
                .map(|cause| Box::new(cause.into_issue(Vec::new()))),
            stack: if self.stack.is_empty() {
                stack
            } else {
                self.stack.into_vec()
            },
        }
    }

    pub(crate) fn with_cause(mut self, cause: RuntimeError) -> Self {
        self.cause = Some(Box::new(cause));
        self
    }

    pub(crate) fn append_cause(&mut self, cause: RuntimeError) {
        let mut tail = &mut self.cause;
        while let Some(existing) = tail {
            tail = &mut existing.cause;
        }
        *tail = Some(Box::new(cause));
    }

    pub(crate) fn with_definition_span(
        mut self,
        span: Option<crate::diagnostic::SourceSpan>,
    ) -> Self {
        self.definition_span = span.map(Box::new);
        self
    }

    pub(crate) fn at_parameter_call_site(
        mut self,
        line: usize,
        definition_span: crate::diagnostic::SourceSpan,
    ) -> Self {
        let original = self.clone();
        self.span = crate::diagnostic::SourceSpan::line(line);
        self.definition_span = Some(Box::new(definition_span));
        self.cause = Some(Box::new(original));
        self
    }
}

impl From<String> for RuntimeError {
    fn from(message: String) -> Self {
        Self::new(0, message)
    }
}

impl From<&str> for RuntimeError {
    fn from(message: &str) -> Self {
        Self::new(0, message.to_string())
    }
}

impl std::fmt::Display for RuntimeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for RuntimeError {}

#[cfg(test)]
mod tests {
    use crate::runtime::interpreter::execute_with_options_structured;
    use crate::runtime::test_support::run;
    use crate::security::PermissionSet;
    use std::path::Path;

    #[test]
    fn structured_errors_keep_nested_and_caught_source_lines() {
        let source = "定义功能 计算（）：\n定义 零 为 0\n返回 1 / 零\n结束\n计算()";
        let error = match execute_with_options_structured(
            source,
            ".",
            false,
            Vec::new(),
            PermissionSet::for_source(Path::new(".")),
            false,
        ) {
            Ok(_) => panic!("嵌套功能应该返回运行错误"),
            Err(error) => error,
        };
        assert_eq!(error.span.start.line, 3);
        assert_eq!(error.stack.as_ref(), ["计算"]);

        let caught = run("定义功能 计算（）：\n定义 零 为 0\n返回 1 / 零\n结束\n尝试：\n计算()\n捕获 异常：\n输出(异常 的 行号)\n结束");
        assert_eq!(caught, vec!["3"]);
    }

    #[test]
    fn rethrowing_a_structured_error_preserves_its_category() {
        let result = execute_with_options_structured(
            "抛出 错误(\"失败\"，类别 为 \"自定义\")",
            ".",
            false,
            Vec::new(),
            PermissionSet::for_source(Path::new(".")),
            false,
        );
        let error = match result {
            Ok(_) => panic!("抛出错误应该失败"),
            Err(error) => error,
        };
        assert_eq!(error.category, "自定义");
        assert_eq!(error.message, "失败");
    }

    #[test]
    fn finally_error_keeps_the_overridden_error_as_its_cause() {
        let result = execute_with_options_structured(
            "尝试：\n尝试：\n抛出 错误(\"原始\"，类别 为 \"原始类别\")\n最终：\n抛出 错误(\"覆盖\"，类别 为 \"覆盖类别\")\n结束\n捕获 已捕获：\n抛出 已捕获\n结束",
            ".",
            false,
            Vec::new(),
            PermissionSet::for_source(Path::new(".")),
            false,
        );
        let error = match result {
            Ok(_) => panic!("最终错误应该覆盖原错误"),
            Err(error) => error,
        };
        assert_eq!(error.category, "覆盖类别");
        assert_eq!(error.message, "覆盖");
        let cause = error.cause.expect("覆盖错误应该保留原错误原因");
        assert_eq!(cause.category, "原始类别");
        assert_eq!(cause.message, "原始");
    }
}
