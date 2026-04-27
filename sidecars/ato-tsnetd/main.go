package main

import (
	"context"
	"flag"
	"fmt"
	"log"
	"os"
	"os/signal"
	"strconv"
	"strings"
	"syscall"
)

const (
	envControlURL = "ATO_TSNET_CONTROL_URL"
	envAuthKey    = "ATO_TSNET_AUTH_KEY"
	envHostname   = "ATO_TSNET_HOSTNAME"
	envSocksPort  = "ATO_TSNET_SOCKS_PORT"
	envGrpcSocket = "ATO_TSNET_GRPC_SOCKET"
)

type stringFlag struct {
	value string
	set   bool
}

func (s *stringFlag) String() string {
	return s.value
}

func (s *stringFlag) Set(value string) error {
	s.value = value
	s.set = true
	return nil
}

type intFlag struct {
	value int
	set   bool
}

func (i *intFlag) String() string {
	return strconv.Itoa(i.value)
}

func (i *intFlag) Set(value string) error {
	parsed, err := strconv.Atoi(value)
	if err != nil {
		return err
	}
	i.value = parsed
	i.set = true
	return nil
}

func main() {
	controlURLFlag := &stringFlag{}
	authKeyFlag := &stringFlag{}
	hostnameFlag := &stringFlag{}
	socksPortFlag := &intFlag{value: 0}

	flag.Var(controlURLFlag, "control-url", "Tailscale control URL")
	flag.Var(authKeyFlag, "auth-key", "Tailscale auth key")
	flag.Var(hostnameFlag, "hostname", "Tailnet hostname")
	flag.Var(socksPortFlag, "socks-port", "SOCKS5 listen port (0 = auto)")
	flag.Parse()

	controlURL := resolveOptionalString(controlURLFlag, envControlURL)
	authKey := resolveOptionalString(authKeyFlag, envAuthKey)
	hostname := resolveOptionalString(hostnameFlag, envHostname)

	socksPort, err := resolvePort(socksPortFlag, envSocksPort)
	if err != nil {
		exitWithError(err)
	}

	ctx, stop := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer stop()

	server, listener, err := startGrpcServer(ctx, controlURL, authKey, hostname, socksPort)
	if err != nil {
		exitWithError(err)
	}
	defer server.GracefulStop()
	defer listener.Close()

	log.Printf("ato-tsnetd gRPC listening socket=%s", listener.Addr().String())

	<-ctx.Done()
}

func resolveOptionalString(flagValue *stringFlag, envKey string) string {
	value := strings.TrimSpace(flagValue.value)
	if !flagValue.set {
		value = strings.TrimSpace(os.Getenv(envKey))
	}
	return value
}

func resolvePort(flagValue *intFlag, envKey string) (int, error) {
	port := flagValue.value
	if !flagValue.set {
		envValue := strings.TrimSpace(os.Getenv(envKey))
		if envValue != "" {
			parsed, err := strconv.Atoi(envValue)
			if err != nil {
				return 0, fmt.Errorf("invalid %s: %w", envKey, err)
			}
			port = parsed
		}
	}
	if port != 0 && (port < 1 || port > 65535) {
		return 0, fmt.Errorf("invalid socks port: %d", port)
	}
	return port, nil
}

func exitWithError(err error) {
	fmt.Fprintln(os.Stderr, err)
	os.Exit(1)
}
