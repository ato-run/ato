# Ato 初期ローンチ向け GitHub repo と self-hosted AI アプリ横断調査

## Executive summary

結論から言うと、Ato の初期モメンタムを最も作りやすいのは、**巨大な star 数**、**すぐ見える localhost UI**、**Docker Compose か docker run で動く配布形態**、そして **README の手順が長く Ato の価値を一言で説明しやすい** repo です。今回の横断調査では、その条件を最も強く満たしたのが **n8n、Open WebUI、Dify、Langflow、Flowise、Langfuse、LobeHub、NocoDB、Hoppscotch、Stirling-PDF** でした。GitHub の規模だけ見ても、n8n は 189k stars、Langflow は 149k、Dify は 142k、Hoppscotch は 79.3k、Stirling-PDF は 79.1k、LobeHub は 77.5k、OpenHands は 74.5k、Open WebUI は 65.8k、NocoDB は 63.1k、Flowise は 53k です。citeturn46view0turn47view1turn31view0turn26view2turn24view0turn46view2turn45view2turn0view0turn25view1turn46view1

ただし、**「最も star が多い repo」=「最初に recipe 化すべき repo」ではありません**。Ato の初期デモでは、**起動成功率・説明のしやすさ・UI の見栄え**が同じくらい重要です。その観点では、**Open WebUI、n8n、Dify、Flowise、Langfuse** が最初の五本として最もバランスが良いです。Open WebUI と n8n は一発起動系の見本になりやすく、Dify と Langfuse は「長い Compose / `.env` / 複数サービス管理」を Ato が吸収する価値を最もドラマチックに見せられます。Flowise は AI workflow UI として、起動後すぐに触れる体験が強いです。citeturn3view0turn4view0turn35view0turn34view0turn33view1turn33view2turn36view1turn39view0turn42view0turn42view1

Ato の初期カタログは、ひとつの軸で揃えるより、**三つのレーン**で見せるのが強いです。  
第一に、**AI アプリの顔**になる Open WebUI / Dify / Flowise / LobeHub / Langfuse。第二に、**セットアップの痛みを Ato が消す** Dify / Langfuse / Twenty / Appsmith / Ragflow のような高摩擦スタック。第三に、**すぐ UI が見える easy wins** である Memos / Linkwarden / Stirling-PDF / Actual / Homepage / Uptime Kuma です。初期の SNS 投稿やデモでは、第一と第二を混ぜると「AI 文脈」と「setup pain の解消」が同時に伝わります。citeturn0view0turn31view0turn46view1turn46view2turn46view3turn26view0turn25view3turn18view2turn20view2turn21view2turn24view0turn20view1turn22view0turn29view3

逆に、**人気は高いが初期ローンチには危険**なカテゴリもはっきりしています。**Coolify / Dokploy** は repo の性質上、ホスト側のコンテナ制御を伴う PaaS で、Ato 初期 recipe の「閉じた再現実行」と相性が悪い推定です。**ComfyUI / AUTOMATIC1111 / InvokeAI** は非常に人気ですが、初期 default recipe としては GPU/モデル配布の重さが大きいです。**Supabase / PostHog / Immich / Ragflow** は人気と戦略価値は高いものの、最初の波には多サービス・多ボリューム・多 secrets の複雑さが重いです。さらに **maybe** は 2025-07-27 に、**h2oGPT** は 2026-02-26 にアーカイブ済みで、初期の「生きているカタログ」には向きません。citeturn30view1turn30view2turn15view3turn17view0turn17view1turn26view1turn29view0turn22view1turn18view2turn20view0turn17view2

Ato 側の機能要件もかなり明確です。**Compose の profile 切替**、**`.env.example` からの env scaffold**、**初回セットアップ URL の表示**、**named volume の lock と再接続**、**HTTP readiness / healthcheck 待ち**、**provider preset**、**recipe の variant 管理**の六つがあれば、今回の上位群のかなり多くを高品質にカバーできます。Dify は `localhost/install` まで導く必要があり、Langfuse は Postgres / Redis / ClickHouse / MinIO を健康状態つきで束ねる必要があり、Open WebUI と n8n は一方で「named volume + port + env preset」だけで体験価値が出ます。citeturn34view1turn33view1turn33view2turn41view0turn42view1turn42view2turn42view3turn35view0turn3view0

## 調査方法と判断基準

評価は、ユーザー指定の重みをそのまま採用しつつ、初期ローンチ用途に合わせて解釈しました。**Popularity / Momentum** は GitHub stars、release の新しさ、repo の更新量、README 上のコミュニティ導線を主に見ました。**Ato Fit** は Docker Compose / docker run / install.sh / source-native の可視性、localhost UI の明確さ、永続 volume の切り分けやすさ、privileged / host network / Docker socket に寄りにくいかで見ています。**Demo Value** は「Ato が README の痛みを消した」と一言で言えるかを重視し、**Technical Feasibility** は startup time・env surface・multi-service dependency・初期化フローの複雑さを見ています。**Strategic Value** は self-hosted AI / local-first / remixable catalog 文脈との相性です。

一次情報は、可能な限り **GitHub README、repo ルート、docker-compose / Dockerfile、install.sh / setup script、release 情報、LICENSE、repo 構成**を優先しました。Docker image の multi-arch / arm64 は、今回のパスでは **明示的に確認できた場合だけ断定し、それ以外は未確認** としました。`Services` や `Runtime shape` は、README / compose が明示しているものはそのまま記載し、repo ルートの `docker`, `deploy`, `.env.example`, `docker-compose` ディレクトリから引いたものは**保守的な推定**として扱っています。

表の凡例は次の通りです。  
**Momentum / Ato fit / Demo value** は `H / M / L`。  
**Difficulty** は `A / B / C / D` で、`A` は早期に recipe 化しやすい、`D` は初期モメンタム用途には不向き、を意味します。  
**Recipe path** は `run`, `compose`, `install.sh`, `source`, `manual` で簡記しています。

## 候補ロングリスト

| Rank | Repo | Stars | Category | Momentum | Ato fit | Runtime shape | Services | Recipe path | Difficulty | Demo value | Strategic reason | Main risk |
|---|---|---:|---|---|---|---|---|---|---|---|---|---|
| 1 | open-webui/open-webui citeturn0view0 | 65.8k | Self-hosted AI chat UI | H | H | mixed | single | run / install.sh | A | H | local AI の顔 | provider env / model pull |
| 2 | n8n-io/n8n citeturn25view0 | 189k | Workflow builder with AI | H | H | docker run | single | run | A | H | AI workflow の王道 | secrets / webhooks |
| 3 | langgenius/dify citeturn7view1 | 142k | AI workflow / RAG | H | H | compose | complex | compose | B | H | README 長い→Ato映え | `.env` と初回 install |
| 4 | langflow-ai/langflow citeturn47view1 | 149k | Agent / workflow UI | H | M | mixed | single-to-mixed | manual / compose | B | H | AI builder 文脈が強い | docs 二次確認必要 |
| 5 | FlowiseAI/Flowise citeturn14view1 | 53.0k | Agent / workflow UI | H | H | mixed | single | run / compose | A | H | すぐ触れる AI canvas | env 面が広い |
| 6 | langfuse/langfuse citeturn14view0 | 27.7k | LLM observability | H | M | compose | complex | compose | B | H | 高摩擦 stack の代表 | secrets + ClickHouse/MinIO |
| 7 | lobehub/lobehub citeturn43view0 | 77.5k | AI UI / agent operator | H | M | mixed | app+infra | install.sh / compose | B | H | AI UI と話題性 | setup.sh 依存が強い |
| 8 | nocodb/nocodb citeturn25view1 | 63.1k | Database UI | H | H | compose | app+db | compose | B | H | “Airtable 代替” は説明しやすい | DB 選定 / migration |
| 9 | hoppscotch/hoppscotch citeturn26view2 | 79.3k | API tool | H | M | mixed | app(+backend) | manual / compose | B | H | Postman 代替の見栄え | backend/auth 構成差 |
| 10 | OpenHands/OpenHands citeturn45view2 | 74.5k | AI agent GUI | H | M | compose | complex | compose | C | H | Devin 類似の強い話題性 | resource-heavy |
| 11 | BerriAI/litellm citeturn47view0 | 47.9k | AI gateway / OpenAI proxy | H | M | mixed | app+db 推定 | manual / compose | B | M | provider 管理の中核 | config 面が広い |
| 12 | Stirling-Tools/Stirling-PDF citeturn24view0 | 79.1k | Productivity / PDF UI | H | H | docker | single | run / compose | A | H | 一発起動デモが強い | file storage 設計 |
| 13 | usememos/memos citeturn20view2 | 59.9k | Notes | H | H | mixed | single | run / source | A | H | 初期カタログの定番 | auth / backup |
| 14 | toeverything/AFFiNE citeturn21view1 | 68.6k | Knowledge base / workspace | H | M | mixed | app+db 推定 | manual / compose | B | H | Notion 代替の訴求力 | stack が大きい |
| 15 | actualbudget/actual citeturn20view1 | 26.6k | Personal finance | M | H | mixed | single | run / source | A | H | local-first の良い見本 | import / sync 周り |
| 16 | linkwarden/linkwarden citeturn21view2 | 18.4k | Bookmarks / read later | M | H | compose | app+db | compose | B | H | bookmark 系の見栄えが良い | archiver / storage |
| 17 | pocketbase/pocketbase citeturn29view2 | 58.5k | Backend UI | H | H | native | single | source | A | H | 1-file backend の対比が強い | Docker 主体ではない |
| 18 | louislam/uptime-kuma citeturn29view3 | 87.1k | Monitoring UI | H | H | mixed | single | run / source | A | H | 誰でも理解できる UI | 通知設定 |
| 19 | danny-avila/LibreChat citeturn16view0 | 24.1k | Self-hosted AI chat | H | M | compose | app+db 推定 | compose | B | H | ChatGPT 代替の理解容易さ | provider/auth 設定 |
| 20 | Mintplex-Labs/anything-llm citeturn14view3 | 46.3k | Local AI / RAG | H | M | mixed | single-to-mixed | manual / compose | B | H | local RAG の顔 | model / embedder 選択 |
| 21 | infiniflow/ragflow citeturn18view2 | 81.0k | RAG engine | H | M | compose | complex | compose | C | H | RAG 文脈の話題性大 | 依存サービス多い |
| 22 | Budibase/budibase citeturn25view2 | 27.9k | Internal tool builder | M | M | compose | complex 推定 | compose | B | H | internal tools 枠で強い | stack が大きい |
| 23 | AppFlowy-IO/AppFlowy citeturn21view0 | 71.0k | Notes / workspace | H | M | mixed | app+infra 推定 | manual | B | H | Notion 代替で広く刺さる | desktop / web 文脈混在 |
| 24 | appsmithorg/appsmith citeturn25view3 | 39.9k | Admin / internal tools | H | M | mixed | complex 推定 | compose | B | H | setup pain を吸収しやすい | env / DB / upgrade |
| 25 | twentyhq/twenty citeturn26view0 | 46.0k | CRM / admin UI | H | M | mixed | app+db+redis 推定 | compose | C | H | “Salesforce 代替” は強い | migration / worker |
| 26 | bytebase/bytebase citeturn28view1 | 14.0k | Database DevSecOps UI | M | H | docker | single | run / compose | A | M | DB team 向けの見栄え | DB 接続前提 |
| 27 | dgtlmoon/changedetection.io citeturn21view3 | 31.7k | Website monitor | H | H | docker | single | run / compose | A | H | 一発で価値が伝わる | browser fetcher 周り |
| 28 | gethomepage/homepage citeturn22view0 | 30.2k | Personal dashboard | H | H | docker | single | run / compose | A | M | カタログ埋めに最適 | config-first で派手さは中 |
| 29 | blinkospace/blinko citeturn20view3 | 10.4k | AI notes | M | M | compose | app+db 推定 | compose | B | H | AI ノートの差別化 | schema / storage |
| 30 | paperless-ngx/paperless-ngx citeturn22view3 | 40.9k | Document manager | H | M | compose | app+db+broker 推定 | compose | B | H | scanner / archive 需要が太い | OCR / ingest の重さ |
| 31 | immich-app/immich citeturn22view1 | 101k | Photo manager | H | M | compose | complex | compose | C | H | 非AI層にも強い人気 | storage / ML / resources |
| 32 | photoprism/photoprism citeturn22view2 | 39.7k | Photo manager | H | M | compose | app+db/search 推定 | compose | C | H | 写真 UI はデモ映え | indexing 時間 |
| 33 | directus/directus citeturn27view0 | 35.8k | Headless CMS / admin UI | H | M | docker | app+db 推定 | compose | B | H | DB→UI 変換デモが強い | DB 依存 |
| 34 | usebruno/bruno citeturn26view3 | 44.3k | API tool | H | L | native | desktop-centric | source | C | M | জনপ্রি Postman 代替 | localhost web UI が弱い |
| 35 | metabase/metabase citeturn27view2 | 47.4k | BI dashboard | H | M | docker | single-to-app+db | run / compose | B | H | BI は見栄えが良い | DB 接続が必要 |
| 36 | grafana/grafana citeturn27view1 | 73.9k | Monitoring / dashboards | H | M | docker | single | run / source | A | H | 非常に有名 | datasource を繋ぐ必要 |
| 37 | apache/superset citeturn27view3 | 72.9k | BI / data viz | H | M | compose | complex | compose | C | H | 強い知名度 | redis/db/worker |
| 38 | getredash/redash citeturn28view0 | 28.6k | BI dashboard | M | M | compose | app+db+redis 推定 | compose | C | M | setup pain を消しやすい | 古さ / maintenance |
| 39 | supabase/supabase citeturn26view1 | 103k | Dev platform | H | L | compose | very complex | compose | D | H | 人気は圧倒的 | 初期レシピには重すぎる |
| 40 | mindsdb/minds-platform citeturn17view3 | 39.2k | AI platform | H | M | mixed | complex | manual / compose | C | M | applied AI の認知 | 役割が広すぎる |
| 41 | continuedev/continue citeturn18view1 | 33.3k | AI dev tooling | H | L | source/native | IDE-centric | source | D | M | 人気は高い | Ato の localhost UI とズレる |
| 42 | getzep/zep citeturn18view0 | 4.6k | Memory layer | M | L | source/mixed | service | manual | D | L | memory 文脈の補完 | UI 訴求が弱い |
| 43 | run-llama/llama_deploy citeturn18view3 | 2.1k | Agent deployment infra | M | L | mixed | service+workers 推定 | manual | C | L | LlamaIndex 文脈の補完 | end-user UI が弱い |
| 44 | maybe-finance/maybe citeturn20view0 | 54.1k | Personal finance | M | M | mixed | app+db 推定 | manual | D | H | star は大きい | archived |
| 45 | invoke-ai/InvokeAI citeturn17view1 | 27.2k | Image generation UI | H | L | native/docker | single | source / run | C | H | 画像 demo は強い | GPU / model weight |
| 46 | Comfy-Org/ComfyUI citeturn15view3 | 114k | Image workflow UI | H | L | source/native | single | source | D | H | 圧倒的人気 | GPU-heavy |
| 47 | AUTOMATIC1111/stable-diffusion-webui citeturn17view0 | 163k | Image generation UI | H | L | source/native | single | source | D | H | 圧倒的知名度 | GPU-heavy |
| 48 | h2oai/h2ogpt citeturn17view2 | 12.0k | Local AI app | M | L | mixed | complex | manual | D | M | private GPT 文脈 | archived |
| 49 | logto-io/logto citeturn28view3 | 12.1k | Auth infra | M | M | compose | app+db 推定 | compose | B | M | AI app auth 基盤 | demo がやや地味 |
| 50 | PostHog/posthog citeturn29view0 | 34.6k | Product analytics | H | L | compose | very complex | compose | D | H | developer tool として強い | stack が重い |
| 51 | coollabsio/coolify citeturn30view1 | 55.7k | Self-hosted PaaS | H | L | mixed | host-control 推定 | manual | D | H | 人気は非常に高い | host-control 系で初期不向き |
| 52 | Dokploy/dokploy citeturn30view2 | 34.2k | Self-hosted PaaS | H | L | mixed | host-control 推定 | manual | D | H | 伸びが強い | host-control 系で初期不向き |

## モメンタム最優先の上位候補

以下は、**人気・話題性・Ato での見せやすさ**を合わせて、初期ローンチの主戦場にするべき上位十件です。`services / targets / state / env / port` は、README / compose で明示されているものはそのまま、そうでないものは **推定** と明記しています。

**Open WebUI**。**なぜ効くか**: Open WebUI は 65.8k stars、直近 release も 2026-05-21 で、README から docker run と install.sh の両方が見え、しかも OpenAI-compatible と各種 LLM runner を前提にした self-hosted AI UI として理解されやすいです。**Ato での嬉しさ**: `docker run -d -p 3000:8080 -v open-webui:/app/backend/data` を recipe 一枚に落とせるので、「LLM UI を一発で出す」体験を最短で作れます。**想定 recipe**: 単一コンテナ variant、Ollama/OpenAI preset variant、install.sh variant の三本立て。**推定 resources**: `target=web`, `port=3000`, `state=open-webui volume`, `env=OPENAI_API_BASE_URL / OPENAI_API_KEY / OLLAMA_BASE_URL`。**最初の検証コマンド**: `ato run github.com/open-webui/open-webui --oci-docker-run`。**blocker**: provider 不在時の初回 UX と Apple Silicon / Docker Desktop 端の安定性。**Ato 側の必要機能**: env preset、named volume lock、HTTP readiness、初回 URL 表示。**優先度**: 最優先。citeturn0view0turn3view0turn4view0turn6view0

**n8n**。**なぜ効くか**: 189k stars と圧倒的な知名度に加え、README が `docker volume create` と `docker run -p 5678:5678` を明示し、AI ネイティブ workflow platform としても押し出しています。**Ato での嬉しさ**: Webhook URL や credentials 以外の面倒を消して、まず UI を即表示できるのが大きいです。**想定 recipe**: 単一コンテナ + 永続 volume の軽量 recipe を first-class にするのが正解です。**推定 resources**: `target=editor`, `port=5678`, `state=/home/node/.n8n`, `env=WEBHOOK_URL / N8N_HOST / basic auth optional`。**最初の検証コマンド**: `ato run github.com/n8n-io/n8n --oci-docker-run`。**blocker**: OAuth / SMTP / webhook など“外部に出る”設定。**Ato 側の必要機能**: persisted volume・port 表示・secret injection。**優先度**: 最優先。なお、n8n は 2026-05-21 にも最新 release が出ており活発ですが、2026 年には auth bypass を含む重大脆弱性報道もあったため、default security posture を recipe に含めるべきです。citeturn35view0turn46view0turn8news6

**Dify**。**なぜ効くか**: 142k stars、2026-05-19 の最新 release、AI workflow / RAG / agent / model management をひとつにまとめた flagship で、README 自体が「Docker Compose が easiest path」と明確です。**Ato での嬉しさ**: Dify は Ato の価値を最も説明しやすい repo のひとつです。`cd docker; cp .env.example .env; docker compose up -d` の長さと、`localhost/install` までの初期化を全部 recipe に畳めるからです。**想定 recipe**: `minimal` と `vector-enabled` の二変種。`minimal` は `api + web + nginx + postgres + redis`、`vector-enabled` は profile で Weaviate 等を追加。**推定 resources**: `port=80/443`, `state=postgres/redis/weaviate volumes`, `env=.env scaffold + provider keys`, `target=/install`。**最初の検証コマンド**: `ato run github.com/langgenius/dify --oci-compose docker/docker-compose.yaml`。**blocker**: env 面が広く、vector DB の選択肢が多いこと。**Ato 側の必要機能**: Compose profile 選択、`.env.example` からの scaffold、依存サービス readiness DAG、初回 install URL 表示。**優先度**: 最優先。citeturn34view0turn34view1turn33view1turn33view2turn33view3turn31view0

**Flowise**。**なぜ効くか**: 53k stars、2026-04-14 に最新 release、しかも README が Docker Compose と docker run の両方をきれいに書いています。**Ato での嬉しさ**: Flowise は「AI workflow canvas が一発で出る」デモに向いています。Dify より軽く、Open WebUI より builder 文脈に寄っているため、Ato の AI catalog を厚く見せられます。**想定 recipe**: `single-container` を標準、外部 DB / Redis を env で繋ぐ variant を後追い。**推定 resources**: `port=3000`, `state=~/.flowise`, `env=PORT / DATABASE_* / REDIS_* / JWT_* / SMTP_*`, `health=/api/v1/ping`。**最初の検証コマンド**: `ato run github.com/FlowiseAI/Flowise --oci-compose docker/docker-compose.yml`。**blocker**: env surface は広いが、minimal path は単一サービスでかなり扱いやすいです。**Ato 側の必要機能**: HTTP healthcheck 待ち、home-dir volume の named volume 化、env variant。**優先度**: 最優先。citeturn36view1turn39view0turn39view1turn46view1

**Langfuse**。**なぜ効くか**: 27.7k stars と AI devtools 文脈では十分大きく、2026-05-21 時点でも活発です。しかも README / Compose から見える stack が、Ato の“pain killer”として非常にわかりやすいです。**Ato での嬉しさ**: Langfuse は単なる observability tool ではなく、**Postgres + Redis + ClickHouse + MinIO + web + worker** をまとめて起動し、しかも secrets を埋める必要があるため、recipe が一番映えます。**想定 recipe**: `minimal-observe` を first wave にして、公開ポートは `3000` と必要なら `9090` のみに絞る。**推定 resources**: `port=3000`, `state=clickhouse/minio/postgres volumes`, `env=NEXTAUTH_SECRET / DATABASE_URL / CLICKHOUSE_* / REDIS_AUTH / MINIO_* / SALT / ENCRYPTION_KEY`。**最初の検証コマンド**: `ato run github.com/langfuse/langfuse --oci-compose docker-compose.yml`。**blocker**: secret 面と internal service の readiness。**Ato 側の必要機能**: multi-service lock、secret generation、restricted port exposure、healthcheck ordering。**優先度**: 最優先。citeturn46view3turn41view0turn42view0turn42view1turn42view2turn42view3

**LobeHub**。**なぜ効くか**: 77.5k stars、2026-05-18 に最新 release、README に Product Hunt バッジが出ており、AI UI としての話題性が高いです。Docker self-hosting も README から辿れます。**Ato での嬉しさ**: LobeHub は `bash <(curl -fsSL https://lobe.li/setup.sh)` と `docker compose up -d` を recipe に閉じ込めることで、「install script を lock して再現する」Ato のメッセージを最も素直に見せられます。**想定 recipe**: install.sh capture variant と compose variant。**推定 resources**: `state=lobehub-db directory / compose volumes`, `env=OPENAI_API_KEY optional/prototyping required`, `targets=web UI + local DB/infrastructure`, `port=実装時要確定`。**最初の検証コマンド**: `ato run github.com/lobehub/lobehub --oci-install-sh https://lobe.li/setup.sh`。**blocker**: setup.sh 依存と env 生成の透明性。**Ato 側の必要機能**: install script freeze、env scaffold、generated compose capture。**優先度**: 高。citeturn44view0turn46view2turn43view0

**NocoDB**。**なぜ効くか**: 63.1k stars の “Airtable alternative” は説明が非常にしやすく、`docker-compose` ディレクトリごと repo に置かれています。**Ato での嬉しさ**: README 追従ではなく、repo 内の `docker-compose` を自動検出して recipe 化するデモができます。**想定 recipe**: Compose variant を標準、外部 DB 接続 variant を後追い。**推定 resources**: `services=app+db`, `state=DB volume`, `env=DB_*`, `port=web UI`, ただし port は実装時に docs で最終確認。**最初の検証コマンド**: `ato run github.com/nocodb/nocodb --oci-compose docker-compose`。**blocker**: DB backend の選択肢。**Ato 側の必要機能**: compose directory discovery、port auto-detection、DB preset。**優先度**: 高。citeturn25view1

**Hoppscotch**。**なぜ効くか**: 79.3k stars で、API tool として Postman 代替を誰でも理解できます。repo は monorepo だが、on-prem / offline / web / desktop / CLI まで持っていて話題化しやすいです。**Ato での嬉しさ**: API tool は UI の第一印象がわかりやすく、Ato で localhost に即立ち上がると demo に強いです。**想定 recipe**: web/on-prem の最小構成 recipe。**推定 resources**: `services=app(+backend)`, `env=auth/backend vars`, `port=web UI`, `state=optional`。**最初の検証コマンド**: `ato run github.com/hoppscotch/hoppscotch --oci-compose`。**blocker**: frontend-only と on-prem full stack の切り分け。**Ato 側の必要機能**: multi-target recipe と default variant。**優先度**: 高。citeturn26view2

**Stirling-PDF**。**なぜ効くか**: 79.1k stars で、誰が見ても価値がわかる PDF UI です。repo ルートに `docker` と `Dockerfile` があり、self-hosted productivity の easy win として非常に強いです。**Ato での嬉しさ**: PDF ツールは single-container でも UI がすぐ映えるため、Ato のカタログ最初期に置くのに向いています。**想定 recipe**: 単一コンテナ / 永続 storage。**推定 resources**: `services=single`, `state=file storage`, `env=ほぼ不要`, `port=web UI`。**最初の検証コマンド**: `ato run github.com/Stirling-Tools/Stirling-PDF --oci-docker-run`。**blocker**: 大きなファイルの永続化パス設計だけ。**Ato 側の必要機能**: upload-safe volume と port publish。**優先度**: 高。citeturn24view0

**Langflow**。**なぜ効くか**: 149k stars と、今回見た中でも Dify / n8n 級のモメンタムがあります。repo ルートに `docker`, `docker_example`, `.env.example` があり、AI agents / workflows の builder としての認知も強いです。**Ato での嬉しさ**: Ato の catalog に Langflow があるだけで、AI builder 文脈の見栄えが一気に上がります。**想定 recipe**: first pass は repo 内の docker assets を使う manual recipe。**推定 resources**: `services=single-to-mixed`, `env=.env.example ベース`, `state=flow / project data`, `port=実装時要確定`。**最初の検証コマンド**: `ato run github.com/langflow-ai/langflow --oci-compose docker_example`。**blocker**: 今回の調査パスでは port / minimal compose の明示確認が足りないこと。**Ato 側の必要機能**: recipe authoring UX と variant 試行。**優先度**: 高だが、実装前に docs/compose の詰めを一回入れるべきです。citeturn47view1

## Self-hosted AI の上位候補

ここでは、**self-hosted AI / local AI / agent UI** に絞って、Ato の AI ストーリーに最も乗せやすい十件を選びました。GPU 必須アプリはあえて外し、**GPU 不要または provider / Ollama / OpenAI-compatible API で回避しやすいもの**を上位に置いています。

**Open WebUI**。GPU は**不要**です。UI 自体は docker run で立ち上がり、実際の推論は Ollama や OpenAI-compatible provider に逃がせます。Ato では「local AI cockpit」を最短で見せる役です。citeturn3view0turn0view0

**Dify**。GPU は**不要**です。README 自体が多数の proprietary / open-source / self-hosted model provider を前提にしており、最小構成は Compose だけで動かせます。Ato では「複雑な AI app platform を一発で起動」に最適です。citeturn34view0turn34view1turn34view3

**Flowise**。GPU は**不要**です。Flowise 自体は UI/agent builder で、Docker Compose と docker run の導線が明快です。Ato では AI workflow canvas の高速デモができます。citeturn36view1turn39view0

**Langflow**。GPU は**不要寄り**です。repo は agents / workflows 向けで、docker assets も持っています。今回のパスでは最小起動 docs の詰めが未完ですが、モメンタムが大きく AI catalog 的価値が高いです。citeturn47view1

**LobeHub**。GPU は**不要**です。README の Docker self-hosting 手順は `OPENAI_API_KEY` を前提にしており、remote provider 駆動が基本線です。Ato では install.sh を lock する見せ方が効きます。citeturn44view0

**LibreChat**。GPU は**不要寄り**です。self-hosted AI chat としての認知が強く、Ato では Open WebUI の比較対象として有効です。recipe 化の優先度は高いですが、provider/auth 周りは Open WebUI より少し重めです。citeturn16view0

**AnythingLLM**。GPU は**不要寄り**です。RAG / local AI 文脈で強く、Ato の catalog では「chat UI だけでなく knowledge UI もある」と見せられます。recipe は provider / embedder の preset 付きにしたいです。citeturn14view3

**LiteLLM**。GPU は**不要**です。Proxy / gateway なので、むしろ Ato では Open WebUI や Langfuse と remix しやすい中核コンポーネントです。AI catalog に“provider 管理”レーンを作るなら最重要候補の一つです。citeturn47view0

**Langfuse**。GPU は**不要**です。推論 UI ではなく observability / eval / prompt playground 側ですが、AI developer audience に刺さりやすく、Ato の strategic value が高いです。citeturn46view3turn42view2

**OpenHands**。GPU は**不要**です。README に CLI / Local GUI があり、Claude / GPT など任意の LLM で動かせることが示されています。ただし CPU / memory / agent runtime 面の重さがあり、AI showcase には入れるべきですが first-wave の default recipe にするには一段慎重でよいです。citeturn45view2

AI 文脈での**人気は非常に高いが early default からは外す**候補は、**ComfyUI、AUTOMATIC1111、InvokeAI** です。いずれも UI と話題性は強い一方、初期 recipe の default としては GPU / model weight / startup cost が重いからです。**h2oGPT** も private GPT 文脈には乗れますが、現在は archive 済みです。citeturn15view3turn17view0turn17view1turn17view2

## すぐ勝てる UI 付き候補

初期カタログを埋める用途では、**Open WebUI、Memos、Stirling-PDF、Linkwarden、Actual、Homepage、Changedetection.io、Uptime Kuma、PocketBase、Bytebase** が特に効きます。共通点は、**UI がすぐ見える、README が比較的短い、状態管理が単純、ポートも想像しやすい**ということです。citeturn0view0turn20view2turn24view0turn21view2turn20view1turn22view0turn21view3turn29view3turn29view2turn28view1

**Open WebUI** は easy win なのに AI の顔にもなる、という意味で別格です。**Memos** は notes の localhost UI が軽く、**Stirling-PDF** は非技術ユーザーにも一目で伝わります。**Linkwarden** は bookmark / read-later の保存価値がわかりやすく、**Actual** は local-first の文脈で Ato の state management 価値を説明しやすいです。citeturn3view0turn20view2turn24view0turn21view2turn20view1

**Homepage** と **Changedetection.io** は、「一発で起動してすぐ使う」感が強く、Ato の catalog を厚く見せるのに向いています。**Uptime Kuma** は monitoring UI として圧倒的に理解しやすく、**PocketBase** は “one-file backend” のため、source/native recipe の良いサンプルになります。**Bytebase** は厳密には easy win と high-value の中間ですが、DB UI として見た目が強く、Ato が Docker 起動と state を整理する価値を伝えやすいです。citeturn22view0turn21view3turn29view3turn29view2turn28view1

もし「Ato の最初の catalog に十本だけ並べる」なら、私はこの順で置きます。**Open WebUI、Memos、Stirling-PDF、Linkwarden、Actual、Homepage、Changedetection.io、Uptime Kuma、PocketBase、Bytebase**。ここに **n8n** と **Dify** を足すと、“軽い勝ち筋” と “高摩擦勝ち筋” の両方が揃います。citeturn0view0turn20view2turn24view0turn21view2turn20view1turn22view0turn21view3turn29view3turn29view2turn28view1turn25view0turn7view1

## 実装順とデモナラティブ

### 最初の五本

- **Open WebUI** — AI 文脈の顔。docker run と install.sh の両方があり、Ato の価値が最短で伝わる。`ato run github.com/open-webui/open-webui` をそのまま広告文にできる。citeturn3view0turn4view0turn0view0
- **n8n** — 圧倒的な知名度と見栄え。`docker run -p 5678:5678` を recipe 化するだけで初回デモが成立する。citeturn35view0turn46view0
- **Dify** — Ato が README の複雑さを吸収する代表例。Compose と `.env` と `/install` を一回で包める。citeturn34view0turn34view1turn33view1turn33view2
- **Flowise** — AI builder の早い勝ち筋。Compose と単一コンテナの両方があり、初期 recipe の作りやすさが高い。citeturn36view1turn39view0turn46view1
- **Langfuse** — “高摩擦 stack を一発で起動できる” の象徴。worker / web / ClickHouse / MinIO / Redis / Postgres まで含めて Ato の真正面の価値になる。citeturn41view0turn42view1turn42view2turn42view3turn46view3

### 次の十本

- **LobeHub** — install.sh を lock するストーリーが強い。citeturn44view0turn46view2
- **Hoppscotch** — API tool の見栄えが良く、developer audience に刺さる。citeturn26view2
- **NocoDB** — “Airtable 代替” は説明が簡単で、compose 発見デモにも使える。citeturn25view1
- **Memos** — 初期カタログの軽量勝ち筋。citeturn20view2
- **Stirling-PDF** — 非AI層まで含めて一発で伝わるデモ。citeturn24view0
- **Linkwarden** — bookmarks/read-later の保存体験がわかりやすい。citeturn21view2
- **Actual Budget** — local-first / state-preserving の説明に向く。citeturn20view1
- **Bytebase** — DB UI は developer demo として強い。citeturn28view1
- **PocketBase** — source/native recipe の良いサンプル。citeturn29view2
- **Langflow** — docs の詰めは必要だが、star 規模が大きく外せない。citeturn47view1

### AI ショーケース

- **Open WebUI** — local AI cockpit。citeturn0view0turn3view0
- **Dify** — all-in-one AI app platform。citeturn31view0turn34view0
- **Flowise** — visual AI workflows。citeturn36view1turn46view1
- **Langflow** — high-momentum agent builder。citeturn47view1
- **LobeHub** — AI UI / operator narrative。citeturn43view0turn44view0
- **LibreChat** — self-hosted chat alternative。citeturn16view0
- **AnythingLLM** — local RAG / KB UI。citeturn14view3
- **LiteLLM** — OpenAI proxy / provider gateway。citeturn47view0
- **Langfuse** — AI devtools / observability レーン。citeturn46view3turn42view2
- **OpenHands** — advanced showcase。citeturn45view2

### いまは避ける候補

- **Coolify / Dokploy** — repo のプロダクト性質上、ホスト側コンテナ制御を伴う PaaS で、Ato 初期の“閉じた再現ランタイム”と緊張があるため、少なくとも first-wave には入れない方が安全です。これは product class に基づく推定です。citeturn30view1turn30view2
- **ComfyUI / AUTOMATIC1111 / InvokeAI** — 人気は非常に高いが、初期 default recipe としては GPU・モデル重み・起動時間のコストが大きすぎます。GPU showcase 用の別レーンに回すべきです。citeturn15view3turn17view0turn17view1
- **Supabase / PostHog / Immich / Ragflow** — いずれも人気とデモ価値は高いが、初期 recipe としてはサービス数・ボリューム数・secret 数が多すぎます。first-wave のあとに着手する方が良いです。citeturn26view1turn29view0turn22view1turn18view2
- **maybe / h2oGPT** — どちらも archive 済みで、初期 catalog には置きにくいです。citeturn20view0turn17view2

### デモナラティブ案

**README の苦行を recipe に圧縮する**。Dify や Langfuse のように、`docker compose`, `.env`, healthcheck, 初回セットアップ URL が長い repo を見せてから、Ato では `ato run github.com/...` で同じ UI が出ることを見せます。Ato の“run before setup”メッセージが最も強く伝わる物語です。citeturn34view0turn34view1turn41view0turn42view1turn42view2

**local AI cockpit を三分で作る**。Open WebUI を表、LiteLLM を provider 管理、Langfuse を観測に置き、Ato が複数の AI ローカルコンポーネントを recipe として lock / remix できることを見せます。単体 repo の recipe から “stack の remixed recipe” へ自然に繋がります。citeturn3view0turn47view0turn46view3

**OSS app store ではなく OSS runtime catalog として見せる**。n8n、Memos、Stirling-PDF、Linkwarden、Open WebUI を並べて、「有名 OSS をインストール説明なしで再現起動できる catalog」として Ato を打ち出します。AI 以外も入れることで、Ato が単なる AI launcher ではなく、**GitHub repo / local project / Docker Compose を recipe 化する runtime**だと伝えやすくなります。citeturn25view0turn20view2turn24view0turn21view2turn0view0