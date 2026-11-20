package deepseek

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"sync"
	"testing"
	"time"

	base "github.com/lsm/oxap/go/adapter"
	"github.com/lsm/oxap/go/adapter/deepseek/internal/native"
	"github.com/lsm/oxap/go/adapter/deepseek/internal/rpc"
	"github.com/lsm/oxap/go/protocol"
)

type fakeBridge struct{ client *fakeClient }

func (b fakeBridge) ClientHandle() Client        { return b.client }
func (b fakeBridge) Done() <-chan struct{}       { return b.client.Done() }
func (b fakeBridge) WaitError() error            { return nil }
func (b fakeBridge) Close(context.Context) error { return b.client.Close() }

type startedRuntimes struct {
	mu      sync.Mutex
	configs []rpc.ProcessConfig
	clients []*fakeClient
	failure func(call int) error
}

func (r *startedRuntimes) factory() ProcessFactory {
	return ProcessFactoryFunc(func(_ context.Context, c rpc.ProcessConfig) (ProcessBridge, error) {
		r.mu.Lock()
		defer r.mu.Unlock()
		r.configs = append(r.configs, c)
		if r.failure != nil {
			if err := r.failure(len(r.configs)); err != nil {
				return nil, err
			}
		}
		client := newFake()
		r.clients = append(r.clients, client)
		return fakeBridge{client}, nil
	})
}

func (r *startedRuntimes) started() int {
	r.mu.Lock()
	defer r.mu.Unlock()
	return len(r.configs)
}

func openOnRuntimes(t *testing.T, runtimes *startedRuntimes, request base.OpenRequest) base.Session {
	t.Helper()
	implementation, err := New(Config{Executable: "/bin/true", WorkingDirectory: t.TempDir(), Provider: "p", Model: "m", ProcessFactory: runtimes.factory()})
	if err != nil {
		t.Fatal(err)
	}
	request.SessionID = "session"
	session, err := implementation.Open(context.Background(), request)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = session.Close(context.Background()) })
	return session
}

func switchTo(t *testing.T, session base.Session, model string) (protocol.SessionModelSwitchResponse, protocol.SessionState, error) {
	t.Helper()
	switcher, ok := session.(base.ModelSwitcher)
	if !ok {
		t.Fatal("the session does not switch its model")
	}
	return switcher.SwitchModel(context.Background(), protocol.SessionModelSwitchRequest{SessionID: "session", ModelID: model})
}

func TestASwitchBeforeTheFirstRunRestartsTheRuntimeOnTheNewModel(t *testing.T) {
	runtimes := &startedRuntimes{}
	session := openOnRuntimes(t, runtimes, base.OpenRequest{ReasoningLevel: protocol.ReasoningHigh})
	response, state, err := switchTo(t, session, "other")
	if err != nil {
		t.Fatal(err)
	}
	if response.ModelID != "other" || response.PreviousModelID != "m" || state.CurrentModelID != "other" {
		t.Fatalf("response %+v, state model %q, want m to other", response, state.CurrentModelID)
	}
	if runtimes.started() != 2 || runtimes.configs[0].Initialize.Model != "m" || runtimes.configs[1].Initialize.Model != "other" {
		t.Fatalf("started %d runtimes, models %q then %q", runtimes.started(), runtimes.configs[0].Initialize.Model, runtimes.configs[1].Initialize.Model)
	}
	if runtimes.configs[1].Initialize.ReasoningEffort != "high" {
		t.Fatalf("the new runtime lost the session's effort: %q", runtimes.configs[1].Initialize.ReasoningEffort)
	}
	select {
	case <-runtimes.clients[0].Done():
	case <-time.After(2 * time.Second):
		t.Fatal("the old runtime was not shut down")
	}
	current, err := session.State(context.Background())
	if err != nil || current.CurrentModelID != "other" || current.Status != protocol.SessionIdle {
		t.Fatalf("state %+v, err %v, want an idle session on other", current, err)
	}
	if _, _, err := switchTo(t, session, "other"); err != nil || runtimes.started() != 2 {
		t.Fatalf("switching to the current model answered %v and started %d runtimes, want a no-op", err, runtimes.started())
	}
}

func assertSwitchRefused(t *testing.T, session base.Session, runtimes *startedRuntimes, when string) {
	t.Helper()
	_, _, err := switchTo(t, session, "other")
	var refusal *base.UnsupportedControlError
	if !errors.As(err, &refusal) || refusal.Feature != protocol.FeatureSessionModelSwitch || refusal.Reason != base.ControlUnsatisfiable || refusal.Field != "model_id" {
		t.Fatalf("switch %s answered %v, want an unsatisfiable model_id refusal", when, err)
	}
	if runtimes.started() != 1 {
		t.Fatalf("the refusal %s started %d runtimes, want none", when, runtimes.started())
	}
}

func TestASwitchIsRefusedWhileARunIsActiveAndAfterItFinishes(t *testing.T) {
	runtimes := &startedRuntimes{}
	session := openOnRuntimes(t, runtimes, base.OpenRequest{})
	client := runtimes.clients[0]
	response, stream := admission(t, session, client, "m1")
	assertSwitchRefused(t, session, runtimes, "while the run is active")

	client.ev(5, "assistant/message", assistantMessage(1, 1, "a", []native.ContentBlock{{Type: "text", Text: "hi"}}, native.MessageSource{Kind: "model", Provider: "deepseek", Model: "chat"}, `[{"type":"chunk","time":5,"chunk":{"type":"text-delta","index":0,"text":"hi"}}]`, &native.TokenUsage{InputTokens: 2, OutputTokens: 1}))
	client.ev(7, "step/end", native.StepBoundary{Turn: 1, Step: 1})
	client.ev(8, "turn/end", native.TurnEnd{Turn: 1, Reason: json.RawMessage(`{"kind":"completed"}`)})
	client.notify(&native.SessionStatusNotification{SessionID: "session", Status: "idle"})
	events := drain(t, stream)
	if last := events[len(events)-1]; last.Type != protocol.TypeRunCompleted {
		t.Fatalf("the run ended with %s, want run.completed", last.Type)
	}
	current, err := session.State(context.Background())
	if err != nil || current.Status != protocol.SessionIdle || response.RunID == "" {
		t.Fatalf("state %+v, err %v, run %q, want an idle session after a finished run", current, err, response.RunID)
	}
	assertSwitchRefused(t, session, runtimes, "after the run finished")
}

func TestASwitchToAModelTheRuntimeRejectsIsModelNotFoundAndKeepsTheSession(t *testing.T) {
	runtimes := &startedRuntimes{failure: func(call int) error {
		if call == 2 {
			return fmt.Errorf("%w: %w", rpc.ErrHandshake, &rpc.RemoteError{Object: rpc.ErrorObject{Code: -32000, Message: "no such model"}})
		}
		return nil
	}}
	session := openOnRuntimes(t, runtimes, base.OpenRequest{})
	_, _, err := switchTo(t, session, "nope")
	var missing *base.ModelNotFoundError
	if !errors.As(err, &missing) || missing.ModelID != "nope" {
		t.Fatalf("switch answered %v, want model_not_found for nope", err)
	}
	current, err := session.State(context.Background())
	if err != nil || current.CurrentModelID != "m" {
		t.Fatalf("state %+v, err %v, want the session still on m", current, err)
	}
	select {
	case <-runtimes.clients[0].Done():
		t.Fatal("a refused switch shut the working runtime down")
	default:
	}
}

func TestASwitchThatCannotStartARuntimeIsNotModelNotFound(t *testing.T) {
	boom := errors.New("spawn failed")
	runtimes := &startedRuntimes{failure: func(call int) error {
		if call == 2 {
			return boom
		}
		return nil
	}}
	session := openOnRuntimes(t, runtimes, base.OpenRequest{})
	_, _, err := switchTo(t, session, "other")
	if !errors.Is(err, boom) || errors.Is(err, base.ErrModelNotFound) {
		t.Fatalf("switch answered %v, want the start failure itself", err)
	}
}

func TestASuppliedFactoryRefusesASwitchWithoutStartingAnything(t *testing.T) {
	started := 0
	implementation, err := New(Config{Factory: ClientFactoryFunc(func(context.Context) (Client, string, error) {
		started++
		return newFake(), "m", nil
	})})
	if err != nil {
		t.Fatal(err)
	}
	session, err := implementation.Open(context.Background(), base.OpenRequest{SessionID: "session"})
	if err != nil {
		t.Fatal(err)
	}
	defer session.Close(context.Background())
	_, _, err = switchTo(t, session, "other")
	var refusal *base.UnsupportedControlError
	if !errors.As(err, &refusal) || refusal.Reason != base.ControlUnsatisfiable || started != 1 {
		t.Fatalf("switch answered %v with %d starts, want an unsatisfiable refusal and no restart", err, started)
	}
}

func TestTheDescriptorOffersTheModelSwitchOnlyAsAnEmulation(t *testing.T) {
	implementation, err := New(Config{Executable: "/bin/true", WorkingDirectory: t.TempDir(), Provider: "p", Model: "m", ProcessFactory: (&startedRuntimes{}).factory()})
	if err != nil {
		t.Fatal(err)
	}
	descriptor, err := implementation.Probe(context.Background())
	if err != nil {
		t.Fatal(err)
	}
	if support := descriptor.Capabilities.Features[protocol.FeatureSessionModelSwitch]; support.Level != protocol.SupportEmulated {
		t.Fatalf("session.model.switch = %+v, want emulated", support)
	}
}
