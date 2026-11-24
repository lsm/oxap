package appserver

import (
	"context"
	"encoding/base64"
	"encoding/json"
	"fmt"
	"slices"
	"strings"
	"time"

	"github.com/lsm/oxap/go/adapter"
	"github.com/lsm/oxap/go/adapter/codex/appserver/internal/native"
	"github.com/lsm/oxap/go/adapter/codex/appserver/internal/rpc"
	"github.com/lsm/oxap/go/protocol"
)

const (
	maxDecodedImage   = 4 << 20
	maxEncodedImages  = 6 << 20
	maxImageTurnFrame = 7 << 20
)

type imageModels struct {
	ids          map[string]bool
	defaultModel string
}

func (a *Adapter) discoverImages() {
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	client, err := a.config.Factory.Start(ctx)
	if err != nil {
		return
	}
	defer client.Close()
	a.images.ids = make(map[string]bool)
	params := map[string]any{"includeHidden": true}
	for range 4 {
		var page struct {
			Data []struct {
				ID, Model       string
				IsDefault       bool
				InputModalities []string
			}
			NextCursor string
		}
		if client.Call(ctx, "model/list", params, &page) != nil {
			break
		}
		for _, model := range page.Data {
			if model.ID == "" || model.Model == "" || !slices.Contains(model.InputModalities, "image") {
				continue
			}
			a.images.ids[model.ID] = true
			a.images.ids[model.Model] = true
			if model.IsDefault {
				a.images.defaultModel = model.Model
			}
		}
		if page.NextCursor == "" {
			break
		}
		params["cursor"] = page.NextCursor
	}
	if a.config.Model == "" && len(a.images.ids) > 0 {
		cwd := struct {
			Cwd string `json:"cwd,omitempty"`
		}{a.config.WorkingDirectory}
		var response struct{ Config struct{ Model string } }
		if client.Call(ctx, "config/read", cwd, &response) != nil {
			a.images.defaultModel = ""
		} else if response.Config.Model != "" {
			a.images.defaultModel = response.Config.Model
		}
	}
}

func inlineImage(image *protocol.ImageContent, total *int) (native.UserInput, error) {
	var input native.UserInput
	if image == nil {
		return input, fmt.Errorf("%w: image input requires inline base64 data", adapter.ErrInvalidSubmission)
	}
	switch image.MediaType {
	case "image/png", "image/jpeg", "image/gif", "image/webp":
	default:
		return input, fmt.Errorf("%w: inline images require PNG, JPEG, GIF or WebP media_type", adapter.ErrInvalidSubmission)
	}
	data := image.Data
	if image.URL != "" || data == "" {
		return input, fmt.Errorf("%w: image input requires inline base64 data", adapter.ErrInvalidSubmission)
	}
	if len(data) > maxEncodedImages-*total {
		return input, fmt.Errorf("%w: inline images exceed 6 MiB of base64 per submission", adapter.ErrInvalidSubmission)
	}
	invalid := fmt.Errorf("%w: image input requires valid standard base64", adapter.ErrInvalidSubmission)
	if len(data)%4 != 0 || strings.ContainsAny(data, "\r\n") {
		return input, invalid
	}
	decoder := base64.StdEncoding.Strict()
	size := decoder.DecodedLen(len(data)) - (min(2, len(data)-len(strings.TrimRight(data, "="))))
	if size > maxDecodedImage {
		return input, fmt.Errorf("%w: an inline image exceeds 4 MiB decoded", adapter.ErrInvalidSubmission)
	}
	if _, err := decoder.DecodeString(data); err != nil {
		return input, invalid
	}
	*total += len(data)
	return native.UserInput{Type: "image", URL: "data:" + image.MediaType + ";base64," + data}, nil
}

func (s *session) prepareImageTurn(params native.TurnStartParams) (rpc.Message, error) {
	client := s.client.(*processClient)
	request, err := client.Process.Client.PrepareCall(native.MethodTurnStart, params)
	if err != nil {
		return rpc.Message{}, err
	}
	frame, err := json.Marshal(request)
	if err != nil {
		return rpc.Message{}, err
	}
	if len(frame) > min(s.frameLimit, maxImageTurnFrame) {
		return rpc.Message{}, fmt.Errorf("%w: the Codex turn exceeds the native frame limit", adapter.ErrInvalidSubmission)
	}
	return request, nil
}

func (s *session) imageModel(model string) (string, error) {
	if model == "" {
		model = s.state.CurrentModelID
		if model == "" {
			model = s.images.defaultModel
		}
	}
	if !s.images.ids[model] {
		return "", &adapter.UnsupportedControlError{Feature: protocol.FeatureContentImage, Reason: adapter.ControlUnsatisfiable, Detail: "the effective Codex model does not support inline images"}
	}
	return model, nil
}
