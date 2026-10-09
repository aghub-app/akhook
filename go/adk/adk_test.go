package adk

import (
	"context"
	"encoding/json"
	"fmt"
	"iter"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"sync"
	"testing"

	"google.golang.org/adk/v2/agent"
	"google.golang.org/adk/v2/agent/llmagent"
	"google.golang.org/adk/v2/model"
	"google.golang.org/adk/v2/runner"
	"google.golang.org/adk/v2/tool"
	"google.golang.org/adk/v2/tool/functiontool"
	"google.golang.org/genai"
)

// akhook is the binary under test: AKHOOK_BIN, or this repository's debug
// build, built when missing.
func akhook(t *testing.T) string {
	t.Helper()
	if bin := os.Getenv("AKHOOK_BIN"); bin != "" {
		return bin
	}
	root, err := filepath.Abs("../..")
	if err != nil {
		t.Fatal(err)
	}
	if _, err := exec.LookPath("cargo"); err != nil {
		t.Skip("set AKHOOK_BIN, or install cargo to build akhook")
	}
	build := exec.Command("cargo", "build", "--quiet")
	build.Dir = root
	if out, err := build.CombinedOutput(); err != nil {
		t.Fatalf("cargo build: %v\n%s", err, out)
	}
	return filepath.Join(root, "target", "debug", "akhook")
}

// scripted calls the tools it is given in order, then answers with what the
// last one returned; it records each request.
type scripted struct {
	mu       sync.Mutex
	calls    []*genai.Content
	requests []*model.LLMRequest
}

func (m *scripted) Name() string { return "scripted" }

func (m *scripted) GenerateContent(_ context.Context, req *model.LLMRequest, _ bool) iter.Seq2[*model.LLMResponse, error] {
	return func(yield func(*model.LLMResponse, error) bool) {
		m.mu.Lock()
		m.requests = append(m.requests, req)
		n := len(m.requests)
		m.mu.Unlock()
		if n <= len(m.calls) {
			yield(&model.LLMResponse{Content: m.calls[n-1]}, nil)
			return
		}
		last := req.Contents[len(req.Contents)-1].Parts[0].FunctionResponse
		yield(&model.LLMResponse{Content: genai.NewContentFromText(fmt.Sprint(last.Response), genai.RoleModel)}, nil)
	}
}

func system(req *model.LLMRequest) string {
	if req.Config == nil || req.Config.SystemInstruction == nil {
		return ""
	}
	return text(req.Config.SystemInstruction)
}

type shellIn struct {
	Command string `json:"command"`
}

func TestHooksOnAnADKAgent(t *testing.T) {
	bin := akhook(t)
	dir := t.TempDir()
	seen := filepath.Join(dir, "stop.json")
	config := fmt.Sprintf(`version: 1
shell_tools:
  run_shell: command
rules:
  - id: no-force-push
    on: shell_exec
    checks:
      - argv: [git, push, --force]
    message: No force pushes.
  - id: recall
    on: prompt_submit
    run:
      argv: [sh, -c, 'echo "{\"context\": \"The user prefers Rust.\"}"']
  - id: capture
    on: stop
    run:
      argv: [sh, -c, 'cat > %s']
`, seen)
	if err := os.WriteFile(filepath.Join(dir, ".akhook.yml"), []byte(config), 0o644); err != nil {
		t.Fatal(err)
	}
	ran := []string{}
	shell, err := functiontool.New(functiontool.Config{Name: "run_shell", Description: "Run a command."},
		func(_ agent.Context, in shellIn) (map[string]string, error) {
			ran = append(ran, in.Command)
			return map[string]string{"output": "ok"}, nil
		})
	if err != nil {
		t.Fatal(err)
	}
	llm := &scripted{calls: []*genai.Content{
		genai.NewContentFromFunctionCall("run_shell", map[string]any{"command": "git push --force"}, genai.RoleModel),
		genai.NewContentFromFunctionCall("run_shell", map[string]any{"command": "git status"}, genai.RoleModel),
	}}
	hooks := New(dir)
	hooks.Command = bin
	hooks.Env = append(os.Environ(), "HOME="+dir, "XDG_CONFIG_HOME="+filepath.Join(dir, "config"),
		"AKHOOK_STATE_DIR="+filepath.Join(dir, "state"))
	cfg := llmagent.Config{Name: "test", Model: llm, Instruction: "Be brief.", Tools: []tool.Tool{shell}}
	hooks.Install(&cfg)
	a, err := llmagent.New(cfg)
	if err != nil {
		t.Fatal(err)
	}
	r, err := runner.NewInMemory("test", a)
	if err != nil {
		t.Fatal(err)
	}
	for _, err := range r.Run(context.Background(), "u", "s", genai.NewContentFromText("push it", genai.RoleUser), agent.RunConfig{}) {
		if err != nil {
			t.Fatal(err)
		}
	}
	// The force push never ran; the model got the rule's reason.
	if strings.Join(ran, ",") != "git status" {
		t.Fatalf("ran %v", ran)
	}
	denied := llm.requests[1].Contents[len(llm.requests[1].Contents)-1].Parts[0].FunctionResponse.Response
	if !strings.Contains(fmt.Sprint(denied["error"]), "no-force-push") {
		t.Fatalf("denied %v", denied)
	}
	// prompt_submit's context reached every model request of the turn.
	for i, req := range llm.requests {
		if s := system(req); !strings.Contains(s, "Be brief.") || !strings.Contains(s, "The user prefers Rust.") {
			t.Fatalf("request %d system %q", i, s)
		}
	}
	// stop got the turn's last message.
	data, err := os.ReadFile(seen)
	if err != nil {
		t.Fatal(err)
	}
	var stop map[string]any
	if err := json.Unmarshal(data, &stop); err != nil {
		t.Fatal(err)
	}
	if stop["event"] != "stop" || !strings.Contains(fmt.Sprint(stop["last_assistant_message"]), "ok") {
		t.Fatalf("stop %v", stop)
	}
}

func TestToolCallsAkhookCannotCheckAreNotRun(t *testing.T) {
	hooks := New(t.TempDir())
	hooks.Command = filepath.Join(t.TempDir(), "missing-akhook")
	ran := false
	shell, err := functiontool.New(functiontool.Config{Name: "run_shell", Description: "Run."},
		func(_ agent.Context, in shellIn) (map[string]string, error) { ran = true; return nil, nil })
	if err != nil {
		t.Fatal(err)
	}
	llm := &scripted{calls: []*genai.Content{
		genai.NewContentFromFunctionCall("run_shell", map[string]any{"command": "ls"}, genai.RoleModel),
	}}
	cfg := llmagent.Config{Name: "test", Model: llm, Tools: []tool.Tool{shell}}
	hooks.Install(&cfg)
	a, err := llmagent.New(cfg)
	if err != nil {
		t.Fatal(err)
	}
	r, err := runner.NewInMemory("test", a)
	if err != nil {
		t.Fatal(err)
	}
	for _, err := range r.Run(context.Background(), "u", "s", genai.NewContentFromText("go", genai.RoleUser), agent.RunConfig{}) {
		if err != nil {
			t.Fatal(err)
		}
	}
	result := llm.requests[1].Contents[len(llm.requests[1].Contents)-1].Parts[0].FunctionResponse.Response
	if ran || !strings.Contains(fmt.Sprint(result["error"]), "could not check") {
		t.Fatalf("ran %v, result %v", ran, result)
	}
}
