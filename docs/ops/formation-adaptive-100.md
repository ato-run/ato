# Formation 6b-F — 100-app Adaptive arm

**baseline 7/100 → adaptive 7/100、追加typed-K PASSは0。** 100件すべてでfresh known-D Formation terminalを保存し、12件で実Adaptive API searchを行った。valid canonical Dは10件、Runtime実行は2件、generated-D PASSは0件。既存7件のPASSはfresh same-K receiptで維持した。

## 測定条件と比較の限界

ato `e494e9375cf1151fc2884d545047dfbdad52c2cc`、ato-api `dcc3049a69051ed6d1c7819580ed1e171815c34c`。#1450は開始時OPEN、head `d56a0395400ea5a31fbc593cedad22c496b00c22`。preregistration `c857ad0ce3e16e3be976adacac2a16d9a9d9ac82` をpushしてから実行した。plan SHA256 `5532dfc7dc4daa25fbc09ada74e1d0f759f98d53aa1fe32dec8c2d8b9475e884`。

source/ref/archive/license100件は6b-Eと完全一致。known-Dを先に実試行し、PASSならcall0。CandidateProducer／DecisionProviderは各対象でON、one proposal round、max proposals1、attempts4、network denied、Exact(local)、Linux/aarch64 bwrap+landlock、既存toolchainを維持した。prompt/2、deepseek-flash、thinking disabled、output2048、Jev1.13.0、固定prompt、temperatureは既存adapterどおり未指定。測定途中の調整・app failure再試行は0。

**81件のKは、ユーザー承認による既存静的Kテンプレートの明示追加であり、LLMがKを形成した改善ではない。** baselineでKが形成された19件はexact同一K。known phaseとzero-known-D Requester searchは2段のハーネスで接続し、前段のfiltered known-D failureは別証跡に保持した。第二SearchState／provider failure contextへは転記していない。前段でDが実行失敗した場合はprovider前に停止するguardがある。したがって、未改変Infer入口の純粋なprovider ON/OFF比較とは扱わない。

catalogが空の80件とknown PASS7件はAdaptive API searchを作っていない。catalogのあるSuperset1件はRequester source-object256MiB capで事前停止した。100 adaptive terminalは、actual known-D terminal＋事前固定catalog/call gateからの測定分類であり、架空の100 durable SearchStateではない。

root-onlyの共通owner authorization（sorted regular root *.py、first16、安全path、Python3.12.7、guest8000）と既存workspace inventoryが今回のcatalog投影範囲。空catalog87件（うちknown PASS7、最終gap80）は、この選択範囲での空集合であり、80件すべてのrepositoryが本質的にoperation vocabularyで表現不能という証明ではない。private ID mapはproviderへ渡していない。source text16KiB／per-file8KiBを維持した。

## 完了とfunnel

実行時刻 `2026-09-30T06:56:07.724155+00:00` → `2026-09-30T07:14:40.346659+00:00`、elapsed 1112.623s。固定pilot5,15,25,35,45,55,65,75,85,95 PASS後、残り90件を実行。untyped0、infra rerun0、app failure rerun0、unknown usage0、未解決call/reservation0。

| Layer | baseline | adaptive | 証拠の意味 |
|---|---:|---:|---|
| A | 100 | 100 | source recognized |
| B | 19 | 100 | explicit static K bound before execution (19 baseline K refs unchanged,81 user-authorized explicit K) |
| C | 19 | 28 | known or validator-admitted canonical D available |
| D | 7 | 9 | Runtime admission reached successful execution start; issued/claimed/refused ticket is not admitted |
| E | 7 | 9 | actual execution, Runtime attestation or known realization |
| F | 7 | 7 | Contract Verifier actually invoked; readiness/launch failure does not count |
| G | 7 | 7 | fresh fully_satisfied authority receipt |
| H | 7 | 7 | retained |

I functional acceptanceは今回0。既存2048/reveal.jsの2 unique appsは別appendであり、このwaveの結果へ加算しない。persistence未測定。D/Eはticket発行・claimのみでは増やしていない。`candidate_not_observable`の2件はlaunch/readinessで終了し、Contract Verifier未到達なのでFに含めない（common attempt.rsのrealizer launch error経路）。

| Efficacy | 件数 |
|---|---:|
| proposal eligible / wire-ready | 13 / 12 |
| producer invoked / valid proposal | 12 / 10 |
| Validator registry admitted / Runtime admitted | 10 / 2 |
| generated-D executed / PASS | 2 / 0 |
| baseline Dなし → generated D | 9 |
| baseline未実行 → execution | 2 |
| baseline PASS維持 / 新規typed-K | 7 / 0 |

## 最終terminal分布

| Primary | 件数 |
|---|---:|
| attempt_failed | 3 |
| known_d_pass | 7 |
| network_denied | 7 |
| operation_catalog_gap | 80 |
| proposal_declined | 2 |
| source_transport_limit | 1 |

80 gapのうち11件は前段known-Dがnetwork deniedで停止した後、追加catalogが空だった。前段known-D結果100件はbaseline terminal・D ref・typed-K到達と一致しており、観測regression0。primary80 gapと7 generated-D network refusalを、baselineのpolicy分類12件と同じ定義の分布として単純比較しない。

## 代表証拠

- known-D PASS7件: Uptime Kuma（static fallbackのみ）、2048、reveal.js、Hextris、0h h1、0h n0、Emoji search。Uptime Kumaを監視service functional successと数えない。
- valid generated D10件: linkding、SearXNG、changedetection.io、ComfyUI、Calibre-Web、text-generation-webui、Wekan、copyparty、Healthchecks、ArchiveBox。Requesterは保存されたproposal bytes/hashを独立再compileして同一Dを確認した。
- 7件のgenerated Dは依存解決がnetwork deniedで実行前refusal。canonical D validityとRuntime admissionは別。
- text-generation-webuiは実processが`yaml`欠損、Wekanは`requests`欠損で終了し、`candidate_not_observable`。AtoがHTTP readiness未成立をtypedに記録し、PASSは発行していない。
- copypartyはpyproject.tomlにlockfileがなく、`ticket_unplannable / intent_requires_authoring`で実行前refusal。
- JupyterLab／Jenkinsはtyped `unsupported`提案で終了。schema failure／provider transport failureとは分けた。
- Supersetは既存source-object capによるpreregistration blocker。app build未実行、model call0。

multi-round必要と確定した件数0。one roundだけでは判断できない5件（declined2＋attempt_failed3）はunknownと記録した。失敗を理由にprompt、operation、dependency、port、permissionを変更していない。新規PASSがないため、新規functional append候補0。

## usage、cost、reservation

| Provider | calls | input tokens | output tokens | 保守的費用推計 USD micros |
|---|---:|---:|---:|---:|
| CandidateProducer | 12 | 29127 | 504 | 9357 |
| DecisionProvider | 12 | 8232 | 888 | 348 |

合計24 calls、input 37359、output 1392、peak/cache-miss上限推計 9705 USD micros（$0.009705、billing receiptではない）。Jev outputは無料。Decision latencyは79–249ms、median105ms、total1472ms。既存CandidateProducer adapterはper-call latencyを記録しないので未測定と明記し、app／wave elapsedを別記した。

reservationは実usageと分離し、12×81102＋12×2753＝1006260 USD microsを返金なしで消費。既存残余4513388から3507128を維持。全RequestEvidenceに実ResponseEvidenceがあり、producer journalはhalt済み、decision responseもusage／model／settlementが閉じている。追加PASSが0なのでcalls/tokens/cost per additional PASSはundefined（JSON null）、0ドルとは表示しない。

公開価格の[DeepSeek snapshot](https://api-docs.deepseek.com/quick_start/pricing/)と[Jev snapshot](https://docs.typesafe.ai/models)、取得時刻と元HTML digestを保存。deepseek-flashというAPI名は固定だが、上流weight revisionの暗号学的固定はない。Requesterだけにsealed memoryでcredentialを注入し、値・値のhashを保存しなかった。controller／Runtime／Coordinatorへkeyを渡していない。

## language / source shape分布

| Language | N | terminals |
|---|---:|---|
| Blade | 1 | operation_catalog_gap:1 |
| C# | 5 | operation_catalog_gap:5 |
| CSS | 1 | known_d_pass:1 |
| Go | 13 | operation_catalog_gap:13 |
| Java | 5 | operation_catalog_gap:4, proposal_declined:1 |
| JavaScript | 19 | attempt_failed:1, known_d_pass:6, operation_catalog_gap:12 |
| JavaScript/Vue | 1 | operation_catalog_gap:1 |
| PHP | 6 | operation_catalog_gap:6 |
| Python | 21 | attempt_failed:2, network_denied:7, operation_catalog_gap:11, source_transport_limit:1 |
| Ruby | 4 | operation_catalog_gap:4 |
| Rust | 6 | operation_catalog_gap:6 |
| TypeScript | 16 | operation_catalog_gap:15, proposal_declined:1 |
| TypeScript/JavaScript | 1 | operation_catalog_gap:1 |
| Vue/TypeScript | 1 | operation_catalog_gap:1 |

| Source shape | N | terminals |
|---|---:|---|
| dotnet-service | 5 | operation_catalog_gap:5 |
| frontend-monorepo | 1 | operation_catalog_gap:1 |
| frontend-spa | 8 | operation_catalog_gap:8 |
| go-service | 14 | operation_catalog_gap:14 |
| jvm-service | 4 | operation_catalog_gap:3, proposal_declined:1 |
| multi-service | 12 | attempt_failed:1, operation_catalog_gap:10, source_transport_limit:1 |
| node-monorepo | 5 | operation_catalog_gap:5 |
| node-service | 10 | known_d_pass:1, operation_catalog_gap:9 |
| php-service | 7 | operation_catalog_gap:7 |
| plain-static | 8 | known_d_pass:6, operation_catalog_gap:2 |
| polyglot-service | 1 | operation_catalog_gap:1 |
| python-monorepo | 1 | proposal_declined:1 |
| python-service | 18 | attempt_failed:2, network_denied:7, operation_catalog_gap:9 |
| ruby-service | 2 | operation_catalog_gap:2 |
| rust-service | 4 | operation_catalog_gap:4 |

## 原本・hash・状態

raw archive SHA256 `d031296251e528a0523b4373415e508bed51f2e7e8aa64187abc3391ec967752`、raw results `13e3ca1aab0934a01477e3c98ba1559060a4e556dd64f2a525ac09f0cfda8288`、manifest `8fdb536157513d70dd57b808265a7aea9a42cf58dcc68210d4c453b9511bd1ef`。raw583 filesを全件hash照合。その他output774 files、frozen archives、receipts／retained objects、D1/R2 stateはLinux hostで保全。開始disk free124713345024 bytes、終了123937714176 bytes。Rust1.96.0、Ubuntu24.04/Linux6.17 aarch64。host-wide Docker pruneなし。

preregistration [plan](formation-adaptive-100-plan.json)、[operation inventory / plan prose](formation-adaptive-100-plan.md)、[100結果](formation-adaptive-100.json)、[paired比較](formation-known-vs-adaptive-100.json)、[raw manifest](evidence/formation-adaptive-100-20260930/raw-manifest.json)、[raw archive](evidence/formation-adaptive-100-20260930/raw-evidence.tar.gz)。追加のsealed-channel正／負テストはdummyのみ、実credential read0／model call0。

implemented: measurement glueのみ、新capability0。measured: actual known terminals100＋scoped adaptive terminal100、実API searches12。verified: fresh same-K receipts7、generated-D PASS0、functional test0。このevidenceは未merge、deployed=false、remote migration=false。既存Runtime／general adapter／5cはmerge済みの固定pinを利用。

WBOはhistorical source-to-OCI image/verified artifact PASS、runtime_policy_capability_required、C/D/E未到達、functional0。Vikunjaはhistorical preregistration unexecuted。今回のautomatic／adaptive successへ合算しない。#1421 untouched、E2 72 cells未実行、source rewrite／per-app hack／K weakening／permission expansion／deploy／migrationなし。

外側hard timeoutはknown／Requester各phase900sで、2段全体の理論上限は900sを超え得るというハーネス上の制約がある。実測の最長app186.712sで、今回の全appは900s以内に終了した。

## CI

結果headのCIは別途記録する。Rust/Cargo/workflow差分は0。greenとは仮定せず、exact-headのcheck結果を確認する。
