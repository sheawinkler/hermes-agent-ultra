# Hermes Agent (Rust)

**[English](./README.md)** | **[中文](./README_ZH.md)** | **[日本語](./README_JA.md)** | **[한국어](./README_KO.md)**

[Hermes Agent](https://github.com/NousResearch/hermes-agent)의 프로덕션 그레이드 Rust 재작성 — [Nous Research](https://nousresearch.com)의 자기 진화형 AI 에이전트.

**84,000+ 줄 Rust 코드 · 16개 크레이트 · 641개 테스트 · 17개 플랫폼 어댑터 · 30개 도구 백엔드 · 8개 메모리 플러그인 · 6개 크로스 플랫폼 릴리스 타겟**

---

## 하이라이트

### 단일 바이너리, 의존성 제로

~16MB 바이너리 하나. Python, pip, virtualenv, Docker 불필요. Raspberry Pi, $3/월 VPS, 에어갭 서버, Docker scratch 이미지에서 실행.

```bash
scp hermes user@server:~/
./hermes
```

### 자기 진화 정책 엔진

에이전트가 자체 실행에서 학습. 3계층 적응 시스템:

- **L1 — 모델 & 재시도 튜닝.** 멀티 암드 밴딧이 과거 성공률·지연시간·비용 기반으로 태스크별 최적 모델 선택. 재시도 전략은 태스크 복잡도에 따라 동적 조정.
- **L2 — 장기 태스크 계획.** 복잡한 프롬프트에 대해 병렬도, 서브태스크 분할, 체크포인트 간격 자동 결정.
- **L3 — 프롬프트 & 메모리 셰이핑.** 시스템 프롬프트와 메모리 컨텍스트를 축적된 피드백 기반으로 요청별 최적화 및 트리밍.

카나리 롤아웃, 하드 게이트 롤백, 감사 로깅이 포함된 정책 버전 관리. 수동 튜닝 없이 엔진이 시간에 따라 개선.

### 진정한 동시성

Rust의 tokio 런타임이 진정한 병렬 실행 제공 — Python의 협력적 asyncio가 아닌. `JoinSet`이 도구 호출을 OS 스레드에 디스패치. 30초 브라우저 스크래핑이 50ms 파일 읽기를 차단하지 않음. 게이트웨이가 GIL 없이 17개 플랫폼 메시지를 동시 처리.

### 17개 플랫폼 어댑터

Telegram, Discord, Slack, WhatsApp, Signal, Matrix, Mattermost, DingTalk, Feishu, WeCom, Weixin, Email, SMS, BlueBubbles, Home Assistant, Webhook, API Server.

### 30개 도구 백엔드

파일 작업, 터미널, 브라우저, 코드 실행, 웹 검색, 비전, 이미지 생성, TTS, 음성 전사, 메모리, 메시징, 위임, cron 작업, 스킬, 세션 검색, Home Assistant, RL 훈련, URL 안전성 검사, OSV 취약점 검사 등.

### 8개 메모리 플러그인

Mem0, Honcho, Holographic, Hindsight, ByteRover, OpenViking, RetainDB, Supermemory.

### 6개 터미널 백엔드

Local, Docker, SSH, Daytona, Modal, Singularity.

### MCP (Model Context Protocol) 지원

내장 MCP 클라이언트 및 서버. 외부 도구 제공자에 연결하거나 Hermes 도구를 다른 MCP 호환 에이전트에 노출.

### ACP (Agent Communication Protocol)

세션 관리, 이벤트 스트리밍, 권한 제어가 포함된 에이전트 간 통신.

---

## 아키텍처

### 16개 크레이트 워크스페이스

```
crates/
├── hermes-core           # 공유 타입, trait, 에러 계층
├── hermes-agent          # 에이전트 루프, LLM 프로바이더, 컨텍스트, 메모리 플러그인
├── hermes-tools          # 도구 레지스트리, 디스패치, 30개 도구 백엔드
├── hermes-gateway        # 메시지 게이트웨이, 17개 플랫폼 어댑터
├── hermes-cli            # CLI/TUI 바이너리, 슬래시 명령
├── hermes-config         # 설정 로딩, 병합, YAML 호환
├── hermes-intelligence   # 자기 진화 엔진, 모델 라우팅, 프롬프트 구축
├── hermes-skills         # 스킬 관리, 스토어, 보안 가드
├── hermes-environments   # 터미널 백엔드
├── hermes-cron           # Cron 스케줄링 및 영속화
├── hermes-mcp            # Model Context Protocol 클라이언트/서버
├── hermes-acp            # Agent Communication Protocol
├── hermes-rl             # 강화 학습 실행
├── hermes-http           # HTTP/WebSocket API 서버
├── hermes-auth           # OAuth 토큰 교환
└── hermes-telemetry      # OpenTelemetry 통합
```

### Trait 기반 추상화

| Trait | 목적 | 구현 |
|-------|------|------|
| `LlmProvider` | LLM API 호출 | OpenAI, Anthropic, OpenRouter, Generic |
| `ToolHandler` | 도구 실행 | 30개 도구 백엔드 |
| `PlatformAdapter` | 메시징 플랫폼 | 17개 플랫폼 |
| `TerminalBackend` | 명령 실행 | Local, Docker, SSH, Daytona, Modal, Singularity |
| `MemoryProvider` | 영구 메모리 | 8개 메모리 플러그인 + 파일/SQLite |
| `SkillProvider` | 스킬 관리 | 파일 스토어 + Hub |

---

## 설치

플랫폼에 맞는 최신 릴리스 바이너리 다운로드:

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

전체 릴리스 바이너리: https://github.com/Lumio-Research/hermes-agent-rs/releases

## 소스에서 빌드

```bash
cargo build --release
```

## 실행

```bash
hermes              # 대화형 채팅
hermes --help       # 모든 명령
hermes gateway start  # 멀티 플랫폼 게이트웨이 시작
hermes doctor       # 의존성 및 설정 확인
```

## 테스트

```bash
cargo test --workspace   # 641개 테스트
```

## 라이선스

MIT — [LICENSE](LICENSE) 참조.

[Nous Research](https://nousresearch.com)의 [Hermes Agent](https://github.com/NousResearch/hermes-agent) 기반.
