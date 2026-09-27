<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# 한글 입력: 개선 가능성·현 구조의 한계·미지원 검토

기준: `91f9a0922736619fca9d514418f77f1d69865981`, 설치된 GTK 4.14.5 / VTE 0.76.0. [이전 사전 검토](korean-input-audit-2026-09-27.md)의 54개 항목과 192회 비교 결과를 바탕으로 공개 API 및 VTE 소스를 대조하고 두 가지 추가 실험을 했다. 이번 작업은 개선안 검토이며 제품 코드는 변경하지 않았다.

## 1. 판단 요약

**한글 기능 자체가 원천적으로 구현 불가능하다고 확인된 항목은 없다.** 다만 현재 구조와 공개 API를 그대로 유지하면 할 수 없는 일, 현재 flowmux에 없는 기능, 아직 확인하지 않은 기능은 각각 다르다.

| 분류 | 해당 항목 | 판단 |
|---|---|---|
| 앱 코드에서 개선 가능 | 빠른 Enter 누락, Shift+←/→ 입력 순서, modifier 판별, 환경 설정 진단 | 우선 수정 대상. 이벤트 대기·키 분기·진단 개선으로 접근 가능 |
| 조건부 완화 가능 | 동기 IBus의 공백·숫자·쉼표 역전 | 측정 환경에서는 비동기 경로가 정상. OS 전체의 기본값을 일괄 변경할 근거는 부족 |
| 입력 계층 개선 필요 | 자모 Backspace와 확정 후 삭제의 동시 보장, 가짜 focus report 제거, 느린 IME에서도 정확한 확정 순서 | VTE/GTK/IBus 경로 보완, 필요한 API 노출 또는 앱이 관리하는 입력 계층 필요 |
| 현재 구현에 없음 | Windows 네이티브 terminal backend, VTE terminal의 Shift+←/→ 범위 선택 | 추가 개발 대상. 영구적으로 지원할 수 없다는 의미가 아님 |
| 미검증 | 한자 후보창, 실제 한/영 키, fcitx/세벌식, DPI, 조합 중 붙여넣기·탭 이동, Windows IME | 지원 불가로 분류하지 않음. 지원 범위를 정하고 실기 판정 필요 |

가장 먼저 해결할 것은 **빠른 Enter 누락과 Shift+방향키 순서**다. Windows 개발에서는 GTK 우회 코드를 그대로 옮기기보다 조합 상태와 확정 입력의 소유권을 분명히 하는 편이 타당하다.

## 2. 이번에 추가로 확인한 두 가지

### F-01. focus reporting이 꺼지면 빠른 Enter가 합쳐짐

기존 실기 수집기는 `CSI ?1004h`로 child focus reporting을 켰다. 동일한 스크립트에서 이 부분만 `CSI ?1004l`로 바꾸고 `sync=0, nav=on`을 두 번 실행했다.

| 조건 | 동작 | 기대 PTY 바이트 | 관측 |
|---|---|---|---|
| focus report on — 기존 측정 | 1ms 간격 Enter 3회 | `0d 0d 0d` | `0d 0d 0d` |
| focus report off — 이번 측정 1 | 동일 | `0d 0d 0d` | **`0d`** |
| focus report off — 이번 측정 2 | 동일 | `0d 0d 0d` | **`0d`** |

각 실행은 48개 중 45개 일치, 3개 불일치였다. 불일치는 빠른 Enter와 기존 Shift+←/→ 두 사례다. **이 결과는 이전의 46/48을 덮어쓰는 것이 아니라, focus report 조건을 추가했을 때 드러나는 차이다.** 1ms 간격은 합성 입력 스트레스 조건이며 일반 사용 중 발생 빈도는 측정하지 않았다.

소스의 `install_enter_preedit_commit_ordering`은 pending Enter를 `Rc<Cell<bool>>` 하나로 표시한다. 조합도 없고 focus report도 없으면 20ms fallback 전에 들어온 여러 Enter가 모두 같은 `true` 상태가 된다. 첫 fallback이 이를 false로 바꾸고 CR을 보내면 다음 fallback들은 보낼 것이 없다고 판단한다. 측정과 일치하는 코드 경로다.

**개선 방향:** pending 입력을 bool 하나로 표현하지 말고 요청별 ID 또는 FIFO로 보관한다. 타이머도 자신이 담당하는 요청만 완료해야 한다. 빈 Enter 개수 보존을 먼저 고치고, 조합·붙여넣기·다음 키와 겹칠 때 전체 순서도 검증한다. 카운터 하나로 바꾸고 대기 입력을 한꺼번에 내보내는 것만으로 모든 순서 문제가 해결되지는 않는다.

### F-02. VTE `commit`은 IME 확정 전용 신호가 아님

별도 VTE 위젯에 `commit` callback을 연결한 뒤, 입력기를 `simple`로 설정하고 **한글을 조합하지 않은 상태**에서 다음을 확인했다.

```text
VTE=0.76.0
key_controller=vte-key-controller im_context=NULL
commit phase=feed_child_ASCII hex=78
commit phase=feed_child_arrow hex=1b5b44
commit phase=enable_focus_reporting hex=1b5b49
commit phase=focus_cycle hex=1b5b4f
commit phase=focus_cycle hex=1b5b49
```

일반 `x`, 방향키, focus report도 모두 신호를 발생시킨다. 공개 문서도 child로 입력을 보내기 전의 신호로 설명하며, IME의 `GtkIMContext::commit`과 구분된다. [VTE commit 문서](https://gnome.pages.gitlab.gnome.org/vte/gtk4/signal.Terminal.commit.html)

따라서 현재 helper에서 이 신호를 받았다는 사실만으로 ‘마지막 한글이 확정됐다’고 판단할 수 없다. 기존 Enter 사례들이 통과한 관측은 유효하지만, **helper 재사용만으로 느린 IME까지 순서가 보장된다는 주장은 성립하지 않는다.**

설치된 GIR/header에는 terminal의 IMContext getter나 preedit 상태 신호가 없다. GTK key controller에는 IMContext getter가 있지만 실제 VTE controller에서 값은 NULL이었다. VTE 0.76.0은 별도 내부 IMContext를 만들고 직접 key filtering을 호출한다. [VTE 0.76.0 widget 소스](https://raw.githubusercontent.com/GNOME/vte/0.76.0/src/widget.cc), [GTK controller API](https://docs.gtk.org/gtk4/method.EventControllerKey.set_im_context.html)

실험의 원시 데이터, probe 소스, 파생 스크립트 생성 조건은 [후속 검토 증거](korean-input-improvement-evidence-2026-09-27.json)에 보존했다. 사용자 WSLg 창을 조작하지 않고 별도 Xvfb에서 실행했다. F-02는 API 의미 검증이며 실제 한글 IME 입력 시험을 대신하지 않는다.

## 3. 개선 가능한 항목과 실행 조건

아래 난도는 수정·검증 범위에 대한 상대 평가다. 작업 일수나 완료 보장은 아니다.

| ID / 우선순위 | 항목 | 개선안 | 범위·난도 | 완료 조건 |
|---|---|---|---|---|
| A01 / P0 | 빠른 Enter 누락 | 요청별 pending queue, 타이머 소유권·중복 완료 방지 | 앱 / 중 | focus report on/off 각각 Enter 1·3·연속 입력 개수 보존. 조합·다음 문자와 섞어 확인 |
| A02 / P0 | Shift+←/→가 확정보다 먼저 도착 | 직접 `feed_child` 경로를 입력 순서 처리에 편입 | 앱 / 중 | `가` 확정 후 이동. 앞에 문자가 있는 실제 readline/TUI에서 삽입 위치까지 확인 |
| A03 / P1 | Enter/Shift+Enter의 확정 판정 | 일반 VTE commit과 IME 확정을 구분하고 지연·취소·surface 종료 정책 명시 | 앱 + 입력 계층 / 큼 | focus report·일반 문자·붙여넣기가 대기 완료로 오인되지 않음. 입력기 지연 주입 검사 |
| A04 / P1 | 동기 IBus 혼합 입력 역전 | 재현 환경은 async 경로 유지. 배포판별 기본 설정·호환 profile 검증 | 앱 설정 + GTK/IBus / 중 | `한글 `·`아1`·`안녕,` 및 일반 구두점, sync별 회귀표. WSLg·Flatpak·native Linux 분리 |
| A05 / P1 | Shift+기호가 Super/Meta 조합도 잡을 가능성 | 관련 modifier 전체를 비교해 정확한 키 조합만 처리 | 앱 / 작음 | Shift 단독 기호는 유지, Ctrl/Alt/Super/Meta 복합키는 원래 소유자에 전달 |
| A06 / P1 | 플랫폼만 보고 우회를 설치 | 실제 IM 모듈과 platform profile을 함께 판단. 수동 enable/disable 우선순위 문서화 | 앱 / 중 | IBus 정상 경로 보존, fcitx/simple에서 불필요한 확정·삭제 우회가 개입하지 않음 |
| A07 / P1 | 현재 IM 환경을 알기 어려움 | 실제 모듈·sync·우회·backend 버전과 적용 이유를 진단 출력에 포함 | 앱 / 작음 | 재현 자료에 유효 설정을 기록. 입력 내용 자체를 상시 수집하지 않아도 진단 가능 |
| A08 / P2 | 자모 삭제와 확정 후 삭제의 충돌 | 실제 preedit 중에는 IME가 Backspace를 처리하고, 비조합 상태에서는 필요한 전달 경로 사용 | 입력 계층 / 큼 | `한 → 하 → ㅎ → 빈 조합` 및 확정 문자열 반복 삭제가 모두 동작 |
| A09 / P2 | 내부 확정 때문에 child focus report 발생 | 내부 IME 처리와 실제 창/pane focus를 분리할 수 있는 경로 확보 | VTE 보완 또는 입력 계층 / 큼 | 조합 확정만으로 focus out/in을 보내지 않으면서 실제 포커스 전환은 보고 |
| A10 / P2 | 숨긴 커서에서 preedit redraw 의존 | preedit 상태 변경이 직접 redraw를 요청하도록 backend 개선 검토 | VTE / 중~큼 | `?25l`에서도 조합 표시. 기존 16ms 보완 제거는 별도 회귀 검증 후 판단 |
| A11 / P1–P2 | editor·검색창·탭 이동 중 조합 충돌 | 각 입력 영역의 composition 상태·단축키 소유권·surface별 확정/취소 정책 검증 | 앱 + 각 입력 widget / 중~큼 | 후보 Enter가 제출/저장으로 새지 않고 이전 surface 텍스트가 새 PTY로 가지 않음 |
| A12 / P2 | Windows 지원 | terminal backend 경계 분리 후 WebView 입력·PTY 연결·focus/좌표 구현 | 신규 backend / 큼 | Windows 실기에서 기존 정상 동작 및 새 후보창/DPI 기준 통과 |

A02의 단기 후보는 기존 `feed_after_preedit_commit` 경로를 활용하는 것이다. 그러나 F-02의 제약 때문에 **이것은 제한된 환경의 개선 후보**이며 A03을 대체하는 완전한 해결책은 아니다. 제품 반영 전 실제 시나리오 검증이 필요하다.

A04에서 숫자·Space를 모두 직접 PTY로 보내도록 바꾸는 방식은 권장하지 않는다. 입력기의 후보 선택이나 조합 정책을 침범할 수 있다. 현재 사용자의 async 설정에서는 해당 세 사례가 이미 정상이다.

## 4. 현재 조건을 유지하면 할 수 없는 부분

‘아래 조건 안에서는 불가능’과 ‘기능 자체가 불가능’을 구분한다.

| 유지하려는 조건 | 불가능하거나 보장할 수 없는 것 | 조건을 바꾸는 대안 |
|---|---|---|
| VTE 0.76.0의 현재 공개 terminal API만 사용 | VTE 내부의 실제 preedit 문자열·커서·활성 상태를 직접 조회하고 해당 내부 IMContext를 제어 | VTE API/구현 보완, GTK/IBus 수정, 또는 앱이 관리하는 입력 계층 |
| 현재의 무조건 확정+DEL 우회 유지 | 동시에 IME의 `한 → 하` 자모 분해도 보존 | 조합 상태별 소유권 분리. 현재 동작과 자모 삭제는 같은 키 처리에서 양립하지 않음 |
| `VteTerminal::commit` 1회 또는 고정 20ms만 확정 근거로 사용 | 실제 IME 완료와 관계없는 입력·focus event 및 임의의 긴 지연까지 정확히 구분 | 조합 lifecycle을 관찰할 수 있는 입력 경로와 요청별 순서 관리 |
| `commit` signal 방출만 중지 | 이미 진행 중인 VTE child 전송을 취소 | 신호 이후에도 전송 버퍼에 추가하는 경로가 있으므로 전송 전 계층에서 제어 |
| focus byte를 전부 삭제하는 필터 사용 | 내부 focus만 제거하면서 실제 사용자 focus 보고도 보존 | 발생 원인·surface를 아는 위치에서 조건부 처리 |
| 재시작 전 미확정 조합 상태를 기록하지 않음 | PTY에 한 번도 전송되지 않은 음절을 저장된 터미널 출력만으로 정확히 복원 | 전환 전에 확정/취소하거나 입력 계층 상태를 별도 관리. 이력으로 조합을 추측하지 않음 |
| terminal 화면만 웹으로 바꾸고 현재 Unix PTY·플랫폼 코드를 유지 | Windows 네이티브 실행 지원 완료 | Windows process/PTY 및 host 통합 구현 |

VTE 0.76.0의 `send_child`는 `commit` 신호를 발생시킨 뒤 outgoing buffer에 데이터를 추가한다. 이 신호를 IME 완료 통지나 취소 가능한 입력 필터로 사용하는 것은 서로 다른 계약이다. [VTE 0.76.0 전송 소스](https://raw.githubusercontent.com/GNOME/vte/0.76.0/src/vte.cc)

GTK `IMContext.reset()`도 범용 ‘확정’ 함수로 취급할 수 없다. 문서는 통상 preedit 상태를 지운다고 설명한다. 또한 IME가 key filtering에서 처리했다고 반환한 키는 다시 처리하지 않는 것이 API 계약이다. [reset 문서](https://docs.gtk.org/gtk4/method.IMContext.reset.html), [filter_keypress 문서](https://docs.gtk.org/gtk4/method.IMContext.filter_keypress.html)

VTE를 계속 사용하면서도 외부 IMContext/입력 widget을 앱이 관리하는 방식은 **검토 가능한 대안**이다. 다만 중복 IME 처리 방지, 조합 그리기, 후보창 좌표, 주변 문자열, focus, 접근성을 함께 구현해야 한다. 단순히 IMContext 하나를 더 만드는 것으로 끝나는 변경이 아니다.

## 5. 현재 미지원과 미검증

| 항목 | 현재 판정 | 근거·해석 |
|---|---|---|
| Windows 네이티브 terminal backend | **미구현** | Unix `forkpty` 경로와 GTK VTE 사용. 조사한 코드에 ConPTY/WebView2 terminal 연결이 없음 |
| WSL 기본 우회 경로의 자모 Backspace | **그 경로에서 미지원** | 측정한 PTY 입력은 음절+DEL. 조합 자모를 지우는 처리 아님 |
| terminal Shift+←/→ 범위 선택 | **현행 미지원** | 앱이 일반 방향키 이동으로 변환. editor의 Shift 선택과는 별개 |
| VTE 내부 preedit 조회/직접 제어 | **현재 공개 API에서 미노출** | 설치된 GIR/header와 controller probe 확인. GTK 전체에 IME API가 없다는 뜻은 아님 |
| 한자 후보창·한/영 실키 | **미검증** | 상위 OS/입력기 기능이 존재해도 앱 단축키와 함께 동작하는지 확인 필요 |
| fcitx/fcitx5·세벌식 | **미검증** | 기존 설정을 보존하는 코드만으로 호환성을 증명하지 못함 |
| macOS 한글 혼합 입력 | **보완 구현 있음 / 이번 실기 없음** | `macos_ime.rs`가 GTK private 경로를 보완. ABI 변경 때 무효화될 수 있음 |
| DPI·다중 모니터·후보창 좌표 | **미검증** | 창/렌더러/입력기의 좌표 전달 검증 필요 |
| 조합 중 붙여넣기·탭 이동·popup | **미검증** | 일반 clipboard/lifecycle 테스트는 조합 중 순서 보장이 아님 |
| 모든 셸/TUI의 동일한 삭제·선택 결과 | **일괄 보장 대상 아님** | flowmux의 입력 순서·바이트 보존과 child의 편집 동작을 나눠 지원 조합별로 평가 |

Windows 웹 터미널은 구현 가능한 방향이다. xterm.js upstream에는 composition 처리가 있지만 도입할 고정 버전과 Microsoft IME 조합의 실기 합격을 대신하지 않는다. WebView host는 focus·크기·입력 연결을 담당하고, Windows 셸 연결에는 별도 pseudoconsole/process 구성이 필요하다. [xterm composition 소스](https://raw.githubusercontent.com/xtermjs/xterm.js/master/src/browser/input/CompositionHelper.ts), [WebView2 host API](https://learn.microsoft.com/en-us/microsoft-edge/webview2/concepts/overview-features-apis), [pseudoconsole 구성](https://learn.microsoft.com/en-us/windows/console/creating-a-pseudoconsole-session)

## 6. 기존 54개 항목의 개선 책임 연결

| 항목 | 개선 책임과 한계 | 필요한 판정 |
|---|---|---|
| H01, H02, H08, H14, H15, H54 | 입력기 선택·전환·후보·재연결. 앱은 연결/키 소유권을 개선할 수 있고 엔진 동작은 OS/입력기와 공동 책임 | 실키·후보창·입력기 재시작. 미검증을 미지원으로 바꾸지 않음 |
| H03, H04, H05, H06, H07, H09, H10, H11, H12, H13, H16, H17, H18, H19 | 조합 표시·혼합 문자·제출. 앱 순서 관리 개선과 입력 계층 보완 대상 | F-01/F-02 및 sync 차이 포함. 주요 정상 사례 보존 |
| H20, H21, H22, H23, H24, H25, H26, H27 | 삭제·이동·제어키. 자모 삭제에는 실제 조합 상태가 필요하고 키 전달 이후 편집 의미는 child가 결정 | raw 바이트와 실제 line editor 결과를 모두 확인 |
| H28, H29, H30, H31, H32, H33, H34 | 숨긴 커서·TUI mode·부하·스크롤·폰트·Unicode·DPI. 렌더러와 host의 개선 대상 | Windows/WSLg 실기, 단순 font 교체나 문자열 정규화로 일괄 해결하지 않음 |
| H35, H36, H37 | 선택·복사·paste. 앱 구현 및 composition 순서 정책으로 개선 가능 | 원문 보존, bracketed paste, 조합 중 붙여넣기 |
| H38, H39, H40, H41 | pane/tab/window 전환·resize·복원. surface별 입력 소유권 설계 가능 | 미확정 음절의 목적지·수명·확정/취소 정책 |
| H42, H43, H44, H45, H46, H47, H48 | 검색·이름·editor·browser. 각 widget의 IME와 전역 accelerator 통합 대상 | terminal 수정만으로 통과 처리하지 않고 각각 실기 |
| H49, H50, H51, H52, H53 | SSH·UTF-8 전송·locale/경로·자동화·no-echo. 앱 전송과 child 환경의 책임 분리 | 원시 문자열 주입과 실제 IME를 구분, 지원할 셸/remote 조합 명시 |

## 7. 권장 순서와 재검증 기준

1. **A01/A02:** Enter 개수와 Shift+방향키 순서를 먼저 수정한다. `?1004h/l`, 일반/alternate screen, 조합 유무, 빠른 입력을 조합한 회귀 검사와 실제 셸 편집을 통과해야 한다.
2. **A03/A05/A06/A07:** 입력 대기 로직의 한계를 명시하고 modifier·우회 적용 범위·진단을 정리한다. 가짜 focus report 제거와 안정적인 조합 완료 관찰은 별개의 요구로 관리한다.
3. **A04:** 현재 WSL async 동작을 유지하면서 WSLg portal·Flatpak·native Linux를 따로 검증한다. 숫자·공백을 무조건 우회시키거나 sync 값을 일괄 강제하는 변경은 피한다.
4. **A08–A10:** VTE/입력 계층 수정의 범위를 결정한다. 우회 코드를 전부 끄거나 fallback 시간을 늘리는 방식은 완료 조건을 충족하지 않는다.
5. **A12:** Windows 최소 terminal 구현에서 한글 조합·자모 삭제·후보창·Enter·focus·DPI부터 확인한다. 이 단계의 결과로 나머지 terminal 기능 이식 범위를 확정한다.

이번 후속 검토에서는 VTE API probe 1회와 focus report off의 실제 IBus 48개 사례를 2회 실행했다. 두 번 모두 45/48이며 같은 3개 불일치를 기록했다. 제품 수정·전체 workspace 재시험·Windows 실기는 수행하지 않았다. 개선 가능성은 위의 근거에 따른 설계 판단이며, 수정 완료 또는 전체 플랫폼 지원 완료를 뜻하지 않는다.
