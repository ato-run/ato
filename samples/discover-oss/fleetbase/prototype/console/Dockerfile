ARG UPSTREAM_PLATFORM=linux/amd64
FROM --platform=$UPSTREAM_PLATFORM fleetbase/fleetbase-console@sha256:2212ecf41247d0cbc9aef4fa7f9d790943e95392d47422ec68c5164b8f4a2286 AS assets
FROM nginx@sha256:41c8801e327f9c60c47aa700390ea5cf1862a6852bd27d103ba2cac41a7d6ec7
COPY --from=assets /usr/share/nginx/html /usr/share/nginx/html
COPY ato.conf /etc/nginx/ato.conf
COPY index-ato.html /usr/share/nginx/html/index.html
COPY assets/ato-*.js /usr/share/nginx/html/assets/
USER 65532:65532
ENTRYPOINT ["nginx", "-c", "/etc/nginx/ato.conf", "-g", "daemon off;"]
