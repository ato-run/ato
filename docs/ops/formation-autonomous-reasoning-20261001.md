# Formation Codex-first探索の実装と受入記録 — 2026-10-01

実Coordinator・実Linux Runtimeで、SVGOMGのknown Dなし → 実行FAIL → CodexによるD修正 → fresh same-K PASSが成立した。同じ初期情報・共通protocolを使うDeepSeekの実API探索でも、known Dなしからfresh same-K PASSが成立した。changedetection.ioとKuttは具体的な依存処理の能力不足で停止した。**全面的な受入は未完了**。成功Dは`k_reached_awaiting_assessment`として保存され、通常Runの許可・公開・配備には変換していない。

Draft PR: [Ato #1452](https://github.com/ato-run/ato/pull/1452)、[API #712](https://github.com/ato-run/ato-api/pull/712)。merge、staging/production deploy、remote migration、flag変更は実施していない。APIの追加migrationは隔離したlocal D1だけに適用した。

## 変更前の経路と再利用

変更前の共有reasoning prototypeは、Codexの実OSS fresh PASSを成立させておらず、全source inventory・context重複、同じ辞退の再問い合わせ、6 exchangeの固定上限、失敗理由の不足があった。登録値には既存の暗号化・Runtimeのprivate secret grant経路がある一方、Formation間で用途・scope・期限を照合するmetadataと入力待ち状態がなかった。

既存requester、Coordinatorのrevision/CAS/fence、Rust proposal compiler、attempt journal、phase別network/authority gate、Runtime、frozen-K Verifierを継続使用した。credentialは既存AES-GCMと`AI_KEYS_MASTER_SECRET`、非Serializeの`ResolvedSecret`/`SecretGrantV1`を使用する。新しい独立した実行系・TypeScriptのcanonical D判定・app別presetは追加していない。

測定中に見つけた共通不具合も修正した。

- 深いLinux作業パスでUnix socket長が上限を超えていた。owner directory FDを保持して`/proc/self/fd`から接続する。実Linux socket handshakeを検証した。
- 後続inspectionに必要な検証済みsource snapshotが初期context取得後にDropされていた。Requesterがsnapshot自体を保持し、終了時に既存cleanupを使用する。
- inspection時間の判定にreasoning待ち時間を算入していた。保存された`inspection_ms`を累積し、round期限と独立した上限にした。
- nested JSのliteral relative importにextension/index解決がなかった。root内のexact candidateだけを公開し、複数候補は曖昧性として返す。裸の説明語からdirectoryを探索範囲へ追加しない。

## フェーズ別の最終契約と実装状態

規範的な今回の設計は[draft ADR-042](../rfcs/draft/ADR-042-formation-autonomous-reasoning.md)。以下はtargetと現行実装を分けた記録であり、未実装を利用可能とは扱わない。

| フェーズ | 契約・今回の変更 | 実測・制約 |
|---|---|---|
| Search開始 | effective config、K、source、ceiling、予算、deadlineを固定。Coordinator永続化を確定点とし、stable start ID/checkpointで復旧する | restartで同じSearch/入力/round/期限を確認。既存Searchを新しい設定でリセットしない |
| round開始 | 残予算・Runtime・前round・pending/UNKNOWNを確認。round枠/revision/deadlineを先に確定しclaim/fenceをjournalへ保存 | Runtime不在でroundを開かず待つ。UNKNOWNを新Dで迂回しない |
| source/context | rootのmanifest/lock/config/Dockerfile/READMEから開始。取得済み文面の明示参照だけでnested scopeを広げ、path/ref/digestと根拠を保存 | glob32 files、inventory128、context4 files/16 KiB、1 file8 KiB。曖昧性・再帰glob・overflowを明示。COPY . .はread根拠にしない |
| reasoning/validation | 共通`ato.formation-reasoning-input/1`でCodex/APIを交換。input/output・省略projection・Runtime能力・過去D/証拠・binding metadata・残予算を保存 | JSON/schema/修正可能validationは同roundで最大3 repair。同じ指摘の反復を止める。既存Rust compilerでcanonical Dへ変換。prompt8はapp非依存のlowering条件をtrusted inputへ追加 |
| admission/実行 | phase別networkとresource/operation/phase別authorityをDへ含め、frozen ceilingで実行。要求条件もD digestに含まれる | SVGOMGで拒否証拠からnetwork修正してPASS。上限外は`exploration_authority_exceeded`。外部権限を人間に承認させるループはない |
| 変数解決 | Dはrequirementだけを保持。owner側のencrypted storeとmetadata/assignmentを分離し、Runtimeが値をprivateに解決する | 登録値・一時signing値・needs_inputを確認。LLM/D/公開証跡へ値とcredential IDを送らない |
| 実行/検証失敗 | 正しいsetupで再現する証拠なしに`source_broken`としない。未知能力・外部情報不足・修正可能FAIL・no_progressを分ける | typed declineの理由/証拠は保存。ただしCoordinator terminal reasonの分類には後述の未完了点がある |
| 接続/復旧 | operation/exchange/attempt IDを維持。応答消失を照会/冪等再送で解決し、LLM/実行を再開始しない。送信後不明なprovider callは保守的に費用計上 | 実HTTP応答消失5か所＋requester restartで重複実行なし。provider429/5xx/401とretry上限はtransport testで確認 |
| round終了 | 証拠・予算・成功D・次状態をfence下で確定。PASS後はrequirementsだけを縮小し、fresh PASS時だけ成功Dを更新する | SVGOMGの縮小版fresh PASSを確認。縮小FAIL時の旧成功保持はRust test。数学的最小性は主張しない |
| 入力待ち/終了 | needs_inputは必要項目・用途・取得方法を返し、LLMを停止。元の期限を保持。今回限りの値は終了/cleanup確定後に削除する | APIに今回のみ/再利用scopeの登録・失効・削除がある。専用PWA入力UI/CLI入力commandは未実装 |

外部設定は`ato form --runtime-network --exploration-config <JSON/TOML>`のconfig/2で`exploration.formation.max_rounds`（既定3）、`max_retries`（初回＋既定3）、`exploration.reasoning.round_timeout_ms`（既定600000）、独立したinspection/exchange/token/cost上限を指定する。Search deadlineは`--deadline-seconds`（既定1800）。既存保存bytesのdefault省略を維持した。新measurementの6または12 exchangeは3 D roundとは別枠であり、過去の6 exchange/24 KiB測定を更新していない。

inspection/acquired/transmitted/truncatedは別に記録する。取得済みcontextは保持するが、送信slotに収まらない情報もprojectionへ残す。1 roundは1 D仮説の生成・validation・実行・K検証。inspection・構文repair・通信retryは追加roundとして数えない。

## 実OSS・API測定

各ケースは手動起動や成功Dの後付けではなく、保存された共通input → Codex/API output → validator → Coordinator → native Runtime → Verifierを通した。新しい測定は旧Searchのretryとして扱わない。

| 対象 | 測定 | 結果 |
|---|---|---|
| SVGOMG | `codex2` | R1 network FAIL → R2 dependencies+build networkでfresh PASS → R3 build network除去後fresh PASS。3 rounds/3 actual attempts |
| SVGOMG | `codex6` | known Dなし。R1実行FAIL → R2依存取得だけのnetworkへ修正してfresh PASS → R3 no_progressで成功保持。3 rounds/2 attempts |
| SVGOMG | `faults2` | 応答消失・restartを含め、同じSearchでR1 FAIL → R2 fresh PASS。2 unique attempts、3 reasoning exchanges |
| changedetection.io | `python5` | root Dockerfile/CLI inspection、同roundのJSON state修正、実requirements解決まで進んだ。`feedgen~=1.0`を満たすbinary wheelがなくFAIL。source distribution/native builder不足を一度だけ辞退。2 rounds/4 exchanges/1 attempt |
| Kutt | `kutt` → 独立した`kutt2` | 旧測定のinspection障害・round expiryは保持。修正後は4 filesを明示参照で取得（inspection計10 ms）、一時JWT binding条件付きDを実行。`npm ci --ignore-scripts`後の`better-sqlite3` native binding不足でmigrateがFAIL。未対応依存処理として一度だけ辞退。2 rounds/4 exchanges/1 attempt |
| SVGOMG / DeepSeek | `api`/`api2`/`api3`/`api4` | 保存した初期情報から計13実call。validation、network/authorityまたは予算で停止。失敗測定を保持 |
| SVGOMG / Codex・DeepSeek | `codex8`/`api5` | 共通のlowering条件を明示した同一native pin。Codexは実行FAIL→修正→fresh PASS（3 rounds/3 exchanges）。APIはauthority拒否（inconclusive、未実行が確定）→同roundのvalidation修正→network拒否→fresh PASS（3 rounds/4 actual calls/3 attempts、57.680秒）。成功Dをseedにしない |

unique実OSSのCodex PASS、API PASSは**各1件（いずれもSVGOMG）**。同じアプリの独立測定を複数アプリの成功として数えない。機能全体・UI・永続化・本番のAcceptanceは未検証。Kuttは旧100件でKが未設定だった対象に今回explicit Kを与えた独立入口であり、旧baseline改善値へ加算していない。

SVGOMGの同一source/K:

- source ref: `f925656d40a507c512bf95ee14ad16445a4ad3ed`
- K: `sha256:6b2244326026e073d2c7d485434bb4d31d72fc43f06a3bc829c3dbfc6b1666d1`
- source closure: `sha256:f259299562bb6ae802c07948dc4c8e364a77b3149ae85704e95ecdc39ad2901c`
- 最終D: `sha256:d57e640c8f7e0c3cdfc483e644fc8b9023cdd06bb62b6c0f29d45aa8aa773ebc`
- `codex6` Search: `search_5810df8a45dace3eac1f893f5734d13d`、PASS attempt: `01M3V70BBCNAG4TTHQ7H37S19R`
- `api5` Search: `search_6d83d720de9e3e52d17c9253b65f6671`、PASS attempt: `01M3VCWAHE9EVF1396ZDG0P9GF`
- `faults2` Search: `search_d89522bc8b626f6ada9ad62e7d8909cd`、PASS attempt: `01M3V8Y5V529CNWTET7M9S8WCR`

Pythonの汎用`python_resolve_requirements`は、source-owned requirementsをpinned pipで取得し、wheelのversion/SHA-256をbounded manifestへ保存する。その固定artifactだけをoffline `--no-index --require-hashes --target`でinstallする。sourceに最初から全hashがないことだけを辞退理由にはしない。現時点ではbinary wheel限定であり、changedetection.ioのsource buildは成立していない。

## credential・変数の検証

専用公開source fixtureの`bindings2`は実Coordinator/実native Runtimeを使用した。外部サービスcredentialやKuttの成功と混同しない。

| ケース | 実結果 |
|---|---|
| 801 | JWT不足で実起動FAIL → private temporary256-bit signing値のbindingによりfresh same-K PASS |
| 802 | ownerが1件のreusable値を登録し、exact application/source/resource/operation/phase/purpose内でfresh PASS |
| 803 | 別Formation/known Dなしから、同じ登録値を再入力なしに照合しfresh PASS |
| 804 | 異なるsource closureではavailable metadataなし、assignment0件、needs_input。LLMは1 exchangeで停止。未実行attempt/待機状態は保持 |
| 一時値cleanup | terminal後、暗号化valueが削除され、revoked metadataは保持されたことをreadonly store照合で確認 |
| scope違い・複数候補・期限切れ・失効 | D1 testsで停止/拒否を確認。期限切れ・失効のnative Run測定は未実施 |

metadataはkind/purpose/service/endpoint/account/tenant、使用application/resource/operation/phase、secret区分、expiry/revocation、embedding許可、reusable/Formation-only scopeを保持する。Dとownerの具体的assignmentを分け、期限切れの割当を別値へ暗黙にすり替えない。外部認証がKに必要ならtemporary/dummyで解決済みにしない。

API migration0312（encrypted valuesとmetadata/assignments/input requests）、0313（claim operation冪等性）、0314（provider error分類の追加）は追補であり、旧rowを保持する。remoteには適用していない。

## 接続異常・restart

透明proxyで**実Coordinatorが保存した直後**のHTTP応答を一度ずつ失わせた。対象はSearch開始、proposal claim、proposal complete、Runtime attempt claim、attempt resultの5か所。proxyはmethod/path/body digest/statusだけを保存し、認証headerや値を保存しない。

requesterをR1 proposal前に停止し、同じjournalからresumeした。復旧前後でSearch/Satisfy ID、R1、入力SHA、Search/round deadlineが一致した。satisfy rowは1、attempt IDは2 unique、claim operationも2 unique、全attempt終了、UNKNOWN0。実行を繰り返して応答消失を隠していない。

Search期限`1790844458363`、R1開始`1790842658363`、R1期限`1790843258363`（round600秒/Search1800秒）はrestart後も同じ。停止前71.554秒と復旧後284.041秒はactive segmentであり、復旧待ちを除いた数値をSearch全体時間とは呼ばない。

provider側では実TCP transport testsで503初回＋3 retryの4 dispatch、429/Retry-After、送信状態/保守的予約、401停止、journal再起動後のretry上限を確認した。**vendorに対する故意の429/timeoutやRuntimeプロセスの未確定切断を実サービスへ注入した測定ではない**。pending/UNKNOWNの保全は既存Rust/Coordinator testsと旧記録の非変更を確認し、旧WBOは再実行していない。

## 検証とCI

- Ato Core探索26 tests PASS、Worker57 tests PASS、Runtime macOS67 tests PASS＋embedding関連の部分検証3 tests PASS（うち新規1 test）、Linux long-path ingressの実handshake test PASS。対象clippy/fmt/diff check PASS。
- API: typecheck/schema check、Coordinator155＋variables5＝160 tests PASS。実local D1/WASMを使うfixture。
- Atoの直前code head `1da5b31a2948fdca443f3918982512afb6104670`の[CI](https://github.com/ato-run/ato/actions/runs/36844595521): Ubuntu PASS、macOS/Windows FAIL。macOSはInstance worker activation、Windowsは既存Unix参照の13 compile errors。開始時base `b3327e159dff92d0e23ad1887848efe50f6509df`の[CI](https://github.com/ato-run/ato/actions/runs/36813243845)でも同じ失敗分類を確認。全CI成功とは報告しない。
- API `fcbdf8881f3e61bfed7ca4375b9e937adad29d65`の[CI](https://github.com/ato-run/ato-api/actions/runs/36829455062): activity-control-plane/instance-state-sync FAIL、full-serial SKIP。CORS/Activitiesの対象2 test filesはexact base `f00fa300142d45434be2417db2f7ae415517255e`とheadを同条件で検証し、どちらも13 FAIL/40 PASS。coop-presence WebSocket handshakeのCI失敗はbrowser base比較未実施。課金由来ではない。

ローカルの[Worker test出力](evidence/formation-autonomous-20261001/worker-tests.txt)、[deadline clippy](evidence/formation-autonomous-20261001/deadline-clippy.txt)、[Ato CI](evidence/formation-autonomous-20261001/ato-ci-1da.txt)、[API CI](evidence/formation-autonomous-20261001/api-ci-fcb.txt)、[API base](evidence/formation-autonomous-20261001/api-base-tests.txt)／[head比較](evidence/formation-autonomous-20261001/api-head-tests.txt)を保存した。最終code head a1e2f30cと証跡追補headの新CIは、この記録時点では実行中。

## 実行pin・証跡

| 実測 | native Ato/Worker source | API JS | Rust WASM authority source |
|---|---|---|---|
| codex6 / python5 / api4 | `aefc9f7cee7212b11527ec29672aece98f8cbf2e` | `fcbdf8881f3e61bfed7ca4375b9e937adad29d65` | `5aa22a9eda19efaa41016d17d819c822bb135d2d` |
| bindings2 | `bd38e44cd745061666c3bce09d234a47759e2582` | 同上 | 同上 |
| faults2 | `1f3cd083f3735d8bef6ce2f560c0e4e1a3384287`（harness wrapper修正は`84a9750e`） | 同上 | 同上 |
| kutt2 | `e124af0184ca857237ed643d59582b433abce07e` | 同上 | 同上 |
| codex8 / api5 | `b267cd91b4b989ea42152d39ae69d4e68a13f013` | 同上 | 同上 |

`codex8`/`api5`の初期inputを21項目で照合し、source/K/context/inventory/ceiling/予算/履歴/能力が一致した。APIのprevious Dは0、Codex D seedはなし。違いはSearch/call ID、観測時刻、provider adapter。

WASM SHA-256: `c9d0246a943e801a6e818e083bcab778b69216ba3509de3a09d9f6776e6bb29c`。native Linux/aarch64、bwrap+Landlock、Node22.14.0/npm10.9.2/Python3.12.7。最終code head `a1e2f30c8ac5f289dbdcaffbabc63c05202ceef1`。実測以降はnonsecretのembedding禁止をartifact guardへ接続し、Coordinator HTTP/source転送のtimeoutを凍結Search/roundの残時間で絞った。遅いHTTP bodyと期限後の非送信を実TCPで検証した。該当tests/clippy済み、この2変更のnative OSS再測定は未実施。実測と最終headの差を隠さない。

[構造化ledger](formation-autonomous-reasoning-20261001.json)、[raw manifest](evidence/formation-autonomous-20261001/raw-manifest.json)、[保存されたinput/output/receipt/usage](evidence/formation-autonomous-20261001/raw-evidence.tar.gz)。323証跡files＋manifest/66 accepted stepのinput/output digestを照合した。archive SHA-256: `c091950d136cb7662c57ed8890af2ab058a954e06685580d0dc8b52f77bc5ca5`。statusはowner scope/私有inventoryを省いたprojectionで、元status SHAも保存する。credential store/key、値、registration ID/assignmentのowner記録はexportしない。

旧`formation-coverage-100.json`のSHA-256は`560302a51a04fc08d9868a8dfb84bc2ac58666e14a0ee3533f181e502c98d45c`のまま。旧100件・shared prototype・失敗測定・UNKNOWN・予算履歴は上書きしていない。

## 未完了・未検証

1. 実OSSはSVGOMGだけがCodex/API双方のfresh PASSを達成。多アプリへの一般化・Kの機能要件拡張・実サービスcredentialは未検証。成功D seedやreceipt再利用はしていない。
2. Python sdist/native buildとnpm native lifecycle/rebuildは未対応。changedetection.io/Kuttの阻害要因として証拠を保存した。Kuttではbuild phaseのDB/変数・migrationとruntimeのstate接続も追加検証が必要。
3. Runtime deliveryのclaim/result retryは既定3でjournal化したが、Searchごとのcustom max_retriesはRuntime transportへ伝播していない。
4. requester status、claim/complete、source upload/download、変数解決、retained HTTPは凍結Search/roundの残時間でtimeoutを絞った。非HTTPのsource展開/処理の全区間とRuntime deliveryのbackoffに対するdeadline統合は未完了。保存済み結果の報告は期限後でも新実行をせず継続できる。
5. reasoning/inspection/validation/provider/attempt時刻は保存するが、Runtime build/launch/verification/cleanup/backoffすべての独立したmetricsは未完了。
6. actual provider call数/費用はrequester journalへ保存する一方、Coordinator集計にはround内複数callとの粒度差がある。
7. typed unsupported evidenceは保持・反復停止するが、Coordinator terminal stopが`no_progress`になる場合がある。修正不能の能力不足と案を作れないno_progressのterminal分類は分離が必要。
8. 一度取得したが送信slotに入らなかったinspection contextの再projection/再要求の扱いには改善余地がある。高度なdynamic import、多repo、未対応monorepo構成はこのv0の能力外。
9. nonsecret値の`artifact_embedding:false`もartifact guardで強制するよう修正し、値が含まれるartifactの拒否と明示許可時の通過を検証した。build設定変更時のrebuildとnative end-to-end測定は未検証。
10. 入力UI/CLI、外部サービスの実credential認証、expired/revokedのnative実行、Runtime uncertain disconnect、実サービスprovider timeout、縮小FAILの実OSS測定は未検証。

## API費用と100件再測定

今回17 actual API callsの保存usageから、cache miss peak予約価格で**65,621 USD micros（$0.065621）**を計上した。送信後未解決予約は0。従前の残予算564,906 → **499,285 micros（$0.499285）**。費用は実usageとpeak価格からの計上額で、invoice照合額ではない。Codex sessionのtoken/費用は公開されていないため不明として保存した。[使用したDeepSeek pricing](https://api-docs.deepseek.com/quick_start/pricing/)はinput $0.30/output $1.20 per millionのsnapshot。

新configのinput cap49,152/output2,048では1 callの保守的予約17,204 micros。100件を各1 callだけで測定する下限予約は$1.720400、追加必要額は**$1.221115**。1件12 callsの全枠なら$20.644800、追加**$20.145515**。1 callで成功するとの見積りではない。現行global call枠は24中17使用、残7であり、100件にはcall allocationの追加も必要。複数OSSのgateが全面成立しておらず、費用・call枠も不足するため新100件再測定は実施していない。
