# Hermes Agent (Rust)

**[English](./README.md)** | **[中文](./README_ZH.md)** | **[日本語](./README_JA.md)** | **[한국어](./README_KO.md)**

[Hermes Agent](https://github.com/NousResearch/hermes-agent) の Rust 書き直し — [Nous Research](https://nousresearch.com) による自己進化型 AI エージェント。

---

## なぜ Rust なのか？本当の価値

### Python AI エージェントの限界

今日の Python AI エージェントには公然の秘密がある：すべて**シングルユーザーのおもちゃ**だということだ。$5 の VPS で動かし、Telegram + Discord + Slack を接続すれば、10 の同時会話で崩壊する。Python の GIL、asyncio の協調スケジューリング、あらゆる場所の `Dict[str, Any]` は以下を意味する：

- **一つの会話が詰まると全部詰まる。** あるセッションの遅いツール呼び出しが、全員のイベントループをフリーズさせる。
- **メモリ膨張。** 各会話は辞書の辞書を持ち、データの実際の形状を知る方法がない。50 セッションのゲートウェイは簡単に 2GB+ の RAM を消費する。
- **サイレント破損。** キー名のタイプミス（`"message"` の代わりに `"mesage"`）が全レイヤーを検出されずに通過し、LLM API に到達してゴミを返す。
- **デプロイの摩擦。** 40+ の依存関係を持つ `pip install`、バージョン競合、プラットフォーム固有の wheel（ARM Linux で `faster-whisper` をインストールしてみてほしい）、500MB の virtualenv。

これらはバグではない。言語の天井だ。

### Rust が実際に変えること

**1. シングルバイナリ、依存関係ゼロ**

```bash
# Python：ターゲットに Python 3.11+、pip、venv、互換 wheel があることを祈る
curl -fsSL install.sh | bash  # 500MB+ インストール

# Rust：15MB のバイナリ一つ、どこでも動く
scp hermes user@server:~/
./hermes
```

これが最大のデプロイ上の利点だ。Raspberry Pi、$3/月の VPS、エアギャップサーバー、他に何もない Docker scratch イメージで動く AI エージェント。ランタイムなし、インタプリタなし、依存関係地獄なし。エッジ AI、IoT、Python をインストールできないエンタープライズ環境では、これが唯一の道だ。

**2. 本物の並行性、見せかけの並行性ではない**

Python の asyncio は協調的だ — CPU 作業を行う一つの行儀の悪いツール呼び出し（10MB レスポンスの JSON パース、正規表現マッチング、コンテキスト圧縮）がすべてをブロックする。Rust の tokio は以下を提供する：

- **真の並列ツール実行。** `JoinSet` がツール呼び出しを OS スレッドに分散。30 秒のブラウザスクレイプが 50ms のファイル読み取りをブロックしない。
- **ロックフリーのメッセージルーティング。** ゲートウェイは GIL なしで 16 プラットフォームからの受信メッセージを同時に処理できる。
- **予測可能なレイテンシ。** GC 一時停止なし。ストリーミング中に Python がガベージコレクションを決定して突然 200ms フリーズすることがない。

100+ の同時会話を処理するマルチユーザーゲートウェイにとって、これは「動く」と「確実に動く」の違いだ。

**3. コンパイラがアーキテクチャの番人**

Python コードベースには 9,913 行の `run_agent.py` と 7,905 行の `gateway/run.py` がある。Python にはそれを防ぐメカニズムがないため、これらのファイルは有機的に成長した。どのファイルも何でもインポートできる。どの関数もどのグローバルも変更できる。型チェッカーはオプションで、日常的に無視される。

Rust の crate システムはこれを物理的に不可能にする：

```
hermes-core          ← trait を定義、誰のものでもない
hermes-agent         ← core に依存、gateway は見えない
hermes-gateway       ← core に依存、agent の内部は見えない
hermes-tools         ← core に依存、provider の詳細は見えない
```

循環依存？コンパイルエラー。エラー処理の忘れ？コンパイルエラー。ツールに間違ったメッセージ型を渡す？コンパイルエラー。これは規律ではない — 物理法則だ。コンパイラが許さないため、アーキテクチャは時間とともに劣化できない。

**4. 最も重要な場所での型安全性**

Python 版では、「メッセージ」は `Dict[str, Any]`。「ツール呼び出し」は `Dict[str, Any]`。「設定」は `Dict[str, Any]`。何か問題が起きると、午前 3 時の本番環境で `KeyError` を受け取る。

Rust では：

```rust
pub struct Message {
    pub role: MessageRole,        // enum、文字列ではない
    pub content: Option<String>,
    pub tool_calls: Option<Vec<ToolCall>>,
    pub reasoning_content: Option<ReasoningContent>,
    pub cache_control: Option<CacheControl>,
}
```

すべてのフィールドに型がある。すべてのバリアントが列挙される。すべてのエラーパスが処理される。LLM が予期しない JSON を返す？`serde` が境界でキャッチする。ツールハンドラが間違った型を返す？コンパイルが通らない。これは本番環境の Python エージェントを悩ませるバグの一クラス全体を排除する。

**5. 長期実行エージェントのメモリ効率**

AI エージェントはリクエスト-レスポンスサーバーではない。数日、数週間、数ヶ月動く。会話履歴、スキルファイル、メモリエントリ、セッション状態を蓄積する。Python の参照カウント GC と dict のオーバーヘッドは、メモリが予測不能に増加することを意味する。

Rust の所有権モデルは以下を意味する：
- セッション終了の瞬間に会話履歴が解放される。GC 遅延なし。
- ツール結果はコンテキスト挿入後すぐに切り詰められ、ドロップされる。
- 100 の同時セッションの全エージェント状態が ~50MB に収まる。2GB ではない。

安い VPS で 24/7 動く個人 AI エージェントにとって、これは $3/月と $20/月の違いだ。

---

## アーキテクチャの決定

### Trait ベースの抽象化

すべての統合ポイントが trait：

| Trait | 目的 | 実装 |
|-------|------|------|
| `LlmProvider` | LLM API 呼び出し | OpenAI, Anthropic, OpenRouter, Generic |
| `ToolHandler` | ツール実行 | 18 種類のツール |
| `PlatformAdapter` | メッセージプラットフォーム | 16 プラットフォーム |
| `TerminalBackend` | コマンド実行 | Local, Docker, SSH, Daytona, Modal, Singularity |
| `MemoryProvider` | 永続メモリ | ファイル, SQLite |
| `SkillProvider` | スキル管理 | ファイルストア + Hub |

### エラー階層

```
AgentError（トップレベル）
├── LlmApi(String)
├── ToolExecution(String)      ← ToolError から自動変換
├── Gateway(String)            ← GatewayError から自動変換
├── Config(String)             ← ConfigError から自動変換
├── RateLimited { retry_after_secs }
├── Interrupted { message }
├── ContextTooLong
├── MaxTurnsExceeded
└── Io(String)
```

### ワークスペース構造

```
crates/
├── hermes-core           # 共有型、trait、エラー型
├── hermes-agent          # エージェントループ、プロバイダ、コンテキスト、メモリ
├── hermes-tools          # ツールレジストリ、ディスパッチ、全ツール実装
├── hermes-gateway        # メッセージゲートウェイ、プラットフォームアダプタ
├── hermes-cli            # CLI バイナリ、TUI、コマンド
├── hermes-config         # 設定の読み込みとマージ
├── hermes-intelligence   # プロンプト構築、モデルルーティング、使用量追跡
├── hermes-skills         # スキル管理、ストア、セキュリティガード
├── hermes-environments   # ターミナルバックエンド
├── hermes-cron           # Cron スケジューリング
└── hermes-mcp            # Model Context Protocol
```

---

## 競争優位性

AI エージェント分野は同じ天井にぶつかる Python プロジェクトで溢れている。生き残るのは以下ができるものだ：

1. **どこでも動く** — Python 3.11 と 40 の pip パッケージがある開発者の MacBook だけでなく、エッジデバイス、組み込みシステム、インターネットアクセスのないエンタープライズサーバー、$3 の VPS インスタンス。

2. **マルチユーザーにスケール** — 一つのプロセスでチーム、家族、コミュニティにサービスし、各会話が他を劣化させない。

3. **数ヶ月にわたり信頼性を維持** — メモリリークなし、GC 一時停止なし、長期実行セッションで静かに蓄積する型エラーなし。

4. **他のシステムに組み込める** — Rust ライブラリは C、C++、Python（PyO3）、Node.js（napi）、Go（CGo）、WASM から呼び出せる。Python エージェントは Python からしか呼び出せない。

---

## 現在の状態

初期段階。アーキテクチャとコア抽象は堅固。Python 版との機能パリティは約 10%。詳細は [GAP_ANALYSIS.md](./GAP_ANALYSIS.md) を参照。

## ビルド

```bash
cargo build --release
```

## 実行

```bash
cargo run --release -p hermes-cli
```

## テスト

```bash
cargo test --workspace
```

## ライセンス

MIT — [LICENSE](LICENSE) を参照。

[Nous Research](https://nousresearch.com) の [Hermes Agent](https://github.com/NousResearch/hermes-agent) に基づく。
