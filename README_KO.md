# Hermes Agent (Rust)

**[English](./README.md)** | **[中文](./README_ZH.md)** | **[日本語](./README_JA.md)** | **[한국어](./README_KO.md)**

[Hermes Agent](https://github.com/NousResearch/hermes-agent)의 Rust 재작성 — [Nous Research](https://nousresearch.com)의 자기 진화형 AI 에이전트.

---

## 왜 Rust인가? 진짜 가치

### Python AI 에이전트의 한계

오늘날 Python AI 에이전트에는 공공연한 비밀이 있다: 모두 **싱글 유저 장난감**이라는 것이다. $5 VPS에서 하나를 실행하고 Telegram + Discord + Slack을 연결하면, 10개의 동시 대화에서 무너진다. Python GIL, asyncio의 협력적 스케줄링, 어디에나 있는 `Dict[str, Any]`는 다음을 의미한다:

- **하나의 대화가 막히면 전부 막힌다.** 한 세션의 느린 도구 호출이 모든 사람의 이벤트 루프를 동결시킨다.
- **메모리 팽창.** 각 대화는 딕셔너리의 딕셔너리를 가지며, 데이터의 실제 형태를 알 방법이 없다. 50개 세션의 게이트웨이는 쉽게 2GB+ RAM을 소비한다.
- **조용한 손상.** 키 이름 오타(`"message"` 대신 `"mesage"`)가 모든 레이어를 감지되지 않고 통과하여 LLM API에 도달해 쓰레기를 반환한다.
- **배포 마찰.** 40+ 의존성을 가진 `pip install`, 버전 충돌, 플랫폼별 wheel(ARM Linux에서 `faster-whisper` 설치를 시도해 보라), 500MB virtualenv.

이것들은 버그가 아니다. 언어의 천장이다.

### Rust가 실제로 바꾸는 것

**1. 단일 바이너리, 의존성 제로**

```bash
# Python: 대상에 Python 3.11+, pip, venv, 호환 wheel이 있기를 기도
curl -fsSL install.sh | bash  # 500MB+ 설치

# Rust: 15MB 바이너리 하나, 어디서든 실행
scp hermes user@server:~/
./hermes
```

이것이 가장 큰 배포 이점이다. Raspberry Pi, $3/월 VPS, 에어갭 서버, 아무것도 없는 Docker scratch 이미지에서 실행되는 AI 에이전트. 런타임 없음, 인터프리터 없음, 의존성 지옥 없음. 엣지 AI, IoT, Python을 설치할 수 없는 엔터프라이즈 환경에서는 이것이 유일한 길이다.

**2. 진짜 동시성, 가짜 동시성이 아닌**

Python의 asyncio는 협력적이다 — CPU 작업을 하는 하나의 나쁜 도구 호출(10MB 응답 JSON 파싱, 정규식 매칭, 컨텍스트 압축)이 모든 것을 차단한다. Rust의 tokio는 다음을 제공한다:

- **진정한 병렬 도구 실행.** `JoinSet`이 도구 호출을 OS 스레드에 분산. 30초 브라우저 스크래핑이 50ms 파일 읽기를 차단하지 않는다.
- **락프리 메시지 라우팅.** 게이트웨이가 GIL 없이 16개 플랫폼의 수신 메시지를 동시에 처리할 수 있다.
- **예측 가능한 지연시간.** GC 일시정지 없음. 스트리밍 중 Python이 가비지 컬렉션을 결정해 갑자기 200ms 동결되는 일 없음.

100+ 동시 대화를 처리하는 멀티유저 게이트웨이에게, 이것은 "작동한다"와 "안정적으로 작동한다"의 차이다.

**3. 컴파일러가 아키텍처 수호자**

Python 코드베이스에는 9,913줄의 `run_agent.py`와 7,905줄의 `gateway/run.py`가 있다. Python에는 이를 방지할 메커니즘이 없기 때문에 이 파일들은 유기적으로 성장했다. 어떤 파일이든 무엇이든 import할 수 있다. 어떤 함수든 어떤 전역 변수든 변경할 수 있다. 타입 체커는 선택사항이며 일상적으로 무시된다.

Rust의 crate 시스템은 이를 물리적으로 불가능하게 만든다:

```
hermes-core          ← trait 정의, 누구의 것도 아님
hermes-agent         ← core에 의존, gateway를 볼 수 없음
hermes-gateway       ← core에 의존, agent 내부를 볼 수 없음
hermes-tools         ← core에 의존, provider 세부사항을 볼 수 없음
```

순환 의존성? 컴파일 에러. 에러 처리 누락? 컴파일 에러. 도구에 잘못된 메시지 타입 전달? 컴파일 에러. 이것은 규율이 아니라 물리법칙이다. 컴파일러가 허용하지 않기 때문에 아키텍처는 시간이 지나도 퇴화할 수 없다.

**4. 가장 중요한 곳에서의 타입 안전성**

Python 버전에서 "메시지"는 `Dict[str, Any]`. "도구 호출"은 `Dict[str, Any]`. "설정"은 `Dict[str, Any]`. 문제가 생기면 새벽 3시 프로덕션 환경에서 `KeyError`를 받는다.

Rust에서는:

```rust
pub struct Message {
    pub role: MessageRole,        // enum, 문자열이 아님
    pub content: Option<String>,
    pub tool_calls: Option<Vec<ToolCall>>,
    pub reasoning_content: Option<ReasoningContent>,
    pub cache_control: Option<CacheControl>,
}
```

모든 필드에 타입이 있다. 모든 변형이 열거된다. 모든 에러 경로가 처리된다. LLM이 예상치 못한 JSON을 반환? `serde`가 경계에서 잡는다. 도구 핸들러가 잘못된 타입을 반환? 컴파일이 안 된다.

**5. 장기 실행 에이전트의 메모리 효율성**

AI 에이전트는 요청-응답 서버가 아니다. 며칠, 몇 주, 몇 달 동안 실행된다. Rust의 소유권 모델은 다음을 의미한다:
- 세션 종료 순간 대화 기록이 해제된다. GC 지연 없음.
- 도구 결과는 컨텍스트 삽입 후 즉시 잘리고 드롭된다.
- 100개 동시 세션의 전체 에이전트 상태가 ~50MB에 들어간다. 2GB가 아니다.

---

## 아키텍처 결정

### Trait 기반 추상화

모든 통합 지점이 trait:

| Trait | 목적 | 구현 |
|-------|------|------|
| `LlmProvider` | LLM API 호출 | OpenAI, Anthropic, OpenRouter, Generic |
| `ToolHandler` | 도구 실행 | 18가지 도구 유형 |
| `PlatformAdapter` | 메시징 플랫폼 | 16개 플랫폼 |
| `TerminalBackend` | 명령 실행 | Local, Docker, SSH, Daytona, Modal, Singularity |
| `MemoryProvider` | 영구 메모리 | 파일, SQLite |
| `SkillProvider` | 스킬 관리 | 파일 스토어 + Hub |

### 에러 계층

```
AgentError (최상위)
├── LlmApi(String)
├── ToolExecution(String)      ← ToolError에서 자동 변환
├── Gateway(String)            ← GatewayError에서 자동 변환
├── Config(String)             ← ConfigError에서 자동 변환
├── RateLimited { retry_after_secs }
├── Interrupted { message }
├── ContextTooLong
├── MaxTurnsExceeded
└── Io(String)
```

### 워크스페이스 구조

```
crates/
├── hermes-core           # 공유 타입, trait, 에러 타입
├── hermes-agent          # 에이전트 루프, 프로바이더, 컨텍스트, 메모리
├── hermes-tools          # 도구 레지스트리, 디스패치, 모든 도구 구현
├── hermes-gateway        # 메시지 게이트웨이, 플랫폼 어댑터
├── hermes-cli            # CLI 바이너리, TUI, 명령
├── hermes-config         # 설정 로딩 및 병합
├── hermes-intelligence   # 프롬프트 구축, 모델 라우팅, 사용량 추적
├── hermes-skills         # 스킬 관리, 스토어, 보안 가드
├── hermes-environments   # 터미널 백엔드
├── hermes-cron           # Cron 스케줄링
└── hermes-mcp            # Model Context Protocol
```

---

## 경쟁 해자

AI 에이전트 분야는 같은 천장에 부딪히는 Python 프로젝트로 넘쳐난다. 살아남을 것은 다음을 할 수 있는 것들이다:

1. **어디서든 실행** — Python 3.11과 40개 pip 패키지가 있는 개발자 MacBook뿐만 아니라, 엣지 디바이스, 임베디드 시스템, 인터넷 접근이 없는 엔터프라이즈 서버, $3 VPS 인스턴스.

2. **멀티유저로 확장** — 하나의 프로세스로 팀, 가족, 커뮤니티에 서비스하며, 각 대화가 다른 것을 저하시키지 않음.

3. **수개월간 안정성 유지** — 메모리 누수 없음, GC 일시정지 없음, 장기 실행 세션에서 조용히 축적되는 타입 에러 없음.

4. **다른 시스템에 임베드** — Rust 라이브러리는 C, C++, Python(PyO3), Node.js(napi), Go(CGo), WASM에서 호출 가능. Python 에이전트는 Python에서만 호출 가능.

---

## 현재 상태

초기 단계. 아키텍처와 핵심 추상화는 견고하다. Python 버전과의 기능 패리티는 약 10%. 자세한 내용은 [GAP_ANALYSIS.md](./GAP_ANALYSIS.md) 참조.

## 빌드

```bash
cargo build --release
```

## 실행

```bash
cargo run --release -p hermes-cli
```

## 테스트

```bash
cargo test --workspace
```

## 라이선스

MIT — [LICENSE](LICENSE) 참조.

[Nous Research](https://nousresearch.com)의 [Hermes Agent](https://github.com/NousResearch/hermes-agent) 기반.
