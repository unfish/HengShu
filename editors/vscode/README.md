# VS Code 衡枢语言支持

这是一个不包含 JavaScript 运行时代码的声明式 VS Code 扩展，提供 `.代码` 文件关联、括号/注释/折叠配置、当前公开关键字和标准库名称的 TextMate 语法高亮。

## 查看过程报告

扩展把 `过程报告.json` 和 `*.过程报告.json` 关联到 `schemas/notebook.schema.json`，在 VS Code 中打开这类文件即可获得字段说明、补全和格式校验。生成方式：

```text
衡枢 运行 主程序.代码 --观察 快照 --过程报告 过程报告.json
```

GUI 结果页的「保存 HTML」写出的 `过程报告.html` 可用 VS Code 内置浏览器或外部浏览器打开。

自动化声明位于 `衡枢.toml`，不是 `.代码` 语法，因此不加入 TextMate 关键字。扩展把项目清单关联到 `schemas/manifest.schema.json`，提供项目、权限、工具和自动化字段说明；TOML 校验和补全由宿主安装的 TOML 语言服务消费该 schema。扩展不负责安装排程或运行通知；这些操作继续使用 `衡枢 排程`、`衡枢 历史` 命令。

## 本地试用

在 VS Code 中打开本目录，按 `F5` 启动扩展开发主机，或在扩展面板中选择“从 VSIX 安装”。也可以把整个 `editors/vscode` 目录复制到个人扩展目录后重启 VS Code。

## 打包发布

衡枢编译器和运行时完全不依赖 Node.js。生成 VSIX 是编辑器发布环节的可选操作，可使用 VS Code 官方 `vsce` 工具：

本目录也提供了不依赖 Node.js 的 VSIX 打包脚本：

```text
cd editors/vscode
./build-vsix.sh ./衡枢语言支持.vsix
```

它只使用系统 `sh`、`cp` 和 `zip`，生成的文件可以直接在 VS Code 的“从 VSIX 安装”中使用。若要上传到 Marketplace，再按官方要求使用 `vsce`：

```text
cd editors/vscode
npx @vscode/vsce package
```

`npx` 只用于扩展发布，不是衡枢项目的构建或运行依赖。

语言服务由以下命令启动：

```text
衡枢 语言服务
```

编辑器客户端应使用标准输入输出与该进程进行 LSP 通信。
