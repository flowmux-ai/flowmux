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

## 완료 전 확인할 범위

- 새 변경을 포함한 Linux 공유 CI 전체와 runtime live phase.
- 계획서의 미완료 항목(전체 provider 완료·원자적 idle 종료 계약, offline binding
  조정·응답 유실, 정렬·Recently ended, 자원 상한·프로세스 containment).
- 실제 provider 장시간 작업과 macOS·Flatpak은 이 Linux fixture의 검증 대상이 아니다.
- `.claude/`의 별도 worktree와 Python bytecode는 runtime 구현 커밋에 포함하지 않는다.

전체 계획 완료, medium 이상 이슈 0건, 배포 가능 여부는 아직 확정하지 않았다.
