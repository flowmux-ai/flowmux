<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Windows native browser foundation

이번 변경은 독립 `windows/`에만 적용한다. 기존 Linux/macOS 및 shared Rust crate,
root manifest/lock은 수정하지 않는다. 전체 114개 기능·13개 게이트를 유지한다.
B01/B02/B06/B09와 G09는 부분 구현이며 전체 브라우저/Windows 지원 완료가 아니다.

`+ Browser` 및 `browser open`은 기존 순수 Rust pane 배치 모델을 사용한다. 오른쪽
browser pane 재사용, 새 오른쪽 split, 명시적 down split을 지원한다. Native WebView2와
주소 EDIT, Go, history/reload/stop/zoom 버튼 및 상태 STATIC을 surface별로 보유한다.
Tab 이동 시 view를 다시 만들지 않는다. 반복 layout은 visibility가 바뀔 때만 show/hide한다.
주소 편집 중 timer가 값을 덮어쓰지 않으며 IME 확정 Enter를 직접 가로채지 않는다.
물리 입력을 통한 이 동작 검증은 아직 하지 않았다.

터미널과 브라우저의 view map, profile과 입력 경로를 분리했다. 외부 페이지에는 terminal
custom protocol/initialization/IPC handler를 등록하지 않으며 WebView2 web messaging과
host objects도 비활성화한다. HTTP/HTTPS/about:blank만 허용하고 reserved terminal
origin, file/data/javascript URL을 거부한다. 새 창 요청과 다운로드는 아직 거부한다.
사용자 브라우저 profile이나 쿠키를 읽지 않는다. 일반 실행의 사이트 권한은 WebView2
기본 UI에 맡기며 숨김 시험은 권한과 script dialog를 비활성화한다.

Native NavigationStarting/Completed의 navigation ID로 문서 세대를 추적한다. Wry의
기본 load callback은 완료된 탐색의 URL 대신 당시 current URL을 조회하므로, URL 비교만으로
이전 completion을 걸러내지 않았다. Native ID가 일치할 때만 loading/error 상태를 갱신한다.
실패 시 오류 코드를 상태 줄과 `navigation_error`에 반환한다. 최초 live trial에서는
상태의 `error: null`이 기존 IPC error envelope로 해석됐다. 상태 필드를 바꾸고 동일 조회를
다시 실행했다. 실패 기록도 보존한다. 기존 IPC 오류 처리 계약은 변경하지 않았다.

CLI는 open/navigate/back/forward/reload/stop/url/title/status/zoom/eval 11개 operation을
지원한다. 명시한 pane의 active browser가 대상이며 조회/탐색 자체는 focus를 요청하지 않는다.
Eval은 동기 JavaScript와 JSON 결과만 지원하며 source/result 128 KiB, pending 최대 16개,
12초 callback deadline을 적용한다. Navigation 중 eval, Promise, 닫힌/변경된 문서와
oversized 결과를 거부한다. Callback 만료/오류는 이미 시작한 JavaScript를 취소하거나
실행 부작용을 되돌리지 않는다. 자동 재전송은 하지 않는다. Selector refs/actions/wait와
async eval은 별도 미구현이다. 모든 timeout/close/navigation 경쟁 조건의 검증은 남아 있다.

`tree.surfaces`는 기존 terminal 배열을 유지하고 `browsers`를 추가한다. Browser의
`identify.shell`은 null이고 terminal 입력/선택/화면 요청은 명시적으로 실패한다.
Browser를 source로 terminal을 split할 때 존재하지 않는 shell map을 인덱싱하지 않고
설정 기본 shell을 사용한다. Mixed pane의 새 terminal cwd는 기존 공유 모델의 terminal
cwd fallback을 따른다. Browser도 기존 제목/알림 배지 경로를 사용한다.

State는 browser URL/tab identity와 terminal history/shell을 구분한다. Browser-only
checkpoint는 nonexistent terminal callback을 기다리지 않고 즉시 atomic writer로 보낸다.
기존 terminal-only state도 계속 읽는다. Profile의 localStorage 지속은 검증했지만
로그인/cookie/session 전체, private mode, profile 선택/관리, bookmark/import는 미완료다.
Zoom/history stack/page JS state를 앱 재시작 뒤 복원한다고 주장하지 않는다.

`verify-browser.ps1`은 isolated config/state의 숨긴 debug host, 명시적 소유 pipe와
loopback HTTP fixture를 사용한다. Discovery는 launch 이후 timestamp와 PID를 확인한다.
OS keyboard/mouse/clipboard, 사용자 flowmux/browser, 외부 사이트, installer는 조작하지 않는다.
Native EDIT는 WM_GETTEXT로 읽기만 한다. 다음 6개 검사군을 실행한다.

- Unicode DOM/title 및 UTF-16 native 주소, URL 인코딩/디코딩과 terminal bridge 부재.
- History/reload, stop 중 eval 거부, redirect, network failure 표시/복구, bounded zoom.
- Forbidden URL과 script navigation, popup 요청 거부, Promise/throw/result-limit 오류,
  browser에 대한 terminal command 거부 및 host 지속 동작.
- Right reuse/down split, mixed pane move 뒤 동일 HWND/JS 값, 기존 shell PID 유지,
  mixed checkpoint에서 terminal history와 browser 분리.
- Mixed pane의 다른 terminal이 활성화된 상태에서도 inactive caller의 새 terminal은
  자신의 작업 경로를 사용한다.
- Browser source split, browser-only 저장/종료/복원, stable surface ID와 독립 profile
  localStorage의 한글·분해형 자모·결합 문자·이모지 지속.

Unicode 내용/주소 문자열 검사이며 glyph 모양, 실제 Windows IME composition/후보창,
backspace/commit/cancel, native button/주소 키보드 조작, DPI/접근성 검증을 대신하지 않는다.
좁은 pane에서의 toolbar 배치, 페이지 찾기/다운로드/popup routing/DevTools/fullscreen/media/login,
browser 모든 자동화 명령과 기존 플랫폼 전체 live 회귀는 계속 미완료다.

기존 capability의 boolean `browser_automation: false`는 유지하고 별도 partial 상태와
명령 목록을 추가했다. Browser controller 오류가 timer의 나머지 terminal/search/save
유지보수를 중단하지 않도록 처리했다. Callback 완료 시점에도 deadline을 재검사한다.

최종 fixture의 inactive-caller 시험을 처음 추가했을 때 GUI launch에 `--cwd`를 빼고
검증기가 자기 private directory를 기대하는 오류가 있었다. 저장된 workspace cwd는
실제 기본값인 사용자 홈이었다. Fixture의 launch cwd를 명시해 수정했고 실패 기록을
`native-browser-failed-fixture-cwd.json`에 보존했다. Windows 결과 수집에서는 두 OS 간
파일 timestamp 비교로 이전 결과까지 선택되는 문제가 있어 새로 생긴 결과 경로로
구분했다. 통과한 native 시나리오는 다시 실행하거나 입력을 재전송하지 않았다.

최종 검사는 frontend 56개, Linux에서 실행한 독립 Windows crate 90개, 숨긴 실제
Windows Rust 117개가 통과했다. Browser 6개군, 기존 named-key 7개군, selection
10개군, state 8개군, cwd 7개군이 최종 debug host에서 통과했다. Release/Clippy와
NSIS 패키지 생성도 통과했다. 설치 프로그램은 실행하지 않았다.
`artifacts-browser.json`은 debug/release 실행 파일과 설치 파일 7개의 hash 기록이다.

`cleanup-browser.json`은 기록한 소유 PID 43개, 그 자식 관계와 fixture 경로를 읽기 전용으로
조회해 남은 일치 프로세스가 없음을 확인했다. 사용자 WSL flowmux PID 787은 계속 실행
중이며 사용자 창/입력/클립보드를 조작하지 않았다. Linux/macOS 전체 live 회귀 또는
실제 Windows IME 완료를 이 결과로 대신하지 않는다.

한글 검증의 문자열 비교는 `StringComparison.Ordinal`로 보강했다. 문화권 비교가
canonical-equivalent 문자를 같은 문자열로 취급하는 여지를 없애기 위해서다. 같은
바이너리에서 DOM/title, decoded URL, move 뒤 JS 값 및 restart 뒤 localStorage를
UTF-16 코드 단위로 정확히 비교했고 숨김 6개 검사군이 다시 통과했다. 결과는
`native-browser-ordinal-background.json`, 추가 소유 PID 6개의 정리는
`cleanup-browser-ordinal.json`에 기록했다. 실제 IME 검증과는 별개다.
