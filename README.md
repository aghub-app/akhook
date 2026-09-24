# akhook
Cross-agent hook rule engine for Codex and Claude Code.

## 安装与使用

```sh
cargo install --path .
akhook init                     # 选择项目中使用的 agent
akhook init --global            # 或在用户设置中安装全局 hook
```

在 `.akhook.yml` 中添加规则；`presets: [omp]` 可启用随包提供的 omp 规则。首版使用 `PreToolUse` 在工具执行前检查文件变更和 shell 命令。可从 [regex、shell 和 AST 示例](examples/README.md)开始；完整配置格式与代码架构见[设计文档](docs/design.md)。
