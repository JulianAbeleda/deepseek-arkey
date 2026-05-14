# DeepSeek Arkey

`deepseek-arkey` 是一个用 Rust 编写的独立终端 CLI，用来在本地终端中使用 DeepSeek。

![DeepSeek 启动演示](../assets/deepseek_boot_demo.gif)

## 这是什么

DeepSeek 是一个 AI 模型提供商。本项目是一个独立的终端客户端，用来与 DeepSeek 模型对话。

## 安装

通过 Homebrew 安装：

```bash
brew install JulianAbeleda/tap/deepseek-arkey
export DEEPSEEK_API_KEY="your_deepseek_api_key"
deepseek-arkey login
deepseek-arkey
```

从源码安装：

```bash
cargo build --release
cp target/release/deepseek-arkey ~/.local/bin/deepseek-arkey
cp target/release/deepseek ~/.local/bin/deepseek
export DEEPSEEK_API_KEY="your_deepseek_api_key"
deepseek-arkey login
```

源码安装时，请确保 `~/.local/bin` 已经加入 `PATH`。`deepseek` 二进制文件保留为兼容别名。

如果使用 zsh，可以这样持久保存 API key：

```bash
echo 'export DEEPSEEK_API_KEY="your_deepseek_api_key"' >> ~/.zsh_secrets
source ~/.zshrc
deepseek-arkey login
```

## 网络搜索

网络搜索通过 provider key 显式启用：

- 搜索提供商：`DEEPSEEK_SEARCH_PROVIDER=brave|tavily`，默认是 `brave`
- Brave key：`BRAVE_SEARCH_API_KEY`，也接受 `BRAVE_API_KEY` 作为别名
- Tavily key：`TAVILY_API_KEY`
- 运行时切换：`/features toggle` 会在 `brave` 和 `tavily` 之间持久切换搜索提供商，但不会保存密钥

普通聊天会为 URL 和实时信息类提示预取网页上下文；如果网页上下文不可用，会显示警告但继续回答。Agent 模式提供两个只读网页工具：`web_search` 和 `fetch_url`。如果缺少所选搜索提供商的 key，或者抓取失败，显式网页工具调用会返回错误。

## 为什么存在

我开始这个项目，是因为对当前 DeepSeek CLI 生态感到不满意。

DeepSeek 自己的 CLI 体验偏向网页端，没有一个专门的终端 UI，能达到 Codex、Kimi 或 Claude Code 那类工具的体验水平。我试过的一些非官方 DeepSeek TUI 也没有达到这个标准。在我看来，很多实现过度依赖现成库和依赖式架构，而不是直接解决终端交互里真正困难的问题。

在 Rust 中做这个项目，常见的捷径是使用 Ratatui。Ratatui 是一个不错的库，但使用它也意味着把一部分控制权交给它的抽象。对这个项目来说，控制很重要。我想自己理解并掌握终端行为。

我尊重的 TUI 通常具备这些高级体验：

- 自然的 scrollback
- 组合式底部 dock
- inline CLI 行为
- 可预测的键盘处理
- 清晰的会话流程

这些就是本项目追求的标准。

## 理念

AI 时代的很多仓库让我感到担忧的一点，是维护纪律。好的项目很容易被大量提交、巨大的脚本和迟到的重构压垮。

本仓库有意围绕以下编码原则构建：

- centralization
- modularization
- orthogonality
- 只在真正减少复杂度时才做 abstraction

目标是让代码库保持精简、可理解、可维护。功能必须证明自己值得存在。复杂度应该被移除，而不是被默认接受。

关于本项目编码标准的更多说明，请参见[编码原则](./coding-principles.zh-CN.md)。

## 项目边界

本仓库的目的，是提供一个聚焦、可控的 DeepSeek 终端体验，面向那些希望审阅并直接指导 AI 行为的人。

欢迎 fork。本项目不会主动加入面向全自动 coding-agent 框架的功能，例如 OpenClaw 或 Hermes。我的观点是：AI 应该帮助人学习、思考并参与工作，而不是把人从过程中移除。

如果你想要一个让自己始终在回路中、并能审阅 AI 行动的 DeepSeek TUI，这个项目就是为你准备的。

## 开发文档

- [编码原则](./coding-principles.zh-CN.md)
- [Commit 规范](./commit-discipline.md)
- [Phase 11 路由审计](./phase11-routing-audit.md)
- [Phase 12 dock 审批范围](./phase12-dock-approval-scope.md)

## 许可证

MIT。详见 [LICENSE](../../LICENSE)。
