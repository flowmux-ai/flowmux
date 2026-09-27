<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Windows viewport·최근 화면 조회

`windows/` 안에서 `capture-pane` alias와 `read-screen --recent`를 구현했다.
공유 Rust core, 기존 Linux/macOS 코드, root Cargo 파일은 수정하지 않았다.
114개 기능·13개 게이트의 전체 범위를 유지하며 T12/T13은 partial이다.

`capture-pane`은 `read-screen`과 같은 CLI 인수를 받고, raw IPC의
`capture_pane`도 `read_screen`으로 해석한다. tmux의 `-S`, `-E`, `-p` 등
capture flags 전체를 구현한 것은 아니다. 기존 pane/caller/surface 대상 선택과
텍스트 출력 형식은 유지한다. JSON에는 기존 surface/sequence/text에 더해 mode,
buffer, 크기, buffer 행 수, 조회 시작·행 수, viewport/base 행과 cursor 좌표를 넣는다.
좌표는 현재 buffer의 zero-based cell 기준이며 scrollback eviction/reflow 뒤에도
유효한 참조가 아니다.

기본 조회는 **현재 스크롤 위치의 viewport**다. `--recent`는 normal buffer의
최신 물리 80행을 읽으며, buffer가 짧으면 있는 행만 읽는다. 빈 마지막 행도 센다.
사용자가 보고 있는 스크롤 위치나 현재 커서 행을 범위로 사용하지 않는다.
Alternate buffer에서는 현재 화면 전체를 읽어 커서보다 아래의 상태 행도 포함한다.
기존 VTE의 최근 상태 화면 추출 계약을 읽고 Windows xterm의 buffer 좌표로 구현했다.
이 단계는 추출 API이며 agent 상태 판별기나 주기적 polling 연결은 아직 없다.

텍스트는 PTY transcript가 아니라 xterm parser가 처리한 실제 grid에서 얻는다.
요청 시 잡은 output sequence까지 처리한 뒤 읽고 완료 sequence를 반환한다.
더 최신 출력까지 처리됐을 수 있으므로 특정 과거 sequence의 화면을 재현한다는
뜻은 아니다. 여러 터미널의 원자적인 동시 snapshot도 아니다.
추출은 동기적으로 읽기만 하며 focus/selection/scroll을 바꾸지 않는다.
매 출력마다 전체 scrollback을 복사하지 않고 요청한 범위의 행만 방문한다.

Unicode 정규화나 codepoint 절단을 하지 않는다. 기존 조회와 같이 각 행 끝 공백을
제거하고, blank row와 soft wrap을 포함한 물리 행 사이에는 LF를 넣는다. ANSI 색상
제어 코드는 텍스트에 포함하지 않는다. 텍스트는 LF를 포함해 128 KiB UTF-8로
제한하며, 초과하면 partial text 없이 명시적인 CLI 오류를 보낸다. 한 행씩 크기를
검사하고 초과 행에서 중단하므로 전체 history 문자열을 만들지 않는다. 다만 한 행의
임시 문자열 할당량까지 이 상한으로 제한하는 것은 아니다. JSON escaping 최악값도
1 MiB bridge frame에 들어가는지 Rust 검사로 확인했다.

Native host는 대상 surface, 요청 하한과 발송 상한 사이의 완료 sequence, 기대한
조회 mode, buffer geometry/range, 텍스트 행 수와 바이트 수를 검사한다. 기존 12초
요청 만료와 탭 닫기 시 오류 처리를 유지하며 pending screen reads를 16개로 제한한다.

`native-screen-background.json`은 실제 숨김 WebView2/ConPTY의 10개 검사다.

1. `capture-pane`과 `read-screen`의 같은 결과, 완성형·분해형 한글·결합문자·이모지와 SGR plain text.
2. 같은 셀 덮어쓰기, CR, erase 뒤 이전 transcript가 아닌 바뀐 grid 조회.
3. 오래된 행으로 스크롤하고 선택한 상태에서 최근 80행 조회, 선택·스크롤 보존.
4. 한글·자모·이모지를 포함한 soft wrap의 물리 줄바꿈 보존.
5. normal 화면 커서를 위로 옮겨도 그 아래 최신 행 포함.
6. alternate 화면의 커서가 첫 행에 있어도 마지막 상태 행 포함.
7. alternate 종료 후 normal history와 화면의 분리 유지.
8. 비활성 탭의 계속된 parsing, workspace 이동 후 같은 PID와 최근 행 유지.
9. 없는 surface 오류 뒤 정상 surface 조회 가능.
10. 프로세스 종료 후 남은 마지막 화면 조회.

Probe는 소유한 private file로 출력만 제어한다. OS 키보드/마우스 입력,
사용자 clipboard 읽기/쓰기, foreground 변경이나 보이는 테스트 창이 없다.
부모 HWND가 숨김 상태인지 확인하며, `cleanup-screen.json`은 테스트 host/probe
PID와 해당 부모 PID를 가진 프로세스가 남아 있지 않은 후속 확인이다.
기존 사용자 WSL flowmux PID 787은 계속 실행 중인 것을 확인했다.

검증 기록:

- `frontend-tests-screen.txt`: 46개 통과. 요청 범위·Unicode·바이트 상한과 복구,
  parser callback 대기, split UTF-8 운반 및 기존 입력/선택/검색 검사를 포함한다.
- `linux-rust-tests-screen.txt`: 독립 Windows crate의 Linux 실행 28개 통과.
- `native-rust-tests-screen.txt`: 숨긴 Windows native 테스트 실행 55개 통과.
- `native-selection-screen-background.json`: 기존 실제 선택 검사 10개 통과.
- `native-output-search-screen-background.json`: 기존 실제 전체 출력 검색 9개군 통과.
- debug/release/Clippy/NSIS build 로그 및 `artifacts-screen.json`의 7개 artifact 해시.

설치 프로그램은 빌드만 했다. 실제 desktop IME·마우스·clipboard·DPI 검증은
수행하지 않았다. Native 초대형 grid의 상한 실패, 모든 timeout/close/reload 경쟁,
지속 출력 중 지연/메모리 예산, 광범위한 Unicode 폭/reflow/TUI 조합,
agent 상태 polling/classification과 기존 플랫폼 전체 live 회귀는 미완료다.
테스트 객체의 buffer/byte-limit 결과를 실제 Microsoft IME 검증으로 세지 않는다.
