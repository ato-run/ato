// Adapter-owned stdio/TCP bridge. No shell, filesystem access or remote host.
package main

import (
	"io"
	"net"
	"os"
	"strconv"
	"time"
)

func main() {
	if len(os.Args) != 2 { os.Exit(2) }
	port, err := strconv.Atoi(os.Args[1])
	if err != nil || port < 1 || port > 65535 { os.Exit(2) }
	conn, err := net.DialTimeout("tcp", net.JoinHostPort("127.0.0.1", strconv.Itoa(port)), 2*time.Second)
	if err != nil { os.Exit(1) }
	defer conn.Close()
	go func() {
		io.Copy(conn, os.Stdin)
		if tcp, ok := conn.(*net.TCPConn); ok { tcp.CloseWrite() }
	}()
	io.Copy(os.Stdout, conn)
}
