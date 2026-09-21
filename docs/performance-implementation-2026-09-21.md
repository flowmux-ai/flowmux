<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

**터미널 성능 개선 구현·검증 기록**

기준 소스는 `9d5b903`. 수정 전 debug/fast GUI와 CLI를 각각
`/tmp/flowmux-implementation-20260921/baseline-debug`, `baseline-fast`에 보존했다.
사용자 창 PID 6619를 보호하며 별도 프로세스·소켓·D-Bus·XDG 상태에서 검증한다.
현재 실행 중인 사용자 창에는 바이너리를 주입하거나 재시작하지 않는다.

| 개선안 | 진행 상태 |
|---|---|
| alternate 화면 전환 시 geometry 고정 | 구현 및 실제 GUI 검증 완료 |
| 미니맵 작업량/그리기 비용 | 구현 및 실제 GUI A/B 완료 |
| 워크스페이스 pane/surface 복원 | 구현 및 실제 GUI 검증 완료 |
| 전체 스크롤백 저장 | 구현 및 실제 GUI 검증 완료 |
| PTY backpressure 중 입력/resize | 구현 및 실제 PTY 검증 완료 |
| IME focus cycle 및 redraw | 불필요한 UI 작업 제거·실제 IBus 검증 완료 |
| synchronized output | 후보 구현·실제 GUI 비교 후 회귀로 revert |
| 최종 통합 A/B·회귀·설치 | 비교·설치 완료; 후속 전체 workspace 1,696개 통과 |

**alternate 화면 geometry**

- 변경: `set_minimap`만 terminal margin을 설정하고, `set_alternate_screen`은 overlay 표시만 바꾼다. 불필요해진 minimap getter를 삭제했다.
- 기준: 실제 자식 PTY의 3회 alternate 왕복에서 `92×37 → 98×37 → 92×37` 반복. `/tmp/fm-gui-sx0b35xo/geometry.json`.
- 수정: 같은 조건에서 모든 샘플 `92×37`. `/tmp/fm-gui-kcfo_jcw/geometry.json`.
- 정상 동작: alternate에서는 미니맵 숨김/스크롤바 표시가 유지되며, 옵션 재적용도 geometry를 바꾸지 않는다. 기존 widget lifecycle 단위 테스트에 해당 조건을 추가해 통과했다.
- 검증: `scripts/test-terminal-rendering-gui.py`의 실제 GUI/PTY 검사, 관련 GTK 단위 테스트, `cargo fmt --all -- --check` 통과.
- 판단: 불필요한 terminal resize 제거를 확인했으므로 유지. 공간 예약은 normal/alternate 모두 동일하다.

**워크스페이스 복귀**

- 변경: 첫 pane의 마지막 탭을 강제로 활성화하던 코드를 제거했다. mapping 전에 기존 pane MRU를 읽고, 해당 workspace에 여전히 속한 pane을 복원한다. 유효한 MRU가 없을 때만 첫 pane으로 돌아간다.
- 기준: 두 탭과 두 pane을 만든 뒤 workspace 왕복 시 active tab 보존=false, 실제 키 입력의 마지막 pane 전달=false. `/tmp/fm-gui-4tbzo1vj/workspace.json`.
- 수정: 두 조건 모두 true. `/tmp/fm-gui-0kt9adom/workspace.json`. 화면 상태뿐 아니라 XTest로 입력한 문자가 마지막 pane에 도착하는지 확인했다.
- 회귀: 새 workspace 진입의 focus fallback 및 Agent Bar/activity의 명시적 pane/surface 목적지 테스트 통과. 포맷 검사 통과.
- 판단: 기존 active tab과 키보드 대상이 유지되므로 변경 유지.

각 후보를 구현 후 기준 바이너리와 비교했다. 검증된 여섯 변경은 유지했고, synchronized-output buffering 후보는 실제 회귀를 확인해 되돌렸다. 검증 한계와 남은 문제는 아래에 구분한다.

**전체 스크롤백 저장**

- 변경: viewport export 대신 기존 absolute-row 범위를 뒤에서 64행씩 읽고 256KiB 저장 한도에서 중단한다. 스타일·soft wrap·현재 스크롤 위치를 보존한다.
- 기준: 위로 스크롤한 뒤 저장하면 첫/마지막 행과 복원 화면의 마지막 행 모두 누락. `/tmp/fm-gui-zwg2leuz/scrollback.json`.
- 수정: 세 조건 모두 true. `/tmp/fm-gui-6cl85fbe/scrollback.json`. 테스트 전용 창을 실제 저장·종료·복원해 확인했다.
- 회귀: styled replay, 한글, 64행 경계를 넘는 soft wrap, 대량 출력의 저장 용량 제한을 포함한 관련 10개 테스트 통과.
- 판단: 화면 밖의 기록이 기존 용량 한도 안에서 복원되므로 유지.

**미니맵 CPU**

- 변경: HTML을 visitor로 읽어 중간 StyledRun 문자열 할당을 없앴다. 내용은 device scale/크기/foreground를 반영한 Cairo 이미지에 캐시하고 viewport highlight만 다시 그린다. wheel preview도 기존 100ms 스케줄러로 합친다. 표시하는 행 수와 색상 정보는 유지한다.
- 동일 fast 최적화 빌드, 30Hz 컬러 출력: Xvfb/Cairo CPU 29.8% → 21.0%, 실제 데스크톱 42.8% → 27.2%. 일반 출력은 각각 9.2% → 6.6%, 10.6% → 7.0%. OFF 컬러 기준은 6.0% → 6.0%, 7.2% → 7.6%.
- 결과: `/tmp/flowmux-perf-20260921/{baseline,updated}-fast-results.json`, `{baseline,updated}-native-fast-results.json`. 실제 GUI 스크린샷 `/tmp/fm-gui-4wy8l6j5/colored.png`에서 컬러 미니맵 표시 확인.
- 검증: minimap 13개 + scrollback/parser 10개 테스트 통과. 한글 cell 폭, 배경색, 화면 전환, resize/reflow, clear-scrollback, 숨김/다시 표시 범위 포함.
- 판단: 두 renderer 환경에서 CPU 감소 확인(컬러 출력 약 30%/36% 감소), 유지. VTE HTML extraction 자체 비용은 남는다.

**PTY backpressure**

- 변경: 실행 중인 pump의 blocking write를 제거하고 양방향 bounded queue와 POLLOUT를 기존 poll에 통합했다. 합성 cwd/title OSC도 같은 출력 queue를 거쳐 순서를 유지한다. 종료 시에는 읽지 않는 outer terminal을 기다리지 않는다.
- 기준: 출력 소비를 500ms 중단하면 입력 500.39ms, resize 500.34ms 지연. 수정: 출력 소비를 멈춘 동안에도 입력 0.065ms, resize 0.069ms 도착. `/tmp/flowmux-implementation-20260921/backpressure-{before,after}.json`.
- 검증: 실제 outer/inner PTY 재현, CLI integration 11개 및 관련 unit 8개 통과. 추가 integration은 출력 소비 전 입력/resize 수신, 1MiB 출력의 바이트 무손실, 자식 종료 코드 17 보존을 함께 검사한다. 기존 알림, OSC 제목/cwd, 시작 전 입력, EOF process-group 종료 검사도 통과.
- 판단: 같은 정체 조건에서 양방향 처리가 분리됨을 확인, 유지.

**IME 포커스·redraw**

- 변경: IME 확정의 동기 focus 왕복에서는 pane-focus callback을 억제했다. 실제 focus 이동은 그대로 보고한다. search entry 등 다른 widget이 focused인 키는 VTE redraw를 요청하지 않고, 16ms 후속 redraw는 한 개로 합친다.
- 검증: 6회 IME focus 왕복의 불필요한 pane callback 6 → 0; 실제 focus 이동 callback은 1회 유지. 관련 GTK/unit 41개 통과.
- 실제 IBus: 기준 `/tmp/fm-gui-s99tgi6o`, 수정 sync `/tmp/fm-gui-21zue41v`, async `/tmp/fm-gui-89vsb0oc`, 강제 legacy navigation `/tmp/fm-gui-cr18p34w`. 각 `result.json`과 화면 캡처 보존. `scripts/test-terminal-ime-gui.py`로 재현 가능.
- 기준/수정 모두 `abc\r\r\r\r가나\r하?가\x1b\r`로 Enter 연타·한글·Backspace·문장부호·ShiftEnter 순서 일치. legacy 분기는 기존 `한\x7f` 동작 유지.
- 제한: VTE가 IMContext를 공개하지 않아 조합 확정에 필요한 focus cycle 자체는 남는다. 따라서 focus-out 바이트(일반 7/legacy 8) 감소나 legacy Hangul 분해 문제 해결을 주장하지 않는다. 검증된 UI 중복 작업 제거만 유지한다.

**synchronized output 후보 — 되돌림**

- 후보 `c0ea359`: PTY에서 split된 DEC2026 begin/end를 인식해 최대 256KiB/250ms 동안 frame을 보류했다. 누락된 end와 큰 출력에서도 byte를 버리지 않고 한도 도달 시 flush한다. parser/lossless/bound 단위 검사 통과.
- 실제 GUI 기준 `/tmp/fm-gui-leyr_za1/sync.json`, 후보 `/tmp/fm-gui-d94__pni/sync.json`. `scripts/test-terminal-sync-gui.py`에 작은 frame, frame 중간 DSR cursor-position query, 330KB frame 재현과 pixel 캡처를 남겼다.
- 작은 frame은 begin→end 사이 OLD FRAME의 text/pixel 보존 false → true로 개선됐다. 그러나 DSR 응답은 9.29ms → 242.18ms로 지연됐고, 330KB frame은 buffer 한도를 넘으면서 여전히 중간 화면을 표시했다.
- 판단: 회귀 때문에 `03e901a`에서 후보 전체를 revert했다. 최종 제품에는 buffering이나 지원을 허위 광고하는 응답이 없다. query를 바로 처리하려면 미완성 frame을 VTE에 전달해야 하므로, proxy buffer만으로는 parser 진행과 화면 표시를 분리할 수 없다. VTE backend의 native synchronized-rendering 지원이 필요한 항목으로 남긴다. 시스템 VTE 변경·업그레이드는 하지 않았다.

**최종 적용 전 비교**

- 동일 fast 빌드 최종 native A/B: minimap ON 컬러 CPU 47.8% → 26.4%(약 45% 감소), OFF는 양쪽 8.2%. ON 일반 출력은 양쪽 10.6%로 이번 최종 표본에서는 차이가 없었다. 별도 첫 비교에서도 컬러 42.8% → 27.2%로 감소했다. 짧은 표본의 수치는 환경 영향을 받으므로 전체 성능 보장으로 일반화하지 않는다.
- 최종 fast 실제 GUI: geometry `/tmp/fm-gui-tsbgqzny`, workspace `/tmp/fm-gui-z8olnb25`, scrollback `/tmp/fm-gui-x46b4b4c`, IBus `/tmp/fm-gui-_ek7bcdi` 모두 기대 조건 통과. revert 뒤 DSR 응답 9.17ms 확인(`/tmp/fm-gui-d6ff9uzv`).
- 정적 검사: 전체 workspace/all-targets Clippy `-D warnings`, fmt 통과. GTK 변경 관련 64개, CLI/기타 workspace 682개, browser navigation integration 1개 통과.
- 최초 검증 당시 전체 GUI suite는 깨끗하지 않았다(아래 후속 재검증에서 실행 환경 원인을 확인하고 정정). 첫 실행 682/685 통과(에디터·사이드바·제목 3개 실패), 재실행은 다른 타이밍/selection 실패 뒤 GTK teardown SIGSEGV. 기준 소스도 minimap assertion 2개 실패 뒤 GTK teardown SIGSEGV였다. 문제 3개 격리 비교는 기준 3/3, 수정 2/3 통과였고 수정의 에디터 테스트는 다음 전체 실행에서 통과했다. 전체 통과나 모든 미확인 회귀의 부재를 주장하지 않는다.
- 측정값, 최종 GUI 결과, 비교 바이너리 SHA256은 [구현 검증 JSON](performance-implementation-evidence-2026-09-21.json)에 보존했다.

- 추가 실패 추적: viewport-shift와 hidden-tab search는 격리 검사 통과. chunk/alternate search는 다른 GTK 검사 직후 실패했지만 새 프로세스 단독 실행은 통과했다. 전체 GUI suite의 순서/수명 관련 불안정성은 이번 변경 범위 밖의 미해결 검증 제한이다.

**설치 및 기존 창 보존**

- `install.sh --check` 후 `install.sh --yes` 완료. `/home/junsu/.local/bin`과 `/home/junsu/.cargo/bin`의 GUI/CLI/viewer 총 6개 파일 SHA256이 전후 비교에서 사용한 fast 바이너리와 정확히 일치한다.
- 설치 경로로 별도 GUI를 실행한 geometry 검증도 13개 샘플 전부 92×37(`/tmp/fm-gui-4hk29z0f/geometry.json`).
- 기존 창 PID **6619**, 시작 시각 **2026-09-15 09:50:42** 유지 확인. 기존 창을 닫거나 재시작하지 않았다. GUI 변경은 새로 실행되는 창부터 적용된다.
- 최종 상태: 구현 변경 6개 유지, 동기화 출력 후보 1개 revert, 변경 관련 실제 시나리오 검증 및 설치 완료. VTE synchronized rendering, IME focus-report 바이트, legacy Hangul Backspace 분해는 남은 제한이다. 당시 전체 GTK suite 불안정성 판단은 아래 후속 재검증으로 정정한다.


**실패 테스트 후속 재검증 — 2026-09-21**

- 원인 정정: 직접 실행한 Cargo GUI 테스트가 `WAYLAND_DISPLAY=wayland-0`를 상속했다. `xvfb-run`만으로 GTK가 X11을 선택하지 않으며, 실제 display probe도 `GdkWaylandDisplay wayland-0`를 반환했다. `GDK_BACKEND=x11`을 명시하자 소스 수정 전에도 GUI 685/685개가 통과했다. 따라서 이전 결과를 제품 고유의 전체 suite 불안정성으로 해석한 판단은 잘못됐다. 기존 Python live-GUI harness는 이미 X11·D-Bus·XDG를 격리했으므로 앞선 실제 기능/성능 비교 기록은 이 문제의 영향을 받지 않는다.
- `51c48d4` — AI Usage 팝오버: 표시되지 않은 창에서 팝오버를 열던 테스트를 수정했다. GDB의 fatal-critical backtrace로 해당 경로를 확인했다. 추가 해제 검사에서 창 누수를 재현했고, accelerator controller의 창/버튼 참조와 action의 버튼 참조를 약한 참조로 바꿨다. 변경 전 창 해제 실패 → controller 수정 후 버튼 해제 실패 → 두 경로 수정 후 모두 해제. 관련 10개 테스트와 실제 단축키·focus 복귀 검증 통과.
- `e5a7c42` — PTY 회수: VTE의 child watch와 flowmux의 `waitpid`가 경쟁했다. 외부 watcher가 등록된 PTY에서는 VTE만 exit status를 회수하고 flowmux는 기존 SIGHUP/5초 뒤 SIGKILL 및 process-group 정리를 유지한다. 실제 PTY 검사로 종료 코드 23 보존, SIGHUP을 무시하는 자식의 SIGKILL 종료, 비동기 close를 확인했다.
- 테스트 환경/준비: 문서와 release workflow에 X11을 명시했다. 접근성 테스트는 `GTK_A11Y=none`에서 GTK critical을 내고도 통과하던 상태였으므로 `GTK_A11Y=test`와 context 존재 검사를 적용했다. 파일 메뉴 테스트는 실제 mapping/allocation을 기다린 뒤 창 중앙에서 메뉴를 연다. CI와 문서에는 `G_DEBUG=fatal-criticals`도 적용해 GTK 오류를 실패로 처리한다.
- 최종 전체 검사: `cargo test --workspace --locked -- --test-threads=1`, Xvfb/private D-Bus, `GDK_BACKEND=x11 GTK_A11Y=test G_DEBUG=fatal-criticals GSK_RENDERER=cairo`; `FLOWMUX_*` 상속 제거. **1,696 passed / 0 failed / 7 ignored**, GUI unit **685/685**, browser integration **1/1**. 제외 7개는 기존 수동 benchmark/stress 검사이며 새로 제외한 검사는 없다. 변경 전 X11 실행의 GTK critical 9건 및 이중 회수 경고 21건 → 최종 둘 다 0건. 시스템 portal/secret 서비스 경고는 별도이며 남아 있다.
- 정적 검사: `cargo clippy --workspace --all-targets --locked -- -D warnings`, `cargo fmt --all -- --check`, `git diff --check` 통과.
- 실제 새 fast GUI: `/tmp/fm-gui-hnz1sakn`에서 AI Usage 열기/닫기 화면 캡처와 XTest 키 입력의 터미널 복귀, 4회의 탭 종료/자연 종료 후 shell 회수, 정상 창 종료를 확인했다. 자연 종료한 로컬 탭은 현재 제품 설계대로 유지되므로 이후 명시적으로 닫았다. GTK critical/이중 회수 경고 없음. 스크립트·검사 로그는 `/tmp/flowmux-gui-review-20260921/`, 수치와 설치 SHA256은 검증 JSON의 `gui_followup`에 기록했다.
- 설치: `install.sh --check` 및 `install.sh --yes` 완료. 두 설치 경로의 GUI/CLI/viewer 6개 파일이 검증한 fast 빌드 SHA256과 일치한다. 기존 사용자 PID 6619(9월 15일 09:50:42 시작)는 그대로 살아 있으며 재시작하지 않았다. 변경은 새로 실행하는 창부터 적용된다.
- 남는 범위: VTE native synchronized rendering과 IME 조합 확정 focus 왕복/focus-report 바이트, legacy Hangul Backspace 분해는 이번 수정 대상 경로와 별개이며 해결됐다고 주장하지 않는다. 앞선 buffering 후보를 다시 도입하지 않았다.
