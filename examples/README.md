# 规则示例

每个 YAML 文件都是一份独立的项目配置。选一个复制到项目根目录的 `.akhook.yml`，再运行 `akhook init --agent claude` 或 `akhook init --agent codex` 安装 hook。也可以把其中的 `rules` 合并到已有配置。

- [regex.yml](regex.yml)：对新写入的 Rust 文本做正则匹配。
- [shell.yml](shell.yml)：对 Bash 工具提交的命令文本做正则匹配。
- [ast.yml](ast.yml)：用 ast-grep 的 Rust 语法模式匹配新写入的代码。

文件规则检查本次新增或写入的文本；shell 规则检查命令字符串，不分析命令执行后的文件变化。
