# **Ato初期ローンチおよび開発者モメンタム獲得に向けた人気リポジトリ・セルフホストAIアプリケーション横断調査報告書**

## **エグゼクティブサマリー**

本調査報告書は、ポータブルなアプリケーション実行ランタイム「Ato」の初期ローンチにおける開発者コミュニティのモメンタム獲得を目的として、GitHub上の高スター数リポジトリおよびセルフホストAIアプリケーションの展開容易性と戦略的価値を横断的に評価したものである。  
2026年現在のソフトウェア開発シーンにおいて、開発者はクラウドサービスのサブスクリプション費用高騰やプライバシー保護への懸念から、インフラの自己管理やセルフホスト型ローカルAIへの回帰を急速に進めている 1。しかし、多重化されたコンテナ、ベクトルデータベース、キャッシュレイヤー、そして複雑な環境変数のセットアップは依然として高い障壁であり、これが「技術的に動かせる」ことと「日常的に再利用・共有できる」ことの間のギャップを生み出している 3。  
本報告書では、このセットアップの複雑性を解消し、Atoの「ワンコマンド起動、ロック、再現、 remixture」という価値を直感的にデモできるリポジトリを厳選した。選定にあたっては、以下の評価指標（![][image1]）を策定し、50の主要リポジトリを客観的にスコアリングした。  
![][image2]  
ここで：

* ![][image3] は**モメンタム（25%）**：GitHubスター数、リリース頻度、開発者コミュニティにおける注目度を示す 2。  
* ![][image4] は**Ato適合性（25%）**：ネットワーク権限や特権不要で、シンプルなサービスグラフを持つクリーンなコンテナ設計であるかを測る 4。  
* ![][image5] は**デモ価値（20%）**：一発起動の「Before/After」が視覚的に際立ち、すぐに触れるWeb UIがあるかを評価する 8。  
* ![][image6] は**技術的実現性（15%）**：マルチアーキテクチャ（arm64/amd64）対応、初期化時間、環境変数設計のシンプルさを測る 3。  
* ![][image7] は**戦略的価値（15%）**：セルフホストAIやローカルファーストといった2026年の最重要技術トレンドを代表する象徴性を示す 10。

調査の結果、Atoランタイムの初期カタログを牽引する中核リポジトリとして、コンテナ管理やローカルモデルとの接続が極めて容易な open-webui、ビジュアルワークフローの最高峰である n8n、そしてローカルファーストの生産性向上ツール群が最有力候補として浮上した 1。これらの戦略的実装により、開発者コミュニティ内での爆発的なシェアと信頼性の確立をシームレスに実現できる。

## **主要リポジトリ横断評価マトリクス（50選）**

以下のマトリクスは、選定した50候補の性能・適合性を一覧化したものである。これらはAtoの持つ「複雑なセットアップをレシピ一本でロックする」機能を示すのに最適なリポジトリである。

| Rank | Repo | Stars | Category | Momentum | Ato fit | Runtime shape | Services | Recipe path | Difficulty | Demo value | Strategic reason | Main risk |
| :---- | :---- | :---- | :---- | :---- | :---- | :---- | :---- | :---- | :---- | :---- | :---- | :---- |
| 1 | open-webui/open-webui | 138k 12 | Self-hosted AI | 極めて高い 6 | 高 | Single / Compose 7 | App \+ Ollama | \--oci-compose 7 | A | High | ローカルAIの標準ゲートウェイ 1 | モデル引き込みの遅延 8 |
| 2 | n8n-io/n8n | 189k 11 | Self-hosted AI | 極めて高い 1 | 高 | Single Container 11 | App \+ SQLite | capsule.toml | A | High | ビジュアルワークフローのデファクト 1 | SQLite書き込みロック |
| 3 | usememos/memos | 56.1k 14 | Productivity | 高 2 | 高 | Single Container 14 | App \+ SQLite | capsule.toml | A | Medium | ローカルファースト軽量ノート 2 | 高負荷時の競合 |
| 4 | nocodb/nocodb | 60k 2 | Dev Tools | 高 2 | 高 | App \+ SQLite/PG 15 | App \+ SQLite | capsule.toml | A | High | ローカルで動くAirtable代替 2 | 初期化時のスキーマロック |
| 5 | uptime-kuma/uptime-kuma | 84.5k 16 | Productivity | 高 2 | 高 | Single Container 16 | App \+ SQLite | capsule.toml | A | High | 起動直後のダッシュボード視認性 | ディスク書き込み頻度 |
| 6 | lobehub/lobe-chat | 77.5k 17 | Self-hosted AI | 高 18 | 高 | Single Container 18 | App Only | capsule.toml | A | High | 洗練されたUI、クリーンな設計 9 | 外部Auth/S3の初期設定 19 |
| 7 | directus/directus | 24k 20 | Dev Tools | 高 | 高 | App \+ Database 20 | App \+ Postgres | \--oci-compose | A | High | 既存DBの上に瞬時に構築するCMS | アセット永続パス |
| 8 | actualbudget/actual | 26.6k 21 | Productivity | やや高 | 高 | Single Container 21 | App \+ SQLite | capsule.toml | A | Medium | ローカルファースト家計簿の決定版 | 同期時の不整合 |
| 9 | metabase/metabase | 46.4k 22 | Dev Tools | 高 | 高 | Single Container 22 | App Only (Java) | capsule.toml | A | High | 美しいSQLダッシュボードの即時起動 22 | JVMの初期メモリ消費 |
| 10 | twentyhq/twenty | 44k 23 | Productivity | 高 23 | 中 | Multi-Container 23 | App+PG+Redis+Worker | \--oci-compose | B | High | 複雑なCRM環境の一発構築 23 | 必要リソースの重さ 23 |
| 11 | changedetection-io/changedetection.io | 24.4k 16 | Productivity | やや高 | 高 | Single Container 16 | App Only (Python) | capsule.toml | A | High | 実用性の高いWeb更新監視システム | スクレイピング拒否 |
| 12 | budibase/budibase | 25k 24 | Dev Tools | やや高 | 高 | Single / Compose 24 | App Only (Svelte/Koa) | capsule.toml | A | High | 業務アプリ開発ダッシュボード 24 | 初期ビルド時の遅延 |
| 13 | blinkospace/blinko | 10.4k 25 | Productivity | 高 | 高 | Single Container 25 | App \+ SQLite | capsule.toml | A | Medium | AI搭載の次世代高速メモ 25 | データベース移行エラー |
| 14 | appsmithorg/appsmith | 30k 26 | Dev Tools | やや高 | 高 | Single Container 26 | App \+ Embedded DB | capsule.toml | A | High | 直感的な内製ツール開発画面 26 | 初期フットプリントの肥大 |
| 15 | linkwarden/linkwarden | 12k | Productivity | やや高 | 高 | App \+ Database | App \+ Postgres | \--oci-compose | A | Medium | PDF保存を伴う美しいブックマーク | Puppeteerの負荷 |
| 16 | langfuse/langfuse | 27.6k 27 | AI Dev Tools | 高 28 | 中 | Multi-Container 5 | App+Worker+PG+CH+Redis | \--oci-compose 29 | B | High | 高度なLLM可観測性プラットフォーム 5 | ClickHouseの起動ラグ 5 |
| 17 | gethomepage/homepage | 20k 30 | Productivity | 中 | 高 | Single Container 30 | App Only (Node) | capsule.toml | A | Medium | 高いカスタマイズ性と見栄えの良さ 30 | 設定ファイルのバインド |
| 18 | bytebase/bytebase | 15k | Dev Tools | 中 | 高 | Single Container | App Only (Go) | capsule.toml | A | High | 洗練されたデータベースCI/CD管理 | サンプルデータの用意 |
| 19 | logto-io/logto | 18k | Dev Tools | やや高 | 高 | App \+ Database | App \+ Postgres | \--oci-compose | B | High | 開発者向けの即席アイデンティティ基盤 | コールバックパスの整合性 |
| 20 | FlowiseAI/Flowise | 42.4k 6 | AI Dev Tools | 中 6 | 高 | Single Container 31 | App Only (Node) | capsule.toml | B | High | ビジュアルLangChainエディタ 32 | 重大な脆弱性（RCEリスク） 33 |
| 21 | BerriAI/litellm | 22k 34 | AI Dev Tools | 高 | 高 | Single Container 18 | App Only (Python) | capsule.toml | A | Medium | OpenAI互換ゲートウェイの最高峰 | プレ認証SQLインジェクション 34 |
| 22 | h2oai/h2ogpt | 15k 35 | Self-hosted AI | 中 | 中 | Single / Compose 35 | App \+ VectorDB | \--oci-compose | B | High | 高精度なオフラインドキュメントRAG 35 | Fallback時のCPU消費過多 |
| 23 | getzep/zep | 8k | AI Dev Tools | やや高 36 | 高 | App \+ Database 36 | App \+ Postgres | \--oci-compose | B | Medium | エージェント向け長期記憶エンジン 36 | クライアント実装の依存 |
| 24 | excalidraw/excalidraw | 50k | Productivity | 高 | 高 | Single Container | App Only | capsule.toml | A | High | 美しい手書き風描画キャンバス | デフォルトの永続化なし |
| 25 | appflowy-io/AppFlowy | 67.9k 37 | Productivity | 高 37 | 中 | Single Container 38 | App Only (Dart/Rust) | capsule.toml | B | High | 個人データ主権を維持するNotion代替 38 | クライアントバイナリ依存 |
| 26 | toeverything/AFFiNE | 57.8k 6 | Productivity | 高 6 | 中 | Single Container 38 | App Only (Node) | capsule.toml | B | High | ビジュアルなホワイトボード・エディタ 38 | ロード時のレンダリング負荷 |
| 27 | paperless-ngx/paperless-ngx | 26k 16 | Productivity | やや高 | 中 | Multi-Container 16 | App+Redis+Postgres | \--oci-compose | B | High | スキャン文書のAIタグ付け管理 16 | arm64依存コンパイル |
| 28 | photoprism/photoprism | 35k 39 | Productivity | やや高 | 中 | App \+ Database 39 | App \+ MariaDB | \--oci-compose | B | High | プライベートAI写真アルバム 39 | 初回インデックス時の負荷 |
| 29 | grafana/grafana | 72.7k 22 | Dev Tools | 高 22 | 高 | Single Container 22 | App Only | capsule.toml | A | High | システム監視の必須標準パネル 22 | 外部データソースの設定 |
| 30 | penpot/penpot | 30k | Productivity | 高 | 中 | Multi-Container | App+Postgres+Redis | \--oci-compose | B | High | オープンソースのFigma代替デザインツール | メモリフットプリント |
| 31 | mindsdb/mindsdb | 22k | AI Dev Tools | 中 | 中 | Single Container | App Only (Python) | capsule.toml | B | Medium | SQLデータベース内AIモデル実行 | イメージサイズの巨大化 |
| 32 | apache/superset | 70k 40 | Dev Tools | 高 40 | 中 | Multi-Container 40 | App+Worker+Redis+PG | \--oci-compose | C | High | Airbnb発の強力なBIツール 40 | 複数コンテナの初期起動ラグ |
| 33 | redash/redash | 28k 40 | Dev Tools | 中 40 | 中 | Multi-Container 40 | App+Worker+Redis+PG | \--oci-compose | C | High | クエリ指向のシンプルなBI 40 | メンテナンス更新の低迷 |
| 34 | standardnotes/app | 44k | Productivity | 中 | 中 | App \+ Database | App \+ MySQL | \--oci-compose | C | High | 暗号化に特化したメモツール | 初期暗号キー生成の複雑さ |
| 35 | invoke-ai/InvokeAI | 27.2k 41 | Self-hosted AI | やや高 | 低〜中 | Single Container 42 | App Only (Python) | capsule.toml | C | High | クリエイター向け画像生成環境 41 | CUDA等GPUリソース依存 42 |
| 36 | maybe-finance/maybe | 54.1k 43 | Productivity | 低〜中 43 | 中 | App \+ Database 43 | App \+ Postgres | \--oci-compose | B | High | 洗練されたRails家計簿ダッシュボード | リポ非推奨化（メンテナンス停止） 43 |
| 37 | AUTOMATIC1111/stable-diffusion-webui | 148k 44 | Self-hosted AI | 極めて高い 44 | 低〜中 | Single Container 45 | App Only (Python) | capsule.toml | C | High | 画像生成コミュニティの中心的存在 46 | ハードウェア・ドライバ競合 45 |
| 38 | ComfyUI/ComfyUI | 40k | Self-hosted AI | 極めて高い | 低〜中 | Single Container | App Only (Python) | capsule.toml | C | High | ノード記述による極限画像生成 | ローカル環境へのライブラリ追加 |
| 39 | run-llama/llama\_deploy | 3k | AI Dev Tools | 中 | 高 | Single Container | App Only (Python) | capsule.toml | B | Low | 分散型エージェントオーケストレータ | CLIのみで視覚的 UI 欠如 |
| 40 | immich-app/immich | 95.6k 16 | Productivity | 極めて高い 47 | 低 | 複雑なサービス設計 | App+Worker+DB+ML 3 | \--oci-compose | C | High | 最高峰の写真管理。凄まじい更新頻度 47 | RAM 6GB以上、高負荷 3 |
| 41 | posthog/posthog | 25k 22 | Dev Tools | 高 22 | 低 | 巨大マルチサービス | App+Kafka+CH+PG+Redis | \--oci-compose | D | High | 包括的なプロダクト分析ツール | ローカルマシンのRAM即時枯渇 |
| 42 | supabase/supabase | 100k 48 | Dev Tools | 極めて高い 49 | 低 | 巨大マルチサービス | 10以上のコンテナ 50 | \--oci-compose | D | High | Firebaseキラーのオープンソース覇者 49 | 複雑なローカルポート競合 |
| 43 | coollabsio/coolify | 40k 16 | Dev Tools | 高 16 | 低 | ホスト依存の起動 | Single Container | N/A | D | Low | Self-hosted PaaSの決定版 | Docker Socketのマウント必須 |
| 44 | dokploy/dokploy | 34.1k 51 | Dev Tools | 高 51 | 低 | ホスト依存の起動 | Single Container 51 | N/A | D | Low | Traefikベースのプロビジョニングツール | Docker Socketのマウント必須 |
| 45 | All-Hands-AI/OpenHands | 60.4k 6 | AI Dev Tools | 高 6 | 低 | 動的コンテナ生成 | App Only | N/A | D | High | 自律型AIソフトウェアエンジニア | 動的コンテナ実行のソケット要求 52 |
| 46 | outline/outline | 38.5k 38 | Productivity | 高 | 低 | 認証・ストア必須 | App \+ Postgres \+ Redis | \--oci-compose | C | High | 洗練されたWiki・ナレッジベース | OIDC、外部ストレージ依存 |
| 47 | usebruno/bruno | 44.3k 53 | Dev Tools | 高 53 | 高 | Single Container 53 | App Only (JS/TS) | capsule.toml | A | High | GitフレンドリーAPIクライアント 53 | ローカルファイルアクセス |
| 48 | continuedev/continue | 31.9k 55 | AI Dev Tools | 高 56 | 高 | Single Container | App Only | capsule.toml | B | Low | IDE（VSCode/JetBrains）拡張機能 | CLI中心（単独UIが薄い） |
| 49 | searxng/searxng | 25k | Productivity | やや高 | 高 | Single Container | App Only (Python) | capsule.toml | A | Medium | プライバシーを重視した検索メタエンジン | 頻繁なスクレイピングによる遮断 |
| 50 | plausible/analytics | 22k | Dev Tools | やや高 | 低〜中 | Multi-Container | App+Postgres+Clickhouse | \--oci-compose | C | High | 高速・軽量・シンプルなアクセス解析 | ClickHouseのローカル初期負荷 |

## **Top 10: Momentum-first recipes**

初期の開発者コミュニティにおけるバズ創出に直結する、知名度・話題性・デモ価値が極めて高い上位10件のシステム設計。

### **1\. open-webui/open-webui**

現在、ローカルAI環境構築において絶対的な覇権を握っており、ChatGPTに匹敵するリッチな体験を自己管理下で実現できるツールとして注目されている 1。通常はOllamaの設定やコンテナ間通信のトラブルに多くの時間を要するが 7、Atoによる一発起動は開発者の初期摩擦を最小化し、圧倒的な体験価値を提供する。

| 項目 | 構成定義・詳細仕様 |
| :---- | :---- |
| **想定レシピ構造** | capsule.toml にてホストゲートウェイ接続を静的解決する方式 |
| **起動サービスグラフ** | open-webui コンテナ（単一構成。ホスト上のOllamaと連動する構成を基本とする） 7 |
| **永続ステート / env** | /app/backend/data 7, 環境変数 OLLAMA\_BASE\_URL=http://host.docker.internal:11434 7 |
| **公開ポート** | ホスト側の 3000 ポート 7 |
| **最初の検証コマンド** | ato run github.com/open-webui/open-webui |
| **想定 blocker** | ホスト上のポート競合、Docker Desktopの host.docker.internal 名前解決失敗 8 |
| **Ato側必要機能** | ホストループバックインターフェース（host-gateway）の自動マッピング 7 |
| **レシピ作成優先度** | **P0（ローンチ時最優先実装）** |

### **2\. n8n-io/n8n**

開発者が自前のワークフローを自動化し、AI機能（LangChainノード等）を組み込むための業界標準ノーコードツールである 1。通常はNode実行環境やローカルボリュームのパーミッション設定、SSL構成が必要になるが 11、Atoを利用することでこれらが即時に抽象化される。

| 項目 | 構成定義・詳細仕様 |
| :---- | :---- |
| **想定レシピ構造** | capsule.toml によるシングルコンテナ・パッケージング |
| **起動サービスグラフ** | n8n コンテナ（内蔵SQLite構成） 1 |
| **永続ステート / env** | /home/node/.n8n 11, 環境変数 N8N\_ENFORCE\_SETTINGS\_FILE\_PERMISSIONS=false |
| **公開ポート** | ホスト側の 5678 ポート 11 |
| **最初の検証コマンド** | ato run github.com/n8n-io/n8n |
| **想定 blocker** | SQLiteデータベースロック時の書き込みラグ、ボリュームパーミッションエラー |
| **Ato側必要機能** | ユーザーID/グループID（UID/GID）の自動マウント調整機能 |
| **レシピ作成優先度** | **P0（初期バズの強力なトリガー）** |

### **3\. usememos/memos**

NotionやGoogle Keepの代替となる、軽量かつ高速な個人用マイクロメモサービスとしてGitHub上で非常に高い成長速度を誇る 2。シングルバイナリ・SQLite依存のシンプルな構成はAtoのポータブル動作モデルと完璧に一致する 14。

| 項目 | 構成定義・詳細仕様 |
| :---- | :---- |
| **想定レシピ構造** | capsule.toml によるゼロスタック構築 |
| **起動サービスグラフ** | memos コンテナ（SQLite内蔵） 14 |
| **永続ステート / env** | /var/opt/memos 14 |
| **公開ポート** | ホスト側の 5230 ポート |
| **最初の検証コマンド** | ato run github.com/usememos/memos |
| **想定 blocker** | 永続ディレクトリが存在しない場合のマウントエラー |
| **Ato側必要機能** | ホスト側のローカル永続ディレクトリ自動生成機能 |
| **レシピ作成優先度** | **P0（即時実用ツール）** |

### **4\. nocodb/nocodb**

自己ホスト可能なAirtable代替として、開発者のみならず一般ビジネス部門からも強く支持されているデータベースUIツールである 2。多層的な接続設定やコンテナでの起動手順をAtoが全て背後で包み込み、ブラウザ上でのシームレスな起動を可能にする 15。

| 項目 | 構成定義・詳細仕様 |
| :---- | :---- |
| **想定レシピ構造** | capsule.toml（SQLite版）および \--oci-compose（Postgres連携版） |
| **起動サービスグラフ** | nocodb コンテナ（デフォルトはSQLite版でデモ性を担保） 15 |
| **永続ステート / env** | /usr/app/data/ 15, JWT秘密鍵の自動生成変数 15 |
| **公開ポート** | ホスト側の 8080 ポート 15 |
| **最初の検証コマンド** | ato run github.com/nocodb/nocodb |
| **想定 blocker** | JWT秘密鍵（NC\_AUTH\_JWT\_SECRET）が動的に渡されない場合のクラッシュ 15 |
| **Ato側必要機能** | ランダムシード環境変数（openssl ライク）の動的インジェクション |
| **レシピ作成優先度** | **P0（強力なビジネス系デモ価値）** |

### **5\. uptime-kuma/uptime-kuma**

サービス死活監視ツールとして、現在最も普及しているビジュアルダッシュボードである 16。UIが美しく、起動直後に監視対象を入力してすぐに動かせる点が、デモにおいて優れた視覚効果を生み出す。

| 項目 | 構成定義・詳細仕様 |
| :---- | :---- |
| **想定レシピ構造** | capsule.toml によるパッケージング |
| **起動サービスグラフ** | uptime-kuma コンテナ |
| **永続ステート / env** | /app/data |
| **公開ポート** | ホスト側の 3001 ポート |
| **最初の検証コマンド** | ato run github.com/louislam/uptime-kuma |
| **想定 blocker** | ICMP監視実行のための特権制限（Pingを実行するためのコンテナ能力許可） |
| **Ato側必要機能** | コンテナへの最小限の capabilities（NET\_RAW 等）付与オプション |
| **レシピ作成優先度** | **P1（見栄え重視の定番枠）** |

### **6\. lobehub/lobe-chat**

20以上のLLMプロバイダに対応した、現代的かつ多機能なAIチャットUIである 10。APIキーの受け渡しをAto側の環境変数プロンプトで制御することで、ユーザーごとに即座にパーソナライズされたAI空間を展開できる 9。

| 項目 | 構成定義・詳細仕様 |
| :---- | :---- |
| **想定レシピ構造** | capsule.toml によるクライアントデータベース（LocalStorage）モード起動 |
| **起動サービスグラフ** | lobe-chat コンテナ（初期は複雑なPostgres/S3を回避） 9 |
| **永続ステート / env** | 永続化はブラウザ側の同期に依存 10, 環境変数 ACCESS\_CODE でロック 9 |
| **公開ポート** | ホスト側の 3210 ポート 10 |
| **最初の検証コマンド** | ato run github.com/lobehub/lobe-chat |
| **想定 blocker** | クライアント側から外部プロバイダAPIへの直接通信時のCORSブロック |
| **Ato側必要機能** | ユーザーに対しコンソール上でAPIキーの入力を求めるインタラクティブ変数プロンプト |
| **レシピ作成優先度** | **P0（デザイン性の高いAI UI）** |

### **7\. directus/directus**

Node.js製ヘッドレスCMSとして高い信頼性を持ち、任意のデータベースを一瞬で美しい管理画面付きAPIへ変換する 20。通常はPostgresの準備や初期スキーママイグレーションが必要だが 20、Atoによってこれらが全自動で隠蔽される。

| 項目 | 構成定義・詳細仕様 |
| :---- | :---- |
| **想定レシピ構造** | \--oci-compose 経由によるPostgres連携型サービススタック |
| **起動サービスグラフ** | directus コンテナ ＋ postgres コンテナ |
| **永続ステート / env** | postgres\_data ボリューム, 管理者用パスワード環境変数など |
| **公開ポート** | ホスト側の 8055 ポート |
| **最初の検証コマンド** | ato run github.com/directus/directus \--oci-compose |
| **想定 blocker** | データベース初期化の待機時間による、CMS本体の接続タイミングずれ |
| **Ato側必要機能** | 依存先コンテナのポート疎通（Healthcheck）を確認後にアプリを立ち上げるヘルスチェック連動機能 |
| **レシピ作成優先度** | **P1（開発者向けDBラッパー）** |

### **8\. actualbudget/actual**

2万6千スターを超える、Node.jsで書かれた100%ローカルファーストの家計管理システム 21。美しいグラフ描画と堅牢な暗号化が特徴であり、Atoの「ローカルにデータを保持しつつ、ランタイムのみを提供する」というコンセプトを完全に体現できる。

| 項目 | 構成定義・詳細仕様 |
| :---- | :---- |
| **想定レシピ構造** | capsule.toml によるシングルコンテナマッピング |
| **起動サービスグラフ** | actual-server コンテナ 21 |
| **永続ステート / env** | /data |
| **公開ポート** | ホスト側の 5006 ポート |
| **最初の検証コマンド** | ato run github.com/actualbudget/actual |
| **想定 blocker** | SSL環境を要求する暗号化モジュールのブラウザセキュリティ仕様 |
| **Ato側必要機能** | ローカル起動時のHTTPS自動プロキシまたは自己署名証明書の透過適用 |
| **レシピ作成優先度** | **P1（ローカルファーストの象徴）** |

### **9\. metabase/metabase**

Clojure製の強力なデータビジュアライゼーションおよびSQLビジネスインテリジェンスツール 22。各種データソースに一瞬で接続し、ノーコードでダッシュボードを生成する「Metabot」機能は、起動した瞬間の感動が最も大きいツールの一つである 22。

| 項目 | 構成定義・詳細仕様 |
| :---- | :---- |
| **想定レシピ構造** | capsule.toml による実行環境カプセル化 |
| **起動サービスグラフ** | metabase コンテナ（組込みH2データベースで初期化） 22 |
| **永続ステート / env** | /metabase-data |
| **公開ポート** | ホスト側の 3000 ポート 22 |
| **最初の検証コマンド** | ato run github.com/metabase/metabase |
| **想定 blocker** | JVMによる初期起動時のメモリ超過クラッシュ |
| **Ato側必要機能** | Javaアプリ向けのメモリ限界設定オプション（-Xmx 制限等） |
| **レシピ作成優先度** | **P1（圧倒的なデモ品質）** |

### **10\. twentyhq/twenty**

SalesforceやHubSpotを自己管理環境へ完全に置き換える、AGPL-3.0の次世代オープンソースCRM 23。起動に必要なサービス（アプリ本体、PostgreSQL、Redis、キューワーカー、アタッチメントストレージ）が多様であり 23、Atoの「複雑なComposeをワンパッケージで隠す」機能の最高峰のデモとなる。

| 項目 | 構成定義・詳細仕様 |
| :---- | :---- |
| **想定レシピ構造** | \--oci-compose 経由によるマルチサービス一括制御 |
| **起動サービスグラフ** | twenty-server ＋ postgres ＋ redis ＋ worker 23 |
| **永続ステート / env** | 各ボリューム定義、各APIシークレット設定 |
| **公開ポート** | ホスト側の 3000 ポート |
| **最初の検証コマンド** | ato run github.com/twentyhq/twenty \--oci-compose |
| **想定 blocker** | VPSやローカルPCにおける2 vCPU/4 GB以上のリソース競合・枯渇 23 |
| **Ato側必要機能** | 必要メモリ容量のチェックおよびユーザーへのリソース警告警告機構 |
| **レシピ作成優先度** | **P1（最上級のオーケストレーション事例）** |

## **Top 10: Self-hosted AI recipes**

セルフホストAI、ローカルLLM周辺ツール、またはエージェントインターフェースに特化した、2026年最大の関心領域となる10選。

### **1\. open-webui/open-webui**

OllamaやOpenAI互換APIに接続可能な、最も多機能で勢いのあるAIインターフェース 12。RAGやMCP統合などの最新機能を搭載している 12。

* **GPU 依存性と Fallback**: 本アプリ自体は純粋なWebフロントエンドであるため、CPU環境下で軽量に起動する 9。ホストマシン上で動作するOllamaのGPUアクセラレーションを自動的に叩きに行くか、またはOpenAIなどの外部APIを代替として叩くように容易にフォールバック可能である 8。

| 項目 | 構成定義・詳細仕様 |
| :---- | :---- |
| **想定レシピ構造** | capsule.toml にてホスト側のOllamaにシームレスにブリッジ |
| **起動サービスグラフ** | open-webui コンテナ 7 |
| **永続ステート / env** | /app/backend/data 7, OLLAMA\_BASE\_URL 7 |
| **公開ポート** | ホスト側の 3000 ポート 7 |
| **最初の検証コマンド** | ato run github.com/open-webui/open-webui |
| **想定 blocker** | ホスト上のOllamaサービスのアドレス競合、DNS解決遅延 8 |
| **Ato側必要機能** | host.docker.internal への自動マッピング解決機能 7 |
| **レシピ作成優先度** | **P0（最優先実装）** |

### **2\. langgenius/dify**

プロダクション対応のAIアプリケーション、RAG、およびエージェントワークフローを構築するためのオープンソースプラットフォーム 61。ビジュアルキャンバスで直感的にアプリが開発できる点が、開発者に極めて強力なモメンタムを提供する 60。

* **GPU 依存性と Fallback**: Difyシステム自体はWebアプリであり、GPUは必須ではない 4。エージェントやRAGの推論処理は、ローカルで別途動作しているOllama（GPU側）または外部のプロバイダAPIを叩くように設定することで完全にCPUのみで動作させることができる 61。

| 項目 | 構成定義・詳細仕様 |
| :---- | :---- |
| **想定レシピ構造** | docker-compose.yaml のマルチコンテナ定義の最適化 4 |
| **起動サービスグラフ** | api ＋ worker ＋ web ＋ db ＋ redis ＋ weaviate ＋ sandbox 4 |
| **永続ステート / env** | volumes/db 4, 各種環境変数（SECRET\_KEY 等の自動生成） 62 |
| **公開ポート** | nginxコンテナがフロントエンドとなるホスト 80 ポート 4 |
| **最初の検証コマンド** | ato run github.com/langgenius/dify \--oci-compose |
| **想定 blocker** | 最低4GB（推奨8GB以上）のRAM要件を満たさない場合のコンテナ起動障害 4 |
| **Ato側必要機能** | 膨大な環境変数ファイルのテンプレート生成と、依存するコンテナ（10以上）の自動起動順序制御 |
| **レシピ作成優先度** | **P1（究極のAI showcase）** |

### **3\. lobehub/lobe-chat**

美しいモダンデザインを誇る、e/accスピリットを持つ開発者が生み出した次世代AIチャット 17。プラグインシステムや各種外部音声/画像モデルを1つのダッシュボードから制御できる 10。

* **GPU 依存性と Fallback**: LobeChat本体は完全なNext.js製CPUアプリである 9。ホストデバイス上のGPUを要求せず、Ollamaや各種クラウドモデル（Anthropic, Gemini, DeepSeek等）をAPI経由でコールする設計を前提としている 9。

| 項目 | 構成定義・詳細仕様 |
| :---- | :---- |
| **想定レシピ構造** | capsule.toml によるパッケージング |
| **起動サービスグラフ** | lobe-chat コンテナ 65 |
| **永続ステート / env** | LocalStorage 9, 環境変数 OPENAI\_API\_KEY, ACCESS\_CODE 9 |
| **公開ポート** | ホスト側の 3210 ポート 9 |
| **最初の検証コマンド** | ato run github.com/lobehub/lobe-chat |
| **想定 blocker** | 公開サーバーへのデプロイ時に ACCESS\_CODE を省略した場合のセキュリティリスク 9 |
| **Ato側必要機能** | 自動的にアクセス制限（ACCESS\_CODE）を設定してユーザーにパスワードを出力する保護機能 |
| **レシピ作成優先度** | **P0（デザイン価値トップレベル）** |

### **4\. langfuse/langfuse**

LLMアプリケーションのエンジニアリング・トレース・可観測性（Observability）のオープンソース覇者 28。開発者がローカル環境や本番環境のLLM入出力をすべてトラッキングし、コストや精度を検証するための必須ツールとなっている 5。

* **GPU 依存性と Fallback**: GPUは一切使用しない 5。OLAPデータベース（ClickHouse）やPostgreSQLを用いて超高速な分析クエリを実行するデータ分析基盤として動作する 5。

| 項目 | 構成定義・詳細仕様 |
| :---- | :---- |
| **想定レシピ構造** | \--oci-compose で ClickHouse と S3 互換コンテナを協調起動するレシピ |
| **起動サービスグラフ** | langfuse-web ＋ langfuse-worker ＋ clickhouse ＋ postgres ＋ redis ＋ minio 5 |
| **永続ステート / env** | /var/lib/clickhouse 67, MinIOの langfuse バケット 67, Postgres 67 |
| **公開ポート** | Web本体用のホスト側の 3000 ポート 29 |
| **最初の検証コマンド** | ato run github.com/langfuse/langfuse \--oci-compose |
| **想定 blocker** | 2026年3月リリースのV4以降、ClickHouseの初期化スキーマ読み込み待機ラグ 66 |
| **Ato側必要機能** | 複数のミドルウェア（Postgres/MinIO/Redis）が一斉に起動した後の安定接続ヘルスチェック 29 |
| **レシピ作成優先度** | **P1（エンジニアにとって不可欠なツール）** |

### **5\. FlowiseAI/Flowise**

LangChainやLlamaIndexの各種AIコンポーネントをドラッグ＆ドロップで繋ぎ合わせ、チャットUI付きのAPIを一瞬で生成できる極めて実用的なビジュアルツール 32。

* **GPU 依存性と Fallback**: 完全なCPU駆動アプリであるためGPUは必要ない 33。すべてのベクトルデータベースの格納、および推論要求は外部サービス（またはローカルホスト上のOllama）にネットワーク経由で委ねられる 33。

| 項目 | 構成定義・詳細仕様 |
| :---- | :---- |
| **想定レシピ構造** | capsule.toml でのパッケージング |
| **起動サービスグラフ** | flowise コンテナ 31 |
| **永続ステート / env** | /root/.flowise への永続化 |
| **公開ポート** | ホスト側の 3000 ポート 31 |
| **最初の検証コマンド** | ato run github.com/FlowiseAI/Flowise |
| **想定 blocker** | セキュリティ的に脆弱な古いバージョンにおけるリモートコード実行（RCE）の危険性 32 |
| **Ato側必要機能** | バージョン依存関係を強制アップデートし、RCEパッチ適用済みバージョン（v3.0.6以降）のみを実行させるセキュリティ保護 |
| **レシピ作成優先度** | **P1** |

### **6\. BerriAI/litellm**

2万2千スターを誇る、すべてのLLMプロバイダAPI（OpenAI, Anthropic, Gemini, Ollama）をOpenAI標準APIフォーマットへ透過的に統合・ロードバランスする不可欠なプロキシ 34。

* **GPU 依存性と Fallback**: 純粋なネットワークプロキシとしてCPUのみで最高パフォーマンスを発揮する 34。

| 項目 | 構成定義・詳細仕様 |
| :---- | :---- |
| **想定レシピ構造** | capsule.toml による軽量コンテナ管理 |
| **起動サービスグラフ** | litellm コンテナ |
| **永続ステート / env** | 基本ステートレス（ログ保存にはPostgresをオプション連携） 34 |
| **公開ポート** | ホスト側の 4000 ポート |
| **最初の検証コマンド** | ato run github.com/BerriAI/litellm |
| **想定 blocker** | 2026年3月に発覚した Authorizationヘッダー経由のプレ認証SQLインジェクション脆弱性（CVE-2026-42208） 34 |
| **Ato側必要機能** | 安全な最新イメージ（修正バージョン）のパッチ検証とデプロイ |
| **レシピ作成優先度** | **P1（実用インフラ）** |

### **7\. h2oai/h2ogpt**

個人情報や社内機密を含んだドキュメント（PDF, DOCX, CSV, 音声, 画像）を、完全にプライベートかつオフラインで安全にAIにインデックスさせて対話するための最高峰のツール 35。

* **GPU 依存性と Fallback**: 高精度なセマンティックチャンキングや高速推論を行うにはGPU（RTX 3090クラス）が強く推奨されるが、本コンテナはCPU環境下でもHF（HuggingFace）および llama.cpp を用いた完全なCPUフォールバック動作をネイティブサポートしている 35。

| 項目 | 構成定義・詳細仕様 |
| :---- | :---- |
| **想定レシピ構造** | capsule.toml によるRAGスタックの一気通貫起動 |
| **起動サービスグラフ** | h2ogpt コンテナ（ベクトルDB Weaviate/Chroma/FAISSなどを内蔵または動的に用意） 35 |
| **永続ステート / env** | ドキュメント保存用ボリューム, 各種モデルダウンロードパス |
| **公開ポート** | ホスト側の 7860 ポート（Gradio UI） 35 |
| **最初の検証コマンド** | ato run github.com/h2oai/h2ogpt |
| **想定 blocker** | モデルデータの初回ローカルロードにかかる膨大なギガバイト単位のダウンロード遅延 35 |
| **Ato側必要機能** | モデルデータをキャッシュするための共有大容量アセットストレージ領域の管理 |
| **レシピ作成優先度** | **P2（オフライン最高峰デモ）** |

### **8\. getzep/zep**

エージェント向けのエンドツーエンド・コンテキストエンジニアリングプラットフォーム 36。会話履歴やビジネスデータを自動的にグラフRAGおよび知識グラフへ構造化する 36。

* **GPU 依存性と Fallback**: 完全なCPU駆動プロキシ。データ操作・永続化はPostgreSQL/pgvectorで行う 36。

| 項目 | 構成定義・詳細仕様 |
| :---- | :---- |
| **想定レシピ構造** | \--oci-compose を用いて、pgvector データベースと協調起動するレシピ |
| **起動サービスグラフ** | zep-server ＋ postgres（pgvectorプラグイン有効化） 36 |
| **永続ステート / env** | /var/lib/postgresql/data 36 |
| **公開ポート** | ホスト側の 8000 ポート |
| **最初の検証コマンド** | ato run github.com/getzep/zep \--oci-compose |
| **想定 blocker** | PostgreSQL側の pgvector 拡張機能の初期ビルドエラー 36 |
| **Ato側必要機能** | 確実に pgvector/pgvector などの最適化イメージを組み合わせて配備するコンポーネント選択 |
| **レシピ作成優先度** | **P2** |

### **9\. invoke-ai/InvokeAI**

プロフェッショナルなクリエイターがStable Diffusion（SD1.5, SDXL, FLUX）モデルを用いて、イラスト生成や手書き連動インペインティングを行うための最高峰の統一キャンバスWeb UI 41。

* **GPU 依存性と Fallback**: 高度な画像生成プロセスはGPU（VRAM 8GB〜24GB以上）を強く要求する 42。CPUモード（provides-extra: cpu）でも動作するが、1枚あたりの生成に著しい時間を要するため、基本的にはホスト側のNvidia/MシリーズMacのGPUバインドが前提となる 42。

| 項目 | 構成定義・詳細仕様 |
| :---- | :---- |
| **想定レシピ構造** | capsule.toml でのパッケージング |
| **起動サービスグラフ** | invoke-ai Webサーバー ＋ モデルマネージャ 42 |
| **永続ステート / env** | models ディレクトリ, outputs ディレクトリ 42 |
| **公開ポート** | ホスト側の 9090 ポート 42 |
| **最初の検証コマンド** | ato run github.com/invoke-ai/InvokeAI |
| **想定 blocker** | コンテナ側からホストのGPUドライバに接続できない場合の初期エラー |
| **Ato側必要機能** | ホストのNvidiaドライバをコンテナ内に安全に通すための gpu デバイスパススルー制御 |
| **レシピ作成優先度** | **P2（画像系・高度クリエイティブ最高峰）** |

### **10\. mindsdb/mindsdb**

データベースのデータ（PostgresやMySQL等）を直接機械学習モデルやLLM（OpenAI, Ollama）とSQLクエリベースで自動接続し、予測データを生成できる高度なデータAI統合ハブ。

* **GPU 依存性と Fallback**: MindsDBエンジン自体はCPU上で快適に駆動する。機械学習タスクにおけるモデル予測の推論は、API経由で外部のLLMプロバイダなどにアウトソース可能。

| 項目 | 構成定義・詳細仕様 |
| :---- | :---- |
| **想定レシピ構造** | capsule.toml によるパッケージング |
| **起動サービスグラフ** | mindsdb コンテナ |
| **永続ステート / env** | データベース接続キー、各環境変数 |
| **公開ポート** | ホスト側の 47334 ポート |
| **最初の検証コマンド** | ato run github.com/mindsdb/mindsdb |
| **想定 blocker** | 複数の依存データコネクタのローカルコンパイルエラー |
| **Ato側必要機能** | プレコンパイル済みのマルチコネクタイメージの使用制御 |
| **レシピ作成優先度** | **P2** |

## **Top 10: Easy wins with visible UI**

Atoの初期アプリケーションストアを即座に埋めるための、起動難易度が非常に低く、シングルコンテナで動作し、視覚的な使い心地が抜群な10選。

### **1\. usememos/memos**

極めて高速に起動し、SQLiteのみで完全にローカルに完結するマイクロブログ（メモ）ツール 2。

| 項目 | 構成定義・詳細仕様 |
| :---- | :---- |
| **想定レシピ構造** | capsule.toml 単一構成 |
| **永続ステート / env** | /var/opt/memos 14 |
| **公開ポート** | ホスト側の 5230 ポート |
| **検証コマンド** | ato run github.com/usememos/memos |
| **レシピ作成優先度** | **P0（最も実装が簡単な実績枠）** |

### **2\. blinkospace/blinko**

TauriとNext.jsで構築された、自己所有のAIアシスタント機能（Ollama対応）を内蔵する、極めて美しくモダンな高速メモツール 25。

| 項目 | 構成定義・詳細仕様 |
| :---- | :---- |
| **想定レシピ構造** | capsule.toml にて構築 |
| **永続ステート / env** | /app/data 25 |
| **公開ポート** | ホスト側の 3210 ポート |
| **検証コマンド** | ato run github.com/blinkospace/blinko |
| **レシピ作成優先度** | **P1（デザイン重視のAI拡張メモ）** |

### **3\. changedetection-io/changedetection.io**

任意のWebサイトの変更情報をリアルタイムに検知し、APIやチャットにプッシュするエンジニア必須の監視ツール 16。起動後に即座に対象URLを入れるだけで完全に機能する。

| 項目 | 構成定義・詳細仕様 |
| :---- | :---- |
| **想定レシピ構造** | capsule.toml |
| **永続ステート / env** | /datastore |
| **公開ポート** | ホスト側の 5000 ポート |
| **検証コマンド** | ato run github.com/changedetection-io/changedetection.io |
| **レシピ作成優先度** | **P1（抜群の実用性と機能実感）** |

### **4\. excalidraw/excalidraw**

ローカルで完全にクリーンに動作する、極めて人気のあるホワイトボード描画キャンバス。

| 項目 | 構成定義・詳細仕様 |
| :---- | :---- |
| **想定レシピ構造** | capsule.toml |
| **永続ステート / env** | なし（すべてローカルブラウザ内に保持） |
| **公開ポート** | ホスト側の 80 ポート |
| **検証コマンド** | ato run github.com/excalidraw/excalidraw |
| **レシピ作成優先度** | **P1（最も軽量なデザインデモ）** |

### **5\. linkwarden/linkwarden**

美しくモダンなブックマークマネージャ。スクリーンショットおよびPDFアーカイブを完全に自己ホスト配下で自動生成する。

| 項目 | 構成定義・詳細仕様 |
| :---- | :---- |
| **想定レシピ構造** | \--oci-compose （SQLite対応のスタンドアロン版イメージがある場合はそちらを優先） |
| **永続ステート / env** | linkwarden\_data 永続ディレクトリ、Postgres |
| **公開ポート** | ホスト側の 3000 ポート |
| **検証コマンド** | ato run github.com/linkwarden/linkwarden \--oci-compose |
| **レシピ作成優先度** | **P1** |

### **6\. bytebase/bytebase**

データベースのDDL変更、スキーマバージョン管理、データアクセスガバナンスのための洗練された開発者向けWeb UI。

| 項目 | 構成定義・詳細仕様 |
| :---- | :---- |
| **想定レシピ構造** | capsule.toml |
| **永続ステート / env** | /var/opt/bytebase |
| **公開ポート** | ホスト側の 5678 ポート |
| **検証コマンド** | ato run github.com/bytebase/bytebase |
| **レシピ作成優先度** | **P2** |

### **7\. logto-io/logto**

開発者のための、わずか数分で多言語サインイン画面とOAuth認証をアプリに実装できる次世代の認証フレームワーク。

| 項目 | 構成定義・詳細仕様 |
| :---- | :---- |
| **想定レシピ構造** | \--oci-compose |
| **永続ステート / env** | Postgres、管理者セットアップの環境変数 |
| **公開ポート** | ホスト側の 3002 ポート |
| **検証コマンド** | ato run github.com/logto-io/logto \--oci-compose |
| **レシピ作成優先度** | **P1** |

### **8\. appsmithorg/appsmith**

ドラッグ＆ドロップの各種UIコンポーネント（テーブル、チャート、フォーム）と、自社のあらゆるDBやAPIを直接バインドできる、非常に人気のある内部ツールビルダー 26。

| 項目 | 構成定義・詳細仕様 |
| :---- | :---- |
| **想定レシピ構造** | capsule.toml（CE版の単一コンテナパッケージを使用） 26 |
| **永続ステート / env** | /appsmith-data 26 |
| **公開ポート** | ホスト側の 80 ポート |
| **検証コマンド** | ato run github.com/appsmithorg/appsmith |
| **レシピ作成優先度** | **P2** |

### **9\. appflowy-io/AppFlowy**

DartとRustによって驚異的なパフォーマンスとオフラインファースト、個人所有データを掲げる、最も人気のあるオープンソースのNotionクローン 37。

| 項目 | 構成定義・詳細仕様 |
| :---- | :---- |
| **想定レシピ構造** | capsule.toml または \--oci-compose で提供されるクラウドバックエンド |
| **永続ステート / env** | /app/data |
| **公開ポート** | ホスト側の 80 ポート |
| **検証コマンド** | ato run github.com/appflowy-io/AppFlowy |
| **レシピ作成優先度** | **P2** |

### **10\. gethomepage/homepage**

自宅や開発環境のあらゆるサーバー、コンテナ、サービスへのブックマークと監視状態を美しく統合する個人用Webダッシュボード 30。

| 項目 | 構成定義・詳細仕様 |
| :---- | :---- |
| **想定レシピ構造** | capsule.toml |
| **永続ステート / env** | /app/config |
| **公開ポート** | ホスト側の 3000 ポート |
| **検証コマンド** | ato run github.com/gethomepage/homepage |
| **レシピ作成優先度** | **P1** |

## **タクティカル実装ロードマップと戦略的除外**

Atoの確実な初期ローンチの安定性と信頼を両立させるために、以下に示す3つのステップで段階的にアプリケーションを展開することを提案する。

                  \[タクティカル・ロードマップ\]  
                           
  PHASE 1: ローンチ中核デモ (P0)  
  ├── open-webui/open-webui  ──────► (ローカルAI統括ハブ)  
  ├── n8n-io/n8n             ──────► (ビジュアルワークフロー)  
  ├── usememos/memos         ──────► (超軽量・即時起動体験)  
  ├── nocodb/nocodb          ──────► (データベース操作UI)  
  └── uptime-kuma            ──────► (即席ビジュアル監視)  
                         │  
                         ▼  
  PHASE 2: カタログ拡張用レシピ (P1)  
  ├── lobehub/lobe-chat      ──────► (高意匠マルチエージェント)  
  ├── directus/directus      ──────► (既存DB管理・API化)  
  ├── actualbudget/actual    ──────► (ローカルファースト家計簿)  
  ├── metabase/metabase      ──────► (データダッシュボード)  
  └── changedetection.io     ──────► (実用監視スクレイピング)  
                         │  
                         ▼  
  PHASE 3: AI Ecosystem 展示 (P2)  
  ├── langgenius/dify        ──────► (10+コンテナのオーケストレーション)  
  ├── langfuse/langfuse      ──────► (ClickHouse駆動LLM監視)  
  ├── BerriAI/litellm        ──────► (超実用的APIプロキシ)  
  └── h2oai/h2ogpt           ──────► (ドキュメントオフラインRAG)

### **First 5 recipes (初期デモ・SNS投稿・AODDに使う最優先候補)**

これらのリポジトリは、「一発で動くこと」の驚きと視覚的インパクトが最大であり、初速の話題性に最も貢献する中核製品群である。

1. **open-webui/open-webui**: 自己所有可能なAI体験をホストPC上のGPUやOllamaに自動接続する最有力ハブ 8。  
2. **n8n-io/n8n**: AIワークフローをビジュアルに構築し、Ato上でのエージェント作成を実演できる最高峰 1。  
3. **usememos/memos**: 最初の1秒で「本当にAtoが機能していること」を実感させる驚異的な起動速度 14。  
4. **nocodb/nocodb**: わずらわしいDB設定を完全に隠蔽し、一瞬で「Excelライクな操作画面」を提供する 15。  
5. **uptime-kuma/uptime-kuma**: 動かすだけで美しく機能する死活監視の代表作 16。

### **Next 10 recipes (カタログ拡張用)**

ストアを埋め、実用性を重視するユーザーをAtoランタイムのエコシステムに引き留めるための高価値リポジトリ。 6\. **lobehub/lobe-chat**: デザインにこだわる開発者にAtoの「美しさ」をアピールする 10。 7\. **directus/directus**: 既存の資産（データベース）と連動する実用CMSのデモ 20。 8\. **actualbudget/actual**: ローカルにデータを完全に留める「データ自己所有」の概念実証 21。 9\. **metabase/metabase**: 面倒なJava/JVM初期起動をAtoが包み込んで瞬時に動かす事例 22。 10\. **changedetection-io/changedetection.io**: 開発者が日々手元に置いておきたくなる非常に実用性の高いツール 16。 11\. **blinkospace/blinko**: AIアシスタント内蔵の、今まさにトレンドに入っている新規リポジトリ 25。 12\. **gethomepage/homepage**: 設定をバインドして即時に個人用ポータルを構築する喜びを与える 30。 13\. **hoppscotch/hoppscotch**: Postman等から脱却したい300万開発者に届ける美しいAPIテスター 69。 14\. **budibase/budibase**: 現場に密着した社内ツールをAto上から一瞬で立ち上げて社内展開するデモ 24。 15\. **linkwarden/linkwarden**: Webページ丸ごとの永続アーカイブを実行する便利なブックマーク。

### **Self-hosted AI showcase**

AIコミュニティに深く刺さり、Atoを「AI時代のポータブルアプリストア」としてポジショニングする。

* **langgenius/dify**: 最も求められているAIノーコードプラットフォームをローカルにワンコマンド展開 4。  
* **langfuse/langfuse**: ClickHouseを含む本格的なデータ・オブザーバビリティ環境をホストに構築 5。  
* **BerriAI/litellm**: APIキーを環境変数で一括管理してロードバランスさせる、実務での実用価値が極めて高いプロキシ 34。  
* **h2oai/h2ogpt**: 完全に自己完結し、外部に一切データが流出しないドキュメントQA空間の構築 35。  
* **getzep/zep**: エージェントのコンテキスト記憶を永続化し、複数のツール群とAPI接続するデータ基盤 36。

### **Avoid for now (初期には危険・延期すべき候補)**

人気は極めて高いが、初期ローンチで展開するには技術的およびセキュリティ上の致命的リスクが伴うため、意図的に除外するリポジトリ。

1. **coollabsio/coolify / dokploy/dokploy**  
   * **除外理由**: ホストオペレーティングシステムの Docker デーモン（/var/run/docker.sock）に直接接続し、ホストマシンの最上位特権で他のコンテナを生成・破壊するアーキテクチャである 16。Atoの目指す「サンドボックス化された安全なカプセル実行」と完全に相反し、万が一のコンテナ脱出（Container Escape）などの壊滅的なセキュリティ侵害のリスクがある。  
   * **リスククラス**: **システム特権の奪取リスク**  
2. **All-Hands-AI/OpenHands**  
   * **除外理由**: AIソフトウェア開発エージェントとして人気を博しているが 6、エージェントがコードを実行し結果をテストするために、ホストのDockerソケットに全面的に依存したコンテナ管理を要求する 52。Atoランタイムの内部でソケットをマウントさせて動かすことはセキュリティ隔離ポリシーを崩壊させる。  
   * **リスククラス**: **サンドボックス隔離の破壊リスク**  
3. **AUTOMATIC1111/stable-diffusion-webui / ComfyUI/ComfyUI**  
   * **除外理由**: 本コンテナおよび動作環境は、ホスト上の物理GPU（NvidiaやApple Silicon）およびそれに対応する特定のドライババージョンとの極めて繊細なバインディングを要求する 45。加えて、起動時に十数ギガバイトに及ぶベースチェックポイントモデルの初期ダウンロードが強制されるため、Atoランタイムの「ワンコマンドで数秒で動く」というアジリティ体験を大きく損なう。  
   * **リスククラス**: **環境依存の非互換性およびデータ大容量化**  
4. **supabase/supabase**  
   * **除外理由**: ローカル起動時に10以上の独立したデータベース、認証、ストレージ、Edge Functions実行用コンテナが立ち上がり 48、さらにホスト側の広範囲なポート範囲を占有するため、ローカルホスト内でのポート衝突が多発する。初期のレシピ安定性を損なうため、ローンチフェーズでは提供を避ける。  
   * **リスククラス**: **ネットワークポート競合およびリソース過負荷**  
5. **posthog/posthog**  
   * **除外理由**: 大規模プロダクト分析のために、ClickHouse、Kafka、PostgreSQL、Redis、インジェクションワーカーなど、極めて巨大かつ複雑なミドルウェアスタックの常時稼働を要求する 22。標準的な開発者のローカルPC（RAM 8GB〜16GB）のCPUおよびメモリを瞬時に枯渇させ、Atoランタイム自体が原因であるかのような誤解を与える。  
   * **リスククラス**: **ハードウェアリソース枯渇**

## **Atoの初期ローンチで見せるべきデモ・ストーリー（3案）**

Atoのポータビリティと、一度実行された環境が完全に「ロック・再現」されることの驚きを開発者に最も強く印象づけるための、具体的なローンチ実演デモシナリオ。

### **シナリオ案 1: 「ローカルAIを1秒で。ポート衝突なし、接続トラブルゼロの完全自己管理チャット」**

  【従来のローカルAIセットアップ】  
  \[Ollamaインストール\] ──► \[ホストポートの競合を解決\] ──► \[フロントエンドコンテナ起動\]   
  ──► \[コンテナ・ホスト間の通信エラーとデバッグ\] ──► ようやく起動（15〜30分） \[8, 58\]  
    
  【AtoによるAI起動体験】  
  $ ato run open-webui  ──────────────────────────────────► 3秒後にブラウザ起動（完了） \[7, 12\]

* **デモの流れ**:  
  1. 開発者のPC（事前にOllamaがインストールされていると仮定）で、Atoコマンドを実行する。  
  2. コマンド一発で open-webui が立ち上がる 7。  
  3. Atoランタイムがホスト上の host.docker.internal を自動解決し 7、ホストマシンのGPUを利用可能なOllamaインスタンスに一瞬で安全にブリッジする 8。  
  4. ユーザーはローカル上のAIモデル（Llama等）と直ちに美しくチャットを開始できる 8。  
* **訴求メッセージ**: 「コンテナからホストのOllamaへのネットワーク経路デバッグに、もう時間を溶かす必要はありません。Atoなら、ホストリソースとの接続関係をロックしたレシピにより、あらゆるローカルAIチャットを一瞬で立ち上げます 8。」

### **シナリオ案 2: 「ClickHouseとS3を内蔵したエンタープライズLLM監視（Langfuse V4）を一発起動」**

  【手動 ClickHouse スタック構築】  
  ──► \[ ClickHouse スキーマ構築 \] ──►   
  ──► \[ MinIO バケット初期化 \] ──► 一部の起動遅延で全体接続エラー（20分） \[29, 67\]  
    
  【Atoの remixture 起動体験】  
  $ ato run langfuse  ────────────────────────────────────► 全コンテナ完全協調起動（完了） 

* **デモの流れ**:  
  1. 開発者は、ローカルのLLMアプリ開発において「Observability（可観測性）」を実装しようとする 28。  
  2. 2026年最新の langfuse V4スタックを起動するため、Atoで一気にサービス群を立ち上げる 5。  
  3. Atoは、ClickHouse、PostgreSQL、Redis、MinIO、そして本体Webとワーカーを一斉に立ち上げ 5、MinIOへのバケット自動生成スクリプトとClickHouseのヘルスチェックをすべて順序どおりに自動制御する 5。  
  4. 起動完了と同時に、完璧に連携したLangfuseダッシュボード（localhost:3000）が開く 5。  
* **訴求メッセージ**: 「ClickHouse、MinIO、Redisを含む現代的なLLM監視スタックの構築は、極めて難易度が高い作業でした 5。Atoはマルチコンテナの複雑な依存関係、初期バケット構築、データベース起動待機を1つのレシピに完璧にロックします。あなたはただアプリからトレースを送るだけです 5。」

### **シナリオ案 3: 「リモートコード実行のリスクを排除した、安全にサンドボックス隔離されたAIエージェント開発」**

  【無防備なエージェント起動】  
  \[AI Agent\] ──► \[ 生成されたPythonコード \] ──► ──► 【システム破壊・漏洩】   
    
  【Atoランタイム上での実行】  
  \[AI Agent\] ──► \[ 生成されたPythonコード \] ──► \[ Atoで隔離されたサンドボックス \] ──► 【ホストは完全に無傷】

* **デモの流れ**:  
  1. 開発者は、ビジュアルエージェント開発ツール（Flowiseなど）を使用して、CSV解析を行うAIエージェントを実行する 31。  
  2. 生成されたPythonコードをコンテナ側が解釈する際、意図せぬコードインジェクションによるホストの乗っ取り脆弱性が存在する 32。  
  3. AtoでFlowiseを動かしている場合、Atoの「完全なサンドボックス隔離レイヤー」が、コンテナからホストオペレーティングシステムへの不正な特権要求、ファイル読み込み、ネットワーク走査をすべて自動的にインターセプトし、完全に拒否する。  
  4. 画面上でエージェント機能は完全に稼働しつつ、ホストOSは堅牢に保護される。  
* **訴求メッセージ**: 「ローカルAIエージェントツールは便利ですが、モデルが生成する動的コードのホストへの実行など、深刻なセキュリティリスクがつきまといます 32。AtoはすべてのセルフホストAIツールを安全な隔離サンドボックス内で実行するため、脆弱性を持つリポジトリであっても、あなたのメインマシンを守りながら安心して試作することができます 33。」

#### **引用文献**

1. Top AI GitHub Repositories in 2026 | by Shubh Jain | Write A Catalyst \- Medium, 5月 23, 2026にアクセス、 [https://medium.com/write-a-catalyst/top-ai-github-repositories-in-2026-e08af3e88314](https://medium.com/write-a-catalyst/top-ai-github-repositories-in-2026-e08af3e88314)  
2. GitHub Trending: January 5, 2026 — The New Year Begins | by Baozilla, Let's go\!, 5月 23, 2026にアクセス、 [https://medium.com/@lssmj2014/welcome-to-2026-9a52575cbd1d](https://medium.com/@lssmj2014/welcome-to-2026-9a52575cbd1d)  
3. Quick start | Immich, 5月 23, 2026にアクセス、 [https://docs.immich.app/overview/quick-start](https://docs.immich.app/overview/quick-start)  
4. Deploy Dify with Docker Compose \- Dify Docs, 5月 23, 2026にアクセス、 [https://docs.dify.ai/en/self-host/quick-start/docker-compose](https://docs.dify.ai/en/self-host/quick-start/docker-compose)  
5. Self-host Langfuse (Open Source LLM Observability), 5月 23, 2026にアクセス、 [https://langfuse.com/self-hosting](https://langfuse.com/self-hosting)  
6. Trending AI Repositories on GitHub — Real-Time Rankings 2026 | OSSInsight, 5月 23, 2026にアクセス、 [https://ossinsight.io/trending/ai](https://ossinsight.io/trending/ai)  
7. open-webui/docker-compose.yaml at main \- GitHub, 5月 23, 2026にアクセス、 [https://github.com/open-webui/open-webui/blob/main/docker-compose.yaml](https://github.com/open-webui/open-webui/blob/main/docker-compose.yaml)  
8. Open WebUI integration \- Docker Docs, 5月 23, 2026にアクセス、 [https://docs.docker.com/ai/model-runner/openwebui-integration/](https://docs.docker.com/ai/model-runner/openwebui-integration/)  
9. LobeChat AI Assistant | Guides \- Clore.ai, 5月 23, 2026にアクセス、 [https://docs.clore.ai/guides/ai-platforms-and-agents/lobechat](https://docs.clore.ai/guides/ai-platforms-and-agents/lobechat)  
10. lobe-chat | Skills Marketplace \- LobeHub, 5月 23, 2026にアクセス、 [https://lobehub.com/skills/enuno-claude-command-and-control-lobe-chat](https://lobehub.com/skills/enuno-claude-command-and-control-lobe-chat)  
11. n8n \- Workflow Automation \- GitHub, 5月 23, 2026にアクセス、 [https://github.com/n8n-io](https://github.com/n8n-io)  
12. open-webui · GitHub Topics, 5月 23, 2026にアクセス、 [https://github.com/topics/open-webui](https://github.com/topics/open-webui)  
13. After Claude Code: 6 Open-Source Tools You Should Know | by NocoBase \- Medium, 5月 23, 2026にアクセス、 [https://medium.com/@nocobase/after-claude-code-6-open-source-tools-you-should-know-a85c989424a2](https://medium.com/@nocobase/after-claude-code-6-open-source-tools-you-should-know-a85c989424a2)  
14. Top 19 Trending Go Projects on GitHub \- January 2026 \- Rost Glukhov, 5月 23, 2026にアクセス、 [https://www.glukhov.org/post/2026/01/most-popular-go-projects-on-github/](https://www.glukhov.org/post/2026/01/most-popular-go-projects-on-github/)  
15. GitHub \- nocodb/nocodb: A Free & Self-hostable Airtable Alternative, 5月 23, 2026にアクセス、 [https://github.com/nocodb/nocodb](https://github.com/nocodb/nocodb)  
16. 25 Trending Self-Hosted Projects on GitHub \- DEV Community, 5月 23, 2026にアクセス、 [https://dev.to/web\_dev-usman/25-trending-self-hosted-projects-on-github-4nom](https://dev.to/web_dev-usman/25-trending-self-hosted-projects-on-github-4nom)  
17. LobeHub \- GitHub, 5月 23, 2026にアクセス、 [https://github.com/lobehub](https://github.com/lobehub)  
18. How to Use ModelsLab Models in LobeChat (2026 Setup Guide), 5月 23, 2026にアクセス、 [https://modelslab.com/blog/api/lobechat-modelslab-provider-setup-guide-2026](https://modelslab.com/blog/api/lobechat-modelslab-provider-setup-guide-2026)  
19. Is there a Docker Compose file or deployment guide for the Next version? \#11654 \- GitHub, 5月 23, 2026にアクセス、 [https://github.com/lobehub/lobehub/discussions/11654](https://github.com/lobehub/lobehub/discussions/11654)  
20. Releases · directus/directus \- GitHub, 5月 23, 2026にアクセス、 [https://github.com/directus/directus/releases](https://github.com/directus/directus/releases)  
21. actualbudget/actual: A local-first personal finance app \- GitHub, 5月 23, 2026にアクセス、 [https://github.com/actualbudget/actual](https://github.com/actualbudget/actual)  
22. 14 Best Analytics Dashboard Templates 2026 \- AdminLTE.IO, 5月 23, 2026にアクセス、 [https://adminlte.io/blog/analytics-dashboard-templates/](https://adminlte.io/blog/analytics-dashboard-templates/)  
23. Twenty CRM is the open source alternative to Salesforce and HubSpot \- Pasquale Pillitteri, 5月 23, 2026にアクセス、 [https://pasqualepillitteri.it/en/news/954/twenty-crm-open-source-salesforce-hubspot-alternative](https://pasqualepillitteri.it/en/news/954/twenty-crm-open-source-salesforce-hubspot-alternative)  
24. Budibase/budibase: AI agents, automations and apps that run your operations. Model agnostic. \- GitHub, 5月 23, 2026にアクセス、 [https://github.com/budibase/budibase](https://github.com/budibase/budibase)  
25. GitHub \- blinkospace/blinko: An open-source, self-hosted personal AI note tool prioritizing privacy, built using TypeScript, 5月 23, 2026にアクセス、 [https://github.com/blinkospace/blinko](https://github.com/blinkospace/blinko)  
26. Building a Github Stars tracker for your open source project \- Appsmith, 5月 23, 2026にアクセス、 [https://www.appsmith.com/blog/building-a-github-star-tracker-for-your-open-source-project](https://www.appsmith.com/blog/building-a-github-star-tracker-for-your-open-source-project)  
27. About Langfuse, 5月 23, 2026にアクセス、 [https://langfuse.com/about/about](https://langfuse.com/about/about)  
28. Why do customers choose Langfuse?, 5月 23, 2026にアクセス、 [https://langfuse.com/handbook/chapters/why](https://langfuse.com/handbook/chapters/why)  
29. langfuse/docker-compose.yml at main \- GitHub, 5月 23, 2026にアクセス、 [https://github.com/langfuse/langfuse/blob/main/docker-compose.yml](https://github.com/langfuse/langfuse/blob/main/docker-compose.yml)  
30. Configuration: Help with group layout and columns · gethomepage homepage · Discussion \#6323 · GitHub, 5月 23, 2026にアクセス、 [https://github.com/gethomepage/homepage/discussions/6323](https://github.com/gethomepage/homepage/discussions/6323)  
31. CSV Agent Prompt Injection Remote Code Execution Vulnerability · Advisory · FlowiseAI/Flowise \- GitHub, 5月 23, 2026にアクセス、 [https://github.com/FlowiseAI/Flowise/security/advisories/GHSA-3hjv-c53m-58jj](https://github.com/FlowiseAI/Flowise/security/advisories/GHSA-3hjv-c53m-58jj)  
32. Flowise: CSV Agent Prompt Injection Remote Code Execution Vulnerability · CVE-2026-41264 \- GitHub, 5月 23, 2026にアクセス、 [https://github.com/advisories/GHSA-3hjv-c53m-58jj](https://github.com/advisories/GHSA-3hjv-c53m-58jj)  
33. FlowiseAI Flowise RCE via CustomMCP Node CVE-2025-59528 \- SonicWall, 5月 23, 2026にアクセス、 [https://www.sonicwall.com/blog/flowiseai-custom-mcp-node-remote-code-execution-](https://www.sonicwall.com/blog/flowiseai-custom-mcp-node-remote-code-execution-)  
34. CVE-2026-42208: Targeted SQL injection against LiteLLM's authentication path discovered 36 hours following vulnerability disclosure | Sysdig, 5月 23, 2026にアクセス、 [https://www.sysdig.com/blog/cve-2026-42208-targeted-sql-injection-against-litellms-authentication-path-discovered-36-hours-following-vulnerability-disclosure](https://www.sysdig.com/blog/cve-2026-42208-targeted-sql-injection-against-litellms-authentication-path-discovered-36-hours-following-vulnerability-disclosure)  
35. h2oai/h2ogpt: Private chat with local GPT with document, images, video, etc. 100% private, Apache 2.0. Supports oLLaMa, Mixtral, llama.cpp, and more. Demo: https://gpt.h2o.ai/ https://gpt-docs.h2o.ai/ · GitHub \- GitHub, 5月 23, 2026にアクセス、 [https://github.com/h2oai/h2ogpt](https://github.com/h2oai/h2ogpt)  
36. getzep/zep: Zep | Examples, Integrations, & More \- GitHub, 5月 23, 2026にアクセス、 [https://github.com/getzep/zep](https://github.com/getzep/zep)  
37. 5 Most Popular Open-Source AI Project Management Tools on GitHub \- NocoBase, 5月 23, 2026にアクセス、 [https://www.nocobase.com/en/blog/5-most-popular-open-source-ai-project-management-tools-on-github](https://www.nocobase.com/en/blog/5-most-popular-open-source-ai-project-management-tools-on-github)  
38. Best Open Source Alternatives to Notion in 2026, 5月 23, 2026にアクセス、 [https://www.opensourcealternatives.to/alternative-to/notion](https://www.opensourcealternatives.to/alternative-to/notion)  
39. GitHub \- photoprism/photoprism: AI-Powered Photos App for the Decentralized Web, 5月 23, 2026にアクセス、 [https://github.com/photoprism/photoprism](https://github.com/photoprism/photoprism)  
40. Top Metabase Alternatives for Startups, Teams & Enterprises in 2026 \- Draxlr, 5月 23, 2026にアクセス、 [https://www.draxlr.com/blogs/metabase-alternatives-for-startups-teams-enterprises/](https://www.draxlr.com/blogs/metabase-alternatives-for-startups-teams-enterprises/)  
41. Invoke \- GitHub, 5月 23, 2026にアクセス、 [https://github.com/invoke-ai](https://github.com/invoke-ai)  
42. InvokeAI \- PyPI, 5月 23, 2026にアクセス、 [https://pypi.org/project/InvokeAI/](https://pypi.org/project/InvokeAI/)  
43. maybe-finance/maybe: The personal finance app for everyone \- GitHub, 5月 23, 2026にアクセス、 [https://github.com/maybe-finance/maybe](https://github.com/maybe-finance/maybe)  
44. TOP 34 Ai Open Source Projects in 2026 \- Web3 Jobs, 5月 23, 2026にアクセス、 [https://web3.career/learn-web3/top-ai-open-source-projects](https://web3.career/learn-web3/top-ai-open-source-projects)  
45. AUTOMATIC1111/stable-diffusion-webui: Stable Diffusion web UI \- GitHub, 5月 23, 2026にアクセス、 [https://github.com/automatic1111/stable-diffusion-webui](https://github.com/automatic1111/stable-diffusion-webui)  
46. AUTOMATIC1111 Stable Diffusion web UI download | SourceForge.net, 5月 23, 2026にアクセス、 [https://sourceforge.net/projects/automatic1111-web-ui.mirror/](https://sourceforge.net/projects/automatic1111-web-ui.mirror/)  
47. v2.5.0 \- 90000 Stars Release · immich-app immich · Discussion \#25577 \- GitHub, 5月 23, 2026にアクセス、 [https://github.com/immich-app/immich/discussions/25577](https://github.com/immich-app/immich/discussions/25577)  
48. 100,000 GitHub stars \- Supabase, 5月 23, 2026にアクセス、 [https://supabase.com/blog/100000-github-stars](https://supabase.com/blog/100000-github-stars)  
49. Developer Update \- May 2026 · supabase · Discussion \#45702 \- GitHub, 5月 23, 2026にアクセス、 [https://github.com/orgs/supabase/discussions/45702](https://github.com/orgs/supabase/discussions/45702)  
50. Developer Update \- March 2026 · supabase · Discussion \#43465 \- GitHub, 5月 23, 2026にアクセス、 [https://github.com/orgs/supabase/discussions/43465](https://github.com/orgs/supabase/discussions/43465)  
51. Dokploy/dokploy: Open Source Alternative to Vercel, Netlify and Heroku. \- GitHub, 5月 23, 2026にアクセス、 [https://github.com/dokploy/dokploy](https://github.com/dokploy/dokploy)  
52. All Hands AI \- GitHub, 5月 23, 2026にアクセス、 [https://github.com/All-Hands-AI](https://github.com/All-Hands-AI)  
53. GitHub \- usebruno/bruno: Opensource IDE For Exploring and Testing API's (lightweight alternative to Postman/Insomnia), 5月 23, 2026にアクセス、 [https://github.com/usebruno/bruno](https://github.com/usebruno/bruno)  
54. Testimonials ❤️ · usebruno bruno · Discussion \#343 \- GitHub, 5月 23, 2026にアクセス、 [https://github.com/usebruno/bruno/discussions/343](https://github.com/usebruno/bruno/discussions/343)  
55. Agentic harness ranking \- based on github stars, 5月 23, 2026にアクセス、 [https://gist.github.com/thoroc/46f1623e96c2df64528ea9ee97acf95d](https://gist.github.com/thoroc/46f1623e96c2df64528ea9ee97acf95d)  
56. This Week In Continue \- January 13 \- January 20, 2026 · continuedev continue · Discussion \#9714 · GitHub, 5月 23, 2026にアクセス、 [https://github.com/continuedev/continue/discussions/9714](https://github.com/continuedev/continue/discussions/9714)  
57. How to Deploy Open WebUI for AI Chat via Portainer \- OneUptime, 5月 23, 2026にアクセス、 [https://oneuptime.com/blog/post/2026-03-20-deploy-open-webui-ai-chat-portainer/view](https://oneuptime.com/blog/post/2026-03-20-deploy-open-webui-ai-chat-portainer/view)  
58. Quick Start \- Open WebUI, 5月 23, 2026にアクセス、 [https://docs.openwebui.com/getting-started/quick-start/](https://docs.openwebui.com/getting-started/quick-start/)  
59. Lobe Chat \- an open-source, modern-design LLMs/AI chat framework. Supports Multi AI Providers( OpenAI / Claude 3 / Gemini / Ollama / Bedrock / Azure / Mistral / Perplexity ), Multi-Modals (Vision/TTS) and plugin system. One-click FREE deployment of your private ChatGPT chat application. · GitHub, 5月 23, 2026にアクセス、 [https://github.com/AIDotNet/lobe-chat](https://github.com/AIDotNet/lobe-chat)  
60. Dify: Leading Agentic Workflow Builder, 5月 23, 2026にアクセス、 [https://dify.ai/](https://dify.ai/)  
61. GitHub \- langgenius/dify: Production-ready platform for agentic workflow development., 5月 23, 2026にアクセス、 [https://github.com/langgenius/dify](https://github.com/langgenius/dify)  
62. Local Source Code Start \- Dify Docs, 5月 23, 2026にアクセス、 [https://docs.dify.ai/en/self-host/advanced-deployments/local-source-code](https://docs.dify.ai/en/self-host/advanced-deployments/local-source-code)  
63. How to Run Dify in Docker for LLM Application Building \- OneUptime, 5月 23, 2026にアクセス、 [https://oneuptime.com/blog/post/2026-02-08-how-to-run-dify-in-docker-for-llm-application-building/view](https://oneuptime.com/blog/post/2026-02-08-how-to-run-dify-in-docker-for-llm-application-building/view)  
64. Lobe Chat \- an open-source, modern-design AI chat framework. Supports Multi AI Providers( OpenAI / Claude 3 / Gemini / Ollama / Qwen / DeepSeek), Knowledge Base (file upload / knowledge management / RAG ), Multi-Modals (Vision/TTS/Plugins/Artifacts). One-click FREE deployment of your private ChatGPT \- GitHub, 5月 23, 2026にアクセス、 [https://github.com/isaccanedo/lobe-chat](https://github.com/isaccanedo/lobe-chat)  
65. Lobe Chat | Dokploy, 5月 23, 2026にアクセス、 [https://docs.dokploy.com/docs/templates/lobe-chat](https://docs.dokploy.com/docs/templates/lobe-chat)  
66. Upcoming architecture changes: Simplify Langfuse for Scale (v4) \#12518 \- GitHub, 5月 23, 2026にアクセス、 [https://github.com/orgs/langfuse/discussions/12518](https://github.com/orgs/langfuse/discussions/12518)  
67. langfuse/docker-compose.dev-azure.yml at main \- GitHub, 5月 23, 2026にアクセス、 [https://github.com/langfuse/langfuse/blob/main/docker-compose.dev-azure.yml](https://github.com/langfuse/langfuse/blob/main/docker-compose.dev-azure.yml)  
68. Docker Compose Deployment (Self-Hosted) \- Langfuse, 5月 23, 2026にアクセス、 [https://langfuse.com/self-hosting/deployment/docker-compose](https://langfuse.com/self-hosting/deployment/docker-compose)  
69. Hoppscotch \- GitHub, 5月 23, 2026にアクセス、 [https://github.com/hoppscotch](https://github.com/hoppscotch)  
70. Open-Source API Development Ecosystem • https://hoppscotch.io • Offline, On-Prem & Cloud • Web, Desktop & CLI \- GitHub, 5月 23, 2026にアクセス、 [https://github.com/hoppscotch/hoppscotch](https://github.com/hoppscotch/hoppscotch)

[image1]: <data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAABAAAAAbCAYAAAB1NA+iAAAA0UlEQVR4XmNgGAVUB5FAvIVIjBWwArE4EP+HYjEg5gFibiAWBWJzIH4ElcMLQAr+oQsiAbwGmDBAFPSjSyABvAaA/AdSIIAkxgLE85D4n5DYGADmf2RwGYjl0cRwApgB6JgoYMYAUdyJJKYHFSMKlDNAFHugid9FYgsDMSMSHwV8ZsC0DZQ2vJD4P5HYGICQf0GJaR+6IAywMUA0n0aXQAIgeVCUYgUTGCAK/NAlgCCAASKH1fmrgPgPAyTpokcdCIPEfwPxdyA2gOoZBaMABQAAVKQ7MAzlGNQAAAAASUVORK5CYII=>

[image2]: <data:image/png;base64,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>

[image3]: <data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAABwAAAAaCAYAAACkVDyJAAABRklEQVR4XmNgGAWjgEJQD8RfgPg/FJ9FlcYADxkQakH6ilGliQcwQ0AYF9AD4loGiBpjNDmSwRMGhE9xgcdAfIwBvxqigBcQpwDxFgbchq2D0oRCgShwAkqD4gObYTxAnAtlg+RXI8mRBWCWgOIFxJZBkgOBH1DanQEir4UkRxZ4isQGGRiHxM8HYm4oGxQS2EKAJABydRoSH2TgQiQ+cvBRNf5gAGQgKDWCwDNkCQaI3Co0MZIBuothvrAFYh0kcW+ouDaSGFngJRr/DQPE4Nto4qASCN1xIHCXAVUcmxowYGSAKAYVVchgOQN2TdjiLxBK/wViPij7N5RGAT1A/AGI3wLxZyD+gyTnA8ShSPyvDAi1nxggBlYiyYMAskPOI7FpAtKB+CASfwISmyagHYjroOxEZAlagu8MkOxlgi4xCkYBWQAA23JVqiMzR7wAAAAASUVORK5CYII=>

[image4]: <data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAACIAAAAaCAYAAADSbo4CAAABVElEQVR4Xu2Uu0rEUBCGf/AKq6idgtvZWGhpZS1WtloJFm5jYyEI2vkAYmclloJ2NoKVooX6CoKVWCneEASv/2EmMk6yspEksJAPPnJmTnbPJDMEKClpUg7pV4MWwl+H7dI3n8yDFkgRF35D6abHPpkHS5BCJl2+U689dM1u5MUj4m1ZoUO6bqMVs5cbfj6GXVwI0XwkWSjLkEOnTG6U7pm4ETp8Ii3PiD/9PKQ9afD/kZos2tBHP30yDe2QIs79Rh0u6Tat0XXN1ZurKr2j+/TM5BPZgPx42m8k8EK7dH1CV83eB+StRIzQWxNf0wkT/7ADmY17SNUP9JVu2psM4/j9tGEd3qaNLSHuN3E4Y8HE/ya0ZMvE9uBexOcjqbBMCG2Y1fUMfacDtBXy6Y/e5IFe7cGLkFZmRpiRGzoImYlTzYdinuiRxoExyD1XdM7kS0qak28TyFm3EUlggAAAAABJRU5ErkJggg==>

[image5]: <data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAACYAAAAaCAYAAADbhS54AAABb0lEQVR4Xu2VQStFURDH/ywUkZ1SPoEsbO1IWLJUvoAlWyyebGRhR1koyUr5DESyYUUkZW+jLCQhZsyc+8a4993rcb3N+dW/N+c/051z3zn3HCASiXyyQnogvaueSPfOu0qqG0CYRBrPyM6VDjc+9KbSDslXnF86k5DGQz5hqPWPlsYl8ps2ZGJFmhap+XO44YE3DaOQmn/9OsP+GnS+5QZS0+UTP2QJ8pxun0jjGvlLxPk3b9ZJXq+EvL0TPoxmn6iDDtTu9QUuzDq/FiH5PudXSBekXh1zTWeSBTZIO6Rj0pHxl0mbZpzJPOShI86fgFxJL84PjJHWII2YPVK/xrukGY2HSY8aM/y8HjP+xjpkz4RlZL1Crh6+K/ltW5LqdOySjJvY+tukBTMuvIy/wTa5098p53McXjBca6XShuoy84Ye0JgncatxK6oTmSXNkbYgR0apnEIO5VXn75NOSNOkc9KZ+k2QfWuXNhKJMB9TJmBCnE20GgAAAABJRU5ErkJggg==>

[image6]: <data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAACsAAAAaCAYAAAAue6XIAAABmUlEQVR4Xu2WzSsFURjGX4SdfMZGlJKlUja2rCyIkqyUpQU7/gIbZWtB8T+IlXSzsBJWpGxYSPKxEELheXvP1OmZQ3Nz79yr5le/ZuY508x7zpw5MyIZGRlFZwt+5WFJ0QImAhkX1hPIUqVRbGR9KsWKOqFcueIgTbZhBWXzYsWOUF4DVyhLlTkOwKOEH3c9bOWw1ITma1lSJVboITf8QElHekGs2GFuCLALB+AbN6TFkySfAnpeG+zghrRIOl8bJNl5RUOXpiTzNepQqGO18AFuwHMv187pKrMHR8U+MBF18FVsWk15+a+sid18mvIQx3CcMr2pX/yB2+o67ue6X03HSjc88/IYY/BFrNf3Tp237xIfNR9t44/JDbyG+/AWtrhcR2w9Okni1/10WY7ygsE3VDTr4lAsb3f7nWKDwwzCDzjLDX+FH2uEjmqfd7zsts9edgSX4Ko71uv0uv1Nsa9kQZmEOxw6tLAcPPWyZrG1+A42ib2AM65NX6hLeCHJ1va8GBKbk7q+ljWLYr+N/mMta/o5yPjvfAMX0GdVu6qz6QAAAABJRU5ErkJggg==>

[image7]: <data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAC0AAAAaCAYAAAAjZdWPAAABq0lEQVR4Xu2WSyhFQRjHvxQbj+ztJSs7G1nZeeSxk5ViQ7JgI1sryYIFWTjFQoqIrJS7Ja8dSSGsiYWFPP6f80135rvj3ttFTTm/+nXn/Gc657tnZs45RAkJ/4dqOA9brWzEagdFMXyHC7ACNsIPOA6frXFBwQU26JDifEyHIRBRXJwPznkWgoMLy1Z0kJiip3RHyExSunDjnDMiUIYos/ALZ0TgNFH2de6jVAd/SZcOhCXKv+gWeKrDAjiE7TrUtMFhHQqjlH/RJ7BDhwWQ1/WO4LoOhTfK3Iz8pnyBu7Ab1pK7B1Zk3DF8onifRLBHcoY/CbbhFdySbJXc89RI7sUMKlP5Gvlf3eZO8PfJmSdn+qyMz3sOpyUbhBvSLpExBp51nrGc3MEi+EjxCR7kN7LG2PC3CfenrIwv5lvPdkEGO+uleMYMPDs513Oh8JPlFQ7IMV+sM939RT/cUVkVuUVfk/ut4/uTP4ZPWiftCFZaOcPru1za97Be2jZ2Yaa9qY4X5fdX4I13Ay9hs5VPwD1yH53f3bUZeABn4TLct/puKX7k8WZPSEgIlU/vCmxxiIMUFQAAAABJRU5ErkJggg==>