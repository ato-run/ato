# Trusted Runner tooling; the resulting OCI manifest digest is configured at
# deployment. This image is never appended to an application's semantic D/K.
FROM ubuntu:24.04@sha256:52df9b1ee71626e0088f7d400d5c6b5f7bb916f8f0c82b474289a4ece6cf3faf
RUN apt-get update && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends tigervnc-standalone-server x11-utils && rm -rf /var/lib/apt/lists/*
LABEL org.ato.native-display.controls-abi="1"
COPY native-display /usr/local/lib/ato/native-display
RUN chmod 0555 /usr/local/lib/ato/native-display
USER 65532:65532
ENTRYPOINT ["/usr/local/lib/ato/native-display"]
