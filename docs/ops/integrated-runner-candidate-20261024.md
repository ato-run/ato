# 統合した通常Runnerの導入候補

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
