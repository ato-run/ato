# 保存済みKutt Source Searchからの製品経路受入

既存successful Search → Rust Source Result → ownerの `ato form-verify --authorize-functional-verification` → 自動functional registration → 通常StateServiceのRun A/Bを、full product APIの隔離Miniflareと実Linux aarch64 Runtimeで確認した。手動Source Instance・private dispatch namespace・fixtureのexecution代用は使っていない。stagingやPWA実ブラウザは別ゲート。

元Search `search_8bcf155bb690432792c7a631806cbe4a` のaccepted PASS `01M4012EE937EENPM82B9BJKKK` とready retainedを、元D1/R2を保存したコピーから利用した。Source Resultは `sha256:ba4193a23b602d3ea760a177f256ea924603c5747005e3fba8b12bbbcc852deb`。Source/K/D/retained/profile/targetは元PASSの保存記録と一致。PASS result JSONのSHA-256 `138aa74f824a847d71b5d8b6c4295e0ac78175af8d837b041fa88d56fc653b43` と元deadline `2026-10-03T05:06:04.881Z` は変わっていない。Source再探索、Native起動、D round、LLM API callはいずれも追加0。

同registrationのfunctional Instance `cinst_01M44EPENESDAYW1SXSSZJ0F2C` とState slot `isslot_01M44EPEW80AFMV6GFM1RVNKQ2` を使用した。最終Aはlogin 200 → link作成201 → 所有者一覧のID/target照合200 → short link 302/Location照合。最終Bは新しいJWTでlogin200 → 同targetの復元照合200 → delete200 → total=0照合200。いずれも同じfrozen Kに対するfully_satisfied fresh receipt、confirmed stop、commit、durable result ACKがある。fenceは4→5、Bのparent revisionはAのcommit。registration/Run計画の再送は同Instance・Schema・Run・lease・Searchを返し、別Instanceやdeadline更新を作らない。

最終active writer/quarantine/attempt・transfer・expanded・stored reservation/対象encrypted input valueはすべて0。26件の今回scopeの入力を、ownerの既存variable DELETEサービスで明示失効させた。自動cleanupを証明したとは表示しない。CLI自身のACK欄はunknownのまま。ACKは別のRuntime保存済みaccepted=true応答とclosed journalで確認した。

前段2失敗を保持している。最初のattemptはretained resumeにもDのDependencies/Build networkを開こうとして、Runtimeの0 network予算で起動前に拒否された。full Dの権限検査を維持し、retained resumeにはRuntime phaseだけのnetwork requirementを渡すよう修正した。実際に外部Runtime egressを宣言するDは0予算で拒否する。not_startedを確認して既存lease recoveryを使い、writerを安全に解放した。

次のRunはroot Kのfresh PASSとlogin/create/list/commitを取得したが、User-Agentなしの追加redirect検査が500でfunctional全体はFAILだった。同namespaceでrestore/deleteのcleanup RunをPASSさせ、既存typed header bindingでUser-Agentを明示した最終A/Bを両方PASSさせた。Source・D・Kを変更していない。計5 functional Search/attempt（起動前失敗1、機能失敗1、cleanup PASS1、最終A/B PASS2）。初回から全PASSとは報告しない。新しいunique appの探索成功数には加算しない。

普通のRun、Public/Install×shared/separateの5 HTTP入口は `409 source_result_functional_only`。公開token・fork・追加Instanceは0。APIはordinary profile/launch、invite発行・既存invite利用、direct forkをidentity/予約作成前に拒否する。旧inviteとdirect forkの詳細境界は実DB contract testで検証している。ordinary capsule publication全体のブラウザ操作を実施したとは表示しない。

実行Runtime pin `c5719f69ea2ef009f3f0d677518b843655b3e196`、API final code pin `34b44f842490279fbd10cca940b0768a10f52a2a`（guard実装 `c7330bf2`）、Source Result authority `662a59d29515641f3ca643b15499abcd4c5adcb7`。owner CLI binaryは `3f85adf0` で、form-verify/inputのcodeはc571と同一。APIは実行後のcommitから再buildし、実行したindex.js/全WASMとbyte-identicalを確認した。後続headとの差は証跡のみ。[全識別子・receipt・budget・ACK・失敗記録](evidence/formation-kutt-source-result-product-20261005.json)。

API関連202test・typecheck PASS（ローカルNode25.6.0）。Runtime network guardの3test・strict worker clippy PASS。c571のUbuntu/macOS/Windows CLI CI、architecture、snapshot PASS。API c733 CIはNode22.23.3でexact base d3ccb8bbと同じworkflow/lockfile、同じActivity/CORS 13失敗名、追加失敗名0、instance-state-sync PASS、full-serial SKIPPED。原因解明や全CI greenとは表示しない。[CI比較](evidence/formation-source-result-CI-comparison-20261005.json)。

検証emailはprivate生成し、保存済みowner CLI/status/診断/計画/観測102ファイルでemail/password/aliasの値本体の出現0を確認した。このarmには新Native tool transcriptがないため、その全操作非露出の再受入とはしない。DBコピーへの0317〜0320適用は隔離検証のみ。配備、共有環境のremote migration、flag、通常Run許可、公開、fork、100件再測定は行っていない。独立APIの残3計画、OCI bind、残call/金額の台帳は変更していない。

旧Native campaignは13/12超過で終了のまま。audit SHA-256 `91262a72bb27ebcc03107b7d0b6081135be78e6868e1d49f7ad5b70ef9470197` は不変。旧WBO UNKNOWNは再実行していない。今回のSource Result製品gateはPASSし、#1481/API #731はReady候補。UNKNOWN supplementalは#1485の独立Draft gateで、20分枠と実30分失効の調整待ち。残予算の現在価格/account確認や推論費用ゼロの推定はしていない。
