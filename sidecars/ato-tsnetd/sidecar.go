package main

import (
	"context"
	"errors"
	"fmt"
	"io"
	"log"
	"net"
	"os"
	"path/filepath"
	"strings"
	"sync"

	"github.com/armon/go-socks5"
	"google.golang.org/grpc"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/credentials/insecure"
	"google.golang.org/grpc/status"
	"tailscale.com/tsnet"
)

type sidecarService struct {
	UnimplementedTsnetServiceServer
	mu          sync.Mutex
	server      *tsnet.Server
	listener    net.Listener
	serveLn     net.Listener
	baseCfg     *StartRequest
	socksPort   uint16
	state       TsnetState
	lastErr     string
	allowList   []string // Egress allowlist
	servePort   uint16
	serveAddr   string
	serveErr    string
	serveTarget string
}

func newSidecarService(baseCfg *StartRequest) *sidecarService {
	return &sidecarService{state: TsnetState_TSNET_STATE_STOPPED, baseCfg: baseCfg}
}

func sanitizeStartInput(controlURL, authKey, hostname string) error {
	if controlURL == "" || authKey == "" || hostname == "" {
		return fmt.Errorf("missing control_url/auth_key/hostname")
	}
	return nil
}

func resolveStartRequest(req *StartRequest, base *StartRequest) *StartRequest {
	resolved := *req
	if resolved.ControlUrl == "" && base != nil {
		resolved.ControlUrl = base.ControlUrl
	}
	if resolved.AuthKey == "" && base != nil {
		resolved.AuthKey = base.AuthKey
	}
	if resolved.Hostname == "" && base != nil {
		resolved.Hostname = base.Hostname
	}
	if resolved.SocksPort == 0 && base != nil {
		resolved.SocksPort = base.SocksPort
	}
	if len(resolved.AllowNet) == 0 && base != nil {
		resolved.AllowNet = base.AllowNet
	}
	return &resolved
}

// AllowListRule implements socks5.RuleSet for egress filtering
type AllowListRule struct {
	allowList []string
}

func (r *AllowListRule) Allow(ctx context.Context, req *socks5.Request) (context.Context, bool) {
	// Get destination address from request
	dst := req.DestAddr
	if dst == nil || dst.String() == "" {
		return ctx, false
	}

	addr := dst.String()

	// If allowlist is empty, allow all (best_effort mode)
	if len(r.allowList) == 0 {
		return ctx, true
	}

	// Check against allowlist
	for _, pattern := range r.allowList {
		if matchesPattern(addr, pattern) {
			return ctx, true
		}
	}

	log.Printf("SOCKS5: Denied connection to %s (not in allowlist)", addr)
	return ctx, false
}

// matchesPattern checks if an address matches an allowlist pattern
// Supports:
// - Exact domain: "example.com"
// - Wildcard domain: "*.example.com"
// - CIDR notation: "10.0.0.0/8"
func matchesPattern(addr, pattern string) bool {
	addr = strings.TrimSpace(addr)
	pattern = strings.TrimSpace(pattern)

	if host, _, err := net.SplitHostPort(addr); err == nil {
		addr = host
	}

	// Exact match
	if addr == pattern {
		return true
	}

	// Wildcard match for subdomains
	if strings.HasPrefix(pattern, "*.") {
		domain := pattern[2:]
		if strings.HasSuffix(addr, domain) {
			baseWithDot := "." + domain
			if strings.Contains(addr, baseWithDot) {
				return true
			}
		}
	}

	// CIDR match
	if strings.Contains(pattern, "/") {
		ip := net.ParseIP(addr)
		if ip != nil {
			_, cidr, err := net.ParseCIDR(pattern)
			if err == nil && cidr.Contains(ip) {
				return true
			}
		}
	}

	return false
}

func (s *sidecarService) Start(ctx context.Context, req *StartRequest) (*StartResponse, error) {
	s.mu.Lock()
	defer s.mu.Unlock()

	if s.state == TsnetState_TSNET_STATE_RUNNING || s.state == TsnetState_TSNET_STATE_STARTING {
		return nil, status.Error(codes.FailedPrecondition, "sidecar already running")
	}

	resolved := resolveStartRequest(req, s.baseCfg)
	controlURL := strings.TrimSpace(resolved.ControlUrl)
	authKey := strings.TrimSpace(resolved.AuthKey)
	hostname := strings.TrimSpace(resolved.Hostname)
	if err := sanitizeStartInput(controlURL, authKey, hostname); err != nil {
		return nil, status.Error(codes.InvalidArgument, err.Error())
	}

	port, err := normalizePort(resolved.SocksPort)
	if err != nil {
		return nil, status.Error(codes.InvalidArgument, err.Error())
	}

	s.state = TsnetState_TSNET_STATE_STARTING
	s.lastErr = ""

	// Store allowlist for egress filtering
	s.allowList = resolved.AllowNet
	log.Printf("SOCKS5: Allowlist configured: %v", s.allowList)

	listener, actualPort, err := bindSocksPort(port)
	if err != nil {
		s.state = TsnetState_TSNET_STATE_FAILED
		s.lastErr = err.Error()
		return nil, status.Error(codes.Internal, err.Error())
	}

	server := &tsnet.Server{
		ControlURL: controlURL,
		AuthKey:    authKey,
		Hostname:   hostname,
		Logf:       log.Printf,
	}

	// Create allowlist rule for egress filtering
	allowRule := &AllowListRule{
		allowList: s.allowList,
	}

	proxy, err := socks5.New(&socks5.Config{
		Dial:  server.Dial,
		Rules: allowRule,
	})
	if err != nil {
		listener.Close()
		s.state = TsnetState_TSNET_STATE_FAILED
		s.lastErr = err.Error()
		return nil, status.Error(codes.Internal, err.Error())
	}

	s.server = server
	s.listener = listener
	s.socksPort = uint16(actualPort)
	s.state = TsnetState_TSNET_STATE_RUNNING

	go func() {
		if err := proxy.Serve(listener); err != nil && !errors.Is(err, net.ErrClosed) {
			s.mu.Lock()
			s.state = TsnetState_TSNET_STATE_FAILED
			s.lastErr = err.Error()
			s.mu.Unlock()
		}
	}()

	return &StartResponse{
		State:     s.state,
		SocksPort: uint32(s.socksPort),
		Message:   "",
	}, nil
}

func (s *sidecarService) Stop(ctx context.Context, req *StopRequest) (*StopResponse, error) {
	s.mu.Lock()
	defer s.mu.Unlock()

	if s.state == TsnetState_TSNET_STATE_STOPPED {
		return &StopResponse{State: s.state, Message: "already stopped"}, nil
	}

	s.state = TsnetState_TSNET_STATE_STOPPING
	if s.listener != nil {
		_ = s.listener.Close()
	}
	if s.serveLn != nil {
		_ = s.serveLn.Close()
	}
	if s.server != nil {
		s.server.Close()
	}
	s.listener = nil
	s.server = nil
	s.socksPort = 0
	s.serveLn = nil
	s.servePort = 0
	s.serveAddr = ""
	s.serveTarget = ""
	s.state = TsnetState_TSNET_STATE_STOPPED

	return &StopResponse{State: s.state, Message: ""}, nil
}

func (s *sidecarService) Status(ctx context.Context, req *StatusRequest) (*StatusResponse, error) {
	s.mu.Lock()
	defer s.mu.Unlock()

	return &StatusResponse{
		State:     s.state,
		SocksPort: uint32(s.socksPort),
		Connected: s.state == TsnetState_TSNET_STATE_RUNNING,
		LastError: s.lastErr,
	}, nil
}

func (s *sidecarService) StartServe(ctx context.Context, req *ServeRequest) (*ServeResponse, error) {
	s.mu.Lock()
	defer s.mu.Unlock()

	if s.state != TsnetState_TSNET_STATE_RUNNING || s.server == nil {
		return nil, status.Error(codes.FailedPrecondition, "sidecar not running")
	}

	if s.serveLn != nil {
		return nil, status.Error(codes.FailedPrecondition, "serve already running")
	}

	target := strings.TrimSpace(req.TargetAddr)
	if target == "" {
		return nil, status.Error(codes.InvalidArgument, "missing target_addr")
	}
	if err := validateTargetAddr(target); err != nil {
		return nil, status.Error(codes.InvalidArgument, err.Error())
	}

	port, err := normalizePort(req.ListenPort)
	if err != nil {
		return nil, status.Error(codes.InvalidArgument, err.Error())
	}

	listener, err := s.server.Listen("tcp", fmt.Sprintf(":%d", port))
	if err != nil {
		s.serveErr = err.Error()
		return nil, status.Error(codes.Internal, err.Error())
	}

	actualPort := uint16(listener.Addr().(*net.TCPAddr).Port)
	hostname := ""
	if s.baseCfg != nil {
		hostname = s.baseCfg.Hostname
	}
	if hostname == "" {
		hostname = "tailnet"
	}
	listenAddr := fmt.Sprintf("%s:%d", hostname, actualPort)

	s.serveLn = listener
	s.servePort = actualPort
	s.serveAddr = listenAddr
	s.serveErr = ""
	s.serveTarget = target

	go acceptServe(listener, target, s)

	return &ServeResponse{
		Running:    true,
		ListenPort: uint32(actualPort),
		ListenAddr: listenAddr,
		Message:    "",
	}, nil
}

func (s *sidecarService) StopServe(ctx context.Context, req *ServeStopRequest) (*ServeStopResponse, error) {
	s.mu.Lock()
	defer s.mu.Unlock()

	if s.serveLn != nil {
		_ = s.serveLn.Close()
	}
	s.serveLn = nil
	s.servePort = 0
	s.serveAddr = ""
	s.serveTarget = ""
	return &ServeStopResponse{Running: false, Message: ""}, nil
}

func (s *sidecarService) ServeStatus(ctx context.Context, req *ServeStatusRequest) (*ServeStatusResponse, error) {
	s.mu.Lock()
	defer s.mu.Unlock()

	return &ServeStatusResponse{
		Running:    s.serveLn != nil,
		ListenPort: uint32(s.servePort),
		ListenAddr: s.serveAddr,
		LastError:  s.serveErr,
	}, nil
}

func validateTargetAddr(addr string) error {
	host, _, err := net.SplitHostPort(addr)
	if err != nil {
		return fmt.Errorf("invalid target_addr: %w", err)
	}
	if host == "localhost" {
		return nil
	}
	ip := net.ParseIP(host)
	if ip == nil {
		return fmt.Errorf("invalid target_addr host")
	}
	if ip.IsLoopback() {
		return nil
	}
	return fmt.Errorf("target_addr must be loopback")
}

func acceptServe(listener net.Listener, target string, s *sidecarService) {
	for {
		conn, err := listener.Accept()
		if err != nil {
			if !errors.Is(err, net.ErrClosed) {
				s.mu.Lock()
				s.serveErr = err.Error()
				s.mu.Unlock()
			}
			return
		}
		go proxyConn(conn, target)
	}
}

func proxyConn(conn net.Conn, target string) {
	defer conn.Close()
	upstream, err := net.Dial("tcp", target)
	if err != nil {
		return
	}
	defer upstream.Close()

	go func() {
		_, _ = io.Copy(upstream, conn)
	}()
	_, _ = io.Copy(conn, upstream)
}

func startGrpcServer(ctx context.Context, controlURL, authKey, hostname string, socksPort int) (*grpc.Server, net.Listener, error) {
	socketPath := strings.TrimSpace(os.Getenv(envGrpcSocket))
	if socketPath == "" {
		return nil, nil, fmt.Errorf("missing %s", envGrpcSocket)
	}
	if err := os.MkdirAll(filepath.Dir(socketPath), 0o755); err != nil {
		return nil, nil, fmt.Errorf("failed to create socket dir: %w", err)
	}
	_ = os.Remove(socketPath)
	listener, err := net.Listen("unix", socketPath)
	if err != nil {
		return nil, nil, fmt.Errorf("failed to listen on socket: %w", err)
	}

	server := grpc.NewServer(grpc.Creds(insecure.NewCredentials()))
	baseCfg := &StartRequest{
		ControlUrl: controlURL,
		AuthKey:    authKey,
		Hostname:   hostname,
		SocksPort:  uint32(socksPort),
	}
	RegisterTsnetServiceServer(server, newSidecarService(baseCfg))
	go func() {
		<-ctx.Done()
		server.GracefulStop()
		_ = listener.Close()
	}()

	go func() {
		if err := server.Serve(listener); err != nil && !errors.Is(err, grpc.ErrServerStopped) {
			log.Printf("gRPC server error: %v", err)
		}
	}()

	return server, listener, nil
}

func bindSocksPort(port int) (net.Listener, int, error) {
	listener, err := net.Listen("tcp4", fmt.Sprintf("127.0.0.1:%d", port))
	if err != nil {
		return nil, 0, fmt.Errorf("failed to bind socks port: %w", err)
	}
	actualPort := listener.Addr().(*net.TCPAddr).Port
	return listener, actualPort, nil
}

func normalizePort(port uint32) (int, error) {
	if port == 0 {
		return 0, nil
	}
	if port > 65535 {
		return 0, fmt.Errorf("invalid socks port: %d", port)
	}
	return int(port), nil
}
