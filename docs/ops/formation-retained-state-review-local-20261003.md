# Formation retained state — 分離したレビュー単位の検証

測定ID `formation-retained-state-review-local-20261003-01`。実装pin
`c8761b01dadf0dd5c816857dd53aa343dfbc3276`、macOS arm64、Rust 1.96.0。
実測時のSourceはこの分離worktree。後続の記録commitへpinを付け替えない。

## 変更と依存

既存Connected WorkerのStateArtifactTransport、writer/session、volume処理を
共通Runtime attemptへ移し、元の入口はre-exportを保つ。Rust authorityは保存済み
Search/submissionと同じSource/K/D/artifactから登録と機能受入を投影する。
論理slot `app.data`はDを変更せず、衝突しない物理StateService keyへ再結合する。

機能受入はOwnerが別途承認したfresh Search/Runだけへ割り当てる。宣言とscope、
同じInstanceのwriter fence、restore、停止後commit、保存済みHTTP操作のmetadata
照合を共通経路で行う。durable finishを機能操作とcommitの後まで遅延し、切断時の
UNKNOWNから処理を再実行しない。探索PASSから通常Run許可は付与しない。

baseはAto Draft #1476。API取消 #730、後続のSource/state登録APIが別単位。
Windows portabilityと既存formatter修正はAto #1478。Session #1479・Skill #1480は
別単位であり、この状態経路を実エージェント測定として計上しない。

## 検証

| 検証 | 結果 |
|---|---|
| Formation functional acceptance | 4 PASS。明示上限、binding競合、logical key、欠損/不一致の保存済み操作証拠。 |
| Formation HTTP operation境界 | 10 PASS。旧bytes、advanced機能の限定、権限/型/capture制限。 |
| Formation Worker library | 82 PASS。独立API、旧Session、durable disconnect、state capabilityを含む。 |
| Formation/HTTP Adapter/Runtime attempt/Worker/receipt authority all-target strict clippy | PASS。 |
| clean SourceからのWASM再build | API側の別記録。旧buildと同一SHA-256 `33ce99ba10a381944177ea93d89ad3596da05f750114ee7d73c89fffccc9a861`、2,511,227 bytes。 |

Worker検証の最初のリンクは、このworktreeのTMPDIRが未作成だったため失敗した。
自分の`.tmp`を作成した再検証が上の82 PASSであり、コード回帰や実機Runtime成功に
読み替えない。Cargo cacheはworkspace内の既存targetを使用し、Sourceは混入させない。

## 未測定

実Kuttの今回探索artifact・changedetection.ioの機能/保存/新Run復元、PWA入力/resume、
実Codex/Claude Code探索は未実行。入力の完全非露出も実製品経路では未証明。
準備済み機能計画はAPI `docs/ops/formation-v0/functional-acceptance-proposal-20261003.*`
で管理し、未知のRuntime/Instance/descriptor gateと実行承認を残す。

独立有料API call、Search/Runtime実受入、remote migration、配備、flag変更、通常Run許可は0。
旧測定、失敗Search、UNKNOWN、receipt、予算台帳は変更していない。
