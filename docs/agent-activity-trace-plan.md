<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Agent Activity Trace 작업 계획

## 문서 상태

- 상태: 제안 (2026-07-31 코드 대조 검토 반영)
- 대상: flowmux의 Agent 작업 관찰성
- 지원 대상: Claude Code, Codex, OpenCode
  (Cline도 generic hook 전송 경로를 재사용하지만 이벤트 커버리지는
  다르므로 MVP 검증 대상에는 포함하지 않는다)
- 예상 MVP 일정: 개발자 1명 기준 약 8~10일

## 1. 배경

flowmux에 다음과 같은 제품 피드백이 전달되었다.

> Agent의 작업흐름, 작업 내역 등을 좀 더 잘 알 수 있게 표시되는 것이 중요하다.

현재 flowmux는 Agent 이름과 `working`, `blocked`, `done` 상태, 마지막
메시지와 알림을 제공한다. 하지만 사용자가 Agent의 현재 단계와 최근
작업 과정을 연속적으로 파악할 수 있는 기록은 제공하지 않는다.

관련 구현은 다음 위치에 있다.

- [Agent Bar](../crates/flowmux/src/ui/agent_bar.rs)
- [Agent 상태 모델](../crates/flowmux-core/src/lib.rs)
- [Agent hook 처리](../crates/flowmux-cli/src/cmd_hooks.rs)
- [hook payload 파싱](../crates/flowmux-cli/src/hooks.rs)
- [notification 기록](../crates/flowmux/src/notifications.rs)
- [side panel](../crates/flowmux/src/ui/sidebar.rs)

## 2. 목표

여러 Agent가 동시에 동작할 때 사용자가 3초 안에 다음 질문에 답할 수
있도록 한다.

1. 어떤 Agent가 실행 중인가?
2. 현재 어느 단계인가?
3. 최근에 무엇을 했는가?
4. 사용자의 개입이 필요한가?
5. 마지막 작업 결과는 무엇인가?

표시 대상은 lifecycle, 도구 사용, 상태 전환, 완료 결과처럼 외부에서
관찰 가능한 정보로 제한한다. Agent의 내부 사고 과정은 표시하거나
저장하지 않는다.

## 3. 제품 결정

새 pane이나 상시 노출되는 global feed를 만들지 않는다. 다음 두
surface가 역할을 나눠 맡는다.

- `Agent Bar`: 현재 실행 중인 Agent와 현재 상태
- side panel의 `Activity` 버튼: 완료된 작업을 포함한 최근 작업 내역

현재 Agent Bar는 live Agent가 없으면 사라진다. 기록 진입점을 Agent
Bar에만 배치하면 Agent가 종료된 후 작업 내역을 다시 열 수 없다.
따라서 Activity 기록은 side panel에서 항상 접근 가능하게 한다.

이 구성은 다음 장점이 있다.

- terminal 영역의 상시 높이와 폭을 추가로 사용하지 않는다.
- 기존 Agent Bar의 빠른 tab 이동 동작을 유지한다.
- Agent Bar를 비활성화해도 기록에 접근할 수 있다.
- notification의 unread 및 desktop toast 의미를 오염시키지 않는다.

## 4. 현실 가능성 및 가치 평가

| 기능 | 현실 가능성 | 사용자 가치 | 화면 효율 | 결정 |
|---|---:|---:|---:|---|
| 현재 상태 구체화 | 5/5 | 4/5 | 5/5 | MVP |
| Recent Activity popover | 4/5 | 5/5 | 5/5 | MVP |
| 완료 결과 요약 | 5/5 | 4/5 | 5/5 | MVP |
| workspace/pane/tab 이동 | 5/5 | 4/5 | 5/5 | MVP |
| tool 이름 표시 (Claude 한정, NOW에만) | 3/5 | 3/5 | 5/5 | 데이터가 있을 때만 |
| 작업 계획/checklist | 2/5 | 3/5 | 3/5 | 후속 |
| 재시작 후 기록 복원 | 3/5 | 3/5 | 5/5 | 사용성 검증 후 |
| 전체 terminal replay | 1/5 | 2/5 | 1/5 | 제외 |

핵심 lifecycle 기록은 현재 hook과 `AgentActivityUpdate`로 구현할 수 있어
현실 가능성이 높다. `AgentActivityUpdate`에는 `message`,
`custom_status`, `seq`, `session_id`가 이미 존재하므로 wire 변경이
필요 없다.

아래 목록은 화면에 표시하는 **정규화된 상태 어휘**다. 모든 Agent가
모든 상태를 발생시키는 것은 아니다. 현재 코드에 실제로 wiring된
이벤트 기준 커버리지는 다음과 같다 (`hook_install.rs`,
`cmd_hooks.rs` 기준).

| 표시 상태 | Claude Code | Codex | OpenCode |
|---|---|---|---|
| Session started | SessionStart hook + wrapper shim | wrapper shim | wrapper shim |
| Turn started | UserPromptSubmit | 없음 | `session.status`(busy) |
| Working (tool) | PreToolUse | 없음 | 없음 |
| Waiting for input | Notification | Phase 0에서 확인 | `permission.asked`, `session.error` |
| Turn completed | Stop | `notify`(agent-turn-complete) | `session.idle` |
| Session ended | SessionEnd hook | PID sweep | PID sweep |

이 표에서 나오는 결론:

- `Error`는 **별도 상태로 만들지 않는다.** `AgentStatus`에 error
  variant가 없고, 유일하게 error를 구분해 주는 신호(OpenCode
  `session.error`)도 이미 notification 경로로 들어온다. MVP에서는
  error를 Waiting for input과 같은 attention 상태로 표시하고 summary
  텍스트로 구분한다. 별도 Error 상태는 `AgentStatus` enum, rollup,
  직렬화까지 건드리는 큰 변경이므로 실사용 요구가 확인된 뒤로 미룬다.
- `Session ended`는 Claude만 hook으로 알 수 있다. Codex/OpenCode는
  daemon의 PID liveness sweep이 presence를 제거하는 시점을 session
  종료로 기록한다.
- Codex에는 신뢰할 수 있는 hook 기반 turn 시작 신호가 없다. `NOW`는
  기존 screen heuristic이 spinner/title을 감지하면 `Working`으로
  보완할 수 있지만, 이 추정 신호는 `RECENT`에는 기록하지 않는다.

tool 이름은 Claude PreToolUse payload의 `tool_name`에서만 얻을 수
있다. 현재 `ClaudeHookInput`은 이 필드를 파싱하지 않으므로 serde 필드
추가가 필요하다. `tool_input`은 파싱하지 않는다(개인정보 경계).
정보가 없으면 추측하지 않고 공통 상태로 fallback한다.

## 5. 화면 구성

### 5.1 side panel header

```text
┌──────────────────────────────────────────┐
│ [+]  Workspaces      [Activity] [Bell]   │
├──────────────────────────────────────────┤
│ workspace-a                              │
│   Claude · Running tests                 │
│ workspace-b                              │
│   Codex · Waiting for approval           │
└──────────────────────────────────────────┘
```

Activity 버튼 정책:

- side panel header에 icon button으로 배치한다.
- 항상 접근 가능하게 한다.
- Activity 자체에는 unread badge를 표시하지 않는다.
- `waiting`과 `error` attention은 기존 Agent Bar와 notification이 담당한다.
- tooltip과 accessible label로 `Agent activity`를 제공한다.

### 5.2 Agent Bar

현재 높이와 item 최대 폭을 유지한다.

```text
Agents
[Claude · Running tests] [Codex · Waiting for approval]
```

각 item에는 다음 정보만 표시한다.

- Agent 이름
- 현재 단계
- 상태 아이콘
- workspace 색상

기존처럼 item을 클릭하면 해당 workspace/pane/tab으로 이동한다. 긴 결과,
workspace 이름, 경과 시간과 기록은 Activity popover에서만 표시한다.

### 5.3 Activity popover

권장 크기는 너비 380~420px, 최대 높이 480px이다.

```text
Agent Activity                               Clear

NOW
● Claude                                   1m
  Running tests
  flowmux-terminal · zsh

▲ Codex                                    3m
  Waiting for approval
  browser-work · zsh

RECENT
12:41  ✓ Claude
       Completed: Fixed browser focus restoration

12:37  ● Claude
       Started turn

12:36  ▲ Claude
       Waited for approval

12:35  ■ Codex
       Session started
```

(NOW 항목의 두 번째 줄은 현재 단계, 세 번째 줄은 workspace 이름과
tab 이름이다.)

화면 정책:

- `NOW`와 `RECENT` 두 구역만 제공한다.
- MVP에서는 filter, graph, 별도 tab을 제공하지 않는다.
- workspace 색상과 이름으로 출처를 구분한다.
- **`RECENT`에는 상태 전환만 기록한다.** tool 단위 이벤트(Claude
  PreToolUse)는 `NOW`의 현재 단계 텍스트만 갱신하고 `RECENT`에 항목을
  추가하지 않는다. PreToolUse는 tool 호출마다 발화하므로, 개별 기록을
  허용하면 활발한 turn 하나가 몇 분 만에 50개 buffer를 tool 이벤트로
  채워 완료 요약을 밀어낸다. 상태 전환만 기록하면 50개로 수 시간
  분량의 의미 있는 timeline이 유지된다.
- 완료 요약은 최대 2~3줄로 표시한다.
- 목록은 스크롤하며 전체 window 크기를 바꾸지 않는다.
- 색상뿐 아니라 아이콘과 텍스트로 상태를 표현한다.
- 항목 클릭 시 해당 workspace/pane/tab으로 이동한다. 기록은 tab보다
  오래 남으므로 fallback을 정의한다: 대상 tab이 닫혔으면 workspace로만
  이동하고, workspace도 없으면 항목을 비활성화하고 tooltip으로 이유를
  알린다.

## 6. 데이터 모델

복잡한 workflow taxonomy나 provider별 모델을 새로 만들지 않는다.

```text
ActivityEntry
├─ agent
├─ status?
├─ summary
├─ created_at
├─ workspace
├─ pane
├─ surface
├─ workspace_label
├─ surface_label
├─ color
├─ session_id?
└─ source
```

- `status`: 기존 `AgentStatus` 재사용. `Session ended`처럼 live 상태가
  없는 lifecycle 기록은 `None`
- `summary`: `custom_status` 또는 `message`에서 생성 (기존
  `AgentPresence::status_text()`와 같은 우선순위)
- `created_at`: GUI가 이벤트를 수신한 시간
- `workspace`, `pane`, `surface`: 기존 클릭 이동 경로 재사용
- `workspace_label`, `surface_label`, `color`: 기록 시점의 표시값.
  대상 workspace/tab이 나중에 닫혀도 기록 문맥을 유지
- `session_id`: 서로 다른 Agent session 기록 구분
- `source`: 실제 값은 `flowmux:hook` / `flowmux:proc` /
  `flowmux:screen`. 기록 대상 판별과 디버깅에 사용

현재 IPC에는 `message`, `custom_status`, `seq`, `session_id`가 이미
존재한다. MVP에서는 별도의 새 IPC verb를 만들지 않고 기존
`AgentActivityUpdate`를 확장 지점으로 사용한다.

```text
Agent hook
   ↓
cmd_hooks에서 상태와 summary 정규화
   ↓
기존 AgentActivityUpdate
   ↓
현재 AgentPresence 갱신 (stale seq는 여기서 이미 걸러짐)
   ├─ Agent Bar 갱신
   └─ ActivityStore에 최근 이벤트 추가
```

구현 시 주의할 배관 두 가지:

- stale `seq` 거부는 새 작업이 아니다.
  `AgentPresence::apply_report`가 이미 `seq`가 뒤처진 report를
  거부하고 `false`를 반환한다. ActivityStore가 **수용된 report만**
  기록하면 상태 갱신에는 별도의 seq 검사가 필요 없다. 단,
  `SessionEnd`는 현재 `apply_report`를 거치지 않는 제거 경로이므로
  Phase 2에서 `seq`와 `session_id`를 확인하는 공통 제거 메서드로
  통합한다.
- ActivityStore는 GTK 쪽(GUI thread) store다. 현재 IPC handler가
  presence 갱신 후 보내는 `GtkCommand::SetAgentStatus`는 workspace
  id만 전달하므로, 정규화된 entry 내용을 GTK 쪽에 전달하려면
  `GtkCommand`에 payload를 추가해야 한다 (CLAUDE.md의 tokio↔GTK
  bridge 패턴 그대로: variant 확장 + dispatch loop 처리).

## 7. 기록 정책

Activity 기록은 notification과 별도의 store에 보관한다. 기존
`NotificationStore`의 bounded `VecDeque`(`MAX_RETAINED` 50 + dedupe
window) 패턴을 그대로 따른다.

- 최근 50개
- 메모리에서만 보관
- 오래된 항목 자동 제거
- `Clear` 지원
- **기록 source는 `flowmux:hook` 이벤트와 presence 제거(session
  종료)만.** `flowmux:screen` 텍스트 스캔과 process polling
  (`flowmux:proc`)은 `NOW` 표시에는 반영되지만 `RECENT`에는 기록하지
  않는다. 이 규칙 하나로 hook/screen/proc 3중 source의 중복 제거
  문제 대부분이 사라진다.
- tool 단위 이벤트(PreToolUse)는 기록하지 않음 (5.3절 참고)
- OpenCode의 반복 `session.updated`는 presence/session ID 갱신에만
  사용하고, presence가 처음 생기거나 session ID가 바뀐 경우가 아니면
  `Session started`를 추가하지 않음
- 동일 surface에서 같은 상태와 summary가 짧은 시간 안에 반복되면 병합
- stale `seq` 이벤트는 `AgentPresence::apply_report`가 이미
  거부하므로 기록 경로에 도달하지 않음
- notification unread 상태 및 desktop toast와 분리

MVP에서는 재시작 후 기록 복원을 지원하지 않는다. 사용자가 실제로
Recent Activity를 반복적으로 사용하고 재시작 이후 기록을 요구하는 것이
확인되면 bounded JSONL 저장을 검토한다. 상태 변경마다 `state.json`을
다시 쓰는 방식은 사용하지 않는다.

## 8. 개인정보 및 신뢰 경계

다음 정보는 기본적으로 저장하지 않는다.

- raw prompt
- raw command line
- tool input
- command stdout/stderr
- 환경 변수
- Agent 내부 사고 과정

Activity에는 정규화된 상태와 짧은 summary만 저장한다. 파일 경로를
표시할 경우 workspace-relative 경로만 사용한다.

Git 변경은 동시에 사용자나 다른 Agent가 만들 수 있으므로 MVP에서는
특정 Agent의 변경으로 귀속하지 않는다. 이후 표시가 필요하면
`Agent changes`가 아니라 `workspace changes`로 명시한다.

## 9. 단계별 구현 계획

### Phase 0 — Agent별 데이터 확인

예상: 1일

이벤트 wiring 자체는 코드에서 이미 확인되어 4절 표로 정리했다. Phase
0은 wiring 조사가 아니라 **실제 payload 내용 검증**이다. Claude Code,
Codex, OpenCode hook payload fixture를 확보하고 다음을 정리한다.

- Codex `notify`가 approval 요청 이벤트를 전달하는지 (버전별 확인 —
  4절 표의 유일한 미확인 칸)
- Claude PreToolUse payload의 `tool_name` 값 형태와 안전한 표시 범위
- 완료 메시지(`last_assistant_message`) 품질과 길이 분포
- session ID와 surface 연결 정확도
- 민감 정보 포함 여부

산출물:

- Agent capability 표
- sanitization 규칙
- 테스트용 익명 fixture
- 정보가 없을 때 사용할 fallback 문구

완료 조건:

- 세 Agent 모두 최소한 시작, 완료, 대기 중 일부를 안정적으로 식별한다.
- 제공되지 않는 정보는 추측하지 않기로 확정한다.

### Phase 1 — 현재 상태 구체화

예상: 1~2일

기존 `custom_status`를 실제로 채운다. 아래 표는 Claude 기준이다.
Codex/OpenCode는 4절 커버리지 표의 이벤트만 발생하므로, generic hook
경로(`run_generic_agent_hook_event`)에서 같은 방식으로 존재하는
이벤트에만 `custom_status`를 채운다.

| Claude hook 이벤트 | 기본 표시 |
|---|---|
| SessionStart | Ready |
| PromptSubmit | Starting turn |
| PreToolUse | Working 또는 안전한 tool 이름 (NOW에만 반영) |
| Notification | Waiting for input |
| Stop | Completed: `<summary>` |
| SessionEnd | Session ended |

작업:

- hook payload 정규화
- `ClaudeHookInput`에 `tool_name` serde 필드 추가. `tool_input`은
  파싱하지 않는다.
- 기존 `shorten_body`(160자 한 줄 축약)를 summary 생성에 재사용
- raw command와 tool input 제외
- Agent Bar와 workspace 상세에서 기존 status text
  (`AgentPresence::status_text`) 재사용
- 상세 데이터가 없으면 기존 `working` 표시 유지
- payload 및 fallback 단위 테스트 추가

이 단계는 UI 변경 없이 먼저 제공할 수 있는 quick win이다.

### Phase 2 — ActivityStore

예상: 1~2일

기존 `NotificationStore`의 bounded `VecDeque` 패턴을 재사용하되 별도
store로 구현한다. 기록 대상을 hook source의 상태 전환으로 제한했으므로
(7절) 병합/중복 제거 로직은 단순하다.

작업:

- `ActivityEntry`와 bounded store 추가
- 최대 50개 유지
- `flowmux:hook` source와 presence 제거 이벤트만 기록
- `SessionEnd`와 PID sweep이 함께 사용하는 안전한 presence 제거
  메서드 추가. `seq`/`session_id`가 현재 session과 맞을 때만 제거하고,
  제거된 agent/session 및 workspace/pane/tab 정보를 반환
- 연속 동일 상태+summary 병합
- `GtkCommand`에 activity entry payload 추가 (6절 배관 참고)
- session 종료 후에도 기록 유지
- `Clear` 구현
- exact tab 이동에 필요한 ID 보존

완료 조건:

- 이벤트 1,000개 입력 후에도 50개만 유지된다.
- 반복 polling과 screen 스캔으로 timeline이 오염되지 않는다.
- tool 호출이 많은 turn 하나가 `RECENT`를 밀어내지 않는다.
- Agent 종료 후에도 recent activity를 조회할 수 있다.

### Phase 3 — Activity popover

예상: 2~3일

작업:

- side panel header에 Activity menu button 추가
- `NOW` live Agent 목록 구현
- `RECENT` 최근 이벤트 목록 구현
- relative time 표시
- workspace 색상과 이름 표시
- 완료 summary 2~3줄 표시
- 기존 tab 이동 경로 재사용
- keyboard 접근, tooltip, accessible label 적용
- 작은 window에서 최대 높이와 스크롤 검증

Agent Bar 자체의 높이와 구조는 변경하지 않는다.

### Phase 4 — 안정화 및 live 검증

예상: 2일

검증 시나리오:

1. 서로 다른 workspace에서 Agent 3개 실행
2. Agent가 장시간 작업
3. background tab에서 승인 요청
4. 완료 후 Agent 프로세스 종료 (Claude는 SessionEnd hook,
   Codex/OpenCode는 PID sweep 경로 각각 확인)
5. 완료된 Agent 기록 다시 열기
6. 같은 상태가 hook과 screen 텍스트 스캔에서 중복 관측
7. 오래된 이벤트가 늦게 도착
8. 기록 항목의 대상 tab을 닫은 뒤 클릭 (workspace fallback 확인)
9. Agent Bar 비활성화 상태에서 Activity 접근
10. 800×600 수준의 작은 window
11. Claude Code, Codex, OpenCode 실제 실행

프로젝트 규칙에 따라 단위 테스트뿐 아니라 실제 flowmux 실행 화면에서
상태 변화, 화면 배치와 클릭 이동을 확인한다.

## 10. 테스트 전략

### 단위 테스트

- hook payload에서 상태와 summary 정규화
- 빈 필드 fallback
- summary 길이 제한
- 민감한 raw payload(`tool_input` 포함)가 entry에 남지 않음
- 연속 동일 이벤트 병합
- PreToolUse 연타 시 `RECENT`에 항목이 추가되지 않음
- screen/proc source 이벤트가 `RECENT`에 기록되지 않음
- 최대 50개 retention
- stale `seq` report가 기록 경로에 도달하지 않음 (기존
  `apply_report` 거부 경로와의 연결 확인)

### UI 테스트

- live Agent가 없을 때 Activity 버튼 접근 가능
- `NOW`와 `RECENT` 구분
- 긴 summary ellipsize 또는 wrap
- 작은 window에서 popover scroll
- 색상 없이도 아이콘과 텍스트로 상태 식별
- Activity 항목 클릭 시 정확한 tab으로 이동

### live 검증

- Claude Code lifecycle
- Codex lifecycle 및 notification
- OpenCode session, permission, error lifecycle
- 여러 workspace와 pane에서 동시 실행
- hook, process sweep, screen 스캔 신호 중복

## 11. 예상 일정

개발자 1명 기준:

| 단계 | 예상 |
|---|---:|
| Agent별 데이터 확인 | 1일 |
| 현재 상태 구체화 | 1~2일 |
| ActivityStore와 종료 경로 | 2일 |
| Activity popover | 2~3일 |
| 안정화 및 live 검증 | 2일 |
| 합계 | 약 8~10일 |

Agent별 상세 tool 표시를 동일한 수준으로 지원하려면 Agent당 1~3일이
추가될 수 있다.

## 12. 주요 위험과 대응

| 위험 | 대응 |
|---|---|
| Agent별 hook 정보 차이 | 4절 커버리지 표 기준으로 존재하는 이벤트만 표시, 추측 금지 |
| tool 이벤트 폭주로 timeline 오염 | tool 이벤트는 `RECENT` 미기록, `NOW`만 갱신 |
| hook / screen 스캔 / process polling 3중 source 중복 | `RECENT`는 `flowmux:hook` source만 기록 |
| 잘못된 이벤트 순서 | 기존 `apply_report`의 `seq` 거부 재사용 |
| prompt나 command의 민감 정보 | raw payload 저장 금지, 정규화된 summary만 보관, `tool_input` 미파싱 |
| 닫힌 tab으로의 이동 실패 | workspace fallback, 대상이 모두 없으면 항목 비활성화 |
| Git 변경의 Agent 귀속 오류 | MVP에서는 Agent별 changed files로 단정하지 않음 |
| 화면 점유 | 영구 panel 대신 header icon과 popover 사용 |
| Agent 종료 후 기록 접근 불가 | live Agent Bar와 영구 Activity 진입점 분리 |

## 13. MVP 범위

### 포함

- 구체적인 현재 상태
- 최근 활동 50개
- `NOW`와 `RECENT` popover
- 완료 summary
- 정확한 workspace/pane/tab 이동
- 중복 제거
- 개인정보 보호
- Claude Code, Codex, OpenCode fallback

### 제외

- 영구 기록
- raw prompt, command, stdout
- Agent 내부 사고 과정
- plan/checklist 동기화
- Agent별 Git 변경 귀속
- 전체 terminal replay
- 별도의 Activity pane이나 global dashboard
- 신규 dependency

이 범위가 피드백을 검증할 수 있는 가장 작은 완성형이다. 영구 저장과
계획 UI는 Recent Activity가 실제로 자주 사용된다는 것이 확인된 뒤
추가한다.

## 14. MVP 완료 기준

다음 조건을 모두 만족하면 MVP를 완료한 것으로 본다.

- 사용자가 3초 안에 live Agent, 현재 단계와 attention 상태를 식별할 수 있다.
- Agent가 종료된 뒤에도 최근 완료 내역을 열 수 있다.
- Activity 항목에서 정확한 workspace/pane/tab으로 이동할 수 있다.
- 반복 hook과 polling이 timeline을 과도하게 채우지 않는다.
- 기록은 50개로 제한되고 메모리가 무한히 증가하지 않는다.
- raw prompt, command, tool input이 저장되지 않는다.
- Agent별 상세 정보가 없을 때 공통 상태로 자연스럽게 fallback한다.
- 작은 window에서도 기존 terminal 영역을 추가로 점유하지 않는다.
- 세 지원 Agent의 실제 lifecycle을 running flowmux에서 검증한다.
