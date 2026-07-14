FROM docker.io/library/ubuntu:24.04@sha256:52df9b1ee71626e0088f7d400d5c6b5f7bb916f8f0c82b474289a4ece6cf3faf

ENV DEBIAN_FRONTEND=noninteractive

RUN apt-get update \
    && apt-get install --yes --no-install-recommends \
        openbox \
        python3 \
        x11-apps \
        x11-utils \
        x11-xkb-utils \
        x11vnc \
        xdotool \
        xterm \
        xvfb \
    && rm -rf /var/lib/apt/lists/*

COPY start-pixel-fixture.sh health.py terminal.sh /opt/ato-pixel-fixture/
RUN chmod 0755 \
    /opt/ato-pixel-fixture/start-pixel-fixture.sh \
    /opt/ato-pixel-fixture/health.py \
    /opt/ato-pixel-fixture/terminal.sh

EXPOSE 8080 5900

ENTRYPOINT ["/opt/ato-pixel-fixture/start-pixel-fixture.sh"]
