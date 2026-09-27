<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Windows 터미널 설정 검증

Windows 전용 호스트에 글꼴·크기·색상·커서·scrollback 설정과 저장·실시간
반영을 추가했다. 변경은 `windows/` 안에 한정했다. Linux·macOS 소스, root
Cargo 파일과 공유 core는 수정하지 않았다. 전체 114개 기능·13개 게이트의
완료 증거는 아니며 T07·T20·U10·O06은 partial이다.

설정은 native **Settings…** 메뉴와 `settings show/set/reset` CLI를 통해
변경한다. 글꼴 크기는 정수 6–72px, scrollback은 0–100000행이다. 커서는
block/underline/bar 및 점멸 여부를 지원하며 terminal dark/light 테마를
제공한다. 크기 확대·축소·초기화 메뉴도 같은 공유 font size를 바꾼다.

`%LOCALAPPDATA%\flowmux\windows\config.json`에 version·revision·terminal을
저장한다. 최대 64KiB이며 알 수 없는 필드·버전·잘못된 값은 거부한다. 쓰기는
OS가 보유한 배타적 lock 안에서 최신 파일을 다시 읽고 해당 필드만 병합한다.
임시 파일을 sync한 뒤 원자적으로 교체하며, 실패하면 이전 파일과 실행 값을
유지한다. 편집창의 원래 값 또는 CLI `--expected`와 달라진 값은 덮어쓰지 않는다.
경쟁 쓰기의 lock 획득 실패는 재시도가 필요한 오류로 반환한다.

각 창의 worker가 유휴 상태에서 약 1초마다 파일 전체를 비교한다. 수동 편집이
revision을 유지해도 변경을 감지한다. 잘못된 파일은 보존하고 오류를 표시한다.
실행 중인 창은 마지막 정상 값을 유지하고 새 창은 기본값을 쓴다. 명시적인
reset만 잘못된 파일을 대체한다. 백그라운드 검사에서는 별도 config/state
디렉터리를 사용하고 일반 사용자 파일을 읽거나 변경하지 않는다.

설정 반영은 기존 xterm 인스턴스의 공개 options를 갱신하고 크기를 다시 계산한다.
셸·WebView를 재생성하거나 입력을 주입하지 않는다. 터미널 IME 조합 또는 기록
복원 중에는 마지막 설정만 보관했다가 종료 후 적용한다. 각 WebView는 실제
options와 색상을 acknowledgement로 보낸다. 호스트는 현재 revision·설정과
일치하는 응답만 수락한다. CLI 쓰기 성공은 저장 완료를 의미하며 모든 터미널의
적용 완료까지 기다리지는 않는다. `settings show`로 적용 상태를 확인할 수 있다.

`native-settings-background.json`의 실제 숨김 Windows 호스트 검사 7개군:

- 창 A의 터미널 3개와 별도 창 B에 14→24px를 반영했다. A의 셸 PID·활성
  surface가 유지됐다. 활성 pane grid는 117×19에서 68×11로 바뀌었고 실제
  PowerShell console 크기도 일치했다. 기존 한글 출력이 검색됐다.
- 한글 완성형·분해형 자모·이모지·따옴표가 포함된 font family를 Ordinal
  비교했다. 다른 창에서 변경한 크기·테마·커서 값이 필드 병합 후 함께 남았다.
  light theme의 실제 xterm background/foreground도 확인했다. 테스트의 가상
  글꼴 이름은 문자열 보존 검사이며 그 글꼴의 설치·렌더링 증거가 아니다.
- 범위 초과·잘못된 형식·제어 문자·알 수 없는 테마·오래된 expected 값을
  거부했다. 파일의 rename을 막은 상황에서도 저장이 실패하고 파일 hash와
  실행 옵션이 유지됐다.
- revision을 바꾸지 않고 파일을 외부에서 원자적으로 교체해 두 창의 글꼴
  크기가 18px로 갱신되는 것을 확인했다.
- 창 A의 재시작 후 세 terminal의 layout·한글 기록 및 공유 설정을 복원했다.
- scrollback을 100행으로 줄이고 600행을 출력했다. 밀려난 과거 한글 marker는
  검색되지 않았고 새 tail marker는 검색됐다.
- 잘못된 파일은 실행 중인 창의 18px를 유지했고 새 창은 기본값과 오류를
  보고했다. 일반 변경은 파일을 덮어쓰지 않았고 명시적 reset 후 세 창이
  같은 새 revision·기본값으로 수렴하며 오류가 해제됐다.

초기 시행 기록은 `native-settings-initial-attempts.json`에 보존했다. 첫 시행은
정상 상태의 최상위 `error:null`을 기존 CLI가 오류로 해석한 실제 구현 결함을
발견했다. 상태 진단 필드를 `config_error`로 분리한 후 동일 조회가 통과했다.
두 번째는 PowerShell 5가 포함된 큰따옴표를 native argument로 전달하면서
문자열을 나눈 harness 문제였다. CSS에서 허용하는 작은따옴표 family로 같은
Unicode 보존 검사를 수행했다. 세 번째는 이미 기본값이던 새 창에서 reset의
revision 갱신 전에 검사가 끝나는 harness 대기 오류였다. 최종 검사는 세 창
모두 reset 응답의 새 revision과 실제 적용을 기다린다.

Windows native Rust 테스트 41개, 독립 Windows crate의 Linux 테스트 17개,
프런트엔드 테스트 20개가 통과했다. 새 native 검사는 실제 파일 lock·경쟁 값·
저장 거부·손상 파일 보존·reset을 확인한다. 프런트엔드 검사는 조합/복원 중
변경 보류·병합 및 focus/input 호출 부재를 검사한다. mock 기반 조합 검사는
실제 Microsoft IME 동작을 증명하지 않는다.

같은 debug 실행 파일로 기존 상태 저장·복원 8개군, workspace 6개군,
pane 7개군, 전체 출력 검색 9개군을 다시 실행해 통과했다. 해당 JSON은
`native-*-settings-background.json`이며 새 설정 검사의 파일명은 위와 같다.
Release build·Clippy도 통과했다. `artifacts-settings.json`에는 실행 파일과
개발용 NSIS 설치 파일의 크기·SHA-256을 기록한다. 설치 파일은 컴파일만 하며
이번 단계에서 실행하지 않는다.

모든 실행 검사는 사용자 요청에 따라 창을 숨기고 OS 키보드·마우스 입력 없이
진행했다. 실제 메뉴·설정 편집창·한글 조합 중 변경·한자 후보·DPI·접근성은
미검증이다. 실제 글꼴 존재·fallback 및 셀 폭/글리프 검사, font picker,
사용자 테마 가져오기, native UI/find bar 색상, per-tab 설정, zoom 단축키
재설정, 고부하에서의 설정 동기화와 전체 config/log/cache 호환도 남아 있다.
현재 크기 변경은 사용자 전체 창에 공유되며 독립적인 pane 확대 설정은 아니다.
