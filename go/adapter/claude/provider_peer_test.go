package claude_test

import (
	"context"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"testing"
	"time"

	base "github.com/lsm/oxap/go/adapter"
	"github.com/lsm/oxap/go/adapter/claude"
	"github.com/lsm/oxap/go/protocol"
)

func TestConfiguredProviderReachesTheCLIPeer(t *testing.T) {
	if runtime.GOOS == "windows" {
		t.Skip("the peer uses a Unix shebang")
	}
	if _, err := exec.LookPath("python3"); err != nil {
		t.Skip("python3 is required for the shared launch peer")
	}
	repo, err := filepath.Abs("../../..")
	if err != nil {
		t.Fatal(err)
	}
	peer, err := os.ReadFile(filepath.Join(repo, "rust/adapter/claude/tests/session_open_peer.py"))
	if err != nil {
		t.Fatal(err)
	}
	dir := t.TempDir()
	executable := filepath.Join(dir, "peer")
	if err := os.WriteFile(executable, peer, 0700); err != nil {
		t.Fatal(err)
	}
	adapter, err := claude.New(claude.Config{
		Executable: executable, WorkingDirectory: dir, Tools: claude.UnrestrictedTools(),
		Environment: []string{
			"HOME=" + dir, "PATH=" + os.Getenv("PATH"), "NODE_ENV=test", "MODE=provider",
			"FIXTURE=" + filepath.Join(repo, claude.CorpusDirectory, "initialize-lifecycle/native.jsonl"),
			"ANTHROPIC_BASE_URL=https://old.example", "ANTHROPIC_CUSTOM_HEADERS=X-Host: first",
		},
		Provider: &claude.Provider{BaseURL: "http://127.0.0.1:1/gateway", Auth: claude.AuthNone},
		Headers:  []claude.Header{{Name: "X-Trace", Value: "two words"}, {Name: "x-trace", Value: "third"}},
	})
	if err != nil {
		t.Fatal(err)
	}
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	session, err := adapter.Open(ctx, base.OpenRequest{SessionID: "session", Participant: protocol.Participant{ID: "user"}})
	if err != nil {
		t.Fatal(err)
	}
	if err := session.Close(ctx); err != nil {
		t.Fatal(err)
	}
	if _, err := os.Stat(filepath.Join(dir, "eof")); err != nil {
		t.Fatal(err)
	}
	t.Log("PASS Go provider launch: route, ordered headers, initialize, and EOF")
}
