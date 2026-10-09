# akhook
Cross-agent hook rule engine for Codex, Claude Code and ADK agents.

## 安装与使用

```sh
cargo install --path .
akhook init                     # 选择项目中使用的 agent
akhook init --global            # 或在用户设置中安装全局 hook
```

在 `.akhook.yml` 中添加规则；`presets: [omp]` 可启用随包提供的 omp 规则。在工具执行前（`PreToolUse`）检查文件变更、shell 命令和任意工具调用（`tool_call`）；用户提交 prompt 和 agent 结束一轮时运行生命周期规则（`prompt_submit`、`stop`），由命令给这一轮补充上下文。ADK agent 通过 `go/adk` 的回调接入。规则命中后可以拒绝（`deny`）、请用户确认（`ask`），或交给脚本决定（`decide`）；shell 规则可以按解析后的命令参数匹配（`argv`）。平台可以用 `--add-config-path` 或 `AKHOOK_ADDITIONAL_CONFIG_PATH` 下发项目配置关不掉的规则。可从 [regex、shell 和 AST 示例](examples/README.md)开始；完整配置格式与代码架构见[设计文档](docs/design.md)。
