package main

import (
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func TestHubStdioDeepSeekWorkStatusLink(t *testing.T) {
	for _, backend := range []string{"go", "zig"} {
		t.Run(backend, func(t *testing.T) {
			binary := os.Getenv("OAP_OAPX_BIN")
			if backend == "go" {
				binary = oapBinary(t)
			} else if binary == "" {
				t.Skip("set OAP_OAPX_BIN to a Zig oxap binary")
			}
			work := t.TempDir()
			fixture := filepath.Join("testdata", "parity", "deepseek-switch")
			script, err := os.ReadFile(filepath.Join(fixture, "child.sh"))
			if err != nil {
				t.Fatal(err)
			}
			if err := os.WriteFile(filepath.Join(work, "child.sh"), script, 0o755); err != nil {
				t.Fatal(err)
			}
			registry, err := os.ReadFile(filepath.Join(fixture, "registry.json"))
			if err != nil {
				t.Fatal(err)
			}
			config := filepath.Join(work, "registry.json")
			if err := os.WriteFile(config, []byte(strings.ReplaceAll(string(registry), "@DIR@", work)), 0o600); err != nil {
				t.Fatal(err)
			}
			c := spawnBinary(t, binary, "--config", config, "--session-history=")
			c.send(`{"id":1,"op":"open","adapter":"deepseek","request":{` + openEnvelopeFields + `"id":"o1","payload":{"session_id":"s1"}}}`)
			c.response(1)
			c.send(`{"id":2,"op":"open","adapter":"deepseek","request":{` + openEnvelopeFields + `"id":"o2","payload":{"session_id":"s2"}}}`)
			c.response(2)
			links := map[string]string{}
			for i, session := range []string{"s1", "s2", "s1"} {
				id := int64(i + 3)
				c.send(`{"id":%d,"op":"work.status","session_id":%q}`, id, session)
				result := c.response(id).Result
				var status struct {
					Link string `json:"link"`
				}
				if err := json.Unmarshal(result, &status); err != nil {
					t.Fatal(err)
				}
				native, found := strings.CutPrefix(status.Link, "deepseek://sessions/")
				if !found || native == "" || native == session {
					t.Errorf("work.status link = %q, want a native session of its own for %s (result %s)", status.Link, session, result)
				}
				if earlier, seen := links[session]; seen && earlier != status.Link {
					t.Errorf("work.status link for %s moved from %q to %q", session, earlier, status.Link)
				}
				links[session] = status.Link
			}
			if links["s1"] == links["s2"] {
				t.Errorf("s1 and s2 share the native session %q", links["s1"])
			}
		})
	}
}
