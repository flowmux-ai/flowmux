<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Terminal minimap 기획 및 기술 설계

## 문서 상태

- 상태: 1단계 구현 및 live 검증 완료 (2026-08-10)
- 대상: flowmux의 VTE terminal surface
- 우선 검증 대상: 일반 shell, Claude Code, Codex CLI, OpenCode
- 결론: 일반 scrollback minimap은 구현 가능하다. alternate-screen TUI의 내부
  history minimap은 terminal 계층만으로 구현할 수 없다.

## 1. 결정 요약

flowmux에는 별도 terminal parser나 agent별 transcript reader를 추가하지 않는다.
기존 `GhosttyPane`의 VTE, `gtk::Overlay`, `GtkAdjustment`를 그대로 사용해 오른쪽
scrollbar를 작은 행 밀도 지도와 viewport 표시로 확장한다.

MVP의 동작 범위는 다음과 같다.

| 실행 형태 | minimap이 볼 수 있는 범위 | 탐색 가능 여부 |
|---|---|---|
| 일반 shell/inline 출력 | VTE가 보관한 전체 scrollback | 클릭·드래그로 이동 가능 |
| primary screen을 쓰는 Agent UI | VTE가 보관한 전체 scrollback | 클릭·드래그로 이동 가능 |
| alternate-screen TUI | 현재 화면 cell만 표시 | 내부 history 탐색은 2단계 검토 대상 |

TUI가 alternate screen에 들어가면 terminal emulator는 그 애플리케이션의 내부
대화 목록이나 scroll offset을 소유하지 않는다. 화면에 현재 그려진 cell만 볼 수
있다. 따라서 Claude/OpenCode 기본 TUI의 전체 대화 minimap을 만들려면 각 제품의
session model과 scroll API가 별도로 필요하다. 현재 공통 공개 API가 없으므로 이는
MVP 범위가 아니다.

## 2. 목표와 비목표

### 2.1 목표

- 긴 build log, test output, agent 대화에서 전체 위치와 출력 밀도를 한눈에 본다.
- 현재 viewport를 사각형으로 표시한다.
- minimap 클릭과 드래그를 VTE scrollback 이동으로 연결한다.
- Claude, Codex, OpenCode가 실행 중일 때 입력, focus, mouse capture, IME를 깨뜨리지
  않는다.
- 최대 1,000,000행 설정에서도 UI thread 비용과 메모리를 고정 상한 안에 둔다.
- 현재 화면만 표현할 수 있는 TUI 상태를 전체 history처럼 보이게 하지 않는다.

### 2.2 비목표

- 실제 작은 글꼴로 terminal 내용을 그대로 다시 렌더링
- PTY output을 두 번째 ANSI parser로 재해석
- Claude/Codex/OpenCode의 비공개 JSON/DB 직접 결합
- TUI 내부 scroll을 흉내 내기 위한 PageUp 또는 mouse event 주입
- prompt, error, tool call, diff를 자동 분류하는 semantic minimap
- session replay 또는 terminal 녹화

semantic command marker는 minimap 사용성이 확인된 뒤 OSC 133/633/1337 기반의
별도 후속 기능으로 평가한다.

## 3. 현재 코드에서 재사용할 수 있는 기반

현재 terminal backend는 VTE 하나다.

- [`ghostty_pane.rs`](../crates/flowmux/src/ui/ghostty_pane.rs)는 VTE를
  `gtk::Overlay`의 main child로 둔다.
- 오른쪽 12px `gtk::Scrollbar`는 VTE와 같은 `GtkAdjustment`를 사용한다.
- `contents-changed`는 Agent screen scan을 100ms 간격으로 제한하는 throttle을
  이미 갖고 있다.
- `scrollback_lines` 기본값은 5,000행이고 설정 상한은 1,000,000행이다.
- `vte4 0.8`의 `v0_76` feature와 system VTE 0.76.0을 사용한다.

VTE API에는 중요한 차이가 있다.

- `text_format()`은 문서상 terminal의 **visible part**만 반환한다.
- `text_range_format()`은 VTE 0.72부터 임의의 row/column 범위를 반환한다.

따라서 minimap은 현재 `screen_text()`를 재사용하지 않고
`text_range_format()`으로 표본 row를 읽어야 한다. 새 crate나 terminal parser는
필요 없다.

또한 현재 `scrollback_snapshot()`이 `text_format(HTML)`을 사용하면서 styled
terminal history 전체를 저장한다고 가정한다. VTE 문서와 실제 의미가 어긋날 수
있으므로 Phase 0에서 별도 회귀 항목으로 확인한다. 이 수정이 필요하더라도 minimap
widget과 결합하지 않는다.

## 4. 제안 UX

### 4.1 기본 형태

- 위치: terminal 오른쪽 `gtk::Overlay`
- 초기 폭: 50px
- 표현: 각 표본 row의 non-whitespace 비율을 가로 막대 길이로 표시
- 색상: terminal foreground/background에서 파생한 단색과 alpha
- viewport: 테두리 없는 최소 6px 높이의 반투명 overlay
- 동작: 왼쪽 클릭은 해당 위치를 viewport 중앙으로 이동, drag는 연속 이동
- focus: minimap은 keyboard focus를 가져가지 않고 terminal focus를 유지

실제 축소 글자를 그리지 않는 이유는 폭 50px에서도 읽을 수 없고, Pango layout 수천
개를 유지하는 비용에 비해 정보 가치가 낮기 때문이다. 출력의 분포와 현재 위치만
보여 주는 density map이 terminal navigation 목적에 충분하다.

### 4.2 history가 한 페이지뿐인 경우

`upper - lower <= page_size`이면 현재 화면의 density와 색상은 표시하되 viewport
overlay와 pointer navigation은 생략한다. 따라서 `clear`가 scrollback을 지운 직후와
alternate-screen TUI에서도 현재 VTE cell은 보이지만 존재하지 않는 history로 이동하지
않는다.

정확한 alternate-screen minimap이나 badge가
필요해지면 `pty-tee`가 이미 추적하는 DECSET/DECRST 47, 1047, 1049 상태를 새 IPC
event로 GUI에 전달해야 한다. `TerminalInputModes` getter 하나만 추가해서는 별도
process인 GUI에 상태가 전달되지 않는다.

### 4.3 기존 scrollbar와 설정

첫 구현은 기존 scrollbar workaround를 제거하거나 일반화하지 않는다.

- `terminal_minimap_enabled=false`: 현재 12px scrollbar 동작 유지
- `terminal_minimap_enabled=true`: scrollbar를 숨기고 minimap overlay 표시

기본값은 ON이고 초기 폭은 50px이다. 설정 UI와 `options.json`에서 ON/OFF 및
12..=96px 폭을 바꿀 수 있으며 열린 terminal에도 즉시 반영한다.

## 5. 데이터 흐름과 구성 요소

```text
PTY output
   -> VTE grid / scrollback
       -> contents-changed (최대 5Hz로 coalesce)
           -> TerminalMinimap sampler
               -> text_range_format(HTML, sampled history row)
               -> text_format(HTML, current screen only)
               -> 기존 VTE HTML parser
               -> Vec<RowSample { density, color }> (원문은 보관하지 않음)
               -> DrawingArea::queue_draw()

GtkAdjustment changed/value-changed
   -> viewport rectangle만 다시 계산

minimap click/drag
   -> y를 adjustment 범위로 변환
   -> adjustment.set_value(clamped target)
   -> VTE viewport 이동
```

### 5.1 최소 구성

`TerminalMinimap` 하나가 다음 상태만 가진다.

- weak `vte::Terminal`
- `gtk::DrawingArea`
- 최대 128개의 `RowSample { density, color }`
- 마지막으로 관찰한 adjustment bounds와 widget 크기
- refresh pending flag

새 service, trait, factory, worker thread는 만들지 않는다. VTE widget은 GTK main thread
밖에서 접근할 수 없으므로 background thread로 넘기지 않는다.

### 5.2 표본 추출

전체 scrollback을 매 refresh마다 복사하지 않는다.

1. `lower`, `upper`, `page_size`, terminal column 수를 읽는다.
2. 전체 row 수가 128 이하이면 각 row를 읽는다.
3. 128행보다 크면 범위 전체에서 균등하게 최대 128행만 선택한다.
4. 각 row를 `text_range_format(HTML, row, 0, row, last_column)`으로 읽는다.
5. 기존 scrollback HTML parser로 대표 foreground/background 색상과
   `non_whitespace_chars / column_count`를 추출한다.
6. raw text는 즉시 버린다.

한 페이지 이하에서는 VTE의 absolute history row와 `clear` 이후 현재 screen row의
좌표가 달라질 수 있으므로 `text_format(HTML)`로 현재 화면만 한 번 읽는다. 이 경로도
최종 cache는 최대 128개 sample이며 원문을 보관하지 않는다.

Unicode display width와 foreground/background cell 색을 정확히 재현하지 않는 것은
의도된 근사다. 장문 history에서도 호출 수, 저장량, 그리기 수가 일정하다는 장점이
더 크다.

표본 row 계산, adjustment mapping, viewport rectangle 계산은 GTK와 분리한 순수 함수로
두고 작은 unit test로 검증한다.

### 5.3 viewport와 pointer mapping

adjustment 단위가 row라고 가정할 때 계산은 다음과 같다.

```text
total      = max(upper - lower, page_size)
view_top   = (value - lower) / total
view_size  = page_size / total
click_row  = lower + (pointer_y / height) * total - page_size / 2
target     = clamp(click_row, lower, upper - page_size)
```

VTE의 range row 좌표와 adjustment 좌표가 동일한지, endpoint가 inclusive인지,
`scroll-unit-is-pixels`가 false인지 Phase 0의 실제 widget test에서 먼저 확인한다.
이 계약이 다르면 구현 전에 좌표 변환만 수정한다.

## 6. Agent별 호환성 검토

2026-08-10 로컬 설치본을 prompt 입력 없이 PTY에서 3~5초 관찰한 결과다. 버전과
설정에 따라 기본 renderer가 바뀔 수 있으므로 escape 관찰값과 공식 option을 구분한다.

| 대상 | 관찰/공식 동작 | 전체 history minimap | 권장 지원 |
|---|---|---:|---|
| 일반 shell | primary screen + VTE scrollback | 가능 | 완전 지원 |
| Codex CLI 0.147.0 기본 | 이 환경에서는 DECSET 1049 미관찰 | 가능할 수 있음 | 기본값에 의존하지 않음 |
| `codex --no-alt-screen` | OpenAI Docs가 alternate screen 비활성화를 명시 | 가능 | 완전 지원 기준 |
| Claude Code 2.1.226 기본 | DECSET 1049 관찰 | 불가 | 현재 화면 minimap |
| `claude --ax-screen-reader` | 공식 문서상 flat text/classic renderer, 로컬에서 1049 미관찰 | foreground에서 가능, multi-turn 실증 필요 | 선택적 호환 모드 |
| OpenCode 1.15.7 기본 | DECSET 1049 관찰 | 불가 | 현재 화면 minimap |
| `opencode run` | one-shot noninteractive frontend | 출력 scrollback만 가능 | interactive TUI 대체로 보지 않음 |

### 6.1 Codex

Codex는 공식 `--no-alt-screen` option이 있고 `tui.alternate_screen` 설정도 이 flag로
override할 수 있다. flowmux 문서와 smoke test는 `codex --no-alt-screen`을 전체 대화
minimap의 보장 조건으로 사용한다. 사용자의 command line이나 Codex 설정을 flowmux가
몰래 변경하지 않는다.

### 6.2 Claude Code

Claude 기본 TUI에서는 current screen map만 정확하다. `--ax-screen-reader`는 flat text,
classic renderer를 사용하고 로컬 probe에서는 alternate screen에 진입하지 않았으므로
foreground session의 terminal scrollback을 구성할 수 있다. 실제 multi-turn 출력이
계속 누적되는지는 Phase 3에서 확인한다. 이는 일반 visual TUI를 유지하는 inline
flag가 아니라 접근성 renderer이며,
공식 문서상 attached background session은 계속 fullscreen으로 렌더링된다. 따라서
flowmux 기본 launch flag로 강제하지 않는다.

### 6.3 OpenCode

공식 CLI/TUI 문서와 로컬 `--help`에는 interactive TUI의 alternate screen을 끄는 option이
확인되지 않았다. 관련 요청도 구현 완료가 아니라 inactivity로 자동 종료됐다.
OpenCode의 client/server model로 별도 frontend를 만들 수는 있지만 terminal minimap을
위해 별도 OpenCode UI를 소유하는 것은 범위를 크게 넘는다.

## 7. 다른 구현 사례

| 제품 | 구현에서 배울 점 | flowmux 결정 |
|---|---|---|
| [Extraterm Mini-map](https://extraterm.org/features.html) | 실제 terminal session mini-map 제공 | 가장 직접적인 선행 사례 |
| [Extraterm ScrollMap source](https://github.com/sedwards2009/extraterm/blob/master/extensions/ScrollMap/src/ScrollMapExtension.ts) | 120 cell 폭, 256행 image block, 16배 축소, 200ms debounce, viewport outline, click/wheel, command block 상태색 사용 | MVP는 density, debounce, viewport, click만 차용 |
| [VS Code terminal shell integration](https://code.visualstudio.com/docs/terminal/shell-integration) | 글자 축소본 대신 성공/실패 command 위치를 overview ruler에 표시. OSC 633/133과 iTerm2 mark 지원 | semantic marker 후속 설계의 기준 |
| [Contour scrollbar](https://contour-terminal.org/configuration/profiles/) | `hide_in_alt_screen`을 제품 설정으로 노출 | alternate screen 제약을 숨기지 않음 |
| [WezTerm scrollback](https://wezterm.org/scrollback.html) | scrollback 상한, scrollbar, search/copy mode를 분리 | minimap도 VTE 보관 범위만 다룸 |
| [Warp Blocks](https://docs.warp.dev/terminal/blocks/block-basics) | command와 output을 block으로 묶고 상태색·이동 제공 | raw minimap 이후 semantic navigation 후보 |

Extraterm은 cell buffer와 block model을 직접 소유하므로 정확한 색상 image cache가
가능하다. flowmux는 VTE 공개 API만 사용하므로 같은 구조를 그대로 복제하지 않고
bounded row sampling으로 단순화한다.

## 8. 성능 예산

MVP 목표값은 구현 후 benchmark로 조정한다.

- refresh 빈도: sustained output에서 최대 5Hz
- text extraction: refresh당 최대 128 row
- 캐시: pane당 128 density/color sample과 작은 widget state만 유지
- 원문 보관: 없음
- draw: 최대 128 bar + viewport 1개
- GTK main-thread stall: p95 8ms 미만, 단일 frame 16ms 초과 없음
- 1,000,000행 scrollback에서도 호출 수와 캐시 크기 증가 없음

128회 VTE range call이 목표 시간을 넘으면 표본 수를 64로 낮춘다. 전체 history
추출, thread 접근, 별도 parser 추가로 해결하지 않는다.

## 9. 구현 단계

### 1단계 — 일반 terminal scrollback (구현 및 live 검증 완료)

- `ui/terminal_minimap.rs`의 고정 상한 sampler, Cairo drawing, pointer mapping
- 기존 `gtk::Overlay`에서 scrollbar와 상호 배타적으로 표시
- content refresh 최대 5Hz, refresh당 최대 128행
- 기본 ON, 폭 12..=96px, 열린 terminal 즉시 적용
- one-page/alternate-screen 상태에서는 현재 화면만 표시하고 history 탐색은 생략
- 표본·viewport·pointer mapping 및 widget graph 해제 test

실제 flowmux terminal에서 4,000행을 출력해 minimap 표시, 중간 클릭, 아래쪽
drag를 검증했다. Options에서 ON/OFF와 폭 24→56px 변경은 열린 terminal에 즉시
반영됐다. 초기 구현에서는 `less`의 alternate screen에서 숨었다가 종료 후 복원됐다.

후속 live 검증에서는 `clear` 직후 현재 화면에 6개 ANSI 색상 행을 출력해 density와
색상 sample이 즉시 다시 나타나는지 확인했다. 이어 360행을 출력한 뒤 50px minimap의
색상 구간, 무테 반투명 viewport, 중간 클릭 이동을 확인했다.

메모리 smoke test는 동일 pane에 50,000행, 100,000행, 다시 100,000행을 출력했다.
RSS는 각각 203,240KB(시작), 203,308KB, 203,584KB, 203,552KB였고 5,000행
scrollback 상한 도달 뒤 계속 증가하지 않았다. minimap 자체는 pane당 최대 128개
density/color sample만 보관하며 terminal 원문은 보관하지 않는다.

### 2단계 — TUI 내부 history (보류)

- DECSET/DECRST 47, 1047, 1049 상태를 GUI에 전달할지 검토
- Claude/Codex/OpenCode별 공개 session/scroll API 조사
- terminal row와 TUI conversation item 사이의 안정적인 좌표계가 있을 때만 구현
- PageUp/mouse event 주입이나 비공개 session 파일 결합은 사용하지 않음

runtime/UI 변경은 unit test나 `cargo check`로 끝내지 않고 실제 flowmux pane에서 같은
시나리오를 재현해 사용자에게 보이는 상태를 확인한다.

## 10. 예상 수정 파일

MVP 본체는 다음 두 파일에 제한한다.

- `crates/flowmux/src/ui/terminal_minimap.rs` — widget, sampling, drawing, mapping
- `crates/flowmux/src/ui/ghostty_pane.rs` — overlay 연결과 refresh signal

- `crates/flowmux-config/src/options.rs`
- `crates/flowmux/src/ui/options_dialog.rs`
- `crates/flowmux/src/ui/workspace_view.rs`
- `crates/flowmux/src/ui/window/mod.rs`
- `docs/configuration.md`

MVP에서는 `flowmux-core`, IPC protocol, `pty-tee`, Agent session 저장 형식을 변경하지
않는다.

## 11. 승인 기준

- 일반 scrollback 전체 범위와 viewport 위치가 minimap에 일관되게 표시된다.
- 클릭/드래그 target이 adjustment 범위를 벗어나지 않는다.
- 1,000,000행 설정에서도 UI가 멈추지 않고 고정된 표본 수만 읽는다.
- terminal keyboard focus, selection, wheel, Agent mouse capture, IME가 회귀하지 않는다.
- Codex `--no-alt-screen`에서는 이전 대화 위치로 실제 이동할 수 있다.
- Claude/OpenCode alternate-screen에서는 현재 화면만 표시하고, 존재하지 않는
  terminal history로 이동하는 것처럼 보이지 않는다.
- live flowmux에서 일반 shell과 inline Agent 시나리오를 검증하고 결과를 기록한다.

## 12. 후속 후보

사용성 검증 뒤에만 다음을 검토한다.

1. OSC 133/633/1337 command/error marker
2. search result marker
3. 정확한 alternate-screen IPC 상태와 badge
4. Agent 공개 session API가 생긴 경우 별도의 semantic conversation outline

Agent transcript 기반 outline은 terminal scroll 위치와 공통 좌표계가 생기기 전까지
minimap에 섞지 않는다.

## 13. 근거 문서

- [VTE `get_text_format()`](https://gnome.pages.gitlab.gnome.org/vte/gtk4/method.Terminal.get_text_format.html)
- [VTE `get_text_range_format()`](https://gnome.pages.gitlab.gnome.org/vte/gtk4/method.Terminal.get_text_range_format.html)
- [OpenAI Docs: Developer commands](https://learn.chatgpt.com/docs/developer-commands?surface=cli)
- [Claude Code CLI reference](https://code.claude.com/docs/en/cli-reference)
- [OpenCode CLI](https://opencode.ai/docs/cli/)
- [OpenCode TUI](https://opencode.ai/docs/tui/)
- [OpenCode issue #106](https://github.com/anomalyco/opencode/issues/106)
