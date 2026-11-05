FROM redis@sha256:b8d831cd7faefb56db98e77ad421379ec35f6b5342a8cff9c8cde2cfd02ec837 AS redis
FROM fleetbase/fleetbase-socket@sha256:2becf68e5781214797a5fea0c66c0cc0e0caf0df632c95edf12bb63da1017bff AS socket
FROM mysql@sha256:213bbfaf699693a40a20a12bb4342d2589a15a3dc7153db698eaed252a92458e AS mysql
FROM fleetbase/fleetbase-api@sha256:ccd1f9d4ee93625edd9f9aa446e8655cbc2ffbe7c249387bdf52169967933572
COPY --from=mysql /usr/sbin/mysqld /usr/sbin/mysqld
COPY --from=mysql /usr/lib64/mysql /usr/lib64/mysql
COPY --from=mysql /usr/share/mysql-8.0 /usr/share/mysql-8.0
COPY --from=mysql /lib64/libaio.so.1.0.1 /usr/lib/aarch64-linux-gnu/libaio.so.1
RUN ldd /usr/sbin/mysqld && /usr/sbin/mysqld --version
RUN mkdir -p /opt/ato-seed && cp -a /fleetbase/api/storage /opt/ato-seed/storage && cp -a /fleetbase/api/bootstrap/cache /opt/ato-seed/cache && rm -rf /fleetbase/api/storage /fleetbase/api/bootstrap/cache && ln -s /data/fleetbase/storage /fleetbase/api/storage && ln -s /data/fleetbase/cache /fleetbase/api/bootstrap/cache && printf '%s\n' 'pdo_mysql.default_socket=/data/mysql.sock' > /usr/local/etc/php/conf.d/zz-ato-local-proof.ini
RUN rm -rf /fleetbase/console && ln -s /data/fleetbase/console /fleetbase/console && ln -s /data/fleetbase/home/composer-auth.json /fleetbase/api/auth.json
COPY ato_origin.py /opt/ato_origin.py
COPY supervisor.py /opt/ato-supervisor.py
ENTRYPOINT ["python3", "/opt/ato-supervisor.py"]

COPY --from=socket /usr/src/app /opt/fleetbase-socket
COPY --from=socket /usr/local/bin/node /opt/fleetbase-socket/node
COPY --from=socket /lib/ld-musl-aarch64.so.1 /lib/ld-musl-aarch64.so.1
COPY --from=socket /usr/lib/libstdc++.so.6 /opt/socket-libs/libstdc++.so.6
COPY --from=socket /usr/lib/libgcc_s.so.1 /opt/socket-libs/libgcc_s.so.1

RUN cp /fleetbase/api/vendor/laravel/octane/src/Commands/stubs/frankenphp-worker.php /fleetbase/api/public/frankenphp-worker.php
RUN setcap -r /usr/local/bin/frankenphp && test -z "$(getcap /usr/local/bin/frankenphp)"
COPY NotPwnedVerifier-ato.php /fleetbase/api/vendor/laravel/framework/src/Illuminate/Validation/NotPwnedVerifier.php
RUN php -l /fleetbase/api/vendor/laravel/framework/src/Illuminate/Validation/NotPwnedVerifier.php

COPY --from=redis /usr/local/bin/redis-server /opt/redis/redis-server
COPY --from=redis /usr/lib/libzstd.so.1 /opt/redis-libs/libzstd.so.1
COPY --from=redis /usr/lib/libssl.so.3 /opt/redis-libs/libssl.so.3
COPY --from=redis /usr/lib/libcrypto.so.3 /opt/redis-libs/libcrypto.so.3

# The upstream image probes a Caddy admin endpoint which this supervisor does
# not run. Probe the actual API and database instead; Ato's own readiness and
# Contract observations remain separate and must still pass.
HEALTHCHECK --interval=15s --timeout=5s --start-period=300s --retries=3 CMD curl --fail --silent --max-time 3 http://127.0.0.1:8000/int/v1/onboard/should-onboard >/dev/null || exit 1
