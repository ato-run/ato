# tests/manual — Pre-Release Manual Test Suite

15 test suites covering the manual verification categories required before an `ato` release. Each suite combines **automated assertions** (run immediately) and **human-in-the-loop checklists** (prompt tester for pass/fail/skip).

## Structure

```
tests/manual/
├── config.sh                    # shared utilities (sourced by all suites)
├── run-all.sh                   # run all 15 suites in sequence
├── results/                     # per-suite log files (gitignored)
├── 01-install-upgrade/          # §1  インストール / アップグレード経路
├── 02-gpu-accelerator/          # §2  実機での GPU / アクセラレータ系
├── 03-first-run-download/       # §3  5GB 級モデルの初回ダウンロード UX
├── 04-sandbox-boundary/         # §4  サンドボックス境界の実測
├── 05-cross-os/                 # §5  クロス OS の挙動差検証
├── 06-share-url/                # §6  Share URL の実 URL 配布フロー
├── 07-ato-desktop-ux/           # §7  ato-desktop の実 UX
├── 08-trust-ux/                 # §8  Trust UX の実体験
├── 09-network-isolation/        # §9  ネットワーク隔離の実観測
├── 10-error-messages/           # §10 エラーメッセージとデバッグ体験
├── 11-ato-api/                  # §11 ato-api の実運用テスト
├── 12-toolchain-interference/   # §12 既存ツールチェーンとの干渉
├── 13-longtail-envs/            # §13 ロングテールの環境
├── 14-doc-alignment/            # §14 ドキュメンテーションとの整合
└── 15-dogfooding/               # §15 リリース前 dogfooding
```

## Prerequisites

- `ato` installed and in `$PATH`
- `curl` available
- `python3` available (for sandbox probe scripts)
- Internet access (for store/share URL tests)
- For §2 (GPU tests): machine with Metal/CUDA/ROCm hardware

## Running

```bash
# All suites
./tests/manual/run-all.sh

# Single suite
bash tests/manual/04-sandbox-boundary/test.sh

# Start from suite 6 (e.g., resume after interruption)
./tests/manual/run-all.sh --from 6

# Run only suite 10
./tests/manual/run-all.sh --only 10
```

## Human Checklist Prompts

When a test reaches a section requiring human judgment, it prints numbered steps and prompts:

```
  [1] Open the share URL on a different machine
  [2] Confirm ato runs the capsule without extra steps
  ...
  Pass [p], Fail [f], Skip [s]?
```

- `p` — all steps completed successfully
- `f` — one or more steps failed (will be counted as a failure)
- `s` — skipped (hardware unavailable, out of scope for this run)

## Results

Each suite writes a log to `tests/manual/results/result_NN_<name>.log`. The log records every `PASS:`, `FAIL:`, and `SKIP:` line. `run-all.sh` aggregates these into a combined summary at the end.

## Priority

Per the specification, **§3 (5GB first-run download)** and **§6 (share URL on another machine)** are release blockers. These two suites must be verified with at least 10 external testers before release.

| Suite | Category | Priority | Hardware Required |
|-------|----------|----------|-------------------|
| 03 | First-run download | 🔴 P0 | Any |
| 06 | Share URL | 🔴 P0 | Any + second machine |
| 04 | Sandbox boundary | 🔴 P0 | Any |
| 01 | Install / upgrade | 🟠 P1 | Fresh machine |
| 02 | GPU | 🟠 P1 | GPU hardware |
| 07 | Desktop UX | 🟠 P1 | macOS/Windows |
| 10 | Error messages | 🟠 P1 | Any |
| 05 | Cross-OS | 🟡 P2 | 3 OS machines |
| 08 | Trust UX | 🟡 P2 | Any |
| 09 | Network isolation | 🟡 P2 | Any + tcpdump |
| 11 | Store ops | 🟡 P2 | Publish access |
| 12 | Toolchain interference | 🟡 P2 | nvm/pyenv etc |
| 13 | Long-tail envs | 🟢 P3 | NixOS/ARM etc |
| 14 | Doc alignment | 🟢 P3 | Any |
| 15 | Dogfooding | 🟢 P3 | Whole team |

## Relationship with samples/

These two directories serve different purposes and must not be conflated:

| Directory | Purpose | Lifetime |
|---|---|---|
| `samples/` | What `ato` **can** do (demonstrations, permanent docs) | Permanent |
| `samples/03-limitations/` | What `ato` **does not do by design** (permanent known gaps) | Permanent |
| `tests/manual/` | Pre-release verification of v0.5 readiness | Ephemeral (per-release) |

A test here failing **does not automatically** mean a sample belongs in `samples/03-limitations/`.
Only move to `samples/03-limitations/` if the gap is **permanent and by design**.

- Gap is "deferred to v0.5.1" → keep in `tests/manual/` as a SKIP or FAIL
- Gap is "by design, won't fix" → add to `samples/03-limitations/`

## Temp Files

All temporary capsule directories are created under `tests/.tmp/manual-tests/` and cleaned up after each test. Never writes to `/tmp`.
