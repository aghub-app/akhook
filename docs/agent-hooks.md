# Claude Code 与 Codex Hooks

本文记录 Claude Code 与 OpenAI Codex 的 hook 生命周期、工具调用拦截、流式输出能力，以及跨端规则引擎需要适配的差异。这里的“流式 hook”特指：模型生成中的文本或工具参数以增量片段进入规则引擎，并允许引擎中断本次生成、向模型注入规则后重试。它不等同于读取完整工具参数后、工具执行前进行匹配。

## 能力速览

| 能力 | Claude Code Hooks | Codex Hooks |
|---|---|---|
| 工具执行前检查 | 支持 `PreToolUse`；可允许、拒绝、要求确认/延后（按事件支持范围），并可修改部分工具输入。 | 支持 `PreToolUse`；可拒绝工具调用、向模型提供上下文，或通过 `updatedInput` 改写受支持的工具输入。 |
| 工具执行后反馈 | `PostToolUse`、`PostToolUseFailure` 等事件；可提供上下文或处理工具结果。副作用已经发生。 | `PostToolUse` 可提供反馈、替换模型可见结果或让模型继续；不能撤销已经发生的副作用。 |
| Bash 命令匹配 | `PreToolUse` 接收工具名和 `tool_input`，可按工具匹配并检查命令。 | `PreToolUse` 可匹配 `Bash` 等工具，检查命令字段；官方 hook 覆盖的工具路径有边界。 |
| Edit/Write 匹配 | `PreToolUse` 可对 `Edit`、`Write` 等工具设置 matcher，并检查相应工具输入。 | 文件编辑通过 `apply_patch` 路径进入 hook；matcher 可使用 `apply_patch`、`Edit`、`Write` 等别名，但实际输入是 `tool_name: apply_patch`，编辑内容通常在 patch 字符串中，需自行解析。 |
| 模型文本流事件 | `MessageDisplay` 在 assistant 文本流式显示时触发，能变换显示内容；属于显示层，不阻止模型生成或工具执行。 | 官方 hooks 是会话/轮次/工具生命周期事件；没有公开的 assistant token/delta hook。 |
| 中断模型当前生成并原地重试 | Hook 不支持。 | Hook 不支持。 |

## Claude Code Hooks

### 生命周期与工具事件

Claude Code 按生命周期事件调用 hook。常见事件包括：

- `SessionStart`、`SessionEnd`：会话开始、结束。
- `UserPromptSubmit`：用户提交 prompt。
- `PreToolUse`：工具执行前。
- `PostToolUse`、`PostToolUseFailure`：工具执行成功或失败后。
- `Stop`、`SubagentStop`：主 agent 或 subagent 准备结束。
- `PreCompact`、`PostCompact`：上下文压缩前后。
- `MessageDisplay`：assistant 文本流式显示期间的展示事件。

Hook handler 可以是 command、HTTP endpoint、MCP tool、prompt 或 agent；各事件支持的 handler 类型有所不同。事件输入以 JSON 提供；command hook 从 stdin 读取，HTTP hook 从 POST body 读取。[1]

### PreToolUse：执行前检查

`PreToolUse` 可以根据工具名称及工具参数实施策略，例如只检查 Bash 命令，或检查 Edit/Write 的文件路径和内容。对可修改的工具调用，可由 hook 返回 `updatedInput`；拒绝时可将拒绝原因反馈给 Claude。工具事件支持通过 matcher 选择工具，还可以按工具参数继续筛选。[1]

示意输入结构：

```json
{
  "hook_event_name": "PreToolUse",
  "tool_name": "Bash",
  "tool_input": {
    "command": "npm test"
  },
  "tool_use_id": "..."
}
```

此时收到的是待执行的工具调用数据，不是模型逐 token 生成该命令时的部分内容。匹配后拒绝工具调用，模型收到原因后可生成后续响应或新的调用；原始生成过程不会回滚到命中点继续重写。[1]

### PostToolUse：执行后处理

`PostToolUse` 在工具执行后触发。可以把校验发现作为额外上下文交给模型，或按支持的输出字段替换模型接收的工具结果。由于事件发生在执行之后，不能撤销文件写入、命令副作用或外部请求。[1]

### MessageDisplay：流式显示但非生成控制

`MessageDisplay` 输入包含 `delta`、`index`、`final` 等字段，随 assistant 文本流式显示触发。它支持通过 `displayContent` 变换当前显示片段。因此它适用于展示格式转换等场景，但不是可阻止模型继续生成、向模型注入规则并重新生成的控制 hook；屏幕显示改写不等于修改模型实际生成内容。[1]

Claude Agent SDK 另有 `include_partial_messages` / `includePartialMessages` 选项，可让应用接收原始 API stream events。那是 SDK 的流式接入接口，不等同于 Claude Code hooks；若要基于它实现中断和重新调度，需要应用本身控制 Agent/API 循环。[3]

## Codex Hooks

### 生命周期与工具事件

Codex hooks 可在会话、轮次、工具和压缩等阶段运行。当前官方事件包括：

- `SessionStart`、`SessionEnd`。
- `UserPromptSubmit`。
- `PreToolUse`、`PermissionRequest`、`PostToolUse`。
- `PreCompact`、`PostCompact`。
- `SubagentStart`、`SubagentStop`。
- `Stop`。

Hook 由事件、matcher group 和 handler 构成。Command hook 通过 stdin 接收 JSON；hook 结果按事件定义提供拒绝、上下文或继续控制。[2]

### PreToolUse：拒绝、上下文与参数改写

`PreToolUse` 可拦截受支持的工具路径，在工具执行前拒绝调用，也可提供额外上下文。对受支持的工具，返回 `permissionDecision: "allow"` 与 `updatedInput` 可改写待执行参数。`Bash`、`apply_patch`、MCP 以及部分本地 function tools 使用此路径；hosted tools（例如 WebSearch）不经过该本地 hook 路径。[2]

工具 matcher 针对工具名及别名。例如文件编辑实际报告 `tool_name: "apply_patch"`，尽管可用 `Edit` 或 `Write` 别名匹配。输入中 patch 位于命令/参数文本中，因此按文件路径限定或仅检查新增行，需要解析 patch 格式；不能假设拿到与 Claude `Edit`/`Write` 相同的结构化字段。[2]

### PostToolUse 与 Stop

`PostToolUse` 在受支持的工具产生结果后运行。它可向模型提供反馈，或替换模型可见的工具结果，但不能撤销已经执行的操作。[2]

`Stop` 在主 agent 准备结束当前轮次时触发。返回 block 并非拒绝整个轮次，而是让 Codex 根据 hook 原因创建 continuation prompt 并继续。因此它可用来要求模型补充工作，但不能替代工具执行前拦截，也不是当前生成中的流中断。[2]

### 覆盖范围

Codex 官方文档区分本地 function-tool hook 路径与 hosted tools。当前文档列举的本地覆盖包括 shell 命令、`apply_patch`、MCP 和其他本地 function tools，同时指出具体工具路径及例外需查看工具覆盖说明。跨端 adapter 不应把“Codex 支持 PreToolUse”理解为“所有工具都必定发出 hook”。[2]

## “细粒度”与“流式”的区别

工具类型可以很细：`Bash`、`Edit`、`Write`、MCP 工具等都可以有独立 matcher 或规则范围。单次工具调用中也可以检查多个字段，例如命令文本、文件路径、patch 内容。此类规则在工具调用已经完整形成后、工具实际运行前评估。

真正的生成流式控制需要另一种接口：规则引擎持续接收文本或工具参数增量，并在某个增量命中时取消当前模型请求、保留命中规则上下文，再发起重试。Claude Code `MessageDisplay` 只控制展示，Codex hooks 没有 assistant delta 事件；两者的 hook API 都不提供这一完整能力。[1][2]

| 匹配方式 | 触发时机 | 可以阻止工具副作用 | 可以撤回已生成文本 | 两端 hooks 支持 |
|---|---|---:|---:|---|
| 逐 token / delta 匹配 | 工具参数或文本仍在生成 | 可在模型调用层取消 | 可在自有 harness 中丢弃未完成响应 | 否（Claude 仅有展示层 delta） |
| PreToolUse 完整参数匹配 | 工具调用已生成、执行尚未开始 | 是，拒绝该调用 | 否 | 是，按 host/tool 能力适配 |
| PostToolUse 结果检查 | 工具执行完成之后 | 否 | 否 | 是，反馈能力因事件而异 |
| Stop/轮次结束检查 | agent 准备结束当前轮次 | 不能保护此前已执行的工具 | 否；可让模型继续 | 是，语义因 host 而异 |

## 跨端规则引擎的适配边界

同一套规则可以共用规则描述、匹配逻辑和状态管理；平台 adapter 负责将各 host 的生命周期事件、工具名、输入结构和返回语义映射到共同格式。例如，统一事件可包含：

```text
phase: before_tool | after_tool | turn_stop | display_delta
tool: bash | edit | write | mcp:<name> | host-specific
call_id: host tool-call id
input: normalized arguments or raw host payload
capabilities: deny | inject_context | rewrite_input | observe_delta | interrupt_generation
```

其中 `capabilities` 应由每个 adapter 按实际 host 能力声明。`observe_delta` 与 `interrupt_generation` 要分开：Claude 的 `MessageDisplay` 可声明展示 delta 能力，但不能声明模型生成中断能力。Codex 当前 hook adapter 不声明 assistant delta 能力。规则只要求两端都具备的能力时，才能保证行为语义一致；其他规则需要标注平台专属行为或不支持状态。

### 工具输入适配

- **Bash**：将命令字符串统一为 `command` 字段；在 `before_tool` 阶段评估并拒绝或改写。
- **Claude Edit/Write**：从对应工具的结构化参数提取路径与待写入内容。
- **Codex Edit/Write**：处理 `apply_patch` 输入，解析文件路径和 patch 变更行，再转换为统一的路径与新增内容视图。
- **PostToolUse**：统一表示为结果反馈，不应映射成“执行前拒绝”。

平台 adapter 应保留原始 payload 和 host call id，便于诊断匹配差异、避免不同工具协议的解析损失。

## 参考资料

1. [Claude Code Hooks reference](https://code.claude.com/docs/en/hooks)
2. [Codex Hooks](https://developers.openai.com/codex/hooks)
3. [Claude Agent SDK: Stream responses in real time](https://code.claude.com/docs/en/agent-sdk/streaming-output)
