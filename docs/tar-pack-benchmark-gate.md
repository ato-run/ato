# Tar Pack Benchmark Gate

## Purpose

`runtime=source` の `.capsule` 生成で、最適化PRの採否をデータ駆動で判断するための計測ゲートです。  
このドキュメントは「まず計測、次に最適化」を徹底するための基準を定義します。

## Benchmark Scenario

- Dataset A: 10,000 files (`1KB/file`)
- Dataset B: 100,000 files (`1KB/file`)
- Command:
  `scripts/ci/tar_pack_benchmark.sh`

スクリプトは以下を出力します。

- `pack_elapsed_ms` (Rust側で計測した pack 処理時間)
- `total_elapsed_ms` (データ生成を含む総時間)
- `peak_rss_kb` (`/usr/bin/time -v` の最大RSS)
- `wall_clock` (`/usr/bin/time -v` の経過時間)

Artifacts are written under `/tmp/tar-pack-benchmark`.

## CI Integration

GitHub Actions `build-multi-os.yml` の `tar_pack_benchmark` ジョブで、

1. `FILES=10000`
2. `FILES=100000`

の2ケースを実行し、結果を artifact として保存します。

## Optimization Acceptance Criteria

Tarストリーマー最適化PRは、以下の両方を満たす場合のみ採択します（Dataset B基準）。

1. `pack_elapsed_ms` がベースライン比で **30%以上改善**。
2. `peak_rss_kb` の悪化が **15%以内**。

上記を満たさない場合、複雑性増加は見合わないため棄却します。

## Local Run

```bash
FILES=10000 scripts/ci/tar_pack_benchmark.sh
FILES=100000 scripts/ci/tar_pack_benchmark.sh
```

必要に応じてローカルでしきい値チェックを有効化できます。

```bash
FILES=100000 MAX_PACK_ELAPSED_MS=120000 MAX_PEAK_RSS_KB=1500000 scripts/ci/tar_pack_benchmark.sh
```
