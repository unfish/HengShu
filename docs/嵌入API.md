# 衡枢 Rust 嵌入 API

衡枢同时提供命令行程序和 Rust 库。0.9 系列的公共入口位于 `hengshu::api`，用于静态检查、解释执行、编译字节码和执行已验证字节码。

```rust
use hengshu::api::{check_source, compile_source, run_compiled, run_source, RunOptions};

check_source("定义 金额 为 12.5")?;
let result = run_source("输出(12.5 * 2)", RunOptions::new("."))?;
assert_eq!(result.output, ["25.0"]);

let compiled = compile_source("输出(1 + 2)")?;
compiled.verify()?;
let vm_result = run_compiled("输出(1 + 2)", RunOptions::new("."))?;
assert_eq!(vm_result.output, ["3"]);
# Ok::<(), hengshu::api::HengshuError>(())
```

`RunOptions` 允许调用方设置源码基准目录、程序参数、输出方式和 `PermissionSet`。默认权限只允许访问基准目录，网络、环境变量和子进程默认关闭。

`HengshuError` 提供处理阶段、可选源码位置和中文消息。`line()`与`column()`返回从 1 开始的起始行列，`span()`返回由`SourcePosition`和`SourceSpan`表示的完整半开范围。运行时错误还通过`category()`、`stack()`和`cause()`暴露错误类别、调用栈和原因链；参数调用错误的`definition_span()`返回相关参数声明范围，主`span()`始终指向调用点。词法、解析、静态检查和运行时错误都直接保留结构化范围；运行时当前以语句或表达式行为范围，列号为 1。命令行只负责显示错误；嵌入方应检查`ErrorKind`、`category()`和位置字段，不应通过匹配完整消息文本判断错误类型。

失败同样是一次执行：`HengshuError::output()`返回程序在失败之前已经打印的每一行，解释器和字节码 VM 两条路径给出相同结果。`HengshuError::report()`返回`DiagnosticReport`，把类别、范围、消息、调用栈、参数定义范围和原因链固化为一个可序列化结构，嵌入方不必从渲染文本反解。

```rust
use hengshu::api::{run_source, RunOptions};

let error = run_source("输出(\"先打印\")\n输出(1 / 0)", RunOptions::new(".")).unwrap_err();
assert_eq!(error.output(), ["先打印"]);
let report = error.report();
assert_eq!(report.category.as_deref(), Some("运行错误"));
```

`DiagnosticReport`也是命令行与独立包的机器可读输出：把`api::DIAGNOSTIC_FILE_VARIABLE`（`HENGSHU_DIAGNOSTIC_FILE`）设为一个文件路径，`衡枢 运行`和打包后的程序在失败时会把同一份 JSON 写入该文件，标准错误仍只显示面向人的中文文本。任何失败都以`api::FAILURE_EXIT_CODE`退出。

`check_source`、`run_source`、`compile_source`和`format_source`统一限制源码为 16 MiB，超限返回`ErrorKind::Resource`。

## 当前公开接口

- `api::check_source`
- `api::run_source`、`RunOptions` 和 `RunResult`
- `api::compile_source`、`api::run_compiled`、`CompiledProgram` 的查询/验证/执行/序列化方法，以及 `api::format_source`
- `HengshuError`、`ErrorKind`、`SourcePosition`、`SourceSpan` 和 `PermissionSet`
- `DiagnosticReport`、`api::DIAGNOSTIC_FILE_VARIABLE` 和 `api::FAILURE_EXIT_CODE`

AST、解释器环境、运行时 `Value`、字节码内部结构和 VM 不属于公开 API。`ErrorKind` 标记为 `non_exhaustive`，以便后续添加更精确的错误类别。项目正式发布前，这些接口不承诺向下兼容。
