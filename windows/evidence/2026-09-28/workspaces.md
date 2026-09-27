<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Windows workspace 메타데이터·닫기 검증

이번 단계는 workspace 이름·색상·순서·닫기, tab 이름 고정과 해당 native
메뉴·편집창을 추가한다. 기존 Linux·macOS 코드와 root Cargo 파일은 바꾸지
않았다. Windows 전용 `windows-sys` feature만 추가했으며 공유 core는
읽기 전용 의존성으로 사용했다. 전체 Windows 목표의 완료 근거는 아니다.

`native-workspaces-background.json`은 Windows 11 build 22623, PowerShell 5.1,
WebView2 112.0.1722.48의 숨김 debug 호스트에서 실행한 여섯 검사군이다.
모든 호스트와 저장 파일은 테스트 소유이고, 별도 상태 디렉터리를 사용했다.
창 표시·foreground 획득·OS 입력·클립보드 조작은 실행하지 않았다.

- 한글, 분해형 자모, 결합 악센트, 이모지, `&`와 `&&`를 포함한 workspace와
  tab 이름을 Ordinal 비교했다. 모델은 원문을 보존하고 native button은
  ampersand 이스케이프를 적용한 caption을 가진다. 색상은 `#12abef`로
  저장됐고 native 색상 표시용 STATIC control도 생성됐다.
- 사용자 tab 제목은 shell의 자동 제목 출력으로 바뀌지 않았다. 비활성 tab의
  이름을 변경해도 활성 surface는 유지됐다. 잘못된 이름·색·순서·ID를
  거부한 뒤 전체 workspace 모델이 동일한지도 확인했다.
- workspace 재정렬 41회에서 원래 surface/PID와 활성 workspace ID가
  유지됐다. 숨은 셸의 `workspace current`는 화면에 활성화된 workspace가
  아니라 호출자의 실제 workspace를 반환했다.
- 비활성 workspace의 여러 pane/tab에 걸친 터미널 3개와 그중 한 셸의
  자식 프로세스 3개를 닫았다. 해당 PID들만 종료됐고 다른 workspace의
  셸·포커스·한글 출력이 유지됐다. 닫힌 tab의 ID로 rename하면 거부됐다.
- 재시작 후 workspace 순서·색상·Unicode 이름·tab 제목 고정·한글 기록이
  복원됐다. 새 셸의 자동 제목 출력도 고정된 제목을 덮어쓰지 않았다.
- 활성 workspace를 닫으면 인접한 생존 workspace가 활성화됐다. 색상
  제거와 마지막 workspace 닫기 거부도 확인했다. 마지막 workspace를 닫은
  뒤의 빈 창 UI는 아직 구현하지 않았으며 `quit`으로 창을 닫을 수 있다.

Linux에서 독립 Windows crate 테스트 15개가 통과했다
(`linux-rust-tests-workspaces.txt`). 이는 이름의 UTF-16 길이/제어 문자,
색 형식, 재정렬·닫기의 식별자/포커스 유지, 색상 필드가 없는 이전 저장
파일의 호환성과 잠긴 Unicode 제목의 저장을 검사한다.
Windows native Rust 테스트 22개도 통과했다
(`native-rust-tests-workspaces.txt`). 여기에는 실제 native EDIT/STATIC
control을 숨긴 상태로 생성하고 Unicode 값·오류 문구를 읽는 검사가 포함된다.
재정렬 중 이미 큐에 들어온 native 버튼/메뉴 이벤트가 재생성된 control의
번호 대신 클릭 시점의 대상 ID를 유지하는 회귀 검사도 포함된다.

최종 반복 중 재시작 직후 `tree` IPC 연결이 한 번 실패했다
(`workspace-startup-failed-trial.json`). 기존 테스트는 PID별 발견 파일의
존재만 기다려 이전 강제 종료의 파일이나 작성 중인 파일을 구분하지 않았다.
새 검증은 실행 시각 이후에 기록된 완전한 발견 파일, 해당 프로세스 PID,
명시적인 pipe를 확인한다. 보강 후 두 번의 전체 검사가 통과했다
(`native-workspaces-background.json`, `native-workspaces-repeat-background.json`).
당시 실패 프로세스의 상태와 발견 파일 시각이 남아 있지 않아 원인을 확정할
수는 없다. 제품 IPC 시작 안정성 전체가 검증됐다고 판단하지 않으며 O02에
후속 추적 항목으로 남겼다.

이름 편집창은 고정된 ID를 대상으로 한다. 편집 도중 다른 수동 이름 변경이
있으면 적용을 거부하며, 자동 shell 제목 변경은 tab rename을 막지 않는다.
native EDIT의 조합 이벤트는 그 control에 맡기고 텍스트 입력 중 Enter/Escape로
즉시 Apply/Cancel하지 않는다. 이 코드는 실제 IME 조합 검증을 대체하지 않는다.

이전 pane, 상태 저장/복원, lifecycle 검사를 다시 실행했다
(`native-panes-workspaces-background.json`,
`native-state-workspaces-background.json`,
`native-lifecycle-workspaces-background.json`). Release build와 Clippy 로그는
`release-build-workspaces.txt`, `release-clippy-workspaces.txt`에 있다.
`artifacts-workspaces.json`은 실행 파일과 재빌드한 설치 파일의 해시다.
설치 파일을 실행하거나 설치/업데이트 승인을 완료한 것으로 처리하지 않았다.

실제 메뉴 클릭·키보드 조작, 이름 편집 중 Microsoft IME, 색상 외관, DPI,
접근성, drag 재정렬, 많은 workspace의 side panel overflow, 자동 제목으로
되돌리기와 빈 창 UI는 남아 있다. 사용자가 백그라운드 작업을 요청했으므로
데스크톱 입력이 필요한 검사는 수행하지 않았다. U01·U03·U08은 partial이며
114개 기능·13개 게이트의 전체 범위는 그대로 유지한다.
