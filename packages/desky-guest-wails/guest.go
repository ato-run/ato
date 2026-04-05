package deskyguestwails

import (
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"os"
	"os/signal"
	"path/filepath"
	"strings"
	"syscall"
	"time"
)

type Context struct {
	sampleRoot string
	adapter    string
	sessionID  string
	guestMode  string
	host       string
	port       string
}

type CommandEnvelope struct {
	Payload map[string]any `json:"payload"`
	Title   *string        `json:"title,omitempty"`
	Path    *string        `json:"path,omitempty"`
}

func FromEnv(adapter string, defaultPort int, sampleRoot string) *Context {
	guestMode := ""
	if os.Getenv("ATO_GUEST_MODE") == "1" {
		guestMode = "1"
	}

	return &Context{
		sampleRoot: sampleRoot,
		adapter:    getenvDefault("DESKY_SESSION_ADAPTER", adapter),
		sessionID:  getenvDefault("DESKY_SESSION_ID", "desky-session"),
		guestMode:  guestMode,
		host:       getenvDefault("DESKY_SESSION_HOST", "127.0.0.1"),
		port:       getenvDefault("DESKY_SESSION_PORT", fmt.Sprintf("%d", defaultPort)),
	}
}

func (c *Context) Adapter() string {
	return c.adapter
}

func (c *Context) SessionID() string {
	return c.sessionID
}

func (c *Context) GuestMode() string {
	return c.guestMode
}

func (c *Context) IsGuestMode() bool {
	return c.guestMode == "1"
}

func (c *Context) SampleRoot() string {
	return c.sampleRoot
}

func (c *Context) BindAddr() string {
	return fmt.Sprintf("%s:%s", c.host, c.port)
}

func (c *Context) CheckEnv() map[string]any {
	return map[string]any{
		"ok":             true,
		"adapter":        c.adapter,
		"session_id":     c.sessionID,
		"ato_guest_mode": emptyToNil(c.guestMode),
	}
}

func (c *Context) Ping(message string) map[string]any {
	return map[string]any{
		"ok":         true,
		"adapter":    c.adapter,
		"session_id": c.sessionID,
		"command":    "ping",
		"echo":       message,
	}
}

func (c *Context) ResolveAllowedPath(relativePath string) (string, error) {
	resolved := filepath.Clean(filepath.Join(c.sampleRoot, relativePath))
	rootClean := filepath.Clean(c.sampleRoot)
	if resolved != rootClean && !strings.HasPrefix(resolved, rootClean+string(os.PathSeparator)) {
		return "", fmt.Errorf("BoundaryPolicyError: Guest file read is outside the allowed root: %s", relativePath)
	}
	return resolved, nil
}

func BuiltinResult(context *Context, command string, envelope CommandEnvelope) (map[string]any, bool) {
	switch command {
	case "check_env":
		return context.CheckEnv(), true
	case "ping":
		return context.Ping(MessageFromPayload(envelope.Payload)), true
	default:
		return nil, false
	}
}

func MessageFromPayload(payload map[string]any) string {
	message, _ := payload["message"].(string)
	return message
}

func NewServer(context *Context, handler func(*Context, string, CommandEnvelope) map[string]any) *http.Server {
	mux := http.NewServeMux()

	mux.HandleFunc("/health", func(w http.ResponseWriter, _ *http.Request) {
		writeJSON(w, http.StatusOK, map[string]any{
			"ok":         true,
			"adapter":    context.adapter,
			"session_id": context.sessionID,
			"guest_mode": emptyToNil(context.guestMode),
		})
	})

	mux.HandleFunc("/rpc", func(w http.ResponseWriter, r *http.Request) {
		if r.Method != http.MethodPost {
			writeJSON(w, http.StatusMethodNotAllowed, map[string]any{"ok": false, "error": "method_not_allowed"})
			return
		}

		body, err := io.ReadAll(r.Body)
		if err != nil {
			writeJSON(w, http.StatusBadRequest, map[string]any{"ok": false, "error": err.Error()})
			return
		}

		var request map[string]any
		if len(body) == 0 {
			body = []byte("{}")
		}
		if err := json.Unmarshal(body, &request); err != nil {
			writeJSON(w, http.StatusBadRequest, map[string]any{"ok": false, "error": err.Error()})
			return
		}

		params, _ := request["params"].(map[string]any)
		command, _ := params["command"].(string)
		payload, _ := params["payload"].(map[string]any)
		title, _ := params["title"].(string)
		pathValue, _ := params["path"].(string)

		envelope := CommandEnvelope{
			Payload: payload,
		}
		if title != "" {
			envelope.Title = &title
		}
		if pathValue != "" {
			envelope.Path = &pathValue
		}

		result := handler(context, command, envelope)
		writeJSON(w, http.StatusOK, map[string]any{
			"jsonrpc": "2.0",
			"id":      request["id"],
			"result":  result,
		})
	})

	return &http.Server{
		Addr:    context.BindAddr(),
		Handler: mux,
	}
}

func StartServer(server *http.Server) <-chan error {
	errCh := make(chan error, 1)
	go func() {
		err := server.ListenAndServe()
		if err != nil && err != http.ErrServerClosed {
			errCh <- err
		}
	}()
	return errCh
}

func WaitForShutdown(server *http.Server, errCh <-chan error) {
	signals := make(chan os.Signal, 1)
	signal.Notify(signals, syscall.SIGINT, syscall.SIGTERM)
	select {
	case err := <-errCh:
		panic(err)
	case <-signals:
		ShutdownServer(server)
	}
}

func ShutdownServer(server *http.Server) {
	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
	defer cancel()
	_ = server.Shutdown(ctx)
}

func writeJSON(w http.ResponseWriter, statusCode int, payload map[string]any) {
	body, err := json.Marshal(payload)
	if err != nil {
		w.WriteHeader(http.StatusInternalServerError)
		_, _ = w.Write([]byte(`{"ok":false,"error":"marshal_error"}`))
		return
	}
	w.Header().Set("Content-Type", "application/json")
	w.Header().Set("Content-Length", fmt.Sprintf("%d", len(body)))
	w.WriteHeader(statusCode)
	_, _ = w.Write(body)
}

func getenvDefault(key, fallback string) string {
	if value := os.Getenv(key); value != "" {
		return value
	}
	return fallback
}

func emptyToNil(value string) any {
	if value == "" {
		return nil
	}
	return value
}
