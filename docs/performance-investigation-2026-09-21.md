<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

**flowmux 성능·플리커링 조사 — 2026-09-21**

가장 먼저 개선할 곳은 미니맵의 메인 스레드 작업량, 동기화 출력 미지원, IME 확정을 위한 포커스 왕복이다. 스크롤 이상은 렌더링 문제뿐 아니라 탭 선택 정책과 저장 범위에서도 재현됐다. 아래에서 **실제 재현**, **코드로 확인한 동작**, **추가 검증이 필요한 가설**을 구분한다. 제품 코드를 수정하거나 사용자 설정을 변경하지 않은 조사·개선안이다.

현재 Codex 창 PID 6619는 종료·재시작하지 않았다. 테스트는 별도 프로세스·소켓·XDG 상태·설정·D-Bus로 실행했고 테스트가 소유한 창만 닫았다. 실제 데스크톱 비교도 별도 창으로 수행했다.

**환경과 측정 해석**

- 조사 소스: `9d5b903`, flowmux 0.10.0. 터미널은 `GhosttyPane`이라는 이름과 달리 GTK VTE 백엔드다.
- 런타임: GTK 4.14.5, VTE 0.76.0, Wayland 데스크톱. 기존 창은 `_NET_WM_PID=6619`인 X11 창이므로 XWayland 경로다. `main.rs::install_gtk_wayland_surface_workaround`도 이 환경에서 X11을 우선한다.
- 실행 중 바이너리와 디스크의 설치 바이너리는 같은 0.10.0 표시지만 SHA-256이 다르다. 기존 `/proc/6619/exe`는 `(deleted)` 상태다. 새 소스/새 설치본의 검증을 기존 창에 이미 적용된 수정으로 해석하면 안 된다.
- 기존 창의 5초 관측은 CPU 18.6%, 그중 GTK 메인 스레드 18.2%였다. 당시 Codex가 출력 중이므로 **유휴 CPU 수치가 아니다**.
- CPU는 한 코어=100%인 프로세스 CPU 시간이다. 아래 A/B는 조건별 5초, 한 번씩 측정한 방향성 비교이며 통계적 벤치마크나 FPS 수치가 아니다. 시스템 전체 GPU/컴포지터 비용도 포함하지 않는다.
- `perf stat`은 `perf_event_paranoid=4` 때문에 거부됐다. 시스템 정책을 바꾸지 않고 `/proc` CPU 시간, 격리 인스턴스의 VTE API 계측, PTY 수신 기록, 화면 캡처를 사용했다.

무입력 상태도 별도로 65.1초 관찰했다. 미니맵과 cursor blink를 켠 현재 소스의 Xvfb 창에서 CPU는 약 1.67%였다. 250ms 간격 픽셀 비교에서는 10×19px 커서 영역의 점멸과 초기 사용량 footer 갱신 한 번이 관측됐고, 이후 전체 창 공백은 관측되지 않았다. 15초/60초 주기 작업을 포함한 관찰이지만 250ms보다 짧은 transient flash를 배제하는 고속 촬영은 아니다. 원자료는 `/tmp/fm-gui-z0ftmnsj`에 있다.

**1. 미니맵이 컬러 출력의 비용을 크게 증폭한다 — P1, 실제 앱 A/B 재현**

| 실행 환경 | 작업 | 미니맵 OFF | 미니맵 ON |
|---|---|---:|---:|
| 설치 배포 바이너리, 실제 데스크톱의 기본 렌더러 | 일반 로그 | 7.0% | 10.8% |
| 동일 | 컬러 로그 | 7.6% | 44.6% |
| 설치 배포 바이너리, Xvfb/Cairo | 일반 로그 | 4.4% | 10.2% |
| 동일 | 컬러 로그 | 6.8% | 30.0% |
| 현재 소스 개발 빌드, Xvfb/Cairo | 일반 로그 | 6.6% | 12.8% |
| 동일 | 컬러 로그 | 8.2% | 60.0% |

워크로드는 먼저 1,200행을 채우고 초당 30행을 추가한다. 컬러 조건은 행마다 약 100자의 색을 각각 바꾸는 스트레스 사례다. 일반 로그 전체를 대표하지 않지만, ANSI 색이 많은 빌드 출력·diff·TUI에서 비용이 커지는 구조를 드러낸다. 현재 사용자 설정에서도 미니맵이 켜져 있다.

경로: [terminal_minimap.rs](../crates/flowmux/src/ui/terminal_minimap.rs)의 `schedule_refresh` → `refresh_now` → VTE HTML 추출 → [terminal_scrollback.rs](../crates/flowmux/src/ui/terminal_scrollback.rs)의 `vte_html_pixel_rows` → Cairo run별 `fill`. 높이의 픽셀 수만큼 행을 추출하며, 모두 GTK 메인 스레드에서 실행한다. 100ms 주기 제한은 있으나 한 번의 작업량 제한은 약하다. 휠로 미니맵 자체를 움직이는 `scroll_preview`는 `refresh_now`를 직접 호출해 이 제한도 우회한다.

독립 VTE 측정에서는 760행의 HTML이 약 2.3MB였고, **추출만 약 16ms**가 걸렸다. XML 파싱·할당·그리기는 별도 비용이다. 60Hz의 프레임 예산 16.7ms와 맞먹는다. 배포본 계측에서도 컬러 조건에서 초당 range API 호출이 약 10회→18회로, 해당 API 실행 시간이 약 0.6ms→57ms로 증가했다. API 외 CPU 증가분을 모두 XML 파서로 단정할 수는 없다.

개선안: 먼저 미니맵을 끄는 옵션을 진단용 A/B로 활용한다. 구현은 휠·리사이즈·출력을 기존 예약 함수로 모으고, 추출할 행/셀 수에 상한을 둔 뒤 캐시된 결과를 화면 높이에 맞춰 그리는 순서가 작다. 동일한 결과는 다시 그리지 않고, 고비용 컬러 입력에서는 주기를 늘리거나 저해상도/단색 표현을 검토한다. GTK/VTE 객체 자체를 worker thread로 옮기면 안 된다. 순수 데이터 파싱의 별도 스레드화는 이 조치 이후에도 필요할 때 검토한다.

검증 조건: 미니맵 ON의 1회 메인 스레드 작업 p95를 4ms 이하로 잡고 실측한다. 1/4/16 visible pane, 30Hz/60Hz 출력, 긴 줄·한글·다색 셀·연속 휠·리사이즈에서 입력 지연 및 메모리 상한을 함께 확인한다. 4ms는 현재 달성 수치가 아닌 제안 예산이다.

**2. VTE 0.76은 동기화 출력 구간의 중간 화면을 노출한다 — P1, 픽셀 재현**

PTY에 `CSI ?2026$p`를 질의하자 `CSI ?2026;4$y`가 돌아왔다. 해당 모드는 permanently reset이다. `OLD FRAME`을 그린 뒤 `CSI ?2026h`와 화면 지우기를 보내고, 종료 신호를 보내기 전 80ms 동안 관찰하면 **화면에서 OLD FRAME이 사라진다**. 그 뒤 `NEW FRAME`과 `CSI ?2026l`을 보내야 새 글자가 나타난다. 캡처에서도 확인했다.

즉, TUI가 화면 지우기와 재그리기를 나누어 보내면 전체/부분 공백 프레임이 보일 수 있다. 이는 앱 출력, PTY 분할 전달, 렌더링 주기가 맞물리는 경로다. 현재 사용자 TUI의 모든 깜빡임이 이 시퀀스 때문이라는 뜻은 아니다. 80ms 간격은 재현을 명확히 하기 위한 의도적인 분리다.

개선안: 동기화 출력 지원을 실제 capability 질의와 픽셀 테스트로 확인한 VTE 버전/패치를 검토한다. 릴리스 번호만 보고 지원을 가정하지 않는다. 지원 없는 backend에서 `TERM_PROGRAM`만 바꾸거나 무조건 몇 ms씩 출력 지연을 넣는 방식은 피한다. proxy에서 frame buffering을 구현해야 한다면 split escape sequence, 최대 버퍼 크기, 종료 마커 누락 시 timeout, 중첩/비정상 시퀀스, 알림 OSC 순서를 모두 설계해야 하므로 우선순위는 검증된 backend 지원이다.

검증 조건: begin→clear 이후에도 이전 프레임이 남고 end 이후 완성 프레임만 나타나야 한다. 입력·커서·IME redraw가 열린 동기화 구간을 깨뜨리지 않는지도 확인한다.

**3. IME 확정이 실제 focus-out/focus-in과 부가 UI 작업을 발생시킨다 — P1, 실제 IBus/PTY 재현**

[ghostty_pane.rs](../crates/flowmux/src/ui/ghostty_pane.rs)의 `flush_pending_preedit`는 `window.set_focus(None)` 다음 `term.grab_focus()`를 실행한다. Enter, Shift+Enter, Shift+기호, 환경별 navigation bypass가 이 함수를 사용한다. 한글을 조합하지 않는 영어 상태의 Enter에서도 실행된다.

실제 flowmux에서 DEC focus reporting(`?1004h`)을 켠 입력 수신 프로그램으로 확인한 결과, Enter마다 `ESC[O`, `ESC[I`, CR이 도착했다. 한글 `가나`의 마지막 `나`를 확정할 때도 focus-out과 commit이 섞여 전달됐다. 이 입력이 focus에 반응하는 TUI의 재그리기를 유발할 수 있다. VTE focus signal은 `PaneFocused`로 이어져 같은 pane에서도 MRU/label/agent display 및 파일·worktree·session 패널 refresh 경로를 탄다([window/mod.rs](../crates/flowmux/src/ui/window/mod.rs)의 `on_pane_focused`, [pane_commands.rs](../crates/flowmux/src/ui/window/pane_commands.rs)).

실제 IBus 동기·비동기 모드에서 `가나`→Enter, `한`→Backspace→`?`, 빠른 Enter 3회를 확인했다. 일반 경로의 결과는 `가나\r`, `하?`이며 CR 유실은 관측하지 않았다. cursor hidden 상태에서도 `나` preedit는 화면에 보였다. 따라서 기존 workaround 전체를 단순 삭제하면 이미 해결된 조합 표시/입력 순서 문제가 되돌아올 수 있다.

환경별 추가 문제: `FLOWMUX_ENABLE_IBUS_NAV_WORKAROUND=1`로 WSL/Flatpak용 경로를 활성화하면 `한`→Backspace에서 `한`을 먼저 commit한 다음 DEL을 보낸다. `하`로 자모를 분해하는 동작과 다르다. 이 분기는 코드에 명시된 절충이며 실제 PTY에서도 확인됐다. 실제 WSLg/Flatpak 호스트 자체를 시험한 것은 아니다.

개선안: 근본적으로 VTE의 preedit invalidation/commit 처리를 cursor visibility와 독립적으로 다루는 backend 수정이 필요하다. 그전에는 IME 내부 focus cycle을 일반 사용자 focus 전환으로 재처리하지 않도록 범위를 좁히고, 같은 pane/surface의 중복 UI refresh를 줄인다. 실제 탭 변경·알림 확인 동작은 유지해야 한다. 20ms fallback만 늘리는 조정은 순서를 보장하지 않으므로 slow IBus에서 별도 검증한다.

또한 `install_terminal_key_capture`는 모든 키에 즉시 redraw + 16ms 후 redraw를 예약한다. 검색 overlay에 포커스가 있어도 terminal redraw를 예약한다. 최소 개선은 실제 terminal focus 확인과 미처리 follow-up 한 개로 합치는 것이다. 단, async preedit가 늦게 도착하는 경우와 hidden cursor 조합 표시를 반드시 유지해야 한다. 이 추가 redraw의 개별 CPU 기여도는 분리 측정하지 않았다.

**4. alternate screen 진입/복귀가 터미널 폭을 바꾼다 — P1, 실제 PTY 크기 재현**

`set_alternate_screen`은 미니맵을 숨길 때 VTE의 `margin_end`를 60→0으로 바꾼다. 상태는 VTE 출력 파싱과 별개인 pty-tee IPC 경로로 오며 출력 이벤트는 100ms 제한을 공유한다. 때문에 화면 모드 변경과 크기 변경이 같은 시점이라는 보장이 없다.

실제 flowmux 안에서 `os.get_terminal_size()`를 기록했다: 일반 화면 **94×36** → alternate 진입 직후 **94×36** → 300ms 뒤 **98×36** → 복귀 직후 **98×36** → 300ms 뒤 **94×36**. 사용자는 창을 늘리지 않았는데 TUI에는 열 수가 달라지고 재배치가 발생한다. 독립 VTE에서도 115↔122열로 같은 메커니즘을 확인했다.

개선안: normal/alternate 모드 전환만으로 terminal allocation을 바꾸지 않는다. 가장 작은 선택은 예약 여백을 일정하게 유지하고 overlay 내용만 바꾸는 것이다. 공간 사용 정책을 바꾸더라도 scrollbar/minimap 표시 여부를 PTY geometry에서 분리한다. 모드 이벤트의 전달 지연도 이때 함께 검증한다.

검증 조건: 창 크기를 바꾸지 않은 진입/복귀에서 열·행 수가 일정해야 한다. vim/less/tig, 빠른 진입→종료, split resize 중 진입, 숨겨진 탭에서 진입한 뒤 활성화, SSH nested PTY를 포함한다.

**5. 워크스페이스 복귀가 이전 탭 대신 마지막 탭을 고른다 — P2, 실제 UI 재현**

`WindowController::activate_workspace`는 저장된 active surface 대신 첫 pane의 `surfaces.last()`를 선택한다. 코드 주석에도 명시된 현재 정책이다. 두 탭 중 첫 탭의 과거 출력 H0817~H0852를 보고 다른 워크스페이스로 갔다 돌아오니 빈 shell인 두 번째 탭으로 바뀌었다. 다시 첫 탭을 선택하면 이전 출력은 남아 있었다. 이는 스크롤 버퍼 소실과 구분해야 한다.

개선안: workspace별 마지막 focused pane 및 active surface를 우선 복원하고 삭제된 경우만 유효한 leaf로 fallback한다. 알림/검색/agent 항목처럼 명시적 목적지가 있는 동작은 그 목적지를 우선한다. 제품 정책 변경이지만 사용자에게 화면/스크롤이 튀는 인상을 직접 줄일 수 있다.

검증 조건: 복귀 전후 pane/surface ID와 logical viewport를 함께 비교한다. 화면 문자열만 비교하면 다른 탭으로 바뀐 문제를 스크롤 문제로 오인한다.

**6. 스크롤백 저장이 전체 히스토리가 아닌 viewport만 보존한다 — P2, 저장 파일 재현**

`scrollback_snapshot`은 `text_format(Html)`을 호출한다. 설치 VTE 0.76에서 이 API는 보이는 화면을 반환한다. 5,100행 출력 후 5,000행을 보유한 독립 VTE에서 반환 HTML은 42행이었다. 실제 앱에 H0000~H0999를 출력하고 과거 H0817~H0852를 보는 탭을 저장했을 때, state.json에는 그 36개 marker만 남았다. 화면 끝으로 이동한 다른 실행에서는 H0966~H0999만 저장됐다.

개선안: normal 화면의 보존 범위를 absolute text row로 정하고 `text_range_format`으로 크기 제한 안에서 추출한다. 기존 output-search 범위 계산을 재사용할 여지가 있다. alternate 화면을 shell history로 저장하는 정책도 분리 검토한다. viewport 위치 보존이 필요하면 내용 저장과 별도로 명시한다. 추출 범위를 전체로 넓히면 현재보다 비용이 증가하므로 chunk/예산을 함께 적용해야 한다.

검증 조건: 화면 밖의 오래된 marker와 최신 marker가 설정된 용량 내에서 모두 보존되고, 중간으로 스크롤한 채 저장해도 최신 출력이 빠지지 않아야 한다. 색·한글·soft wrap·CSI 3J·alternate·재시작 복원까지 확인한다. 현재 창은 재시작하지 않았다.

**7. 출력 backpressure가 입력·리사이즈까지 막는다 — P1, PTY 중계기 재현**

[pty_tee.rs](../crates/flowmux-cli/src/pty_tee.rs)의 단일 pump는 `write_all(stdout, slice)`이 끝나야 outer input과 signal self-pipe를 다시 처리한다. stdout이 EAGAIN이면 `wait_writable`은 출력 fd만 기다린다. 종료 신호를 확인하는 기존 보호는 있지만 입력과 SIGWINCH를 그 안에서 처리하지는 않는다.

격리 outer PTY의 출력을 일부러 읽지 않아 막은 뒤 입력 `q`와 SIGWINCH를 보냈다. **500ms 동안 자식에 둘 다 전달되지 않았고**, 출력 읽기를 재개하자 약 0.2ms 안에 둘 다 도착했다. GUI 메인 스레드가 느려져 VTE의 읽기가 밀리면 이 구조가 입력·resize 지연을 증폭시킬 수 있다. 일상 입력에서 항상 500ms 지연된다는 의미는 아니다.

개선안: 제한된 pending output 버퍼와 POLLOUT을 같은 poll loop에 포함해, 출력이 막혀도 입력·resize·종료 신호를 처리한다. 반대 방향의 큰 붙여넣기도 같은 원칙으로 검토한다. 버퍼 무제한 확대나 EAGAIN busy loop로 바꾸면 안 된다.

검증 조건: 출력 소비를 멈춘 상태에서도 resize와 입력이 정한 시간 예산 내 전달돼야 한다. 현재의 termination/EOF/OSC 순서/바이트 보존 테스트도 유지한다.

**증상별 조사 범위와 남은 불확실성**

| 상황 | 확인한 결과 / 고려해야 할 경로 |
|---|---|
| 단색·다색 지속 출력 | 실제 창/가상 창의 미니맵 A/B로 비용 증가 재현 |
| clear→redraw, 부분/전체 TUI 갱신 | 2026 구간 중간 공백 프레임 픽셀 재현 |
| 한글 조합, hidden cursor | 실제 IBus에서 inline preedit 확인; 현 workaround는 유효한 역할도 있음 |
| 조합 중 Enter/기호/Backspace | 동기·비동기 IBus의 commit 순서 확인, synthetic focus events 확인 |
| WSL/Flatpak navigation workaround | 분기 강제 활성화로 음절 단위 삭제 확인; 실제 해당 호스트는 미시험 |
| 빠른 연속 Enter, Shift+Enter | 연속 Enter 3회는 유실 없음; Shift+Enter의 별도 callback/fallback 경로와 기존 단위 테스트 검토 |
| 일반 키·검색창 입력 | unconditional terminal redraw 및 16ms follow-up 확인; 개별 비용은 미분리 |
| 창 축소→확대 | 짧은 행의 실제 UI round trip에서 H0817~H0852 유지 |
| split→ratio 25/75/50%→unsplit | 같은 탭에서 H0817~H0852 유지; 지속 출력/IME 동시 drag는 추가 스트레스 대상 |
| 긴 줄 reflow·scrollback 한도 | VTE 행 수/좌표 재기준화 확인; 물리 행 한도 도달 후 좁히면 오래된 데이터가 탈락할 수 있음 |
| 탭 표시줄 출현 | 37→36 visible rows가 되며 첫 행이 H0816→H0817로 바뀜; 마지막 H0852 유지. 버퍼 소실과 구분 |
| workspace 왕복 | active surface가 마지막 탭으로 바뀌는 정책 재현 |
| normal↔alternate | 미니맵 여백으로 실제 PTY 폭 변경 재현 |
| 스크롤 중 새 출력 | 독립 VTE에서 상단 logical line 유지; 단순 adjustment 숫자 변화만으로 오류 판정하지 않음 |
| minimap 클릭/drag/휠 | 좌표 변환·offset 코드 검토, 휠의 무제한 동기 refresh 확인; 실제 반복 drag 성능은 미측정 |
| CSI 3J, cursor-up repaint, wrap | 기존 minimap 회귀 테스트 통과; 새 임의 시퀀스의 완전성 증명은 아님 |
| 숨겨진 탭/워크스페이스 출력 | VTE scan과 pty-tee 중복 회피 및 unmapped minimap 취소 확인; 관련 기존 테스트 통과 |
| 선택 중 TUI 갱신·복사 | VTE selection 소실을 256KiB cache로 보완하는 코드 존재. 복사 보완이 화면 selection 유지까지 보장하지는 않음 |
| 검색 결과 이동 | 의도적으로 adjustment/selection을 바꾸는 경로 구분; 파괴적 전역 스크롤 보정 제안 안 함 |
| 출력 폭주·느린 소비·큰 paste | 출력→입력/resize starvation 재현; paste 반대 방향도 같은 pump 구조 |
| SSH/nested PTY | 로컬/remote resize 전달 경로 검토; 실제 네트워크 지연과 원격 IME는 별도 환경 필요 |
| 유휴로 보이는 상태 | cursor blink, working spinner, TUI spinner/title, 250ms hover preview를 실제 무출력과 구분 |
| 65초 실제 무입력 관찰 | 커서와 초기 footer 갱신 외 지속적인 전체 창 flash 미관측; 250ms sampling 한계 있음 |
| 주기 작업 | 250ms editor persistence, 1초 cwd, 2초 process poll, 15초 snapshot, 60초 malloc trim 경로 검토. cwd/process 읽기는 이미 worker 분리됨 |
| 확대/overview/preview | snapshot·reparent·animation 경로 존재. 무조건 queue_draw 제거/전체 widget 재생성은 개선안에서 제외 |
| 폰트·한글 wide cell·emoji·DPI | Pango/VTE font fallback과 reflow 경계 고려. 실제 fractional scaling/다중 모니터 이동은 미검증 |
| GPU/Wayland/XWayland | 현재 창 XWayland 확인, 실제 데스크톱 미니맵 부하 재현. compositor/driver 기인 전체 창 flash는 재현하지 못함 |
| macOS | 강제 Cairo 선택 코드 확인. macOS 실제 측정은 없으므로 Linux 결과를 전용하지 않음 |

현재 설치 창에서 사용자가 간헐적으로 경험한 모든 전체 화면 flash를 동일하게 포착했다고 주장하지 않는다. 특히 GPU damage/컴포지터, DPI 변경, 장시간 실행 후 상태, 실제 SSH/WSLg/Flatpak/macOS 조합은 위의 재현된 원인과 별도 검증이 필요하다. 이는 미니맵 비용·중간 프레임·focus 이벤트·geometry 변화·탭 선택·저장 범위·backpressure의 재현 사실과 구분한다.

**실행 순서와 완료 판정 제안**

1. 미니맵 주기/작업량 상한과 모드 전환 시 고정 geometry를 먼저 수정한다. 동일 워크로드 ON/OFF CPU, input latency, frame time p95/p99로 검증한다.
2. 동기화 출력 지원 backend를 별도 테스트 인스턴스에서 비교한다. 정지 프레임과 완료 프레임의 픽셀 증거로 검증한다.
3. IME focus cycle의 자식 TUI 이벤트 및 중복 UI refresh를 줄인다. 조합 표시·확정 순서·키 유실을 먼저 보존한다.
4. PTY pump의 backpressure 처리를 개선한다. flood 중 입력/resize가 출력 소비와 독립적으로 진행되는지 검사한다.
5. workspace 복귀 정책 및 실제 scrollback 저장 범위를 수정한다. terminal ID·logical line·저장 marker로 확인한다.
6. 남는 전체 창 flash는 별도 창에서 동일 출력으로 XWayland/Wayland 및 default/Cairo renderer를 비교한다. 사용자 창의 backend 설정을 즉석에서 바꾸거나 재시작하지 않는다.

회귀 확인은 CPU 수치뿐 아니라 key-to-echo/preedit latency, 프레임 시간, terminal rows/columns, 활성 pane/surface, viewport의 logical marker, child focus event, 저장 데이터 범위를 기록해야 한다. 현재 54개 관련 테스트는 통과했지만 이런 시간·픽셀·실제 IME 문제 전체를 검사하는 테스트는 아니다.

**검증·자료**

- 현재 소스 `cargo build -p flowmux -p flowmux-cli` 성공.
- Xvfb에서 `cargo test -p flowmux ui::terminal_minimap::tests -- --test-threads=1`: 13개 통과.
- Xvfb에서 `cargo test -p flowmux ui::ghostty_pane::tests -- --test-threads=1`: 41개 통과.
- 개발 빌드 A/B: `/tmp/fm-gui-2uj9b84x`, `/tmp/fm-gui-dg52tk9e`.
- 설치본 Xvfb A/B: `/tmp/fm-gui-kneukj84`, `/tmp/fm-gui-hgiw01su`.
- 설치본 실제 데스크톱 A/B: `/tmp/fm-gui-0n7rawqf`, `/tmp/fm-gui-eb0qcoy6`.
- IBus 동기/비동기/nav: `/tmp/fm-gui-b1knx_af`, `/tmp/fm-gui-b7clhun2`, `/tmp/fm-gui-30dqgtan`. 각 폴더의 `input.jsonl`, `phases.json`, PNG 참고.
- 레이아웃/저장: `/tmp/fm-gui-tg47i1ti`, `/tmp/fm-gui-fyr1a18g`의 `results.json`, screen text, state.json.
- VTE/PTY probe 및 실행 스크립트: `/tmp/flowmux-perf-20260921`. `sync-before.png`, `sync-during.png`, `sync-after.png`, `backpressure-results.json` 포함.
- 재실행은 그 폴더의 `run_app.py`, `ime_app.py`, `layout_app.py`, `idle_app.py`를 `uv run --with python-xlib --with pillow python <script>`로 실행한다. 기존 저장소의 `scripts/test-ssh-workspace-gui.py` Harness를 재사용한다. 스크립트의 보호 PID 6619와 바이너리 경로는 이 조사 환경 기준이다. `/tmp` 자료는 임시이며 주요 수치는 이 문서와 [증거 요약 JSON](performance-evidence-2026-09-21.json)에 보존한다.
