# ARM64 synthetic-only experiment. The base is a separately built local image;
# its identity and the prerequisites are documented in the parent README.
FROM mariadb@sha256:f1c56f035ed268a0d7c2c2e06dc879f6319b997cb434cf822a4cfd72a7aa184d AS database
FROM beveren-fsm-local-proof:20261106
USER root
COPY --from=database /usr/sbin/mariadbd /usr/sbin/mariadbd
COPY --from=database /usr/bin/mariadb-install-db /usr/bin/mariadb-install-db
COPY --from=database /usr/bin/mariadb /usr/bin/mariadb
COPY --from=database /usr/bin/resolveip /usr/bin/resolveip
COPY --from=database /usr/lib/mysql/plugin /usr/lib/mysql/plugin
COPY --from=database /usr/share/mysql /usr/share/mysql
COPY --from=database /usr/lib/aarch64-linux-gnu/liburing.so.2.1.0 /usr/lib/aarch64-linux-gnu/liburing.so.2
RUN set -eu; result="$(ldd /usr/sbin/mariadbd)"; printf '%s\n' "$result"; \
    ! printf '%s\n' "$result" | grep -q 'not found'; \
    /usr/sbin/mariadbd --version; /usr/bin/mariadb --version
USER frappe
