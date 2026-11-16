<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Agent runtime 구현 검증과 액션 아이템

기준: `origin/main..agent/runtime-plan-verification`의 managed Agent Host 변경과
[활성 Agents 설계](active-agent-runtime-design.md). 사용자 실행과 설치본은 유지하고,
별도 XDG state/runtime·D-Bus·display에서 검증한다. 액션 아이템마다 수정과 회귀
검증을 하나의 커밋에 담는다. 최종 심각도 판정은 관측한 결과에만 적용한다.

## 검토 기록

### AR-001 · medium · GUI가 없는 동안 provider 화면 판정 누락

- 원인: Gemini·Antigravity의 잠정 Stop을 확정하는 ready 화면 판정이 GUI의
  `StateStore` 경로에만 있었다. Host는 Codex grace만 처리했다.
- 재현: 수정 전 `test-agent-native-hooks-gui.py`가 GUI 종료 후 Gemini의
  stable ready 화면에서 Idle 전환을 기다리다 실패했다.
  `/tmp/fm-headless-screen-before-live.log`, private root `/tmp/fm-gui-xyrxj82t`.
- 조치: 기존 화면 판정을 owner 공통 runtime으로 옮겨 GUI와 Host가 함께 사용한다.
  Host는 기존 terminal emulator의 최근 live 80행·현재 title을 관측한다.
  ANSI style·과거 scrollback·다른 화면의 내용은 판정에 섞지 않는다.
- 보존: native session·wait·sequence·600ms 안정화·continuation·outcome 및 알림
  중복 제거. UI 완료는 전체 provider 완료로 승격하지 않으며 자동 종료하지 않는다.
- 검증: daemon library 267개 통과·1개 ignored. 수정 후 격리 live 27개 통과·exit 0
  (`/tmp/fm-headless-screen-after-live.log`, `/tmp/fm-gui-ccvk5d5b`).
  재시작한 GUI의 Agents 완료·실패 표시, 동일 PID/birth·spawn 1회·새 입력도
  확인했다. 두 private runtime에 잔여 process가 없다. Workspace Clippy
  (`--all-targets -- -D warnings`), format·diff 공백 검사도 통과했다.

### AR-002 · medium · 승인 대기 전의 ready 안정화 시간 재사용

- 원인: 잠정 Stop의 ready frame을 관측한 후 승인 대기에 들어갔다가 해제되면,
  이전 `idle_frame`의 시간을 그대로 사용했다. 대기 해제 직후 완료로 표시할 수 있었다.
- 재현: 공통 reducer의 InstanceId 회귀 검사가 수정 전 실패했다
  (`/tmp/fm-runtime-wait-before-tests.log`).
- 조치: 대기 진입·해결 전이에서 provider 화면 안정화 기록을 무효화한다.
  승인·입력·session 대기와 같은 공통 ledger의 경계를 사용하므로 GUI가 없는
  경우와 일반 terminal에 같은 규칙을 적용한다.
- 검증: provider 관련 단위 검사 4개 통과. Gemini·Antigravity의 실제 Host에서
  대기 해제 직후 Working 유지와 새 ready grace를 확인했다. 격리 live 전체
  29개 통과·exit 0 (`/tmp/fm-runtime-wait-after-live.log`,
  `/tmp/fm-gui-rc0y4z93`). 해당 runtime의 잔여 process가 없다.

### AR-003 · medium · 화면에서 확인한 headless 실패의 오류 알림 누락

- 원인: Host의 전이 알림은 완료와 입력 대기에만 인과 ID를 부여했다.
  Cline·Gemini의 화면 실패는 Idle/Failed로 표시돼도 오류 알림이 없었다.
- 재현: GUI 없는 Host에서 실패 전이를 반복한 회귀 검사가 알림 0건으로 실패했다
  (`/tmp/fm-runtime-failure-before-tests.log`).
- 조치: Idle/Failed도 Host의 응답별 알림 receipt를 사용하고 Error로 보관한다.
  managed Antigravity의 native 실패 hook은 추가 Notify를 보내지 않아 중복을 막는다.
  일반 Tab-bound 실패 알림과 입력 대기의 NeedsInput 동작은 유지한다.
- 검증: 실패 반복·새 작업의 별도 실패 회귀 검사와 managed/Tab-bound CLI 검사
  2개 통과. 격리 live 30개 통과·exit 0
  (`/tmp/fm-runtime-failure-after-live.log`, `/tmp/fm-gui-5xr7w63n`). 실제
  Cline·Gemini 화면 실패와 Antigravity native 실패의 Error 1건 보관, GUI의
  bell 복원과 desktop 재전송 방지를 확인했다. stdout/event PASS 수가 일치하며
  해당 runtime의 잔여 process가 없다. 전체 Linux gate 결과는 별도로 기록한다.

### AR-004 · medium · 비활성 자동 회수를 작동하는 설정처럼 표시

- 원인: provider 전체 완료와 안전한 유휴 종료 계약이 없어 자동 회수가 비활성인데,
  Options는 timeout을 편집하게 하고 설정 시간 후 회수한다고 안내했다.
- 조치: 값을 보존하며 입력을 비활성화하고, 보이는 행에 `not active`를 표시한다.
  tooltip은 자동 회수가 없으며 Agents의 명시적 종료가 필요하다고 안내한다.
- 검증: Options·terminal search GTK 검사 21개 통과. 격리 New Agent live
  10개 통과·exit 0 (`/tmp/fm-runtime-options-after-live2.log`,
  `/tmp/fm-gui-f24occ1i`). 실제 표시·spin 비활성·30분 값 보존, 동일 PID/birth의
  tab·GUI 종료 생존과 복원을 확인했다. stdout/event PASS 수 일치와 잔여 process
  부재도 확인했다. 이는 자동 회수 기능의 구현이 아니다.
- fixture: Options 검사는 실행을 만든 후 수행한다. 실행 없는 Host는 10초 후
  종료하므로, launch 전 Options 검사로 시간을 늘리면 fixture가 이전 연결을 사용했다.

### AR-005 · medium · 공유 gate의 검색 스크롤 관측 경합

- 관측: 첫 Linux gate는 GUI 958개 통과·1개 실패·4개 ignored 뒤 exit 101이었다.
  `output_search_chunks_preserve_wraps_duplicates_and_alternate_screen`이 고정
  50ms 후 두 번째 검색 위치가 첫 번째보다 큰지 확인하다 실패했다
  (`/tmp/fm-runtime-review-shared-ci.log`). 같은 binary의 격리 단독 실행은 통과했다
  (`/tmp/fm-search-before-isolated.log`). 제품의 검색 결함으로 확정하지 않는다.
- 조치: 첫 검색의 scrollback 진입과 두 번째 검색의 전진을 GTK main loop에서
  각각 최대 1초 관측한다. 두 검색의 위치 비교 조건을 유지하고 실패 위치를 출력한다.
- 검증: 수정 후 Options·terminal search GTK 21개 통과
  (`/tmp/fm-runtime-options-search-tests.log`). 전체 gate에서 재검증한다.

## 완료 전 확인할 범위

- 새 변경을 포함한 Linux 공유 CI 전체와 runtime live phase.
- 계획서의 미완료 항목(전체 provider 완료·원자적 idle 종료 계약, offline binding
  조정·응답 유실, 정렬·Recently ended, 자원 상한·프로세스 containment).
- 실제 provider 장시간 작업과 macOS·Flatpak은 이 Linux fixture의 검증 대상이 아니다.
- `.claude/`의 별도 worktree와 Python bytecode는 runtime 구현 커밋에 포함하지 않는다.

전체 계획 완료, medium 이상 이슈 0건, 배포 가능 여부는 아직 확정하지 않았다.
