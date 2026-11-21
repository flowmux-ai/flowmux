<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Agent state and attention implementation plan

Status: implementation in progress. Baseline: `cd92d280c9b0`.

## Flowmux에서 확인한 개선 지점

1. **읽은 미해결 요청의 강조가 약해진다.** `crates/flowmux-core/src/lib.rs:1117` rollup은 seen 항목을 제외한다. `crates/flowmux/src/ui/mod.rs:59`는 읽은 Blocked에 idle CSS를 사용한다. 실제 Blocked 상태가 사라지는 것은 아니지만 workspace 경고와 색상은 약해진다. 소스상 동작 확인이며 실사용 실패를 재현했다는 주장은 아니다.
2. **상태를 만든 근거와 소유권 source가 다르다.** `AgentPresence`의 source는 hook/process 소유권 보존에 쓰인다. 화면이 상태를 정교화해도 그 source를 그대로 유지할 수 있다. UI에 현재 source만 보여주면 화면 추정을 hook 확정으로 오해할 수 있다.
3. **Done은 성공 증거가 아니다.** `public_status()`는 읽지 않은 Idle을 Done으로 투영한다. 응답 종료와 빌드·테스트 성공을 구분하는 표현이 필요하다.
4. **알림에 원인 ID가 부족하다.** `crates/flowmux/src/notifications.rs:23`의 8초 중복 창과 Info/TurnCompleted 병합은 session/turn/request를 모른다. 서로 다른 빠른 turn이 병합되거나 늦은 재전송이 중복될 가능성은 회귀 fixture로 먼저 증명해야 한다.
5. **hook 진단이 설치와 전달을 충분히 구분하지 못한다.** best-effort hook의 종료 코드 0은 앱 반영 성공을 보장하지 않는다. 설치·지원·전달·적용 상태가 필요하다.
6. **주의 우선순위와 안정적인 행 갱신이 부족하다.** `activity.rs`는 Working/Blocked를 함께 진행 중으로 다룬다. `sidebar.rs:1102`는 activity rows를 다시 만든다. UI 변경 시 선택·키보드 포커스·스크롤 유지까지 설계해야 한다.

보존할 강점: stale seq/session 거절, 병렬 wait와 tombstone, batch 경계, root/child ledger, screen fingerprint, SSH 상태 제한, notification exact-surface navigation, 기존 All/Unread·종류·workspace 필터. 새로운 알림 센터나 별도 상태 엔진으로 중복 구현하지 않는다.

## 목표 데이터와 상태 정책

기존 public `AgentStatus` enum을 우선 유지하고 내부/선택 필드를 추가한다. 기존 CLI·GUI 조합에서 optional 필드 부재는 unknown으로 해석한다.

| 정보 | 제안 필드/의미 | 보관 범위 |
| --- | --- | --- |
| 소유권 | surface generation, session, process identity | 기존 runtime |
| 상태 근거 | hook / program-status / screen / process, event kind, received_at, applied_at, source sequence | 마지막 적용 근거 + bounded trace |
| wait | permission / question / auth / quota / api_failure / unknown; actor=user/external/agent; request ID 또는 batch/turn scope | 기존 wait map 확장 |
| turn 결과 | completed / failed / canceled / unknown | 현재·최근 turn. 테스트 성공과 별개 |
| attention | reason, target, opened_at, acknowledged_at, resolved_at, resolution_reason | 현재 pending은 알림 이력과 독립 |
| 관찰 가능성 | connected / disconnected / unconfirmed | 상태와 별도. heartbeat 부재를 실패로 단정하지 않음 |

판정 규칙:

- hook의 인과관계가 확인된 wait는 해당 request/scope 해제 또는 유효한 turn/session 경계 전까지 보존한다.
- 화면은 기존 보호 조건 아래에서만 보완한다. 근거 없는 신뢰도 백분율은 만들지 않는다.
- `identity_source`와 `status_evidence`를 분리한다. UI에는 “hook 확인”, “터미널 보고”, “화면 추정”, “연결 끊김”을 표시한다.
- 마지막 이벤트 경과 시간은 진단 정보다. 오래 조용하다는 이유만으로 Working을 Done으로 바꾸지 않는다.
- quota 자동 재시도는 “재시도 대기”이며 무조건 “사용자 입력 필요”에 넣지 않는다. 설정·로그인 등 실제 개입이 필요하면 행동 요청으로 올린다.
- 진단은 기존 trace에 수락/거절 이유와 상관 ID를 추가한다. 전체 프롬프트·transcript 저장은 기본 요구가 아니다.
- `flowmux agents --explain` 및 surface별 설명 조회는 **제안 명령**이다. 현재 명령으로 안내하지 않는다.

알림 ID는 `(surface_generation, session, turn, request_or_scope, event_kind)`를 기준으로 만든다. 같은 ID 재전송은 8초 이후에도 같은 항목으로 처리하고, 다른 turn은 8초 이내라도 구분한다. native ID가 없는 경우 adapter 내부 generation과 scoped ordinal을 쓰되 문자열 유사도만으로 요청을 합치지 않는다. 기존 OSC 9/99/777처럼 익명 알림은 한계가 있으므로 정확히 같은 반복만 제한된 시간창에서 억제하고, 넓은 Info↔완료 병합은 제거하는 방향을 검증한다.

상태 변경을 거절한 stale 이벤트에서는 알림 효과도 만들지 않도록 reducer의 수락 결과에 연결한다. 무한 journal 대신 현재 세션의 bounded receipt와 기존 종료 tombstone을 먼저 이용한다. 앱 재시작을 넘는 전달 보장이 필요하다고 확인될 때만 persistence를 확장한다.

## 예상 UI와 사용자 동작

**권장안은 기존 sidebar + agent bar + notification popover를 공통 attention projection에 연결하는 것이다.** 기본 화면에 상세 event 로그를 늘어놓지 않는다.

- workspace에는 “조치 2”를 표시한다. 벨의 “안 읽음 3”과 다른 숫자다.
- sidebar 상단은 승인·답변·인증 요청, 다음은 실패 확인, 결과 확인, 진행 중 순서다. 동일 우선순위는 최초 대기 시각으로 안정 정렬한다. 도구 이벤트마다 순서를 바꾸지 않는다.
- 행은 두 줄: `Claude · 결제 모듈` / `승인 필요 · 2분 대기`. 읽은 요청에도 warning 색상과 “승인 필요”를 유지한다. 작은 unread 점만 사라진다.
- child 요청은 부모의 `하위 요청 1`에 합산하고 펼쳐 정확한 child로 이동한다. child 완료를 매번 상위 완료 알림으로 올리지는 않는다.
- 클릭하면 정확한 workspace/pane/surface로 이동하고 알림을 읽음 처리한다. 이 동작은 요청을 해결하지 않는다.
- 해결은 일치하는 hook/provider 응답으로만 처리한다. 앱에 일반 “승인” 버튼을 만들지 않는다. 요청 ID·권한 있는 provider API를 확보한 별도 통합만 실제 승인 UI 후보가 된다.
- 완료 문구는 “응답 완료 · 결과 확인”이다. 검증 성공이라는 체크마크나 “작업 성공”은 별도 증거가 있을 때만 표시한다.
- 상세 펼치기는 마지막 근거, 이벤트 종류, 경과 시간, 미해결 요청 수, 연결 상태, 대상 위치를 보여준다. 민감한 원문은 기본 숨김이다.
- 닫힌 대상은 “세션 종료”, 연결이 끊긴 원격 대상은 “연결 끊김”으로 표시한다. 클릭해서 새 세션을 자동 생성하거나 오래된 요청을 실행하지 않는다.
- 목록은 keyed update로 바꿔 선택/포커스/스크롤을 유지한다. 기존 agent bar 표시 설정과 notification 필터를 보존한다.

대안은 notification popover에 “조치 필요” 탭을 더하는 방식이다. 화면 점유는 적지만 workspace를 넘나드는 대기 요청이 숨겨지므로, 기본 권장은 sidebar 우선이고 popover는 동일 데이터의 보조 진입점이다.

## OSC 7501: 후속 호환성 단계

현재 `flowmux-notify`는 OSC 9/99/777을 처리하며 7501은 확인되지 않았다. 표준은 Program status rev 0.4를 기준으로 검토한다. 상태 레코드는 heartbeat가 아니며 단순 무응답 timeout으로 만료시키면 안 된다. process/shell 경계와 root/child clear 규칙이 필요하다.

1. 기존 stream parser의 임의 chunk 처리를 재사용하되 notification과 program-status를 별도 타입으로 파싱한다.
2. 규격의 크기·record 수·제어문자 제한을 적용하고 unknown key를 안전하게 처리한다. root/child 관계와 clear를 구현한다.
3. 지원되는 CLI/GUI 조합에서만 capability를 광고한다. 파싱만 가능한 상태에서는 probe에 완전 지원을 답하지 않는다.
4. PTY tee의 안쪽 프로세스에 probe 응답을 정확히 한 번 전달한다. renderer 중복 응답, 종료 후 shell에 응답 문자열 유입, SSH capability 불일치를 검증한다.
5. hook의 미해결 request와 충돌하는 OSC는 기존 인과관계 규칙으로 제한한다. source·turn 경계·중간 working 관측 없이 오래된 done/idle로 wait를 지우지 않는다.
6. OSC가 terminal title/glyph 동작을 바꿀 수 있으므로 기존 화면 fallback도 실제 agent에서 재검증한다.

프로토콜의 terminal 보고는 사용자가 실행한 프로그램의 출력이며 승인 권한이나 작업 성공 증명이 아니다. Flowmux의 기존 hook 및 화면 판정과 호환되는지 별도로 검증한다.

## 구현 단위와 예상 일정

한 명의 개발자와 독립 리뷰, 기존 테스트 환경 사용을 가정한 추정이다. 버전 호환성 재현 결과로 조정한다. 문서·mockup은 이 계획의 결과물이며 아래 구현을 완료했다는 뜻이 아니다.

| 단계 | 독립 변경 단위 | 주요 파일 | 완료 기준 | 예상 |
| --- | --- | --- | --- | --- |
| A | 현재 동작 fixture와 버전/지원 표 고정 | lifecycle tests, hook_install tests, notification tests | 읽음/병렬 wait/stale 알림/timeout 차이 재현과 기대 동작 합의 | 1–2일 |
| B | pending과 read 분리 | core rollup, ui/mod.rs, agent_bar.rs | 읽어도 조치 수·경고 유지; matching resolve에만 감소 | 1–2일 |
| C | 상태 근거·wait reason·outcome 전달 | core, protocol, agent_runtime, cmd_hooks, hooks | 화면 추정/실패/취소/종료 구분; 이전 optional 필드 호환 | 2–3일 |
| D | 원인 ID 기반 notification admission/dedup | ipc_handler, notifications, notification_commands | 같은 이벤트 1건, 다른 turn 별도, 거절된 stale event의 toast 0건 | 2–3일 |
| E | 공통 projection와 안정적인 주의 UI | activity, sidebar, agent_bar, notification_popover, config | 같은 원인·건수 표시, 키보드/스크롤 유지, 정확한 대상 이동 | 2–3일 |
| F | 실제 agent·native GUI 검증 및 성능 측정 | 기존 hook/notification GUI scripts, macos_native | fixture와 실제 agent 결과를 분리 기록, Linux/macOS 회귀 통과 | 2–3일 |
| G | OSC 7501 + capability + 실제 호환 검증 | notify stream/osc, pty_tee, IPC, runtime | chunk/probe/process/SSH/기존 fallback 회귀 통과 | 추가 3–5일 이상 |

의존성: A → B/C → D → E → F. G는 C/D 이후 별도 배포 범위로 진행한다. 각 단계는 검증 가능한 커밋 단위로 쪼갠다. MVP는 약 10–16 개발일, OSC 포함은 약 13–21일을 초기 범위로 본다. agent 버전별 문제나 두 플랫폼 실환경이 준비되지 않으면 더 필요하다.

## 검증과 출시 판정

필수 시나리오:

1. pending 2개 중 1개를 읽고 1개만 해결: 조치 수는 읽음 때 2, 해결 때 1.
2. 이전 turn Stop이 새 prompt 뒤 도착: 새 작업은 Working 유지, 잘못된 완료 알림 없음.
3. 같은 request/event 재전송이 8초 이후 도착: row/toast 중복 없음. 다른 turn의 완료가 8초 이내 도착: 각각 보존.
4. call ID 없는 permission + batch 경계, AskUserQuestion ID wait, 병렬 tool과 permission을 각각 재생.
5. StopFailure quota/API, Interrupt, 정상 Stop: 재시도 대기/실패/취소/응답 완료를 구분.
6. root/child stop 역순, 일부 child 이벤트 누락, child 질문: root 조기 완료와 요청 유실 없음.
7. 화면상 과거 footer, 오래 조용한 실행, SSH 재접속, 죽은 PID: 과거 화면으로 상태 부활·완료하지 않음.
8. focus된 요청/숨겨진 workspace/이동한 surface/닫힌 surface/scrollback: 현행 focus 정책과 정확한 이동 보존.
9. 알림 이력 50건 초과: 이력 retention 때문에 미해결 요청이 사라지지 않음.
10. hook 미지원/버전 탐지 실패/사용자 hook 공존: 기존 handler를 삭제하거나 agent 설정 경고를 만들지 않음.
11. OSC split packet/잘못된 payload/중복 probe/프로세스 교체/원격 미지원: crash, 잘못된 상태, shell 입력 오염 없음.
12. UI 갱신 중 키보드 선택·스크롤 보존. 다수 agent의 동일 상태 ping으로 unread 증가·행 재정렬 없음.

검증 층위는 순서대로 reducer unit/replay → isolated GUI fixture → 설치된 실제 agent의 동일 시나리오 → Linux/macOS CI다. 기존 `scripts/test-agent-hooks-gui.py`는 합성 hook와 fixture 화면을 사용하므로 실제 에이전트 end-to-end 증거로 대신하지 않는다. `scripts/test-notifications-gui.py`, native macOS main-thread checks, `scripts/test-ci.sh`를 변경 범위에 맞게 사용한다. 활성 사용자 세션은 종료하지 않는다.

정량 목표는 아직 측정값이 아니다: 정의한 회귀 세트에서 요청 유실·잘못된 완료·중복 causal 알림 0건, 로컬 IPC 수신→화면 반영 p95 250ms 이하를 초기 예산으로 측정한다. 100개 관찰 대상에서 기준선과 CPU/메모리/갱신 지연을 비교하고 예산을 확정한다. 사용성 확인은 승인·질문·실패·완료를 2초 내 구분하고 한 번의 선택으로 원래 요청에 도달하는 것을 목표로 한다.

구현은 A–F 이후 G까지 순차 검증한다. 상태 이유가 설명되고 미해결 요청이 읽음으로 가려지지 않는 것이 성공 기준이며, 지원 hook 수나 화면 배지 수의 증가 자체를 목표로 삼지 않는다.
