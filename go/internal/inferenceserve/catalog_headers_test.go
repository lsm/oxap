package inferenceserve

import (
	"context"
	"io"
	"net/http"
	"net/http/httptest"
	"testing"

	"github.com/lsm/oxap/go/internal/provider"
	"github.com/lsm/oxap/go/providercatalog"
)

func TestCatalogHeadersReachDiscoveryAndInference(t *testing.T) {
	var methods []string
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		methods = append(methods, r.Method)
		if r.Header.Get("X-Literal") != "literal" || r.Header.Get("X-Env") != "synthetic-value" || r.UserAgent() != "synthetic-client" {
			t.Errorf("%s did not receive catalog headers", r.Method)
		}
		if r.Method == http.MethodGet {
			io.WriteString(w, `{"data":[{"id":"deepseek-v4-pro"}]}`)
			return
		}
		w.Header().Set("Content-Type", "text/event-stream")
		io.WriteString(w, chunkFrame(`{"role":"assistant","content":"hello"}`, "", ""))
		io.WriteString(w, chunkFrame(`{}`, "stop", ""))
		io.WriteString(w, "data: [DONE]\n\n")
	}))
	defer server.Close()
	catalog := heldParityCatalog(t)
	literal, agent := "literal", "synthetic-client"
	for i := range catalog.Providers {
		if catalog.Providers[i].ID == "deepseek" {
			catalog.Providers[i].Wires = []string{"openai-completions"}
			catalog.Providers[i].Headers = map[string]providercatalog.HeaderValue{
				"X-Literal": {Literal: &literal}, "X-Env": {Env: "EXAMPLE_HEADER"}, "User-Agent": {Literal: &agent},
			}
		}
	}
	key := "synthetic"
	stored := func(id string) *providercatalog.StoredCredential {
		if id == "deepseek" {
			return &providercatalog.StoredCredential{APIKey: &key}
		}
		return nil
	}
	env := []providercatalog.EnvironmentValue{{Name: "DEEPSEEK_BASE_URL", Value: server.URL}, {Name: "EXAMPLE_HEADER", Value: "synthetic-value"}}
	found := (provider.Discoverer{Catalog: catalog, Environment: env, Stored: stored}).Discover(context.Background(), provider.DiscoverForceFetch)
	runner := &Runner{Catalog: catalog, Environment: env, Stored: stored, Models: found.Models}
	out := runnerOutcome(t, runner, parityModelRef, `[{"role":"user","content":"hi"}]`)
	if len(out) == 0 || out[len(out)-1].Type != "inference.completed" {
		t.Fatalf("inference did not complete: %v", typesOf(out))
	}
	if len(methods) != 2 || methods[0] != "GET" || methods[1] != "POST" {
		t.Fatalf("requests = %v, want discovery and inference", methods)
	}
}
