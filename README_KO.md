# Hermes Agent (Rust)

**[English](./README.md)** | **[中文](./README_ZH.md)** | **[日本語](./README_JA.md)** | **[한국어](./README_KO.md)**

[Hermes Agent](https://github.com/NousResearch/hermes-agent)의 프로덕션 그레이드 Rust 재작성 — [Nous Research](https://nousresearch.com)의 자기 진화형 AI 에이전트.

**84,000+ 줄 Rust 코드 · 16개 크레이트 · 641개 테스트 · 17개 플랫폼 어댑터 · 30개 도구 백엔드 · 8개 메모리 플러그인 · 6개 크로스 플랫폼 릴리스 타겟**

---

## Python v2026.4.13 정렬 상태

기준 베이스라인: `NousResearch/hermes-agent@v2026.4.13` (`1af2e18d408a9dcc2c61d6fc1eef5c6667f8e254`).

- 진행률: 스코프 내 정렬 항목 **10 / 13** 완료.
- 완료된 핵심 영역: 프롬프트 레이어/핵심 가이던스 정렬, 스마트 라우팅 기본 런타임 전환 및 폴백, memory 도구 시맨틱과 용량 제한, 내장 `MEMORY.md`/`USER.md` 스냅샷 주입, memory 라이프사이클 훅(`on_memory_write`, `queue_prefetch`, `on_pre_compress`, `on_session_end`, `on_delegation`), `session_search` 이중 모드 + `role_filter`/limit 정렬.
- 남은 핵심 영역: `resolve_turn_route` 런타임 시그니처 필드 완전 정렬, Python 스타일 skills+memory 기반 자기진화 루프 정렬.

### TODO (패리티 트래커)

- [x] Long Memory: 내장 memory action/target 시맨틱 + 문자 수 제한.
- [x] Long Memory: 세션 시작 시 memory 스냅샷 프롬프트 주입.
- [x] Long Memory: 라이프사이클 훅(`on_memory_write`, `on_pre_compress`, `on_session_end`, `on_delegation`).
- [x] Session Search: recent 모드(빈 query), 키워드 모드, `role_filter`, `limit <= 5`.
- [x] Session Search: child->parent lineage 정규화 지원(parent session 해석).
- [x] Session Search: Python 동등의 세션별 LLM 요약 파이프라인.
- [x] Session Search: hidden/internal source 필터링 규칙 정렬.
- [x] Session Search: 런타임 컨텍스트 기반 현재 active session lineage 자동 주입/제외.
- [x] Smart Model Selection: 턴 단위 cheap-route 및 policy recommendation route.
- [x] Smart Model Selection: 라우팅 provider 생성 실패 시 primary provider 폴백.
- [ ] Smart Model Selection: Python `resolve_turn_route` 전체 런타임 시그니처 표면(`api_mode`, `command`, `args`, `credential_pool`, `signature`) E2E 정렬.
- [ ] Self-Evolution: Python 스타일 memory/skills 기반 자동 적응 루프 정렬.
- [ ] Self-Evolution: Python `v2026.4.13` 동작 기준 parity 검증 테스트.

### 기능 구현 상태 (요청 체크리스트)

상태 범례: `implemented` = 현재 코드베이스에서 사용 가능, `partial` = 구현은 있으나 요청 문구/동작과 완전 동일하진 않음.

| 기능 | 상태 | 메모 |
|---|---|---|
| Interactive CLI + one-shot (`crates/hermes-cli`) | implemented | TUI 상호작용 모드 + `chat --query` one-shot 경로 존재. |
| Agent loop: 스트리밍 + 도구 실행 + 컨텍스트 압축 | implemented | `run_stream`, 병렬 도구 실행, 자동 압축 구현. |
| Prompt caching | partial | SystemPromptBuilder 캐시는 있으나 완전한 cross-request 캐시 동작은 추가 정렬 필요. |
| Provider: Anthropic, OpenAI chat-compatible, OpenAI Responses, OpenRouter-compatible | implemented | `hermes-agent`/`api_bridge`/추가 provider 어댑터로 지원. |
| 내장 도구: 파일/터미널/패치/메모리/웹/비전 + opt-in 코드 실행 | implemented | 해당 카테고리 모두 포함, 코드 실행은 policy/toolset 제어 전제. |
| 구성된 stdio/HTTP 서버에서 Runtime MCP 도구 발견 | implemented | MCP client가 stdio/http 설정과 runtime tools/list 지원. |
| prompts/resources용 MCP 브리지 + capability gating | partial | prompts/resources API는 존재, 엄격 capability-gated bridge는 계속 정리 중. |
| 로컬 메모리 스냅샷 + 요청 단위 스킬 매칭/주입 | implemented | `MEMORY.md`/`USER.md` 스냅샷 주입 + skills prompt 오케스트레이션 적용. |
| SQLite 세션 히스토리 + resume | implemented | `sessions.db` 영속화 및 세션 복원 플로우 존재. |
| 멀티 모델 지원(OpenAI/Anthropic/OpenRouter) | implemented | 라우팅/provider 스택에서 지원. |
| 내장 도구 수(요청 26개) | implemented | Rust 현재 이미 그 이상(30+ 도구 백엔드). |
| TUI: 대화형 채팅 + 30+ slash 명령 + 도구 진행 표시 + 상태바 | implemented | TUI/상태바/다수 slash 핸들러 구현됨. |
| 컨텍스트 자동 로드(`AGENTS.md`, `CLAUDE.md`, `MEMORY.md`, `USER.md`) | implemented | 컨텍스트 파일 로더 + memory 스냅샷 로더 존재. |
| 메모리 시스템: SQLite + FTS5 + 크로스 세션 영속성 | implemented | 세션 영속화 + FTS 기반 `session_search` 구현. |
| Skills 시스템: YAML 기반 생성/관리 | implemented | skills 툴체인 + skill store/hub 구현. |
| Personality 시스템: coder/writer/analyst 전환 | partial | 전환 기능 구현, 구체 페르소나는 로컬 personality 파일 의존. |
| 컨텍스트 압축: 자동 + 수동 | implemented | loop 자동 압축 + 수동 slash 경로 존재. |
| 서브 에이전트 위임 | partial | `delegate_task` 및 위임 훅 구현, 완전 자율 child-agent 오케스트레이션은 발전 중. |
| 메시징: Telegram/Discord/Slack API | implemented | gateway 플랫폼 어댑터 구현. |
| 보안: 경로 검증, 위험 명령 차단, 검색 깊이 제한 | partial | 명령 승인/credential-file 가드는 구현, 일부 보안 축은 추가 정렬 중. |
| 중국어 입력: TUI UTF-8 완전 지원 | implemented | Rust/TUI 경로에서 UTF-8 입출력 처리 가능. |

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
내장 `memory` 도구는 Python 패리티 시맨틱(`action=add|replace|remove`, `target=memory|user`, replace/remove의 `old_text` 부분 일치)을 따릅니다.
내장 memory 저장소 제한도 Python 기본값과 정렬되어 있습니다: `memory` ≈ 2200자, `user` ≈ 1375자.
내장 `session_search`는 Python 스타일 이중 모드를 지원합니다: `query` 생략 시 최근 세션 탐색, `query` 지정 시 키워드 검색. `role_filter` 지원, `limit` 상한 5.
`session_search`는 보조 LLM 자격 증명이 있으면 세션 단위 LLM 요약을 수행할 수 있습니다(`HERMES_SESSION_SEARCH_SUMMARY_API_KEY` 또는 `OPENAI_API_KEY`, 선택적 base/model override).

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
