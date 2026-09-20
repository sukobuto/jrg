# jrg

**Intent-aware ripgrep results, reranked with Jev.** A native Rust CLI for humans and coding agents. No Python runtime or search index required.

検索キーワードと「探している処理の説明」を渡し、ripgrep の候補を Jev の Noul で順位付けします。

```text
rg --json → 前後5行の候補 → 近接領域を統合 → Jev Noul → 上位10件
```

## インストール

実行時に `rg` が必要です。Rust はソースからビルドする場合に必要です。

```bash
# Homebrew（macOS / Linux）
brew install sukobuto/tap/jrg

# Cargo（Rust 1.88 以上）
cargo install jrg --locked
# rg が未導入なら別途インストールしてください
```

ビルド済みバイナリは [GitHub Releases](https://github.com/sukobuto/jrg/releases) から取得できます。アーカイブを展開して `jrg`（Windows は `jrg.exe`）を PATH の通った場所に置いてください。

開発用にはリポジトリ内で `cargo install --path . --locked`、または `cargo run -- ...` を使えます。

## プロジェクトで使う

プロジェクトの `.jrgenv` にキーを設定します。

```dotenv
TYPESAFE_API_KEY=your-api-key
```

`.jrgenv` をプロジェクトの `.gitignore` に追加してください。共有用の雛形は [.jrgenv.example](.jrgenv.example) です。

```bash
jrg 'retry|backoff|429' src \
  --about '一時的なHTTPエラーで再試行する条件を決めている実装' \
  --json --top 8 --min-relevance 0.5
```

認証の優先順位:

1. 空でない環境変数 `TYPESAFE_API_KEY`
2. `--env-file PATH` で明示した dotenv ファイル
3. カレントディレクトリから Git ルートまでで最も近い `.jrgenv`

Git 外ではカレントディレクトリだけを調べます。Git worktree にも対応します。`--no-env-file` はファイルからの読み込みを無効にします。通常の `.env` は自動では読みませんが、`jrg ... --env-file .env` で使用できます。

dotenv の値を設定データとして読み、シェルとして実行しません。`TYPESAFE_API_KEY` 以外の設定をアプリケーションの環境変数に取り込みません。`.env` / `.env.*` / `.jrgenv` / `.jrgenv.*` と読み込み対象の認証ファイルは、`--hidden`、正の glob、明示的なファイル指定でも候補・出力・API 送信から除外します。これらのファイルへのシンボリックリンクも除外します。

AGENTS.md への指示例は [docs/AGENTS.example.md](docs/AGENTS.example.md) にあります。意図から処理を探す場合は jrg、正確な識別子や文字列の全件検索には rg を使い分けます。

## API を使わず試す

```bash
jrg retry examples/retry_demo \
  --about 'HTTPリクエスト失敗時に、ステータスによって再試行するか判断する処理' \
  --dry-run --json --stats
```

`--dry-run` は認証情報を読み込まず、通信もしません。収集した全候補を `relevance: null` で返し、`--top` と `--min-relevance` は適用しません。通常実行では、検索意図・候補のパス・行番号・ソース断片を TypeSafe API に送信します。

サンプルは再試行の可否を決める `policy.py` と、キーワードが重なるだけの `metrics.py` です。Python ファイルは検索用データなので実行しません。

## 出力と終了コード

`--json` では stdout に JSON 配列、stderr に警告・エラーと任意の統計を出します。以下の確率は説明用です。

```json
[
  {
    "path": "src/policy.rs",
    "start_line": 1,
    "end_line": 3,
    "match_lines": [1],
    "snippet": "fn should_retry(status: u16) -> bool {\n    status == 429 || status >= 500\n}\n",
    "relevance": 0.97
  }
]
```

行番号は1始まり、範囲は両端を含みます。同じ行の複数一致は `match_lines` では1行として扱います。パスは rg の表記を保持します（相対パスに `./` が付く場合があります）。テキスト出力では一致行を `>` で示します。

| 終了コード | 意味 |
| --- | --- |
| `0` | 結果あり |
| `1` | キーワード一致なし、またはしきい値以上の結果なし |
| `2` | 引数・検索・認証・API エラー |
| `130` | Ctrl-C による中断 |

API の途中失敗では部分的な結果を出しません。実行中の HTTP 通信はタイムアウトまで終了を待つ場合があります。

## 主なオプション

| オプション | 既定値 | 用途 |
| --- | --- | --- |
| `--about` / `--intent` | 必須 | 探している処理の説明 |
| `-C`, `--context` | `5` | 一致行の前後の行数 |
| `-n`, `--top` | `10` | 返す上位件数 |
| `--min-relevance` | `0` | Noul の下限。指定値を含む |
| `--max-candidates` | `200` | 評価する候補数の上限 |
| `--max-filesize` | `1M` | rg のファイルサイズ制限 |
| `-g`, `--glob` | なし | rg の glob。複数指定可 |
| `-t`, `--type` | なし | rg の言語タイプ。複数指定可 |
| `-i`, `-F`, `--hidden` | 無効 | 大小文字無視、リテラル検索、隠しファイルを含める |
| `--json`, `--stats` | 無効 | JSON 結果、stderr の統計 |
| `--dry-run` | 無効 | ローカルで全候補を表示 |
| `--env-file`, `--no-env-file` | 自動探索 | 認証ファイルを明示指定 / 読み込み無効 |
| `--batch-size`, `--workers` | `8`, `4` | 1リクエストの候補数 / 最大同時リクエスト数 |
| `--timeout`, `--retries` | `30`, `2` | HTTP タイムアウト秒数 / 過負荷の最大再試行回数 |
| `--model` | `jev-latest` | TypeSafe モデル |
| `--api-url` | `https://api.typesafe.ai/v1/systemone` | HTTPS エンドポイント。localhost のみ HTTP も可 |

既定のしきい値を `0` にして、未検証の確率で必要な候補を落とさないようにしています。まず top-K を評価し、自分のタスクでしきい値を調整してください。

認証ファイル以外は rg の ignore・隠しファイル・バイナリ除外に従います。明示ファイルや正の glob は rg 本来の仕様どおり ignore 等に優先し、明示ファイルにはサイズ上限が適用されない場合があります。除外を優先したい場合は、ディレクトリ指定・`--type`・負の glob を使ってください。個人用 `RIPGREP_CONFIG_PATH` は無効化しているため、`--pre` などの任意コマンドは実行しません。stdin 検索や任意の rg オプション転送は未対応です。

## 設計と評価

- rg の JSON をストリームで読み、ファイルを読み直さずに近接する領域を統合します。検索後の編集で一致位置とソースがずれることを避けます。
- 1候補を120行または UTF-8 12,000 bytes で分割します。境界では前後の文脈が短くなります。1行が上限を超える場合と、非 UTF-8 のソース・パスはエラーにします。
- 候補上限を超えた時点で rg を停止します。パス・行順の先頭候補が対象で、打ち切りは警告と統計に明示します。
- 1候補に1つの独立した Noul を割り当て、「検索意図の処理を実装する、または理解に実質的に役立つか」を評価します。質問本文で対象の `candidates[i].snippet` を明示します。
- シリアライズしたリクエスト全体を24,000 bytes 以内に分割します。文字数/token 比を固定せず、質問とソースを合わせて保守的に制限します。
- 有限の `0..1` だけをスコアとして受理します。同点はパス・開始行順です。429 / 503 / 529 はバックオフで再試行します。数値の `Retry-After` に従い、30秒超や日時形式なら再実行を案内します。接続失敗は重複課金を避けるため自動再試行しません。

`--stats` は候補数、一致行数、返却件数、候補/返却ソースの文字数、検索/rerank 時間、リクエスト数（再試行を含む）、API 入力トークン数、応答モデルを stderr の最後の JSON 行に出します。usage がなければ `input_tokens: null`、API 未使用なら `0` です。文字数はソース断片のみで、エージェントへ渡す JSON 全体のトークン数や課金額ではありません。

**Jev は rg が候補にしなかったコードを発見できません。** Noul は指定した質問への Yes 確率で、検索正解率の保証ではありません。AST による関数単位の展開・二段階 rerank・MCP サーバーは未対応です。

HTTP 契約は [TypeSafe API](https://docs.typesafe.ai/api)、判定型は [Noul](https://docs.typesafe.ai/primitives/noul) に基づきます。小規模な実 API 検証は [評価記録](docs/evaluation.md) を参照してください。

## 開発

```bash
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
cargo build --release --locked
cargo publish --dry-run --locked
```

テストは Rust、実際の rg、ローカル HTTP サーバーで完結し、API キーや Python は不要です。公開手順は [docs/releasing.md](docs/releasing.md) にあります。

v0.2 は Python プロトタイプの CLI・JSON フィールド・終了コードを引き継ぎます。JSON の Unicode は UTF-8 で出力するため、エスケープ表記は異なる場合があります。非 UTF-8 パスは曖昧な位置を返さず、明示的なエラーに変更しています。Python 版は Git 履歴の `cb88653` に残っています。

MIT License.
