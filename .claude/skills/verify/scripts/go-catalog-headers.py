#!/usr/bin/env python3
"""Drive Go provider discovery and inference against an isolated local server."""
import os
from pathlib import Path
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parents[4]
GO = r'''
package main

import (
    "bytes"
    "context"
    "encoding/json"
    "fmt"
    "io"
    "net/http"
    "net/http/httptest"
    "os"

    "github.com/lsm/oxap/go/internal/inferenceserve"
    "github.com/lsm/oxap/go/internal/provider"
    "github.com/lsm/oxap/go/providercatalog"
    "github.com/lsm/oxap/providers"
)

func main() {
    methods := []string{}
    valid := true
    server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
        methods = append(methods, r.Method)
        valid = valid && r.Header.Get("X-Literal") == "literal" && r.Header.Get("X-Env") == "synthetic-value" && r.UserAgent() == "synthetic-client"
        if r.Method == "GET" {
            io.WriteString(w, `{"data":[{"id":"fixture-model"}]}`)
            return
        }
        w.Header().Set("content-type", "text/event-stream")
        io.WriteString(w, "data: {\"id\":\"x\",\"object\":\"chat.completion.chunk\",\"model\":\"fixture-model\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"hello\"},\"finish_reason\":null}]}\n\n")
        io.WriteString(w, "data: {\"id\":\"x\",\"object\":\"chat.completion.chunk\",\"model\":\"fixture-model\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n")
    }))
    defer server.Close()
    catalog, err := providercatalog.Load(providers.Files)
    if err != nil { panic(err) }
    literal, agent := "literal", "synthetic-client"
    for i := range catalog.Providers {
        if catalog.Providers[i].ID == "deepseek" {
            catalog.Providers[i].Wires = []string{"openai-completions"}
            catalog.Providers[i].Headers = map[string]providercatalog.HeaderValue{"X-Literal": {Literal: &literal}, "X-Env": {Env: "EXAMPLE_HEADER"}, "User-Agent": {Literal: &agent}}
        }
    }
    key := "synthetic"
    stored := func(id string) *providercatalog.StoredCredential {
        if id == "deepseek" { return &providercatalog.StoredCredential{APIKey: &key} }
        return nil
    }
    env := []providercatalog.EnvironmentValue{{Name: "DEEPSEEK_BASE_URL", Value: server.URL}, {Name: "EXAMPLE_HEADER", Value: "synthetic-value"}}
    discover := provider.Discoverer{Catalog: catalog, Environment: env, Stored: stored}
    found := discover.Discover(context.Background(), provider.DiscoverForceFetch)
    runner := &inferenceserve.Runner{Catalog: catalog, Environment: env, Stored: stored, Models: found.Models}
    var request inferenceserve.CreateRequest
    if err := json.Unmarshal([]byte(`{"model_ref":"deepseek/openai-chat-completions@fixture-model","messages":[{"role":"user","content":"hi"}]}`), &request); err != nil { panic(err) }
    var events []inferenceserve.Envelope
    run := func() { runner.Create(context.Background(), "request", request, func(event inferenceserve.Envelope) { events = append(events, event) }) }
    run()
    if !valid || len(methods) != 2 || methods[0] != "GET" || methods[1] != "POST" || len(events) == 0 || events[len(events)-1].Type != "inference.completed" { fmt.Fprintln(os.Stderr, "discovery or inference failed"); os.Exit(1) }
    encoded, err := json.Marshal(events)
    if err != nil { panic(err) }
    if bytes.Contains(encoded, []byte("synthetic-value")) { fmt.Fprintln(os.Stderr, "header leaked into inference events"); os.Exit(1) }
    discover.Environment = env[:1]
    missing := discover.Discover(context.Background(), provider.DiscoverForceFetch)
    runner.Environment = env[:1]
    events = nil
    run()
    if len(missing.Models) != 0 || len(methods) != 2 || len(events) != 2 || events[1].Type != "inference.failed" { fmt.Fprintln(os.Stderr, "missing environment was not refused"); os.Exit(1) }
    fmt.Printf("PASS: %d requests carried headers; missing environment sent no request; inference settled\n", len(methods))
}
'''


def main():
    output = Path(sys.argv[1]).resolve()
    output.mkdir(parents=True, exist_ok=False)
    with tempfile.TemporaryDirectory(prefix='verify-go-catalog-', dir=ROOT / 'go') as directory, tempfile.TemporaryDirectory(prefix='verify-go-home-') as home:
        source = Path(directory) / 'main.go'
        source.write_text(GO)
        env = {'HOME': home, 'TMPDIR': home, 'NODE_ENV': 'test', 'PATH': os.environ['PATH']}
        for name in ('GOMODCACHE', 'GOCACHE'):
            if name in os.environ:
                env[name] = os.environ[name]
        result = subprocess.run(['go', 'run', str(source)], cwd=ROOT, env=env, text=True, capture_output=True, timeout=120)
        (output / 'result.txt').write_text(result.stdout + result.stderr + f'\nexit={result.returncode}\n')
        sys.stdout.write(result.stdout + result.stderr)
        if result.returncode:
            raise SystemExit(result.returncode)


if __name__ == '__main__':
    main()
