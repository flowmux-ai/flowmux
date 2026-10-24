<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
# 브라우저 작업 흐름 구현·검증 기록

검증일: 2026-10-24. 기준 커밋: `6ffc5dff1e60d4692d6c2027d9c431ce03fa8781`.
초기 구현 브랜치: `feat/browser-workflow`. 이후 `main`에 반영했다. 버전은 `0.11.4`를 유지했다.

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
| 기존 전체 브라우저 시나리오 포함 검사 | 통과 | 후속 포인터 수정 후 통과 |
| workspace/all-targets Clippy, 경고 오류 처리 | 통과 | 통과 |

`flowmux-browser`, `flowmux-cli`, `flowmux-ipc` 테스트가 통과했다.
GUI IPC 68건, bridge 4건, 제어권 상태 테스트, 캡처 좌표 테스트도 통과했다.
포맷과 `git diff --check`를 확인했다. 초기 기능 커밋을 main에 푸시한 뒤 원격 CI를 실행했고,
macOS 팝업 검사 실패를 아래와 같이 보완했다. 후속 커밋의 원격 CI 결과는 별도 확인한다.
배포·사용자 설치 앱 교체는 하지 않았다.

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

### 후속 macOS 포인터 회귀 수정

수정 전에는 입력 필드에 글자를 입력한 뒤 Shadow DOM 버튼을 클릭하면
`native pointer input completion timed out`으로 실패했다. trusted 이벤트가
`pointerup → mouseup → pointerdown → mousedown` 순서로 도착했고 `click`이 없었다.

WebKit이 편집 중인 문서의 마우스 이벤트를 `NSTextInputContext` completion을
통해 비동기로 전달하는 것을 확인했다. 해당 네이티브 이벤트의 completion이
전달을 마친 뒤 AppKit 메인 큐에서 다음 이벤트를 보낸다. 원래 입력 문맥 처리와
completion의 결과는 유지하고, 메서드 대체는 이벤트 전달 범위에서 원복한다.
고정 지연이나 클릭 재시도를 넣지 않았다.

- 입력 후 클릭 8회에서 trusted down → up → click 순서와 버튼 상태를 검증했다.
- 전체 브라우저 시나리오의 드래그·더블클릭·팝업·대화상자·파일 선택·한글 입력을 검증했다.
- WebDriver는 네이티브 시퀀스가 끝난 뒤 응답한다. AppKit 전용 런루프에서도 검증했다.
- 제어권 변경·탭 숨김·포커스 상실 시 남은 누름을 취소하되 이미 누른 버튼은 해제한다.
  네이티브 취소 fixture에서 release 도착과 메서드·물리 버튼 상태 원복을 확인했다.

### 원격 CI 팝업 검사 보완

초기 main 커밋 `fc6285af`의 macOS CI에서 즉시 닫히는 팝업을 연 직후의
JS wait가 잠깐 활성화된 팝업에 연결되어, 탭 닫힘에 따른 제어권 취소로 실패했다.
검사 의도는 부모 문서의 보존 확인이므로 저장한 부모 `surface_id`를 명시했다.
닫힌 탭에서 대기 작업을 취소하는 동작은 유지한다.

### 로컬 증거 위치

- `/tmp/fm-workflow-p1-before-macos.log`: P1 수정 전 실패.
- Linux VM `/tmp/fm-workflow-p1-before-linux.log`: P1 수정 전 실패.
- `/tmp/fm-workflow-final-linux.log`: 마지막 Linux 전체 브라우저 검사.
- `/tmp/fm-workflow-final-focused-macos.log`: 마지막 macOS 신규 기능·P1 집중 검사.
- `/tmp/fm-workflow-full-macos.log`: 기존 전체 검사 실패.
- `/tmp/fm-workflow-p1-after-macos3.log`: 포인터 역순 계측.
- `/tmp/fm-workflow-native-failure.png`: 실패 당시 화면.
- `/tmp/fm-pointer-order-before.log`: 입력 후 클릭 순서 회귀 fixture 수정 전 실패.
- `/tmp/fm-pointer-full-macos3.log`: 후속 수정 후 전체 브라우저 검사.
- `/tmp/fm-pointer-webdriver-native2.log`: AppKit 입력·취소 후 해제·창 격리 검사.
- `/tmp/fm-pointer-clippy3.log`: 후속 수정 정적 검사.
- `/tmp/fm-workflow-wayland.log`: Wayland 진단과 동작 확인.
- `/tmp/fm-workflow-control-ui-linux.log`, `/tmp/fm-workflow-control-ui-macos.log`: 제어 버튼 검사.
- `/tmp/fm-workflow-final-headless2.log`, `/tmp/fm-workflow-final-gui-unit.log`: 관련 테스트.
- `/tmp/fm-workflow-clippy-linux.log`, `/tmp/fm-workflow-clippy-macos2.log`: 정적 검사.

모든 GUI 검사는 별도 상태·런타임 디렉터리를 사용했다. 사용 중인 Flowmux와
에이전트 세션을 종료·재시작하지 않았다.
