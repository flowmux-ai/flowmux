<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Windows GUI·콘솔 진입점 및 CLI 스트림

O01의 Windows 콘솔 진입점과 GUI/CLI 분기를 구현했다. 수정은 `windows/`에만
있으며 root Cargo 파일, 공유 core, Linux·macOS 소스는 변경하지 않았다.
전체 114개 기능·13개 게이트는 계속 partial/pending이다.

설치 프로그램은 `flowmux-command.exe`를 `flowmux.com`으로 포함한다.
일반 PATHEXT 순서에서 CMD·PowerShell의 `flowmux` 호출은 이 콘솔 실행 파일을
선택한다. 바로가기는 GUI인 `flowmux.exe`를 사용하며 `flowmuxctl.exe`는 기존처럼
CLI 전용이다. `.com`도 PE 실행 파일이며 별도의 스크립트 interpreter를 거치지
않는다. CLI 요청은 같은 Rust IPC 함수를 직접 호출하고 stdout/stderr와 종료
코드를 호출 셸에 전달한다. 설치 프로그램은 PATHEXT를 바꾸지 않는다.

Windows의 [standard handle 및 subsystem 설명](https://learn.microsoft.com/en-us/windows/console/getstdhandle)과
[AttachConsole 문서](https://learn.microsoft.com/en-us/windows/console/attachconsole)를
기준으로 GUI와 console entry point를 나눴다. Microsoft의
[devenv 설명](https://learn.microsoft.com/en-us/visualstudio/ide/reference/devenv-command-line-switches?view=visualstudio)도
콘솔 `.com`과 GUI `.exe`를 구분한다. 해당 문서는 설계 근거이며 flowmux의
동작 증거는 아래 실제 실행 기록이다.

이전 실행 파일에서 `flowmux.exe --shell=cmd --temporary`가 CLI로 잘못
분류되어 exit 2와 `unexpected argument '--shell'`를 반환했다
(`native-entrypoints-before-routing.json`). 첫 인수 문자열 비교를 제거하고
하나의 clap grammar로 launch와 command를 파싱한다. `--cwd=...`, `--shell=...`,
옵션 순서를 처리하고 launch 옵션/command 혼합은 실행 전에 거부한다.
기본 도움말은 두 사용 경로를 함께 보여 주며 version 조회도 지원한다.

콘솔 경로에서 새 창을 시작할 때는 sibling `flowmux.exe`의 절대 경로와
개별 UTF-16 인수를 사용한다. 빈 인수·따옴표·끝 역슬래시를 보존하고 NUL 및
Windows 명령줄 길이 초과를 거부한다. `--json`의 `spawned_pid`는 프로세스 생성
영수증이다. WebView/PTY 준비 완료나 이후 startup 성공을 의미하지 않는다.
새 호스트는 자신의 시작 오류를 기록/표시한다. CLI 쪽 쓰기 실패가 이미 생성된
창이나 이미 적용된 명령을 취소하지는 않는다.

초기 console launcher는 `Command`와 `Stdio::null`로 GUI를 실행했다.
실제 테스트에서는 launcher가 종료됐지만 GUI가 살아 있는 동안 stdout EOF가
오지 않았다. 소유 PID·고유 테스트 cwd·IPC identity를 확인한 기록은
`native-entrypoints-before-inheritance.json`에 있다. 해당 GUI만 정상 quit하자
검사 프로세스가 EOF를 받고 진행했다. 최종 구현은 `CreateProcessW`의
`bInheritHandles=FALSE`로 자식에게 핸들을 전달하지 않는다. 동일 런처 시나리오가
GUI를 계속 실행한 채 완료됐다. helper의 EOF 대기에도 기한을 추가했다.
새 GUI가 없거나 손상된 실행 파일인 경우에는 thread error mode를 보존/복원하며
시스템 대화상자 없이 오류를 반환한다.

GUI를 CLI 용도로 명시적으로 호출하면 부모 console에 연결하되 파일·pipe·NUL
표준 핸들을 유지한다. CLI 오류는 message box를 표시하지 않는다. 성공·도움말·
version은 exit 0, argument 오류는 2, runtime/출력 오류는 1이다. 성공은 stdout,
진단은 stderr이며 `--json` runtime 오류는 `error` 필드 객체다. argument 오류는
clap의 텍스트다. 일반 `read-screen`은 plain text이고 JSON 요청은 기존 객체다.
닫힌 stdout은 write 오류로 처리하며 panic이나 IPC 재전송을 하지 않는다.

`native-entrypoints-background.json`의 최종 숨김 실환경 검사 10개군:

1. GUI·console·control 세 실행 파일의 help/version/doctor가 stdout을 사용하고
   argument 오류는 stderr와 exit 2를 반환했다.
2. 없는 명시적 pipe는 JSON stderr와 exit 1을 반환했다. launch/CLI 옵션 혼합은
   거부했다. 별도 오류 창을 기다리지 않고 모든 프로세스가 종료됐다.
3. 첫 옵션 `--shell=cmd`와 equals cwd로 직접 GUI를 시작하고, console launcher의
   PID 영수증으로 별도의 PowerShell GUI를 시작했다. 두 호스트는 준비 완료 후에도
   숨김 상태였고 cwd의 한글·분해형 자모·이모지·공백·`&`·`#`가 보존됐다.
4. 실제 CMD와 PowerShell이 bare `flowmux`를 console entry로 선택하고 응답을
   기다렸다. PowerShell은 선택한 경로도 확인했다. JSON pipeline과 오류 종료 코드가
   전달됐으며 검사 시 PATHEXT 값을 기록했다.
5. Unicode CLI 인수, JSON/plain `read-screen`, 2개 창 사이 명시적 요청 대상이
   유지됐다. 한 창에 새 탭을 만들어도 다른 창의 탭 수는 바뀌지 않았다.
6. CMD의 파일 stdout/stderr 리디렉션에서 JSON과 진단이 분리됐다.
7. 실제 소유 ConPTY의 CMD에서 GUI CLI를 `start /b /wait`로 실행하고 `>NUL`을
   사용했다. 종료 뒤 marker는 나타났고 버려야 할 JSON은 터미널에 나타나지 않았다.
8. console→GUI→ConPTY→C# probe 경로를 통해 Unicode executable과 cwd, 빈 인수,
   포함된 따옴표·끝 역슬래시·literal metacharacter를 Ordinal 비교했다.
9. 격리한 복사본의 sibling GUI가 없거나 invalid image일 때 즉시 JSON 오류를
   반환했다. 실제 설치 디렉터리를 수정하지 않았다.
10. 소유 테스트 pipe server가 요청을 받은 뒤 CLI의 stdout reader를 닫고 응답했다.
    세 실행 파일 모두 `writing CLI stdout` 오류와 exit 1로 종료했으며 panic은 없었다.

Windows native Rust 테스트 48개와 독립 Windows crate의 Linux 테스트 21개가
통과했다. 공통 grammar 및 native UTF-16 quote 경계, 기존 IPC·ConPTY·상태·설정
테스트를 포함한다. UTF-16 테스트의 unpaired surrogate 보존은 launcher 인용
함수의 증거이며 UI/설정 전반의 잘못된 Unicode 지원을 뜻하지 않는다.
기존 숨김 셸 검사 9개군도 통과했다(`native-shells-entrypoints-background.json`).
이 회귀 실행은 NUL 보존 보완 전이고, 최종 entry-point 10개군은 그 보완 후다.
각 실행 시각은 JSON에 보존했다. 프런트엔드는 변경하지 않았다.

Release build·Clippy 및 NSIS 빌드 결과와 SHA-256은 같은 디렉터리의
`release-*-entrypoints.txt`, `installer-build-entrypoints.txt`,
`artifacts-entrypoints.json`에 기록한다. 설치 프로그램은 실행하지 않았다.
`verify-installer.ps1`에 console payload 검사를 추가했지만 이번 빌드의 실제
설치·업데이트·uninstall 완료를 증명하지는 않는다.

모든 새 호스트는 debug background 모드와 격리된 config/state 또는 temporary
모드로 실행했다. 키보드·마우스 OS 입력, foreground 변경, 사용자 클립보드,
사용자/registry PATH 변경은 하지 않았다. 테스트 자식의 process-local PATH만
조정했다. 실제 IME·메뉴·DPI, 변형 PATHEXT/alias/custom shell, 기존 플랫폼의
모든 명령 호환, clean-machine 배포와 전체 feature parity는 남아 있다.
`flowmux.exe`를 직접 부르면 GUI 프로세스에 대한 호출 셸의 대기 규칙이 적용되므로
자동화는 `flowmux.com` 또는 `flowmuxctl.exe`를 사용한다. O01은 partial이다.
원본 터미널이나 부모 Windows job 종료 후 새 GUI의 독립 수명은 이번 검사의
범위에 포함하지 않았다. 다중 창 수명·소유권 검증에서 이어서 확인해야 한다.
