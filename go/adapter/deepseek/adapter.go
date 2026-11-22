package deepseek

import (
	"context"
	"crypto/rand"
	"encoding/hex"
	"errors"
	"fmt"
	"github.com/lsm/oxap/harnesses"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"sync/atomic"
	"time"

	base "github.com/lsm/oxap/go/adapter"
	"github.com/lsm/oxap/go/adapter/deepseek/internal/native"
	"github.com/lsm/oxap/go/adapter/deepseek/internal/rpc"
	"github.com/lsm/oxap/go/adapter/internal/journal"
	"github.com/lsm/oxap/go/protocol"
)

var (
	pin                = harnesses.Current("deepseek-harness")
	PinnedVersion      = pin.EndpointVersion
	CapabilityRevision = pin.CapabilityRevision
	CorpusDirectory    = pin.Corpus
)

const (
	endpointID             = "deepseek.harness"
	defaultJournalCapacity = 256
)

var ErrNativeProtocol = errors.New("deepseek adapter: invalid native protocol observation")

type Client interface {
	Call(context.Context, string, any, any) error
	CallStarted(context.Context, string, any, any, chan<- error) error
	Inbound() <-chan rpc.InboundMessage
	Done() <-chan struct{}
	Err() error
	Close() error
}

type ClientFactory interface {
	Start(context.Context) (Client, string, error)
}
type ClientFactoryFunc func(context.Context) (Client, string, error)

func (f ClientFactoryFunc) Start(ctx context.Context) (Client, string, error) { return f(ctx) }

type ProcessBridge interface {
	ClientHandle() Client
	Done() <-chan struct{}
	WaitError() error
	Close(context.Context) error
}
type ProcessFactory interface {
	Start(context.Context, rpc.ProcessConfig) (ProcessBridge, error)
}
type ProcessFactoryFunc func(context.Context, rpc.ProcessConfig) (ProcessBridge, error)

func (f ProcessFactoryFunc) Start(ctx context.Context, c rpc.ProcessConfig) (ProcessBridge, error) {
	return f(ctx, c)
}

type Config struct {
	Factory            ClientFactory
	ProcessFactory     ProcessFactory
	Executable         string
	Args               []string
	Environment        []string
	WorkingDirectory   string
	Provider           string
	Model              string
	MaxTokens          *int64
	Clock              base.Clock
	IDs                base.IDGenerator
	NativeSessionID    func(protocol.SessionID) string
	JournalCapacity    int
	FrameLimit         int
	QueueCapacity      int
	WriteQueueCapacity int
	ShutdownTimeout    time.Duration
}

type Adapter struct {
	config Config
	clock  base.Clock
	ids    base.IDGenerator

	process *rpc.ProcessConfig
}

func (a *Adapter) NativeLink(nativeID string) string {
	return "deepseek://sessions/" + nativeID
}

func New(config Config) (*Adapter, error) {
	if config.Factory == nil && config.ProcessFactory == nil && config.Executable == "" {
		return nil, errors.New("deepseek adapter: factory or executable is required")
	}
	if config.Factory == nil {
		if config.WorkingDirectory == "" || !filepath.IsAbs(config.WorkingDirectory) {
			return nil, errors.New("deepseek adapter: absolute working directory is required")
		}
		if err := native.ValidateInitializeParams(native.InitializeParams{Cwd: config.WorkingDirectory, Provider: config.Provider, Model: config.Model, MaxTokens: config.MaxTokens}); err != nil {
			return nil, err
		}
	}
	if config.Clock == nil {
		config.Clock = systemClock{}
	}
	if config.IDs == nil {
		config.IDs = &sequenceIDs{}
	}
	if config.JournalCapacity <= 0 {
		config.JournalCapacity = defaultJournalCapacity
	}
	if config.ProcessFactory == nil {
		config.ProcessFactory = ProcessFactoryFunc(func(ctx context.Context, c rpc.ProcessConfig) (ProcessBridge, error) {
			p, err := rpc.Start(ctx, c)
			if err != nil {
				return nil, err
			}
			return &rpcProcess{p}, nil
		})
	}
	if config.Factory == nil {

		env := config.Environment
		if env != nil {
			env = append([]string{}, env...)
		}
		pc := rpc.ProcessConfig{Path: config.Executable, Args: append([]string(nil), config.Args...), Dir: config.WorkingDirectory, Env: env, FrameLimit: config.FrameLimit, QueueCapacity: config.QueueCapacity, WriteQueueCapacity: config.WriteQueueCapacity, ShutdownTimeout: config.ShutdownTimeout, Initialize: native.InitializeParams{Cwd: config.WorkingDirectory, Provider: config.Provider, Model: config.Model, MaxTokens: config.MaxTokens}}
		config.Factory = ClientFactoryFunc(func(ctx context.Context) (Client, string, error) {
			p, err := config.ProcessFactory.Start(ctx, pc)
			if err != nil {
				return nil, "", err
			}
			return &processClient{Client: p.ClientHandle(), bridge: p}, config.Model, nil
		})
		return &Adapter{config: config, clock: config.Clock, ids: config.IDs, process: &pc}, nil
	}
	return &Adapter{config: config, clock: config.Clock, ids: config.IDs}, nil
}

type rpcProcess struct{ *rpc.Process }

func (p *rpcProcess) ClientHandle() Client { return p.Client }

type processClient struct {
	Client
	bridge ProcessBridge
}

func (p *processClient) Done() <-chan struct{} { return p.bridge.Done() }
func (p *processClient) Err() error {
	if err := p.bridge.WaitError(); err != nil {
		return err
	}
	return p.Client.Err()
}
func (p *processClient) Close() error {
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	return p.bridge.Close(ctx)
}

func acceptsImages(model string) bool {
	switch model {
	case "deepseek-flash", "deepseek-v4-flash", "deepseek-v4-flash-vision-exp":
		return true
	default:
		return false
	}
}

func capabilityRevision(model string) string {
	if acceptsImages(model) {
		return CapabilityRevision + "-image"
	}
	return CapabilityRevision
}

func (a *Adapter) Probe(ctx context.Context) (base.Descriptor, error) {
	if err := ctx.Err(); err != nil {
		return base.Descriptor{}, err
	}
	features := map[string]protocol.FeatureSupport{
		"protocol.initialize":              {Level: protocol.SupportEmulated, Reason: "adapter-owned one-shot initialization freeze"},
		"capabilities":                     {Level: protocol.SupportEmulated, Reason: "conservative descriptor for pinned SDK wire"},
		"session.open":                     {Level: protocol.SupportEmulated, Reason: "one process and native session per OAP session"},
		protocol.FeatureOpenReopen:         {Level: protocol.SupportNative, Reason: reopenSupportReason},
		"session.state":                    {Level: protocol.SupportDegraded, Reason: "reducer-owned live projection; no native query"},
		protocol.FeatureSessionModelSwitch: {Level: protocol.SupportEmulated, Reason: "the runtime freezes its model at initialize, so a session that has not run restarts its own runtime on the new model; once it has run, a switch is refused"},
		protocol.FeatureSessionReasoning:   {Level: protocol.SupportNative, Modes: []string{protocol.ModeSessionOpen}, Reason: "initialize carries reasoningEffort for the runtime this session starts; the DeepSeek route takes off, low, high and max"},
		protocol.FeatureCompactionPolicy:   {Level: protocol.SupportEmulated, Modes: []string{protocol.ModeSessionOpen}, Reason: "a --patch layer on this session's runtime sets compaction-basic's thresholdRatio for share, or disables it for off; its headroom still caps the threshold, and tokens is refused"},
		"session.message.submit":           {Level: protocol.SupportDegraded, Reason: "receipt plus entered direct-user message proves start"},
		"session.message.delivery.auto":    {Level: protocol.SupportDegraded, Reason: "accepted only for known idle sessions and normalized to start"},
		"session.message.delivery.queue":   {Level: protocol.SupportUnavailable, Reason: "overlapping native followups are outside the adapter contract"},
		"session.message.delivery.steer":   {Level: protocol.SupportUnavailable, Reason: "selected SDK wire has no steer request"},
		"run.streaming":                    {Level: protocol.SupportNative}, "run.status": {Level: protocol.SupportEmulated},
		"run.cancel":           {Level: protocol.SupportUnavailable, Reason: "selected SDK wire has no cancel request"},
		"run.resume":           {Level: protocol.SupportDegraded, Reason: "selected SDK wire has no resume request; OAP resume replays the adapter journal"},
		"run.reconciliation":   {Level: protocol.SupportDegraded, Reason: "live status corroboration only"},
		"run.replay":           {Level: protocol.SupportDegraded, Reason: "bounded adapter journal; gaps are explicit and there is no native replay request"},
		"action.tools":         {Level: protocol.SupportDegraded, Reason: "call/result only; started is synthesized"},
		"action.tools.execute": {Level: protocol.SupportUnavailable, Reason: "Harness executes tools internally"},
		"action.permissions":   {Level: protocol.SupportUnavailable, Reason: "selected SDK wire has no interaction channel"},
	}
	if acceptsImages(a.config.Model) {
		features[protocol.FeatureContentImage] = protocol.FeatureSupport{Level: protocol.SupportNative, Reason: "inline image input on the configured model; after a model switch, the selected model governs admission"}
	}
	return base.Descriptor{Capabilities: protocol.CapabilityDescriptor{Endpoint: protocol.EndpointDescriptor{ID: endpointID, Name: "DeepSeek Harness SDK Adapter", Version: PinnedVersion, Adapter: "deepseek-harness-jsonrpc"}, ProtocolVersions: []string{protocol.Version}, Profiles: []string{protocol.Profile}, Features: features}, CapabilityRevision: capabilityRevision(a.config.Model), Journal: base.JournalDescriptor{Scope: "session", Persistence: "process_memory", Replay: protocol.SupportDegraded, Capacity: a.config.JournalCapacity}, MaxActiveRunsPerSession: 1, InteractiveGates: false, CancellationTarget: "none", CancellationImplementation: "unavailable"}, nil
}

func (a *Adapter) Open(ctx context.Context, req base.OpenRequest) (base.Session, error) {
	if err := ctx.Err(); err != nil {
		return nil, err
	}
	if req.Reopen && strings.TrimSpace(req.NativeSessionID) == "" {
		return nil, &base.UnsupportedControlError{Feature: protocol.FeatureOpenReopen, Reason: base.ControlUnsatisfiable, Detail: "a reopen names no stored native session"}
	}

	if err := base.RefuseUnadvertisedToolSources(req); err != nil {
		return nil, err
	}

	if err := base.RefuseUnadvertisedTools(req); err != nil {
		return nil, err
	}
	descriptor, err := a.Probe(ctx)
	if err != nil {
		return nil, err
	}
	if err := base.RefuseUnadvertisedSettings(req, descriptor.Capabilities); err != nil {
		return nil, err
	}
	client, model, err := a.resumed(ctx, req, "")
	if err != nil {
		return nil, err
	}
	id := req.SessionID
	if id == "" {
		id = protocol.SessionID(a.ids.NewID("session"))
	}
	nativeID := req.NativeSessionID
	var recovery *protocol.RecoveryMetadata
	if req.Reopen {
		recovery = &protocol.RecoveryMetadata{Recovered: true, Reason: reopenRecoveryReason}
	} else {
		nativeID = a.nativeSessionID(id)
	}
	now := a.clock.Now().UnixMilli()
	s := &Session{client: client, inbound: client.Inbound(), clock: a.clock, ids: a.ids, journal: journal.New(a.config.JournalCapacity), nativeID: nativeID, model: model, revision: descriptor.CapabilityRevision, state: protocol.SessionState{SessionID: id, Status: protocol.SessionIdle, CurrentModelID: model, UpdatedAtMS: now, ReasoningLevel: req.ReasoningLevel, CompactionPolicy: req.CompactionPolicy, Recovery: recovery}, runs: map[protocol.RunID]*runState{}, tools: map[string]*toolState{}, children: map[string]*childState{}, stop: make(chan struct{}), exited: make(chan struct{}), restart: func(ctx context.Context, model string) (Client, string, error) { return a.resumed(ctx, req, model) }}
	go s.dispatch(s.client, s.inbound, s.stop, s.exited)
	return s, nil
}

const (
	reopenSupportReason  = "session/resume reloads the bound stored session after initialize, under the initialized model route"
	reopenRecoveryReason = "the harness restored the bound conversation; OAP runs and cursors remain process-local"
)

func (a *Adapter) nativeSessionID(id protocol.SessionID) string {
	if a.config.NativeSessionID != nil {
		return a.config.NativeSessionID(id)
	}
	var random [16]byte
	_, _ = rand.Read(random[:])
	return hex.EncodeToString(random[:])
}

func (a *Adapter) resumed(ctx context.Context, req base.OpenRequest, model string) (Client, string, error) {
	client, model, err := a.start(ctx, req, model)
	if err != nil || !req.Reopen {
		return client, model, err
	}
	if err := resume(ctx, client, req.NativeSessionID); err != nil {
		_ = client.Close()
		return nil, "", &base.UnsupportedControlError{Feature: protocol.FeatureOpenReopen, Reason: base.ControlUnsatisfiable, Detail: "the harness could not resume the stored session: " + err.Error()}
	}
	return client, model, nil
}

var resumeTimeout = 30 * time.Second

func resume(ctx context.Context, client Client, nativeID string) error {
	ctx, cancel := context.WithTimeout(ctx, resumeTimeout)
	defer cancel()
	answered := make(chan error, 1)
	go func() {
		var result native.SessionResumeResult
		answered <- client.Call(ctx, native.MethodSessionResume, native.SessionResumeParams{SessionID: nativeID}, &result)
	}()
	for {
		select {
		case err := <-answered:
			return err
		case message := <-client.Inbound():
			if message.Barrier != nil {
				close(message.Barrier)
				continue
			}
			return errors.New("a native observation preceded the resume response")
		case <-client.Done():
			if err := client.Err(); err != nil {
				return err
			}
			return errors.New("the runtime closed before answering the resume")
		case <-ctx.Done():
			return ctx.Err()
		}
	}
}

var deepseekEfforts = map[protocol.ReasoningLevel]bool{protocol.ReasoningOff: true, protocol.ReasoningLow: true, protocol.ReasoningHigh: true, protocol.ReasoningMax: true}

func (a *Adapter) start(ctx context.Context, req base.OpenRequest, model string) (Client, string, error) {
	if model == "" && req.ReasoningLevel == "" && req.CompactionPolicy == nil {
		return a.config.Factory.Start(ctx)
	}
	if req.ReasoningLevel != "" && !deepseekEfforts[req.ReasoningLevel] {
		return nil, "", &base.UnsupportedControlError{Feature: protocol.FeatureSessionReasoning, Reason: base.ControlUnsatisfiable, Field: "reasoning_level", Detail: "the DeepSeek route takes off, low, high and max"}
	}
	patch, err := compactionPatch(req.CompactionPolicy)
	if err != nil {
		return nil, "", err
	}
	if model == "" && req.ReasoningLevel == "" && patch == "" {
		return a.config.Factory.Start(ctx)
	}
	if a.process == nil {
		feature, field := protocol.FeatureSessionReasoning, "reasoning_level"
		switch {
		case model != "":
			feature, field = protocol.FeatureSessionModelSwitch, "model_id"
		case req.ReasoningLevel == "":
			feature, field = protocol.FeatureCompactionPolicy, "compaction_policy"
		}
		return nil, "", &base.UnsupportedControlError{Feature: feature, Reason: base.ControlUnsatisfiable, Field: field, Detail: "a supplied factory starts the runtime, so the adapter cannot configure it"}
	}
	pc := *a.process
	pc.Args = append([]string(nil), pc.Args...)
	pc.Initialize.ReasoningEffort = string(req.ReasoningLevel)
	if model != "" {
		pc.Initialize.Model = model
	} else {
		model = a.config.Model
	}
	if patch != "" {
		file, err := os.CreateTemp("", "oap-deepseek-*.patch.yml")
		if err != nil {
			return nil, "", err
		}
		defer os.Remove(file.Name())
		if _, err := file.WriteString(patch); err != nil {
			_ = file.Close()
			return nil, "", err
		}
		if err := file.Close(); err != nil {
			return nil, "", err
		}
		pc.Args = append(pc.Args, "--patch", file.Name())
	}
	p, err := a.config.ProcessFactory.Start(ctx, pc)
	if err != nil {
		return nil, "", err
	}
	return &processClient{Client: p.ClientHandle(), bridge: p}, model, nil
}

func compactionPatch(policy *protocol.CompactionPolicy) (string, error) {
	if policy == nil {
		return "", nil
	}
	switch policy.Kind {
	case protocol.CompactionAuto:
		return "", nil
	case protocol.CompactionOff:
		return "- id: compaction-basic\n  disabled: true\n", nil
	case protocol.CompactionShare:
		return fmt.Sprintf("- id: compaction-basic\n  config:\n    thresholdRatio: %s\n", strconv.FormatFloat(float64(policy.SharePercent)/100, 'f', -1, 64)), nil
	}
	return "", &base.UnsupportedControlError{Feature: protocol.FeatureCompactionPolicy, Reason: base.ControlUnsatisfiable, Field: "compaction_policy", Detail: "compaction-basic sets its threshold from the window, so a token count is refused"}
}

type systemClock struct{}

func (systemClock) Now() time.Time { return time.Now() }

type sequenceIDs struct{ next atomic.Uint64 }

func (g *sequenceIDs) NewID(kind string) string { return fmt.Sprintf("%s-%d", kind, g.next.Add(1)) }

var _ base.Adapter = (*Adapter)(nil)
