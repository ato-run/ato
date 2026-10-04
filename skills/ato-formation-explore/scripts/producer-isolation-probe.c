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

static bool denied_tcp(int port) {
  int fd = socket(AF_INET, SOCK_STREAM, 0);
  if (fd < 0) return blocked();
  struct sockaddr_in address = {0};
  address.sin_family = AF_INET;
  address.sin_port = htons((unsigned short)port);
  inet_pton(AF_INET, "127.0.0.1", &address.sin_addr);
  int result = connect(fd, (struct sockaddr *)&address, sizeof(address));
  bool denied = result < 0 && blocked();
  close(fd);
  return denied;
}

static bool allowed_unix(const char *path) {
  int fd = socket(AF_UNIX, SOCK_STREAM, 0);
  if (fd < 0) return false;
  struct sockaddr_un address = {0};
  address.sun_family = AF_UNIX;
  if (strlen(path) >= sizeof(address.sun_path)) { close(fd); return false; }
  strcpy(address.sun_path, path);
  bool allowed = connect(fd, (struct sockaddr *)&address, sizeof(address)) == 0;
  close(fd);
  return allowed;
}

int main(int argc, char **argv) {
  if (argc != 7 && argc != 8) return 2;
  const char *names[] = {"source", "owner-credential", "session-capability", "private-grant", "runtime-ticket",
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
  bool tcp_denied = denied_tcp(atoi(argv[6]));
#define JSON_BOOL(value) ((value) ? "true" : "false")
  printf("{\"public_skill_read\":%s,\"public_skill_write_denied\":%s,"
         "\"private_seven_classes_read_denied\":%s,\"private_seven_classes_write_denied\":%s,"
         "\"scratch_write\":%s,\"symlink_escape_denied\":%s,\"fork_inherits_denial\":%s,"
         "\"source_direct_launch_denied\":%s,\"shell_exec_denied\":%s,"
         "\"security_exec_denied\":%s,\"curl_exec_denied\":%s,"
         "\"docker_unix_socket_denied\":%s,\"arbitrary_tcp_denied\":%s",
         JSON_BOOL(public_read), JSON_BOOL(public_write), JSON_BOOL(private_read),
         JSON_BOOL(private_write), JSON_BOOL(scratch_write), JSON_BOOL(escape_denied),
         JSON_BOOL(inherited), JSON_BOOL(direct), JSON_BOOL(shell), JSON_BOOL(security),
         JSON_BOOL(curl), JSON_BOOL(unix_denied), JSON_BOOL(tcp_denied));
  if (argc == 8) printf(",\"selected_relay_unix_socket_connected\":%s", JSON_BOOL(allowed_unix(argv[7])));
  printf("}\n");
  return 0;
}
