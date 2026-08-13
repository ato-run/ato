# Agent Interface Spec

**Status:** Draft  
**Last Updated:** 2026-02-23

## 1. Scope

本書は Agentic ecosystem 連携の外部I/Fを定義する。
ExecutionPlan の隔離契約は `docs/specs/EXECUTIONPLAN_ISOLATION_SPEC.md` を正本とする。

## 2. Skill Translation

- `ato run --from-skill` により `SKILL.md` 等を `ExecutionPlan` に変換する。
- 変換後は通常の consent/hash 判定を適用する。

## 3. Structured Failure for LLMs

- Fail-Closed 時は stderr JSONL を出力する。
- 診断コード体系は `docs/specs/ATO_ERROR_CODES.md` を参照する。

例:

```json
{
  "level": "fatal",
  "code": "ATO_ERR_POLICY_VIOLATION",
  "message": "Network access denied",
  "resource": "network",
  "target": "api.evil.com"
}
```

## 4. Reference Agent

- 公式リファレンス実装 `ato-agent` は ExecutionPlan/fail-closed/consent を前提に実装する。
