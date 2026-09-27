<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Windows named key와 cursor mode

독립 `windows/`의 `send-key`를 현재 terminal mode에 맞게 인코딩하도록 수정했다.
기존 Linux/macOS 파일, root Cargo/lock과 shared Rust crate는 수정하지 않았다.
114개 기능·13개 게이트를 유지하며 T21/O04는 partial이다. 물리 키 반복, 실제
focus reporting/IME, 고급 keyboard protocol과 전체 shell/TUI 호환을 완료로 보지 않는다.

## 기존 동작 재현

이전 `e2e6cd0` 실행 파일의 SHA-256을 `artifacts-keys-baseline.json`에 고정했다.
숨긴 소유 ConPTY probe가 `CSI ? 1 h`로 application cursor mode를 켜고 완료 marker를
출력한 뒤 parser-complete `read-screen`으로 확인했다. 그 다음 기존
`send-key Up --pane ...`를 호출했다. Native input trace와 ReadConsoleW 결과 모두
일반 모드의 `1B 5B 41`이었으며, 기대한 application mode의 `1B 4F 41`과 달랐다.
`native-keys-before-background.json`이 이 live 재현 기록이다.

첫 baseline 시도는 시작 직후 같은 PID의 오래된 발견 파일을 읽어 존재하지 않는
이전 pipe 이름으로 연결하려다 실패했다. 같은 PID에 새 nonce의 record가 뒤이어
게시된 것을 확인했다. 검증기가 launch 이후 LastWriteTime의 record만 받도록 고쳤고,
실제 CLI의 endpoint 검증이나 재시도 정책을 완화하지 않았다. 실패 JSON도 보존했다.
Paste/Key probe의 private 제어 파일은 Delete sharing과
MoveFileEx(REPLACE_EXISTING)를 사용해 읽는 중에도 원자적으로 교체한다.

## 입력 계약

Native host는 이름/대상을 검사하고 ConPTY reader의 현재 출력 sequence를 포착한다.
해당 출력까지 xterm parser callback이 완료된 뒤 WebView가 공개 API의
`applicationCursorKeysMode`를 응답한다. Rust encoder가 그 모드로 키를 변환하고
한 번만 session input queue에 넣는다. Normal의 arrows/Home/End는 CSI,
application 모드에서는 SS3이고, modifier가 있으면 xterm의 CSI modifier 형식이다.

기존 `ok: true` 응답을 유지하고 surface, parsed sequence, accepted byte count,
application cursor mode와 `delivery: queued`를 추가한다. 이는 queue 수락 응답이며
셸이 실행·소비했음을 보장하지 않는다. Native는 surface identity와 sequence의
하한/상한, ready/restore/exit 상태를 재검사한다. Pending named key는 창 전체 최대
16개, surface당 하나다. 같은 surface의 pending paste와도 중복되지 않는다. 12초
만료나 닫기 이후 응답은 입력하지 않고 버리며 CLI에 오류를 반환한다. 자동 재전송은 없다.
동시에 별도 `send-keys`를 호출하는 경우까지 원자적인 순서를 보장하는 계약은 아니다.

DOM keyboard event를 합성하거나 focus를 요청하지 않는다. 모드 조회 직전에
composition, xterm composition settling, history restore, disableStdin을 검사하고
사용 불가 상태면 오류를 응답한다. 실제 한글 text 생산과 commit은 기존 xterm/IME
경로가 계속 소유한다. 이 guard의 mocked 상태 검사는 실제 Windows IME 증거가 아니다.

`--surface`는 비활성·이동한 탭의 stable ID를 지정하며 그 탭의 모드를 사용한다.
`--pane`와 함께 지정하면 오류다. 지정하지 않으면 기존 caller/active 대상 규칙을
따른다. 종료된 process를 다시 시작하거나 닫힌 surface를 다른 탭으로 대체하지 않는다.

새 CLI는 `send_key_mode` method를 사용하고 새 host는 이전 `send_key`도 받는다.
이전 host의 serde enum에는 새 method가 없으므로 잘못된 대상에 입력하기 전에
거부한다. 기존 method에 새 surface 필드만 추가하면 이전 host가 그 필드를 무시해
active tab에 입력할 수 있어 method를 구분했다. Raw IPC에서 새 surface 계약을
사용하려면 `send_key_mode`를 보내야 한다. `named_key_protocol` capability로 조회한다.

## 지원과 경계

이름은 ASCII, 64 byte 이하며 대소문자를 구분하지 않는다. Enter/Return, Tab,
Escape/Esc, Backspace/BSpace, arrows, Home/End, Insert/Ins, Delete/Del,
PageUp/PgUp, PageDown/PgDn, F1–F12, Ctrl+A–Z, Alt+letters, Ctrl/Alt+Space,
일부 Ctrl 기호를 지원한다. ShiftEnter/ShiftTab 별칭을 유지하고 Shift+Enter는
기존 flowmux의 ESC CR 계약이다. Modifier는 pinned xterm의 encoding을 따른다.

Ctrl/Shift+Insert clipboard action, Shift+PageUp/Down viewport action,
Ctrl+Shift+letter, Meta/Win 키는 터미널 바이트를 대신 만들지 않고 거부한다.
Named key는 flowmux UI shortcut을 호출하지 않는다. 텍스트·한글은 send-keys/paste
경로를 사용한다. Numpad, key release/repeat 합성, Kitty/modifyOtherKeys 협상,
모든 IME/keyboard layout의 물리 키 대응을 지원한다고 주장하지 않는다.

## 검증

`named-key.cases.json`의 128개 frozen vector는 고정된 xterm 6.0.0의
`evaluateKeyboardEvent`와 기존 flowmux Input의 Shift+Enter 처리를 기준으로 한다.
Node 검사는 설치된 해당 소스와 vector를 다시 비교하고 Rust 검사는 같은 vector를
검사한다. Private xterm 소스는 test oracle에서만 읽으며 제품 bundle은 공개 mode API를
사용한다. 초기 검사에서 Ctrl+F가 function-key 분기에 들어가는 오류도 발견해 수정했다.
Node는 parser 완료까지 대기, 단일 응답, selection snapshot 정리와 composition/
settling/restore/disabled-input guard를 별도로 검사한다.

`verify-keys.ps1`은 숨긴 debug host, 명시적 소유 pipe와 격리 config/state를 사용한다.
자식 probe는 raw ENABLE_VIRTUAL_TERMINAL_INPUT으로 읽으며 정상 입력 byte를 UTF-8
파일에 보관한다. Native의 opt-in input trace와 매번 같은 기준 byte를 비교해 두 경계
모두 일치함을 확인한다. Ctrl+C/D/Z 등도 이 raw reader의 바이트 검사이며 각 실제
셸의 signal/job-control 동작을 대신하는 검사가 아니다. 종료는 private file을 통해
자식에게 요청하므로 Ctrl+Q도 평범한 시험 데이터다. OS keyboard/mouse/clipboard나
사용자 foreground 창은 조작하지 않는다.

최종 native 검사는 일반/application 모드 각각 128개 vector, legacy wire 호환,
비활성 source, 잘못된 키/clipboard 조합/대상 거부, 이동 뒤 동일 PID/mode 유지,
자연 종료 및 닫힌 탭 거부를 확인한다. 이전 문제의 동일 live 시나리오가 새 경로에서
application sequence로 바뀌는 것을 포함한다. 관련 paste/selection/screen 검증과
Rust/frontend/build 기록을 함께 보존한다.

지속 고부하에서의 round-trip 지연, 모든 expiry/close/restore 경쟁 조건, 실제 IME
조합 중 자동화 요청, 물리 key repeat/focus 전환, 모든 shell/TUI, 이전 GUI 실행 파일과
새 CLI의 설치 업데이트 과정, Linux/macOS 전체 live 회귀는 별도 미완료다.
설치 파일은 빌드만 하며 실행하지 않는다.

최종 검사 수는 frontend 56개, Linux에서 실행한 독립 Windows Rust crate 86개,
실제 숨김 Windows Rust 113개다. Native named key 7개군, paste 12개, selection
10개, screen 10개 검사가 통과했다. Release/Clippy/NSIS 빌드도 통과했으며
`artifacts-keys.json`에 실행 파일·설치 파일 7개의 SHA-256을 기록했다. Frontend
bundle을 재생성했고 기존 dependency license notice를 유지했다.

`cleanup-keys.json`은 소유 host/probe/test PID 14개와 그 PID를 부모로 가진 프로세스를
조회해 남은 일치 항목이 없음을 확인한 기록이다. 사용자 WSL flowmux PID 787은
계속 실행 중이며 무관한 프로세스를 종료하지 않았다.
