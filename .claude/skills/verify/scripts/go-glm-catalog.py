#!/usr/bin/env python3
"""Drive GLM discovery and inference in Go against an isolated local server."""
import os
from pathlib import Path
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parents[4]
GO = r'''
package main

import (
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
        methods = append(methods, r.Method + " " + r.URL.Path)
        valid = valid && r.Header.Get("Authorization") == "Bearer " + os.Getenv("ZHIPU_API_KEY")
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
    env := []providercatalog.EnvironmentValue{{Name: "OXAP_BASE_URL", Value: server.URL + "/v1"}, {Name: "ZHIPU_API_KEY", Value: os.Getenv("ZHIPU_API_KEY")}}
    discover := provider.Discoverer{Catalog: catalog, Environment: env, Stored: nil}
    found := discover.Discover(context.Background(), provider.DiscoverForceFetch)
    runner := &inferenceserve.Runner{Catalog: catalog, Environment: env, Stored: nil, Models: found.Models}
    var request inferenceserve.CreateRequest
    if err := json.Unmarshal([]byte(`{"model_ref":"zai-coding-plan/openai-chat-completions@fixture-model","messages":[{"role":"user","content":"hi"}]}`), &request); err != nil { panic(err) }
    var events []inferenceserve.Envelope
    run := func() { runner.Create(context.Background(), "request", request, func(event inferenceserve.Envelope) { events = append(events, event) }) }
    run()
    if !valid || len(methods) != 2 || methods[0] != "GET /v1/models" || methods[1] != "POST /v1/chat/completions" || len(events) == 0 || events[len(events)-1].Type != "inference.completed" { fmt.Fprintln(os.Stderr, "discovery or inference failed"); os.Exit(1) }
    fmt.Printf("PASS: %d authenticated discovery and inference requests completed\n", len(methods))
}
'''


def main():
    output = Path(sys.argv[1]).resolve()
    output.mkdir(parents=True, exist_ok=False)
    with tempfile.TemporaryDirectory(prefix='verify-go-catalog-', dir=ROOT / 'go') as directory, tempfile.TemporaryDirectory(prefix='verify-go-home-') as home:
        source = Path(directory) / 'main.go'
        source.write_text(GO)
        env = {'HOME': home, 'TMPDIR': home, 'NODE_ENV': 'test', 'ZHIPU_API_KEY': 'synthetic-glm-key', 'PATH': os.environ['PATH']}
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
