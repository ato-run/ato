# Ato Development Roadmap — Remaining Work

- **最終確認日:** 2026-09-01
- **調査対象:** accepted RFC、`nightly` (`d1acd3dd`)、GitHub open issues 121件

本書は、Ato runtime の残作業を実装可能な単位で管理するロードマップである。
GitHub issue の一覧を複製するのではなく、どの成果をどの順序で完成させるかを示す。
個別 issue の状態と優先度は GitHub を正とし、本書は依存関係、完了条件、issue 間の
意味的な整合性を管理する。

## 1. 判断基準

すべてのタスクは、次の3軸を混同せずに判断する。

1. **What exists / runs now** — 現在のコードと deployment configuration
2. **What behavior is normative** — `docs/rfcs/accepted/` の現行契約
3. **Where the architecture is heading** — Computation-first / Web-first の design direction

code と accepted RFC が矛盾する場合、code を仕様として正当化しない。`Detected
mismatch` として、実装を直すか RFC を改訂するかを先に決める。draft RFC や既存
issue の記述は、accepted RFC と整合するまで実装根拠にしない。

### 優先度

| 優先度 | 意味 |
|---|---|
| **P0** | 後続実装の意味を変える仕様不一致、security / data-loss、または release blocker |
| **P1** | 現行 product loop と主要ユースケースを成立させる作業 |
| **P2** | 対応範囲、互換性、運用品質を広げる作業 |
| **P3** | 需要と前提を検証してから着手する探索作業 |

### タスクの状態

- `[ ]` 未着手または未完了
- `[~]` 進行中。必ず追跡 issue または branch を併記する
- `[x]` 完了。完了条件を満たす test / receipt / RFC へのリンクを残す
- `NEW` はまだ専用 issue がない項目。実装開始前に issue を作成する

## 2. 現在地

### Normative model

- Computation は `ComputationObject { semantics, boundary, residual }` であり、
  `ComputationRef` が canonical identity である。
- Capsule は seal された immutable な Computation 地点、Run は mutable cursor、
  Record は Evolution の evidence である。
- Protocol は logical interaction、Adapter は physical interaction、Materializer は
  physical realization を所有する。これらを Kernel の domain enum に畳み込まない。
- Composition は Computation に閉じ、placement、service graph、application category を
  semantic root にしない。

### Adapter implementation

| Adapter | 実装 | 現在の主用途 | 主な残課題 |
|---|---|---|---|
| Process | `ato.process@1` | process lifecycle / process-tree ownership | cross-platform lifecycle、終了 evidence、conformance |
| PTY | `ato.pty@1` | input / resize / signal / output evidence | PTY と pipe の意味差、backpressure、platform parity |
| Workspace | `ato.workspace@1` | workspace mutation / capture / restore | observation coverage、large object、credential exclusion |
| Binding | `ato.binding@1` | logical binding attach / replace / detach | lease、expiry、execution scope、secret non-persistence |
| HTTP | `ato.http@1` | inbound request Evolution / response evidence | protocol limits、streaming、disconnect、redaction |
| Browser | `ato.browser@1` | browser input / WebMCP operations | production相当Acceptance、SLO receipt、pointer semantics |

Process、PTY、Workspace、Binding、HTTP は accepted `PROTOCOL_ADAPTER.md` の built-in
v1。Browser は built-in に昇格させず、draft
`BROWSER_PROTOCOL_ADAPTER_EXTENSION_V0.md` が experimental extension の契約を
所有する。accepted `PROTOCOL_ADAPTER.md` はこの位置づけを明記している。

### Browser Activity release-critical vertical slice

対象は Public Activity URL → Browser computation参加 → 複数actor操作 → final head / Record
まで。PTY完全化、Composition一般化、新Adapter、全platform conformanceはこのgateに
含めない。

| Phase | 状態 | 実装済み / 残アクション |
|---|---|---|
| Contract | [x] | experimental role、authoritative ingress、evidence-only presentation、actor provenance、Runner ordering、apply/replay、security、unsupported behaviorをdraft RFCに固定 |
| Multi-actor | [x] | host＋participant A＋participant B、全operation provenance、duplicate operation、release後拒否、rebind identity、Runner receipt順序、single final head / applied-only Recordを自動検証 |
| ACK / timeout | [x] | duplicate ACKとlate ACKを冪等化。未知ACK、sequence mismatch、hostile page timeoutはfail closed |
| Transport security | [x] | Browser channelをactivity_id / run_id / epoch / expiryへ束縛し、command/ACKにmonotonic sequenceを付与。cross-Activity / cross-Run / stale epoch / expired / replay / future sequenceをnegative test |
| Presentation | [x] | screenshot/frame refresh後もComputation headとRecord件数が不変であるarchitecture testを追加 |
| Secret boundary | [x] | attach→use→replace→旧session拒否→expire→rebind、hash-only DB、CAS / Record / bundle / workspaceのsecret-like canary非混入を自動検証 |
| Process cleanup | [x] | Run-owned process groupでchild / grandchildを回収し、別Runをkillしない。終了時`orphan_process_count = 0`をassert |
| Record correlation | [x] | durable appendが返すRecord IDをActivity receiptの`record_ref`へ接続 |
| CI vertical E2E | [~] | Rust実Browser E2EとActivity Room 3-actor E2EはPASS。次は同一jobでPublic URL作成からfinal head / Record / secret scan / orphan確認までを通す |
| Staging acceptance | [ ] | production相当stagingで同一Acceptanceを最低5回実行し、全runのreceiptを保存する。手動ブラウザ確認はgateに数えない |
| Join→Interactive SLO | [~] | 6区間、4つのproduct span、p50 / p95 / success rate、failure stageの共通receipt集計とunit testは実装済み。次は実milestoneを相関しCI/staging artifactとして保存する |

**Release判定:** 最後の3項目（CI vertical E2E、staging 5回、SLO receipt）が完了するまで
Browser Activity pathをrelease-readyと呼ばない。

## 3. P0 — 仕様と追跡情報の整合

| ID | TODO | 成果物 / 完了条件 | 依存・追跡 |
|---|---|---|---|
| MODEL-001 | [ ] open issue を `Computation / evidence / Protocol / Adapter / Materialization / orchestration` の6分類で棚卸しする | #490、#502、#1086、#1089、#1091、#1092、#1179、#1193 を `keep / rewrite / supersede / close` に分類し、各 issue に accepted RFC への参照と移行判断を残す | NEW。最初に実施 |
| MODEL-002 | [ ] 旧「Capsule = launch contract」「ExecutionGraph = semantic identity」記述と accepted Computation model の関係を決着する | identity に入る値と evidence / realization に留める値を RFC の型と例で固定する。未合意なら新 draft RFC を作り、合意前に旧 issue の実装を進めない | MODEL-001、#490、#502、#1086 |
| MODEL-003 | [ ] authoring input から初期 Computation への canonical projection を1経路に統一する | `capsule.toml` / resolved inputs から C0 を生成する producer が1つになり、順序差で同じ入力の `ComputationRef` が変わらない fixture test を持つ | MODEL-002、#1179 |
| MODEL-004 | [ ] session / run state の identity を統一する | Run head、Record、Materialization、physical session ID を別フィールドとして保存し、Record や host path が Computation identity に混入しない migration test を持つ | MODEL-002、#1193 |
| MODEL-005 | [ ] executor ごとの network / filesystem / environment policy semantics を統一する | source / OCI / snapshot / remote の各 lane で、同じ宣言が同じ effective posture または明示的な unsupported error になる | #1176、#1177、#1192 |
| MODEL-006 | [ ] acceptance / rejection を evidence として保存する | rejection に reason、checked contract、provider posture、causal reference が入り、失敗を current Computation と誤認しない | #1159 |
| ADAPTER-001 | [x] Browser Adapter の規範上の位置づけを決める | `ato.browser@1` を built-in に昇格させず、`BROWSER_PROTOCOL_ADAPTER_EXTENSION_V0.md` で experimental extension の role、ingress、presentation、provenance、ordering、replay、security、unsupported behavior、Detected mismatch を固定した | #1311、accepted `PROTOCOL_ADAPTER.md` |
| ROADMAP-001 | [ ] 固定件数の issue 一覧をロードマップから廃止する | GitHub query を issue 状態の正とし、本書には outcome、依存関係、完了条件だけを残す。月1回、closed / superseded / untracked を監査する | NEW |

## 4. P1 — Computation model の改善

| ID | TODO | 成果物 / 完了条件 | 依存・追跡 |
|---|---|---|---|
| CORE-001 | [ ] ProtocolSemantics の登録・versioning・unsupported feature 処理を固定する | unknown Protocol / operation / version / required feature が typed error で fail closed になり、Kernel が payload を decode しない architecture test を追加する | MODEL-002 |
| CORE-002 | [ ] evidence-only と semantic Evolution の分類を全 Adapter で検証する | inbound / outbound / observation ごとの effect 表を作成し、memory-only change でも successor Computation が seal される統合 test を持つ | CORE-001、ADAPTER-002 |
| CORE-003 | [ ] Composition の Port ownership と接続検証を product path に載せる | unmatched internal client、Protocol / role mismatch、implicit first-process assignment が launch 前に拒否され、正常系は親 boundary だけを export する | #1037、accepted `COMPOSITION.md` |
| CORE-004 | [ ] Binding を physical Endpoint から独立した logical contract にする | exact Run / execution scope、lease、replace、detach、expiry を記録し、provider credential 値を CAS / Record / logs に保存しない | #1185 |
| CORE-005 | [ ] generic Replay の Adapter capability gap を可視化する | target closure に必要な Adapter と operation を事前検証し、`apply` 不足時は再生開始前に typed error を返す。Protocol switch を Replay に追加しない | accepted `MATERIALIZATION.md` |
| CORE-006 | [ ] Materialization fallback を identity 非依存にする | replay / filesystem / snapshot の選択が同じ target `ComputationRef` を変えず、compatibility rejection と fallback decision を evidence として残す | #1184 |

## 5. P1 — Adapter の品質

### 5.1 共通 conformance gate

| ID | TODO | 成果物 / 完了条件 | 依存・追跡 |
|---|---|---|---|
| ADAPTER-002 | [ ] 全 Adapter 共通 conformance suite を作る | `preflight → attach → activate → observe/apply → verify → quiesce → detach/wait`、部分 attach rollback、double quiesce、persistence failure を同じ harness で検証する | NEW |
| ADAPTER-003 | [ ] machine-readable capability matrix を生成する | Adapter / Protocol / operation / version / required feature / observe / apply / verify / replay / platform を CI artifact と docs に出す。宣言と実装の差で CI を失敗させる | ADAPTER-002、#1196 |
| ADAPTER-004 | [ ] payload と Record の共通防御を追加する | canonical encoding、size limit、sequence、causal reference、redaction、malformed / truncated input の negative test を全 Adapter に要求する | ADAPTER-002 |
| ADAPTER-005 | [~] Adapter transport の認証と anti-replay を統一する | Browser Activity pathはactivity / Run / epoch / expiry scopeとmonotonic sequence、negative testを実装済み。残りはIPC / vsock等へ同じ要件を適用する | #1178、#1185 |
| ADAPTER-006 | [ ] platform matrix を release gate にする | Linux / macOS / Windows で capability の supported / unsupported を実測し、silent degradation を禁止する | #1154、#1192 |

### 5.2 Adapter 別の残作業

| ID | TODO | 成果物 / 完了条件 | 依存・追跡 |
|---|---|---|---|
| PROCESS-001 | [~] process-tree ownership を全 platform で完成させる | Browser release pathとUnix Process AdapterはRun-owned group、cross-Run fault test、orphan=0を実装済み。残りはWindowsと全error経路のconformance | NEW、ADAPTER-002 |
| PROCESS-002 | [ ] minimal environment 契約を検証する | undeclared host env が渡らず、platform base env と Binding projection のみが渡る Linux / macOS / Windows test を追加する | MODEL-005 |
| PTY-001 | [ ] `ato.pty@1` が保証する terminal semantics を明文化する | 現在の pipe-backed 実装との差を監査し、実 PTY が必要なら置換、不要なら Adapter / Protocol 名と仕様を正す | NEW、Detected mismatch 候補 |
| PTY-002 | [ ] PTY の長時間・高出力品質を上げる | bounded buffer、backpressure、UTF-8 非依存 byte fidelity、resize / signal ordering、detach 中の出力を test する | ADAPTER-004 |
| WORKSPACE-001 | [ ] workspace observation と replay の同値性を検証する | put / delete / rename、empty dir、symlink、permission、large file の supported 範囲を固定し、capture→mutation→replay の fixture を追加する | ADAPTER-002 |
| WORKSPACE-002 | [ ] credential exclusion を継続的に検証する | `.env`、key、credential store、VCS metadata、`.capsule/`、include で再許可できない exclusion を corpus test にする。secret scan は「証明」と表現しない | #1183 |
| BINDING-001 | [ ] Binding lease と secret rotation を実装する | timer-driven expiry、exact execution binding、replace 後の旧 credential 無効化、log / Record / snapshot 非混入を E2E で示す | #1185 |
| HTTP-001 | [ ] `ato.http@1` v1 の対応範囲と上限を固定する | request line / header / body 上限、timeout、disconnect、duplicate header、chunked / streaming の supported / rejected behavior を Protocol test にする | ADAPTER-004 |
| HTTP-002 | [ ] response evidence の安全性を上げる | authorization / cookie / secret-like header の policy、body capture 上限、truncation evidence、backpressure を実装・検証する | HTTP-001 |
| BROWSER-001 | [ ] pointer actuation の意味を決定する | DOM synthetic event と browser-native actuation の security / compatibility を比較し、`ato.browser@1` の normative behavior と fallback を決定する | #1311、ADAPTER-001 |
| BROWSER-002 | [~] multi-actor Browser operation bridge を完成させる | local/API自動試験はactor provenance、Runner ordering、duplicate / late ACK、epoch、disconnect/rebind、hostile timeout、record_refまでPASS。残りはPublic URLからの単一CI E2Eとstaging反復receipt | branch `feat/browser-runner-multi-actor-bridge-v0` |
| BROWSER-003 | [x] Browser presentation と Evolution input を分離する | screenshot / frame refreshがComputation headとRecordを変えないarchitecture testを追加。DOM / console / mediaはcontract上evidence/projectionに限定 | BROWSER-002、`BROWSER_PROTOCOL_ADAPTER_EXTENSION_V0.md` |

## 6. P2/P3 — Adapter の種類を増やす判断

新 Adapter は「対応できる対象を増やす」だけでは追加しない。次の entry criteria を
すべて満たしてから draft RFC と implementation issue を作る。

1. 異なる2つ以上の実ユースケースがある。
2. logical Protocol、role、operation、payload version が定義できる。
3. observation が evidence-only か Evolution か分類できる。
4. physical resource ownership と quiesce boundary が定義できる。
5. observe / apply / verify / replay の対応可否を明示できる。
6. secret、authentication、multi-tenant、resource exhaustion の threat model がある。
7. capability sample と limitation sample を1つずつ作れる。

| ID | 候補 | 先に検証すること | 判断後の成果物 |
|---|---|---|---|
| CANDIDATE-001 | [ ] WebSocket / SSE | HTTP v1 の拡張で足りるか、双方向 stream に独立 Protocol が必要か | ADR + chat/notification Activity sample |
| CANDIDATE-002 | [ ] Media / WebRTC | media frame は presentation evidence、control / consent は Evolution と分離できるか | ADR + multi-participant media sample |
| CANDIDATE-003 | [ ] Agent tool / MCP | tool schema と invocation/result を Protocol に保ち、Kernel に tool-specific enum を追加せず表現できるか | draft RFC + human/agent shared task sample |
| CANDIDATE-004 | [ ] Structured state / database | raw database engine 別 Adapter ではなく、transaction / migration / checkpoint を typed state Protocol として共通化できるか | draft RFC + SQLite/Postgres comparison sample |

GPU、VM、OCI、placement provider、snapshot backend は Adapter 候補ではない。これらは
Materialization / provider / orchestration の責務として追跡する。

## 7. P1 — ユースケースとサンプル

### 7.1 必須 vertical slices

| ID | TODO | シナリオ | 完了条件 |
|---|---|---|---|
| USECASE-001 | [~] Browser Activity multi-actor sample | host と2 participant が同じ browser computation を操作する | 3-actor Room/Runner試験はPASS。Public URL作成からfinal head / RecordsまでのCI/staging一貫実行を残す |
| USECASE-002 | [ ] in-memory HTTP counter sample | workspace を変更せず HTTP interaction だけで computation が進む | request ごとに successor `ComputationRef` が生成され、response は evidence-only、replay 後の結果が一致する |
| USECASE-003 | [ ] interactive PTY task sample | human / agent が input、resize、signal、detach / resume を行う | byte fidelity、operation ordering、quiesce、unsupported replay を含む期待結果を検証する |
| USECASE-004 | [x] secret Binding rotation sample | API credential を attach→replace→expire→rebind する | Public Browser controller credentialとCLI portable closureで旧credential拒否、hash-only永続化、secret-like canary非混入を自動検証 |
| USECASE-005 | [ ] composed Web + worker + state sample | compatible Ports を接続した複数 Computation を1つに compose する | type mismatch と unbound Port を拒否し、内部 interaction を Tau、外部 Port だけを親 boundary に公開する |
| USECASE-006 | [ ] two-materialization equivalence sample | 同じ Capsule を Replay と別 Materializer で realize する | target `ComputationRef` は同一、compatibility / fallback は evidence として比較できる |
| USECASE-007 | [~] safe-failure samples | missing Adapter、expired Binding、denied network、incompatible snapshot、hostile browser | Browser hostile timeout / expired credential / stale operation / cleanupは自動化済み。非Browser failure sampleは別scopeで継続 |

### 7.2 sample の管理品質

| ID | TODO | 成果物 / 完了条件 | 依存・追跡 |
|---|---|---|---|
| SAMPLE-001 | [ ] 現在の `samples/` と recipe catalog を棚卸しする | 各 sample を `smoke / capability / use-case / real-app / limitation / legacy` に分類し、owner、platform、lane、Protocol、Adapter、CI status を一覧化する | #371、#637 |
| SAMPLE-002 | [ ] sample metadata schema を追加する | 対象 capability、expected Records / Evolution、required bindings、supported platform、timeout、assertion を machine-readable にする | SAMPLE-001 |
| SAMPLE-003 | [ ] generic sample assertion runner を作る | stdout / HTTP / file / Record / head transition / security negative assertion をローカルと CI で同じコマンドから実行する | SAMPLE-002 |
| SAMPLE-004 | [ ] recipe の既知不具合を直して catalog と同期する | homepage、pocketbase、n8n、Linkwarden、Langflow を UI path と readiness まで検証し、CLI-only success を PASS にしない | #243、#446、#447、#448、#449 |
| SAMPLE-005 | [ ] source provenance と license gate を必須化する | SPDX、source repository、revision / digest、third-party notice が揃わない recipe を catalog publish から拒否する | #625 |
| SAMPLE-006 | [ ] real-app sample を compatibility corpus に昇格する | recipe の偶発的成功ではなく、version pin、expected screen/API、state lifecycle、stop 後 orphan=0 を継続検証する | #369、#628、QUALITY-001 |

## 8. P2 — 品質・互換性・運用

| ID | TODO | 成果物 / 完了条件 | 依存・追跡 |
|---|---|---|---|
| QUALITY-001 | [ ] corpus-based Capsule compatibility matrix を構築する | supported realization lane ごとに正常系3件以上と代表的失敗1件以上を持ち、OS / arch / Adapter / state / network / result を保存する | #1194、SAMPLE-003 |
| QUALITY-002 | [ ] failure taxonomy を統一する | authoring / resolve / placement / materialize / attach / activate / interact / quiesce / restore を分類し、E999 や文字列判定に退化させない | #1194 |
| QUALITY-003 | [ ] fault-injection E2E を定期実行する | process crash、disk full、network loss、expired secret、corrupt object、Unhealthy、partial attach を disposable host で実行し、receipt と cleanup を検証する | #1235、#1160 |
| QUALITY-004 | [ ] feature / posture drift を検出する | build artifact、runner、deployment ごとの supported feature と effective isolation を machine-readable に比較し、staging / production drift を通知する | #1196 |
| QUALITY-005 | [~] product loop の SLO を計測する | milestone保存と共通receipt集計（4 span、6区間、p50 / p95 / success rate、failure stage）は実装済み。残りは実Acceptance milestoneを入力してCI/staging artifactを保存する | Web-first product goal |
| QUALITY-006 | [ ] release gate を1つにまとめる | format / clippy / unit / conformance / sample corpus / cross-platform / security negative / AODD receipt の必須・条件付き・informational を明記する | ADAPTER-006、QUALITY-001 |

## 9. 実行順と exit gate

### Gate A — Semantic alignment

1. MODEL-001〜002 で旧 issue と accepted RFC の不一致を解消する。
2. ADAPTER-001 で Browser の規範上の位置づけを決める。
3. MODEL-003〜006 の issue と acceptance criteria を確定する。

**Exit:** 新しい実装 task が必ず6分類のどれかに入り、Capsule / Run / Record /
Materialization を同一 identity として扱う active issue がない。

### Gate B — Adapter quality baseline

1. ADAPTER-002〜006 の共通 conformance / capability / security matrix を作る。
2. Browser、HTTP、Binding を優先して Adapter 別 task を通す。
3. USECASE-001、002、004、007 を CI に載せる。

**Exit:** supported と宣言した Adapter operation が observe / apply / verify / quiesce /
replay / security / platform のいずれで未検証か、machine-readable に判定できる。

### Gate C — Composition and real use cases

1. CORE-003〜006 を完成させる。
2. USECASE-003、005、006 を追加する。
3. real-app recipe を SAMPLE-006 / QUALITY-001 の corpus に昇格する。

**Exit:** 単一 process の起動だけでなく、typed interaction、Binding、Composition、
再開、安全な失敗を end-to-end で示せる。

### Gate D — Adapter expansion

1. CANDIDATE-001〜004 を entry criteria で評価する。
2. product loop を改善する候補だけを draft RFC として採用する。
3. capability / limitation sample と conformance test を同じ変更で追加する。

**Exit:** 新 Adapter が use-case special case や Kernel enum を増やさず、既存の
Protocol / Adapter / Port / Computation model で説明・検証できる。

## 10. Definition of Done

ロードマップ項目は、コードを merge しただけでは完了にしない。該当するものをすべて
満たした時点で `[x]` にする。

- accepted RFC または明示された experimental contract と整合している。
- unit / integration / E2E / negative test がリスクに応じて追加されている。
- resource cleanup、typed failure、secret redaction を検証している。
- current behavior と future direction を docs で区別している。
- public capability の追加には capability matrix と sample がある。
- cross-repo 変更は owning repo ごとに issue / PR / deployment receipt がある。
- `NEW` 項目には実装開始前に GitHub issue が作成され、本書から参照されている。
- 完了 evidence がない手動確認は、再現手順と確認日を残している。
