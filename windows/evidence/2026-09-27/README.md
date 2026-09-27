<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Windows 초기 구현 검증

이 증거는 Windows 호스트의 첫 구현분을 검증한다. 전체 flowmux 기능이나
114개 기능 항목·13개 검증 게이트의 완료 증거가 아니다.

검증 환경은 Windows 11 build 22623, Windows PowerShell 5.1, Microsoft 한글 IME,
WebView2 112.0.1722.48이다. Rust/Win32 창과 WebView2 터미널을 실제 실행했으며
셸은 WSL을 거치지 않는 Windows 프로세스다.

| 검증 | 결과 | 증거 |
|---|---|---|
| PowerShell·한글 UTF-8·4분할·기존 PID 유지 | 통과 | `native-smoke-bundled-conpty.json` |
| 탭 생성·종료 100회 | 셸 종료 및 핸들 수 안정 | 같은 파일의 `cycleSamples` |
| 실제 IME 11개 사례 | 통과 | `native-ime-bundled-conpty.json` |
| 조합 중 셸 전송 없음·커서 숨김 상태 preedit | 확인 | IME JSON, `hidden-cursor-preedit.png` |
| 설치·재설치·제거·PATH 보존 | 현재 사용자 환경에서 통과 | `installer-smoke-bundled-conpty.json` |
| Windows 네이티브 Rust 테스트 | 7개 통과 | `native-rust-tests.txt` |
| 정상 종료 후 한글 출력·종료 코드·화면 유지 | 통과 | `native-lifecycle.json`, `normal-exit.png` |
| 탭 닫기·호스트 강제 종료 시 자손 프로세스 정리 | 셸 + 자손 3개 모두 종료 | `native-lifecycle.json` |
| lifecycle 수정 후 100회 반복 | process handle 13 유지, 전체 295/295/294/294 | `native-smoke-lifecycle.json` |
| Linux에서 독립 모델/프로토콜 테스트 | 4개 통과 | `linux-rust-tests.txt` |
| 프런트엔드 바이트/입력 순서 테스트 | 3개 통과 | `terminal/src/*.test.mjs` 재실행 가능 |
| Windows release Clippy | 경고 없이 통과 | `release-clippy.txt` |
| 창을 숨긴 상태의 pane/workspace 이동·재정렬 | PID·출력·cwd·CLI 대상 보존, workspace 이동 40회 | `native-tab-move-background.json` |
| 이동 구현 후 백그라운드 100회 생성·종료 | process handle 13 유지, 전체 260/260/261/261 | `native-smoke-move-background.json` |
| 이동 구현 후 백그라운드 lifecycle | 정상 종료·자손 정리 통과 | `native-lifecycle-move-background.json` |
| 이동 구현 후 모델/IPC 및 네이티브 테스트 | Linux 7개, Windows 10개 통과 | `linux-rust-tests-move.txt`, `native-rust-tests-move.txt` |
| 이동 구현 후 Windows release Clippy | 경고 없이 통과 | `release-clippy-move.txt` |
| 숨김 호스트 1/4/16 pane 동시 출력 | 각 1만 줄, 마지막 한글 출력·parser barrier 통과 | `native-output-load-background.json` |
| 비활성 tab 출력 조회 | 출력 완료 및 focus 유지 | 같은 파일의 `hiddenTab` |

`artifacts.json`은 첫 구현, `artifacts-lifecycle.json`은 세션 종료 개선 후 검증한
실행 파일과 설치 파일의 크기·SHA-256을 기록한다. 개선 후 실제 IME 11개 사례와
설치·재설치·제거도 다시 통과했다(`native-ime-lifecycle.json`,
`installer-smoke-lifecycle.json`).
설치 검증은 자체 임시 설치 경로를 만들고 제거했으며 기존 WSL flowmux 창을
조작하거나 사용자 작업을 종료하지 않았다.

## 재현 중 구분한 문제

1. 시스템 기본 ConPTY는 종료한 세션마다 process handle이 하나씩 증가했다.
   WebView 없는 Windows 단독 테스트에서도 20회에 64 → 84로 재현됐다.
   공식 ConPTY 패키지를 앱에 동봉한 뒤 단독 테스트와 실제 100회 반복에서
   증가가 사라졌다. 불투명한 OS 핸들 구조를 직접 수정하는 우회는 사용하지 않는다.
2. 처음 사용한 .NET 콘솔 바이트 스트림은 한글을 잘못 측정했다. Unicode
   `ReadConsoleW` 경계에서 받아 UTF-8로 기록하도록 측정기를 교정했다.
3. 방향키의 확장 키 플래그가 빠지면 테스트 입력이 `Numpad4`로 전달됐다.
   DOM의 key/code/modifier 기록으로 원인을 확인하고 실제 `ArrowLeft`로
   생성한 뒤 Shift+Left 입력 순서와 modifier가 함께 통과했다.
4. 부모 프로세스가 콘솔 출력을 리다이렉트한 상태에서 ConPTY 셸을 시작하면
   출력이 부모 콘솔로 새고 조기 종료될 수 있었다. 명시적인 standard handle
   초기화 후 정상 종료 코드 7과 마지막 출력이 PTY를 통해 전달된다.
5. ConPTY의 호스트 참조를 초기 client 시작 후 release하고, process 종료와
   출력 EOF를 모두 받은 뒤 세션을 해제한다. 화면 및 아직 도착 중인 parser
   ACK는 유지한다. 정상 종료 시 마지막 출력 검사는 명령 echo와 구별한다.

`native-ime-debug.json`은 ConPTY 전달 전 바이트도 비교한 초기 디버그 검증이다.
최종 동봉 패키지 검증은 `native-ime-bundled-conpty.json`을 기준으로 한다.
`*-initial.json`은 교체 이전의 제한된 스모크 기록이며 최종 게이트 판정에 쓰지 않는다.

`artifacts-tab-move.json`은 이후 이동 구현의 debug/release 실행 파일과
재생성한 개발용 설치 파일을 구분해 기록한다. 이 단계는 숨김 debug 호스트
검증이며 최신 설치 파일의 실제 IME 검증을 의미하지 않는다.

## 탭 이동 검증의 경계

새 이동 기능은 같은 창에서 기존 PTY와 WebView를 유지한다. 이동 전 환경 변수에
남은 pane ID 대신 surface ID로 현재 위치를 조회하며, 다른 탭이 활성 상태일 때
이동한 셸이 직접 호출한 `identify`와 `read-screen`도 자신의 터미널을 반환했다.

Windows 데스크톱을 동시에 사용하던 사용자의 입력이 실기 시험에 섞였다는
확인이 있어 이후 검증은 백그라운드로 전환했다. 메뉴 및 조합 중 이동 시험은
완료 판정에 사용하지 않는다. 같은 WebView에 중복으로 포커스를 주지 않도록
보완했지만, 이 경로의 실제 IME 결과는 재검증 전이다. 앞선 IME 11개 합격 결과를
최신 focus 변경의 합격 근거로 확장하지 않는다.

숨김 모드에서는 최상위 창을 표시하지 않고 native focus를 요청하지 않는다.
표시 상태/foreground 여부와 40회 이동 후 출력 보존을 실제 Windows에서 확인했다.
화면을 가상으로 채우거나 문자열 전송을 IME 검증으로 간주하지 않았다.

## 출력 부하와 메모리

PowerShell 표준 출력으로 각 pane에 1만 줄의 한글·색상 출력을 동시에 보냈다.
시작 gate부터 마지막 화면 조회까지 1/4/16 pane에서 약 0.98/2.52/5.99초였다.
계측용 코드 실행과 CLI polling도 포함하므로 renderer 처리량 자체를 나타내지 않는다.

출력 후 전체 프로세스 트리의 private memory는 약 276/732/2303MiB였다.
16 pane에서는 PowerShell 16개, ConPTY 16개, WebView2 21개 프로세스가 포함됐다.
Rust 호스트만 측정한 수치가 아니며 peak나 장기 안정성 결과도 아니다. 특히
다수의 셸과 WebView를 유지하는 현재 구조의 메모리 비용은 추가 개선 대상이다.
최소화·잠금·복귀 및 release 빌드의 같은 부하 검증은 별도로 남아 있다.

## Windows 상태 저장·복원

`native-state-background.json`은 고유한 테스트 폴더와 숨김 호스트만 사용했다.
2개 workspace의 터미널 4개(비활성 tab 포함)를 저장하고 재시작하여 ID·배치·
선택 상태·시작 cwd·한글 문자열·초록색 SGR 보존과 새 셸 PID를 확인했다.
명령처럼 보이는 기록과 VT 응답 요청을 넣어도 해당 명령의 파일 생성은 발생하지
않았다. 복원 기록은 새 ConPTY의 초기 화면 지우기 이후에도 scrollback에 남았다.

같은 창 상태의 중복 복원 거부, 다른 창의 독립 저장, 강제 종료 후 잠금 해제와
복원, 30초 주기 자동 저장, 최신 닫힌 창 자동 선택도 통과했다. 파일 교체를
거부하는 핸들을 열어 저장 실패를 유도했을 때 이전 파일의 SHA-256이 유지됐고
창이 열린 채 남았다. 명시적 저장 취소 종료 및 임시 실행도 기존 파일을 보존했다.

셸 프로세스·agent 실행 상태를 이어가는 기능은 아니다. 이 단계에서는 시작 cwd를
복원했으며 이후 셸 내부 `cd` 추적은 아래 별도 검증으로 추가했다. 정상 buffer의 색상 기록만 저장하며 alternate
screen, 창 위치·크기·설정, 모든 Unicode/줄바꿈 조합, 자동 저장 중/복원 직후 실제 IME 입력과
네이티브 저장 실패 대화상자는 아직 검증하지 않았다. 기록은 terminal별
128KiB로 제한되며 마지막 저장 이후 출력은 강제 종료 시 유실될 수 있다.

저장·복원 구현 후 Windows 네이티브 Rust 테스트 12개, Linux 독립 테스트
8개, 프런트엔드 테스트 6개와 release Clippy를 통과했다
(`native-rust-tests-state.txt`, `linux-rust-tests-state.txt`,
`frontend-tests-state.txt`, `release-clippy-state.txt`).
기존 탭 이동 40회·lifecycle·생성/종료 20회도 숨김 모드로 재검증했다
(`native-tab-move-state-background.json`, `native-lifecycle-state-background.json`,
`native-smoke-state-background.json`). `artifacts-state.json`은 이 단계의 실행 파일과
다시 만든 개발용 설치 파일을 기록한다. 새 설치 파일의 실제 IME/설치 합격 증거는 아니다.

## 현재 셸 경로와 한글 경로 보존

`native-cwd-background.json`은 창을 숨기고 고유 상태 폴더를 사용했다.
한글 완성형·분해형 자모·결합 악센트·공백·`%#;'`가 포함된 경로를 실제
PowerShell에서 변경하고 새 tab/split의 시작 위치, 비활성 tab의 독립 갱신,
workspace 이동 후 경로 유지 및 5개 surface의 재시작 복원을 확인했다.
프롬프트 출력 전에 같은 명령줄에서 `cd; flowmuxctl new-tab`을 실행해도
새 디렉터리를 상속했다. 기록에 삽입한 과거 OSC는 복원 경로를 바꾸지 않았다.

초기 구현의 문자 그대로인 OSC 경로 출력은 Windows PowerShell의 코드 페이지
949를 거치며 결합문자를 `?`로 바꿨다. OSC 7의 ASCII URI로 전달하도록 수정한
뒤 원래 Unicode 경로와 정확히 일치했다. 콘솔 인코딩을 강제로 바꾸지 않는다.
프롬프트 wrapper를 closure로 만들면 `$pwd` 등 이전 값을 보관할 수 있어,
고정 script template과 생성한 식별자로 원래 프롬프트를 참조하도록 했다.
사용자 프롬프트가 실패 상태·종료 코드 17·변경된 일반 변수·현재 `$pwd`를
읽는 것, 재설치의 멱등성, PSReadLine 함수·키 설정·코드 페이지 보존을 확인했다.

Registry provider에서는 마지막 파일시스템 경로를 유지했고 원격 OSC 7과 UNC
OSC 9 경로는 무시했다. UNC/device 경로 추적, 다른 셸, 긴 경로의 프로세스 시작,
다양한 prompt framework는 지원 또는 검증이 남아 있다. Constrained Language
mode에서는 자동 prompt wrapper를 설치하지 않는다.

OSC 제목 갱신 시 사용자 지정 제목과 활성 표시가 사라지던 native caption도
수정했다. 숨김 창의 BUTTON 문자열을 읽어 확인했으며 화면 표시나 입력을
사용하지 않았다. 이 결과는 IME 조합·후보창·DPI 검증을 대신하지 않는다.

Windows 네이티브 Rust 14개, Linux 독립 Rust 9개, 프런트엔드 8개 테스트와
release Clippy 결과는 `native-rust-tests-cwd.txt`, `linux-rust-tests-cwd.txt`,
`frontend-tests-cwd.txt`, `release-clippy-cwd.txt`에 기록한다.
상태 저장·복원, 탭 이동 40회, 정상 종료·자손 정리도 최신 prompt 구현으로
다시 통과했다(`native-state-cwd-background.json`,
`native-tab-move-cwd-background.json`, `native-lifecycle-cwd-background.json`).
`artifacts-cwd.json`은 해당 실행 파일과 다시 만든 개발용 설치 파일을 식별한다.
최신 설치 파일의 실제 IME·설치 검증은 보류 상태다.

## 단일 터미널 검색

`native-find-background.json`은 숨김 Windows 호스트에서 같은 검색 UI controller를
CLI로 호출했다. 한글이 포함된 scrollback 결과 3개 사이의 다음·이전·순환,
대소문자 구별, 정규식 및 잘못된 정규식 이후 복구를 확인했다. 선택 문자열은
분해형 자모·결합 악센트·이모지를 그대로 보존했고 한글 검색어가 soft-wrap
경계를 걸쳐 있어도 일치했다. 숨은 tab 검색은 활성 tab을 바꾸지 않았으며
이동한 tab과 종료 후 화면도 검색할 수 있었다.

실제 ConPTY 출력이 `CACHE_OLD`를 `CACHE_NEW`로 덮어쓰고 같은 커서 위치로
돌아왔을 때, 화면 읽기는 새 문자열을 반환했지만 검색은 이전 문자열을
반환하는 결함을 재현했다. 사용 중인 addon-search 0.16의 line cache는
cursor/linefeed/resize 이벤트로 무효화되므로 이 출력에서 남아 있었다.
파싱된 출력 뒤 다음 검색 전에 addon을 공개 API로 재생성하도록 수정한 후,
같은 네이티브 재현에서 이전 문자열 미검출·새 문자열 선택을 확인했다.
터미널이나 셸을 교체하지 않는다. Alternate 화면과 normal history도 서로
검색 결과를 섞지 않았고 normal 복귀 후 이전 기록 검색이 유지됐다.

프런트엔드 자동 검사는 조합 중 Enter·Escape·legacy 229 이벤트가 이동/닫기를
실행하지 않는 것, parser barrier, 검색 결과 크기 제한 시 emoji surrogate pair
보존을 포함한다. 실제 Microsoft IME 조합이나 키보드·마우스를 사용한 검증은
아니다. 버튼·포커스 복귀·검색창 IME·DPI는 사용자 요청에 따라 보류했다.
정규식은 JavaScript 문법이며 VTE/PCRE2 전체 호환 및 복잡한 표현식의 지연 시간,
모든 터미널을 한 번에 검색하는 UI는 별도 구현·검증 대상이다.

이번 단계의 Linux 독립 Rust 9개, 프런트엔드 12개 테스트와 Windows release
Clippy 결과는 `linux-rust-tests-find.txt`, `frontend-tests-find.txt`,
`release-clippy-find.txt`에 기록했다. 기존 상태 저장·복원과 한글 경로 검증도
최종 빌드에서 통과했다(`native-state-find-background.json`,
`native-cwd-find-background.json`). `artifacts-find.json`은 debug/release 실행
파일과 갱신한 개발용 설치 파일을 기록하며 실제 설치 검증은 추가하지 않았다.

## 남은 검증 범위

한자 후보창·다중 DPI·모니터 이동·조합 중 탭/창 이동, clipboard와 NFC/NFD/emoji
폭 일치, editor/search/rename IME 전체, thread 생성 실패 등 추가 실패 경로,
최소화/잠금/16개 터미널 출력 부하, 브라우저 자동화·에디터·파일/Git·agent·SSH,
새 사용자·WebView2 미설치·오프라인 설치·코드 서명은 별도 작업으로 남아 있다.

기존 플랫폼의 root `cargo fmt --all -- --check`와 headless `cargo check --locked`는
Windows 작업 공간 분리 후 통과했다. macOS의 실제 실행 회귀 검증을 대신하지 않는다.
Windows 커밋은 기존 플랫폼 소스·root Cargo 파일·기존 빌드 경로를 수정하지 않는다.
