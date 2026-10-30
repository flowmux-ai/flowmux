<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Flowmux Session Flow 테스트와 검증 계획

상태: 구현·검증 진행 중. 아래 항목은 수용 조건이며 통과 목록이 아니다. 작성일: 2026-10-31. 요구사항은 [설계 문서](agent-session-flow-design.md)의 R01–R14를 기준으로 한다.

검증은 모델의 정확성, 실제 provider 계약, native GUI 동작, 기존 기능 회귀, 재시작·실패 복구, 최종 CI를 각각 증명한다. Fixture와 목업 성공으로 실제 fork/resume 성공을 대체하지 않는다.

## 공통 시험 데이터

재현 가능한 fixture를 만들고 동일 key·event sequence를 단위 테스트와 GUI harness에서 공유한다. 실제 provider 검증에는 별도의 작은 저장소와 비민감 대화를 사용한다.

| Fixture | 구성 |
| --- | --- |
| F1 | Claude A0→A1→A3, A0→A2. A2는 기록 삭제, 관계는 남음. Workspace W에 A1만 열림 |
| F2 | Codex B0→B1→…→B20. W에는 B7만 열림. B20은 원격 연결 없음 |
| F3 | Gemini C0→C1→C2. 최초 W에는 없음. 이후 C1을 resume |
| F4 | 다른 프로필에 같은 native ID, 다른 host에 같은 ID, 서로 무관한 같은 제목의 세션 |
| F5 | 누락된 parent, 잘린 metadata, subagent 관계, 순환, 자기 참조, 두 부모 충돌 |
| F6 | 1,000/10,000 노드, 20 component, 깊이 1,000 chain, 긴 한국어·영어·RTL·emoji 제목 |
| F7 | 분할된 terminal, 비활성 tab, 실행 중 agent, 입력 중 draft, browser, unsaved editor, file/worktree/session panels |

F1+F2의 W에는 2개 component, 25개 session 노드가 보여야 한다. A3을 추가로 열어도 2개/25개를 유지한다. C1을 resume하면 3개/28개가 된다. 재개 불가 A2와 B20도 노드 수에 포함되지만 실행 대상이 되지 않는다.

## 모델과 저장 테스트

| ID | 요구 | 입력 또는 실패 주입 | 관찰할 결과 |
| --- | --- | --- | --- |
| D01 | R03 R06 R07 | F1+F2의 A1/B7을 anchor로 조회 | 조상·형제·자손 전체 포함, 정확히 2개/25개 |
| D02 | R07 | A3도 열린 상태로 추가 | 같은 component 중복 0, graph edge 중복 0 |
| D03 | R06 R07 R13 | C1 resume, A1→C1 session 교체 | 새 계보 추가; 다른 A anchor가 없을 때만 A component 제거 |
| D04 | R04 R13 | A1을 inactive tab에 두고 활성 tab은 shell | 흐름 버튼 유지. Inactive tab 제거 후 anchor 재계산 |
| D05 | R06 R10 | A0 기록 삭제 또는 parent metadata만 존재 | placeholder 유지, 연결 보존, 재개 비활성 |
| D06 | R03 R06 | F4의 같은 native ID | host/profile별 별개 key; 잘못된 병합 없음 |
| D07 | R03 | Native parent가 subagent를 의미 | fork edge 생성 안 함; 별도 정보 또는 미지원 처리 |
| D08 | R06 | F5의 cycle/self-edge/부모 충돌 | 무한 탐색·오염 전파 없음; 충돌 이유와 partial 상태 |
| D09 | R03 R14 | 동일 fork event·metadata 반복 | idempotent upsert; edge·operation 수 불변 |
| D10 | R14 | transaction commit 전 종료, commit 후 종료 | 부모·자식 관계와 operation이 반쪽 저장되지 않음 |
| D11 | R12 R14 | 새 native ID 확인 후 저장 오류 | 성공 완료 없음, 재조정 가능, 재fork 없음 |
| D12 | R13 | workspace/surface generation 변경 후 이전 조회 응답 | 늦은 응답의 UI·launch 효과 0 |
| D13 | R14 | 미래 schema, 손상 DB, read-only 경로 | 기능 오류 설명; 기존 terminal/session 기능 정상 |
| D14 | R06 | 대량 기록 일부 page만 로드 | partial·개수 표시; 전체 완료 허위 표시 없음 |

실제 native 저장소를 수정하는 단위 테스트는 금지한다. 임시 디렉터리의 읽기 전용 JSONL/SQLite fixture로 파서와 revision 변경을 검증한다.

## Fork와 resume 동작 테스트

| ID | 요구 | 시나리오 | 관찰할 결과 |
| --- | --- | --- | --- |
| O01 | R01 | shell/browser/editor 또는 fork 미지원 버전 우클릭 | Fork 비활성 또는 에이전트 메뉴 없음; 실행 0 |
| O02 | R01 R13 | 메뉴를 연 뒤 tab/session 교체 후 클릭 | 원래 target mismatch 거절; 새 focused 세션에 실행 안 함 |
| O03 | R02 R03 R12 | A1에서 fork | 원본 A1 유지, 다른 ID 자식, 오른쪽 split, edge 1개, child focus |
| O04 | R02 R11 | F7의 중첩 split에서 fork | 클릭한 leaf만 분할; 다른 widget/PTY/process identity 유지 |
| O05 | R03 | 자식에서 다시 fork | 실제 부모가 자식으로 기록, root에 잘못 붙지 않음 |
| O06 | R09 | 닫힌 A0를 graph에서 선택 | 마지막 아래 pane 오른쪽에 resume; fork edge 추가 없음 |
| O07 | R09 | 이미 열린 A1을 여러 번 선택 | 기존 surface focus; 프로세스·pane 증가 0 |
| O08 | R09 R12 | 다른 workspace/창에서 같은 key 실행 | 기존 실행 위치 확인·이동; 중복 writer 0 |
| O09 | R09 R12 | 두 창에서 동시에 같은 세션 resume | 한 launch만 허용; 다른 요청은 기존 실행 또는 진행 상태로 연결 |
| O10 | R12 | 동일 원본 fork 빠른 반복, timeout 후 재클릭 | 진행 중 중복 호출 0; outcome unknown을 자동 재실행하지 않음 |
| O11 | R12 | native fork 성공, GTK attach 실패 | child/edge 보존; 재시도는 resume; 추가 child 0 |
| O12 | R02 R12 | dispatch 전 spawn 실패·pane 생성 실패 | 원본 보존; 작업 소유의 빈 임시 pane만 정리 |
| O13 | R12 | 정리 직전 사용자가 임시 pane에 입력·새 tab 추가 | 사용자 작업이 생긴 pane을 자동으로 닫지 않음 |
| O14 | R09 R10 | cwd 삭제, binary 미설치, profile 불일치 | preflight 거절, 불가 이유, 잘못된 default profile 실행 0 |
| O15 | R12 | resume 명령이 새로운 native session으로 fallback | ID mismatch로 실패 표시, resume 성공 주장 없음 |
| O16 | R01 R12 | 진행 중 턴을 안전하게 snapshot할 수 없는 provider | fork 비활성; 원본 interrupt·rewrite 없음 |
| O17 | R09 | spawn 직전 선택 대상 pane 닫기·이동 | stale target 거절; 다른 pane으로 조용히 재지정 안 함 |
| O18 | R02 R09 | split 최소 크기 미달 | 명령 실행 전 안내; 자동 tab fallback·기존 pane 닫기 없음 |
| O19 | R12 R14 | 오래된 PID 재사용, owner 사망, owner 조회 불가 | process start identity 검증; 불명확 시 중복 launch 없음 |
| O20 | R09 R12 | session title/path의 따옴표·개행·제어문자 | 표시 escape/길이 제한; shell injection·추가 Enter 없음 |
| O21 | R09 R12 | Flowmux 밖 동일 세션 실행 흔적·writer 충돌이 있으나 소유권 불명확 | Flowmux binding 부재를 실행 없음으로 오해하지 않음; 소유권 확인 전 자동 중복 resume 없음 |

Operation tests는 결과가 확정되는 단계와 외부 side effect 발생 단계를 구분한다. 테스트 double의 fake child ID는 orchestration 테스트 증거이며 실제 provider 계약 증거가 아니다.

## Native UI와 입력 테스트

| ID | 요구 | 조작 | 관찰할 결과 |
| --- | --- | --- | --- |
| U01 | R04 R05 | 기록 없는 W에서 첫 fork, 흐름 버튼 클릭 | 성공 후 버튼 생성, 자동 panel open 없음, 클릭 후 상단 약 1/3 |
| U02 | R05 R11 | panel 열기·닫기 50회 | 아래 pane 분할·PID·draft·scroll·side panel 유지 |
| U03 | R05 | workspace A/B에서 서로 다른 panel 높이·camera | workspace별 상태 복원, 다른 workspace 데이터 유입 없음 |
| U04 | R08 | 커서 위치 여러 곳에서 wheel in/out | 커서 아래 world 좌표 유지, 배율 clamp, 그래프 밖 휠 동작 보존 |
| U05 | R08 R09 | 빈 공간·노드 위 5px 이상 drag | 빈 공간은 pan, 노드는 force 배치 이동; resume/fork 0 |
| U06 | R08 R09 | 이동 모드에서 노드 클릭·drag | pane 변화 0; 선택 모드 복귀 후 click만 open |
| U07 | R08 | drag 중 pointer cancel, 창 focus loss, Escape | capture 해제; 다음 클릭·휠 정상, stuck drag 없음 |
| U08 | R08 | 20–300% 배율, Retina/일반 scale에서 노드 클릭 | 보이는 노드와 hit target 일치, 옆 노드 오작동 없음 |
| U09 | R08 | 긴 chain과 여러 component에서 전체 맞춤 | 전체 bounds 포함, component 간 겹침 없음 |
| U10 | R10 | disabled A2/B20 mouse·Enter·Space | split/launch 0, 이유와 비활성 접근성 상태 유지 |
| U11 | R09 | graph node pointer click와 keyboard Enter | 닫힌 노드는 split resume, 열린 노드는 정확한 tab focus |
| U12 | R11 R13 | pan 중 새로운 fork/status/identity event | camera 유지, drag 상태 보존, 구조 변화만 layout 갱신 |
| U13 | R08 R10 | 축소·테마 변경·큰 글꼴·긴 제목 | 의미 구분·불가 표식 유지, markup 실행 없음, 텍스트 중첩 최소화 |
| U14 | R08 | trackpad pinch/scroll, mouse wheel, 가운데 버튼 | OS별 입력 의미 일치, 과도한 점프·앱 전체 확대 없음 |
| U15 | R04 R05 | 비활성 workspace 아이템의 흐름 버튼 클릭 | 해당 workspace의 그래프와 pane을 일관되게 표시 |
| U16 | R11 | file/worktree/session panel, browser media, unsaved editor와 동시 사용 | 각 패널 상태·미저장 내용·브라우저 상태 보존 |

키보드 탐색의 접근성 이름에는 세션 제목, 도구, 열린 위치 또는 재개 불가 이유를 포함한다. 자동화된 hit-test만으로 접근성 검증을 대체하지 않고 macOS VoiceOver와 Linux의 사용 가능한 접근성 도구로 주요 동작을 확인한다.

## Force 그래프와 공유 계보 검증

| ID | 요구 | 시나리오 | 관찰할 결과 |
| --- | --- | --- | --- |
| G01 | R06 R07 | 서로 다른 workspace에서 같은 계보의 다른 세션을 열기 | 동일한 node/edge/기본 배치, 서로 다른 선택·camera |
| G02 | R08 | 노드를 drag하고 놓기 | 인접 노드가 제한된 힘으로 반응, 수렴 후 tick 중단, 좌표 유한 |
| G03 | R08 R11 | 패널 숨김·애니메이션 감소·빈 그래프 | 불필요한 frame tick 없음, 기존 terminal 입력 정상 |
| G04 | R08 | 우주 배경과 밝은/어두운 테마, disabled·selected 노드 | 배경 대비와 상태 가독성, 색상 외 표식 보존 |
| G05 | R06 R07 | 한 workspace에서 노드 이동 후 같은 계보의 다른 workspace 열기 | 세션 배치는 공유, 포커스와 camera는 독립 |

## 실제 provider 검증

각 adapter는 다음 기록을 남긴다. 지원되지 않은 기능은 `unsupported`, 실행하지 않은 검증은 `not_run`으로 남긴다. 둘을 실패나 성공으로 합치지 않는다.

| 필드 | 기록 내용 |
| --- | --- |
| 환경 | OS, architecture, provider binary 경로·버전, profile/store namespace, 저장소 경로 |
| Capability | discover / read lineage / resume / native fork / child ID correlation / open live session |
| 관계 증거 | 원본 ID, child ID, fork 경계, provider 응답 또는 읽기 전용 metadata 근거 |
| 실행 증거 | 요청 ID, 새 pane/surface ID, 실제 process identity, 최종 observed session ID |
| 보존 증거 | 원본이 독립적으로 계속 사용 가능, 이전 draft 유지, 임의 파일 변경 없음 |
| UI 증거 | 그래프의 위치·관계·활성화 상태, 정확한 split, 노드 열기 후 화면 |
| 결과 | passed / failed / blocked / unsupported / not_run, 원인과 로그 경로 |

필수 실제 시나리오:

1. 기존 대화 A를 준비하고 marker가 있는 작은 대화 맥락을 만든다.
2. Flowmux 메뉴에서 fork B를 만들고 서로 다른 native ID인지 확인한다.
3. B가 이전 맥락을 이어받았는지 확인한다. A와 B에 각각 새 대화를 추가해 독립 저장을 확인한다.
4. B에서 C를 분기하고 provider 기록과 그래프 A→B→C가 일치하는지 확인한다.
5. B를 종료하고 그래프에서 resume해 **B의 같은 ID**로 열리는지 확인한다.
6. Provider 자체 기능으로 Flowmux 밖에서 만든 별도 D 계보의 자식을 resume한다. 조상·형제·자식 수집과 독립 component 표시를 확인한다.
7. Native 기록 일부 삭제 또는 테스트용 remote 연결 해제를 적용하고 비활성 노드와 연결 보존을 확인한다.
8. 이전 workspace에서 이미 실행 중인 B를 다른 workspace에서 선택하고 중복 실행이 없는지 확인한다.

Claude·Codex·OpenCode의 one-click fork는 이 시나리오를 통과해야 활성화한다. Gemini·agy는 독립 분기와 비파괴 호출 경로까지 증명된 경우에만 동일하게 활성화한다. Cline은 native fork 계약이 확인되기 전까지 resume/관계 조회를 개별 검증하며 fork를 지원으로 표시하지 않는다.

Agent의 실제 호출은 로그인·권한·사용량을 필요로 할 수 있다. 실패한 인증이나 없는 binary는 정확한 blocker로 남기며 fixture 결과로 통과시키지 않는다. 검증 중 기존 사용자 에이전트에 메시지를 보내거나 입력을 주입하지 않는다.

## 플랫폼별 격리와 회귀

- 사용자 실행 앱과 agent를 종료하거나 재시작하지 않는다. 별도 Flowmux instance, 별도 state/data/config와 테스트용 profile을 사용한다.
- macOS는 짧은 `/tmp` runtime 경로와 GTK/AppKit main-thread harness를 사용한다. 일반 `cargo test --workspace`만으로 native GUI 통과를 선언하지 않는다.
- Linux는 공통 gate의 지원된 GUI 환경에서 native 입력을 검증한다. X11/Wayland를 구분해 결과에 기록한다.
- 로컬 GUI에서 보이는 상태뿐 아니라 domain pane tree, surface identity, provider ID를 함께 대조한다.
- 기능 비활성 상태에서도 기존 우클릭 Copy/Paste/Split/Close, session new-tab resume, tab 이동, sidebar 선택, 알림의 exact-surface 이동을 검증한다.
- 지원되는 모든 기존 resume 도구에서 홈·설정 override, 로그인 shell PATH, 공백이 있는 cwd, 다른 프로젝트 resume을 회귀 시험한다.

기존 실행 명령:

```sh
cargo fmt --all -- --check
cargo check --workspace
cargo clippy --workspace --all-targets -- -D warnings
bash scripts/test-ci.sh linux
bash scripts/test-ci.sh macos
```

두 OS gate는 각 OS에서 실행한다. 초기에는 변경된 headless crate의 관련 테스트만 먼저 실행하고, native 수정 후 해당 main-thread harness와 공통 gate를 실행한다. 이 문서를 작성하면서 위 구현 테스트를 실행했다는 의미는 아니다.

새 GUI harness를 공통 gate에 추가하면 `scripts/test-ci-runner.py`의 phase/mock/count도 함께 갱신한다. [Test](../.github/workflows/test.yml), [Sanitizers](../.github/workflows/sanitizers.yml), [License audit](../.github/workflows/license-audit.yml)의 최종 remote 결론을 확인한다. 코드·의존성이 바뀌지 않은 문서 작업에 제품 전체 빌드를 요구하지 않는다.

## 성능 측정

측정 전에 OS·CPU·메모리·화면 scale·빌드 모드·데이터 크기를 고정한다. Cold discovery와 warm projection/render를 따로 기록한다. 평균만 기록하지 않고 p50/p95와 최대 정지 시간을 남긴다.

| 시험 | 방법 | 초기 목표 |
| --- | --- | --- |
| Warm projection | F6 1,000노드·20 component, 최소 30회 | 최초 scene 준비 p95 ≤500 ms |
| Pan/zoom | 10초 연속 입력을 5회, 1,000노드 | frame p95 ≤33 ms, 입력 응답 ≤100 ms |
| 장기 사용 | 30분, panel toggle 100회, workspace 전환 100회 | crash·stuck input·노드/handler 지속 증가 없음 |
| 큰 그래프 | 10,000노드와 깊이 1,000 | stack overflow 없음, cancellable discovery, viewport culling 동작 |
| Concurrent UI | 상태 event와 metadata page 로딩 중 입력 | GTK에서 blocking 파일 I/O·DB 대기 없음 |

위 수치는 목표이며 현재 측정 결과가 아니다. 큰 그래프에서 목표를 못 맞추면 데이터가 빠진 것을 숨기지 않고 partial 상태와 후속 작업을 남긴다. 메모리 측정은 warm-up 후 retained object/handler 수와 RSS 추이를 함께 본다. 짧은 성공 run으로 장기 누수 없음이라고 결론내리지 않는다.

## 증거와 완료 판정

새 검증 결과는 `output/session-flow-validation/<run-id>/` 아래에 manifest, 로그, 최종 화면, provider 관계 증거를 묶는 방식을 제안한다. Manifest에는 코드 commit, build 정보, OS/provider 버전, test ID, 기대·실제 결과, artifact 경로와 blocker를 기록한다. 개인 대화·인증 토큰은 기록하지 않는다.

제안 marker는 `SESSION_FLOW_FOREST_OK`, `SESSION_FLOW_SPLIT_RESUME_OK`, `SESSION_FLOW_NATIVE_FORK_OK`, `SESSION_FLOW_CAMERA_OK`다. Harness 구현 전에는 기존 명령이나 이미 얻은 결과처럼 사용하지 않는다.

릴리스 체크:

- R01–R14마다 적어도 하나의 모델/GUI 테스트와 필요한 실제 도구 증거가 연결되어 있다.
- D/O/U 매트릭스의 적용 가능한 필수 항목이 통과했다. Unsupported 기능은 UI에서도 비활성이다.
- Native fork와 resume은 예상 ID·분기 관계·최종 pane을 확인했다.
- 원본·기존 세션·미저장 작업 보존과 실패·timeout·재시작 복구를 통과했다.
- Linux/macOS native gate와 최종 remote CI가 성공했다. 미수행 OS나 adapter를 전체 지원에 포함하지 않는다.
- 기능을 끈 뒤 기존 세션이 계속 동작하고, 구버전에서도 기존 세션 저장소를 읽을 수 있다.
- 목업, fixture, local test, real provider, native GUI, remote CI 결과를 구분해 출시 기록에 남겼다.

HTML 목업은 초기 상호작용 설계 자료다. 아래 실행 기록은 구현 이후 확보한 증거이며, 전체 매트릭스의 통과 여부와 구분한다.

## 실행 기록

2026-10-31 기록. 아래 결과는 위 전체 D/O/U/G 매트릭스를 모두 통과했다는 의미가 아니다.

| 범위 | 결과 | 증거 |
| --- | --- | --- |
| Core 모델 | 178 passed; 1만 노드·20개 계보, 긴 chain, projection, cycle/namespace, geometry | `/tmp/flowmux-session-flow-domain-tests.log` |
| State | 79 passed 및 별도 integration 통과; transaction, 동시 writer, PID 재사용, stale refresh, native 증거 기반 복구 | 같은 domain log |
| Daemon | 211 passed, 기존 ignored 1개; source/surface 검증과 증분 split | `/tmp/flowmux-session-flow-headless.log` |
| 정적 검사 | workspace check, all-target clippy `-D warnings`, format, diff check 통과 | `/tmp/flowmux-session-flow-check.log`, `/tmp/flowmux-session-flow-clippy-final.log` |
| Hook·capability·해제 | Codex installer 25 passed/기존 ignored 1개; stderr help/실패 응답 및 제한된 PATH, fork callback registry 해제 테스트 통과 | `/tmp/flowmux-session-flow-hook-tests.log`, `/tmp/flowmux-session-flow-capability-final.log`, `/tmp/flowmux-session-flow-weak-registry.log` |
| GTK canvas·배치 | `MACOS_SESSION_FLOW_LAYOUT_OK`, `MACOS_NATIVE_SMOKE_OK`; widget/PID 보존, wheel 좌표, drag/Pan/비활성 노드, 50회 toggle, widget 해제, workspace 전환 중 drag 취소 | `/tmp/flowmux-session-flow-native-ui-final.log` |
| 실제 Codex 0.166.0 | `MACOS_SESSION_FLOW_CODEX_REAL_OK`; 첫 요청 자동 전송 없이 identity 식별, fork/split/resume/중복 방지 | `/tmp/flowmux-session-flow-real-ui-final.log` |
| 실제 Claude Code 2.1.314 | source `8683a908-3515-4cc9-a35a-1da44cf7a563` → child `21fcba07-00f6-4ea1-9749-24b6cbde520d`; `FLOW_NATIVE_731` 상속; resume 같은 child ID | `/tmp/flowmux-session-flow-claude-evidence.json` |
| 실제 OpenCode 1.17.18 | source `ses_e720557a7ffehEpgE0O7ocPC90` → child `ses_e7202b093ffeGhjCBP9bu4Du0H`; 원본 메시지 복제. 모델 호출은 401로 blocked | `/tmp/fm-flow-providers-h_ja_8_6/opencode*.log`, 격리된 SQLite |
| 라이선스 생성 | 고정된 cargo-about 0.9.2로 재생성 후 committed notices와 차이 없음 | `/tmp/flowmux-session-flow-licenses.log` |
| 전체 macOS gate | 화면 잠금(`CGSSessionScreenIsLocked=Yes`) 상태에서 browser-control의 stable pointer 및 editor-keyboard의 window active 대기 실패. 기존 설치 버전에서도 browser 실패 재현. 별도 실행한 remaining 14개 중 13개, tail 21개 중 17개 통과. 실패 목록은 아래 참고; 전체 통과 아님 | `/tmp/flowmux-session-flow-macos-gate.log`, `/tmp/flowmux-session-flow-browser-baseline.log`, `/tmp/flowmux-session-flow-macos-remaining.log`, `/tmp/flowmux-session-flow-macos-tail.log` |
| Linux coverage·원격 CI | `6fd9157b` Test·Sanitizers 실행 중, License success. 이전 `738b1e8d`에서 lines 82.26%, regions 82.33%, functions 81.12%로 기준 충족했으나 테스트 2건 실패. registry 강한 참조와 env PATH 조회 문제를 수정하여 재실행 | Test `41106537904`, Sanitizers `41106543517`, License `41106548949`; `/tmp/flowmux-session-flow-linux-job.log` |

Native GUI는 별도 `/tmp/fm-native-*` XDG/runtime 및 테스트 소켓을 사용했다. Codex는 임시 history/config profile을 사용했다. Claude는 임시 프로젝트의 새 session ID와 기존 로그인을 사용했으며, `--setting-sources ''`, 빈 MCP 설정, 도구 비활성으로 기존 사용자 hook·도구 실행을 제외했다. 사용자 세션에 입력을 보내거나 사용자 앱을 종료하지 않았다.

Writer 조정은 Flowmux가 관측한 실행에 대한 보장이다. 다른 앱의 실행을 원자적으로 잠그는 기능은 아니다. 인증 실패, 미지원 외부 fork, 수동 접근성·장기 성능 미수행은 `passed`로 바꾸지 않는다.


최신 코드의 로컬 회귀 보강(`6fd9157b`): fork callback은 weak registry를 사용해 pane tree 해제를 방해하지 않는다. Provider PATH에 시스템 명령이 없더라도 capability probe는 `/usr/bin/env`로 실행된다. Linux가 발견한 두 실패를 수정한 뒤 해당 로컬 테스트와 전체 all-target clippy를 통과했다. 기존 Linux 실패 run은 수정 후 통과 증거로 대체하지 않는다.

화면 잠금 상태에서 남은 재실행 항목:

- `browser-control`: 안정적인 pointer target 대기 실패; 기존 설치 버전도 동일.
- `editor-keyboard`: native keyboard window active 대기 timeout.
- `markdown`: PNG renderer timeout; native Markdown smoke 자체는 통과.
- `webdriver-native`: pointer fixture 표시 실패.
- `webdriver`: automation window가 native focus를 얻지 못함.
- `browser-chrome`: 입력 후 예상 DOM field focus가 비어 있음.

잠금 해제 후 `bash scripts/test-ci.sh macos`로 전체 gate를 다시 실행한다. 이 실패들이 모두 잠금만으로 발생했다는 결론은 재실행 전까지 확정하지 않는다. 사용자 앱 PID `72789`는 실행 중임을 확인했으며 종료·재시작하지 않았다.

추가 입력 검증: `/tmp/flowmux-session-flow-pan-input-final.log`에서 native harness의 `MACOS_SESSION_FLOW_LAYOUT_OK`와 `MACOS_NATIVE_SMOKE_OK`를 확인했다. GTK gesture/key signal을 통한 Space·가운데 버튼 팬, 누적 pinch 배율과 커서 기준점, Escape 이후 늦은 drag-end의 열기 억제를 검사했다. 실제 하드웨어 트랙패드/키보드 검증을 대신하는 증거는 아니다. 캡처는 `docs/design-assets/agent-session-flow-native.png`이며 실제 provider 데이터가 아닌 fixture다. All-target clippy는 `/tmp/flowmux-session-flow-input-clippy.log`에 기록했다.

실제 메뉴 경로 검증: `/tmp/flowmux-session-flow-context-menu.log`에서 native terminal의 secondary-click gesture가 구성한 `Fork session` 버튼의 미지원 상태 비활성을 확인했다. `/tmp/flowmux-session-flow-real-menu.log`에서는 동일 버튼의 활성 상태 및 clicked callback → bridge → fork → split → child 종료 → 같은 ID resume을 통과했다. source `01a18ddc-a51f-7472-ab63-045abb779243`, child `01a18e15-da1a-7362-b196-a1276bd3fdda`. 모델 요청을 자동 입력하지 않았으며 원본 widget/PID가 유지됐다. 이 검사는 GTK gesture/button signal 경로이며 물리 마우스 입력 검증과는 구분한다.

메뉴 대상 보존 회귀: `/tmp/flowmux-session-flow-stale-menu.log`의 native fixture는 메뉴를 연 후 같은 surface의 session ID와 catalog binding을 교체한다. 원래 key를 유지한 메뉴가 `Source session changed`로 거절되고 pane 및 pending operation이 증가하지 않음을 확인했다. UI status의 일시적 변경은 notify 이벤트로 관측하여 이후 refresh에 덮여도 검증을 잃지 않는다. `/tmp/flowmux-session-flow-real-menu-key.log`에서는 정상 메뉴 fork/resume도 다시 통과했다(child `01a18e19-e39a-73f0-a62b-d8cfd06553f6`). Callback 해제 테스트와 all-target clippy도 통과했다.
