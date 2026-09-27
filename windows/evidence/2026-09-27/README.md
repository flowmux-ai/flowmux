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

## 아직 검증하지 못했거나 구현 중인 범위

한자 후보창·다중 DPI·모니터 이동·조합 중 탭/창 이동, clipboard와 NFC/NFD/emoji
폭 일치, editor/search/rename IME 전체, thread 생성 실패 등 추가 실패 경로,
최소화/잠금/16개 터미널 출력 부하, 브라우저 자동화·에디터·파일/Git·agent·SSH,
새 사용자·WebView2 미설치·오프라인 설치·코드 서명은 별도 작업으로 남아 있다.

기존 플랫폼의 root `cargo fmt --all -- --check`와 headless `cargo check --locked`는
Windows 작업 공간 분리 후 통과했다. macOS의 실제 실행 회귀 검증을 대신하지 않는다.
Windows 커밋은 기존 플랫폼 소스·root Cargo 파일·기존 빌드 경로를 수정하지 않는다.
