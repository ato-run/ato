# Wan Animate (Animation / Replace)

Source: `samples/portable-wan-animate`. This file is outside the bundle so it can name the bundle pins without changing them.

A separate app bundle from `samples/portable-wan-animation`. The accepted
Animation bundle, Contract, Derivation, Model Sets and workflow are not
modified; this app adds an explicit Replace workflow to a new immutable
bundle.

## Modes

| Mode | Preparation path | Output |
| --- | --- | --- |
| Animation | `POST /generate/animation` | `animation.mp4` |
| Replace | `POST /generate/replace` | `replace.mp4` |

Both modes take the same two inputs (`image.jpeg`, `video.mp4`). Replace keeps
the source video's background and mask; Animation removes them. The health
Contract is unchanged (`/health` returns `wan-animation-ready`).

## Pins (built 2026-10-20 with `ato pack`)

- bundle SHA-256: `sha256:776f16463929ec7d72925537b3f0097cdd34b69c3ef06857950492caebac6aa6`
- ContractRef: `sha256:e2ccef685f593b9f44772e467b86d6b3ff4daf9dd433ec0217a50c210e0a2fcb`
- DerivationRef: `sha256:17288d7b6964e71cbd506f93c97cc7980cc757f175f4e3984663ee393d41de3e`

## Verification

```sh
python3 -m unittest -v test_workflow.py
```

This checks the mode wiring only. It is not a GPU acceptance and does not
prove Replace generation on a real device. Do not import or run it on a paid
GPU without the separate approval described in TODO #112.
