<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Windows 텍스트 붙여넣기와 bracketed paste

변경 범위는 독립 `windows/` workspace다. 기존 Linux·macOS 터미널과 공유
core를 수정하지 않았다. 전체 114개 기능·13개 게이트의 범위도 유지한다.
T08 선택·복사·붙여넣기와 T09 bracketed paste는 partial이며 완료 판정이 아니다.

`paste TEXT --pane ID` 또는 `paste TEXT --surface ID`는 OS clipboard를 읽지
않는 명시적 텍스트 명령이다. 생략한 대상은 호출 surface 또는 현재 탭이다.
기존 `send-keys`는 원래의 직접 입력 경로를 유지한다. `paste`는 ConPTY reader가
받은 출력까지 xterm parser가 처리한 다음 현재 모드로 `terminal.paste`를 호출한다.
이 barrier는 이미 받은 mode 변경보다 paste가 앞서는 문제를 막는다. 요청 이후
프로세스가 출력할 미래 mode 변경까지 예측하거나 셸 실행 완료를 보장하지 않는다.

동작은 고정한 xterm 6.0.0 구현 및
[공식 Terminal API](https://xtermjs.org/docs/api/terminal/classes/terminal/),
[IModes API](https://xtermjs.org/docs/api/terminal/interfaces/imodes/)를 확인했다.
xterm이 LF·CRLF를 CR로 바꾸고, `CSI ? 2004 h/l`의 현재 모드에 따라
`ESC[200~` / `ESC[201~`를 붙인다. NFC/NFD 변환은 하지 않는다.
문자열 안의 literal ESC와 end marker도 보존되므로 bracketed paste를
임의의 clipboard 내용을 안전한 명령으로 바꾸는 기능으로 설명하지 않는다.

프런트엔드는 xterm의 동기 onData를 한 번 포착해 paste 응답에 담는다.
일반 키 입력이나 Shift+Enter 변환 경로로 중복 전송하지 않는다. native host는
surface/generation/token 검증, 대상·출력 sequence·실행 상태 확인 후 전체 payload를
한 번 input queue에 넣는다. CLI 응답의 `accepted_bytes`는 bracket을 포함한
UTF-8 바이트 수이며 `delivery: queued`는 큐 접수만 뜻한다. 이후 프로세스 종료나
write 실패로 일부/전체 입력이 소비되지 않을 수 있다. 응답 유실 시 자동 재시도하지 않는다.

원문은 UTF-8 128 KiB까지 허용한다. 이는 모든 바이트가 JSON 6바이트 escape로
확장돼도 기존 1 MiB bridge frame에 들어가도록 정한 상한이다. NUL과 잘못된
Unicode는 거부하며, 상한 초과를 자르거나 일부 전송하지 않는다. 빈 입력은 bracket도
보내지 않는다. OS command-line 한도는 별개여서 큰 본문의 live 검사는 raw Named
Pipe 요청을 사용한다. CLI paste는 terminal당 하나, 전체 16개 pending 한도이며
12초 기한, 닫힌 대상, 중복·만료 응답을 처리한다. Native queue 접수 실패도 CLI에
반환한다. 모든 timeout/close/queue-full 경쟁 조건을 live 재현한 것은 아니다.

터미널 DOM의 paste event도 같은 포착 경로를 쓴다. terminal ancestor의 capture
handler가 기본 이벤트를 취소하므로 xterm bubbling handler가 다시 전송하지 않는다.
DOM paste는 현재 parse된 mode를 사용하며 입력 event 순서를 지연시키지 않는다.
검색 입력창은 handler 범위 밖이다. 조합 중·복원 중·입력 비활성 상태는 거부하고
오류를 terminal 안에 표시한다. 조합 확정/취소를 강제하거나 나중에 paste를 재생하지
않는다. `compositionend` 직후 및 keyCode 229 처리 직후에는 xterm 6의 지연
확정 task가 실행될 때까지 paste guard만 유지한다. guard 해제 timer는 문자열을
전송하지 않는다. 이 순서의 단위 검사도 추가했지만 실제 IME는 사용자의
백그라운드 작업 요청 때문에 재검증하지 않았다. 정상적인 글자 생성은 계속
xterm/Windows IME가 담당한다.

`scripts/verify-paste.ps1`는 고유 디렉터리·임시 상태의 숨김 debug host와 자신이 만든
`PasteProbe.cs` ConPTY child만 사용한다. foreground는 읽어서 검사할 뿐 바꾸지 않는다.
실제 키보드/마우스 이벤트, 사용자 clipboard, 설치 프로그램을 사용하지 않는다.
probe는 private file로 mode 변경을 받아 출력하고 `ReadConsoleW`에서 받은 Unicode를
strict UTF-8 encoder로 저장한다. 이 파일과 debug opt-in pre-ConPTY 입력 trace를
expected 바이트와 각각 비교한다. 테스트 데이터의 SHA-256을 증거 JSON에 보존한다.

최종 결과는 `native-paste-background.json`에 기록했다. 숨김 live 검사 12개 항목:

1. 완성형 한글·분해형 자모·이모지·탭·CR/LF/CRLF의 비정규화 및 개행 변환.
2. mode 활성화 이후 한글 다중 행의 정확한 bracket 한 쌍.
3. 빈 문자열에서 입력과 bracket 없음.
4. 본문 안 literal escape와 paste marker 보존.
5. mode 비활성화 직후 bracket 제거.
6. 정확히 131072 UTF-8 바이트 원문의 무손실 전송. bracket 포함 131084바이트.
7. 다른 탭을 활성화해도 숨김 surface의 mode·대상 유지, 활성 탭 유지.
8. 131073바이트 요청 전체 거부.
9. NUL 포함 요청 전체 거부.
10. pane/surface 동시 지정 거부.
11. 없는 surface 거부.
12. 정상 종료한 probe에 paste 거부, 프로세스 재시작 없음.

첫 verifier 실행은 trace에 ConPTY 초기 device/focus 응답까지 포함해 실패했다.
실제 console 입력은 이미 expected와 같았다. probe READY 이전 startup 바이트를
명시적인 별도 경계로 기록하고, 이후 대상의 모든 바이트를 비교하도록 검사기를
수정했다. 성공 판정을 위해 입력 손실이나 임의의 중복 바이트를 제거하지 않았다.

프런트엔드 테스트는 composition/restore/disabled guard, Shift+Enter와 분리,
중복·부분 emission 거부, UTF-8 상한과 surrogate, DOM 이벤트 소유권,
parser-completion barrier를 포함한다. DOM 테스트는 event 객체를 사용하는
단위 검사이며 실제 OS clipboard나 Microsoft IME 검사가 아니다.

최종 프런트엔드 27개, Windows native Rust 52개, 독립 Windows crate의 Linux
테스트 25개가 통과했다. 기존 숨김 찾기 7개군과 저장·복원 8개군도 최종 실행 파일에서
통과했다. 기존 검색 및 history restore 경로가 새 paste 처리와 함께 동작하는
검사이며 기존 Linux/macOS 애플리케이션 전체 회귀를 대신하지 않는다.
Release·Clippy·NSIS build 로그와 최종 실행 파일의 SHA-256을 같은 디렉터리에
보존한다. 설치 프로그램은 빌드만 했으며 실행하지 않았다.

실제 desktop clipboard, Ctrl+V/Ctrl+Shift+V·native context menu, 마우스 선택·더블클릭,
TUI redraw 이후 copy cache, clipboard 소유권 경쟁, 다른 IME/셸/TUI 조합은 남아 있다.
128 KiB 초과 clipboard의 스트리밍 paste도 지원하지 않는다. 해당 항목과
기존 플랫폼 전체 live 회귀에 대해 통과 또는 불가능 판정을 내리지 않는다.
