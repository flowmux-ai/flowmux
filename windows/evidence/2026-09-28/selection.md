<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Windows 선택 보관·복사 경로

독립 `windows/` 안에서 터미널 선택과 복사 UI를 추가했다. 기존 Rust Linux/macOS,
공유 core, root Cargo 파일은 수정하지 않았다. 114개 기능·13개 게이트의 범위를
유지하며, T08은 partial이다. 실제 desktop clipboard 또는 마우스 조작을 검증했다고
주장하지 않는다.

기존 Linux VTE 구현의 선택 cache 동작을 읽고 Windows xterm에 맞게 적용했다.
선택이 끝난 시점의 문자열을 저장해 TUI가 같은 셀을 다른 내용으로 덮거나 화면을
지워도 원래 문자열을 복사할 수 있다. 현재 선택과 문자열이 같으면 `live`, 다르면
`retained`로 보고한다. retained 좌표는 원래 선택의 좌표이며 현재 내용의 안정적인
참조가 아니다. 각 surface가 자체 snapshot을 가지므로 탭 숨김·이동·프로세스 종료를
거쳐도 유지되고, 새 선택·선택 해제·검색 변경·입력·붙여넣기·normal/alternate 전환·
xterm에 도달한 RIS reset은 이전 cache를 버린다. IME 조합을 대신 확정하지 않는다.

선택 문자열 추출과 셀 좌표 계산은 xterm의 공개 API를 사용한다. NFC/NFD 정규화를
하지 않는다. 128 KiB UTF-8 초과 선택은 오류로 보관하고 이전 cache나 잘린 문자열을
사용하지 않는다. 이것은 보관/전송 상한이며 `getSelection()`이 만드는 임시 문자열의
최대 메모리 사용량을 보장하지 않는다. 아주 큰 선택의 추출 비용은 남은 검증 항목이다.

`selection --surface ID read|all|clear|range ROW COLUMN LENGTH`는 parser barrier를
지난 실제 WebView2 xterm에 적용된다. `range`는 현재 buffer의 zero-based cell
범위이며 scrollback을 포함한다. 없는 대상과 닫힌 요청을 처리하고, 잘못된 범위는
현재 선택을 바꾸지 않는다. 종료한 프로세스의 남은 화면도 선택할 수 있다. 결과는
text/source/buffer/position/error를 반환한다. 이 명령은 OS clipboard와 별개다.

UI에는 Ctrl+Shift+C/Ctrl+Insert, Ctrl+Shift+V/Shift+Insert 및 터미널 context menu를
추가했다. Ctrl+C는 원래 terminal key 경로를 유지한다. context menu는 Copy/Paste/
Select all/Clear selection을 제공하고, 방향키·Home/End·Escape를 처리한다. IME 및
AltGr 키를 가로채지 않으며 application mouse mode에서는 Shift 우클릭으로 열도록
했다. 검색 입력창은 자체 텍스트 편집을 유지한다.

복사 이벤트는 bounded snapshot의 `text/plain`을 한 번 제공한다. 빈 선택과 상한
오류에서는 clipboard에 데이터를 제공하지 않는다. 단축키와 메뉴는 표준
[Clipboard API](https://developer.mozilla.org/en-US/docs/Web/API/Clipboard)를 사용한다.
[writeText](https://developer.mozilla.org/en-US/docs/Web/API/Clipboard/writeText)와
[readText](https://developer.mozilla.org/en-US/docs/Web/API/Clipboard/readText)의
성공 여부는 API 결과로 처리하며, deprecated `execCommand`나 임시 textarea로
IME 상태를 바꾸는 fallback은 추가하지 않았다. WebView2 권한 동작은 고정한 wry
0.57.0의 clipboard-read handler와
[Microsoft permission API](https://learn.microsoft.com/en-us/microsoft-edge/webview2/reference/win32/icorewebview2permissionrequestedeventargs)를
확인했다. 실제 runtime/desktop별 허용 여부는 아직 확인하지 않았다.

Clipboard 읽기가 진행 중이면 추가 API 호출을 만들지 않는다. 포커스/탭 표시 상태,
조합 시작, 다른 키·텍스트 입력·붙여넣기가 발생하면 결과의 유효 기간을 끝내고,
늦게 완료된 읽기는 입력하지 않는다. 이는 OS clipboard 작업 자체를 취소한다는
뜻은 아니다. 작업이 끝날 때까지 추가 요청은 제한된다. API 거부/미지원 오류를
터미널 안에 표시하며 자동 재시도하지 않는다. 문자열이 선택돼 있어도 keyboard
focus를 잃은 문서의 async clipboard 요청은 시작하지 않는다.

Background host는 별도 init flag로 clipboard controller를 막고 wry의 자동 read
권한 허용도 사용하지 않는다. 검증기는 사용자 clipboard의 읽기/쓰기, 저장 후 복원,
OS 키보드/마우스 입력, foreground 전환을 하지 않는다. `SelectionProbe.cs`를
소유한 숨김 ConPTY에서 실행하고 private file로 출력만 바꾼다. WebView2의 선택은
Named Pipe 명령으로 바꾼다. 이는 물리적인 drag·더블클릭·단축키·Microsoft IME
검증과 다르다.

`native-selection-background.json`의 live 10개 항목:

1. 완성형 한글·분해형 자모·이모지의 정확한 선택 문자열.
2. 같은 셀의 문자열을 덮어써도 원래 copy snapshot 유지.
3. 전체 화면 지우기 후 snapshot 유지.
4. 명시적인 선택 해제 후 이전 문자열 없음.
5. 검색 닫기/검색 실패 뒤 이전 문자열 재사용 없음.
6. alternate buffer에서 wide 문자 cell range 선택.
7. 잘못된 범위가 기존 선택을 바꾸지 않고, buffer 전환은 기존 cache를 제거.
8. 비활성 surface 조회와 workspace 간 이동 후 같은 PID·선택 유지.
9. Select all이 상한 초과 시 잘라내거나 예전 문자열을 돌려주지 않음.
10. 정상 종료 뒤 화면 선택 조회·해제 가능.

기존 숨김 붙여넣기 12개 항목, 단일 찾기 7개군, 전체 출력 검색 9개군도
최종 debug 실행 파일에서 통과했다. 각 실제 JSON을 별도로 보존한다.
프런트엔드 41개, Windows native Rust 52개, 독립 Windows crate의 Linux Rust
25개 검사가 통과했다. Release·Clippy·NSIS 빌드 로그와 실행 파일 SHA-256도
같은 디렉터리에 보존했다.
프런트엔드 검사는 선택 보관/해제, 크기 제한, parser barrier, 복사 이벤트의 데이터
제공, clipboard 거부/지연/취소, IME·AltGr·반복 키, 메뉴 위치/키보드 이동을 검사한다.
Clipboard와 DOM은 테스트 객체를 사용한다. 구현 중 pointerdown과 compatibility
mousedown 사이에서 이전 선택을 다시 읽을 수 있는 순서를 재현했고, primary-down
시점에는 snapshot을 지우기만 하도록 수정했다. 이 수정도 실제 마우스 조작 검증을
대체하지 않는다. Alt-click cursor 이동의 단일 문자 선택은 snapshot으로 남기지 않는다.

실제 Windows clipboard round trip·권한/소유권 경쟁·여러 앱 간 개행 규칙, 물리적인
drag/더블클릭/column selection·메뉴·단축키, 드래그 중 연속 redraw, IME·DPI·접근성,
아주 큰 선택의 메모리/응답성, 모든 timeout/close 경쟁 조건 및 기존 플랫폼 전체
live 회귀는 미완료다. 사용자 백그라운드 작업 조건을 유지하면서 검증한 범위만
기록한다. 설치 프로그램은 빌드만 하며 실행하지 않는다.
