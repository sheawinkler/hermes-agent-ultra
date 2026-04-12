# Hermes Agent (Rust)

**[English](./README.md)** | **[中文](./README_ZH.md)** | **[日本語](./README_JA.md)** | **[한국어](./README_KO.md)**

[Hermes Agent](https://github.com/NousResearch/hermes-agent) のプロダクショングレード Rust 書き直し — [Nous Research](https://nousresearch.com) による自己進化型 AI エージェント。

**84,000+ 行の Rust コード · 16 クレート · 641 テスト · 17 プラットフォームアダプタ · 30 ツールバックエンド · 8 メモリプラグイン · 6 クロスプラットフォームリリースターゲット**

---

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
