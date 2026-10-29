# Pandoc (Terminal)

Pandoc in a hosted Terminal: each command runs once and its output and exit
status stay on screen. Files written to the output directory are saved after
the Run stops.

| | |
|---|---|
| Program | Pandoc 3.7.0.2 (`pandoc/core`, Alpine, BusyBox `sh`) |
| Image | `docker.io/pandoc/core@sha256:388e8ed4…7ec9` (linux/amd64) |
| Licenses | Pandoc: GPL-2.0-or-later (source: https://github.com/jgm/pandoc). Image build: https://github.com/pandoc/dockerfiles. This directory: MPL-2.0 |
| Limits | 512 MiB memory, 1 CPU, 64 pids, no network, read-only root filesystem, UID 65532 |

The image is pulled by digest from Docker Hub; the Capsule carries only
`launch.sh`, `example.md`, this README and `capsule.toml`.

## Usage

The shell starts in `ATO_OUTPUT_DIR` when the Run has an output grant, and
otherwise in `/tmp` (a 64 MiB tmpfs that disappears when the Run ends). Hosted
OCI Runners grant outputs only on host-boundary Runners today, so on a shared
Runner use `/tmp` for intermediate files and print results to the Terminal:

```sh
pandoc /app/example.md -s -o example.html
pandoc /app/example.md -o example.docx
echo $?
```

The shell runs inside the Run's container only. It has no network and no
access to the host.

## Contract

The observation waits for the prompt (`probe_ready_output`, `pandoc$ ` followed
by BusyBox's cursor-position query `ESC[6n`), then runs
`printf '# Hello\n' | pandoc -t html; echo "E$?D"`. It ends at `E0D` and the
next prompt, so K covers Pandoc's actual HTML and exit status 0:
`sha256:fedfeb15effefa5859ae948bd64de35fe4c20c2fab4ae2328b7cb8e44586c452`.
