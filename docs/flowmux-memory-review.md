<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# 실행 중인 flowmux 창 메모리 검토

검토일: 2026-09-11 KST. 수정 전 소스 기준: `4ef1592` (`0.9.15`). 아래 소스 근거의 행 번호는 이 기준 커밋을 가리킨다.

## 결론

현재 창의 메모리가 계속 증가한다는 증거는 이번 짧은 관측에서 나오지 않았다. 그러나 수정 전 소스에서 **pane/tab을 닫을 때 GTK 위젯이 해제되지 않는 순환 참조**, **메뉴를 닫아도 남는 순환 참조**를 확인했다. 이번 변경은 이 소유권 문제를 수정한다.

이 문서는 조사 근거와 적용한 수정의 기록이다. 현재 Codex가 실행 중인 flowmux 창과 기존 pane/tab을 닫거나 재시작하지 않았다. 측정은 `/proc`과 읽기 전용 `tree` 요청을 사용했다. 소유권 실험과 실제 UI 생성/닫기 검증은 별도 Xvfb/D-Bus 세션에서 실행했다. 설치된 바이너리는 교체하지 않았으며 수정은 새 빌드를 다음에 실행할 때 적용된다.

## 적용한 수정

- pane body DnD는 controller의 `widget()`을 사용하고, tab DnD/context 메뉴와 registry 조회 callback의 역참조는 weak로 바꿨다. macOS 전용 DnD/query 경로에도 같은 소유권 수정을 적용했다.
- 실제 WindowController의 pane 닫기 테스트에서 추가로 확인한 terminal/browser/editor 포커스 controller → frame 순환 참조를 weak로 바꿨다.
- terminal/tab/file 메뉴와 overlay 메뉴의 부모 위젯 역참조를 weak로 바꿨다. 파일 행의 우클릭 controller가 자기 행을 붙잡던 참조도 제거했다.
- 일회성 popover는 닫힌 뒤 idle에서 unparent한다. GTK 4.14는 포커스 이동을 after-paint까지 미루므로, 그 전에 동일 위젯을 hide/unparent하면 pending focus 참조가 덮어써져 남을 수 있다. 닫기 신호에서 메뉴 내부 포커스를 먼저 해제하고, Move submenu는 부모 버튼으로 포커스를 돌린다.
- 검색 snapshot 총 바이트 예산은 변경하지 않았다. 현재 창의 과점유 원인이라는 증거가 없으며 검색 범위 정책을 별도로 정해야 한다.

## 수정 검증

회귀 테스트는 실제 flowmux Rust UI 생성 함수를 호출하고, 닫은 위젯과 callback의 weak reference가 해제되는지 검사한다. pane/tab/registry/overlay 순환 참조는 수정 전 테스트 실패로 확인했다.

- 실제 WindowController 창을 Xvfb에 표시하고 오른쪽 pane 닫기 명령을 dispatch: 닫힌 VTE/tab 해제 및 왼쪽 pane 유지 확인.
- terminal/file 메뉴는 직접 dismiss와 버튼 실행을 검증하고, tab 메뉴는 Move submenu 실행 및 이동 callback을 검증한다. overlay 메뉴는 버튼 실행/Escape/바깥 클릭 경로를 검증한다.
- DnD, pane 메뉴, callback routing, sidebar 상태 유지 테스트를 포함한 관련 테스트 **33개 통과** (`--test-threads=1`, 별도 Xvfb/D-Bus 및 XDG 디렉터리).
- `cargo clippy -p flowmux --bin flowmux --tests --locked -j 2 -- -D warnings`, `cargo fmt --all`, `git diff --check` 통과.
- Linux GTK 4.14 환경에서 검증했다. macOS 전용 경로의 실행 검증은 하지 않았다.

현재 실행 프로세스에 대한 heap profile이나 수정 빌드와의 RSS 비교는 수행하지 않았다. 이 수정이 현재 CPU 사용량을 얼마나 줄이는지는 검증하지 않았으며 CPU 문제가 해결되었다고 주장하지 않는다.

## 실행 상태와 측정 범위

- 대상 프로세스: `3193634`, 소켓: `/run/user/1000/flowmux-3193634.sock`.
- 현재 Codex pane: `fea7258a-0d19-4f57-af41-9c2076899443`.
- 현재 Codex tab: `179ae3e5-2c50-43e0-ba80-e06863312e67`.
- 약 20시간 실행 중. 읽기 전용 tree에는 workspace 5개, terminal tab 9개가 있다.
- 본체 RSS는 약 333MiB에서 시작했다. 공유 페이지를 비례 배분한 본체 PSS는 약 219MiB였다.
- 직접 자식인 WebKitNetworkProcess와 WebKitWebProcess의 RSS는 각각 약 68.6MiB, 53.1MiB였다. 살아 있는 browser tab이 tree에 없다는 사실만으로 이 프로세스들을 누수로 판정할 수는 없다.
- `/proc/3193634/exe`는 `/home/junsu/.local/bin/flowmux (deleted)`를 가리킨다. 실행 파일이 실행 후 교체되었으므로 현재 checkout과 실행 중인 코드가 일치한다고 가정하지 않았다. 아래 소스 문제를 현재 프로세스의 메모리 증가 원인으로 확정하지 않는다.
- 약 70GiB의 VSZ는 가상 주소 공간이다. 실제 메모리 문제 판단에는 RSS/PSS/anonymous pages를 사용했다. 본체 swap은 0이다.
- 디스크 설정의 scrollback은 5,000행, 복원 켜짐, minimap 켜짐이다. 실행 중인 객체에 설정을 다시 적용하지 않았다.

관측 원본: `/tmp/flowmux-memory-observation-3193634.json`. 15초 간격으로 본체와 위의 WebKit 자식 둘을 관측했다. 이 합계는 shell, pty-tee, Codex/MCP 프로세스를 포함하지 않는다. 대화 중 출력이 발생하는 상태의 관측이며 엄밀한 무출력 idle 실험은 아니다.

2026-09-11 15:22:01–15:25:01 KST, 3분간 13회 측정:

| 지표 | 시작 | 종료 | 관측 범위 |
|---|---:|---:|---:|
| 본체 RSS | 333.2MiB | 327.6MiB | 325.3–334.2MiB |
| 본체 PSS | 219.0MiB | 213.4MiB | 211.0–219.9MiB |
| 본체 anonymous | 159.7MiB | 154.0MiB | 151.7–160.6MiB |
| 본체+WebKit 두 자식 PSS 합 | 272.8MiB | 267.2MiB | — |
| 본체 열린 FD | 114 | 116 | 114–118 |

두 WebKit 자식의 RSS/anonymous/FD는 일정했다. 본체 CPU는 같은 구간 평균 약 40.3%로, 논리 코어 하나 기준이다. CPU 사용은 지속되지만 관측한 메모리는 단조 증가하지 않았으며, FD도 중간 최고값에서 내려왔다. 짧은 관측이므로 드문 동작에 따른 누수나 장기 누수 부재를 증명하지는 않는다.

## 1. P1 — pane/tab 이벤트 처리기의 강한 참조가 위젯 해제를 막는다

### 소스 근거

- `crates/flowmux/src/ui/workspace_view.rs:1675`: 각 pane의 `GtkStack`에 `attach_pane_body_dnd`를 설치한다.
- `crates/flowmux/src/ui/workspace_view.rs:3097`: 전달된 stack을 `frame`이라는 강한 참조로 복제한다.
- `crates/flowmux/src/ui/workspace_view.rs:3103`: motion 콜백이 이 참조를 소유한다. 아래 drop 콜백도 같은 stack을 소유한다.
- `crates/flowmux/src/ui/workspace_view.rs:1789`: tab의 우클릭 GestureClick이 자기 tab을 강하게 참조한다. 메뉴를 한 번도 열지 않아도 참조가 생성된다.
- `crates/flowmux/src/ui/workspace_view.rs:2494`: tab의 DragSource와 DropTarget 콜백도 tab을 강하게 참조한다.
- `crates/flowmux/src/ui/window/pane_callbacks.rs:225`, `:354`: `workspace_of_pane`, `position_of_surface_in_pane`은 registry를 강하게 캡처한다. registry가 tab을 보관하므로 별도의 역참조도 생긴다.

참조 경로:

```text
GtkStack → 부착된 DropController/DropTarget → closure → 같은 GtkStack
                                                    └─ stack의 VTE 자식 유지
tab → GestureClick/DragSource/DropTarget → closure → 같은 tab
PaneRegistry → tab → callback → Rc<PaneRegistry>
```

`forget_pane`(`workspace_view.rs:852`)은 registry 항목을 제거하고 PTY를 닫지만, 위 GTK 순환 참조를 끊지는 않는다. `close_pty`(`ghostty_pane.rs:1206`)도 GTK 위젯의 signal/controller를 해제하는 함수가 아니다. 닫힌 pane의 stack이 남으면 VTE와 그 버퍼가 함께 남을 수 있다. 닫기/재구성을 반복할수록 누적되는 구조이며 idle 중 계속 증가해야만 하는 누수는 아니다.

### 최소 수정 방향

1. pane body 콜백에서는 캡처한 `frame` 대신 인자로 받은 controller의 `widget()`을 조회한다. 그것이 어려운 곳은 `glib::WeakRef`로 보관하고 실행 시 `upgrade()`한다.
2. tab context/DnD의 `tab_for_*` 캡처를 약한 참조로 바꾼다. DragSource는 tab 내부 button에 부착될 수 있으므로 단순히 모든 곳을 `controller.widget()`으로 바꾸지 않는다.
3. registry를 조회하는 callback은 `Rc::downgrade`를 사용하고 registry가 없으면 `None`을 반환한다. 이미 `is_ssh_pane`에 이 패턴이 있다.
4. 정상적인 탭 이동/재부착 과정에서도 동작하도록, `unmap`을 곧바로 파괴로 취급하지 않는다.

### 회귀 검증

- 실제 `attach_pane_body_dnd`를 설치한 stack에 VTE를 넣고 외부 소유권을 모두 제거한 뒤 stack/VTE의 weak upgrade가 실패해야 한다.
- 실제 `build_surface_tab_widget`로 만든 tab을 부모와 registry에서 제거한 뒤 weak upgrade가 실패해야 한다. 메뉴 클릭이나 drag를 하지 않은 경우도 포함한다.
- 실제 PaneCallbackRouter를 사용한 registry도 마지막 소유자 제거 후 해제되어야 한다.
- 현재 `closing_terminal_releases_widget_graph`(`ghostty_pane.rs:3509`)는 `GhosttyPane`만 생성한다. workspace 쪽 DnD/tab wiring을 설치하지 않으므로 이 문제를 검출하지 못한다.

## 2. P2 — 닫힌 메뉴가 자기 자신과 주변 위젯을 계속 붙잡는다

### 소스 근거

- `crates/flowmux/src/ui/ghostty_pane.rs:930`: 메뉴 Button 콜백이 `popover.clone()`을 보관한다. Copy/Paste는 VTE도 강하게 캡처한다. `:1004`의 `unparent()`만으로 자식→popover 순환은 끊기지 않는다.
- `crates/flowmux/src/ui/workspace_view.rs:1816`: tab 메뉴와 Move submenu에도 같은 패턴이 있다. `:1905`도 unparent만 수행한다.
- `crates/flowmux/src/ui/overlay_menu.rs:53`: `dismiss`가 overlay와 scrim을 강하게 보관한다. scrim 안의 button/key/gesture 콜백이 dismiss를 다시 보관한다. `remove_overlay` 후에도 scrim 자체의 순환이 남고 바깥 overlay도 유지한다.
- `crates/flowmux/src/ui/file_browser.rs:2415`: 파일 메뉴 Button이 popover를 캡처한다. 이 경로는 `closed`에서 unparent하는 처리도 없어 숨겨진 메뉴가 부모에 계속 붙어 있을 수 있다.

```text
popover → button → clicked closure → popover
scrim → menu/button/controller → dismiss closure → scrim + outer overlay
```

메뉴를 열고 닫을 때마다 객체 그래프가 남을 수 있다. 특히 터미널 메뉴는 VTE를, overlay 메뉴는 창의 콘텐츠 overlay를 유지할 수 있다. `malloc_trim(0)`은 아직 참조 중인 객체를 해제할 수 없다.

### 최소 수정 방향

- button/gesture/key 콜백의 popover·scrim·overlay 역참조를 weak로 바꾼다. `dismiss`만 고치고 `scrim_for_click` 같은 다른 자기 참조를 남겨서는 안 된다.
- 터미널 메뉴의 VTE 캡처도 weak로 바꾸어 메뉴 수명에 터미널 수명이 종속되지 않게 한다.
- 파일 메뉴는 닫힐 때 unparent하고, Move submenu도 부모/자식 정리 경로를 명시한다.
- 메뉴 프레임워크를 새로 만들 필요는 없다. 기존 생성 함수의 소유권과 정리를 바로잡으면 된다.

### 회귀 검증

Copy/Paste 같은 항목 실행, Escape, 메뉴 밖 클릭 각각으로 dismiss한 뒤 메뉴와 scrim의 weak upgrade가 실패해야 한다. 실제 메뉴를 반복해서 생성·dismiss하는 테스트에서 생존 위젯 수가 늘지 않아야 한다. callback 동작과 포커스 복귀도 함께 확인한다.

## 과점유 위험: 전체 터미널 검색에는 총 바이트 예산이 없다

`terminal_output_search.rs:397` 이후는 512행씩 읽지만 결국 `text.push_str`로 해당 터미널의 검색 범위 전체를 하나의 String에 모은다. `:465` 부근의 SearchHit은 그 전체 OutputSnapshot을 Arc로 보관한다. 따라서 검색 결과 500개 제한은 500행 분량의 메모리 제한이 아니다. 일치 결과가 있는 여러 터미널의 전체 snapshot이 동시에 남을 수 있다.

이는 해제되지 않는 순환 참조와는 다르며, 현재 실행 창에서 해당 검색을 사용 중이라는 증거도 없다. 기본 5,000행에서는 우선순위가 낮지만 설정 최대치는 1,000,000행이다. 대규모 history를 지원하려면 별도 총 snapshot 바이트 예산을 두고, 초과 시 검색 범위 제한을 표시하거나 행 단위 처리로 전환해야 한다. Arc clone 자체를 없애는 것은 해결책이 아니다.

## 이미 있는 제한과 해제 처리

- `flowmux-core/src/lib.rs:19`: 저장할 scrollback은 tab당 256KiB 제한이며 `TerminalScrollback` 내용은 `Arc<str>`로 공유된다. state clone을 전부 깊은 복사로 오인하지 않는다.
- `terminal_minimap.rs:330`: unmap 시 예약 refresh를 제거하고 pixel cache를 비운다. refresh 자체도 중복 예약을 제한한다.
- `browser_pane_webkit.rs:36`: NetworkSession 공유 cache 값은 weak reference다. `NetworkSessionSignal::drop`은 signal을 disconnect한다.
- `window/polling.rs:24`: GNU 환경에서는 60초마다 allocator의 회수 가능한 페이지를 반환하도록 요청한다. RSS 하락은 객체 누수 부재를 증명하지 않으며, 순환 참조는 이 방법으로 고쳐지지 않는다.
- VTE `get_text_format`의 설치된 GIR 문서는 보이는 부분의 추출이라고 명시한다. `scrollback_snapshot()`을 전체 retained history 무조건 복사로 취급해 과점유 원인으로 판정하지 않았다.

## 수행한 소유권 실험과 한계

`/tmp/flowmux-memory-lifetime-check.c`는 Rust의 GObject 강한 clone/closure 수명을 GTK C의 `g_object_ref`와 signal closure destroy notify로 재현한다. 별도 Xvfb에서 stack+VTE, tab controller, popover, overlay scrim 네 경로를 검사했다.

결과:

```text
pane DnD: strong=0 retained_stack=0 retained_VTE=0
tab controller: strong=0 retained=0
popover menu: strong=0 retained=0
overlay menu: strong=0 retained=0
pane DnD: strong=1 retained_stack=1 retained_VTE=1
tab controller: strong=1 retained=1
popover menu: strong=1 retained=1
overlay menu: strong=1 retained=1
```

`strong=0`은 역방향 소유 참조를 만들지 않는 대조군이다. 강한 capture가 있으면 외부 참조와 parenting을 제거해도 객체가 살아 있으며, signal을 disconnect해 capture를 해제하면 객체가 해제되는 것까지 assert로 확인했다. 컴파일은 `-Wall -Wextra -Werror`를 통과했고 실험 종료 코드는 0이었다.

이 실험은 **동일한 GTK 소유 구조의 검증**이며 flowmux Rust 함수를 직접 호출한 회귀 테스트나 현재 실행 바이너리에 대한 heap profile은 아니다. 실제 창의 누수 객체 개수·누적 바이트·발생 이력은 확정하지 않았다. 이번 수정에서는 위 C 실험에 더해 별도 Xvfb/D-Bus의 실제 flowmux WindowController와 UI 함수로 생성/닫기 경로를 검증했다. 사용 중인 창에 새 코드를 적용하려고 재시작하지 않는다.
