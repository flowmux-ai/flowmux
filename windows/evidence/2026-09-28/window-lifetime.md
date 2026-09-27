<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# 터미널에서 연 새 Windows 창의 독립 수명

Windows 전용 구현에서 새 창의 생성 주체를 수정했다. `windows/` 밖의
Linux·macOS 소스, 공유 core, root Cargo 파일은 변경하지 않았다. 전체
114개 기능·13개 게이트는 계속 partial/pending이다.

이전 `a7b4ba1` 실행 파일로 숨김 호스트의 PowerShell 탭에서 `flowmux.com`을
실행해 별도 CMD 창을 열었다. 원본 탭을 닫자 source shell뿐 아니라 새 GUI와
그 셸도 종료됐다. 실제 PID 및 전후 상태는
`native-window-lifetime-before.json`에 기록했다. GUI에 표준 핸들을 전달하지
않는 것만으로 프로세스의 Windows Job 소속은 분리되지 않았다.

Microsoft의 [Job Objects](https://learn.microsoft.com/en-us/windows/win32/procthread/job-objects)와
[Nested Jobs](https://learn.microsoft.com/en-us/windows/win32/procthread/nested-jobs)
설명에 따르면 자식은 기본적으로 생성 프로세스의 job 계층을 따른다.
수정은 terminal job의 breakaway 정책을 바꾸는 대신 원래 GUI 호스트가
새 GUI를 생성하도록 한다. `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`와 일반
terminal descendant 정리는 그대로다. 새 창은 원래 GUI와 같은 외부 job을
가질 수 있으며, 임의의 외부 job 정책을 벗어난다고 보장하지 않는다.

native flowmux의 `FLOWMUX_PIPE_NAME` context를 가진 `flowmux.com`과
`flowmux.exe` launch 경로는 기존 검증된 Named Pipe를 통해 요청한다.
브로커는 현재 실행 중인 caller surface인지 확인하고 자기 GUI executable을
새 프로세스로 실행한다. 기존 terminal process, view, layout을 옮기지 않는다.
외부에서 시작한 launcher는 기존처럼 직접 자기 sibling GUI를 실행한다.
일반 CLI 명령은 그대로 처리한다. 내부 `launch_window`는 serde 요청이며
clap subcommand 목록에는 노출하지 않는다.

요청은 argv, 실제 caller cwd, native 환경 block을 UTF-16 단위로 전달한다.
환경은 `GetEnvironmentStringsW`로 읽어 Unicode 값, 줄바꿈과 `=C:` 등의
drive-directory 항목을 보존한다. native buffer는 복사 후 해제한다.
환경 block은 128 Ki UTF-16 단위, argv는 256항목 및 Windows command-line
한도로 제한하며 기존 전체 IPC frame 한도도 적용한다. NUL argv, 잘못된
double-NUL 환경 block, 기존 절대 경로가 아닌 caller cwd, launch가 아닌 CLI
명령은 생성 전에 거부한다. 최종 인용된 명령줄의 길이도 다시 검사한다.

새 GUI의 환경에서는 이전 pipe/socket/surface/pane/workspace/tab/CLI context만
제거한다. 새로운 PTY가 자신의 IDs와 endpoint를 받으므로 같은 launch를
재위임하지 않는다. 나머지 환경, 특히 호출 셸에서 바꾼 PATH를 보존한다.
환경·argv의 Debug 출력에는 값 대신 크기만 표시하고 환경 block을 로그에
기록하지 않는다. 검증 artifact에는 테스트용 표식의 일치 여부만 기록한다.

stale endpoint나 closed surface는 오류가 되며 다른 창이나 로컬 생성으로
fallback하지 않는다. IPC는 한 번만 제출한다. 요청 후 timeout/호출자 종료가
생기면 창이 이미 만들어졌을 수 있다. receipt의 `spawned_pid`는 생성된 PID이며
WebView/ConPTY 준비 완료나 이후 셸 시작 성공을 뜻하지 않는다.

`native-window-lifetime-background.json`의 최종 숨김 검사 7개군:

1. 원본 탭을 닫으면 원본 셸은 종료되지만 새 GUI와 CMD 셸은 같은 PID로 유지됐다.
   이전 실패와 같은 흐름으로 확인했다.
2. terminal job 안에서 GUI entry를 실행해 원본 GUI에 생성 요청을 위임했다.
   한글·분해형 자모·이모지·개행 환경 값과 relative cwd를 Ordinal 비교했다.
   child 안에서 `flowmux.com identify`를 실행해 새 GUI·surface·pipe를 확인했다.
3. source PowerShell에서 PATH에 한글 디렉터리를 추가하고 bare 이름의 C# shell
   probe를 선택했다. 원래 GUI의 PATH에 없던 exe가 실행돼 정확한 cwd, 빈 인수,
   따옴표·끝 역슬래시·Unicode argv를 파일로 기록했다.
4. 유지된 CMD 창에서 다시 새 CMD 창을 연 뒤 첫 CMD 창을 정상 quit했다.
   새 창과 해당 shell PID는 유지됐다.
5. console 및 GUI launcher에 없는 pipe 또는 닫힌 source surface를 지정했다.
   stdout에 PID를 반환하지 않고 JSON stderr/exit 1로 거부했다.
6. raw 내부 요청에서 CLI 명령, relative caller directory, malformed environment,
   NUL argv를 보냈다. 각 의미에 맞는 정확한 오류를 검사해 단순 역직렬화 실패와
   구별했다. 초기 verifier의 nested-array 형태 오류도 고친 뒤 다시 통과했다.
7. 최초 GUI를 강제로 종료했다. 원본 셸은 정리됐고 살아 있는 3개 독립 GUI는
   응답했다. PowerShell PID, 테스트 환경 값, 자기 창을 가리키는 CLI가 유지됐다.

GUI entry/복잡한 argv 검사는 source ConPTY의 PowerShell 안에서 managed
`ProcessStartInfo`를 사용했다. 이 자식도 원본 terminal job 안에서 시작하므로
수명 검증 대상은 같지만, PowerShell 5의 GUI-process 대기 및 embedded-quote
marshalling을 증명하는 테스트는 아니다. 최초 원형은 PowerShell 5가 GUI
stdout receipt를 받지 못해 실패했다. 만들어진 창은 정확한 source PID와
고유 cwd/IPC identity로 확인해 정상 종료했고, 검사 호출 방식을 명시적으로
리디렉션하는 것으로 바꿨다. 자동화에는 여전히 console entry를 권장한다.

Windows native Rust 테스트 50개와 독립 Windows crate의 Linux 테스트 23개가
통과했다. environment 형식·상한·context 제거·UTF-16 보존과 Debug redaction,
기존 IPC/ConPTY/state/settings 검사를 포함한다. 기존 entry-point 10개군과
lifecycle 3개군도 최종 코드에서 통과했다. lifecycle 검사는 정상 셸 종료의
마지막 출력, terminal close 및 GUI 강제 종료 시 3단계 descendant 정리를
확인하므로 새 창의 독립 수명이 일반 프로세스 정리를 끄지 않았다는 근거다.
각 실제 JSON 및 로그는 같은 디렉터리에 별도로 보존한다.

Release·Clippy·NSIS build와 실행 파일 SHA-256도 보존한다. 설치 프로그램은
실행하지 않았고 모든 호스트/검사 프로세스는 숨김이었다. OS 키보드·마우스
입력, clipboard, 사용자/registry PATH를 사용하지 않았다. 변경한 PATH와
환경 값은 소유한 테스트 프로세스 안에만 있었다.

기존 tab을 다른 창으로 옮기는 U06, window geometry, sleep/resume·잠금,
request-dispatch 직후 caller/broker crash의 모든 경쟁 조건, 외부 job 정책,
다른 버전의 broker/launcher 조합, release desktop·IME·DPI 및 전체 플랫폼
회귀는 미검증이다. O01·U13·O10은 partial이며 완료 판정은 하지 않는다.
