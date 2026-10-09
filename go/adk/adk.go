// Package adk runs akhook for agents built with ADK (Agent Development Kit,
// https://adk.dev) in Go.
//
// ADK has no hook configuration: a host registers callbacks on its agent.
// Hooks are those callbacks. Each runs `akhook adk hook <action>`, with the
// event in Claude Code's hook format, so the same .akhook.yml rules apply to
// ADK agents as to Claude Code and Codex:
//
//   - before each tool call, pre_tool_use: a denied call is not run, and the
//     model gets the reason as the tool's error;
//   - when an invocation starts, prompt_submit with the user's message: the
//     context the rules add goes into the invocation's system instruction;
//   - when it ends, stop with the agent's last message.
//
// Usage:
//
//	hooks := adk.New(workspace)
//	config := llmagent.Config{Name: "agent", Model: model, Tools: tools}
//	hooks.Install(&config)
//	agent, err := llmagent.New(config)
package adk

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"log/slog"
	"os/exec"
	"strings"
	"sync"

	"google.golang.org/adk/v2/agent"
	"google.golang.org/adk/v2/agent/llmagent"
	"google.golang.org/adk/v2/model"
	"google.golang.org/adk/v2/tool"
	"google.golang.org/genai"
)

// Hooks are an agent's akhook callbacks.
type Hooks struct {
	// Command is the akhook binary; "akhook" (on PATH) when empty.
	Command string
	// Cwd is the agent's working directory, absolute: akhook finds the
	// project's .akhook.yml from there and runs rule commands there.
	Cwd string
	// Env is akhook's environment, such as AKHOOK_ADDITIONAL_CONFIG_PATH;
	// nil inherits this process's.
	Env []string

	mu sync.Mutex
	// By invocation: the context prompt_submit added, and the last message.
	contexts map[string]string
	last     map[string]string
}

// New is the hooks of an agent working in cwd.
func New(cwd string) *Hooks {
	return &Hooks{Cwd: cwd}
}

// Install adds the hooks' callbacks to config, after any it has.
func (h *Hooks) Install(config *llmagent.Config) {
	config.BeforeAgentCallbacks = append(config.BeforeAgentCallbacks, h.beforeAgent)
	config.BeforeModelCallbacks = append(config.BeforeModelCallbacks, h.beforeModel)
	config.AfterModelCallbacks = append(config.AfterModelCallbacks, h.afterModel)
	config.BeforeToolCallbacks = append(config.BeforeToolCallbacks, h.beforeTool)
	config.AfterAgentCallbacks = append(config.AfterAgentCallbacks, h.afterAgent)
}

type output struct {
	HookSpecificOutput struct {
		PermissionDecision       string `json:"permissionDecision"`
		PermissionDecisionReason string `json:"permissionDecisionReason"`
		AdditionalContext        string `json:"additionalContext"`
	} `json:"hookSpecificOutput"`
}

// run runs `akhook adk hook <action>` with event on stdin.
func (h *Hooks) run(ctx context.Context, action string, event map[string]any) (*output, error) {
	event["cwd"] = h.Cwd
	input, err := json.Marshal(event)
	if err != nil {
		return nil, err
	}
	command := h.Command
	if command == "" {
		command = "akhook"
	}
	cmd := exec.CommandContext(ctx, command, "adk", "hook", action)
	cmd.Dir = h.Cwd
	cmd.Env = h.Env
	cmd.Stdin = bytes.NewReader(input)
	var stdout, stderr bytes.Buffer
	cmd.Stdout, cmd.Stderr = &stdout, &stderr
	if err := cmd.Run(); err != nil {
		return nil, fmt.Errorf("akhook %s: %w: %s", action, err, strings.TrimSpace(stderr.String()))
	}
	if stderr.Len() > 0 {
		slog.WarnContext(ctx, "akhook", "action", action, "stderr", strings.TrimSpace(stderr.String()))
	}
	out := &output{}
	if len(bytes.TrimSpace(stdout.Bytes())) == 0 {
		return out, nil
	}
	if err := json.Unmarshal(stdout.Bytes(), out); err != nil {
		return nil, fmt.Errorf("akhook %s: invalid output: %w", action, err)
	}
	return out, nil
}

// beforeTool checks the call; a call akhook denies, or cannot check, is not
// run and its result is the reason.
func (h *Hooks) beforeTool(ctx agent.Context, t tool.Tool, args map[string]any) (map[string]any, error) {
	out, err := h.run(ctx, "pre_tool_use", map[string]any{
		"hook_event_name": "PreToolUse",
		"tool_name":       t.Name(),
		"tool_input":      args,
		"tool_use_id":     ctx.FunctionCallID(),
		"session_id":      ctx.SessionID(),
	})
	if err != nil {
		return map[string]any{"error": "akhook could not check this tool call: " + err.Error()}, nil
	}
	// akhook answers ask rules on ADK with one-time approvals, as denials.
	if d := out.HookSpecificOutput; d.PermissionDecision == "deny" {
		return map[string]any{"error": d.PermissionDecisionReason}, nil
	}
	return nil, nil
}

// beforeAgent runs prompt_submit with the message that started the
// invocation and keeps the context it adds. Lifecycle hooks never stop the
// agent: a failure is logged.
func (h *Hooks) beforeAgent(ctx agent.Context) (*genai.Content, error) {
	out, err := h.run(ctx, "prompt_submit", map[string]any{
		"hook_event_name": "UserPromptSubmit",
		"session_id":      ctx.SessionID(),
		"turn_id":         ctx.InvocationID(),
		"prompt":          text(ctx.UserContent()),
	})
	if err != nil {
		slog.WarnContext(ctx, "akhook prompt_submit failed", "error", err)
		return nil, nil
	}
	if extra := out.HookSpecificOutput.AdditionalContext; extra != "" {
		h.mu.Lock()
		if h.contexts == nil {
			h.contexts = map[string]string{}
		}
		h.contexts[ctx.InvocationID()] = extra
		h.mu.Unlock()
	}
	return nil, nil
}

// beforeModel adds the invocation's context to the system instruction of
// each of its model requests.
func (h *Hooks) beforeModel(ctx agent.Context, req *model.LLMRequest) (*model.LLMResponse, error) {
	h.mu.Lock()
	extra := h.contexts[ctx.InvocationID()]
	h.mu.Unlock()
	if extra == "" {
		return nil, nil
	}
	if req.Config == nil {
		req.Config = &genai.GenerateContentConfig{}
	}
	if req.Config.SystemInstruction == nil {
		req.Config.SystemInstruction = &genai.Content{Role: genai.RoleUser}
	}
	req.Config.SystemInstruction.Parts = append(req.Config.SystemInstruction.Parts, genai.NewPartFromText(extra))
	return nil, nil
}

// afterModel remembers the invocation's last message.
func (h *Hooks) afterModel(ctx agent.Context, resp *model.LLMResponse, _ error) (*model.LLMResponse, error) {
	if resp == nil || resp.Partial {
		return nil, nil
	}
	if message := text(resp.Content); message != "" {
		h.mu.Lock()
		if h.last == nil {
			h.last = map[string]string{}
		}
		h.last[ctx.InvocationID()] = message
		h.mu.Unlock()
	}
	return nil, nil
}

// afterAgent runs stop with the last message and forgets the invocation.
func (h *Hooks) afterAgent(ctx agent.Context) (*genai.Content, error) {
	h.mu.Lock()
	last := h.last[ctx.InvocationID()]
	delete(h.last, ctx.InvocationID())
	delete(h.contexts, ctx.InvocationID())
	h.mu.Unlock()
	if _, err := h.run(ctx, "stop", map[string]any{
		"hook_event_name":        "Stop",
		"session_id":             ctx.SessionID(),
		"turn_id":                ctx.InvocationID(),
		"last_assistant_message": last,
	}); err != nil {
		slog.WarnContext(ctx, "akhook stop failed", "error", err)
	}
	return nil, nil
}

// text is content's text, without thoughts.
func text(content *genai.Content) string {
	if content == nil {
		return ""
	}
	var parts []string
	for _, part := range content.Parts {
		if part.Text != "" && !part.Thought {
			parts = append(parts, part.Text)
		}
	}
	return strings.Join(parts, "")
}
