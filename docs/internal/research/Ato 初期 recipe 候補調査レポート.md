# **Atoランタイム初期レシピ戦略：極小フットプリントとマルチランタイム駆動によるOSS展開のロードマップ**

Atoは、配布元がどのような形態（GitHubレポジトリ、ローカルプロジェクト、Docker Compose、またはシェルスクリプトなど）であっても、ユーザーに「READMEを一切読み解くことなく、コマンド一つでアプリケーションを起動し検証できる」という独自の統合検証体験を提供する画期的な実行ランタイムである。本リサーチ報告書では、Ato開発チームが初期レシピ（Recipe）として優先的に構築・保守すべきOSSアプリケーションについて、5つの主要カテゴリにわたり横断的に調査を実施した。選定においては、Atoランタイム独自のバリュープロポジションである「ソースコード（Native）とOCIコンテナの透過的オーケストレーション」を証明できること、保守容易性が高いこと、そしてホスト環境に余計な設定負担を課さないことを最優先評価軸としている。  
単一コンテナで完結する軽量ユーティリティから、AI機能やデータベースが協調して動作する高付加価値なマルチサービススタックまで、Atoランタイムの性能を余すところなく実証するための30件の選定候補テーブル、および技術的に徹底解剖した上位10件の詳細システムプロファイルを以下に提示する。

## **カテゴリ横断型初期候補30選比較**

本選定リストは、Atoの適合性評価基準（公開レポジトリ、ライセンスの透明性、複数アーキテクチャ対応、特権不要、ポートおよびステート管理の明確さ）に基づき、30件のプロジェクトを格付けしたものである。

| Rank | Repo | Category | Use case | Runtime shape | Services | Recipe path | Difficulty | Demo value | Risks | Why it fits Ato |
| :---- | :---- | :---- | :---- | :---- | :---- | :---- | :---- | :---- | :---- | :---- |
| 1 | usememos/memos 1 | Personal productivity | 超軽量タイムライン型メモシステム 1 | OCI / Native Dual 1 | Single Process (SQLite) 1 | explicit capsule.toml 1 | A | 極めて高い 1 | マウント先の権限不一致 3 | 20MBの極小OCIフットプリントとGoバイナリの二面性によるポータビリティ 1 |
| 2 | blinkospace/blinko 4 | Personal productivity | RAG内蔵AIカード型メモツール 4 | OCI-first | app \+ postgres 5 | \--oci-compose 6 | B | 圧倒的なモダンAI UI 4 | ホストNW駆動構成の競合 6 | AI/RAG体験を単一コマンドでホスト側GPU不要で立ち上げる親和性 4 |
| 3 | sosedoff/pgweb 7 | Developer tools | Go製ブラウザ型Postgresクライアント 7 | Native-first / OCI 7 | Single Process 7 | source/native inference | A | 開発者の日常ユーティリティ 7 | ホストDBへのNWバインド 7 | Goシングルバイナリで依存性ゼロ、即座に接続検証可能 7 |
| 4 | axllent/mailpit 10 | Developer tools | 開発者向けSMTPダミーサーバー 10 | Native-first / OCI 10 | Single Process 10 | explicit capsule.toml | A | 抜群の開発実用性 11 | ポート1025/8025の競合 10 | メール送信テストをエミュレートし、即時Web画面で確認できる爽快感 10 |
| 5 | linkwarden/linkwarden 13 | Personal productivity | 完全魚拓アーカイブ付ブックマーク 13 | OCI-first | app \+ postgres 14 | \--oci-compose 14 | B | 視覚的なWebキャプチャ表示 13 | Puppeteer高負荷 13 | 複雑なアーカイブエンジン（Next.js \+ DB）を裏側で自動連動 13 |
| 6 | open-webui/open-webui 15 | AI / agent tools | Ollama/OpenAI汎用ローカルAI UI 15 | OCI-first | Single Process 15 | explicit capsule.toml | B | 最も高いトレンド価値 17 | ホストOllama接続エラー 18 | ホスト側Ollama自動検出ブリッジと組み合わせることで価値が爆発 15 |
| 7 | go-vikunja/vikunja 19 | Personal productivity | 高機能タスク・カンバン管理ツール 19 | OCI-first / Native 21 | Single Process (SQLite) 22 | explicit capsule.toml | B | 実用的なタスク管理 20 | パーミッションの壁 21 | Go/JSハイブリッド構造で、SQLite駆動による一発起動が極めて容易 22 |
| 8 | go-shiori/shiori 24 | Personal productivity | Go製Pocketクローンブックマーク 24 | Native-first / OCI 24 | Single Process (SQLite) 24 | source/native inference | A | CLIとWeb UIの高度な統合 24 | アーカイブ生成の負荷 24 | Goバイナリ1本でCLIとWebポート起動を同時に達成可能 24 |
| 9 | Mintplex-Labs/anything-llm 25 | AI / agent tools | 完全ローカルRAGナレッジベース 25 | OCI-first | Single Process 25 | explicit capsule.toml | B | 完全ローカルRAGの即時体験 25 | 初回起動時のモデル取得遅延 | Vector DB、LLM、Embedderの内包構成による一発起動のデモ効果 25 |
| 10 | flowiseai/flowise 27 | AI / agent tools | LLMワークフロードラッグ＆ドロップ 27 | Native-first / OCI 27 | Single Process 27 | source/native inference | B | ビジュアルフロー作成の衝撃 28 | Node.jsメモリ不足制限 27 | npm/npxでローカル開発環境のように動くNativeの特性をフルに発揮 27 |
| 11 | gristlabs/grist-core 29 | Data / internal tools | 関係性データベース機能付表計算 29 | OCI-first | Single Process 29 | \--oci-compose | B | 実用的な業務DB構築 29 | 特になし 29 | SQLiteベースのデータ永続化とPythonサンドボックスの優れた連携 29 |
| 12 | umami-software/umami 31 | Self-hosted mini apps | プライバシー配慮型アクセス解析 31 | OCI-first | app \+ postgres 31 | \--oci-compose 31 | B | 実用的なデータダッシュボード | Postgresポート競合 31 | Prismaマイグレーション自動化とNext.js起動プロセスの実証 31 |
| 13 | filebrowser/filebrowser 32 | Self-hosted mini apps | 軽量・多機能Webファイルブラウザ 32 | Native-first / OCI 32 | Single Process (BoltDB) 32 | source/native inference | A | ローカルストレージのWeb化 | マウントパスの書き込み制限 34 | ホスト内特定フォルダを即座にブラウザ上でエクスプローラ化 32 |
| 14 | FlareSolverr/FlareSolverr | Self-hosted mini apps | Cloudflareセキュリティ自動突破プロキシ | OCI-first | Single Process | explicit capsule.toml | A | 開発ユーティリティの補完 | ヘッドレスブラウザの消費 | バックエンド自動化処理をAto経由で裏起動する実用デモに最適 |
| 15 | promptfoo/promptfoo | AI / agent tools | LLMプロンプト・モデル評価CLI/UI | Native-first | Single Process | source/native inference | A | AI評価自動化の可視化 | 特になし | npx promptfoo viewによりローカルで即座にUIポートを起動 |
| 16 | nocodb/nocodb 36 | Data / internal tools | OSS版Airtableデータベースハイブリッド 36 | OCI-first | Single Process 37 | \--oci-compose 37 | C | リッチな表計算UI 36 | 2026年ライセンス制限変更 39 | ライセンス変更に伴う再配布・商用制限リスクが高いため注視 39 |
| 17 | dbgate/dbgate 40 | Developer tools | 汎用SQL/NoSQLクライアントUI 40 | OCI-first | Single Process | explicit capsule.toml | A | クロスDB操作デモ 40 | 特になし 40 | 依存性の低い単一のWeb UIパッケージとして極めて高速に起動 |
| 18 | n8n-io/n8n | AI / agent tools | ビジュアルワークフロー自動化ツール | OCI-first | Single Process (SQLite) | explicit capsule.toml | B | 業務自動化プロトタイプ | ライセンス要件注意 | SQLiteデフォルト起動により単一コンテナで十分なデモが可能 |
| 19 | zadam/trilium | Personal productivity | 階層構造・開発者向け多機能Wiki | OCI-first | Single Process (SQLite) | explicit capsule.toml | B | 巨大ナレッジベース構築 | なし | SQLite一体型で、ポートマッピングとデータマウントのみで完結 |
| 20 | appsmithorg/appsmith | Data / internal tools | ローコードUI・内部ツールビルダー | OCI-first | Multi-service (Embedded MongoDB) | explicit capsule.toml | C | 内部ツール即時構築 | コンテナイメージの肥大化 | 単一コンテナ内にJava, Node, DBが同居し、初回起動が極めて重い |
| 21 | ToolJet/ToolJet | Data / internal tools | OSS製内部ツール開発プラットフォーム | OCI-first | app \+ postgres | \--oci-compose | B | UI構築デモ | 環境変数の初期設定 | Composeグラフの自動解決が必要、DBマイグレーションの待機制御 |
| 22 | excalidraw/excalidraw | Self-hosted mini apps | 手書き風仮想ホワイトボードツール | OCI-first | Single Process (Stateless) | explicit capsule.toml | A | インタラクティブ描画 | なし | 完全ステートレスかつ単一ポートの静的配信で動くため瞬時起動 |
| 23 | stonith404/pingvin-share | Self-hosted mini apps | 軽量自己完結型ファイル共有Webアプリ | OCI-first | Single Process | explicit capsule.toml | A | 1回限り共有リンク生成 | ディスククォータ制御 | データディレクトリのマウントとポート公開だけで即時動作可能 |
| 24 | budibase/budibase | Data / internal tools | エンタープライズ向けローコードプラットフォーム | OCI-first | Multi-service | \--oci-compose | C | ローコードビルダー | 3種類以上の内包サービス | CouchDBやRedisを含み、Ato初期デモとしては接続トポロジが重い |
| 25 | metabase/metabase | Developer tools | OSS製データ可視化・BIダッシュボード | Native-first / OCI | Single Process (H2DB) | explicit capsule.toml | B | リッチなグラフUI | Javaランタイム要求 | H2DBを内蔵した単一のJARとして動かすデモで、Atoの万能性を証明 |
| 26 | Kareadita/Kavita | Self-hosted mini apps | 電子書籍・コミック・PDFリーダー | OCI-first | Single Process | explicit capsule.toml | B | 本棚UIビューア | ファイルスキャン時のCPU | .NET製バイナリで動くため、ファイルバインドとの相性が良好 |
| 27 | wallabag/wallabag | Personal productivity | 自己ホスト型Read-it-laterツール | OCI-first | Single Process (SQLite) | explicit capsule.toml | B | 広告除去テキスト閲覧 | PHP-FPMとNginx調整 | SQLite駆動コンテナでの立ち上げにより、PHP依存性を一切意識させない |
| 28 | vrana/adminer | Developer tools | 単一PHPファイル軽量DB管理UI | Native-first | Single Process (PHP) | source/native inference | A | 驚異的な軽量DB接続 | なし | わずか数百KBのPHPファイルをAto組み込みPHP-CLIサーバーで起動可能 |
| 29 | amir20/dozzle | Developer tools | Dockerコンテナログリアルタイム監視UI | OCI-first | Single Process | manual recipe required | D | 特になし | Docker.sockのマウント必須 | **【初期対象外】** ホストの/var/run/docker.sockへのアクセスを要求 |
| 30 | portainer/portainer | Developer tools | Dockerコンテナ・スタック統合管理UI | OCI-first | Single Process | manual recipe required | D | 特になし | Docker.sockマウント必須 | **【初期対象外】** コンテナランタイムを操作する特権が必要でAto制限違反 |

## **優先選定上位10レポジトリの技術的徹底プロファイル**

### **1\. usememos/memos**

#### **システム構成および基本パラメータ**

| 項目 | 収集事実および推定スペック |
| :---- | :---- |
| **Repository URL** | https://github.com/usememos/memos 1 |
| **Star 数** | 59.9k 1 |
| **最終更新日 / バージョン** | 2026年4月27日 (v0.28.0) 2 / 1週間前にアクティブなコミットあり 1 |
| **License** | MIT License 1 |
| **主な言語 / framework** | Go 55.4% (バックエンド), TypeScript 43.9% (React / Tailwind) 1 |
| **アプリの用途** | タイムライン型パーソナル・ナレッジベース 1 |
| **起動方式** | Mixed (Goバイナリ Native 2 / Docker CLI 1 / Docker Compose 2) |
| **必要 service** | Single Process (SQLiteデフォルト) 1 |
| **必要 env / secrets** | オプション設定のみ (PORT等、接続DB変更時の DB\_TYPE 指定) 3 |
| **公開 port** | 5230 1 |
| **readiness の取りやすさ** | / へのHTTP GETによるステータス200検知 1、またはAPIエンドポイントへの疎通確認 |
| **persistent state の場所** | /var/opt/memos (SQLiteデータベース memos\_prod.db が自動格納される) 1 |
| **arm64 対応見込み** | 確定 (Goネイティブのためクロスコンパイル可能、公式OCIイメージもマルチアーキ対応) 1 |
| **Ato recipe 難易度** | A (すぐ作れる、設定変更なしで稼働) 1 |
| **Ato でのデモ価値** | 極めて高い (数秒で起動し、即座にポータブルなナレッジベースをローカルに構築) 1 |
| **主なリスク** | ホスト側のデータマウント先フォルダにおけるパーミッション制限 3 |
| **推奨 recipe path** | explicit capsule.toml |

#### **技術的解剖およびAtoレシピとしての適合分析**

MemosがAtoランタイムのファーストレシピとして最も優れている理由は、その圧倒的な軽量性とゼロ構成（ゼロコンフィギュレーション）設計に帰結する 1。わずか20MB程度のコンテナイメージであり、内部プロセスとして単一のGoコンパイル済みバイナリが実行され、データの保存先として複雑なRDBMSを設定することなく、組み込みのSQLiteが即時プロビジョニングされる 1。これはAtoの「起動速度」と「ローカルPCを汚さない自動永続化」を実証する上で完璧なショーケースとなる。  
想定されるレシピのターゲット構造は、ホスト環境にNodeやGoのビルドチェーンが存在しなくとも即座にコンテナレイヤーがホストのパーミッションとバインドできる構成をとる。  
Atoが解決すべき変数設定として、ユーザー環境ごとのポート重複への対処が挙げられる。デフォルトの 5230 ポートが塞がっている際、Atoランタイム側で環境変数を自動調整してホスト側ポートを安全にスライドさせる機構が有効に機能する 1。

#### **実行に必要な最小設定**

Ini, TOML  
\[capsule\]  
name \= "memos"  
version \= "0.28.0"  
type \= "oci"

\[target.oci\]  
image \= "neosmemo/memos:stable"

\[ports\]  
internal \= 5230  
default\_host \= 5230

\[volumes\]  
"/var/opt/memos" \= "data"

#### **デモ・検証における実践シナリオ**

デモのシナリオとして、開発者がローカル環境で「ブラウザを離れることなく、即座にメモやコードの断片をタイムラインに投げ込む」ケースを想定する 1。ato run memos を叩いた直後、コンソールに表示されたlocalhostのポートを開くと、ログイン不要でその場でタイムライン型UIが展開される 1。Atoを停止させ、ホストのボリュームフォルダ内にSQLiteファイル（memos\_prod.db）が生成されていること、そして再起動後にメモの内容が完全に引き継がれていることを示すことで、永続化レイヤーの抽象化が証明される 3。  
本構成において、失敗を招く唯一のボトルネックは、ホスト側の自動バインドディレクトリにおける書き込み権限の欠落である 3。これを事前に検証するため、Atoの初期レシピ作成者は、ホスト側の一時フォルダにアクセス権限を付与して疎通させるコマンドを最初に叩き、プロトタイプの検証を完了させる。

Bash  
mkdir \-p /tmp/memos\_ato\_test && chmod 777 /tmp/memos\_ato\_test  
docker run \-d \--name memos\_test \-p 5230:5230 \-v /tmp/memos\_ato\_test:/var/opt/memos neosmemo/memos:stable

Memosのレシピ作成優先度は「S」である。Atoの起動ロジックに一切の依存がなく、最初に構築してランタイムそのものの結合テストの指標とするのに最も適しているからである 1。

### **2\. blinkospace/blinko**

#### **システム構成および基本パラメータ**

| 項目 | 収集事実および推定スペック |
| :---- | :---- |
| **Repository URL** | https://github.com/blinkospace/blinko 4 |
| **Star 数** | 10.4k 4 |
| **最終更新日 / バージョン** | 2026年4月11日 (Blinko v1.8.7) 4 |
| **License** | GPL-3.0 License 4 |
| **主な言語 / framework** | TypeScript 92.0%, Rust 2.9% (Next.js / Tailwind / Prisma) 4 |
| **アプリの用途** | 個人用ローカルAI搭載・RAGナレッジ管理ツール 4 |
| **起動方式** | Mixed (Docker Compose / OCI 優先) 5 |
| **必要 service** | app \+ postgres (PostgreSQL 14以降が必要) 5 |
| **必要 env / secrets** | NEXTAUTH\_SECRET, DATABASE\_URL, NODE\_ENV=production 5 |
| **公開 port** | 1111 6 |
| **readiness の取りやすさ** | curl \-f http://localhost:1111/ によるHTTPレスポンス取得 6 |
| **persistent state の場所** | /app/.blinko (アプリ固有データ) 6, PostgreSQL物理ボリューム領域 |
| **arm64 対応見込み** | 高い (Node/TypeScriptベースであり、主要イメージはマルチ対応可能) |
| **Ato recipe 難易度** | B (少し調整すれば作れる。マルチコンテナ連携とシークレット生成が必要) 5 |
| **Ato でのデモ価値** | 圧倒的に高い (ローカル完結型のRAGが即動く、デザインが美麗) 4 |
| **主なリスク** | 公式Composeが host ネットワークをデフォルトとしており、ポート衝突のリスクあり 6 |
| **推奨 recipe path** | \--oci-compose 6 |

#### **技術的解剖およびAtoレシピとしての適合分析**

Blinkoは、ローカル完結型のAI・RAG（検索生成拡張）カードメモツールという、現代の最も強力なトレンドを捉えたアプリケーションである 4。Next.jsをベースとした洗練されたUIを誇り、個人データの完全な所有を可能にする自己ホスト設計が特徴である 4。AtoランタイムでBlinkoを起動する価値は、通常であれば「PostgreSQLデータベースの準備」「Prismaスキーマの初期化」「NextAuth暗号化トークンの設定」という、開発者以外のユーザーには理解困難な複数のステップを、Atoが完全にオートメーションで隠蔽・隠滅できる点にある 5。  
想定レシピでは、Ato独自の「シークレット自動プロビジョニング機能」を使用して NEXTAUTH\_SECRET をランダム生成し、さらにPostgreSQLサービスとアプリサービスが自動的に連携するサービスグラフ（Service graph）を定義する 5。  
最大のリスクは、Blinkoが公式の生産用Composeファイル（docker-compose.prod.yml）内で driver: host（ホストネットワーク）を使用している点にある 6。これをそのままAtoで実行すると、ホスト上の他のポートと予期せぬ競合を起こし、さらにネットワークの分離性が損なわれる 6。そのため、Atoレシピ側ではこれをBridgeネットワークへリライトし、Atoランタイム経由で安全にポート 1111 をホストへ公開するアプローチ（カプセル化）を採る 5。

#### **実行に必要な最小設定**

YAML  
version: '3.8'  
services:  
  db:  
    image: postgres:14-alpine  
    environment:  
      POSTGRES\_DB: postgres  
      POSTGRES\_USER: postgres  
      POSTGRES\_PASSWORD: ${AUTO\_GEN\_POSTGRES\_PASS}  
    volumes:  
      \- pgdata:/var/lib/postgresql/data  
  app:  
    image: blinkospace/blinko:latest  
    depends\_on:  
      \- db  
    ports:  
      \- "1111:1111"  
    environment:  
      NODE\_ENV: production  
      NEXTAUTH\_SECRET: ${AUTO\_GEN\_NEXTAUTH\_SECRET}  
      DATABASE\_URL: postgresql://postgres:${AUTO\_GEN\_POSTGRES\_PASS}@db:5432/postgres  
    volumes:  
      \- blinko\_data:/app/.blinko  
volumes:  
  pgdata:  
  blinko\_data:

#### **デモ・検証における実践シナリオ**

デモの文脈として、ユーザーが「AIで自分の頭脳の第二のメモリ（Fleeting thoughts）を作りたい」と望む場面をシミュレートする 4。ato run blinko コマンドだけで、データベースの構築・自動マイグレーションが行われ、ブラウザで即時アカウント作成が可能になる 5。起動後、Ollama連携機能を利用してホスト側のローカルLLMと接続し、入力したメモをAIが自動でタグ付けしたり要約したりする挙動を見せることで、ローカルAIスタックの極めて強力なインタープリターとしてのAtoの価値が示される 4。  
検証のためにチームが最初に叩くべきコマンドは、デフォルトのホストネットワーク指定をブリッジに剥がした状態でのAppとDBの分離実行テストである。

Bash  
docker network create blinko\_test\_net  
docker run \-d \--name blinko-db \--network blinko\_test\_net \-e POSTGRES\_DB=postgres \-e POSTGRES\_USER=postgres \-e POSTGRES\_PASSWORD=mysecretpassword postgres:14-alpine  
docker run \-d \--name blinko-app \--network blinko\_test\_net \-p 1111:1111 \-e NODE\_ENV=production \-e NEXTAUTH\_SECRET=my\_ultra\_secure\_nextauth\_secret \-e DATABASE\_URL=postgresql://postgres:mysecretpassword@blinko-db:5432/postgres blinkospace/blinko:latest

Blinkoの優先度は「S」である。RAGやローカルAIをテーマにした現代的なデモとして、本プロジェクトは絶大な訴求力を持つからである 4。

### **3\. sosedoff/pgweb**

#### **システム構成および基本パラメータ**

| 項目 | 収集事実および推定スペック |
| :---- | :---- |
| **Repository URL** | https://github.com/sosedoff/pgweb 7 |
| **Star 数** | 9.4k 7 |
| **最終更新日 / バージョン** | 2026年2月27日 (v0.16.19) 8 |
| **License** | MIT License 7 |
| **主な言語 / framework** | Go 54.7%, JavaScript 17.8% 7 |
| **アプリの用途** | Go製 PostgreSQL 用の超軽量Webデータベースエクスプローラー 7 |
| **起動方式** | Mixed (Native Go バイナリ配布 7 / OCIコンテナ 9) |
| **必要 service** | Single Process (データベース接続用の親アプリは不要で本体のみ起動) 7 |
| **必要 env / secrets** | PGWEB\_SESSIONS=1 (複数のデータベース接続画面を許可する場合) 7 |
| **公開 port** | 8081 9 |
| **readiness の取りやすさ** | / へのHTTP GETによるステータスコード200の高速レスポンス 7 |
| **persistent state の場所** | なし (完全なステートレス設計、お気に入りブックマークの保存はオプション) 7 |
| **arm64 対応見込み** | 確定 (Goコンパイルによる、Mac arm64、Linux arm64バイナリ配布あり) 7 |
| **Ato recipe 難易度** | A (すぐ作れる、環境依存性が一切なく軽量) 7 |
| **Ato でのデモ価値** | 高い (開発ツールカテゴリにおいて、開発者に馴染みのある日常の即時Web化ツール) 7 |
| **主なリスク** | ホストマシンの localhost データベースに接続する際のコンテナNW境界問題 7 |
| **推奨 recipe path** | source/native inference 9 |

#### **技術的解剖およびAtoレシピとしての適合分析**

Pgwebは、一切の動的ライブラリ依存を持たないGo言語による超高速データベースクライアントツールである 7。このプロファイルは、Atoの「Nativeアプリケーションのインファレンス（自動検出）および即時バイナリ駆動」を極限までシンプルに実証する 7。メモリ占有率がわずか数MBであり、ローカルまたはリモートのPostgreSQLサーバーにSSHトンネル等を介して安全に接続できるため、開発者に極めて愛されている 7。  
想定レシピ構造は、AtoがホストOSのアーキテクチャ（amd64またはarm64）を検出し、最適なネイティブバイナリをGitHubリリースから自動フェッチして実行するパスをとる 7。コンテナを介さないため、ホスト上のPostgreSQLインスタンス（127.0.0.1:5432）に対しても「コンテナ内から接続できない」といったネットワークの境界障害が一切発生せず、シームレスに機能する。  
もしコンテナ版を実行する場合は、Atoが自動的にホストアドレスを host.docker.internal にマッピングして引き渡す。また、環境変数 PGWEB\_SESSIONS=1 を指定して起動することで、初期設定ファイルを用意せずとも、ユーザーがブラウザ上で任意のPostgreSQL接続文字列を自由に入力して利用可能にする 7。

#### **実行に必要な最小設定**

Ini, TOML  
\[capsule\]  
name \= "pgweb"  
version \= "0.16.19"  
type \= "native"

\[target.native\]  
exec \= "pgweb"  
args \= \["--sessions", "--bind=0.0.0.0", "--port=8081"\]

\[ports\]  
internal \= 8081  
default\_host \= 8081

#### **デモ・検証における実践シナリオ**

デモの文脈として、開発チーム内での「一時的なデータベース確認作業」を設定する 7。高機能だが起動が重い専用アプリ（DBeaverやpgAdminなど）をローカルにインストールすることなく 9、ato run pgweb を叩くだけで一瞬でブラウザベースのSQLエクスプローラが立ち上がる様子を示す 7。  
接続先を指定し、ブラウザ上でEXPLAINクエリを実行してSQLのボトルネックを高速に見抜くまでのフリクション（無駄な時間）がゼロになる体験を可視化する 7。検証のために最初に叩くべきコマンドは、マルチセッションモードを指定したコンテナ起動テストである 7。

Bash  
docker run \-d \--name pgweb\_test \-p 8081:8081 \-e PGWEB\_SESSIONS=1 sosedoff/pgweb:latest

pgwebの優先度は「A」である。Native実行モデルにおけるもっともシンプルで実用的なテストベッドとなるからである 7。

### **4\. axllent/mailpit**

#### **システム構成および基本パラメータ**

| 項目 | 収集事実および推定スペック |
| :---- | :---- |
| **Repository URL** | https://github.com/axllent/mailpit 10 |
| **Star 数** | 9.3k 43 |
| **最終更新日 / バージョン** | 2026年4月15日 (v1.29.7) 44 |
| **License** | MIT License 10 |
| **主な言語 / framework** | Go 62.7%, Vue 27.5% 10 |
| **アプリの用途** | 開発者向けSMTPメール配信キャプチャテストツール＆API 10 |
| **起動方式** | Mixed (Goシングルスタティックバイナリ 10 / OCIコンテナ 10) |
| **必要 service** | Single Process 10 |
| **必要 env / secrets** | オプション設定のみ (MP\_DATABASE等、永続化時のパス定義) 46 |
| **公開 port** | 8025 (Web UI), 1025 (SMTPポート) 10 |
| **readiness の取りやすさ** | / へのHTTP GETによる200 OKレスポンス 46 |
| **persistent state の場所** | オプション指定、SQLiteファイル /data/mailpit.db 46 |
| **arm64 対応見込み** | 確定 (darwin-arm64, linux-arm64用のビルド済みバイナリが完備) 44 |
| **Ato recipe 難易度** | A (すぐ作れる、設定変数が極めてシンプル) 10 |
| **Ato でのデモ価値** | 極めて高い (メール検証インフラを1コマンドで用意、Websockets即時同期) 10 |
| **主なリスク** | SMTPポート（1025）やWeb UIポート（8025）のポート競合 10 |
| **推奨 recipe path** | explicit capsule.toml 10 |

#### **技術的解剖およびAtoレシピとしての適合分析**

Mailpitは、ローカル開発環境における「送信テストメールをすべてインターセプトしてWeb画面に格納する」ための無敵の開発者向けダミーSMTPサーバーである 10。現在メンテナンスされていないMailhogからの完璧な移行先として確固たる地位を築いており 10、HTML互換性検査器や、Websocketsによる新規メッセージ受信時の即時UI同期 10、そして高度なREST APIテスト機能を備える 10。  
Atoレシピにおいて、Mailpitは最も完璧なインフラ・レシピの役割を担う。最大の技術的特徴は、SMTPを受け取るTCPポート 1025 と、管理UIを提供するHTTPポート 8025 の「デュアルポートバインド」を単一プロセスで公開する点にある 10。  
Atoのレシピ設計では、この2つの異なる種類のポート（SMTP / HTTP）を明示的にマッピングしつつ、ローカル開発中のアプリ（PHP, Ruby, Node等）のSMTP設定を容易にするため、マシンのlocalhost上に透過的に投影する。データ格納用のSQLiteデータベースパス（MP\_DATABASE）を指定しボリュームマウントすることで、再起動時にも検証したメール履歴を完全に残すように設定可能である 10。

#### **実行に必要な最小設定**

Ini, TOML  
\[capsule\]  
name \= "mailpit"  
version \= "1.29.7"  
type \= "oci"

\[target.oci\]  
image \= "axllent/mailpit:latest"

\[ports\]  
"8025:8025" \= "http"  
"1025:1025" \= "smtp"

\[env\]  
MP\_DATABASE \= "/data/mailpit.db"  
MP\_MAX\_MESSAGES \= "5000"

\[volumes\]  
"/data" \= "mail\_data"

#### **デモ・検証における実践シナリオ**

デモの文脈として、ローカルのRailsやLaravel、Djangoアプリケーションから「メールを送信し、本物の配信経路を汚さずに送信結果を確認する」シナリオを設定する 12。ato run mailpit をバックグラウンドで走らせ、アプリ側のメール送信先を localhost:1025 に変更して送信処理を叩くと、MailpitのWebUI（http://localhost:8025）上にリアルタイムにHTMLメールが流れてくる様子を見せつける 10。  
これにより、複雑なメール配送インフラのローカル・スタブ構築において、Atoが完璧なインフラプロバイダーになることを証明する。最初に叩くべき検証コマンドは、ボリュームマウントを含めたコンテナ起動テストである。

Bash  
mkdir \-p /tmp/mailpit\_data && chmod 777 /tmp/mailpit\_data  
docker run \-d \--name mailpit\_test \-p 8025:8025 \-p 1025:1025 \-v /tmp/mailpit\_data:/data \-e MP\_DATABASE=/data/mailpit.db axllent/mailpit:latest

Mailpitの優先度は「S」である。すべてのウェブ開発者が手元に欲しがるツールであり、Atoの複数ポートバインディングの実力を見せるのに最適だからである 12。

### **5\. linkwarden/linkwarden**

#### **システム構成および基本パラメータ**

| 項目 | 収集事実および推定スペック |
| :---- | :---- |
| **Repository URL** | https://github.com/linkwarden/linkwarden 13 |
| **Star 数** | 18.4k 13 |
| **最終更新日 / バージョン** | 2026年4月30日 (v2.14.1) 50 |
| **License** | AGPL-3.0 License 13 |
| **主な言語 / framework** | TypeScript 91.8% (Next.js / Prisma / Puppeteer) 13 |
| **アプリの用途** | 共同作業型・Webアーカイブ自動保存搭載ブックマークマネージャー 13 |
| **起動方式** | Mixed (Docker Compose によるマルチコンテナ起動優先) 14 |
| **必要 service** | app \+ postgres 14 |
| **必要 env / secrets** | NEXTAUTH\_SECRET, DATABASE\_URL 14 |
| **公開 port** | 3000 14 |
| **readiness の取りやすさ** | ポート 3000 へのHTTP GET 14、またはNext.jsのヘルス疎通 |
| **persistent state の場所** | /data/data (PDF、スクリーンショット等の物理アセット保存用) 14 |
| **arm64 対応見込み** | 高い (Next.jsベースかつ、コンテナイメージは主要プラットフォーム対応) 50 |
| **Ato recipe 難易度** | B (少し調整すれば作れる。マルチコンテナ協調およびストレージ永続化の設定) 14 |
| **Ato でのデモ価値** | 極めて高い (ブックマーク登録後、一瞬で魚拓アーカイブがPDF化される魔法) 13 |
| **主なリスク** | Puppeteerによるバックグラウンドブラウザ起動時のメモリ・CPUスパイク |
| **推奨 recipe path** | \--oci-compose 14 |

#### **技術的解剖およびAtoレシピとしての適合分析**

Linkwardenは、単なるブックマークマネージャーの枠を超え、登録したWebサイトのPDF、スクリーンショット、および純粋なHTML魚拓を、内部で Puppeteer（ヘッドレスChromium）を回してローカルに完全保存（魚拓化）する堅牢な情報蓄積プラットフォームである 13。AtoでLinkwardenを起動する価値は、Puppeteerの動作に必要な膨大なホスト側依存ライブラリをNext.jsコンテナ内にすべて封じ込め、さらにデータのマウントパス（/data/data）とPostgreSQLの接続を完璧にカプセル化して一発起動できる点にある 13。  
想定レシピ構造は、Atoの \--oci-compose 機能を活用し、Next.jsのアプリサーバーとPostgreSQLサーバーを同一ネットワーク上でリンクさせる形式をとる 14。  
ここでのリスク要因は、Puppeteerがページキャプチャを行う際に、コンテナがセキュリティ制限（No-Sandboxオプションなど）に引っかかり、レンダリングエラーを起こす点である。Atoランタイム側では、レシピ生成時にコンテナの環境変数や実行時のキャップ調整を行い、ホストに過度な特権（Privileged）を与えずにヘッドレスブラウザが安全に描画できるように制限を定義する。

#### **実行に必要な最小設定**

YAML  
version: '3.8'  
services:  
  db:  
    image: postgres:16-alpine  
    environment:  
      POSTGRES\_DB: linkwarden  
      POSTGRES\_USER: linkwarden  
      POSTGRES\_PASSWORD: ${AUTO\_GEN\_DB\_PASS}  
    volumes:  
      \- pg\_data:/var/lib/postgresql/data  
  app:  
    image: ghcr.io/linkwarden/linkwarden:latest  
    depends\_on:  
      \- db  
    ports:  
      \- "3000:3000"  
    environment:  
      DATABASE\_URL: postgresql://linkwarden:${AUTO\_GEN\_DB\_PASS}@db:5432/linkwarden  
      NEXTAUTH\_SECRET: ${AUTO\_GEN\_JWT\_SECRET}  
    volumes:  
      \- storage\_data:/data/data  
volumes:  
  pg\_data:  
  storage\_data:

#### **デモ・検証における実践シナリオ**

デモの文脈として、調査や研究作業において「重要な参考URLをスクラップするが、後でサイトが消えても読み返せる（Link Rot防止）安心感」を実演する 13。ato run linkwarden を起動し、ダッシュボードから適当な技術系ニュースサイトのURLを保存する 13。  
わずか数秒で、そのページの高精細なPDFとPNGキャプチャ、および広告が除去されたクリーンな「リーダー表示」が生成され、マウントされたフォルダに保存される様子を見せる 13。最初に動作検証を行うコマンドは、Composeに落とし込む前の環境疎通テストである 14。

Bash  
docker run \-d \--name link\_db \-e POSTGRES\_DB=linkwarden \-e POSTGRES\_USER=linkwarden \-e POSTGRES\_PASSWORD=testpass postgres:16-alpine  
docker run \-d \--name link\_app \-p 3000:3000 \-e DATABASE\_URL=postgresql://linkwarden:testpass@link\_db:5432/linkwarden \-e NEXTAUTH\_SECRET=supersecret123 ghcr.io/linkwarden/linkwarden:latest

Linkwardenの優先度は「S」である。Next.js 15などのモダンWeb技術が詰まっており 52、Atoの「複雑な協調型マルチコンテナ」を実証する最強のデモプロジェクトだからである 14。

### **6\. open-webui/open-webui**

#### **システム構成および基本パラメータ**

| 項目 | 収集事実および推定スペック |
| :---- | :---- |
| **Repository URL** | https://github.com/open-webui/open-webui 16 |
| **Star 数** | 138k 17 |
| **最終更新日 / バージョン** | 2026年4月 17 |
| **License** | MIT License 17 |
| **主な言語 / framework** | Python 97% (FastAPI / Svelte / PyTorch統合) 17 |
| **アプリの用途** | 完全ローカル・オフライン対応の超高機能AIチャットプラットフォーム 15 |
| **起動方式** | Mixed (Docker CLI 15 / Python pip/uv 15 / Desktopアプリ 15) |
| **必要 service** | Single Process (単体でSQLiteを内蔵し動作可能、バックエンドモデルは外部連携) 15 |
| **必要 env / secrets** | OLLAMA\_BASE\_URL, WEBUI\_AUTH=false (個人利用での強制認証解除) 18 |
| **公開 port** | 3000 (ホストポート、コンテナ内の 8080 を受ける) 15 |
| **readiness の取りやすさ** | /health エンドポイントへのGETリクエスト（HTTP 200を安定返却） |
| **persistent state の場所** | /app/backend/data (チャットログ、RAG用ドキュメント等の格納) 15 |
| **arm64 対応見込み** | 高い (Pythonスタックであり、公式がメインブランチイメージをarm64向けに配信) |
| **Ato recipe 難易度** | B (少し調整すれば作れる。ホストゲートウェイへの通信バインド定義が必要) 15 |
| **Ato でのデモ価値** | 圧倒的に最高 (ChatGPTに劣らないローカルUIが一瞬で立ち上がる驚愕) 16 |
| **主なリスク** | ホスト側のOllamaサーバーをコンテナ内から見失うネットワーク分離障害 18 |
| **推奨 recipe path** | explicit capsule.toml 15 |

#### **技術的解剖およびAtoレシピとしての適合分析**

Open WebUIは、GitHubで13万スターを超える世界で最も熱狂的に開発されているローカルAIインターフェースである 17。OllamaやOpenAI互換APIと完璧に連携し、RAG（文書アップロードチャット）やエージェント機能、音声・ビデオ通話までを完全ローカルで動かすことができる 15。Atoレシピとしての適合性が非常に高い理由は、OllamaなどのバックエンドAIサーバーをホスト側に置きつつ、UIフロントエンドだけをAtoの軽量サンドボックス内で動かす「分離共生型AI環境」を完璧に構築できるからである 15。  
想定レシピ構造は、ホスト側にすでにインストールされているOllama（ポート 11434）を自動検出し、コンテナ実行時にホストと接続するためのブリッジを提供する 18。  
ここで不可欠なAto側の機能は、extra\_hosts による host.docker.internal:host-gateway の注入である 15。LinuxやmacOSなどのホストOSの種類に応じて自動的にこのネットワーク・エイリアスを解決してコンテナ内に引き渡すことで、ユーザーはコンテナネットワークの設定に迷うことなく、一瞬で手元のモデルがドロップダウンリストに表示される体験を得られる 18。

#### **実行に必要な最小設定**

Ini, TOML  
\[capsule\]  
name \= "open-webui"  
version \= "latest"  
type \= "oci"

\[target.oci\]  
image \= "ghcr.io/open-webui/open-webui:main"  
extra\_hosts \= \["host.docker.internal:host-gateway"\]

\[ports\]  
"3000:8080" \= "http"

\[env\]  
OLLAMA\_BASE\_URL \= "http://host.docker.internal:11434"  
WEBUI\_AUTH \= "false"

\[volumes\]  
"/app/backend/data" \= "data"

#### **デモ・検証における実践シナリオ**

デモのシナリオとして、ローカルPCにモデルは落としてあるが、CLI（ターミナル）での対話に疲れた開発者が、一瞬で「ChatGPTを自社サーバーでホストする」かのような体験を手にするケースを描く 16。ato run open-webui を実行し、http://localhost:3000 を開く 15。  
Atoがホストネットワーク内のOllamaを自動ブリッジするため、起動した瞬間から、手元にあるモデル（Llama 3など）がドロップダウンから選択可能な状態になり、そのまま高度なマルチモーダルチャットやドキュメントRAG対話が開始される 16。最初に検証すべきコマンドは以下の通りである 15。

Bash  
docker run \-d \-p 3000:8080 \--add-host=host.docker.internal:host-gateway \-v open\_webui\_test:/app/backend/data \--name open\_webui\_test ghcr.io/open-webui/open-webui:main

Open WebUIの優先度は「S」である。現在のテック業界で最もウケが良いキラーデモとなり、Atoの実力を即座に信じ込ませる力があるからである 17。

### **7\. go-vikunja/vikunja**

#### **システム構成および基本パラメータ**

| 項目 | 収集事実および推定スペック |
| :---- | :---- |
| **Repository URL** | https://github.com/go-vikunja/vikunja 19 |
| **Star 数** | 4.3k 19 |
| **最終更新日 / バージョン** | 2026年5月 19 |
| **License** | AGPL-3.0 License 19 |
| **主な言語 / framework** | Go 67.5% (バックエンド), TypeScript/Vue 15.6% (フロントエンド) 19 |
| **アプリの用途** | カンバン、ガントチャート、タスクリストを統合した統合生産性向上システム 20 |
| **起動方式** | Mixed (単一コンテナ 22 / Docker Compose 23 / システムパッケージバイナリ 22) |
| **必要 service** | Single Process (SQLiteモード) 22 / app \+ postgres (プロダクション推奨) 23 |
| **必要 env / secrets** | VIKUNJA\_SERVICE\_SECRET (セッショントークン署名用のJWTキー、生成必須) 23 |
| **公開 port** | 3456 21 |
| **readiness の取りやすさ** | ポート 3456 へのHTTP GET、または /api/v1/info で詳細ヘルスステータス取得 |
| **persistent state の場所** | /db (SQLiteファイル格納場所) 22, /app/vikunja/files (添付ファイル) 22 |
| **arm64 対応見込み** | 確定 (Goネイティブのためマルチアーキテクチャバイナリおよびイメージを公開) 19 |
| **Ato recipe 難易度** | B (少し調整すれば作れる。JWTシークレット生成とマウントフォルダ権限のバインド) 23 |
| **Ato でのデモ価値** | 高い (これほど高機能な進捗・タスク管理画面が1コマンドで軽快に動き出す感動) 20 |
| **主なリスク** | コンテナ内の非ルートユーザー（UID 1000）によるボリューム書き込み拒否エラー 21 |
| **推奨 recipe path** | explicit capsule.toml 22 |

#### **技術的解剖およびAtoレシピとしての適合分析**

Vikunjaは、フロントエンド（Vue）とバックエンド（Go）が現代的なマイクロモノリスとして「一つの実行可能バイナリおよび単一コンテナ」に完璧に事前パッケージングされた、非常に美しい設計を持つタスク管理システムである 19。Atoにとって完璧な初期レシピ候補となる理由は、通常であれば「フロントとAPIが別ポートで動き、CORSポリシーを調整する」か「リバースプロキシを立ててマージする」という面倒な手続きを完全に不要とし、CORS無効設定もしくは単一ポートバインドのみでSQLite駆動の高速Todoサーバーとして動かせる点にある 22。  
Atoのレシピ側で必ず処理すべき必須設定は、VIKUNJA\_SERVICE\_SECRET の自動暗号鍵生成と、コンテナユーザー（デフォルトUID 1000）のための「永続フォルダの権限調整」である 21。  
ホスト側でマウントされるファイルディレクトリが非ルートユーザーによって書き込み可能でない場合、GoのSQLiteドライバがデータベースファイル（/db/vikunja.db）の作成時にクラッシュする 21。Atoランタイムがマウントフォルダの所有権調整（または適切なアクセス許可付与）を自動的にインターセプトして調整する検証機能があれば、最もスムーズに起動する。

#### **実行に必要な最小設定**

Ini, TOML  
\[capsule\]  
name \= "vikunja"  
version \= "latest"  
type \= "oci"

\[target.oci\]  
image \= "vikunja/vikunja:latest"

\[ports\]  
"3456:3456" \= "http"

\[env\]  
VIKUNJA\_DATABASE\_TYPE \= "sqlite"  
VIKUNJA\_DATABASE\_PATH \= "/db/vikunja.db"  
VIKUNJA\_SERVICE\_SECRET \= "auto\_generated\_jwt\_secret"  
VIKUNJA\_SERVICE\_PUBLICURL \= "http://localhost:3456/"

\[volumes\]  
"/db" \= "db"  
"/app/vikunja/files" \= "files"

#### **デモ・検証における実践シナリオ**

デモの文脈として、個人開発者やチームが「完全にクラウド（SaaS）から独立した、セキュアで高速なカンバン・ガントチャートToDoシステム」を数秒で起動するシナリオを提供する 20。ato run vikunja を実行させ、即座に立ち上がった画面でタスクを作成する 20。  
そのタスクにPDFなどのファイルをドラッグ＆ドロップしてアップロードし、データがローカルの files ボリュームにバイナリ形式で正しく同期されている様子を見せることで、ポータブルなローカルアプリケーションとしての確固たる価値が示される 20。検証のための事前起動テストは以下の通りである 22。

Bash  
mkdir \-p /tmp/vikunja\_db /tmp/vikunja\_files && chmod \-R 777 /tmp/vikunja\_\*  
docker run \-d \--name vikunja\_test \-p 3456:3456 \-v /tmp/vikunja\_db:/db \-v /tmp/vikunja\_files:/app/vikunja/files \-e VIKUNJA\_DATABASE\_TYPE=sqlite \-e VIKUNJA\_DATABASE\_PATH=/db/vikunja.db \-e VIKUNJA\_SERVICE\_SECRET=testkey \-e VIKUNJA\_SERVICE\_PUBLICURL=http://localhost:3456/ vikunja/vikunja:latest

Vikunjaの優先度は「B」である。タスク管理は「日常生活で直近に導入しやすい」ため、Atoを広くホビーユーザーや一般開発者にリーチさせる重要なキラーコンテンツとなるからである 58。

### **8\. go-shiori/shiori**

#### **システム構成および基本パラメータ**

| 項目 | 収集事実および推定スペック |
| :---- | :---- |
| **Repository URL** | https://github.com/go-shiori/shiori 24 |
| **Star 数** | 11.5k 24 |
| **最終更新日 / バージョン** | 2025年9月26日 (v1.8.0) 24 |
| **License** | MIT License 24 |
| **主な言語 / framework** | Go 51.3%, JavaScript 33.2% (Web UI) 24 |
| **アプリの用途** | Go製 Pocket クローンの超高速・軽量ブックマークアーカイブマネージャー 24 |
| **起動方式** | Mixed (Native Go バイナリ 24 / OCIコンテナ 60) |
| **必要 service** | Single Process (組み込みSQLite、またはPostgres/MySQL等にも拡張可能) 24 |
| **必要 env / secrets** | SHIORI\_DIR (データベースおよびアーカイブデータの格納ルートディレクトリ) 61 |
| **公開 port** | 8080 10 |
| **readiness の取りやすさ** | / へのHTTP GETによる即座のステータス200返却 24 |
| **persistent state の場所** | コンテナ内 /shiori (ホストの永続化フォルダへ自動マウント) |
| **arm64 対応見込み** | 確定 (Goコンパイルおよび、ビルドされたバイナリが全CPUプラットフォーム対応) 62 |
| **Ato recipe 難易度** | A (すぐ作れる、極小バイナリ駆動モデル) 24 |
| **Ato でのデモ価値** | 高い (CLIでのブックマーク管理とWebブラウザでのプレビューのシームレスな融合) 24 |
| **主なリスク** | 特になし。極めて軽量で安定しているが、稀にWebコンテンツパース時の高CPU 24 |
| **推奨 recipe path** | source/native inference 24 |

#### **技術的解剖およびAtoレシピとしての適合分析**

Shioriは、Go言語で開発された、徹底的に無駄を削ぎ落としたシンプル・最速のPocket風ブックマーク管理ユーティリティである 24。単一バイナリ形式で動作し、データベースとしてデフォルトでSQLite3が起動するためポータビリティが極めて高い 24。Atoレシピとしての独自性は、「ターミナル上のCLIコマンドとして動く」特性と、「ウェブブラウザ上の美しいダッシュボード画面として動く」特性を、AtoがNativeエンジン上で一切のオーバーヘッドなく切り替えて提供できる点にある 24。  
想定レシピ構造は、Atoの「Native runtime mode」を利用した直接実行である。Goのバイナリパッケージをそのまま実行するか、ghcr.io/go-shiori/shiori:latest のコンテナイメージを使用する 24。  
Atoが提供するコンフィギュレーション抽象化では、永続データフォルダパス（SHIORI\_DIR）をホスト側のディレクトリに安全にバインドさせる 61。ブックマークを追加する際、Shioriは自動的にWebページのリーダービューテキストと「静的Webアーカイブ」を作成するため、このアーカイブアセットが確実にホスト上のNamed Volume等に格納されるよう設計する 24。

#### **実行に必要な最小設定**

Ini, TOML  
\[capsule\]  
name \= "shiori"  
version \= "1.8.0"  
type \= "native"

\[target.native\]  
exec \= "shiori"  
args \= \["serve", "--port", "8080", "--address", "0.0.0.0"\]

\[ports\]  
internal \= 8080  
default\_host \= 8080

#### **デモ・検証における実践シナリオ**

デモの文脈として、開発者が「ブラウザのお気に入りをエクスポートし、手元で爆速検索・完全ローカル魚拓保存する」ライフスタイルをシミュレートする 24。ato run shiori を起動すると、リソース消費がほぼ皆無の状態で即座にポート 8080 にログイン画面が表示される 10。  
デフォルトのアカウント（shiori / gopher）63 でログインした後、任意のウェブ記事のURLを登録すると、一瞬で画像やテキストがパースされ、完全にスタンドアロンなオフライン記事として手元にアーカイブ化される様子を見せる 24。検証用のコンテナ起動確認コマンドは以下の通りである。

Bash  
docker run \-d \--name shiori\_test \-p 8080:8080 \-v /tmp/shiori\_data:/shiori ghcr.io/go-shiori/shiori:latest

Shioriの優先度は「A」である。Memosと並び、データ構造が驚くほどシンプルなため、Ato Nativeプロセスの検証に最適なマイルストーンとなるからである 24。

### **9\. Mintplex-Labs/anything-llm**

#### **システム構成および基本パラメータ**

| 項目 | 収集事実および推定スペック |
| :---- | :---- |
| **Repository URL** | https://github.com/Mintplex-Labs/anything-llm 64 |
| **Star 数** | 60.5k 64 |
| **最終更新日 / バージョン** | 2026年4月22日 (v1.12.1) 64 |
| **License** | MIT License 26 |
| **主な言語 / framework** | JavaScript 100% (NodeJS Express / Vite / React) 25 |
| **アプリの用途** | 個人およびチーム向けの完全プライベート・オールインワンAI＆ローカルRAGシステム 25 |
| **起動方式** | Mixed (Docker OCIコンテナ優先 25 / 各OS用Desktopアプリ 25) |
| **必要 service** | Single Process (内蔵ベクターDB、ドキュメント・コレクター、SQLite) 25 |
| **必要 env / secrets** | DISABLE\_TELEMETRY=true (推奨、テレメトリ追跡のオプトアウト設定) 25 |
| **公開 port** | 3001 66 |
| **readiness の取りやすさ** | / へのHTTP GET、またはExpressバックエンドの起動ポート検知 |
| **persistent state の場所** | /app/server/storage (データベース、ベクター、文書物理データ) 25 |
| **arm64 対応見込み** | 確定 (公式がamd64およびarm64向けのOCIイメージをビルドして配布) 26 |
| **Ato recipe 難易度** | B (少し調整すれば作れる。大容量データマウントとヘルスチェックの遅延調整) 25 |
| **Ato でのデモ価値** | 圧倒的に最高 (PDFやTXTを投げ込めば、その場でオフラインAIが完全回答する衝撃) 25 |
| **主なリスク** | 初回立ち上げ時の内蔵ベクターDB初期化やコレクター構築のヘビーハンドリング 25 |
| **推奨 recipe path** | explicit capsule.toml 25 |

#### **技術的解剖およびAtoレシピとしての適合分析**

AnythingLLMは、PDF、TXT、DOCXなどのあらゆる文書ファイル群をインテリジェントにパース（ベクトル化）し、好みのLLMモデルと接続して完全ローカルで動作する「企業の秘密情報を守るためのAIプライベートチャット・RAGエンジン」である 25。このスタックを動かす最大の技術的障壁は、通常であれば「ベクターデータベース（ChromaやLanceDB等）のプロビジョニング」「テキスト解析用パーサー（コレクター）の起動」「NodeExpressサーバーとReact UIの接続」という、極めて難解な連携が必要となる点だが 25、AnythingLLMはこれらを単一の統合コンテナイメージ内に封じ込めることで、劇的なシンプルさを実現している 25。  
Atoレシピ側での設計において、最も重視すべきパラメータは、ストレージパス（/app/server/storage）の永続化と、プライバシー保護のためのオプトアウト環境変数設定（DISABLE\_TELEMETRY=true）の暗黙注入である 25。  
また、本コンテナは起動時に内部パーサーやベクターエンジンの初期スキャンを行うため、HTTPポートが開放されるまでに通常の軽量Webアプリよりも数十秒のオーバーヘッド（遅延）が発生する。Atoランタイムの「HTTP Readinessチェック」において、タイムアウト設定を十分（例: 60秒）に長く設定して、ユーザーに不完全な起動エラーを知らせないよう制御することが求められる。

#### **実行に必要な最小設定**

Ini, TOML  
\[capsule\]  
name \= "anythingllm"  
version \= "1.12.1"  
type \= "oci"

\[target.oci\]  
image \= "mintplexlabs/anythingllm:latest"

\[ports\]  
"3001:3001" \= "http"

\[env\]  
DISABLE\_TELEMETRY \= "true"

\[volumes\]  
"/app/server/storage" \= "storage"

#### **デモ・検証における実践シナリオ**

デモの文脈として、企業の法務部門や開発チームが「漏洩リスクのあるソースコードやPDF規約ファイルを、一切外部ネットワークに出すことなくAIに解釈させる」ユースケースを提案する 25。ato run anythingllm を実行し、http://localhost:3001 のダッシュボードを開く 66。  
組み込みのローカルベクターDBが即座に起動し 25、ブラウザから仕様書（PDF）をドラッグ＆ドロップして、ローカルで動いているOllama等のモデルを介してRAGによる完全インテリジェント対話が実現する様子を見せる 25。最初に叩く検証コマンドは、マウントフォルダを分離した単一コンテナテストである 25。

Bash  
mkdir \-p /tmp/anythingllm\_storage && chmod 777 /tmp/anythingllm\_storage  
docker run \-d \--name anythingllm\_test \-p 3001:3001 \-v /tmp/anythingllm\_storage:/app/server/storage \-e DISABLE\_TELEMETRY=true mintplexlabs/anythingllm:latest

AnythingLLMの優先度は「S」である。RAGシステムの中でトップクラスのスター数を誇り 64、Atoを導入したその日のうちに企業内セキュリティ基準に準拠したAI検証環境を完璧にデプロイできるからである 26。

### **10\. flowiseai/flowise**

#### **システム構成および基本パラメータ**

| 項目 | 収集事実および推定スペック |
| :---- | :---- |
| **Repository URL** | https://github.com/flowiseai/flowise 27 |
| **Star 数** | 53k 27 |
| **最終更新日 / バージョン** | 2026年4月14日 (v3.1.2) 27 |
| **License** | Apache-2.0 / Custom (一部商用利用に独自定義あり、要確認) 27 |
| **主な言語 / framework** | TypeScript 61.5%, JavaScript 27.5% (NodeJS Monorepo) 27 |
| **アプリの用途** | ローコードによるAIエージェント、LangChain/LlamaIndexフロー設計GUI 27 |
| **起動方式** | Mixed (npm/npxによるNativeグローバル実行 27 / OCIコンテナ 27) |
| **必要 service** | Single Process (組み込みのSQLiteで動作可能) 27 |
| **必要 env / secrets** | 特になし (ポートバインドやログレベル、APIキー定義などのオプションのみ) 27 |
| **公開 port** | 3000 27 |
| **readiness の取りやすさ** | ポート 3000 へのHTTP GETによる、React SPAダッシュボード描画検知 27 |
| **persistent state の場所** | /root/.flowise (フロー定義DB database.sqlite および認証情報格納) |
| **arm64 対応見込み** | 高い (JavaScript/TypeScript製であり、Node公式コンテナ上でマルチ稼働) 27 |
| **Ato recipe 難易度** | B (少し調整すれば作れる。Native実行時のヒープ制限設定、およびフォルダ権限) 27 |
| **Ato でのデモ価値** | 圧倒的に最高 (AIエージェントやボットの接続図をWeb上で組み立てるビジュアル効果) 28 |
| **主なリスク** | モノレポ構造のNodeJSプロセスのため、ビルド時や起動時のメモリ不足クラッシュ 27 |
| **推奨 recipe path** | source/native inference 27 |

#### **技術的解剖およびAtoレシピとしての適合分析**

Flowiseは、複雑な LangChain、LlamaIndex などのAIツールチェインスタックを、Figmaのような完全に直感的なビジュアル・ドラッグ＆ドロップインターフェースで組み立て、ワンクリックで実稼働APIエンドポイントとしてホストできるAI開発者向けの圧倒的なローコードプラットフォームである 27。通常、npmパッケージのグローバルインストール（npm install \-g flowise）27 は、ホスト側のNodeバージョンやグローバル権限、プリコンパイルパッケージの不一致により、驚くほど高い確率で起動時にエラー（JavaScript heap out of memory等）を発生させる 27。  
AtoがFlowiseのNativeおよびOCI実行レシピを提供することで、Nodeランタイムのバージョン不整合を完璧に排除し、あらゆるホスト上で動作を即座に保証する 27。  
Nativeインファレンスにおいては、Atoがホスト側のNode環境（ポート競合、メモリ上限）を自動で調整し、内部引数に \--max-old-space-size=4096 などを注入してモノレポ構造の実行プロセスを徹底的に安定させる 27。永続データ（/root/.flowise）は、Atoのボリュームマップ技術で開発者のホームディレクトリ配下に自動マウントされ、設計したフローダイアグラム（database.sqlite）が再起動後も完全に残るようにする 27。

#### **実行に必要な最小設定**

Ini, TOML  
\[capsule\]  
name \= "flowise"  
version \= "3.1.2"  
type \= "oci"

\[target.oci\]  
image \= "flowiseai/flowise:latest"

\[ports\]  
"3000:3000" \= "http"

\[volumes\]  
"/root/.flowise" \= "flowise\_data"

#### **デモ・検証における実践シナリオ**

デモの文脈として、「1からコードを書かずに、ものの10分で自社仕様のAIエージェントボットを組み立てて本番システムに埋め込む」開発者の俊敏性（アジリティ）を示す 27。ato run flowise を叩くと、一切のNode依存エラーを意識させることなく、ポート 3000 上に洗練されたフロー設計エディタが出現する 27。  
エディタ上でLLMコンポーネントとチャットメモリ、PDF読み込みパーサーを矢印で接続し、即時その場で会話テストを行うビジュアルデモを行う 28。最初に検証を行うためのコマンドは以下の通りである。

Bash  
docker run \-d \--name flowise\_test \-p 3000:3000 \-v /tmp/flowise\_test\_data:/root/.flowise flowiseai/flowise:latest

Flowiseの優先度は「A」である。AI開発者の日常使いにおいて最も衝撃を与える「ビジュアルAIビルダー」であり、Atoのポータビリティを宣伝する強烈な武器となるからである 28。

## **実実環境に向けたローンチ・ロードマップ**

Atoランタイムの性能を現実の開発環境で確実に実証するため、実装・検証プロセスを「第1期」と「第2期」の2段階に分け、明確な戦略的根拠を伴うローンチプランを以下に提示する。

### **最初の 5 recipes (第1期：技術的堅牢性とスピードの実証)**

第1期フェーズでは、Atoランタイムエンジン自体のコア機能（ポート解決、シングルプロセスマウント、Goネイティブバイナリ自動検知）の徹底的な品質保証を最優先する。そのため、外部依存がなく、軽量で、バグが極限まで生じにくい「シングルプロセスかつSQLite駆動」の頑健な5つのプロジェクトを厳選して展開する。

【第1期：初期レシピリリースフロー】  
                                  
  (1) memos      \==\>  Atoの基本動作、SQLiteマウント検証   
         │  
         v  
  (2) pgweb       \==\>  Nativeバイナリ駆動と、超高速プロセスの評価   
         │  
         v  
  (3) mailpit    \==\>  TCP/HTTP「複数ポートバインディング」の実証   
         │  
         v  
  (4) shiori     \==\>  CLI引数解釈と、Web UIの同一バイナリ切り替え評価   
         │  
         v  
  (5) filebrowser  \=\>  ホストフォルダとの高頻度読み書き・パーミッション解決 

* **1\. usememos/memos** 1: Goバックエンド、Reactフロントエンドが1つになった20MBの極小フットプリント 1。Atoがホストマシンのパーミッション問題やポート重複をどう自動ハンドリングするかのテストに最も完璧なプロジェクトである 1。  
* **2\. sosedoff/pgweb** 7: 開発者が日常で頻繁に使うGoシングルバイナリの超軽量DBクライアント 7。コンテナを介さない「Native実行チャネル」の速度と、ポート衝突時の一括リダイレクトを試す格好のベンチマークとなる 7。  
* **3\. axllent/mailpit** 10: SMTPのポート 1025 と Web UIのポート 8025 を同時に立ち上げ公開する、開発用必須ユーティリティ 10。Atoにおける「複数ポート・マルチポート・ルーティング」の検証として絶対に稼働させるべき一品 10。  
* **4\. go-shiori/shiori** 24: Go製のPocket代替ブックマークツール 24。CLI引数を透過的にコンテナや生プロセスにパスしつつ、ポート起動を完了させるAtoの「コマンドライン・シームレスパススルー」の検証台。  
* **5\. filebrowser/filebrowser** 32: ホスト内のファイルシステムの一部をブラウザ上で即座に視覚化・編集・アップロード可能にする超高機能Webファイラー 32。ホストフォルダをバインドする際の書き込み権限調停を、Atoランタイムのミドルウェアレイヤーで完全に自動解決する機構を確立する 34。

### **次の 10 recipes (第2期：マルチコンテナ連携と高度なAI・データ利活用の実証)**

第2期フェーズでは、Atoランタイムの最も強力な差別化要因である「マルチコンテナサービスオーケストレーション」「AI・LLM連携ポートブリッジ」「プロダクション級業務システムの自動起動」の実装と、圧倒的なデモ価値（README不要の即時AI体験）の創出へ一気に舵を切る。

【第2期：キラーAI＆データツール展開フロー】

    
     \- open-webui  : ホスト側Ollama自動認識ブリッジの究極デモ   
     \- anything-llm : PDFドラッグ＆ドロップによる完全ローカルRAG検証   
     \- flowise      : ビジュアルAIエージェントの、Node.js Nativeメモリ最適化   
     \- blinko        : Postgres協調型AIカードメモのComposeリライト自動解決   
         │  
         v  
   \[業務スプレッド・ナレッジ管理スタック\]  
     \- linkwarden  : Puppeteerによるリソース制約・ヘルスチェック遅延の制御   
     \- vikunja      : 高機能ToDo・カンバン。アカウントベースURLの自動割り振り   
     \- grist-core   : 表計算＋SQLite＋Pythonサンドボックスによるローカル業務DB   
         │  
         v  
   \[インフラ・データ可視化スタック\]  
     \- umami        : Prisma DB自動マイグレーションを含むNext.js統計ダッシュボード   
     \- dbgate       : 各種SQL/NoSQLに対する、即時Web GUIクライアントの提供   
     \- pingvin-share       : ファイル共有を一時URLで可能にする軽量なセルフホスト共有ユーティリティ

* **6\. open-webui/open-webui** 15: OLLAMA\_BASE\_URL に対するホストゲートウェイ（host.docker.internal）の自動挿入を技術検証し、開発者のローカルLLMとUIコンテナを何の設定もなく結合させる 18。  
* **7\. Mintplex-Labs/anything-llm** 25: RAG用ドキュメントコレクターなどの重厚な初期化処理を持つコンテナに対し、Atoの「HTTP Readinessチェック」における待機タイムアウト自動延長とユーザーローディング画面表示の実装。  
* **8\. flowiseai/flowise** 27: NodeJSモノレポ特有の実行メモリクラッシュを避けるため、Atoランタイムが自動的にNativeのヒープ割り当て引数（max-old-space-size）を拡張注入するオートセットアップの実証 27。  
* **9\. blinkospace/blinko** 4: 公式Composeファイルが host ネットワークを要求する問題を 6、Atoが解析段階で自動的にBridgeポートマッピングに書き換えて安全にホストポート 1111 を開放する「スマートComposeコンバータ（--oci-compose）」の検証 5。  
* **10\. linkwarden/linkwarden** 13: Puppeteerをコンテナ内で動作させるためのシステムリソース（低スペックホストでの起動制御）および、PDFなどの生成魚拓ファイルをホスト側ストレージへ確実に保存するボリューム永続化保証 13。  
* **11\. go-vikunja/vikunja** 19: SQLite駆動において、新規のアカウント作成処理をベースURL（VIKUNJA\_SERVICE\_PUBLICURL）と完全同期させ、ポートが変更されても認証エラー（401 Unauthorized）を出さない設定の自動追従 21。  
* **12\. gristlabs/grist-core** 29: 表計算とリレーショナルDBが融合したGristをローカルで動かす 29。SQLite駆動のため、データのインポートからホスト保存までが完全に透過的に機能する。  
* **13\. umami-software/umami** 31: Next.js製の軽量アクセス解析ダッシュボード 31。起動時に自動実行されるPrismaのデータベースマイグレーション（テーブル構築）が完了するまで、Web公開ポートへのリダイレクトをAtoランタイムが厳密に待機する「マイグレーション完了ブロッキングチェック」の品質検証。  
* **14\. dbgate/dbgate** 40: さまざまなデータベースに接続可能なブラウザ型SQLビューア 40。開発者が日常で一時的なクエリ検証を行う際のスピードとマウントポータビリティの実証。  
* **15\. stonith404/pingvin-share**: 完全に自己完結したファイル共有サービス。ホスト内の一時フォルダをマウントし、共有されたファイルがホストを汚さずにAtoが終了した瞬間に一緒に消去可能な「Ephemeralモード（使い捨て実行モード）」の極めて有効なデモ。

本実装ロードマップを厳格に執行することにより、Ato開発チームは第1段階においてシステムの堅牢性を極めて高い基準で担保し、続く第2段階でテックコミュニティのAIエージェント、ローカルRAG開発者、およびインフラ自己ホスト派の日常を劇的にアップデートする「README不要の一発起動の快感」を最大限にアピールすることが可能となる。

#### **引用文献**

1. usememos/memos: Open-source, self-hosted note-taking ... \- GitHub, 5月 23, 2026にアクセス、 [https://github.com/usememos/memos](https://github.com/usememos/memos)  
2. memos module \- github.com/usememos/memos \- Go Packages, 5月 23, 2026にアクセス、 [https://pkg.go.dev/github.com/usememos/memos](https://pkg.go.dev/github.com/usememos/memos)  
3. Deploy Memos (Open-Source Knowledge Base & Memo Notes Tool) \- Railway, 5月 23, 2026にアクセス、 [https://railway.com/deploy/memo](https://railway.com/deploy/memo)  
4. blinkospace/blinko: An open-source, self-hosted personal ... \- GitHub, 5月 23, 2026にアクセス、 [https://github.com/blinkospace/blinko](https://github.com/blinkospace/blinko)  
5. How to use Blinko: Self-hosted AI notes guide \- Roundproxies, 5月 23, 2026にアクセス、 [https://roundproxies.com/blog/blinko/](https://roundproxies.com/blog/blinko/)  
6. docker-compose.prod.yml \- blinkospace/blinko \- GitHub, 5月 23, 2026にアクセス、 [https://github.com/blinkospace/blinko/blob/main/docker-compose.prod.yml](https://github.com/blinkospace/blinko/blob/main/docker-compose.prod.yml)  
7. sosedoff/pgweb: Cross-platform client for PostgreSQL databases \- GitHub, 5月 23, 2026にアクセス、 [https://github.com/sosedoff/pgweb](https://github.com/sosedoff/pgweb)  
8. pgweb command \- github.com/flowbi/pgweb \- Go Packages, 5月 23, 2026にアクセス、 [https://pkg.go.dev/github.com/flowbi/pgweb?utm\_source=godoc](https://pkg.go.dev/github.com/flowbi/pgweb?utm_source=godoc)  
9. Deploy PgWeb \[Updated May '26\] \- Railway, 5月 23, 2026にアクセス、 [https://railway.com/deploy/pgweb](https://railway.com/deploy/pgweb)  
10. naivesystems/mailpit \- GitHub, 5月 23, 2026にアクセス、 [https://github.com/naivesystems/mailpit](https://github.com/naivesystems/mailpit)  
11. Mailpit \- email & SMTP testing tool, 5月 23, 2026にアクセス、 [https://mailpit.axllent.org/](https://mailpit.axllent.org/)  
12. The official Mailpit integration plugin for Lando \- GitHub, 5月 23, 2026にアクセス、 [https://github.com/lando/mailpit](https://github.com/lando/mailpit)  
13. GitHub \- linkwarden/linkwarden: ⚡️⚡️⚡️ Self-hosted collaborative bookmark manager to collect, read, annotate, and fully preserve what matters, all in one place., 5月 23, 2026にアクセス、 [https://github.com/linkwarden/linkwarden](https://github.com/linkwarden/linkwarden)  
14. LinkWarden project \- Libre Self-hosted, 5月 23, 2026にアクセス、 [https://libreselfhosted.com/project/linkwarden/](https://libreselfhosted.com/project/linkwarden/)  
15. Home / Open WebUI, 5月 23, 2026にアクセス、 [https://docs.openwebui.com/](https://docs.openwebui.com/)  
16. open-webui/open-webui: User-friendly AI Interface (Supports Ollama, OpenAI API ... \- GitHub, 5月 23, 2026にアクセス、 [https://github.com/open-webui/open-webui](https://github.com/open-webui/open-webui)  
17. Open WebUI \- GitHub, 5月 23, 2026にアクセス、 [https://github.com/open-webui](https://github.com/open-webui)  
18. Open WebUI integration \- Docker Docs, 5月 23, 2026にアクセス、 [https://docs.docker.com/ai/model-runner/openwebui-integration/](https://docs.docker.com/ai/model-runner/openwebui-integration/)  
19. GitHub \- go-vikunja/vikunja: The to-do app to organize your life., 5月 23, 2026にアクセス、 [https://github.com/go-vikunja/vikunja](https://github.com/go-vikunja/vikunja)  
20. How to Deploy Vikunja (Task Manager) via Portainer \- OneUptime, 5月 23, 2026にアクセス、 [https://oneuptime.com/blog/post/2026-03-20-deploy-vikunja-task-manager-portainer/view](https://oneuptime.com/blog/post/2026-03-20-deploy-vikunja-task-manager-portainer/view)  
21. Docker Walkthrough \- Vikunja, 5月 23, 2026にアクセス、 [https://vikunja.io/docs/docker-walkthrough/](https://vikunja.io/docs/docker-walkthrough/)  
22. Installing \- Vikunja, 5月 23, 2026にアクセス、 [https://vikunja.io/docs/installing/](https://vikunja.io/docs/installing/)  
23. Full docker example \- Vikunja, 5月 23, 2026にアクセス、 [https://vikunja.io/docs/full-docker-example/](https://vikunja.io/docs/full-docker-example/)  
24. Shiori \- Simple bookmark manager built with Go \- GitHub, 5月 23, 2026にアクセス、 [https://github.com/go-shiori/shiori](https://github.com/go-shiori/shiori)  
25. GitHub \- Sukomal07/anythingllm: The all-in-one Desktop & Docker AI application with full RAG and AI Agent capabilities., 5月 23, 2026にアクセス、 [https://github.com/Sukomal07/anythingllm](https://github.com/Sukomal07/anythingllm)  
26. AnythingLLM | The all-in-one AI application for everyone, 5月 23, 2026にアクセス、 [https://anythingllm.com/](https://anythingllm.com/)  
27. FlowiseAI/Flowise: Build AI Agents, Visually \- GitHub, 5月 23, 2026にアクセス、 [https://github.com/flowiseai/flowise](https://github.com/flowiseai/flowise)  
28. Flowise \- Build AI Agents, Visually, 5月 23, 2026にアクセス、 [https://flowiseai.com/](https://flowiseai.com/)  
29. gristlabs/grist-core: Grist is the evolution of spreadsheets. \- GitHub, 5月 23, 2026にアクセス、 [https://github.com/gristlabs/grist-core](https://github.com/gristlabs/grist-core)  
30. GitHub \- gristlabs/grist-desktop: Desktop Grist, packaged with Electron, 5月 23, 2026にアクセス、 [https://github.com/gristlabs/grist-desktop](https://github.com/gristlabs/grist-desktop)  
31. GitHub \- umami-software/umami: Umami is a modern, privacy-focused analytics platform. An open-source alternative to Google Analytics, Mixpanel and Amplitude., 5月 23, 2026にアクセス、 [https://github.com/umami-software/umami](https://github.com/umami-software/umami)  
32. filebrowser/filebrowser: Web File Browser · GitHub \- GitHub, 5月 23, 2026にアクセス、 [https://github.com/filebrowser/filebrowser](https://github.com/filebrowser/filebrowser)  
33. filebrowser \- File Browser, 5月 23, 2026にアクセス、 [https://filebrowser.org/cli/filebrowser.html](https://filebrowser.org/cli/filebrowser.html)  
34. Installation \- File Browser, 5月 23, 2026にアクセス、 [https://filebrowser.org/installation.html](https://filebrowser.org/installation.html)  
35. Setting Up Filebrowser with Docker Compose \- Techdox Docs, 5月 23, 2026にアクセス、 [https://docs.techdox.nz/filebrowser/](https://docs.techdox.nz/filebrowser/)  
36. GitHub \- nocodb/nocodb: A Free & Self-hostable Airtable Alternative, 5月 23, 2026にアクセス、 [https://github.com/nocodb/nocodb](https://github.com/nocodb/nocodb)  
37. nocodb/nocodb-dev: For Testing / Development \- GitHub, 5月 23, 2026にアクセス、 [https://github.com/nocodb/nocodb-dev](https://github.com/nocodb/nocodb-dev)  
38. nocodb \- NPM, 5月 23, 2026にアクセス、 [https://www.npmjs.com/package/nocodb](https://www.npmjs.com/package/nocodb)  
39. Heads up: Nocodb is no longer open source. \- Cloudron Forum, 5月 23, 2026にアクセス、 [https://forum.cloudron.io/topic/14918/heads-up-nocodb-is-no-longer-open-source.](https://forum.cloudron.io/topic/14918/heads-up-nocodb-is-no-longer-open-source.)  
40. docker-compose.yaml \- GLPI \- GitLab, 5月 23, 2026にアクセス、 [https://gitlab.ow2.org/glpi/glpi/-/blob/8cb44967e1e1eb6941daa92924882dfdf97e7a46/docker-compose.yaml](https://gitlab.ow2.org/glpi/glpi/-/blob/8cb44967e1e1eb6941daa92924882dfdf97e7a46/docker-compose.yaml)  
41. documentation docker compose malformed "yaml: line 4: did not find expected key" · Issue \#931 · blinkospace/blinko \- GitHub, 5月 23, 2026にアクセス、 [https://github.com/blinkospace/blinko/issues/931](https://github.com/blinkospace/blinko/issues/931)  
42. Exploring Pgweb: A cross platform client for PostgreSQL databases \- PTC Community, 5月 23, 2026にアクセス、 [https://community.ptc.com/iot-connectivity-tips-384/exploring-pgweb-a-cross-platform-client-for-postgresql-databases-130777](https://community.ptc.com/iot-connectivity-tips-384/exploring-pgweb-a-cross-platform-client-for-postgresql-databases-130777)  
43. Discussions \- axllent mailpit \- GitHub, 5月 23, 2026にアクセス、 [https://github.com/axllent/mailpit/discussions](https://github.com/axllent/mailpit/discussions)  
44. Releases · axllent/mailpit \- GitHub, 5月 23, 2026にアクセス、 [https://github.com/axllent/mailpit/releases](https://github.com/axllent/mailpit/releases)  
45. axllent/mailpit · GitHub \- Workflow runs, 5月 23, 2026にアクセス、 [https://github.com/axllent/mailpit/actions](https://github.com/axllent/mailpit/actions)  
46. Mailpit | Dokploy, 5月 23, 2026にアクセス、 [https://docs.dokploy.com/docs/templates/mailpit](https://docs.dokploy.com/docs/templates/mailpit)  
47. Local Email Debugging with Mailpit \- Jeff Geerling, 5月 23, 2026にアクセス、 [https://www.jeffgeerling.com/blog/2026/mailpit-local-email-debugging/](https://www.jeffgeerling.com/blog/2026/mailpit-local-email-debugging/)  
48. Setting Mailpit to work with Laravel and Docker \- Stack Overflow, 5月 23, 2026にアクセス、 [https://stackoverflow.com/questions/77787420/setting-mailpit-to-work-with-laravel-and-docker](https://stackoverflow.com/questions/77787420/setting-mailpit-to-work-with-laravel-and-docker)  
49. Sending Email Using Mailpit with Laravel Sail | by Lim Yih En | Medium, 5月 23, 2026にアクセス、 [https://medium.com/@yihen\_26052/sending-email-using-mailpit-with-laravel-sail-a8958f17492c](https://medium.com/@yihen_26052/sending-email-using-mailpit-with-laravel-sail-a8958f17492c)  
50. Package linkwarden \- GitHub, 5月 23, 2026にアクセス、 [https://github.com/orgs/linkwarden/packages/container/package/linkwarden](https://github.com/orgs/linkwarden/packages/container/package/linkwarden)  
51. linkwarden/LICENSE.md at main \- GitHub, 5月 23, 2026にアクセス、 [https://github.com/linkwarden/linkwarden/blob/main/LICENSE.md](https://github.com/linkwarden/linkwarden/blob/main/LICENSE.md)  
52. Linkwarden | Proxmox VE Helper Scripts \- GitHub Pages, 5月 23, 2026にアクセス、 [https://community-scripts.github.io/ProxmoxVE/scripts?id=linkwarden](https://community-scripts.github.io/ProxmoxVE/scripts?id=linkwarden)  
53. Linkwarden — Bookmarks, Evolved, 5月 23, 2026にアクセス、 [https://linkwarden.app/](https://linkwarden.app/)  
54. Releases · open-webui/open-webui \- GitHub, 5月 23, 2026にアクセス、 [https://github.com/open-webui/open-webui/releases](https://github.com/open-webui/open-webui/releases)  
55. danielrosehill/openwebui-docs \- GitHub, 5月 23, 2026にアクセス、 [https://github.com/danielrosehill/openwebui-docs](https://github.com/danielrosehill/openwebui-docs)  
56. guide : using llama-ui — the new WebUI of llama.cpp \#16938 \- GitHub, 5月 23, 2026にアクセス、 [https://github.com/ggml-org/llama.cpp/discussions/16938](https://github.com/ggml-org/llama.cpp/discussions/16938)  
57. Vikunja \- GitHub, 5月 23, 2026にアクセス、 [https://github.com/go-vikunja](https://github.com/go-vikunja)  
58. Setup Vikunja using Docker Compose \- The Homelab Wiki, 5月 23, 2026にアクセス、 [https://thehomelab.wiki/books/docker/page/setup-vikunja-using-docker-compose](https://thehomelab.wiki/books/docker/page/setup-vikunja-using-docker-compose)  
59. Packages · Vikunja \- GitHub, 5月 23, 2026にアクセス、 [https://github.com/orgs/go-vikunja/packages?ecosystem=container\&sort\_by=downloads\_asc](https://github.com/orgs/go-vikunja/packages?ecosystem=container&sort_by=downloads_asc)  
60. shiori command \- github.com/go-shiori/shiori \- Go Packages, 5月 23, 2026にアクセス、 [https://pkg.go.dev/github.com/go-shiori/shiori](https://pkg.go.dev/github.com/go-shiori/shiori)  
61. go-shiori shiori · Discussions · GitHub, 5月 23, 2026にアクセス、 [https://github.com/go-shiori/shiori/discussions](https://github.com/go-shiori/shiori/discussions)  
62. Shiori \- GitHub, 5月 23, 2026にアクセス、 [https://github.com/go-shiori](https://github.com/go-shiori)  
63. shiori/docs/API.md at master \- GitHub, 5月 23, 2026にアクセス、 [https://github.com/go-shiori/shiori/blob/master/docs/API.md](https://github.com/go-shiori/shiori/blob/master/docs/API.md)  
64. Mintplex Labs \- GitHub, 5月 23, 2026にアクセス、 [https://github.com/mintplex-labs](https://github.com/mintplex-labs)  
65. AnythingLLM download | SourceForge.net, 5月 23, 2026にアクセス、 [https://sourceforge.net/projects/anything-llm.mirror/](https://sourceforge.net/projects/anything-llm.mirror/)  
66. AnythingLLM Embedded Chat Widget \- GitHub, 5月 23, 2026にアクセス、 [https://github.com/Mintplex-Labs/anythingllm-embed](https://github.com/Mintplex-Labs/anythingllm-embed)  
67. FileBrowser Quantum Documentation, 5月 23, 2026にアクセス、 [https://filebrowserquantum.com/](https://filebrowserquantum.com/)