/* Controlled POSIX negative fixture. No credentials, Keychain, or agents used. */
#include <arpa/inet.h>
#include <errno.h>
#include <fcntl.h>
#include <stdbool.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/un.h>
#include <sys/wait.h>
#include <unistd.h>

static bool blocked(void) { return errno == EPERM || errno == EACCES; }

static bool denied_read(const char *path) {
  int fd = open(path, O_RDONLY);
  if (fd >= 0) { close(fd); return false; }
  return blocked();
}

static bool denied_write(const char *path) {
  int fd = open(path, O_WRONLY);
  if (fd >= 0) { close(fd); return false; }
  return blocked();
}

static bool denied_exec(const char *path) {
  pid_t child = fork();
  if (child < 0) return false;
  if (!child) {
    execl(path, path, (char *)NULL);
    _exit(blocked() ? 0 : 1);
  }
  int status = 0;
  return waitpid(child, &status, 0) == child && WIFEXITED(status) && WEXITSTATUS(status) == 0;
}

static bool inherited_denial(const char *path) {
  pid_t child = fork();
  if (child < 0) return false;
  if (!child) _exit(denied_read(path) ? 0 : 1);
  int status = 0;
  return waitpid(child, &status, 0) == child && WIFEXITED(status) && WEXITSTATUS(status) == 0;
}

static bool denied_unix(const char *path) {
  int fd = socket(AF_UNIX, SOCK_STREAM, 0);
  if (fd < 0) return blocked();
  struct sockaddr_un address = {0};
  address.sun_family = AF_UNIX;
  if (strlen(path) >= sizeof(address.sun_path)) { close(fd); return false; }
  strcpy(address.sun_path, path);
  int result = connect(fd, (struct sockaddr *)&address, sizeof(address));
  bool denied = result < 0 && blocked();
  close(fd);
  return denied;
}

static bool tcp_connect(const char *host, int port, bool expected_allowed) {
  int fd = socket(AF_INET, SOCK_STREAM, 0);
  if (fd < 0) return !expected_allowed && blocked();
  struct sockaddr_in address = {0};
  address.sin_family = AF_INET;
  address.sin_port = htons((unsigned short)port);
  inet_pton(AF_INET, host, &address.sin_addr);
  int result = connect(fd, (struct sockaddr *)&address, sizeof(address));
  bool passed = expected_allowed ? result == 0 : result < 0 && blocked();
  close(fd);
  return passed;
}

static bool denied_udp(int port) {
  int fd = socket(AF_INET, SOCK_DGRAM, 0);
  if (fd < 0) return blocked();
  struct sockaddr_in address = {0};
  address.sin_family = AF_INET;
  address.sin_port = htons((unsigned short)port);
  inet_pton(AF_INET, "127.0.0.1", &address.sin_addr);
  int result = (int)sendto(fd, "probe", 5, 0, (struct sockaddr *)&address, sizeof(address));
  bool denied = result < 0 && blocked();
  close(fd);
  return denied;
}

static bool denied_ipv6(int port) {
  int fd = socket(AF_INET6, SOCK_STREAM, 0);
  if (fd < 0) return blocked();
  struct sockaddr_in6 address = {0};
  address.sin6_family = AF_INET6;
  address.sin6_port = htons((unsigned short)port);
  inet_pton(AF_INET6, "::1", &address.sin6_addr);
  int result = connect(fd, (struct sockaddr *)&address, sizeof(address));
  bool denied = result < 0 && blocked();
  close(fd);
  return denied;
}

int main(int argc, char **argv) {
  if (argc != 11) return 2;
  bool no_extra_descriptors = true;
  for (int fd = 3; fd < 1024; fd++) {
    if (fcntl(fd, F_GETFD) >= 0 || errno != EBADF) no_extra_descriptors = false;
  }
  const char *names[] = {"source", "owner-credential", "owner-connection", "private-grant", "runtime-ticket",
                         "state-db", "old-measurements", "credential-store-file"};
  char path[4096];
  bool private_read = true, private_write = true;
  for (size_t i = 0; i < sizeof(names) / sizeof(names[0]); i++) {
    if (snprintf(path, sizeof(path), "%s/%s", argv[2], names[i]) >= (int)sizeof(path)) return 2;
    private_read = denied_read(path) && private_read;
    private_write = denied_write(path) && private_write;
  }
  int fd = open(argv[1], O_RDONLY);
  bool public_read = fd >= 0;
  if (fd >= 0) close(fd);
  bool public_write = denied_write(argv[1]);
  if (snprintf(path, sizeof(path), "%s/probe-scratch", argv[3]) >= (int)sizeof(path)) return 2;
  fd = open(path, O_WRONLY | O_CREAT | O_EXCL, 0600);
  bool scratch_write = fd >= 0;
  if (fd >= 0) { close(fd); unlink(path); }
  bool escape_denied = denied_read(argv[4]);
  if (snprintf(path, sizeof(path), "%s/source", argv[2]) >= (int)sizeof(path)) return 2;
  bool inherited = inherited_denial(path);
  if (snprintf(path, sizeof(path), "%s/direct-launch", argv[2]) >= (int)sizeof(path)) return 2;
  bool direct = denied_exec(path);
  bool shell = denied_exec("/bin/sh");
  bool security = denied_exec("/usr/bin/security");
  bool curl = denied_exec("/usr/bin/curl");
  bool unix_denied = denied_unix(argv[5]);
  bool relay_allowed = tcp_connect("127.0.0.1", atoi(argv[6]), true);
  bool provider_allowed = tcp_connect("127.0.0.1", atoi(argv[7]), true);
  bool wrong_port = tcp_connect("127.0.0.1", atoi(argv[8]), false);
  bool wrong_address = denied_ipv6(atoi(argv[6]));
  bool provider_wrong_port = tcp_connect("127.0.0.1", atoi(argv[9]), false);
  bool provider_wrong_address = denied_ipv6(atoi(argv[7]));
  bool owner_unix = denied_unix(argv[10]);
  bool secondary_ipv4 = tcp_connect("127.0.0.2", atoi(argv[6]), false);
  bool udp = denied_udp(atoi(argv[6]));
#define JSON_BOOL(value) ((value) ? "true" : "false")
  printf("{\"public_skill_read\":%s,\"public_skill_write_denied\":%s,"
         "\"private_eight_classes_read_denied\":%s,\"private_eight_classes_write_denied\":%s,"
         "\"scratch_write\":%s,\"symlink_escape_denied\":%s,\"fork_inherits_denial\":%s,"
         "\"source_direct_launch_denied\":%s,\"shell_exec_denied\":%s,"
         "\"security_exec_denied\":%s,\"curl_exec_denied\":%s,"
         "\"docker_unix_socket_denied\":%s,\"relay_tcp_allowed\":%s,"
         "\"provider_fixture_tcp_allowed\":%s,\"relay_wrong_port_denied\":%s,"
         "\"relay_wrong_address_denied\":%s,\"provider_wrong_port_denied\":%s,"
         "\"provider_wrong_address_denied\":%s,\"owner_unix_socket_denied\":%s,"
         "\"ipv4_secondary_denied\":%s,\"udp_denied\":%s,\"extra_descriptors_absent\":%s}\n",
         JSON_BOOL(public_read), JSON_BOOL(public_write), JSON_BOOL(private_read),
         JSON_BOOL(private_write), JSON_BOOL(scratch_write), JSON_BOOL(escape_denied),
         JSON_BOOL(inherited), JSON_BOOL(direct), JSON_BOOL(shell), JSON_BOOL(security),
         JSON_BOOL(curl), JSON_BOOL(unix_denied), JSON_BOOL(relay_allowed),
         JSON_BOOL(provider_allowed), JSON_BOOL(wrong_port), JSON_BOOL(wrong_address),
         JSON_BOOL(provider_wrong_port), JSON_BOOL(provider_wrong_address),
         JSON_BOOL(owner_unix), JSON_BOOL(secondary_ipv4), JSON_BOOL(udp), JSON_BOOL(no_extra_descriptors));
  return 0;
}
