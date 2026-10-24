<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
# 브라우저 작업 흐름 구현·검증 기록

검증일: 2026-10-24. 기준 커밋: `6ffc5dff1e60d4692d6c2027d9c431ce03fa8781`.
구현 브랜치: `feat/browser-workflow`. 버전은 `0.11.4`를 유지했다.

## 구현 범위

1. P1 회귀 3건: Linux Caps Lock 입력 의미·원상 복원, 입력 중 confirm의 순차 응답,
   스크롤 후 fixed 요소 캡처 좌표 수정.
2. 환경 진단: 실제 backend, 활성·표시 탭, 입력 전제, 캡처 제한, CLI 인코더,
   쿠키 주입 미지원 안내. 오프라인 capabilities는 유지.
3. 제어권 인계: 탭별 작업 ID, 변경 명령 직렬화, 사용자 제어·재개·중단 버튼,
   인계 이후 대기 중인 네이티브 입력 및 JS wait 반복 차단. 기존 탭 보존.
4. 결과 확인: 선택적 receipt, 동작 ID·문서 세대·네이티브 이벤트,
   URL/텍스트 조건 대기, 결과 불명·대화상자 대기 구분, 자동 재실행 금지.

사용법과 제한은 [브라우저 작업 흐름](browser-workflow.md)에 정리했다.

## 확인 결과

| 검사 | Linux | macOS |
| --- | --- | --- |
| P1 회귀 fixture | 통과: Caps Lock 포함 3건 | 통과: 해당하는 2건 |
| 작업 ID·동시 변경·인계 후 차단 | 통과 | 통과 |
| 기대 결과·시간 초과 재실행 금지·팝업·대화상자 receipt | 통과 | 통과 |
| 실제 제어 버튼 회수·재개·중단·해제 | GTK/Xvfb 통과 | AppKit 메인 스레드 harness 통과 |
| 활성/비활성 탭·대화상자·ffmpeg 미설치 진단 | 통과 | 통과 |
| Wayland 진단·native type·키 입력 거부 | headless Weston에서 통과 | 해당 없음 |
| 기존 전체 브라우저 시나리오 포함 검사 | 통과 | 실패: 아래 기존 포인터 회귀 |
| workspace/all-targets Clippy, 경고 오류 처리 | 통과 | 통과 |

`flowmux-browser`, `flowmux-cli`, `flowmux-ipc` 테스트가 통과했다.
GUI IPC 68건, bridge 4건, 제어권 상태 테스트, 캡처 좌표 테스트도 통과했다.
포맷과 `git diff --check`를 확인했다. 원격 CI·배포·사용자 설치 앱 교체는 하지 않았다.

macOS의 신규 기능 집중 검사는 기존 전체 시나리오를 제외하고 실행했다.
따라서 집중 검사 마지막의 공통 harness 완료 표시를 전체 macOS 통과 근거로 사용하지 않는다.

### P1 수정 전후 증거

- 수정 전 macOS: 입력 confirm 응답 전 `type`이 끝나지 않았고,
  fixed 빨간 요소 대신 파란 배경을 저장했다.
- 수정 전 Linux: 위 두 문제에 더해 Caps Lock 상태의 논리 키 입력이 잘못되거나
  완료 대기가 시간 초과됐다.
- 수정 후: 동일 fixture에서 순차 대화상자 응답, 다음 입력, 기대 RGB 픽셀,
  스크롤 위치 보존을 확인했다. Linux는 `a`, `A`, `Shift+a`, `1`, `Shift+1`의
  결과와 매 입력 후 Caps Lock 유지까지 확인했다.

### 남은 기존 macOS 포인터 회귀

기존 전체 시나리오에서 Shadow DOM 버튼 클릭이
`native pointer input completion timed out`으로 실패한다. 첫 클릭이나 다른
후반부 클릭에서도 발생했던 간헐적 문제이며, 이번 P1 수정 전부터 관찰됐다.

별도 계측에서는 대상이 보이고 좌표도 맞았지만 trusted 이벤트가
`pointerup → mouseup → pointerdown → mousedown` 순서로 도착했고 `click`이 없었다.
현재 네이티브 구현은 down/up 이벤트를 연속 `sendEvent`한다. WebKit의 입력 문맥
처리에는 비동기 completion 경로가 있어 순서 역전의 원인 후보로 좁혔지만,
아직 수정·검증한 해결책은 없다.

고정 지연이나 클릭 재시도로 덮지 않았다. 후속 수정은 네이티브 이벤트 순서를
보장하면서 IME, 드래그, 대화상자, 중단 시 버튼 해제까지 검증해야 한다.
현재 상태를 macOS 전체 회귀 통과나 출시 검증 완료로 표시하면 안 된다.

### 로컬 증거 위치

- `/tmp/fm-workflow-p1-before-macos.log`: P1 수정 전 실패.
- Linux VM `/tmp/fm-workflow-p1-before-linux.log`: P1 수정 전 실패.
- `/tmp/fm-workflow-final-linux.log`: 마지막 Linux 전체 브라우저 검사.
- `/tmp/fm-workflow-final-focused-macos.log`: 마지막 macOS 신규 기능·P1 집중 검사.
- `/tmp/fm-workflow-full-macos.log`: 기존 전체 검사 실패.
- `/tmp/fm-workflow-p1-after-macos3.log`: 포인터 역순 계측.
- `/tmp/fm-workflow-native-failure.png`: 실패 당시 화면.
- `/tmp/fm-workflow-wayland.log`: Wayland 진단과 동작 확인.
- `/tmp/fm-workflow-control-ui-linux.log`, `/tmp/fm-workflow-control-ui-macos.log`: 제어 버튼 검사.
- `/tmp/fm-workflow-final-headless2.log`, `/tmp/fm-workflow-final-gui-unit.log`: 관련 테스트.
- `/tmp/fm-workflow-clippy-linux.log`, `/tmp/fm-workflow-clippy-macos2.log`: 정적 검사.

모든 GUI 검사는 별도 상태·런타임 디렉터리를 사용했다. 사용 중인 Flowmux와
에이전트 세션을 종료·재시작하지 않았다.
