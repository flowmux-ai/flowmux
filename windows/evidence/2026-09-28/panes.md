<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Windows pane 크기·방향 이동·최대화 검증

U02와 U04의 일부 구현 및 검증 기록이다. 전체 114개 기능·13개 게이트의
완료 판정은 아니다. 변경은 `windows/`에 한정했고 Linux·macOS 소스,
공유 core 및 root Cargo manifest/lock은 수정하지 않았다.

`native-panes-background.json`은 Windows 11 build 22623, PowerShell 5.1,
WebView2 112.0.1722.48에서 `scripts/verify-panes.ps1`을 실행한 결과다.
모든 호스트는 숨김 debug 모드와 별도 상태 디렉터리를 사용했다. 각 상태
대기에서 창이 보이지 않고 foreground도 차지하지 않는지 검사했다.
OS 키보드·마우스 입력, 클립보드 및 실제 IME 조작은 사용하지 않았다.

검사한 동작은 다음과 같다.

- 좌우 분할 안에 상하 분할을 만들고 바깥 비율 0.6, 안쪽 비율 0.35를
  적용했다. 계산한 pane 사각형과 실제 WebView bounds를 비교하고 각 셸의
  `[Console]::WindowWidth/WindowHeight`가 xterm 행·열 수와 같은지 확인했다.
- 실제 크기는 왼쪽 69×42, 오른쪽 위 45×12, 오른쪽 아래 45×26이었다.
  위 pane 최대화 후 117×42로 바뀌었고 복원 후 원래 사각형·행·열로 돌아왔다.
- 잘못된 비율과 존재하지 않는 ID는 모델을 바꾸지 않았다. 범위 안의 극단
  비율은 공유 core와 같은 0.05–0.95로 제한됐다. 비활성 workspace의 비율을
  바꾸어도 현재 workspace와 포커스는 유지됐다.
- 방향 이동은 해당 workspace의 원래 분할 위치를 사용했다. 이웃이 없는
  방향으로 요청하면 포커스가 유지됐고, 최대화 중 숨은 이웃으로 이동하면
  원래 분할이 복원됐다.
- 최대화 중 형제 pane의 출력과 한글 찾기가 계속 동작했다. native 버튼의
  문구가 `Restore pane`으로 바뀌는 것도 읽기 전용 API로 확인했다.
- 최대화·복원 40회에서 매번 xterm 크기 변경 완료를 기다렸고 모든 원래
  surface와 셸 PID가 유지됐다. 숨은 셸의 CLI도 자신의 stable surface를
  기준으로 방향 이동했다. workspace 전환·split·move·close·resize 후
  zoom 상태가 정리됐다.
- 최대화 중 저장·종료한 뒤 새 숨김 호스트를 띄웠다. 중첩 비율·pane/tab
  식별자·포커스·한글 기록이 복원됐고 일시적인 최대화 상태는 남지 않았다.

Rust 테스트는 특정 분할만 변경되는지, 잘못된 요청의 불변성, 경계의 잡은
위치 보존, 양 끝 제한, 작은 영역의 픽셀 분할과 방향 이동을 검사한다.
프런트엔드 테스트는 조합 중 이벤트, `isComposing`, keyCode 229, AltGr,
오른쪽 Alt, 키 반복이 pane 단축키로 잘못 처리되지 않는 경로를 검사한다.
이는 실제 Microsoft IME 검증을 대신하지 않는다.

실제 divider 드래그, 물리 키보드의 Alt+Arrow / Ctrl+Alt+M, 버튼 클릭,
조합 중 포커스·resize·최대화 전환, 후보 창 위치, 작은 창에서의 사용성,
DPI 및 접근성은 미검증이다. 사용자가 데스크톱에서 작업 중이며 테스트
창을 띄우지 말라고 요청했으므로 이 대화에서는 실행하지 않았다.
native 이벤트 처리 코드는 추가했지만 백그라운드 검사에서는 캡처와 커서
변경을 비활성화하고 동일한 모델·레이아웃·PTY 경로를 CLI로 호출했다.

Linux에서 독립 Windows crate 테스트 12개, 프런트엔드 테스트 19개,
Windows native Rust 테스트 17개가 통과했다. Release build와 Clippy도
통과했다. `*-tests-panes.txt`, `release-build-panes.txt`,
`release-clippy-panes.txt`에 로그를 보관했다.
최종 debug 빌드에서 기존 전체 출력 검색 9개 검사군과 탭 이동 40회를
다시 확인했다(`native-output-search-panes-background.json`,
`native-tab-move-panes-background.json`).

`artifacts-panes.json`은 실행 파일과 새로 빌드한 개발용 설치 파일의
크기·SHA-256이다. 설치 파일은 실행하지 않았고 새 배포의 설치/제거
승인 근거로 사용하지 않는다. U02·U04 및 전체 Windows 목표는 진행 중이다.
