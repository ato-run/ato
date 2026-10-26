# 統合した通常Runnerの導入候補

> Status: historical evidence / past plan. Language: ja+en.
> Preserved content; relative navigation updated. Current [documentation map](../../../README.md).

source b6341239acea53132ed7583c44b32d731387d8dcを独立exportし、通常の
ato-connected-realization-workerをLinuxでbuildした。test executableではない。
ELF x86_64 GNU/Linux、50,479,808 bytes、SHA256
`444ac71b9195db34d3d1f49846e08c6e7d0d1466eff75e7e6bc78531c2b85360`。

Cargo0.1.0/rustc1.96.0、dev/debug0/incremental0/jobs2/offline/locked。
release/LTO版や導入済みRunnerではない。--helpは通過、--versionは未提供なので
Cargo package versionとsource/provenanceを分けている。全2489source/663inputsが
build前後一致。詳細なcommand/log/manifest hashは隣のprovenance JSONを参照。

Worker184成功/21既存ignored、netd121＋egress3＋ingress11＋lifecycle5＋resolver9成功、
pixel2既存ignored。計333成功/23 ignoredは複数の直列実行で確認し、1回の全target成功と
言い換えない。fixture7はWorkerの部分集合。all-target Clippy/fmt成功。
初回長TMPDIRの3SUN_LEN＋1Chrome CDP失敗、固定port40000のingress11失敗は元ログも保持。
短い専用TMPDIRと独立ingress11実行で全対象が成功、sourceやskipは変更していない。
テストが残した自分のexact netd子processだけ回収し、既存Runner/listenerは変更していない。

成果物はubuntu-sugamoの
/home/ekohsuke/ato-run/.tmp/integrated-runner-candidate-20261024/artifacts/に保持。
通常binary・provenance・SHA256SUMS、固定source/check script/logがある。
共有Linux Cargo targetの占有は解放した。インストール、Enrollment、credential参照、
service/flag/広告変更、実GPU/新Docker workload、外部公開は行っていない。

## 導入で維持する既存契約

調査で、HostBoundaryをato_managed/shared6slotsへ追加することは既存APIが明示拒否して
いると確認した。別のID/UIDだけで同じhostを別machineと扱ってはいけない。
HostBoundaryは1owner/1Runの実machine境界が前提で、共有環境の隔離完成とは主張しない。

既存経路はowner単回Attach Enrollment→user_managed/ato-user-runner→新しいcredential/
workroot/1slot→明示user_runner placement。従来credentialのコピーやmanaged audienceへの
偽装はしない。実machine条件とowner認証は未充足。Source jobは別のexact-job Workerが
必要で、通常Native Workerがsourcequeueを自動消費するとは扱わない。

さらに現在のlegacy Process自動選択もruntime_launchを要求するため、この新identityを
自動候補へ入れると既存appの配置に影響し得る。元のadvertisement/実行を既定で維持する
共通の『明示Runner配置のみ受付』制約を、既存capability/Lease経路で調査・追加する。
新Corekind/auth/tableは増やさず、K/launch_spec/digestへ配置情報を混入させない。
このbaseline候補はその後続制約を含まない。導入はしない。

## 後続候補 — 明示配置制約とTerminal実Chrome試験

前節はb6341239時点の履歴である。[ato #1544](https://github.com/ato-run/ato/pull/1544)と
[API #878](https://github.com/ato-run/ato-api/pull/878)で既定offの明示配置制約を実装した。
既存capability/Lease/認証identityを使い、広告されたRunnerを自動配置から除外し、保存済み
明示配置・現行Run/Lease・command digestと照合する。Workerは準備・storage副作用の前に
照合する。K/D/runtime spec/digestやSemantic Coreへ配置情報を含めない。
共有Runnerへの導入・設定変更は行っていない。

[ato #1545](https://github.com/ato-run/ato/pull/1545)の実行source
`b69b7cc6b6f142e75f8ec5bf46a290a972b6f8b7`から新しい通常Workerをprepareした。
Linux x86_64、50,490,512 bytes、mode0555、SHA256
`0586494e6cb3bac11a00624a6948fdeaf62122e4953746067d5c0c7cc4c7081c`。
dev/debug0/incremental0/offline/locked/jobs2。--helpで明示配置flagを確認した。
成果物はubuntu-sugamoの
`/home/ekohsuke/ato-run/.tmp/terminal-browser-20261024/bin/ato-connected-realization-worker-b69b7cc6b`。
662inputsのmanifest SHA256
`8a79c72f79b4314a75d1e730996b270ef44a81cbf29b929c5376b25a9d93e82d`は前後一致した。
通常Workerとテストbinaryは別物で、release/LTO版や導入済みserviceとは扱わない。

同sourceのWorkerテストはMac192成功/20ignored、Linux193成功/23ignored、all-target
Clippy/fmt成功。実Chrome Terminal試験は別のtest executableを使用し、元K・受動status・
再接続・出力保存・全回収が成功した。詳細はnative-terminal-browser-joint-20261024.md。
test binary SHA256は
`322fc33cbb733b7b278d3a474fc014e0825ca55c55a11cc5cd5cec2a0e2fd69c`。
独立postflightで両binaryとsource/launcher不変、所有container/network0・listener/PID終了を
確認した。共有Linux Cargo占有は解放済みである。

API codeのstaging配備は完了したが、marker広告Runner0であり、機能の導入完了ではない。
owner手動認証と実machine境界は未充足、旧queue先頭での不正Leaseによる停止を避けるには
新identity/空queueが必要である。credential流用・共有6slot変更・新Run・本番操作はない。
