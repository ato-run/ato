# Native受入の開始境界ガード

`native-session-budget.py`はaudit専用だった。新しいowner入口
`scripts/acceptance/coverage/native-session-owner.py`は、auditをSearch作成と
Native launchの直前に必ず通す。Coordinatorの一般budget仕様は変更しない。

owner planは`ato.native-acceptance-campaign/1`。measurement_idと全aggregate
ceilingを固定する。既存campaignから引き継ぐ場合、historical auditのdigestと
累計消費を不変floorにする。未登録のidentifierも過去auditのSearch消費に含む。
新campaignのzero開始は明示した`initial_campaign:true`だけに限定する。
旧13/12 campaignを新しいzero ledgerへ読み替える操作は行わない。

```sh
python3 scripts/acceptance/coverage/native-session-owner.py \
  --plan OWNER_PLAN --ledger OWNER_LEDGER --ato PINNED_ATO initialize
python3 scripts/acceptance/coverage/native-session-owner.py \
  --plan OWNER_PLAN --ledger OWNER_LEDGER --ato PINNED_ATO audit
python3 scripts/acceptance/coverage/native-session-owner.py \
  --plan OWNER_PLAN --ledger OWNER_LEDGER --ato PINNED_ATO start \
  --source SOURCE --exploration-config FROZEN_PLAN --api COORDINATOR \
  --token-file OWNER_TOKEN_FILE --exact-runtime OWNED_RUNTIME \
  --work-root FRESH_REQUESTER --connection FRESH_CONNECTION \
  --max-attempts 4 --deadline-seconds 1800 \
  --max-transfer-bytes TRANSFER_CAP --max-expanded-bytes EXPANDED_CAP \
  --max-stored-bytes STORED_CAP
python3 scripts/acceptance/coverage/native-session-owner.py \
  --plan OWNER_PLAN --ledger OWNER_LEDGER --ato PINNED_ATO native \
  --connection SAVED_CONNECTION \
  --native-launcher skills/ato-formation-explore/scripts/native-session.py -- \
  --agent SELECTED_AGENT --binary PINNED_NATIVE --version EXACT_VERSION \
  --mcp-binary PINNED_MCP --output FRESH_NATIVE_OUTPUT --socket FRESH_SOCKET
```

pathはownerが選び、秘密値をargvへ置かない。Native固有の認証/設定引数は共通
launcherの実在flagに従う。新担当者がresponse JSONを手動で受け渡す手順はない。
`native`は登録済みの同一接続だけを選び、他のconnection引数を拒否する。
UNKNOWN再接続は同じentryで`--reconcile-only`を渡し、新Search予約を作らない。

排他lock下で、Coordinatorのallocated round、全exchange、used+reserved attempt、
inspectionと経過時間を読み、保存済み最大値と照合する。cancelled/unanswered round、
予約attempt、古いsnapshotで消費を減らさない。active Searchは未使用の最大scopeも
保持する。新Search全体をconfigとCLI budgetから予約し、実CLIが読むconfig bytesを
owner領域へ固定する。予約はprocess作成より先にfsyncする。開始途中のcrash、
不完全status、binding変更、plan変更、消費超過はfail closedでowner照合へ戻す。
終了済みSearchの未使用予約だけを解放し、履歴と元deadlineを保持する。

関連13test PASS。実際の旧campaign auditを新入口へ接続すると、同じD round
13/12で拒否された。Source/Nativeは新規に開始していない。実UNKNOWNとKuttの
追加Native枠は確認中であり、上限を無断で増やしたり台帳を初期化したりしない。
API残3計画・100件再測定・配備・remote migrationは今回の入口に含まない。
