# Time-Traveling Stream Rules（TTSR）

TTSR 是一种按条件触发的流式规则机制：规则监听 agent 正在生成的内容；命中时可中断当前响应，把规则正文作为提醒注入上下文，再让模型重新生成。规则用于识别输出中的特定模式，例如工具调用中的禁用 API、命令片段或反复出现的文本表述。

## 工作流程

1. 从规则文件加载规则及其条件、输出范围和中断策略。
2. 在模型流式输出期间检查被监视的内容。匹配针对累计内容进行，因此可跨多个流式片段。
3. 规则命中后，根据中断模式选择立即打断，或在工具结果后提供提醒。
4. 对需要中断的命中，当前响应被中止；规则正文作为提醒提供给模型，模型据此重新生成。对 edit/write，检查的是即将写入的源代码内容。

TTSR 是“中断并重新生成”，不会直接改写匹配文本。已经完成执行的工具调用无法由后续命中撤销。

## 规则文件

项目规则放在 `.omp/rules/<rule>.md`；用户级规则放在 `~/.omp/agent/rules/<rule>.md`。同名项目规则会遮蔽用户级规则。

规则使用 Markdown 正文和 YAML frontmatter。示例：

```markdown
---
description: Avoid Box::leak in production Rust code
condition: 'Box::leak\('
scope: 'tool:edit(*.rs), tool:write(*.rs)'
interruptMode: always
---

不要在生产代码中使用 `Box::leak`。请改用适合当前所有权需求的替代方案，并重新规划修改。
```

### Frontmatter 字段

| 字段 | 类型 | 含义 |
|---|---|---|
| `description` | 字符串 | 规则摘要，用于规则检查界面显示。 |
| `condition` | 字符串或字符串列表 | JavaScript 正则表达式；匹配被监视输出的累计内容。 |
| `astCondition` | 字符串或字符串列表 | ast-grep 模式；用于 edit/write 源代码的结构化匹配。 |
| `scope` | 字符串或字符串列表 | 指定监视的输出面及可选工具、路径过滤器。可用范围包括文本、thinking，以及 `tool:<name>(<glob>)`。未设置时监视文本和工具输出，不包括 thinking。 |
| `globs` | 字符串或字符串列表 | 附加的候选文件路径条件；不替代 `scope`。 |
| `interruptMode` | 枚举 | 每条规则的中断策略：`always`、`prose-only`、`tool-only` 或 `never`。 |
| `alwaysApply` | 布尔值 | 一般规则元数据。存在有效 TTSR 条件时，规则仍按 TTSR 处理，不会因此变成常驻上下文。 |

规则至少要有一个可用的 `condition` 或 `astCondition`。旧字段名 `ttsr_trigger` 可兼容读取；新规则使用 `condition`。看起来像文件 glob 的旧式 `condition` 值（例如 `*.rs`）可被兼容解释为监视该 glob 下 edit/write 的 catch-all 条件。

## 输出范围与匹配

常见范围：

- `text`：助手文本。
- `thinking`：可见时的推理输出。
- `tool:<name>`：指定工具的参数流。
- `tool:edit(<glob>)`、`tool:write(<glob>)`：指定路径模式的编辑或写入。

正则针对累计输出匹配，可识别跨流片段的内容。edit/write 使用被引入的源代码内容进行匹配；其他工具使用流式工具参数。`globs` 是额外的文件路径门槛：文本流没有关联文件路径，因此文本范围规则不应依赖 `globs`。

## 命中与重复策略

默认 `interruptMode: always`：命中后立即停止当前响应，显示规则命中通知，并使用规则正文重新生成。`interruptMode: never` 对匹配工具调用提供软提醒：工具可继续执行，模型在下一步从工具结果旁的提醒中获知规则；对文本输出则让该轮完成，随后提供提醒。`prose-only` 和 `tool-only` 将中断限制在对应输出类别。

全局设置示例：

```yaml
ttsr:
  enabled: true
  contextMode: discard       # discard | keep
  interruptMode: always      # always | prose-only | tool-only | never
  repeatMode: once           # once | after-gap
  repeatGap: 10              # 已完成轮数
  builtinRules: true
  disabledRules: []
```

默认每条规则在每个会话中触发一次。`repeatMode: after-gap` 可在经过 `repeatGap` 个已完成轮次后允许再次触发。触发记录随会话保存；注入的提醒在上下文压缩后仍被记录。单条规则的 `interruptMode` 可覆盖全局设置。多个规则可同时命中；若其中任一规则要求中断，则响应中断。

## 查看、测试和扫描

```bash
# 列出启用规则、条件、范围与来源文件
omp ttsr list

# 在示例输入上测试规则匹配及未匹配项
omp ttsr test --verbose \
  --source tool --tool edit --path src/lib.rs \
  'let value = Box::leak(Box::new(input));'

# 检查已有文件中可能匹配规则的内容
omp ttsr scan --rule .omp/rules/no-box-leak.md src/

# 查看或修改启用配置
omp config get ttsr.enabled
omp config set ttsr.enabled true

# 配置规则重复策略
omp config set ttsr.repeatMode after-gap
omp config set ttsr.repeatGap 10

# 禁用指定规则
omp config set ttsr.disabledRules '["no-box-leak"]'
```

可通过 `omp ttsr test` 复现规则的 source、tool 和 candidate path 条件，再用 `omp ttsr scan` 检查已有文件。

## 来源

- [omp 文档：Time-Traveling Stream Rules](https://omp.sh/docs/ttsr)
