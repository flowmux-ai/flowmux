<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Windows 셸 선택·인수·시작 실패 복구

Windows 전용 코드에 기본 셸, 새 탭·workspace별 셸, split의 셸 상속,
실행 인수 및 재시작 시 셸 복원을 추가했다. `windows/` 밖의 Linux·macOS
소스·root Cargo 파일·공유 core는 변경하지 않았다. 전체 114개 기능·13개
게이트는 계속 partial/pending이다.

`powershell`은 System32의 Windows PowerShell, `cmd`는 System32의 CMD를
사용한다. `pwsh`는 PATH 및 ProgramFiles의 PowerShell 7 경로를 탐색한다.
CLI `shells`가 실제 탐색 결과를 보고한다. 절대 `.exe`/`.com` 경로나 PATH의
실행 파일 이름도 지정할 수 있다. PATH에서는 절대 디렉터리만 탐색하고
PATHEXT의 `.COM`/`.EXE` 순서를 따른다. cwd의 같은 이름 파일을 암묵적으로
추가 탐색하지 않는다. `.bat`/`.cmd` 파일은 interpreter를 명시해야 한다.

실행 파일 경로는 `CreateProcessW`의 application 인수로 전달하고 argv는
각 항목을 따로 인용한다. 빈 값·따옴표·끝의 역슬래시를 보존한다. 일반적인
셸 명령 문자열로 다시 합쳐 해석하지 않는다. 인용 규칙은 Microsoft의
[C runtime argv 규칙](https://learn.microsoft.com/en-us/cpp/c-language/parsing-c-command-line-arguments?view=msvc-170)을
따르며 `cmd /C` 등 고유 command parser의 의미까지 바꾸지는 않는다.
[CreateProcessW](https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-createprocessw)의
32,767 UTF-16 단위 제한은 실제 확장한 명령줄에서도 검사한다.

기본 셸은 기존 공유 config에 저장하며 다음 새 탭·workspace에 적용한다.
실행 중인 셸은 바꾸지 않는다. split은 원본 셸·인수를 상속하거나 명시적
override를 사용한다. CMD는 `/D`로 AutoRun을 끄고 자식 환경의 기존 PROMPT
앞에 cwd 보고만 추가한다. console encoding과 사용자 registry/profile은
바꾸지 않는다. built-in PowerShell은 기존 세션 한정 prompt wrapper를 쓴다.
직접 지정한 executable은 supplied argv만 받으며 자동 prompt 통합을 넣지 않는다.

`--cwd`의 상대 경로는 CLI/launcher 호출 위치에서 절대 경로로 만든다.
cwd를 생략하면 원본 터미널의 보고된 경로를 상속한다. 직접 IPC의 상대 경로는
원본 터미널 기준이며 모호한 drive-relative 경로는 거부한다. 프로세스마다
환경·PTY·kill-on-close job 소유권은 기존과 같다.

window state에는 surface별 program/argv를 저장한다. 없는 예전 형식은
당시 동작인 Windows PowerShell을 복원한다. 새 기본값이 이전 탭을 바꾸지
않는다. 복원은 기록된 argv로 새 프로세스를 실행하므로 명시한 startup
명령은 다시 실행된다. 저장된 화면의 텍스트를 명령으로 실행하지는 않는다.
없어진 executable은 그 터미널만 실패 상태가 되고 다른 셸과 기록을 유지한다.
실행 실패 탭은 저장할 수 있고 `retry-shell`/Command Prompt 복구 버튼이
같은 surface에서 다시 시작한다. 실행 중이거나 이미 정상 종료한 프로세스를
retry 명령으로 대체하지 않는다.

첫 실환경 시도는 잘못된 exe를 시작할 때 tree 응답이 IPC 기한 안에 오지
않았다(`native-shells-before-error-mode.json`). 오류 대화상자 대신 오류를
호출자에게 반환하도록 thread error mode를 설정하고 이전 값을 복원한 뒤
동일 시나리오가 통과했다. 수정 후에는 실제 OS 오류 216이 상태에 보고되고
동일 surface가 CMD로 복구됐다. 첫 멈춤의 native stack이나 대화상자 자체를
수집한 것은 아니므로 이를 별도의 확정된 stack 분석으로 해석하지 않는다.
이 동작은 [SetThreadErrorMode 문서](https://learn.microsoft.com/en-us/windows/win32/api/errhandlingapi/nf-errhandlingapi-setthreaderrormode)의
critical-error 처리 방식에 따른다. 초기 실패 경로는 읽기 worker가 소유하기
전의 출력 pipe를 먼저 닫고 ConPTY를 해제하도록 drop 순서도 보완했다.
[ClosePseudoConsole 문서](https://learn.microsoft.com/en-us/windows/console/closepseudoconsole)는
출력을 계속 읽거나 pipe를 먼저 닫지 않으면 정리 중 대기가 발생할 수 있다고
명시한다. debug spawn trace에는 단계·PID만 기록하며 argv나 입력은 기록하지 않는다.

추가 상태 검토에서 CMD로 복구했지만 탭의 자동 제목은 실패한 exe 경로로
남은 사례도 확인했다(`native-shells-title-before.json`). retry 시 자동 제목을
새 셸로 갱신하고 이후 cwd 보고가 이어지게 했다. 사용자가 잠근 이름은 그대로
보존한다. 같은 숨김 복구 흐름에서 두 경우를 확인했으며 저장된 결과는
`native-shells-title-after.json`에 있다. 실제 native caption의 시각적 검증과는
구별한다.

`native-shells-background.json`의 숨김 실환경 검사 9개군:

- 기본값을 CMD로 바꾼 뒤 기존 PowerShell PID는 유지됐고 새 탭은 실제
  cmd.exe로 실행됐다. 한글·분해형 자모·이모지·공백·`&`·`#` 경로를 Ordinal
  비교했다.
- CMD의 `cd /d` 후 cwd 보고, 한글 출력, split의 셸/cwd 상속 및 PowerShell
  `-NoProfile` 탭 override를 확인했다.
- 없는 프로그램·폴더·잘못된 기본값·실행 중인 탭의 retry를 거부하고 layout과
  focus를 유지했다. 25,000자 한글 argv는 Windows 명령줄 한도 안에 있어도
  UTF-8 config의 64KiB를 초과하므로 저장 전에 거부하고 기존 hash를 유지했다.
- 상대 `--cwd`를 CLI 호출 위치에서 해석했고 GUI/원본 터미널의 다른 cwd와
  혼동하지 않았다. 상대 cwd로 시작한 새 창도 절대 경로를 저장했다.
- 한글 경로의 실제 C# console probe가 빈 인수, NFD/이모지, 포함된 따옴표,
  마지막 역슬래시, `& ... > ...`를 정확한 argv로 받았다. 표식 명령은 실행되지
  않았고 cwd·surface 환경·최종 한글 출력·exit code 17도 확인했다. 복잡한 argv는
  PowerShell 5의 marshalling을 배제하기 위해 이 검사의 직접 IPC로 전달했다.
- invalid-image 실패 탭을 저장하고 명시적 retry로 CMD를 시작했다. 탭 ID가
  유지됐고 실패 상태와 실패한 exe의 자동 제목이 해제됐다.
- 혼합 셸·인수·cwd·ID·한글 기록을 새 프로세스로 복원했다.
- 복원할 executable을 없애도 다른 셸이 시작됐으며 실패 탭의 기록은 CMD
  retry 후에도 검색됐다. 사용자가 잠근 한글 제목은 retry 후에도 유지됐다.
- shell map이 없는 예전 checkpoint는 PowerShell로 복원하고 이후 새 탭은
  현재 기본 CMD를 사용했다.

Windows native Rust 테스트 46개와 독립 Windows crate의 Linux 테스트 20개가
통과했다. 실제 파일을 사용한 PATH/PATHEXT 순서, argv 경계·NUL·길이 제한,
구형 CLI/JSON 호환과 shell state 검증을 포함한다. invalid executable을 5회
시도해 각 호출이 5초 이내 반환하고 thread error mode가 복원되며 handle 수가
허용치(초기 + 4)를 넘지 않는지 확인했다. UTF-8 config 초과 거부도 포함한다.
기존 프런트엔드 20개 검사와 asset build도 통과했지만 새 복구 버튼의 실제
클릭·렌더링을 증명하는 테스트는 아니다.

이 단계에서 기존 설정 7개군, 상태 복원 8개군, 프로세스 수명 3개군 및 100회
탭 생성·닫기 검사를 통과했다. 명시한 JSON 파일은 각각
`native-settings-shells-background.json`, `native-state-shells-background.json`,
`native-lifecycle-shells-background.json`, `native-cycles-shells-background.json`이다.
최종 cwd 회귀 검사는 `native-cwd-shells-background.json`에 기록한다.
설정 크기·경로 보완 전후의 각 시행 시각은 JSON에 보존한다. 단일 시점의 하나의
장시간 성능 실험으로 합쳐 해석하지 않는다. Release build·Clippy 및 NSIS
설치 파일 빌드 기록과 hash는 같은 디렉터리의 관련 파일에 보존한다.
설치 프로그램은 실행하지 않는다.

호스트 창을 숨기고 소유한 PTY/pipe만 사용했다. OS 키보드/마우스 입력과
사용자 클립보드는 사용하지 않았다. PowerShell 7은 이 PC에 설치/탐색되지
않아 실제 실행 검증은 남아 있다. Git Bash 등 다른 interactive shell,
다양한 script parser/AutoRun/profile, App Execution Alias, UNC·매우 긴 경로,
실제 메뉴·복구 버튼·IME·DPI·접근성, worker/thread 생성 실패의 fault injection,
WebView/GPU 장애와 모든 기존 플랫폼 runtime 회귀는 별도 검증이 필요하다.
T01·T18·T23·U12·O06은 전체 지원 완료로 바꾸지 않는다.
