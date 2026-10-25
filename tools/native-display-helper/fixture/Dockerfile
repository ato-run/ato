FROM ubuntu:24.04@sha256:52df9b1ee71626e0088f7d400d5c6b5f7bb916f8f0c82b474289a4ece6cf3faf AS build
RUN apt-get update && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends gcc libc6-dev libx11-dev && rm -rf /var/lib/apt/lists/*
COPY main.c /src/main.c
RUN gcc -O2 -Wall -Wextra -o /gui /src/main.c -lX11
FROM ubuntu:24.04@sha256:52df9b1ee71626e0088f7d400d5c6b5f7bb916f8f0c82b474289a4ece6cf3faf
RUN apt-get update && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends libx11-6 && rm -rf /var/lib/apt/lists/*
COPY --from=build /gui /usr/local/bin/gui
USER 65532:65532
ENTRYPOINT ["/usr/local/bin/gui"]
