# akhook 架构与接口

akhook 是一个 Rust CLI：项目在根目录用 `.akhook.yml` 定义规则，由 Claude Code、Codex 等 agent 的 hook 调用同一个规则引擎。首版在工具执行前检查完整调用；命中时拒绝调用，并把规则提示交给 agent。原始 TTSR 的生成中断与重试需要控制模型循环，现有 agent hooks 无法提供；见 [TTSR 参考](ttsr.md) 和 [hook 能力](agent-hooks.md)。

## CLI 与配置位置

```text
akhook init                         # 创建项目配置，交互选择要安装 hook 的 agent
akhook init --global                # 创建用户配置，交互选择要全局安装 hook 的 agent
akhook <agent> hook pre_tool_use    # 固定入口：从 stdin 读事件 JSON，向 stdout 写 agent 决策 JSON
akhook approval show <id>           # 查看一条待确认请求（JSON）
akhook approval grant <id>          # 用户同意后放行该调用一次
```

所有命令都接受可重复的 `--add-config-path <PATH>`，也读取环境变量 `AKHOOK_ADDITIONAL_CONFIG_PATH`（按系统路径列表分隔）；见下文“附加配置”。

首版 `agent` 为 `claude` 或 `codex`。`init` 要求 `akhook` 已在 `PATH` 上，因为登记的命令直接使用这个名字。它在 agent 的现有设置中合并 akhook 登记，重复运行不重复添加，也不改动其他 hook：

| agent | 项目登记 | 用户登记 | 固定命令 |
|---|---|---|---|
| Claude Code | `.claude/settings.json` | `~/.claude/settings.json` | `akhook claude hook pre_tool_use` |
| Codex | `.codex/hooks.json` | `~/.codex/hooks.json` | `akhook codex hook pre_tool_use` |

若选中的 agent 已全局登记，项目 `init` 只创建 `.akhook.yml`，避免同一次工具调用运行两遍。用户级规则配置位于操作系统标准配置目录下的 `akhook/akhook.yml`；CLI 显示实际路径。运行时从 hook 事件的 `cwd` 向上查找最近的 `.akhook.yml`，再与用户级配置合并。**全局 hook 登记**决定哪些项目调用 akhook；**全局规则**决定这些项目默认应用什么规则。

### 附加配置

附加配置来自 `AKHOOK_ADDITIONAL_CONFIG_PATH` 中的各路径，再加上各个 `--add-config-path`，按此顺序在用户级和项目配置**之后**合并。用户级和项目配置的 `disabled_rules` 与同 ID 规则都影响不到附加配置的规则；只有附加配置自己或更靠后的附加配置能关掉或覆盖它们。因此宿主（例如把 akhook 内置到 agent 镜像里的平台）可以用附加配置下发工作区里的 `.akhook.yml` 关不掉的规则。显式给出的附加配置不存在时视为配置错误。附加配置中 `command` 的相对程序路径仍按项目根目录解析，宜写绝对路径。

## `.akhook.yml`

```yaml
version: 1
presets: [omp]
disabled_rules: [omp/rs-box-leak]
rules:
  - id: no-box-leak
    on: file_change
    paths: ["**/*.rs"]
    actions: [create, modify]
    checks:
      - regex: 'Box::leak\('
      - ast:
          language: rust
          pattern: 'Box::leak($VALUE)'
    message: 请改用不泄漏内存的写法。

  - id: custom-lint
    on: file_change
    paths: ["**/*.rs"]
    checks:
      - command:
          argv: ["./hooks/lint"]
          timeout_ms: 5000
    message: 请修正检查器指出的问题。

  - id: no-force-delete
    on: shell_exec
    checks:
      - regex: 'rm\s+-rf\b'
    message: 请先确认删除范围。

  - id: gh-pr-create
    on: shell_exec
    checks:
      - argv: [gh, pr, create]
    message: 请改用 PR 工具。

  - id: gh-write
    on: shell_exec
    checks:
      - argv: [gh, [pr, issue], [merge, close, comment]]
    message: gh 只能用于读取。

  - id: push
    on: shell_exec
    action: ask
    checks:
      - argv: [git, push]
    message: 推送需要用户确认。
```

`on` 指规范化操作，不是 agent 的工具名。`paths` 和 `actions` 是文件规则的可选过滤器；`checks` 中任一条件命中即命中规则，多个规则命中则合并提示并拒绝整次工具调用。正则使用 Rust 正则语法。`ast` 使用内嵌 ast-grep 库，只检查文件变更新引入的内容；`language` 可省略并按路径推断。删除文件没有新增内容，因此正则和 AST 不对删除操作运行，外部检查器仍可检查其路径与动作。

`argv` 只用于 `shell_exec`。命令先用 tree-sitter-bash 拆成简单命令：列表、管道、子 shell、命令替换里的命令，以及 `sh`/`bash`/`zsh`/`dash`/`ksh -c` 的脚本参数都会展开；每个简单命令去掉前置的变量赋值和重定向，参数去掉引号与转义。`argv` 的第一项与程序名的 basename 比较，其余各项须按顺序出现在参数中，可以不相邻，因此 `gh -R a/b pr create` 也匹配 `[gh, pr, create]`。每一项可以是一个词，也可以是备选词列表。`echo gh pr create` 中的 `gh` 只是参数，不会匹配。

`omp` preset 默认关闭，由 `presets: [omp]` 显式启用。preset 规则 ID 使用 `omp/<原规则名>`；同 ID 的项目规则覆盖用户级规则，用户级规则覆盖 preset。`disabled_rules` 在合并后关闭对应 ID 的规则。首版将 omp 规则的命中统一映射为**执行前拒绝**，而不是沿用其软提醒时机。

`omp` preset 的来源固定为设计时上游[最新正式版 v18.2.11](https://github.com/can1357/oh-my-pi/releases/tag/v18.2.11)（提交 `e415159`）。该标签的 27 条内置规则及 MIT 许可已放在 [`presets/omp`](../presets/omp/README.md)；运行时解析原规则的条件、路径范围和正文，不以本机安装的 omp 版本或浮动的 `main` 分支为准。

### 动作

规则命中后的动作由 `action` 决定；不填时为 `deny`。

- `action: deny`：拒绝工具调用。
- `action: ask`：请用户确认。Claude Code 直接返回 `permissionDecision: "ask"`。Codex 会解析 `ask` 但仍然执行工具，所以 akhook 改用一次性授权：见下文“用户确认”。

同一次工具调用中，只要有一条规则的动作为 `deny` 就拒绝，提示只包含拒绝的规则；否则若有 `ask` 就请用户确认。

### 用户确认（Codex）

`ask` 命中且 agent 无法自己询问用户时，akhook 在状态目录（`AKHOOK_STATE_DIR`，默认为系统的用户状态目录下的 `akhook`）写入 `requests/<id>.json`，内容是规则、提示、`cwd` 与候选操作，然后拒绝调用，并在提示后附上说明。说明默认要求用户运行 `akhook approval grant <id>` 后重试同一调用；配置项 `ask_instruction`（后面的配置覆盖前面的）可以替换它，其中 `{request_id}` 会被替换为请求 ID。宿主可以把它换成“调用某个确认工具”，由工具询问用户后执行 `akhook approval grant <id>`。

请求 ID 由命中的规则 ID、`cwd` 和候选操作的哈希得到，因此只有完全相同的调用会对应同一个请求。`grant` 写入 `grants/<id>.json`，十分钟内有效；下一次相同调用删除它并放行，只放行一次。授权文件对 agent 自己可写，所以这是防止 agent 误操作的确认步骤，不是安全边界。

## Rust 规则接口

运行路径为 `main.rs`（CLI）→ `agents/claude.rs` 或 `agents/codex.rs`（各自实现 `Agent` trait，负责 hook JSON 与文件变更解析）→ `config.rs`/`preset.rs`（规则来源与合并）→ `rules.rs`（匹配和外部检查）→ 对应 agent adapter（决策编码）。`agents/mod.rs` 只定义 trait 和共享的协议辅助函数；规范化的操作和决策类型放在 `model.rs`。agent adapter 不读取规则细节，规则引擎不解析 agent 的原始 JSON。

```rust
struct ToolAttempt {
    cwd: PathBuf,
    call_id: Option<String>,
    candidates: Vec<Candidate>,
}

enum Candidate {
    FileChange {
        path: PathBuf,
        action: FileAction,
        added_text: Option<String>, // 删除时为 None；空文件创建时为 Some("")
    },
    ShellExec { command: String },
}

enum FileAction { Create, Modify, Delete }
enum CheckSpec {
    Regex { pattern: String },
    Ast { language: Option<Language>, pattern: String },
    Argv { argv: Vec<ArgvItem> },
    Command { argv: Vec<String>, timeout_ms: u64 },
}
enum RuleAction { Deny, Ask }
enum Decision { Allow, Deny(Vec<RuleHit>), Ask(Vec<RuleHit>) }

fn evaluate(rules: &RuleSet, attempt: &ToolAttempt) -> Result<Decision, RuleError>;
```

Claude adapter 从 Edit/Write 的结构化参数提取文件变更；Codex adapter 从 `apply_patch` 的命令文本解析每个文件变更；两端的 Bash/shell 工具生成 `ShellExec`。一个工具调用可产生多个候选操作，任一命中就拒绝整个调用；同一规则在多文件中命中只报告一次。原始工具名、调用 ID 和输入留在 adapter 侧供诊断，规则引擎不依赖 agent 协议。未来的小模型分类可增加 `CheckSpec` 条件类型并沿用相同的命中结果，首版不引入插件框架。

## 外部检查命令

`command.argv` 直接启动程序，不经过 shell 解释；相对程序路径和工作目录以项目根目录为准，无项目配置时以事件 `cwd` 为准。每个经过规则过滤的候选操作调用一次检查器，stdin 传入版本化 JSON：

```json
{"version":1,"rule_id":"custom-lint","candidate":{"type":"file_change","path":"src/lib.rs","action":"modify","added_text":"..."}}
```

检查器成功退出时，stdout 返回 `{"matched":false}` 或 `{"matched":true,"message":"可选的具体提示"}`；具体提示优先于规则的静态 `message`。超时、非零退出和无效输出是检查错误。项目 `.akhook.yml` 中声明的命令会直接以当前用户身份运行，不增加信任确认；这是项目配置的执行权限边界。

## 错误与覆盖范围

- 配置无效、已支持的工具输入无法解析、匹配规则所需的 AST 语言无法确定，或检查器出错时，adapter 返回带原因的拒绝结果；不能通过空输出或进程错误假装规则通过。未知工具不作决定。
- Shell 规则只检查命令文本；`cat > file` 或脚本运行后的实际文件变化不能从任意命令中可靠推断。
- `PreToolUse` 能阻止受支持工具的执行，不能撤回已生成文本，也不能覆盖未进入该 hook 路径的工具；完整能力边界见 [agent hooks](agent-hooks.md)。
