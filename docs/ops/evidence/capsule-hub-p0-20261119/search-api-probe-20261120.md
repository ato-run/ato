# staging Hub一覧APIの検索挙動（2026-11-20）

再現手順:

```sh
curl -s -H 'User-Agent: ato-cli/0.9.6-stg' \
  'https://staging.api.ato.run/v1/hub/capsules?q=interop-static&limit=50&offset=0' | jq '{total, n: (.items|length), applied}'
```

測定結果:

| クエリ | items | total | applied | 先頭3件 |
|---|---:|---:|---|---|
| q=interop-static | 50 | 65 | なし | siyuan, redmine, gitea |
| q=openhands | 50 | 65 | なし | siyuan, redmine, gitea |
| q=gitlab | 50 | 65 | なし | siyuan, redmine, gitea |
| (指定なし, offset=50) | 15 | 65 | なし | code-server, nocodb, keycloak |

- 一覧APIは `q` を解釈していない。どのクエリでも total は65のまま、先頭50件も同じ並びになる。
- `interop-static` と `openhands` は2ページ目以降にしか存在しないため、1ページ目のクライアント絞り込みでは0件になる。画面で「追加読み込み」を押した後にだけ見つかる。
- 画面側は読み込み済みitemsに対して絞り込む（`PAGE_SIZE = 50`）。APIに検索パラメータがないため、51件目以降は検索対象にならない。
- 修正は ato-api PR #1324 で行う（`q`/`category` をSQLのWHEREに適用し、`total` はフィルタ後、`applied` を応答に追加）。PWA側は hub-p0-ui-ux で追従する。
