# jrg

検索キーワードと「探している処理の説明」を渡し、ripgrep の結果を Jev で順位付けするプロトタイプです。インデックスは作りません。

```text
rg --json → 前後5行の候補 → 近接領域を統合 → Jev Noul → 上位10件
```

参照会話の v0 を実装しています。言語ごとの AST、関数単位の展開、二段階 rerank、MCP サーバーは対象外です。

## セットアップ

Python 3.11 以上と `rg` が必要です。実行時の Python 外部依存はありません。

```bash
# macOS で rg が未導入の場合
brew install ripgrep

# リポジトリ内で利用
uv sync
uv run jrg --help

# または CLI としてインストール
uv tool install .
# pip install . も利用できます
```

インストールせず `python3 -m jrg ...` でも実行できます。

## まずローカルで試す

```bash
python3 -m jrg retry examples/retry_demo \
  --about 'HTTPリクエスト失敗時に、ステータスによって再試行するか判断する処理' \
  --dry-run --json --stats
```

`--dry-run` は候補一覧を表示します。通信せず、API キーも不要です。`relevance` は `null` で、`--top` と `--min-relevance` は適用しません。API に送る候補を一通り確認できます。

通常の検索では、検索意図・候補のパス・行番号・ソース断片を TypeSafe API に送ります。

```bash
# 自分のキーを環境変数に設定します。.env の自動読み込みはしません。
export TYPESAFE_API_KEY='your-api-key'

python3 -m jrg retry examples/retry_demo \
  --about 'HTTPリクエスト失敗時に、ステータスによって再試行するか判断する処理' \
  --top 5 --stats
```

`.env` に `TYPESAFE_API_KEY` を設定した場合は、uv に明示的に読み込ませて実行できます。`.env` は Git の追跡対象から除外しています。

```bash
uv run --env-file .env jrg retry examples/retry_demo \
  --about 'HTTPリクエスト失敗時に、ステータスによって再試行するか判断する処理' \
  --json --stats
```

サンプルには、再試行の可否を決める `policy.py` と、キーワードだけが重なる `metrics.py` があります。実際の順位・確率は Jev の判定によります。

## エージェントから使う

```bash
jrg 'retry|backoff|429' src \
  --about '一時的なHTTPエラーで再試行する条件を決めている実装' \
  --json --top 8 --min-relevance 0.5
```

stdout は JSON 配列のみ、警告・エラー・任意の統計は stderr に出力します。結果の例（確率は説明用）:

```json
[
  {
    "path": "src/policy.py",
    "start_line": 1,
    "end_line": 2,
    "match_lines": [1],
    "snippet": "def should_retry(status):\n    return status == 429 or status >= 500\n",
    "relevance": 0.97
  }
]
```

`start_line` / `end_line` は両端を含む1始まりの行番号です。`match_lines` は一致があった行で、同じ行の複数一致は1行として扱います。パスは `rg` の出力を保持します（相対パスには `./` が付く場合があります）。JSON 内の日本語は Unicode エスケープされますが、JSON パーサーで元に戻ります。

テキスト出力はスコア、パス、行範囲、ソースを表示し、一致行を `>` で示します。

終了コード:

| コード | 意味 |
| --- | --- |
| `0` | 結果あり |
| `1` | キーワード一致なし、またはしきい値以上の結果なし |
| `2` | 引数・検索・API エラー |
| `130` | 中断 |

## 主なオプション

| オプション | 既定値 | 用途 |
| --- | --- | --- |
| `--about` / `--intent` | 必須 | 探している処理の説明 |
| `-C`, `--context` | `5` | 一致行の前後の行数 |
| `--top` | `10` | 返す上位件数 |
| `--min-relevance` | `0` | Noul の下限。指定値を含む |
| `--max-candidates` | `200` | 評価する候補数の上限 |
| `--max-filesize` | `1M` | `rg` に渡すファイルサイズ制限 |
| `-g`, `--glob` | なし | `rg` の glob。複数指定可 |
| `-t`, `--type` | なし | `rg` の言語タイプ。複数指定可 |
| `-i`, `-F`, `--hidden` | 無効 | 大小文字無視、リテラル検索、隠しファイルを含める |
| `--json`, `--stats` | 無効 | JSON 結果、stderr の統計 |
| `--dry-run` | 無効 | API を呼ばず候補を表示 |
| `--batch-size` | `8` | 1リクエストの候補数上限 |
| `--workers` | `4` | API の最大同時リクエスト数 |
| `--timeout` | `30` | HTTP 通信のタイムアウト秒数 |
| `--retries` | `2` | 429 / 503 / 529 応答の最大再試行回数 |
| `--model` | `jev-latest` | TypeSafe のモデル名 |
| `--api-url` | `https://api.typesafe.ai/v1/systemone` | API エンドポイント。localhost のみ HTTP も可 |

しきい値を既定で `0` にするのは、未検証のしきい値で必要な候補を落とさないためです。最初は top-K の順位を評価し、実タスクでしきい値を調整してください。

`rg` の通常の ignore・隠しファイル・バイナリ除外に従います。ただし **明示的なファイル指定や正の `--glob` は rg 本来の仕様どおり ignore 等に優先**します。サイズ制限も明示的なファイル指定では適用されないことがあります。除外を優先したい場合はディレクトリ指定と `--type`、負の glob（`-g '!secrets/**'` など）を使ってください。`RIPGREP_CONFIG_PATH` は無効化しているため、個人設定の `--pre` などは実行されません。stdin 検索や任意の rg オプションの透過転送はありません。

## 候補と判定の設計

- `rg --json --context N --sort path` の出力をストリームで読み、同じファイルの連続した領域を統合します。ファイルを読み直さないので、検索直後の編集で一致位置と評価するソースが食い違うことを避けます。
- 密集した一致がファイル全体に広がらないよう、候補を最大120行または UTF-8 12,000 bytes で分割します。分割境界では前後の文脈が短くなります。1行がサイズ上限を超える場合や、非 UTF-8 のソースを含む場合は明示的なエラーにします。
- 候補上限を超えたことを確認した時点で `rg` を停止します。対象はパス・行順の先頭候補であり、全体の上位候補を保証しません。打ち切りは stderr と統計に明示します。
- 1リクエストに複数候補を入れ、それぞれに独立した Noul を割り当てます。質問の ID 自体はモデルに渡らないため、質問本文に `candidates[i].snippet` を明示します。
- 「検索意図の挙動を実装する、またはその理解に実質的に役立つか」を判定します。テストや文書も、それを探す意図なら関連ありにできます。候補間で確率を正規化する Choice は使いません。
- リクエストは質問文と JSON エスケープを含む24,000 bytes 以内に制限します。日本語やコードを一定の文字/token 比で見積もらず、保守的なサイズ上限を使います。単独候補でも収まらなければエラーになります。
- Noul の有限値 `0..1` を検証し、欠落や不正値があれば検索全体を失敗させます。同点はパス・開始行順に揃えます。API 障害から未評価結果への自動フォールバックは行いません。
- 過負荷応答を短い指数バックオフで再試行します。数値の `Retry-After` を尊重し、30秒超や日時形式なら再実行を案内します。接続失敗は、課金を伴う重複実行を避けるため自動再試行しません。

HTTP 契約は [TypeSafe API reference](https://docs.typesafe.ai/api)、判定型は [Noul](https://docs.typesafe.ai/primitives/noul)、複数質問は [Primitives](https://docs.typesafe.ai/primitives) に基づきます（2026-09-20 確認）。

## 評価と制約

`--stats` は候補数、一致行数、返却件数、候補/返却ソースの文字数、検索/rerank 時間、API リクエスト数（再試行を含む）、API が返した入力トークン数、応答モデル名を stderr の最後の JSON 行に出します。usage がない場合の `input_tokens` は `null` です。API 未使用なら `0` です。

文字数はソース断片だけの量です。LLM に渡す JSON 全体のトークン数や課金額を表すものではありません。比較実験では同じ検索意図・候補に対し、`--dry-run --json` と通常実行の結果を保存して、必要なコードが top-K に残るかを確認してください。

Jev は **rg が候補にしなかったコードを発見できません**。必要なら正規表現の選択肢や検索パスを増やしてください。Noul は指定した質問への Yes 確率であり、コード検索での正解率の保証ではありません。

## 開発・テスト

```bash
python3 -m unittest discover -v
uvx ruff check .
uvx ruff format --check .
uv build
```

テストは実際の `rg` とローカル HTTP サーバーで、候補統合、上限、ignore、API 契約、順位付け、しきい値、再試行、異常応答、CLI の終了コードを検証します。API キーは不要で、実サービスへの通信はありません。Jev 本番環境での疎通・検索品質は、自分のキーと実タスクで別途確認してください。

```text
jrg/search.py  候補の作成と rg プロセス管理
jrg/jev.py     Noul リクエスト、バッチ化、順位付け
jrg/cli.py     引数、出力、統計
```
