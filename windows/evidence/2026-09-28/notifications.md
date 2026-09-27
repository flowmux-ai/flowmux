<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Windows 알림 목록과 OSC 수신

독립 `windows/`에 네이티브 알림 목록, unread 배지, CLI 조작과 source 복귀를
추가했다. 기존 Linux/macOS 파일과 root Cargo/lock은 수정하지 않았다. 순수 Rust인
기존 `flowmux-notify`의 `osc.rs`/`stream.rs`와 `flowmux`의 `notifications.rs`를
읽기 전용 path module로 사용한다. GTK/D-Bus 구현은 링크하지 않는다. Windows
workspace가 직접 사용하는 chrono는 이미 lock에 존재하던 0.4.45다.

114개 기능·13개 게이트를 유지하며 T19/U14/A07은 partial이다. 현재 Windows
desktop toast, OS taskbar badge, toast activation, 알림 설정, activity/usage panel,
agent hook 설치·identity/lifecycle 통합은 구현하지 않았다. 알림 수신이나 출력량을
agent의 완료·대기·세션 종료로 추론하지 않는다.

## 구현 계약

- 창마다 메모리에 최대 50개를 오래된 순서로 보관한다. 새 항목은 가장 오래된
  항목을 밀어낸다. 앱 재시작 시 알림 목록은 복원하지 않는다.
- 기존 store와 동일하게 같은 `(pane, surface)`의 최근 8초 내 알림은 body가
  달라도 중복으로 본다. Info/Completed보다 Attention, Attention보다 Error처럼
  우선순위가 높아지면 수락한다. Global 항목은 중복 제거하지 않는다. Pane을
  옮기면 중복 키는 달라지지만 복귀 대상은 stable surface ID로 찾는다.
- 실제 foreground인 main window의 활성·표시 중 source에서 발생한 Info/Completed는
  억제하며 Attention/Error는 유지한다. Background test에서는 foreground로 판단하지
  않는다. Suppression 정책은 순수 Rust 테스트로 검사했고 실제 foreground 동작은
  이번 검증에서 조작하지 않았다.
- `notify`/`notify-complete`는 explicit pane/surface, 동일 창의 caller surface,
  active surface를 사용한다. Global 알림은 source가 없다. 제목/본문은 UTF-8
  1/8 KiB 한도이고 NUL·완전히 빈 내용은 거부한다. 명시적 CLI Unicode는 정규화하지
  않는다. Agent 명칭은 한 줄의 256 byte 이하 문자열이다.
- `notifications list [--unread]`는 읽음 상태를 바꾸지 않는다. `show`는 기존 항목을
  읽음 처리한다. 이후 도착한 알림은 새 unread로 유지한다. `open`은 source의 현재
  위치를 먼저 확인해 같은 탭으로 이동하고 해당 source 알림을 읽음 처리한다.
  `jump-to-unread`는 가장 오래된 unread에 같은 동작을 적용한다. Source가 닫혔으면
  오류를 내며 읽음·선택 상태를 바꾸지 않는다. 해당 항목은 read/delete로 처리할 수 있다.
- `mark-read`, `delete`, `clear`를 지원한다. Native 목록은 상세 Unicode 문자열,
  Read/Unread, level, workspace와 닫힌 source 상태를 표시한다. Notifications 버튼과
  workspace/tab caption에 unread 수를 반영한다. 알림 도착은 화면/탭을 활성화하지 않는다.
- Native panel 생성은 창을 표시하지 않는다. Background에서는 show/open도 창 표시,
  foreground 변경, WebView focus를 요청하지 않는다. 생산 빌드의 명시적 open은 main
  window를 복원하고 focus를 요청한다. 실제 Windows focus 정책/마우스/키보드/DPI/
  접근성 동작은 별도 검증이 남아 있다.

ConPTY에서 받은 원본 출력만 per-surface extractor에 넣는다. OSC 9/99/777의 기존
단순 형식을 지원하고 numeric OSC 9 subcommand(cwd/progress 등)는 알림에서 제외한다.
OSC parser는 기존 방식대로 control과 바깥 whitespace를 정리하며 Unicode를 NFC로
정규화하지 않는다. Split UTF-8/ESC sequence는 streaming buffer에 보존하고 invalid
UTF-8와 oversized payload는 버린다. 미완성 payload는 기존 extractor의 64 KiB 제한을
사용하고 한 output chunk에서 최대 16개 알림만 처리한다. Batch가 가득 찬 뒤
더 높은 우선순위가 도착하면 앞선 낮은 우선순위 항목을 하나 제거하고 끝에 추가한다.
따라서 낮은 우선순위 출력 폭주가 같은 chunk 뒤의 Error를 밀어내지 않는다. 설정/복원된 history는 이
경로로 재전송하지 않는다. 모든 Kitty multipart OSC 99 기능을 지원하는 것은 아니다.

`tree`의 `observed_output_bytes`와 `last_output_ms`는 이 host가 받은 원본 출력의
누적 byte 수와 UTC timestamp다. 화면의 문자 수나 완료 시각이 아니며, 프로그램의
identity/state를 바꾸는 증거로 사용하지 않는다. 기존 WebView 출력 sequence/ACK와
parser barrier는 유지한다.

## 숨김 실제 Windows 검증

`verify-notifications.ps1`은 `FLOWMUX_TEST_BACKGROUND=1`인 소유 host, 격리 config/state,
`--temporary`, 명시적 Named Pipe만 사용한다. Probe는 private file로 출력 동작을
제어하고 ST 종료 OSC만 보내므로 BEL 음향을 발생시키지 않는다. Keyboard/mouse/OS
clipboard를 사용하지 않는다. Native control text는 제한 시간 있는 WM_GETTEXT로
읽으며, UI 입력 메시지를 보내지 않는다. 검사 전후 창이 invisible이고 foreground가
아님을 확인한다.

`native-notifications-background.json`의 7개 검사군은 다음을 확인한다.

1. 실제 ConPTY를 지난 OSC 9/99/777의 완성형 한글·NFD 자모·이모지, severity escalation,
   numeric OSC 9 필터와 실제 output byte/time 증가. 1,000개 Info 뒤의 Error도 유지.
2. 명시적 Unicode 제목/여러 줄 본문, source 중복 제거, 우선순위 상승, read-only list,
   실제 Notifications/workspace/tab native caption.
3. 숨긴 native panel의 실제 list row count와 EDIT 상세 문자열, show의 기존 항목 읽음
   처리, 이후 항목 도착 시 unread와 control 갱신.
4. 비활성 source의 완료 알림이 현재 탭을 바꾸지 않으며, workspace/pane 이동 뒤
   같은 surface/PID로 복귀하는지 확인.
5. 자연 종료한 terminal의 마지막 화면 복귀, 닫힌 source는 read/focus 변경 없이 오류.
6. Delete/clear, 55개 global 알림 중 최근 50개 보관과 순서, unread jump, oversized
   제목 거부와 빈 목록 동작.
7. 별도로 숨긴 두 창이 명시적 pipe와 각자의 알림 저장소를 유지하는지 확인.

첫 실행의 control detail 검사는 다른 프로세스 EDIT에 `GetWindowText`를 사용해 실패했다.
검증기를 WM_GETTEXT/SendMessageTimeout으로 수정했다. 다음 실행은 cwd metadata probe가
존재하지 않는 `C:\한글`을 출력해 새 workspace 생성이 거부됐다. Probe 소유 directory에
실제 한글 하위 경로를 만들도록 수정했다. 원본 실패 JSON 두 개를 `*-control-*-failure.json`로
보존했다. 이 둘은 검증 코드 수정이며 제품에서 잘못된 cwd 검사를 완화하지 않았다.
Native Rust 결과 수집에서도 `Start-Process` 후 종료한 Process의 ExitCode가 null인
PowerShell wrapper 문제가 있었다. 테스트 stdout은 통과였지만 그 실행을 정상 종료
증거로 사용하지 않았다. `-Wait -PassThru`로 종료 코드를 확보해 재검증하고 원본
wrapper 결과를 보존했다. 최종 실행은 빌드 종료 후 실행한 별도 hidden host 기록이다.

최종 native notification 7개군, 기존 screen 10개, minimap 9개, settings 7개군이
통과했다. Linux에서 실행한 독립 Windows crate Rust 83개 검사도 통과했으며,
여기에는 재사용한 기존 pure notification 모듈 검사들이 포함된다. 실제 숨김 Windows
Rust 110개 검사와 release/Clippy/NSIS 빌드도 통과했다.
검사 결과와 실행 파일·설치 파일 7개의 SHA-256을 별도로 보존했다. Frontend는 이번
변경에서 수정하지 않았다. 설치 파일은 빌드만 하고 실행하지 않는다.

ST를 사용한 현재 app-local ConPTY/WebView2/Windows 조합의 증거이며, BEL 전달·다른
OS/ConPTY/shell 조합·persistent restore 경쟁 조건·고부하·모든 닫기/읽음 경합의 증거는
아니다. Synthetic 이벤트나 CLI 문자열을 실제 Microsoft IME 검사로 세지 않았다.
Linux/macOS 전체 live 회귀 및 모든 release gate의 완료를 주장하지 않는다.

`cleanup-notifications.json`은 기록한 소유 host/probe/test PID 35개와 그 PID를 부모로
가진 프로세스를 읽기 전용으로 조회해 남은 일치 항목이 없음을 확인한 기록이다.
사용자 WSL flowmux PID 787은 계속 실행 중임을 별도로 확인했다. 무관한 프로세스를
종료하거나 사용자 창·clipboard·입력을 조작하지 않았다.
