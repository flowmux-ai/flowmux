<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Terminal minimap 개선 설계

## 상태와 범위

- 대상: flowmux의 VTE terminal surface
- 지원: 일반 shell과 primary-screen 프로그램의 VTE scrollback
- 제한: alternate-screen TUI에서는 애플리케이션 내부 history를 VTE가 소유하지
  않으므로 현재 화면만 표시한다.

## 개선 동작

minimap은 전체 history를 고정 높이에 비례 압축하는 scrollbar가 아니다. minimap의
실제 픽셀 높이만큼 연속된 행을 표시하는 지역 history 창이다.

- terminal row 하나를 높이 1px로 간격 없이 그린다.
- glyph가 있는 cell은 foreground, background가 있는 cell은 background 색으로 그린다.
- minimap wheel은 25행씩 지역 history 창만 이동한다.
- click과 drag는 그 지역 좌표를 terminal viewport 위치로 바꾼다.
- viewport 표시는 실제 terminal page row 수와 같은 높이다.
- 출력 갱신은 100ms로 모은다.

flowmux는 VTE grid 좌표와 GTK `DrawingArea`를 사용해 이 동작을 구현한다.

## flowmux 구현

`TerminalMinimap`은 다음 작은 상태만 유지한다.

- weak `vte::Terminal`
- `gtk::DrawingArea`
- 현재 지역 창의 시작 row와 아래쪽 기준 offset
- minimap 픽셀 높이만큼의 `Vec<Vec<VtePixelRun>>`
- refresh timeout source

정상 scrollback의 한 번의 갱신은 다음 순서다.

1. `GtkAdjustment`의 `lower`, `upper`로 minimap 높이만큼의 지역 창을 계산한다.
2. `text_range_format(HTML)`을 한 번 호출해 그 row 범위를 읽는다.
3. 기존 VTE HTML parser로 cell column, 길이, 색상 run만 남긴다.
4. Cairo에서 terminal column을 minimap 폭에 맞춰 cell pixel을 그린다.
5. terminal viewport가 지역 창 밖이면 indicator를 위나 아래에 고정하고 흐리게 한다.

원문, 별도 terminal parser, worker thread, service, 새 dependency는 추가하지 않는다.
지역 창의 메모리와 draw 비용은 전체 scrollback 길이와 무관하게 minimap 높이로 제한된다.

## 배치와 입력

minimap은 기존 `gtk::Overlay` 오른쪽에 있지만, 활성화할 때 VTE에 같은 폭의
`margin-end`를 준다. 따라서 terminal grid와 minimap은 겹치지 않으며, 과거에 split
minimum-size를 깨뜨렸던 `gtk::Box` wrapper도 다시 도입하지 않는다.

- minimap ON: 표준 scrollbar를 숨기고 VTE 오른쪽 gutter에 minimap을 표시한다.
- minimap OFF: VTE margin을 0으로 되돌리고 표준 scrollbar를 표시한다.
- alternate screen: minimap과 gutter를 숨기고 표준 scrollbar를 표시한다.
- wheel over minimap: preview offset만 변경하고 event propagation을 중단한다.
- click/drag: 지역 row 좌표에서 clamped adjustment 값을 계산한다.
- terminal scroll: viewport가 지역 창 경계를 넘으면 preview offset도 함께 이동한다.
- focus: 입력 뒤 terminal이 focus를 유지한다.

폭은 `12..=96px`, opacity는 `0..=100%`이며 열린 terminal에도 즉시 적용된다.

## alternate screen

`pty-tee`가 전달하는 DECSET/DECRST 47, 1047, 1049 상태를 사용한다.
alternate screen에서는 minimap을 숨기고 표준 scrollbar를 표시한다. Agent별 비공개
transcript나 PageUp/mouse event 주입은 사용하지 않는다.

## 검증 기준

- leading space, wide Unicode, foreground/background cell 위치가 보존된다.
- `clear` 뒤에도 현재 scrollback 좌표를 정확히 읽는다.
- minimap wheel은 terminal `GtkAdjustment::value`를 변경하지 않는다.
- click/drag target과 preview offset은 각 범위를 벗어나지 않는다.
- minimap ON/OFF와 폭 변경 시 VTE gutter가 함께 바뀐다.
- alternate screen에서 minimap 대신 표준 scrollbar가 보인다.
- keyboard input, selection, terminal wheel, Agent mouse capture, IME, split layout이
  기존처럼 동작한다.
- runtime/UI 변경은 실제 flowmux pane에서 출력, wheel, click/drag, ON/OFF를 재현한다.

## 관련 코드와 문서

- [`terminal_minimap.rs`](../crates/flowmux/src/ui/terminal_minimap.rs)
- [`terminal_scrollback.rs`](../crates/flowmux/src/ui/terminal_scrollback.rs)
- [`ghostty_pane.rs`](../crates/flowmux/src/ui/ghostty_pane.rs)
- [VTE `get_text_format()`](https://gnome.pages.gitlab.gnome.org/vte/gtk4/method.Terminal.get_text_format.html)
- [VTE `get_text_range_format()`](https://gnome.pages.gitlab.gnome.org/vte/gtk4/method.Terminal.get_text_range_format.html)
