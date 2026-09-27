<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# flowmux 한글 입력 사전 검토 — 2026-09-27

현재 소스의 한글 입력 보완 코드, 자동 테스트, 기존 실기 기록을 검토하고 **실제 IBus에 키 이벤트를 보내는 격리 GUI 검증**을 추가했다. 목적은 Windows 웹 터미널로 옮길 때 보존할 동작과 현재부터 해결해야 할 제한을 구분하는 것이다. 제품 코드는 수정하지 않았다.

기준 소스는 `91f9a0922736619fca9d514418f77f1d69865981`이다. 현재 사용자 GUI의 실행 환경에서는 `GTK_IM_MODULE=ibus`, `IBUS_ENABLE_SYNC_MODE=0`, `FLOWMUX_ENABLE_IBUS_NAV_WORKAROUND=1`을 확인했다. 사용자 GUI를 교체하거나 입력을 보내지 않았으며, 검증에는 현재 소스로 새로 빌드한 별도 GUI를 사용했다.

## 1. 판단

**현재와 같은 비동기 IBus + 키 우회 설정에서는 주요 한글 입력 경로가 동작한다. 그러나 ‘한글 입력의 모든 동작이 안정적으로 검증됐다’고 판정할 수는 없다.**

- 조합 중인 글자 표시, Enter/Shift+Enter 순서, 한영 전환, 숫자·기호 혼합, 쌍자음·복합모음·겹받침은 이번 격리 환경에서 확인했다.
- **Shift+←/→는 조합 확정보다 이동 키가 먼저 전송된다.** 네 설정 모두 재현했다.
- **WSL/Flatpak 기본 우회 경로의 Backspace는 자모 분해가 아니다.** `한`을 확정한 뒤 DEL을 보낸다. ‘삭제 키가 먹히는 것’과 ‘한 → 하가 되는 것’을 구분해야 한다.
- **동기 IBus를 강제하면 공백·숫자·쉼표가 마지막 음절보다 앞서 전송됐다.** 현재 사용 중인 비동기 설정에서는 이 세 사례가 통과했다.
- 한자 후보창, 실제 한/영 키, Windows Microsoft IME, 고배율·다중 모니터, 조합 중 탭 이동과 붙여넣기는 아직 합격 근거가 없다.

Windows 이식의 기준은 정상 입력 결과와 사용자 경험이다. GTK focus 왕복, DEL 우회 같은 현행 해결 방법 자체를 이식할 필요는 없다. 특히 자모 삭제의 제한을 새 구현의 정상 사양으로 고정하지 않는다.

자료: [전체 측정 JSON](korean-input-audit-evidence-2026-09-27.json), [48개 사례의 설정별 결과](evidence/korean-input-2026-09-27/matrix.md), [재현 스크립트](../scripts/audit-terminal-korean-gui.py), [앞선 Windows 기능 검토](windows-web-terminal-feasibility-2026-09-27.md).

후속 [개선 가능성·미지원 검토](korean-input-improvement-review-2026-09-27.md)에서 focus reporting을 끈 경우 빠른 Enter 3회가 1회로 합쳐지는 현상을 추가 확인했다. VTE `commit` 신호가 IME 확정 전용이 아니라는 점도 실험으로 확인했다. 아래 수치는 기존 focus report on 조건의 기록이며, 후속 결과와 구분해 읽어야 한다.

## 2. 검증 범위와 결과

### 환경과 방법

| 항목 | 이번 검증 |
|---|---|
| 실행 호스트 | WSL Linux. 사용자 WSLg 화면과 분리 |
| GUI | 현재 소스 `cargo build --locked --profile fast -p flowmux -p flowmux-cli` |
| GTK / VTE | 4.14.5 / 0.76.0 |
| 입력기 | IBus 1.5.29-rc2, ibus-hangul 1.5.5, libhangul 0.1.0 계열 |
| 화면 | Xvfb/X11, cairo, 1600×1000. 별도 abstract socket |
| 입력 경로 | XTest 실제 키 이벤트 → GTK → private IBus → VTE → PTY → raw 입력 수집기 |
| 격리 | 별도 D-Bus·IBus socket·XDG 설정/상태. `GTK_USE_PORTAL=0` |
| 조합 조건 | 두벌식 기본 설정, 터미널 커서 숨김 `CSI ?25l`, focus reporting `CSI ?1004h` |
| 관찰 | 실제 PTY 바이트, 단계별 PNG, focus report, 정상 종료와 사용자 GUI 생존 확인 |

**WSL 호스트 위의 X11/direct IBus 검증이며 WSLg portal 경로의 검증은 아니다.** Windows 네이티브 IME·WebView2·ConPTY도 실행하지 않았다. 수집기는 입력을 에코하거나 편집하지 않는다. 따라서 바이트 순서 확인과 셸/TUI에서 실제로 글자가 지워지고 커서가 움직이는지의 확인은 별도다.

### 48개 사례 × 네 설정

`nav on`은 키 우회 활성화, `nav off`는 `FLOWMUX_NO_IBUS_NAV_WORKAROUND=1`로 명시적 비활성화다. WSL 자동 감지를 그대로 두고 설정 이름만 바꾸지 않았다.

| `IBUS_ENABLE_SYNC_MODE` | 키 우회 | 일치 | 불일치 | 불일치 내용 |
|---|---|---:|---:|---|
| `0` | on | 46 | 2 | 조합 중 Shift+←/→ 순서 |
| `0` | off | 39 | 9 | 위 2개 + Tab/←/→/Home/End/Delete/Escape 미전달 |
| `1` | on | 43 | 5 | Shift+←/→ + 공백/숫자/쉼표 순서 |
| `1` | off | 36 | 12 | 위 5개 + Tab/←/→/Home/End/Delete/Escape 순서 |

총 **192회 중 164회가 정의한 전달 조건과 일치하고 28회가 불일치**했다. `sync=1, nav=on` 48개를 추가 반복했으며 동일한 5개 불일치를 재현했다. 반복 실행은 192회 집계에 포함하지 않았다. 조건이 다른 사례의 통과율을 제품 안정성 백분율로 해석하면 안 된다. `nav off`는 기본 WSL 동작과 비교하기 위한 진단 조건이다.

현재 사용자 설정에 대응하는 첫 행의 합격 사례에는 다음이 포함된다.

- 영어 `abc`, 빈 Enter, 1ms 간격 Enter 3회.
- `가나`의 마지막 `나` 조합 표시, `안녕하세요` + Enter, `가` + Shift+Enter, keypad Enter.
- `한글 `, `아1`, `안녕,`, 한영 전환으로 입력한 `한abc글`.
- Shift 기호 21종: `? ! @ # $ % ^ & * ( ) _ + : " < > { } | ~`.
- `까따빠싸짜`, `과되워의`, `값`, 단독 자모 `ㄱ`.
- 우회가 켜진 상태의 조합 후 Tab/←/→/Home/End/Delete/Escape 전달.

Shift 기호의 첫 탐색 실행에서는 Xlib가 `<`의 물리 키를 잘못 선택했다. comma+Shift로 교정한 뒤 네 설정 전체를 다시 실행했다. 최초 실행은 최종 집계에서 제외했다. 각 단계의 기대값·실제값·원시 바이트는 JSON에 보존했다.

### 코드 테스트

| 확인 | 결과 | 증명 범위 |
|---|---|---|
| GUI crate의 관련 순수 테스트 | 중복 제외 14개 통과 | IM 모듈 선택, 우회 조건·키 매핑, Shift+Enter, selection cache 등 |
| `closing_terminal_releases_widget_graph` | 격리 GTK에서 1개 통과 | 내부 focus 왕복 6회는 pane focus callback을 늘리지 않고 실제 focus 전환은 전달 |
| `flowmux-terminal` | 23개 통과 | PTY·환경·입력 모드 등. 전체 23개가 한글 IME 테스트인 것은 아님 |
| editor frontend `npm test` | 타입 검사 및 34개 통과 | `isComposing` 때 편집기 focus navigation 차단 등을 포함. 실제 IME 입력은 아님 |
| 전체 workspace 테스트 | 이번 검토에서는 미실행 | 제품 변경 없이 관련 범위만 재검증 |

새 GUI 스크립트는 불일치를 발견하면 종료 코드 1을 반환한다. 이번 live 검증을 ‘전부 통과’로 표시하지 않는다. 테스트 도구는 `/tmp/flowmux-korean-audit-20260927`에 패키지를 풀어 사용했으며 시스템 입력기 설정은 변경하지 않았다.

## 3. 현재 구현의 책임과 조건

실제 터미널 렌더러는 **GTK VTE**다. `GhosttyPane`이라는 Rust 타입 이름이 Ghostty IME 구현을 의미하지 않는다.

| 구간 | 현재 처리 | 근거 |
|---|---|---|
| IM 모듈 선정 | 살아 있는 IBus socket이 있으면 미지정·wayland/simple/xim을 ibus로 교정. fcitx/fcitx5 등 명시된 다른 입력기는 유지 | [main.rs](../crates/flowmux/src/main.rs), `should_force_ibus_im_module` |
| 끊어진 IBus | Linux에서 IBus가 없고 미지정/ibus이면 simple로 전환하여 일반 문자 키가 사라지는 상황을 방지. **한글 입력기를 자동 설치·대체하는 기능은 아님** | 같은 파일, `should_force_simple_im_module` |
| 동기 설정 | IBus이면 비WSL에서 미지정 sync를 1로 설정. WSL에서는 강제하지 않음. 이미 지정된 값은 유지 | 같은 파일, 시작 시 IM 설정 |
| Flatpak | host IBus portal 통신 권한 제공. 실제 portal 검증은 별도 | [manifest](../packaging/flatpak/com.flowmux.App.yml) |
| 조합 그리기 | terminal의 부모 Overlay에서 키를 관찰하고 즉시 redraw + 최대 한 개의 16ms 후 redraw. 실제 terminal focus일 때만 실행 | [ghostty_pane.rs](../crates/flowmux/src/ui/ghostty_pane.rs), `install_terminal_key_capture` |
| Enter | IBus에서 Return/ISO_Enter/KP_Enter를 잡고 focus 왕복으로 확정 → commit signal 뒤 idle에서 CR. 20ms fallback | 같은 파일, `install_enter_preedit_commit_ordering` |
| Shift+Enter | terminal에만 적용. IBus/macOS에서는 확정 후 `ESC CR`; 기타 경로는 바로 전송 | 같은 파일, `feed_after_preedit_commit` |
| Shift+ASCII 기호 | IBus에서 조합을 focus 왕복으로 확정 후 해석된 기호를 UTF-8로 전송. Shift+영문 쌍자음은 통과 | 같은 파일, `install_ibus_shifted_symbol_passthrough` |
| 일반 편집·탐색 키 | WSL/Flatpak 또는 명시적 opt-in이면 조합 확정 후 키 바이트 주입 | 같은 파일, `install_ibus_nav_workaround` |
| Shift+←/→ | 일반 ←/→ 바이트로 바꿔 전송. **조합 확정 호출이 없다** | 같은 파일, `install_shift_arrow_cursor_move` |
| Ctrl+C | WSL의 plain Ctrl+C는 ETX 직접 전송. Ctrl+Shift+C 복사는 구분 | 같은 파일, `install_wsl_ctrl_c_interrupt_passthrough` |
| pane focus 추적 | 내부 IME focus 왕복 동안 callback만 억제. 실제 pane 전환 callback은 유지 | 같은 파일, `PREEDIT_FOCUS_CYCLE` |
| child 입력 | PTY proxy가 바이트 큐로 전달. DECCKM에서 일반 방향키 escape를 application cursor 형식으로 변경 | [pty_tee.rs](../crates/flowmux-cli/src/pty_tee.rs), [key_modes.rs](../crates/flowmux-terminal/src/key_modes.rs) |
| agent 환경 | `CLAUDE_CODE_NATIVE_CURSOR=1`, terminal identity 등 주입. 이것만으로 실제 Claude 버전의 IME 동작을 증명하지 않음 | [terminal lib.rs](../crates/flowmux-terminal/src/lib.rs) |
| macOS | 한 keyDown 안의 여러 `insertText:` 결과를 누적해 한글 뒤 숫자·기호에서 앞 음절이 덮어써지는 문제를 보완 | [macos_ime.rs](../crates/flowmux/src/ui/macos_ime.rs) |

키 우회 설치 조건은 `!disable && (WSL || Flatpak || enable)`다. `FLOWMUX_NO_IBUS_NAV_WORKAROUND`는 **변수의 존재**로 판단하므로 값이 `0`이어도 끈다. enable은 `1/true/yes/on`과 같은 참 값으로 판단한다. 이 설치 조건 자체에는 `GTK_IM_MODULE=ibus` 확인이 없다.

`FLOWMUX_ENABLE_VTE_CAPTURE_KEYS=1`의 smart PageUp/PageDown 처리는 기본 비활성이다. VTE에 광범위한 capture key controller를 설치하면 조합 표시를 방해했던 이력이 있기 때문이다. 이 opt-in을 Windows에서 기본값으로 가져갈 근거는 없다.

## 4. 발견한 제한과 불일치

### K-01. 조합 중 Shift+방향키의 순서 — 현재 재현

조합 중 `가` → Shift+← → Enter를 입력했다.

| 조건 | 원하는 전달 순서 | 실제 전달 순서 |
|---|---|---|
| Shift+← | `가 ESC[D CR` | `ESC[D 가 CR` |
| Shift+→ | `가 ESC[C CR` | `ESC[C 가 CR` |

네 설정 모두 동일했다. 소스에서도 `install_shift_arrow_cursor_move`는 확정 없이 `feed_child`한다. 앞서 확정된 문장이 있는 실제 line editor에서는 이동 후 잘못된 위치에 마지막 음절이 들어갈 가능성이 있다. **raw 입력 순서까지 재현했으며 실제 셸/TUI에서의 최종 편집 결과는 아직 확인하지 않았다.** 우선 수정·실기 검증할 대상으로 분류한다.

### K-02. 동기 IBus의 공백·숫자·쉼표 — 현재 재현

| 입력 의도 | `sync=0` | `sync=1` |
|---|---|---|
| `한글` + Space | `한글 ` | `한 글` |
| `아` + `1` | `아1` | `1아` |
| `안녕` + `,` | `안녕,` | `안,녕` |

두 nav 설정 모두에서 관측했고 sync=1/nav=on 반복에서도 동일했다. 이번 구현은 Shift+기호·Enter를 별도 보완하지만 Space·숫자·비Shift 쉼표는 일반 IME 경로로 남긴다. 해당 경로와 측정값이 일치한다.

이 결과를 모든 Linux/IBus 배포판의 결함으로 일반화하지 않는다. 현재 WSL 사용자 설정은 `sync=0`이므로 이 세 사례는 통과했다. 다만 비WSL 기본 sync=1 경로와 오래된 배포판은 같은 사례를 반드시 재검증해야 한다. 과거 검증의 `한 + Backspace + ?` 통과만으로 일반 기호 전체를 보장할 수 없다.

### K-03. Backspace: 삭제 복구와 자모 분해의 차이 — 설계상 제한

| 설정 | `한` 조합 후 Backspace | PTY에 도착하는 값 |
|---|---|---|
| nav off | `하`가 조합 상태로 남음 | 이 단계에는 없음. 다음 `?`에서 `하?` 확정 |
| nav on | 조합을 확정하고 DEL 전송 | `한`의 UTF-8 + `7f` |

nav on 수집기에는 편집 기능이 없으므로 화면에서 글자가 삭제되는 효과를 주장하지 않는다. 일반 셸은 DEL을 해석하지만 구체적인 삭제 단위는 셸/TUI의 책임이다. 현재 기본 경로는 IME의 `한 → 하 → ㅎ` 삭제 경험을 보장하지 않는다.

시각 근거: [nav off의 `하` 조합](evidence/korean-input-2026-09-27/backspace-nav-off.png), [nav on에서 조합 표시 종료](evidence/korean-input-2026-09-27/backspace-nav-on.png).

### K-04. 우회를 끄면 편집·탐색 키 전달이 달라짐 — 현재 재현

조합 후 Tab/←/→/Home/End/Delete를 누르면 nav off + sync=0에서는 `가`만 도착하고 키 바이트가 없었다. sync=1에서는 키가 먼저, `가`가 나중에 도착했다. Escape도 같은 차이를 보였다. nav on에서는 `가` → 해당 키 순서였다.

따라서 자모 Backspace를 되찾기 위해 현재 WSL에서 우회를 일괄 해제하는 것은 완료된 해결책이 아니다. Escape가 조합 취소용인지 TUI로 전달할 키인지도 제품별로 명확히 정해야 한다. 이번 기대값은 현행 우회 경로가 제공하려는 ‘확정 후 키 전달’이며, 모든 입력기의 후보 선택·취소 정책을 이 기대값으로 강제하지 않는다.

### K-05. 조합 확정이 child에 focus report를 발생시킴 — 현재 재확인

focus reporting을 켠 수집기에서 Enter·Shift+Enter·Shift+기호와 nav 우회 때 `ESC[O`/`ESC[I`가 관측됐다. 영어의 빈 Enter에도 발생한다. `PREEDIT_FOCUS_CYCLE`은 flowmux 내부 callback을 막지만 VTE의 child focus report까지 제거하지 않는다. 실제 TUI가 이 신호를 보고 다시 그릴 수 있으며 그 비용은 이번 검토에서 측정하지 않았다.

### K-06. 정적 검토상 추가 확인이 필요한 경계

- Enter/Shift+Enter의 20ms fallback은 느린 IME에서의 수학적 순서 보장이 아니다. 이 문서의 focus report on 조건에서는 빠른 Enter 3회가 통과했다. 후속 검토의 off 조건에서는 1회로 합쳐졌으며, 긴 지연·키 자동 반복·확정과 재입력의 겹침은 추가 검증이 필요하다.
- Shift+기호 판별은 Control/Alt/Shift만 마스킹한다. Super/Meta가 함께 눌린 경우의 정책은 주석의 ‘Shift만’과 대조할 필요가 있다. 실기 결함으로 확정하지 않았다.
- fcitx/fcitx5 설정은 보존하지만 IBus용 Enter 보완을 설치하지 않는다. Linux의 비IBus Shift+Enter도 같은 확정 대기 경로를 타지 않는다. 해당 입력기 실기는 없다.
- macOS 보완은 GTK private Objective-C class/method에 의존한다. 못 찾으면 경고 후 원래 동작을 유지한다. 현재 Linux 실행 결과로 macOS 성공을 판정할 수 없다.

## 5. 한글 동작 전체 점검 목록

‘현재 실기’는 이번 격리 환경에서만 확인했다는 의미다. ‘코드/테스트’는 구현 또는 간접 테스트 근거가 있고 실제 해당 IME 시나리오는 미검증이라는 의미다. 이 목록은 테스트를 통과한 기능 목록과 미검증 항목을 혼동하지 않기 위한 보존 계약이다.

| ID | 점검 대상 | 현재 근거·판정 | Windows 및 추가 검증 조건 |
|---|---|---|---|
| H01 | 입력기 탐지·초기 연결 | 코드/선정 함수 테스트 | 시작부터 한글 가능, 끊어진 입력기에서도 영문 입력 생존 |
| H02 | 한/영 전환 | Shift+Space 현재 실기 | 실제 한/영 키, 우측 Alt, Windows 전환키, 한영 모드 유지 |
| H03 | `ㅎ → 하 → 한` 단계 표시 | 구현 있음, `나`/`하` 화면 확인 | 각 단계 가시성, 확정 전 중복 PTY 입력 없음 |
| H04 | 음절 확정·다음 음절 이동 | `가나`, `안녕하세요` 현재 실기 | 빠른 받침 이동·문장 연속 입력 |
| H05 | 쌍자음 | `까따빠싸짜` 현재 실기 | Shift+자모가 단축키에 먹히지 않음 |
| H06 | 복합모음·겹받침 | `과되워의`, `값` 현재 실기 | 겹받침 재분해·모음 뒤 종성 이동 추가 |
| H07 | 단독 자음·모음 | `ㄱ` 현재 실기; 나머지 미검증 | 호환 자모·조합 자모·고어 조합 구분 |
| H08 | 두벌식 외 입력기 | 미검증 | 세벌식 및 다른 IME를 지원 범위에 넣으면 별도 실행 |
| H09 | 한글+영문 전환 | `한abc글` 현재 실기 | 확정·언어 전환 때 누락/중복 없음 |
| H10 | 한글+숫자 | 비동기 통과, 동기 순서 문제 | `아1`, 소수·음수·keypad 숫자 |
| H11 | 공백 | 비동기 통과, 동기 순서 문제 | 조합 확정 후 공백 1개 |
| H12 | Shift 기호 | 21종 현재 실기 | 한국어/영문 키 배열, modifiers별 소유권 |
| H13 | 일반 구두점 | 쉼표 설정별 차이 확인 | 마침표·슬래시·따옴표·백슬래시 등 추가 |
| H14 | 한자 변환·후보창 | 미검증 | 후보 위치, ↑↓, Enter 선택, Escape 취소, 후보 페이지 |
| H15 | 후보 Enter와 제출 Enter 구분 | 미검증 | 후보 선택이 셸 명령 제출로 새지 않음 |
| H16 | Enter 최종 음절 순서 | 현재 실기 통과 | `안녕하세요` 완성 후 CR 정확히 1회 |
| H17 | 연속 Enter·자동 반복 | 빠른 3회 현재 실기 | 장시간·수십 회·느린 입력기 추가 |
| H18 | Shift+Enter | 현재 실기 통과 | 최종 음절 후 `ESC CR`, editor/browser에는 강제하지 않음 |
| H19 | keypad/ISO Enter | keypad 실기, ISO 코드만 | 실제 키 배열별 동일 의미 |
| H20 | 조합 중 Backspace | nav off 분해 / nav on 확정+DEL | Windows에서는 IME 자모 삭제와 확정 글자 삭제 분리 |
| H21 | 확정 후 반복 Backspace | 우회 코드 있음; 이번 raw 편집 미검증 | 줄 경계·한영 혼합·이모지·선택 영역 삭제 |
| H22 | Delete | 바이트 경로 현재 실기 | 조합 취소/확정 정책과 앞쪽 문자 삭제 결과 |
| H23 | Tab·일반 방향키·Home/End | nav on 실기 통과; off 차이 | 후보 조작/완성 메뉴/셸 navigation 문맥 구분 |
| H24 | Shift+←/→ | 순서 문제 현재 재현 | 조합 확정 정책과 이동/선택 의미 재설계 |
| H25 | PgUp/PgDn·Insert·F1–F12 | 키 매핑 코드/테스트 | 조합 중·후 실키, keypad, TUI별 기능 |
| H26 | Ctrl+C·Ctrl/Alt 편집키 | Ctrl+C 분기 테스트 | 조합 중 중단·Ctrl+Backspace·Alt+Backspace 정책 |
| H27 | Escape | 설정별 전달 차이 | 후보 취소, 조합 취소, TUI 모드 탈출을 구분 |
| H28 | 숨긴 터미널 커서 | `?25l`에서 `나` 표시 현재 실기 | 실제 agent 자체 커서·커서 스타일별 동일 |
| H29 | alternate screen/DECCKM | mode 파서 테스트 | vim/tig/agent에서 한글과 이동키를 함께 실기 |
| H30 | 조합 중 대량 출력 | 미검증 | 스트리밍 중 입력 유실·지연·위치 흔들림 없음 |
| H31 | 조합 중 scrollback 이동 | 일부 코드만 | 스크롤·출력 도착·bottom 복귀 후 조합 위치 |
| H32 | 폰트 fallback·글자 폭 | VTE 위임, Noto CJK 표시 확인 | 한글 2셀 정렬, fallback·줄바꿈·선택 정확성 |
| H33 | NFC/NFD·자모·emoji | 부분 Unicode 간접 테스트 | UTF-8 바이트 보존, 강제 정규화 없음, 폭·삭제 비교 |
| H34 | DPI·줌·모니터 이동 | 미검증 | 100/125/150/200%, 창 이동, 후보창/caret 좌표 |
| H35 | 마우스 선택·복사 | VTE 및 bounded selection cache | 조합 제외/확정 텍스트 복사, repaint 중 한글 선택 보존 |
| H36 | 한글 붙여넣기 | VTE clipboard 위임 | 한영/emoji/다중 행, bracketed paste, 정확히 1회 |
| H37 | 조합 중 붙여넣기 | 미검증 | 미확정 음절과 paste의 순서·취소 정책 |
| H38 | pane/tab/workspace 전환 | 내부 focus callback 테스트만 | 조합이 원래 surface에 확정되고 새 surface로 새지 않음 |
| H39 | 창 포커스·popup·메뉴 | 일반 lifecycle 테스트 근거 | 조합 중 Alt+Tab, 메뉴 열기/닫기, 복귀 입력 |
| H40 | pane 크기 변경·탭 이동 | 미검증 | 조합·후보창 위치, PTY 대상, 표시 좌표 일치 |
| H41 | 숨은 tab·복원 | 일반 상태 코드만 | 조합 중 surface 숨김/종료/재시작 정책; 미확정 상태 저장 금지 여부 결정 |
| H42 | 검색 overlay | GTK SearchEntry 사용 | 조합 확정 후 검색, Shift+Enter 이전 결과, Escape 동작 |
| H43 | 전체 터미널 출력 검색 | Unicode 문자열·VTE buffer 경로 | 한글 검색·soft wrap·클릭 이동, byte/문자/cell 위치 구분 |
| H44 | command palette·이름 변경 | GTK entry 사용 | 조합 Enter가 불완전 이름 저장·잘못된 명령 실행으로 이어지지 않음 |
| H45 | editor 본문 | Monaco, 한글 문자열 저장 간접 테스트 | 실제 IME 조합·undo/redo·선택 삭제·저장 |
| H46 | editor 찾기·파일 열기·바꾸기 | Web UI/Monaco 위임 | 본문 외 input에서도 조합·Enter 처리 |
| H47 | editor 전역 단축키 | `isComposing` focus 이동 차단 테스트 | native host accelerator가 그 이전에 가로채지 않는지 실기 |
| H48 | browser 페이지·주소·찾기 | WebKit/GTK 각각 위임 | terminal용 Shift+Enter·키 우회가 침범하지 않음 |
| H49 | 로컬/SSH/tmux 입력 | SSH 테스트에 UTF-8 주입 사례 있음 | 실제 IME+remote locale+reconnect+tmux 편집 결과 |
| H50 | UTF-8 경계·PTY 전달 | 바이트 큐/PTY 코드·테스트 | 한글 3바이트를 chunk마다 분리해도 손상 없음 |
| H51 | shell locale·한글 경로 | macOS UTF-8 fallback 등 코드 | Linux locale, PowerShell/CMD 등 대상 셸별 파일명·cwd·저장 |
| H52 | `send-keys`/`read-screen` | 텍스트/버퍼 API | 확정 문자열 API 검증을 실제 키보드 IME 검증으로 세지 않음 |
| H53 | 암호·no-echo 입력 | 미검증 | child의 no-echo 정책과 IME preedit 표시 범위 확인 |
| H54 | IME 종료·재연결·설정 변경 | 시작 시 도달성 검사만 확인 | 실행 중 입력기 재시작과 모드 복구 |

관련 UI 근거: [키 바인딩 계약](keybindings.md), [keybindings.rs](../crates/flowmux/src/keybindings.rs), [editor focus navigation](../editor/flowmux-editor-web/src/focus_navigation.ts), [editor WebKit 테스트](../crates/flowmux/src/ui/editor_behavior_tests.rs), [file browser](../crates/flowmux/src/ui/file_browser.rs), [command palette](../crates/flowmux/src/ui/window/command_palette.rs), [전체 출력 검색](../crates/flowmux/src/ui/window/terminal_output_search.rs), [browser](../crates/flowmux/src/ui/browser_pane_webkit.rs).

편집기의 `InsertText("한글🙂 ")` 테스트와 SSH의 `send-keys` 한글 테스트는 **이미 확정된 문자열을 삽입**한다. 저장·인코딩·readline 삭제에 대한 근거이며 조합·후보창·한/영 키 지원의 증거로 사용하지 않았다.

## 6. Windows 웹 터미널의 합격 기준

Windows 구현은 [앞선 검토의 I01–I12](windows-web-terminal-feasibility-2026-09-27.md)의 초안을 이 목록과 결합해 평가한다. 현재 코드의 우회 처리에 의존해 합격 판정을 내리지 않는다.

1. **텍스트 전달:** IME가 확정한 문자열은 단일 경로로 정확히 한 번 전달한다. 조합 중 키마다 문자를 별도로 생성하지 않는다. 한글·emoji·분리된 UTF-8 chunk를 보존한다.
2. **확정 순서:** Enter, Shift+Enter, Space, 숫자, 모든 기호, 이동·삭제 키에 대해 마지막 음절과 동작의 순서를 비교한다. 고정 timeout만으로 해결됐다고 판정하지 않는다.
3. **삭제:** 조합 중에는 IME의 자모 삭제를 보존하고 확정 후에는 TUI의 삭제 의미를 따른다. 현행 WSL의 확정+DEL 타협은 개선 대상으로 둔다.
4. **조합 UI:** 커서가 숨겨져도 preedit와 후보창이 올바른 곳에 보인다. alternate screen·출력 폭주·resize·DPI 변경에서도 확인한다.
5. **키 소유권:** 후보 선택 Enter/Escape, 한/영·한자 키, editor 단축키와 app 전역 단축키를 구분한다. Rust host와 WebView가 같은 입력을 두 번 처리하지 않는다.
6. **surface 소유권:** 조합 중 pane/tab/window 이동 시 확정 또는 취소 정책을 명시한다. 텍스트가 새 PTY로 전달되거나 이전 surface의 조합이 되살아나지 않는다.
7. **사용 시나리오:** raw 수집기 외에 PowerShell, CMD, 지원 대상 agent/TUI와 선택적 SSH/tmux에서 최종 표시·편집 결과를 확인한다. 실제 한/영 키와 Microsoft IME 후보창을 사용한다.
8. **비터미널 입력:** editor 본문·찾기, command palette, 터미널 검색, 파일/탭/워크스페이스 이름 변경, browser 주소창을 각각 통과시킨다.

첫 구현 판정은 현재 정상 경로의 대표 사례 통과, K-01/K-02와 같은 순서 문제 부재, 자모 Backspace, 실제 한/영·한자 후보창, focus/DPI를 필수 조건으로 한다. 이번 Linux 결과만으로 Windows 한글 지원 완료를 선언할 수 없다.

## 7. 재현 및 남은 작업

입력 테스트 의존성이 있는 Linux에서 다음과 같이 실행한다. 각 실행은 자체 GUI·IBus를 띄우고 종료하며 `ARTIFACTS:` 경로에 결과를 남긴다. 현재 알려진 불일치 때문에 종료 코드 1이 예상된다.

```bash
cargo build --locked --profile fast -p flowmux -p flowmux-cli
python3 scripts/audit-terminal-korean-gui.py --sync 0 --nav on
python3 scripts/audit-terminal-korean-gui.py --sync 0 --nav off
python3 scripts/audit-terminal-korean-gui.py --sync 1 --nav on
python3 scripts/audit-terminal-korean-gui.py --sync 1 --nav off
```

필요 도구는 Xvfb, ibus-daemon/ibus-hangul, dbus-daemon, python3-xlib, Pillow다. `--gui`, `--cli`로 검증할 바이너리를 지정할 수 있다. 스크립트는 현재 Linux에서 사용하는 abstract Unix X11 socket에 의존한다. Windows 테스트 드라이버로 직접 사용할 수는 없다.

기존 [terminal IME 스크립트](../scripts/test-terminal-ime-gui.py)는 `AUDIT_NAV`가 없으면 일반 경로의 `하`를 기대하지만 WSL에서는 앱이 자동으로 우회를 켠다. 따라서 그대로 실행하면 실제 경로와 기대값이 어긋날 수 있다. 새 audit 스크립트는 on/off를 명시하고, 수신 chunk별 문자열 대신 원시 바이트를 합쳐 비교한다.

후속 순서는 **K-01의 조합/이동 순서 수정 및 실제 line editor 검증 → 현재 WSLg portal에서 동일 사례 대조 → Windows 최소 터미널에서 위 합격 기준 검증**이다. 동시에 비WSL sync=1의 K-02 재현 범위를 확인한다. 이번 산출물은 사전 검토와 재현 자료이며 제품 결함 수정이나 Windows 구현은 포함하지 않는다.

과거 근거는 [성능 조사](performance-investigation-2026-09-21.md), [구현 검증](performance-implementation-2026-09-21.md), [당시 입력 증거](performance-evidence-2026-09-21.json), [당시 최종 검증 증거](performance-implementation-evidence-2026-09-21.json)를 대조했다. 당시의 조합 표시·Enter·Backspace·Shift+Enter 확인을 유지하면서 이번에 일반 공백·숫자·기호·Shift 방향키 검증 범위를 확장했다.
