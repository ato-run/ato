# 引き継ぎ: WasedaP2P を Docker なし `capsule.toml` だけで動かす検証

> 作成: 2026-05-01
> ato CLI: 0.4.103 (host) / 0.4.109 (nacelle, auto-installed)
> 対象リポ: <https://github.com/itsukison/wasedap2p>
> 作業ディレクトリ: `~/ato-tests/WasedaP2P`
> 関連: `docs/capsulize-vikunja-memos-libretranslate.md`

---

## 0. 結論先出し

**「github clone してから capsule.toml だけを 1 ファイル付け足す」では現状起動できない。**

理由は 4 つあり、上から重い順に積み重なっている:

| # | 障害 | 原因の所在 | 「capsule.toml だけ」で回避可能か |
|---|---|---|---|
| 1 | 上流アプリが PostgreSQL ハードコード | `WasedaP2P/backend/main.py` + `db_init.py::ensure_schema_updates()` の `ALTER TABLE ... ADD COLUMN IF NOT EXISTS` が SQLite で構文エラー | **不可**（上流コード変更が必要、もしくは host postgres を別途用意） |
| 2 | `main.py` に `if __name__ == "__main__":` ブロックなし | 上流コードは ASGI モジュールとしてしか起動できず、ato source/python は `python <entrypoint>.py` 形式しか受け付けない | **不可**（launcher .py を別途同梱する必要） |
| 3 | 上流リポに `requirements.txt` も `uv.lock` も無い | ato CLI の build phase が `Provision [app]: uv venv && uv pip install -r requirements.txt` を組み立てるため | **不可**（requirements.txt を別途同梱する必要） |
| 4 | `python -m uvicorn ...` 形式の `run` がプロセス起動直後に「lost child handle」になる | nacelle 0.4.109 supervisor + ato 0.4.103 の組み合わせ | **回避可能だが回避策は launcher .py** |

**最低限必要な追加ファイル数 (Docker なしで動かすため): 3 個**

- `capsule.toml`
- `backend/serve.py`（10 行程度の uvicorn launcher。`if __name__: uvicorn.run("main:app", ...)`）
- `backend/requirements.txt`（fastapi/uvicorn/sqlalchemy/psycopg/passlib/argon2-cffi/pyjwt/python-multipart/python-dotenv の 9 行）

加えて、**host 側で PostgreSQL を立てる**か、**上流の `db_init.py` を SQLite 互換に直す**かのどちらかが必要。

---

## 1. 検証ログ

### 1.1 環境

```sh
$ ato --version
ato 0.4.103
$ which ato
/Users/egamikohsuke/.cargo/bin/ato
$ ls ~/ato-tests/WasedaP2P/   # git clone https://github.com/itsukison/wasedap2p で展開
backend/  frontend/  README.md
```

### 1.2 source/python の最小再現サンプル（成功した）

```sh
mkdir -p /tmp/ato-pyfastapi && cd /tmp/ato-pyfastapi
cat > main.py <<'EOF'
import os, uvicorn
from fastapi import FastAPI
app = FastAPI()
@app.get("/")
def root(): return {"ok": True}
if __name__ == "__main__":
    uvicorn.run(app, host="127.0.0.1", port=8765)
EOF
cat > requirements.txt <<'EOF'
fastapi==0.115.6
uvicorn==0.32.1
EOF
cat > capsule.toml <<'EOF'
schema_version  = "0.3"
name            = "test-pyfastapi"
version         = "0.0.1"
type            = "app"
runtime         = "source/python"
runtime_version = "3.11.10"
run             = "main.py"
port            = 8765
EOF
ato run . -y --sandbox -v --rebuild
# 別ターミナル: curl http://127.0.0.1:8765/  →  {"ok":true}
```

→ **起動成功**（curl が `{"ok":true}` を返す）。

### 1.3 WasedaP2P で発生した症状（時系列）

#### 1.3.1 `--enforcement best-effort` は廃止

```
× --enforcement best-effort is no longer supported; use --enforcement strict
```

→ **strict のみ**。`best-effort` は実質 0.4.103 で廃止済み。

#### 1.3.2 `--sandbox` か `--dangerously-skip-permissions` 必須

```
E301: source/native|python execution requires explicit --sandbox opt-in or
      --dangerously-skip-permissions
```

→ source/python 系は host process として走るので Tier 2 sandbox 必須。

#### 1.3.3 `--dangerously-skip-permissions` は uv venv を使ってくれない

dangerous モードで起動したら、`.venv/lib/python3.11/site-packages` が `sys.path` に入らず、`ModuleNotFoundError: No module named 'uvicorn'` が発生:

```
PHASE execute RUN  capsule execution
/Users/egamikohsuke/.ato/toolchains/python-3.11.10/python/bin/python3: No module named uvicorn
```

`build` phase は `uv venv && uv pip install -r requirements.txt` を確かに走らせて 12 パッケージ入れている（成功ログあり）が、実行 phase は **toolchain 直の python3 を呼ぶだけ**で venv を activate しない。

→ つまり **`--dangerously-skip-permissions` モードは source/python の依存解決と組み合わない**。`--sandbox` モード必須。

#### 1.3.4 `runtime_version` だけでは E999 が出る

```toml
[targets.app]
runtime         = "source"
language        = "python"
runtime_version = "3.11.10"
run             = "main.py"
```

```
PHASE execute FAIL targets.app.runtime_version or runtime_tools.python is
                  required for authoritative python execution
```

→ `[targets.X]` 形式で `runtime = "source"` + `language = "python"` を指定した場合は、追加で `[targets.X.runtime_tools] python = "3.11.10"` が必要、または top-level の legacy 形式 `runtime = "source/python"` を使う必要がある。

**回避策**: top-level で `runtime = "source/python"` + `runtime_version = "3.11.10"` を書く（python-fastapi sample と同じ）。

#### 1.3.5 `working_dir = "backend"` を指定すると uv.lock を強要される

```toml
runtime     = "source/python"
working_dir = "backend"
run         = "serve.py"
```

```
E104: source/python target requires uv.lock for fail-closed provisioning
```

しかし **同じ requirements.txt を manifest dir 直下に置く（`working_dir` 指定なし）と uv.lock 無しでも provisioning が走る**:

```
⚙️  Provision [app]: uv venv && uv pip install -r requirements.txt
Resolved 23 packages in 1.52s ...
```

→ **`working_dir` を切ると lockfile probe が working_dir 内のみを見るため、shadow lockfile auto-generation がスキップされる**バグの疑い。
コード上の関連箇所:
- `crates/ato-cli/src/adapters/runtime/provisioning/diagnose.rs:63` (`working_dir.join("uv.lock")` のみ candidate に)
- `crates/capsule-core/src/routing/importer/mod.rs:223` (`probe_required_python_lockfile` は `project_root` を見る)
- `crates/ato-cli/src/adapters/runtime/provisioning/shadow.rs:459` (shadow generation も working_dir 基準)

#### 1.3.6 `python -m uvicorn ...` 形式の `run` で「lost child handle」発生

```toml
run = "python -m uvicorn main:app --host 127.0.0.1 --port 8765"
```

```
PHASE execute RUN  capsule execution
[nacelle] Supervisor mode: waiting for child PID 89354 to terminate...
Error: Internal exec lost child handle for PID 89354
       (run_id=exec-89352, async_handles=[], sync_handles=[])
[✓] Sandbox initialized
📈 Metrics: session=..., duration_ms=1, peak_memory_bytes=28065792
```

`run = "main.py"`（同じ FastAPI が `if __name__:` で起動）にすると同じバイナリ・同じ deps で curl が `{"ok":true}` を返す。

→ **`-m` 引数を付けると nacelle supervisor が child handle を register 完了する前に exec が完了して報告される race condition、もしくは command spawning に欠陥がある**。
コード上の関連箇所:
- `crates/capsule-core/src/routing/launch_spec.rs:125-170` (python branch で first token が `python`/`python3`/`uv` の場合、`tokens.get(1)` を entrypoint として抜き出す。`-m` も entrypoint 扱いされる)
- `crates/ato-cli/src/adapters/runtime/executors/source.rs:140-162` (resulting `Command::new(python_bin).arg("-m")` を spawn → nacelle に sandbox 化を頼む直前で何かが壊れる)
- `crates/ato-cli/src/adapters/runtime/executors/source.rs:680-708` (nacelle path で uv run --with-requirements ... python3 -m uvicorn ... を組み立てるロジック)

#### 1.3.7 `working_dir` を切らずに `serve.py` を root に置いて成功した瞬間でも、後続テストで再現性が低い

`run = "serve.py"`（manifest dir 直下に launcher を作って main.py を import）でも同じ "lost child handle" が出る case あり。
duration_ms=0 か 1 のときは **child が即死**しているケースで、`uvicorn` が起動前にコケた可能性が高い。

WasedaP2P を `.venv/bin/python serve.py` で直接走らせたところ、SQLite 互換性問題（後述 1.3.8）で起動 lifespan で例外発生・即終了する。これが lost child handle として観測される真因。

→ つまり「lost child handle」は **2 つの原因の混在**:
- (a) `python -m` のときは確実に発生（アプリ無関係）
- (b) `python script.py` のときは **アプリが import / lifespan で例外を吐いて即落ち**することの supervisor 側の見え方

(a) は確定で nacelle/ato 側のバグ、(b) は誤解を招く error message。

#### 1.3.8 SQLite で `ensure_schema_updates()` が落ちる

```sh
.venv/bin/python serve.py   # 直接実行
```

```
INFO:     Started server process [93658]
INFO:     Waiting for application startup.
ERROR:    Traceback ...
  File "/Users/egamikohsuke/ato-tests/WasedaP2P/db_init.py", line 24, in ensure_schema_updates
    connection.execute(text("ALTER TABLE users ADD COLUMN IF NOT EXISTS email_verified BOOLEAN NOT NULL DEFAULT FALSE"))
sqlite3.OperationalError: near "EXISTS": syntax error
```

→ 上流の `backend/db_init.py:21-30` が **PostgreSQL 専用構文** (`ADD COLUMN IF NOT EXISTS`) をハードコードしている。
SQLite 3.35+ は `ALTER TABLE ... ADD COLUMN` 自体は OK だが `IF NOT EXISTS` は未サポート（sqlite3 module 経由で見える）。

**結論**: WasedaP2P を SQLite で起動するには上流コード変更が要る（`try/except` で対応するか、dialect で分岐するか）。

#### 1.3.9 ato build phase の `.venv` 上書き失敗（再実行の冪等性なし）

```sh
ato run . -y --sandbox -v   # （--rebuild なし、2 回目）
```

```
⚙️  Provision [app]: uv venv && uv pip install -r requirements.txt
error: Failed to create virtual environment
  Caused by: A virtual environment already exists at `.venv`. Use `--clear` to replace it
PHASE build FAIL provision command failed with exit code 2
```

→ build phase の `uv venv` 呼び出しが `--clear` を渡していない。`--rebuild` を付けるか手動で `rm -rf .venv` を打たないと 2 回目以降は通らない。
- `crates/ato-cli/src/adapters/runtime/provisioning/shadow.rs` 周辺の provisioning command 生成ロジックに該当行があるはず。

---

## 2. 詳細解説

### 2.1 「`python -m` で lost child handle」 ≒ ato/nacelle のバグ

#### 2.1.1 何が起きているか

`run = "python -m uvicorn main:app ..."` を書くと:

1. ato は launch_spec.rs の python branch で `command = "-m"`, `args = ["uvicorn", "main:app", ...]` に分割する
2. source.rs の `force_python_no_bytecode` 経路（line 142-162）で:
   ```rust
   let python_bin = resolve_host_managed_runtime_binary(plan, lock, ManagedRuntimeKind::Python)?;
   let mut python = Command::new(python_bin);
   python.arg(&host_command_path);   // host_command_path = "-m" (resolve_host_command_path で working_dir 配下に "-m" が無いのでそのまま返される)
   ```
3. nacelle に sandbox 起動依頼を投げる
4. nacelle supervisor:
   ```
   [nacelle] Supervisor mode: waiting for child PID NNNNN to terminate...
   Error: Internal exec lost child handle for PID NNNNN (run_id=exec-NNNNN, async_handles=[], sync_handles=[])
   [✓] Sandbox initialized
   ```
   すぐに「lost child handle」エラーが出て、duration_ms=0/1 で完了。

#### 2.1.2 なぜ `run = "main.py"` だと動くのか

main.py を直接渡すと `command = "main.py"`, `args = []` になる。
`resolve_host_command_path("main.py")` が `working_dir/main.py` の存在を確認して **絶対パスに canonicalize** する。
`Command::new(python_bin).arg("/absolute/path/main.py")` という具体的な path を渡すので、nacelle 側の child handle register が安定する。

つまり:
- **fix の方向 A**: launch_spec.rs で `first == "python"` かつ `tokens[1] == "-m"` のときは `command = "python"`, args 全部を渡す形にして、source.rs 側で `python -m uvicorn ...` をそのまま spawn する
- **fix の方向 B**: nacelle 側で「Sandbox initialized」が出る前にイベントループを回して child handle の register 完了を待つ
- **fix の方向 C**: dual-target サンプル (`uvicorn-cli` のようなもの) を canonical sample として追加し、`python -m` パターンをサポート対象として明示

#### 2.1.3 issue として書くべきか

**書くべき**。再現手順がシンプル（`python -m uvicorn` を `run` に書くだけ）で、Python アプリで一般的に使われるパターン。
issue タイトル例:
> source/python: `run = "python -m <module>"` causes nacelle "lost child handle" before the script ever starts

repro:
- repo: 上記 1.2 の minimal sample で main.py の `if __name__: ` ブロックを削除し、capsule.toml の run を `"python -m uvicorn main:app --host 127.0.0.1 --port 8765"` に変更
- expected: HTTP 200 from /
- actual: process exits at duration_ms<2, supervisor reports lost handle

### 2.2 「`requirements.txt` 必須」 ≒ ato CLI の生 Python 起動には常にロックファイルか requirements.txt が要る

#### 2.2.1 規範: fail-closed provisioning

ato 0.4.x は **「source/python target requires uv.lock for fail-closed provisioning」** をエラーコード `E104 / ATO_ERR_PROVISIONING_LOCK_INCOMPLETE` で出す。
コード:
- `crates/capsule-core/src/routing/importer/mod.rs:223-228` (`probe_required_python_lockfile`)
- `crates/capsule-core/src/engine/execution_plan/derive.rs:717` (test asserts the error code)

#### 2.2.2 ただし shadow lockfile auto-generation で救済できる

manifest dir 直下に `requirements.txt` がある場合、`crates/ato-cli/src/adapters/runtime/provisioning/shadow.rs` が:

1. `.venv/` を作成
2. `uv pip install -r requirements.txt` を実行
3. （shadow lockfile を導出して `.ato/derived/` に書く）

を build phase で自動でやってくれる。**つまり `requirements.txt` さえあれば `uv.lock` は最終的に不要**。

#### 2.2.3 `working_dir` を切ると shadow auto-gen がスキップされる

これは恐らくバグ。`crates/ato-cli/src/adapters/runtime/provisioning/diagnose.rs:63` が

```rust
let candidates = vec![working_dir.join("uv.lock")];
```

としか見ない。`requirements.txt` の auto-shadow が manifest dir では走るのに、working_dir に切り替えた瞬間に発火しないのは UX としても破綻している。

#### 2.2.4 issue として書くべきか

**書くべき**。タイトル例:
> source/python: shadow lockfile auto-generation does not fire when `working_dir` differs from manifest dir

repro:
- minimal: `capsule.toml` に `working_dir = "backend"` + `run = "serve.py"`、`backend/requirements.txt` を置く
- expected: build phase で provisioning が成功
- actual: E104 ATO_ERR_PROVISIONING_LOCK_INCOMPLETE

加えて、別 issue として **「上流リポに requirements.txt が無いケース」のハンドリング指針** を docs に書く価値がある。今は「自分で作って同梱しろ」が暗黙のルールになっており、capsule consumer 側の体験が悪い:

> docs: clarify that source/python capsules must ship a requirements.txt or uv.lock; document the shadow-lockfile auto-generation behavior

### 2.3 もう一つ気になった隙間: `ato run` 2 回目の build phase が壊れる

#### 2.3.1 再現

1 回目: `ato run . -y --sandbox --rebuild` → 成功（`.venv` を新規作成）
2 回目: `ato run . -y --sandbox` → 失敗（`.venv` 既存、`uv venv` が `--clear` 無しで衝突）

```
error: Failed to create virtual environment
  Caused by: A virtual environment already exists at `.venv`. Use `--clear` to replace it
PHASE build FAIL provision command failed with exit code 2
```

#### 2.3.2 fix 方向

provisioning shadow command を `uv venv --clear .venv && uv pip install -r requirements.txt` にするか、`if .venv 存在 && hash(requirements.txt) 一致` なら provisioning skip。
後者の方が dev loop が高速になる（再 install しないで済む）。

#### 2.3.3 issue として書くべきか

**書くべき**。タイトル例:
> source/python: build phase fails on second run because `uv venv` is invoked without `--clear`

---

## 3. 実用上の現実解（Docker なし、capsule.toml 1 ファイルだけ縛りを諦めた場合）

### 3.1 最小スキャフォールド構成

```
~/ato-tests/WasedaP2P/
├── capsule.toml            ← 我々が新規作成
├── backend/
│   ├── serve.py            ← 我々が新規作成（uvicorn launcher 6 行）
│   ├── requirements.txt    ← 我々が新規作成（pip 依存 9 行）
│   ├── main.py             (上流のまま)
│   ├── db_init.py          (上流のまま、ただし PostgreSQL 必須)
│   ├── ...
│   └── (.env を別途作成)
└── frontend/               (上流のまま)
```

### 3.2 capsule.toml 案（最小、backend のみ動かす版）

```toml
schema_version  = "0.3"
name            = "wasedap2p-backend"
version         = "0.1.0"
type            = "app"
runtime         = "source/python"
runtime_version = "3.11.10"
run             = "backend/serve.py"   # working_dir は切らない
port            = 8000

required_env = ["SECRET_KEY", "DATABASE_URL"]

[isolation]
allow_env = [
  "SECRET_KEY",
  "ALGORITHM",
  "DATABASE_URL",
  "FRONTEND_URL",
  "SMTP_HOST",
  "SMTP_PORT",
  "SMTP_USERNAME",
  "SMTP_PASSWORD",
  "SMTP_FROM_EMAIL",
  "SMTP_USE_TLS",
  "SMTP_USE_SSL",
]
```

requirements.txt も **manifest dir 直下** に置くこと（working_dir バグ回避）。`backend/serve.py` は:

```python
import os, sys, uvicorn
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
if __name__ == "__main__":
    uvicorn.run("main:app", host="127.0.0.1", port=int(os.environ.get("PORT", "8000")))
```

frontend (Vite) は別 capsule か、`runtime = "source/node"` で別 target を切るか、host で素直に `npm install && npm run dev` を回す。

### 3.3 Postgres どうする

3 択:

| 案 | 方法 | コスト |
|---|---|---|
| **A. host postgres** | `brew install postgresql@16 && brew services start postgresql@16` | 5 分、上流 README が想定する構成と同じ |
| **B. SQLite + `db_init.py` パッチ** | `try/except sqlite3.OperationalError: pass` を ensure_schema_updates に入れる | 5 分、ただし上流改変 |
| **C. ato 提供の host-managed postgres** | 現時点で ato にそのような機能なし | — |

**推奨 A**。WasedaP2P は元から host postgres を期待しているので、capsule.toml で `DATABASE_URL=postgresql+psycopg://localhost/wasedap2p` を渡す形が一番自然。

---

## 4. 引き継ぎプロンプト（次セッションへ貼ればすぐ再開できる）

````
WasedaP2P (https://github.com/itsukison/wasedap2p) を ato で Docker なしで動かす検証の続きをやってほしい。
状況は docs/handoff_wasedap2p_no_docker_capsule_20260501.md に詳しく書いた。要点:

- リポは ~/ato-tests/WasedaP2P に clone 済み。capsule.toml / serve.py / requirements.txt
  は実験で何度も書き換えているので、状態確認から始めて。
- 「capsule.toml 1 ファイルだけ追加」は不可能と判明（上流コードに requirements.txt が無く、
  main.py に if __name__ ブロックが無く、ALTER TABLE ADD COLUMN IF NOT EXISTS が
  PostgreSQL 専用のため SQLite では落ちる）。
- ato 0.4.103 + nacelle 0.4.109 で 3 つのバグ/UX 問題を発見した:
    1. `run = "python -m <module>"` 形式で lost child handle (要 issue)
    2. `working_dir` を切ると requirements.txt の shadow auto-gen がスキップされる (要 issue)
    3. 2 回目の `ato run` で `uv venv` が `--clear` なしで衝突 (要 issue)
  詳細は同 doc §2.

次の作業オプション (どれかを選んで進めて):
  (a) host PostgreSQL を立てて、backend/serve.py + backend/requirements.txt + capsule.toml の
      最小 3 ファイル構成で実際に backend だけでも起動できることを確認する。frontend は
      後回し or 別 capsule。
  (b) 上記 3 つのバグを ato 本体（このリポ /Users/egamikohsuke/Ekoh/projects/ato/capsuled-dev/apps/ato）
      の issue としてまとめる。`gh issue create` で出すか、まず docs/TODO.md にぶら下げるか
      は user に確認してから。
  (c) backend の SQLite 対応パッチ（db_init.py の ensure_schema_updates に try/except を入れる）
      を上流に PR として送るための準備。fork → 修正 → upstream PR の流れ。

どれを優先するか聞いてから動いてください。
````

---

## 5. 参考コードロケーション

すべて `/Users/egamikohsuke/Ekoh/projects/ato/capsuled-dev/apps/ato/` 配下:

| パス | 役割 |
|---|---|
| `crates/capsule-core/src/routing/launch_spec.rs:85-194` | `run` 文字列を python/node/その他に分岐して entrypoint と args を切り出す中核 |
| `crates/ato-cli/src/adapters/runtime/executors/source.rs:115-220` | host process spawn 経路（dangerous モード） |
| `crates/ato-cli/src/adapters/runtime/executors/source.rs:550-740` | nacelle exec adapter（`uv run --with-requirements ... python3 ...` を組み立てる） |
| `crates/capsule-core/src/routing/importer/mod.rs:223-228` | `uv.lock` 必須要件の出所 |
| `crates/ato-cli/src/adapters/runtime/provisioning/shadow.rs:459-528` | shadow lockfile auto-generation |
| `crates/ato-cli/src/adapters/runtime/provisioning/diagnose.rs:63` | `working_dir.join("uv.lock")` のみ probe する箇所（バグ疑い） |
| `crates/capsule-core/src/foundation/types/manifest.rs:1150-1260` | `NamedTarget`（`[targets.X]` table の field 一覧） |
| `crates/capsule-core/src/foundation/types/manifest.rs:346-403` | `ServiceSpec` / `ServiceNetworkSpec` |
| `crates/ato-cli/samples/python-fastapi/` | source/python の正準サンプル |
| `crates/ato-cli/tests/fixtures/oci-multi-component/capsule.toml` | services + targets を使ったマルチサービスサンプル |

実機で再現させたいときの最短コマンド:

```sh
cd ~/ato-tests/WasedaP2P
# 1.3.6 の python -m バグ
cat > capsule.toml <<'EOF'
schema_version="0.3"
name="repro-pym"
version="0.1.0"
type="app"
runtime="source/python"
runtime_version="3.11.10"
run="python -m uvicorn main:app --host 127.0.0.1 --port 8765"
port=8765
EOF
cp backend/main.py ./
cat > requirements.txt <<'EOF'
fastapi==0.115.6
uvicorn==0.32.1
sqlalchemy==2.0.36
psycopg[binary]==3.2.3
EOF
rm -rf .venv .ato
SECRET_KEY=x DATABASE_URL=sqlite:///./x.db ato run . -y --sandbox -v --rebuild
# → "lost child handle" + duration_ms<2
```
