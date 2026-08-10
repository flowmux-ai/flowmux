<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Claude Code 세션 간 메시징 연동 기획

## 문서 상태

- 상태: 제안 (2026-08-10)
- 대상: Claude Code v2.1.224 이상 (로컬 확인 버전: 2.1.226)
- 결론: flowmux가 메시지 전송 자체를 대행하지는 않는다. 대신 **workspace의
  안정 속성으로 만든 고정 세션 이름을 주입하고, pane ↔ 세션 매핑을 상태로
  수집·노출**해서 flowmux 안의 Claude들이 서로를 찾고 제어하는 비용을
  최소화한다. workspace 표시 이름은 수시로 변하므로 주소로 쓰지 않는다(3.1.1).

## 1. 배경: Claude Code 세션 간 메시징 요약

Claude Code v2.1.224부터 서로 다른 세션(프로세스)끼리 메시지를 주고받는
기능이 추가되었다. flowmux 연동 관점에서 중요한 사실:

| 항목 | 내용 |
|---|---|
| 발견 | 같은 머신·같은 OS 사용자의 세션은 디스크 레지스트리 + 세션별 Unix socket으로 서로 발견. `/list-agents`(`/peers`) 또는 `ListAgents` 도구 |
| 주소 | 세션 **이름**이 곧 주소. 이름 충돌 시 짧은 식별자로 구분 |
| 이름 지정 | `claude --name <이름>` 시작 플래그, 세션 중 `/rename`. 미지정 시 cwd 폴더명에서 자동 생성 (예: `flowmux-3f`) |
| 전송 | `SendMessage` 도구. 평문 텍스트만. 수신측 tool 실행 중이면 대기, idle이면 새 턴 시작. 절대 인터럽트하지 않음 |
| 권한 | 메시지로 권한 승인·설정 변경 불가. 수신 세션의 권한 규칙이 그대로 적용. `crossSessionInbound` 설정으로 수신 정책(accept/hold/refuse) 제어 |
| 훅 연계 | 모든 훅 프로세스에 `CLAUDE_CODE_MESSAGING_SOCKET` env 노출(세션별 고유). 훅 stdin JSON에 `session_id` 포함. 메시지 수신 시점 훅은 **없음** |
| headless | `claude -p`도 소켓을 바인딩해 수신·목록 노출 가능. `--bare`는 불참 |
| 외부 프로세스 | Claude 세션이 아닌 프로세스가 메시지를 넣는 공식 방법 **없음**. Agent SDK에도 미노출 |
| 범위 | 컨테이너 경계 불통과. Remote Control/claude.ai 세션은 응답 전용 |

출처: <https://code.claude.com/docs/en/cross-session-messaging.md>

### flowmux가 이미 가진 것

- `flowmux fix`가 `~/.claude/settings.json`에 훅(`SessionStart`, `Stop` 등 6종)을
  설치하고, 훅은 `flowmux hooks claude <event>`로 daemon에 보고한다
  (`crates/flowmux-cli/src/hook_install.rs`, `cmd_hooks.rs`).
- claude 실행을 감싸는 wrapper shim이 이미 PATH에 있다
  (`$XDG_DATA_HOME/flowmux/shims/claude`) — 인자 조작이 가능한 유일한 지점.
- `SessionStart` 훅으로 (agent, surface) → `session_id` 매핑을 이미 저장한다
  (`crates/flowmux-state/src/agent_sessions.rs`), `flowmux tree`의 `TreeAgent`로 노출.
- `flowmux-procmon`이 pane별 실행 중 에이전트(claude/codex/…)를 감지한다.
- tmux 호환 shim 기반 Claude Code agent teams 지원 (AGENTS.md 참고).

즉 "어느 pane에 어떤 Claude 세션이 있는가"는 이미 절반 알고 있다. 빠진 것은
**메시징 주소(세션 이름)** 와 이를 에이전트·사용자에게 보여주는 통로다.

### 기존 수단과의 관계

| 수단 | 용도 | 한계 |
|---|---|---|
| `flowmux send-keys` + `read-screen` | 임의 프로그램 원격 타이핑 | Claude TUI에 텍스트 주입은 사용자 입력 위장 — 취약하고 상태 파악 어려움 |
| agent teams (tmux shim) | 한 리더가 팀원을 스폰해 하나의 작업 분담 | 리더가 만든 팀 내부로 한정. 독립 세션 간 통신 불가 |
| 세션 간 메시징 | **이미 떠 있는 독립 세션끼리** 통신·조정 | 평문 텍스트만, Claude만 발신 가능 |

세 수단은 대체가 아니라 보완 관계다. 이번 기획은 세 번째를 flowmux에서
쓸 만하게 만드는 것이다.

## 2. 접근안 비교

### A안 — 문서만 (코드 변경 없음)

AGENTS.md/SKILL에 ListAgents/SendMessage 사용 패턴만 추가. cwd 자동 이름에 의존.

- 장점: 즉시 적용, 유지보수 0.
- 단점: 같은 저장소의 worktree 여러 개를 각 workspace로 쓰는 핵심 시나리오에서
  이름이 전부 비슷해져(`flowmux-3f`, `flowmux-a1`…) 에이전트가 pane과 세션을
  대조할 방법이 없다. side panel의 workspace 이름과도 불일치.

### B안 — 이름 주입 + 매핑 수집 + 문서 (권장)

shim이 `--name <workspace 이름>`을 주입하고, SessionStart 훅으로 세션 이름·
메시징 소켓 경로를 수집해 상태·`tree`·새 `flowmux agents` CLI로 노출.
AGENTS.md/SKILL 계약 갱신.

- 장점: `ListAgents`에 workspace 유래의 읽을 수 있는 고정 주소가 표시되고,
  "현재 workspace 이름 ↔ 세션 주소" 대조는 매핑 테이블(`flowmux agents`)이
  담당 — workspace 이름이 변해도 주소 체계가 깨지지 않는다(3.1.1).
  이후 UI 확장의 토대.
- 단점: shim/doctor 개정 필요. `--name` 플래그 의존(버전 게이트 필요).

### C안 — 완전 제어 플레인

B안 + 메시징 소켓에 flowmux가 직접 JSON을 써서 외부 발신 + 수신 inbox UI.

- 기각: 소켓 wire 프로토콜이 비공개·미문서화라 마이너 업데이트에 파손된다.
  메시지 수신 훅이 없어 inbox UI도 정확한 신호원이 없다. CDP 사례처럼
  "지원 안 되는 것은 no-op로 위장하지 않는다" 원칙에 따라 비목표로 명시.

**결정: B안.** C안의 UI 요소 중 안전한 것(세션 이름 표시)만 후속 단계로.

## 3. 상세 설계 (B안)

### 3.1 세션 이름 주입 (shim)

`hook_install.rs`의 claude shim 스크립트 개정:

```bash
# exec "$real" "$@" 직전에:
if [ -n "${FLOWMUX_SURFACE_ID:-}" ] && [ "$FLOWMUX_CLAUDE_NAME_INJECT" = 1 ]; then
  case " $* " in
    *" -n "*|*" --name"*|*" --bare "*|*" --help "*|*" --version "*) ;;  # 사용자 의사 존중
    *)
      if ! flowmux_is_subcommand "$1"; then   # mcp/update/doctor/… 스킵
        session_name=$(flowmuxctl session-name 2>/dev/null)
        if [ -n "$session_name" ]; then
          export FLOWMUX_CLAUDE_SESSION_NAME="$session_name"
          set -- --name "$session_name" "$@"
        fi
      fi ;;
  esac
fi
exec "$real" "$@"
```

- `flowmuxctl session-name` (신규 CLI 서브커맨드): `FLOWMUX_WORKSPACE_ID`로
  daemon에 workspace 정보를 물어 이름을 계산해 출력. daemon 미가동·env 부재 시
  빈 출력 → shim은 주입을 건너뛴다. 기존 `WorkspaceTree` 응답을 클라이언트에서
  조회하면 되므로 **새 IPC verb 불필요**.
- `FLOWMUX_CLAUDE_NAME_INJECT` 상수는 `flowmux fix` 시점에 `claude --version`
  ≥ 2.1.224 여부로 0/1을 shim 안에 기록한다(런타임 버전 프로빙 비용 회피).
  버전이 낮으면 주입 없는 기존 동작 유지.

#### 3.1.1 이름 계산 규칙 — 안정값만 쓴다

`Workspace.name`은 **자동값**이다: root_dir 마지막 폴더로 시작해 PTY OSC·cwd
변화 등 daemon 신호로 수시 갱신된다(cmux `processTitle` 대응,
`flowmux-core/src/lib.rs`의 `Workspace` 문서 주석). 세션 이름은 한 번 박히면
세션 수명 동안 바뀌지 않으므로(외부에서 `/rename` 불가) 변동값을 스냅샷하면
side panel 표시와 즉시 어긋난다. 따라서 주입 이름은 workspace의 **안정 속성**
으로만 계산한다:

```
base   = custom_title (사용자가 지정했으면)
         | root_dir 마지막 폴더명 (아니면)      # 워크스페이스 생성 후 불변
suffix = WorkspaceId 앞 4 hex                   # 고유성 보장, 불변
name   = slug(base) '-' suffix                  # 예: flowmux-feature-x-7f3a
```

- `display_title()`의 현재 자동값은 쓰지 않는다 — 표시용이지 주소용이 아니다.
- 같은 workspace에 Claude가 둘 이상이면 `session-name`이 daemon 상태에서 기존
  주입 이름과의 충돌을 확인하고 `-2`, `-3` 순번을 덧붙인다(스폰 시점 결정,
  결정적).
- workspace 이름이 이후 무엇으로 바뀌든 **주소는 불변**이다. "현재 표시 이름
  ↔ 세션 주소" 대응은 3.3의 `flowmux agents`(P2 매핑)가 담당한다 — 즉 진실의
  원천은 표시 이름이 아니라 매핑 테이블이고, 이름 base는 사람이 읽기 위한
  힌트에 불과하다.
- 사용자가 workspace를 rename(`custom_title` 변경)해도 떠 있는 세션의 주소는
  그대로다. 새로 뜨는 세션부터 새 base가 반영된다. 한계로 문서화한다.

#### 3.1.2 왜 shim인가 — 훅/skill/plugin으로는 불가

세션 이름을 정하는 공식 표면은 `--name` 시작 플래그와 대화형 `/rename` 둘뿐
이다. SessionStart 훅은 `session_id`를 받기만 할 뿐 이름을 설정할 수 없고
(훅의 설정 변경 금지는 문서 명시), `/rename`은 빌트인 명령이라 모델·skill·
plugin이 호출할 수 없으며, 대응 env var도 없다. argv에 개입할 수 있는 유일한
지점이 이미 설치되는 wrapper shim이므로, 사용자가 이름 없이 `claude`만 쳐도
자동으로 이름이 붙는 경로는 shim뿐이다.

#### 3.1.3 shim 미경유 세션 폴백

절대경로 실행이나 IDE 확장의 직접 스폰 등은 shim을 우회해 이름 미주입 세션이
된다. 두 단계로 완화한다:

1. **cwd 대조 (P3 문서화)**: `ListAgents`는 각 세션의 작업 디렉터리를 보여
   준다. 이름 없는 세션은 `flowmux --json agents`의 pane cwd/root_dir와
   대조해 특정한다 — AGENTS.md 발견 레시피에 폴백 절차로 포함.
2. **훅 감지 + 안내 (선택)**: SessionStart 훅이 `FLOWMUX_CLAUDE_SESSION_NAME`
   부재(= shim 미경유)를 감지하면 상태에 `name_injected: false`로 기록하고,
   훅 additionalContext로 "사용자에게 `/rename <계산된 이름>`을 안내하라"는
   컨텍스트를 주입해 반자동 복구를 유도할 수 있다. 필수 아님 — 채택 여부는
   구현 시 판단.
- `FLOWMUX_CLAUDE_SESSION_NAME` export는 3.2의 훅 수집용이다 (claude의 자식
  프로세스인 훅이 상속).

### 3.2 세션 메타데이터 수집 (훅 → daemon → 상태)

`flowmux hooks claude session-start` 처리(`cmd_hooks.rs`) 확장:

- stdin JSON의 `session_id` (기존) 에 더해,
- env `CLAUDE_CODE_MESSAGING_SOCKET` → 메시징 참여 여부·소켓 경로,
- env `FLOWMUX_CLAUDE_SESSION_NAME` → shim이 주입한 세션 이름

을 함께 daemon에 보고한다. IPC `AgentSessionUpdate`에 optional 필드
`session_name`, `messaging_socket` 추가(모두 `Option` — 구버전 클라이언트와
와이어 호환). `agent_sessions.rs` 저장소와 `TreeAgent`(`flowmux tree` 응답)에
같은 필드를 추가한다.

주의: 사용자가 세션 중 `/rename` 하거나 직접 `--name`을 넘기면 저장된 이름과
실제 이름이 어긋날 수 있다. 이 값은 **best-effort 힌트**이고, 진실의 원천은
항상 Claude 쪽 `ListAgents`다. 문서에 명시한다.

### 3.3 `flowmux agents` CLI (신규, daemon 변경 없음)

`WorkspaceTree` 응답을 클라이언트에서 평탄화해 출력:

```
$ flowmux agents
WORKSPACE      PANE      AGENT   STATUS   SESSION NAME   MESSAGING
feature-x      1a2b…     claude  running  feature-x      yes
bugfix-y       9c8d…     claude  idle     bugfix-y       yes
scratch        44ef…     codex   running  -              -
```

`--json`이면 에이전트 소비용 배열. 에이전트가 "pane ↔ 메시징 주소" 대조에
쓰는 단일 진입점이 된다.

### 3.4 AGENTS.md / SKILL 계약 갱신

AGENTS.md에 "Claude 세션 간 메시징" 절 추가 (SKILL 파일도 동기화 —
훅/SKILL 페이로드는 바이너리 내장이므로 doctor/fix 개정과 함께):

- 발견 레시피: `flowmux --json agents`로 대상 pane의 세션 이름 확인 →
  `ListAgents`로 존재 확인 → `SendMessage`.
- 수단 선택 기준: 다른 pane의 **Claude**와 통신·작업 지시는 SendMessage 우선.
  `send-keys`는 비-Claude 프로그램이나 메시징 불가 상황의 폴백.
  하나의 작업을 팀원에게 분담시키는 것은 기존 agent teams.
- 오케스트레이터 패턴: workspace마다 worktree worker 세션, 리더 세션 1개가
  `flowmux agents`로 전체를 파악하고 SendMessage로 지시·수합.
- 금지 사항: 메시징으로 자기 세션에서 거부된 권한을 다른 세션에 우회 요청하지
  말 것(수신측 권한이 그대로 적용되고, Claude Code 자체 규칙 위반).
  메시지에 `/command`를 넣어도 텍스트로 도착할 뿐 실행되지 않음.

### 3.5 doctor / fix 개정

- fix: 개정된 shim 설치 + `claude --version` 검사로 `FLOWMUX_CLAUDE_NAME_INJECT`
  결정 + SKILL/AGENTS 문서 페이로드 갱신.
- doctor: shim 내용이 최신 개정판인지, 기록된 주입 플래그가 현재 claude 버전과
  일치하는지 감사. 불일치 시 `flowmux fix` 재실행 안내.

### 3.6 (후속, 선택) side panel 세션 이름 표시

`TreeAgent.session_name`이 생기면 side panel의 workspace 행 에이전트 배지에
세션 이름을 툴팁 등으로 노출할 수 있다. 메시지 수신 표시는 수신 훅이 없어
정확한 신호원이 없으므로 만들지 않는다. 본 기획 범위 밖, 별도 판단.

## 4. 비목표

- flowmux가 메시징 소켓에 직접 쓰기(비공개 프로토콜) — 하지 않는다.
- 메시지 inbox/수신 알림 UI — 수신 시점 훅이 없어 신호원이 없다.
- `crossSessionInbound` 등 사용자 권한 설정 자동 변경 — 문서 안내만.
- Agent SDK 연동 — SDK에 기능 미노출.
- flowmux 자체 메시지 버스 신설 — Claude Code 기능을 그대로 쓴다.

## 5. 오픈 이슈 / 리스크

1. `--name`이 `--resume`, `-p`, 서브커맨드와 조합될 때의 정확한 동작은 구현
   시점에 실측으로 확인한다(스킵 목록 조정 가능성).
2. 기능이 서버측 feature flag 및 `CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC` 류
   env로 꺼질 수 있다. 꺼져 있어도 `--name` 주입 자체는 무해하다.
3. 워크스페이스 이름에 공백·비ASCII가 있어도 되는지 실측 확인(현재 설계는
   그대로 전달).
4. 레지스트리/소켓 등 미문서 세부는 마이너 업데이트에 바뀔 수 있다 — 설계가
   문서화된 표면(`--name`, 훅 env, 훅 JSON)만 쓰는 이유.

## 6. 테스트 전략

- 단위: shim 스크립트 생성 내용(hook_install), env·stdin 파싱(cmd_hooks),
  `agent_sessions` 새 필드 왕복, `tree` 직렬화. 모두 headless 크레이트라
  `cargo test -p flowmux-cli -p flowmux-state -p flowmux-ipc`로 충분.
- 라이브 검증(AGENTS.md 규칙): workspace 2개에 claude 각각 실행 →
  ① 두 세션의 `/list-agents`에 상대가 workspace 이름으로 보이는지
  ② `SendMessage` 왕복이 되는지
  ③ `flowmux agents` 출력이 실제와 일치하는지 확인.

## 7. 단계별 계획

| 단계 | 내용 | 산출물 |
|---|---|---|
| P1 | shim `--name` 주입 + `flowmuxctl session-name` + doctor/fix 개정 | ListAgents에 workspace 이름 표시 |
| P2 | 훅 메타데이터 수집 + 상태/`tree` 확장 + `flowmux agents` CLI | pane ↔ 세션 매핑 조회 |
| P3 | AGENTS.md + SKILL 계약 갱신 | 에이전트 사용 패턴 확립 |
| P4(선택) | side panel 세션 이름 표시 | 사용자 가시성 |

P1~P3는 순서대로 각각 독립 배포 가능하다.
