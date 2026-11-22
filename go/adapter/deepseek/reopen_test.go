package deepseek

import (
	"context"
	"errors"
	"sync"
	"testing"

	base "github.com/lsm/oxap/go/adapter"
	"github.com/lsm/oxap/go/adapter/deepseek/internal/native"
	"github.com/lsm/oxap/go/adapter/deepseek/internal/rpc"
	"github.com/lsm/oxap/go/protocol"
)

type recordedCall struct {
	method string
	params any
}

type resumingClient struct {
	mu        sync.Mutex
	calls     []recordedCall
	resumeErr error
	closed    bool
	in        chan rpc.InboundMessage
	done      chan struct{}
}

func newResumingClient(resumeErr error) *resumingClient {
	return &resumingClient{resumeErr: resumeErr, in: make(chan rpc.InboundMessage), done: make(chan struct{})}
}

func (c *resumingClient) Call(ctx context.Context, method string, params, result any) error {
	return c.CallStarted(ctx, method, params, result, nil)
}

func (c *resumingClient) CallStarted(_ context.Context, method string, params, _ any, started chan<- error) error {
	c.mu.Lock()
	c.calls = append(c.calls, recordedCall{method, params})
	c.mu.Unlock()
	if started != nil {
		started <- nil
		close(started)
	}
	if method == native.MethodSessionResume {
		acknowledged := make(chan struct{})
		c.in <- rpc.InboundMessage{Barrier: acknowledged}
		<-acknowledged
		return c.resumeErr
	}
	return errors.New("refused")
}

func (c *resumingClient) Inbound() <-chan rpc.InboundMessage { return c.in }
func (c *resumingClient) Done() <-chan struct{}              { return c.done }
func (c *resumingClient) Err() error                         { return nil }
func (c *resumingClient) Close() error {
	c.mu.Lock()
	defer c.mu.Unlock()
	if !c.closed {
		c.closed = true
		close(c.done)
	}
	return nil
}

func (c *resumingClient) recorded() []recordedCall {
	c.mu.Lock()
	defer c.mu.Unlock()
	return append([]recordedCall(nil), c.calls...)
}

func resumingAdapter(t *testing.T, client *resumingClient, nativeIDs func(protocol.SessionID) string) *Adapter {
	t.Helper()
	implementation, err := New(Config{NativeSessionID: nativeIDs, Factory: ClientFactoryFunc(func(context.Context) (Client, string, error) { return client, "deepseek-chat", nil }), Clock: &testClock{}, IDs: &testIDs{}, JournalCapacity: 64})
	if err != nil {
		t.Fatal(err)
	}
	return implementation
}

func TestReopenResumesTheBoundNativeSessionAndReportsItRecovered(t *testing.T) {
	client := newResumingClient(nil)
	implementation := resumingAdapter(t, client, sameNativeID)
	descriptor, err := implementation.Probe(context.Background())
	if err != nil {
		t.Fatal(err)
	}
	if support := descriptor.Capabilities.Features[protocol.FeatureOpenReopen]; support.Level != protocol.SupportNative {
		t.Fatalf("reopen advertised as %q, want native", support.Level)
	}
	opened, err := implementation.Open(context.Background(), base.OpenRequest{SessionID: "s1", Reopen: true, NativeSessionID: "stored"})
	if err != nil {
		t.Fatal(err)
	}
	defer opened.Close(context.Background())
	session := opened.(*Session)
	if session.NativeSessionID() != "stored" {
		t.Fatalf("native id = %q, want the stored one", session.NativeSessionID())
	}
	state, err := session.State(context.Background())
	if err != nil {
		t.Fatal(err)
	}
	if state.SessionID != "s1" || state.Recovery == nil || !state.Recovery.Recovered || state.Recovery.Reason != reopenRecoveryReason {
		t.Fatalf("state = %+v, want s1 recovered", state)
	}
	calls := client.recorded()
	if len(calls) != 1 || calls[0].method != native.MethodSessionResume || calls[0].params != (native.SessionResumeParams{SessionID: "stored"}) {
		t.Fatalf("calls = %+v, want one session/resume naming the stored native id", calls)
	}
}

func TestAReopenTheHarnessCannotResumeIsRefusedAsUnsatisfiable(t *testing.T) {
	client := newResumingClient(&rpc.RemoteError{Object: rpc.ErrorObject{Message: "no stored session"}})
	_, err := resumingAdapter(t, client, sameNativeID).Open(context.Background(), base.OpenRequest{SessionID: "s1", Reopen: true, NativeSessionID: "stored"})
	var refusal *base.UnsupportedControlError
	if !errors.As(err, &refusal) || refusal.Feature != protocol.FeatureOpenReopen || refusal.Reason != base.ControlUnsatisfiable {
		t.Fatalf("open answered %v, want reopen unsatisfiable", err)
	}
	if !client.closed {
		t.Fatal("the runtime that could not resume was left running")
	}
}

func TestAReopenNamingNoNativeSessionIsRefusedBeforeTheRuntimeStarts(t *testing.T) {
	started := 0
	implementation, err := New(Config{Factory: ClientFactoryFunc(func(context.Context) (Client, string, error) {
		started++
		return nil, "", errors.New("a refused reopen must not start a runtime")
	})})
	if err != nil {
		t.Fatal(err)
	}
	_, err = implementation.Open(context.Background(), base.OpenRequest{SessionID: "s1", Reopen: true, NativeSessionID: " "})
	var refusal *base.UnsupportedControlError
	if !errors.As(err, &refusal) || refusal.Feature != protocol.FeatureOpenReopen || refusal.Reason != base.ControlUnsatisfiable || started != 0 {
		t.Fatalf("refusal = %v, runtimes started = %d", err, started)
	}
}

func TestAFreshOpenRunsUnderANativeIDOfItsOwnWhichThePromptAndLinkCarry(t *testing.T) {
	client := newResumingClient(nil)
	implementation := resumingAdapter(t, client, nil)
	opened, err := implementation.Open(context.Background(), base.OpenRequest{SessionID: "s1"})
	if err != nil {
		t.Fatal(err)
	}
	defer opened.Close(context.Background())
	nativeID := opened.(*Session).NativeSessionID()
	if nativeID == "s1" || len(nativeID) != 32 || nativeID == implementation.nativeSessionID("s1") {
		t.Fatalf("native id = %q, want 32 fresh hex characters of its own", nativeID)
	}
	if link := implementation.NativeLink(nativeID); link != "deepseek://sessions/"+nativeID {
		t.Fatalf("link = %q", link)
	}
	_, _, _ = opened.Submit(context.Background(), base.SubmitRequest{Request: protocol.MessageSubmitRequest{SessionID: "s1", Delivery: protocol.DeliveryAuto, Messages: []protocol.Message{{Role: protocol.RoleUser, Content: protocol.TextContent("hello")}}}})
	calls := client.recorded()
	if len(calls) == 0 || calls[0].method != native.MethodSessionPrompt {
		t.Fatalf("calls = %+v, want a session/prompt", calls)
	}
	if prompt, ok := calls[0].params.(native.SessionPromptParams); !ok || prompt.SessionID != nativeID {
		t.Fatalf("prompt = %+v, want it addressed to %q", calls[0].params, nativeID)
	}
}

func sameNativeID(id protocol.SessionID) string { return string(id) }
