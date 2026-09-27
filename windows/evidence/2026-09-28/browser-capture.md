<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Windows browser viewport PNG capture — partial acceptance

Windows 전용 `browser screenshot pane:<id> <path>`를 구현했다. WebView2의
CapturePreview(PNG)를 메모리 IStream에 받아 페이지 viewport만 저장한다. 데스크톱,
다른 pane, native toolbar는 캡처하지 않으며 focus, scroll, zoom, window visibility를
바꾸지 않는다. CLI 상대 경로는 CLI cwd 기준으로 절대화하며 raw IPC에는 절대 경로가
필요하다. Plain 출력은 요청한 절대 경로이고 JSON에는 surface, navigation generation,
width, height, bytes도 포함한다. Unicode를 정규화하지 않는다.

WebView2는 ContentLoading 이전에 요청하면 실패하거나 이전 페이지를 캡처할 수 있다.
그래서 현재 구현은 navigation이 끝나고 browser error 상태가 없는 논리적 visible tab만
허용한다. 관련 API 동작은 [Microsoft CapturePreview 문서](https://learn.microsoft.com/en-us/microsoft-edge/webview2/reference/win32/icorewebview2?view=webview2-1.0.3537.50#capturepreview)에 명시되어 있다.
Error 상태에는 차단된 navigation 등도 포함되므로 이런 상태는 성공적인 재탐색 등으로
해소한 뒤 캡처해야 한다. HTTP 오류 응답 문서와 네트워크 navigation 실패는 다르다.

원래 surface와 native navigation epoch, logical visibility/layout revision, zoom revision,
physical bounds를 callback에서 재검사한다. 탭이 닫히거나 문서/viewport가 바뀌면 파일 쓰기를
시작하지 않는다. PNG header의 크기도 native bounds와 일치해야 한다. 이 검사는 DOM과
animation을 freeze하지 않으며, 문서 내부 변경이나 자체 scroll에 대한 원자적 snapshot을
보장하지 않는다. 검증된 bytes의 파일 저장이 시작된 뒤에는 추가 navigation이 그 bytes를
바꿀 수 없다. 결과의 surface/generation은 캡처한 시점의 대상이다.

한 창에서 capture/file writer를 합쳐 두 건까지 허용한다. Status의 captures_pending은
창 전체의 미완료 건수다. Viewport는 한 변 최대 8,192px 및 총 8 Mi pixels, PNG는 32 MiB
이하다. 이는 수락하는 이미지의 제한이며 WebView2 renderer 전체나 임시 COM stream의
메모리 상한을 강제하는 것은 아니다. Callback/저장 응답에는 12초 기한을 적용하고 기존
IPC의 15/25초 한도는 바꾸지 않았다. 기한이 지나도 실제 callback/writer가 끝나기 전에는
slot을 반환하지 않는다. Native callback이 영구히 돌아오지 않으면 host 종료 전까지
그 slot이 남는다. Callback 유실 후의 자동 복구는 아직 지원하지 않는다.

COM stream은 UI thread에서만 사용한다. 별도 worker는 목적지와 같은 폴더에 create_new로
임시 파일을 만들고 write/sync_all 뒤 MoveFileExW(REPLACE_EXISTING|WRITE_THROUGH)로
교체한다. 실패하면 기존 목적지 bytes를 유지하고 소유 임시 파일 삭제를 시도한다. Process
crash 또는 삭제 실패에서는 임시 파일이 남을 수 있다. 쓰기 시작 뒤 timeout/tab close/응답
유실은 파일 미생성을 보장하지 않으며 자동 재시도하지 않는다. 기존 parent는 있어야 한다.
Device namespace/reserved name, alternate data stream, control 문자, 모호한 trailing
space/dot와 비-PNG 확장자는 거부한다. 일반/extended drive 및 UNC 경로 문법은 허용하나
실제 network filesystem, reparse-point 교체 경쟁과 crash durability는 미검증이다.

숨김 debug host와 소유 loopback fixture에서 다음 6개군이 통과했다.

- PNG를 System.Drawing으로 decode해 실제 배경색과 493×587 viewport 크기 확인;
  한글·분해형 자모·combining accent·emoji 파일명과 CLI 상대 cwd를 ordinal 비교.
- 현재 scroll 위치, plain 경로 출력, 기존 PNG 교체와 DOM focus/scroll 불변 확인.
- 열린 exclusive 파일 때문에 교체가 실패해도 기존 bytes 유지; 임시 파일 제거;
  없는 parent를 생성하지 않고 오류 반환.
- Raw 상대 경로, 비-PNG, ADS/device 경로, terminal target 및 loading 중 캡처 거부.
- Zoom 유지, active browser tab 변경/복귀, 원래 terminal process identity 유지.
- 소유 renderer의 제한된 busy loop 동안 두 native capture를 대기시켜 세 번째 거부;
  줌 변경 후 두 결과 모두 폐기하고 파일 미생성 및 slot 회수/후속 캡처 성공 확인.

첫 5개군 실행과 최종 6개군 실행 모두 성공했다. 최종 JSON은
native-browser-capture-background.json이며 zoom/scrolled PNG도 함께 보존했다.
Zoom 이미지에서 완성형 한글과 분해형 자모의 글리프 및 emoji가 그려진 것을 눈으로
확인했다. 특정 한 환경의 정적 페이지 결과이며 모든 글꼴/문자폭/한글 IME acceptance를
뜻하지 않는다. OS keyboard/mouse/clipboard, DOM focus, 사용자 창은 조작하지 않았다.

최종 독립 Windows crate Linux 테스트 97개, 실제 Windows Rust 테스트 126개가 통과했다.
기존 hidden browser 6개군, DOM 6개군, wait 6개군, action 8개군, terminal named keys 7개군도
최종 debug artifact에서 통과했다. 새 capture를 포함해 39개군이다. Debug/release build,
release Clippy all-targets -D warnings, fmt 및 release GUI/control/console 세 진입점의
숨김 doctor가 통과했다. 사용된 WebView2 Runtime은 112.0.1722.48이다. Frontend는 이번에
변경/재빌드/재시험하지 않았다. Installer는 NSIS로 생성했으며 실행하지 않았다.
artifacts-browser-capture.json에 debug/release/installer 7개 SHA-256을 기록했다.

cleanup-browser-capture.json은 기록된 소유 PID 39개와 fixture 경로 7개를 읽기 전용으로
조회하며 남은 일치 프로세스가 없음을 확인한다. 사용자 WSL flowmux PID 787은 실행 중이다.
이번 변경은 windows/에만 있으며 root manifest/lock, shared crates, Linux/macOS 코드는
수정하지 않았다. Linux/macOS GUI 전체 회귀를 새로 실행한 것으로 해석하면 안 된다.

B08/G09는 partial이다. 실제 IME, physical focus/DPI/다중 모니터/minimized window,
remote filesystem, 모든 close/navigation/disconnect/timeout race, callback 유실 복구는
남아 있다. Full-page stitching/desktop/terminal 캡처는 이 명령의 지원 범위가 아니다.
전체 114개 기능/13개 gate 범위를 유지하며 49개 기능 partial, 65개 pending이다.
