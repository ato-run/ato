# Source Resultからの明示的な機能検証

Ato #1481のRust Source Result authority → API #731の保存・登録 → このowner CLIの順で利用する。Session/Skill #1479 → #1480 → 受入 #1485とは別トラック。API配備・remote migration・通常Run許可・公開は含まない。

通常のSource探索の固定Kに対するfresh PASSとretained publicationが保存されると、APIがRust検証済みのSource Resultを保存する。仮のSource InstanceやCapsule identityは作らない。owner認証の `GET /v1/runtime-network/exploration/:searchId/source-result` は、Rustが選んだsubmissionのattemptからその記録を読む。最新artifactを代用せず、保存途中の中断は同じPASSの記録から補う。owner値、ticket、capability、入力値は返さない。

ownerは安定したrequest/registration/Search IDと予算・権限・typed HTTP検証計画を保存し、明示的に承認する。`retained_ref` と `source_result_ref` はCLIが取得して固定する。計画に指定した場合は同じ値を要求する。これはownerの検証計画であり、agentのD応答をJSONで手渡す入口ではない。

```sh
ato form-verify --source-search-id SOURCE_SEARCH --api COORDINATOR \
  --owner-token-file PRIVATE_OWNER_SESSION --plan SAVED_FUNCTIONAL_PLAN \
  --authorize-functional-verification
```

planの必須fieldは `request_id`、`registration_request_id`、新しい `search_id`、`runtime_id`、`environment_id`、保存済みtargetと同じ `target_triple`、`permission: "functional_verification"`、`budget`、`ceiling`。budgetは既存SearchBudget型で、max_attemptsは1、modeはfirst_pass。追加の `acceptance` は既存FunctionalAcceptanceV1型。credential値、legacy source_instance_id、未知fieldをこのplanへ追加しない。秘密入力は既存 `ato form-input` の値stdin/credential選択経路で行う。

同じplanの再送は既存APIの冪等処理で、元のSearch/Run/lease/deadlineを返す。別Run Bでは同じregistration_request_idを保持し、別request_id/search_idを明示する。同じInstance/state namespaceを復元して新writer fenceを使う。別registrationは別Instance/namespaceになる。終了済みSearchの再開や元探索の枠・deadline初期化ではない。CLIの出力はSource/K/D/retained/profileと登録/Instance/Runの識別子・状態に限定し、Runtime grantを返さない。

検証はCLIの承認必須、Source/target固定、再送、同じHTTP Client経由のGET→POSTとsecret component非出力を確認した。APIでは通常Source anchor、NULL capsule_revision_idの登録、別namespace、既存writer/commit/restore/UNKNOWN/input経路、owner認証と普通のruntime_launch拒否をfixtureで確認した。migrationの既存Instance/state保持も確認した。

2026-10-05追補: 保存済みsuccessful Kutt Source Searchから、実full product API/RuntimeでSource Result → 自動functional Instance → Run A作成・stop/commit → Run B復元/削除をPASSした。[製品経路受入](formation-kutt-source-result-product-20261005.md)。旧private dispatch fixtureを読み替えず、新しいNative探索も消費していない。UNKNOWN supplementalは#1485の別Draft gate。独立API残3計画・100件測定・配備・remote migrationは実施していない。
