# Hermes Agent (Rust)

**[English](./README.md)** | **[中文](./README_ZH.md)** | **[日本語](./README_JA.md)** | **[한국어](./README_KO.md)**

[Hermes Agent](https://github.com/NousResearch/hermes-agent) のプロダクショングレード Rust 書き直し — [Nous Research](https://nousresearch.com) による自己進化型 AI エージェント。

**84,000+ 行の Rust コード · 16 クレート · 641 テスト · 17 プラットフォームアダプタ · 30 ツールバックエンド · 8 メモリプラグイン · 6 クロスプラットフォームリリースターゲット**

---

## Python v2026.4.13 アラインメント状況

ベースライン対象：`NousResearch/hermes-agent@v2026.4.13`（`1af2e18d408a9dcc2c61d6fc1eef5c6667f8e254`）。

- 進捗：スコープ内のアラインメント項目 **10 / 13** 完了。
- 完了済みの重点：プロンプト層/コア guidance の整合、スマートルーティングの基本ランタイム切替とフォールバック、memory ツールの意味論と容量制限、`MEMORY.md`/`USER.md` スナップショット注入、memory ライフサイクルフック（`on_memory_write`、`queue_prefetch`、`on_pre_compress`、`on_session_end`、`on_delegation`）、`session_search` の二重モードと `role_filter`/limit 整合。
- 残りの重点：`resolve_turn_route` ランタイム署名フィールドの完全整合、Python 方式の skills+memory 駆動自己進化ループの整合。

### TODO（パリティ追跡）

- [x] Long Memory：内蔵 memory action/target 意味論 + 文字数上限。
- [x] Long Memory：セッション開始時の memory スナップショット注入。
- [x] Long Memory：ライフサイクルフック（`on_memory_write`、`on_pre_compress`、`on_session_end`、`on_delegation`）。
- [x] Session Search：recent モード（空 query）、キーワードモード、`role_filter`、`limit <= 5`。
- [x] Session Search：子セッション→親セッション正規化（parent session 解決）。
- [x] Session Search：Python 相当のセッション単位 LLM 要約パイプライン。
- [x] Session Search：hidden/internal source フィルタリング整合。
- [x] Session Search：実行時コンテキストから現在セッション lineage を自動注入/除外。
- [x] Smart Model Selection：ターンごとの cheap-route と policy recommendation route。
- [x] Smart Model Selection：ルート先 provider 構築失敗時に primary provider へフォールバック。
- [ ] Smart Model Selection：Python `resolve_turn_route` 完全ランタイム署名面（`api_mode`、`command`、`args`、`credential_pool`、`signature`）の E2E 整合。
- [ ] Self-Evolution：Python 方式 memory/skills 駆動の自動適応ループ整合。
- [ ] Self-Evolution：Python `v2026.4.13` 振る舞いフィクスチャとのパリティ検証テスト。

### 機能実装ステータス（要求チェックリスト）

凡例：`implemented` = 現在のコードベースで利用可能、`partial` = 実装はあるが要求表現/挙動と完全一致ではない。

| 機能 | ステータス | 備考 |
|---|---|---|
| Interactive CLI + one-shot（`crates/hermes-cli`） | implemented | TUI 対話モード + `chat --query` one-shot 経路あり。 |
| Agent loop：ストリーミング + ツール実行 + コンテキスト圧縮 | implemented | `run_stream`、並列ツール実行、自動圧縮を実装。 |
| Prompt caching | partial | SystemPromptBuilder のキャッシュはあるが、完全なクロスリクエスト挙動は未完了。 |
| Provider：Anthropic / OpenAI chat-compatible / OpenAI Responses / OpenRouter-compatible | implemented | `hermes-agent`/`api_bridge`/拡張 provider で実装済み。 |
| 組み込みツール：file/terminal/patch/memory/web/vision + opt-in code execution | implemented | ツールセットで網羅。コード実行は policy/toolset 制御前提。 |
| 設定済み stdio/HTTP サーバーからの Runtime MCP ツール発見 | implemented | MCP client は stdio/http 設定と runtime tools/list をサポート。 |
| prompts/resources 用 MCP bridge（capability gating 付き） | partial | prompts/resources API は存在。厳密な capability-gated bridge は継続整備中。 |
| ローカル memory スナップショット + リクエスト単位の skill マッチ/注入 | implemented | `MEMORY.md`/`USER.md` 注入と skills prompt 編成を実装。 |
| SQLite セッション履歴 + resume | implemented | `sessions.db` 永続化とセッション復元フローあり。 |
| マルチモデル（OpenAI/Anthropic/OpenRouter） | implemented | ルーティング/provider スタックで対応。 |
| 内蔵ツール数（要求 26） | implemented | Rust 側は既にそれ以上（30+ backend）。 |
| TUI：対話チャット + 30+ slash コマンド + ツール進捗 + ステータスバー | implemented | TUI・ステータスバー・多数 slash 処理を実装。 |
| コンテキスト自動ロード（`AGENTS.md`/`CLAUDE.md`/`MEMORY.md`/`USER.md`） | implemented | context file loader と memory snapshot loader あり。 |
| メモリシステム：SQLite + FTS5 + クロスセッション永続化 | implemented | セッション永続化 + FTS `session_search` を実装。 |
| Skills システム：YAML ベース作成/管理 | implemented | skills ツールチェーン + store/hub を実装。 |
| Personality：coder/writer/analyst 切替 | partial | 切替機能は実装済み。具体ペルソナはローカル personality ファイル依存。 |
| コンテキスト圧縮：自動 + 手動 | implemented | loop 自動圧縮 + 手動 slash 経路あり。 |
| サブエージェント委任 | partial | `delegate_task` と委任フックは実装。完全自律オーケストレーションは発展中。 |
| Messaging：Telegram/Discord/Slack API | implemented | gateway プラットフォームアダプタあり。 |
| セキュリティ：パス検証、危険コマンド遮断、検索深さ制限 | partial | コマンド承認と credential/file ガードあり。全要求軸の完全整合は継続中。 |
| 中国語入力：TUI UTF-8 フル対応 | implemented | Rust/TUI 経路で UTF-8 入出力を処理可能。 |

## ハイライト

### シングルバイナリ、依存関係ゼロ

~16MB のバイナリ一つ。Python、pip、virtualenv、Docker 不要。Raspberry Pi、$3/月 VPS、エアギャップサーバー、Docker scratch イメージで動作。

```bash
scp hermes user@server:~/
./hermes
```

### 自己進化ポリシーエンジン

エージェントが自身の実行から学習する。3 層の適応システム：

- **L1 — モデル＆リトライチューニング。** マルチアームドバンディットが履歴の成功率・レイテンシ・コストに基づきタスクごとに最適モデルを選択。リトライ戦略はタスクの複雑さに応じて動的に調整。
- **L2 — 長タスク計画。** 複雑なプロンプトに対して並列度、サブタスク分割、チェックポイント間隔を自動決定。
- **L3 — プロンプト＆メモリシェイピング。** システムプロンプトとメモリコンテキストを蓄積されたフィードバックに基づきリクエストごとに最適化・トリミング。

カナリアロールアウト、ハードゲートロールバック、監査ログ付きのポリシーバージョニング。手動チューニングなしでエンジンが時間とともに改善。

### 真の並行性

Rust の tokio ランタイムが真の並列実行を提供 — Python の協調的 asyncio ではない。`JoinSet` がツール呼び出しを OS スレッドにディスパッチ。30 秒のブラウザスクレイプが 50ms のファイル読み取りをブロックしない。ゲートウェイは GIL なしで 17 プラットフォームのメッセージを同時処理。

### 17 プラットフォームアダプタ

Telegram、Discord、Slack、WhatsApp、Signal、Matrix、Mattermost、DingTalk、Feishu、WeCom、Weixin、Email、SMS、BlueBubbles、Home Assistant、Webhook、API Server。

### 30 ツールバックエンド

ファイル操作、ターミナル、ブラウザ、コード実行、Web 検索、ビジョン、画像生成、TTS、文字起こし、メモリ、メッセージング、委任、cron ジョブ、スキル、セッション検索、Home Assistant、RL トレーニング、URL 安全性チェック、OSV 脆弱性チェックなど。
内蔵 `memory` ツールは Python パリティ意味論（`action=add|replace|remove`、`target=memory|user`、replace/remove の `old_text` 部分一致）に対応。
内蔵 memory ストア上限も Python 既定と整合：`memory` ≈ 2200 文字、`user` ≈ 1375 文字。
内蔵 `session_search` は Python 方式の二重モードに対応：`query` 省略で recent ブラウズ、`query` 指定でキーワード検索。`role_filter` 対応、`limit` 上限は 5。
`session_search` は補助 LLM 認証がある場合にセッション単位要約を実行可能（`HERMES_SESSION_SEARCH_SUMMARY_API_KEY` または `OPENAI_API_KEY`、任意で base/model 上書き）。

### 8 メモリプラグイン

Mem0、Honcho、Holographic、Hindsight、ByteRover、OpenViking、RetainDB、Supermemory。

### 6 ターミナルバックエンド

Local、Docker、SSH、Daytona、Modal、Singularity。

### MCP（Model Context Protocol）サポート

組み込み MCP クライアントとサーバー。外部ツールプロバイダに接続、または Hermes ツールを他の MCP 互換エージェントに公開。

### ACP（Agent Communication Protocol）

セッション管理、イベントストリーミング、権限制御付きのエージェント間通信。

---

## アーキテクチャ

### 16 クレートのワークスペース

```
crates/
├── hermes-core           # 共有型、trait、エラー階層
├── hermes-agent          # エージェントループ、LLM プロバイダ、コンテキスト、メモリプラグイン
├── hermes-tools          # ツールレジストリ、ディスパッチ、30 ツールバックエンド
├── hermes-gateway        # メッセージゲートウェイ、17 プラットフォームアダプタ
├── hermes-cli            # CLI/TUI バイナリ、スラッシュコマンド
├── hermes-config         # 設定読み込み、マージ、YAML 互換
├── hermes-intelligence   # 自己進化エンジン、モデルルーティング、プロンプト構築
├── hermes-skills         # スキル管理、ストア、セキュリティガード
├── hermes-environments   # ターミナルバックエンド
├── hermes-cron           # Cron スケジューリングと永続化
├── hermes-mcp            # Model Context Protocol クライアント/サーバー
├── hermes-acp            # Agent Communication Protocol
├── hermes-rl             # 強化学習ラン
├── hermes-http           # HTTP/WebSocket API サーバー
├── hermes-auth           # OAuth トークン交換
└── hermes-telemetry      # OpenTelemetry 統合
```

### Trait ベースの抽象化

| Trait | 目的 | 実装 |
|-------|------|------|
| `LlmProvider` | LLM API 呼び出し | OpenAI, Anthropic, OpenRouter, Generic |
| `ToolHandler` | ツール実行 | 30 ツールバックエンド |
| `PlatformAdapter` | メッセージプラットフォーム | 17 プラットフォーム |
| `TerminalBackend` | コマンド実行 | Local, Docker, SSH, Daytona, Modal, Singularity |
| `MemoryProvider` | 永続メモリ | 8 メモリプラグイン + ファイル/SQLite |
| `SkillProvider` | スキル管理 | ファイルストア + Hub |

---

## インストール

プラットフォームに対応する最新リリースバイナリをダウンロード：

```bash
# macOS (Apple Silicon)
curl -LO https://github.com/Lumio-Research/hermes-agent-rs/releases/latest/download/hermes-macos-aarch64.tar.gz
tar xzf hermes-macos-aarch64.tar.gz && sudo mv hermes /usr/local/bin/

# macOS (Intel)
curl -LO https://github.com/Lumio-Research/hermes-agent-rs/releases/latest/download/hermes-macos-x86_64.tar.gz
tar xzf hermes-macos-x86_64.tar.gz && sudo mv hermes /usr/local/bin/

# Linux (x86_64)
curl -LO https://github.com/Lumio-Research/hermes-agent-rs/releases/latest/download/hermes-linux-x86_64.tar.gz
tar xzf hermes-linux-x86_64.tar.gz && sudo mv hermes /usr/local/bin/

# Linux (ARM64)
curl -LO https://github.com/Lumio-Research/hermes-agent-rs/releases/latest/download/hermes-linux-aarch64.tar.gz
tar xzf hermes-linux-aarch64.tar.gz && sudo mv hermes /usr/local/bin/
```

全リリースバイナリ：https://github.com/Lumio-Research/hermes-agent-rs/releases

## ソースからビルド

```bash
cargo build --release
```

## 実行

```bash
hermes              # インタラクティブチャット
hermes --help       # 全コマンド
hermes gateway start  # マルチプラットフォームゲートウェイ起動
hermes doctor       # 依存関係と設定チェック
```

## テスト

```bash
cargo test --workspace   # 641 テスト
```

## ライセンス

MIT — [LICENSE](LICENSE) 参照。

[Nous Research](https://nousresearch.com) の [Hermes Agent](https://github.com/NousResearch/hermes-agent) に基づく。
