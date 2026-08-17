# Store wrapper: FileBrowser Quantum, anonymous-preview build (no login).
#
# Pattern (reusable for other Store capsules): FROM the upstream image at a
# pinned DIGEST, bake an app-specific config that bypasses interactive login
# for the throwaway free-preview lane, and bake small demo content. No secrets,
# no build steps — the import lane digest-pins the FROM and the no-secret seal
# scan still gates the artifact.
FROM ghcr.io/gtsteffaniak/filebrowser@sha256:2cd949cd06c058576773c5b8400c854478a64c4f364b1e2bea275819847f099b

# noauth config in BOTH locations the image consults (FILEBROWSER_CONFIG points
# at the data/ copy; the app also reads ./config.yaml).
COPY --chown=1000:1000 config.yaml /home/filebrowser/data/config.yaml
COPY --chown=1000:1000 config.yaml /home/filebrowser/config.yaml

# Demo content served at /srv (pair with an ephemeral copy-up mount on /srv so
# anonymous users can also create/edit files that die with the run).
COPY --chown=1000:1000 demo/ /srv/
