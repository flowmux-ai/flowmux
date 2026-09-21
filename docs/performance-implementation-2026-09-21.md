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
| synchronized output | 대기 |
| 최종 통합 A/B·회귀·설치 | 대기 |

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

다음 항목들은 구현 후 기준 바이너리와 동일 조건으로 비교하고, 효과 부재나 회귀가 확인된 후보는 수정하거나 되돌린 뒤 결과를 기록한다. 이 문서는 진행 기록이며 전체 작업 완료를 뜻하지 않는다.

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
