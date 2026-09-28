<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Windows GUI 구조 동등성 작업표

2026-09-28 소스 기준. 목표는 Linux에서 같은 기능을 찾고 실행하는 화면 구성과 흐름을 Windows에 제공하는 것이다. 색상·아이콘·사이드바 폭의 유사성만으로 GUI 동등성을 완료 처리하지 않는다. 현재 화면 구조 변경은 **부분 구현**이다. Options·알림·Files·Browser의 첫 구조 변경은 숨김 Windows 런타임 검증을 통과했다. 각 화면의 검증 범위는 아래 기록을 따르며, 이전 chrome 검증을 새로운 popup·창·dock 검증으로 재사용하지 않는다. 검색 모달·pane header/menu·workspace overview도 **숨김 Windows 런타임 부분 검증을 통과**했다. [이번 검증 기록](evidence/2026-09-28/ui-parity-next.md)의 바이너리별 범위와 미검증 사항을 따른다.

[acceptance.json](acceptance.json)의 114개 기능은 **partial 61 / pending 53**이며 완료된 기능 행은 없다. 이 문서는 그 기능들을 사용자 화면 단위로 묶는 구현 지침이다. CLI/IPC 성공, 내부 서비스 구현, 화면 진입점, 실제 대화상자·패널 동작은 각각 확인해야 한다. 기존 수용 기준이나 과거 증거의 상태를 바꾸지 않는다.

## 판단 기준

- **내부 기능 있음**: 기존 Windows 서비스·명령을 재사용할 수 있다. 전체 기능의 완료를 뜻하지 않는다.
- **진입점 누락**: 기능을 실행할 서비스가 있어도 Linux와 대응되는 메뉴·버튼·단축키·문서 흐름이 제공되지 않는다.
- **표현 구조 차이**: 진입점은 있지만 popup/window/docked panel, 소유 창, modality, 정보 순서 또는 조작 흐름이 다르다.
- **기능 작업 필요**: Windows의 지원되는 전체 기능 경로가 아직 없다. 동작하지 않는 Agents/Worktree 버튼이나 가짜 데이터로 화면을 채우지 않는다.

Windows의 HWND/WebView2 구현 방식은 유지할 수 있다. 같은 플랫폼 위젯을 쓰는 것이 아니라 **같은 기능 진입 → 같은 종류의 화면 → 같은 상태 변화 → 같은 복귀 흐름**을 구현해야 한다. 창 장식·글꼴 raster의 플랫폼 차이는 별도로 기록한다.

## 화면별 대조

| 화면 / 수용 기준 | Linux의 실제 구성 | 현재 Windows와 구분 | 다음 구현 단위 |
|---|---|---|---|
| 주 창·workspace 목록 — U01–U05, U12–U14 | [window shell](../crates/flowmux/src/ui/window/mod.rs#L749)의 두 ToolbarView와 가로 Paned, [sidebar](../crates/flowmux/src/ui/sidebar.rs#L272)의 + / Workspaces / bell, 스크롤 목록, 아래 Agents 영역, footer | [host](src/native/host.rs#L729)·[workspace layout](src/native/workspaces.rs#L37)에 이름/cwd, 선택·색, 폭 조절과 작은 footer가 있다. Agents 영역과 footer 기능 구성이 다르고 Previous/Next는 Linux 스크롤 목록과 다른 조작이다. **표현 구조 차이 + 기능 작업 필요** | 목록 스크롤·선택 가시성·pane 소유권을 하나의 주 창 시나리오로 맞춘다. 폭·아이콘 변경만으로 완료하지 않는다. |
| Pane header·메뉴 — U04–U05, U08 | [pane toolbar](../crates/flowmux/src/ui/workspace_view.rs#L1582)는 zoom·오른쪽 split·아래 split·tab 추가·browser·pane 메뉴 순서이며, [pane 메뉴](../crates/flowmux/src/ui/workspace_view.rs#L3503)와 tab 메뉴를 구분한다. | [header layout](src/native/workspaces.rs#L42)에 같은 도구 순서를 반영하고 좁은 폭에서 표시할 수 있는 도구만 배치한다. [pane 메뉴](src/native/workspaces.rs#L272)는 Close Pane을 제공하고 마지막 pane에서는 비활성화하며, 대상 pane/surface 소유권과 기존 dirty close 보호를 유지한다. **숨김 native 런타임 검증 통과(부분 범위)**. | 여러 pane에서 각 도구의 실제 대상·좁은 폭 배치·pane/tab 메뉴 분리·마지막 pane 및 unsaved editor 보호를 검증한다. 실제 키보드·DPI·접근성 완료를 뜻하지 않는다. |
| Options — T01, T07, T14–T15, T20, U09–U10, O06, O08 | [Options](../crates/flowmux/src/ui/options_dialog.rs#L84)는 소유 창에 연결된 **비모달**, 독립 이동 가능한 760×720 창. [General / Theme / Keybindings / Update](../crates/flowmux/src/ui/options_dialog.rs#L318) 탭, 스크롤 그룹과 Reset/Close | [새 Options 창](src/native/options_panel.rs#L142)이 비모달 소유 창·760×720 기본 크기·General/Theme 페이지·현재 값·Apply·Reset/Reload/Close를 제공하고 [기존 settings 서비스](src/native/appearance.rs#L110)를 호출한다. **숨김 native 런타임 검증 통과(부분 범위)**. General의 글꼴/scrollback/cursor/minimap/default shell과 Theme의 dark/light가 대상이다. | Keybindings/Update 탭, Linux Theme의 전체 편집 기능, 일반 설정 범위가 아직 부족하다. 행별 Apply는 Linux의 즉시 반영과 다른 흐름이다. 실제 저장·동시 변경·오류·페이지 전환·닫기·한글 조합 경계를 검증하고 차이를 남긴다. |
| 알림 bell — T19, U14, A07 | [bell](../crates/flowmux/src/ui/sidebar.rs#L279)에 붙는 폭320 popover. [목록](../crates/flowmux/src/ui/sidebar.rs#L1188)은 높이160–420 스크롤, 최신순, 빈 상태, All Clear, 행별 삭제/원래 대상 열기. 표시용 snapshot을 만든 뒤 unread를 ack | [native popover](src/native/notification_panel.rs#L267)는 독립 작업창을 320-DIP 소유 popup으로 바꾸고, bell anchor·bounded 스크롤·제목/본문/시간 행·행별 Open/Delete·All Clear·빈 상태를 구현했다. [Show](src/native/notifications.rs#L219)는 unread 표시 snapshot을 만든 다음 store를 ack한다. **숨김 native 런타임 검증 통과(부분 범위)**. | 숨김 소유 HWND에서 실제 스타일/소유권/좌표·원문 caption·행 삭제·전체 삭제·닫기/재열기·대상 이동을 확인한다. 실제 hover·외부 클릭·키보드 focus 복귀·DPI·접근성은 별도 검증이며, toast/agent 상태 전체 지원은 아니다. |
| 모든 터미널 검색 — T11 | [검색창](../crates/flowmux/src/ui/window/terminal_output_search.rs#L188)은 **모달** transient 760×520. query, Match case, 상태, 결과 목록, Show more | [search panel](src/native/search_panel.rs#L255)을 760×520-DIP 소유 모달 창으로 변경했다. query·Match case·Refresh·상태·위치/미리보기 두 줄 목록·Show more를 제공하며, 닫기/결과 이동 시 자신이 비활성화한 owner를 복구한다. [검색 서비스](src/native/search.rs#L12)는 CLI의 500개/offset 계약을 유지하고 UI만 500→최대700개로 다시 조회한다. **숨김 native 런타임 검증 통과(부분 범위)**. | 700개 제한을 상태에 표시하고 추가 결과는 query를 좁히도록 안내한다. 결과 열기는 아직 더블클릭이며, 미리보기는 Linux monospace 대신 공용 body 글꼴이다. 모달 owner 복구·원문 query·Show more/선택·빈/오류 상태를 숨김 native 검증으로 확인하고, 물리 IME는 별도 검증한다. |
| Files — F07–F10, F17 | **주 창 workbench 전체 오른쪽의 단일 dock**: [window split](../crates/flowmux/src/ui/window/mod.rs#L1828). [source_pane 교체](../crates/flowmux/src/ui/window/file_browser.rs#L244)로 같은 panel을 재사용하고 pane별 tree 상태를 저장한다. 표시 폭320, 작은 header/tree/context menu | [window layout](src/native/host.rs#L943)과 [Files active-source layout](src/native/files.rs#L1604)이 주 창 오른쪽에 한 개의 보이는 dock을 배치하고, source pane별 상태는 bounded cache에 유지한다. 기본 폭320 DIP, 작은 header/path와 Actions 메뉴, 필요한 작업에서만 destination form을 노출한다. **숨김 native 런타임 검증 통과(부분 범위)**. | 여러 pane/workspace에서 source 교체·상태 복원·오른쪽 전체 높이·Open 대상·닫기 복귀를 검증한다. 제한된 단일 파일 copy/rename/move의 기존 안전 조건을 유지한다. Clipboard/batch/directory/delete/외부 앱 미구현과 목록/트리 표현 차이는 남아 있다. |
| Editor — F01–F06, F17 | [editor pane](../crates/flowmux/src/ui/editor_pane.rs), Files에서 문서 열기, Monaco 편집·찾기, dirty/외부 변경/저장 흐름 | [Windows editor](src/native/editor.rs)·[search](src/native/editor_search.rs#L157)에 실제 Monaco와 bounded 작업 있음. Monaco 내부 UI를 일괄 재작성할 필요는 없다. Files/Open의 대상 소유권과 Open/Save As/dirty close/외부 변경/Quick Open 전체 GUI 동선은 별도 검증 대상. **내부 기능 있음; 진입점·대화상자 대응 확인 필요** | Files→문서→편집→저장/충돌→닫기를 한 흐름으로 연결한다. CLI command 통과를 GUI 버튼·단축키·확인창 완료로 대체하지 않는다. |
| Browser — B01–B11 | [browser chrome](../crates/flowmux/src/ui/browser_pane_webkit.rs#L280)은 주소와 탐색·bookmark·download controls가 있는 가로 chrome; 각 기능의 별도 popup/panel을 사용 | [Windows browser chrome](src/native/browser_chrome.rs#L170)은 세 줄98 DIP에서 **한 줄40 DIP**로 변경되었으며 주소·탐색·reload/stop·More 진입을 재배치했다. 기존 WebView2 navigation/DOM/download/find/popup 서비스를 유지한다. **숨김 native 런타임 검증 통과(부분 범위)**. | 좁은 pane의 overflow와 주소 입력·로딩·오류, find/download 진입 및 닫기를 실제 WebView와 함께 검증한다. Bookmarks/profile/권한 등 미완 기능과 부속 popup의 Linux 구성 차이는 별도 작업이다. |
| Workspace overview — U07 | [overview](../crates/flowmux/src/ui/window/workspace_overview.rs#L19)는 주 창 overlay; [footer 진입](../crates/flowmux/src/ui/sidebar.rs#L383) 제공 | [native overview](src/native/overview.rs#L559)는 주 창 내부 overlay에 실제 workspace 카드·pane 배치·소유 WebView2의 CapturePreview를 표시하고 선택/닫기를 기존 경로에 연결한다. [footer](src/native/host.rs#L747)와 [터미널 Ctrl+Alt+K](terminal/src/pane-shortcuts.mjs#L13) 진입이 추가되었다. **숨김 native 런타임 검증 통과; U07 partial**. | 활성/준비 완료 tab만 실제 캡처한다. 비활성 workspace 썸네일과 이전 화면 캐시는 미구현이며, 숨김 터미널 PNG에서는 배경·커서만 확인되어 글자 raster를 승인하지 않았다. 선택/닫기·미저장 보호·제한 시간·클리핑 흐름은 숨김 native에서 확인했다. Ctrl+Alt+K는 현재 터미널에만 연결되어 browser/editor/native 입력에서 같은 단축키를 보장하지 않는다. 물리 키보드·IME·focus와 실제 화면 합성은 미검증이다. |
| Worktrees·Git — F11–F13 | [worktree panel](../crates/flowmux/src/ui/worktree_panel.rs#L55)과 [window routing](../crates/flowmux/src/ui/window/worktrees.rs), footer 진입, 목록/정보/제거 흐름 | 관련 Windows 수용 기준 모두 pending. Files tree나 cwd 표시로 Git/worktree 지원을 주장할 수 없음. **기능 작업 필요** | 조회와 소유권·안전/강제 제거 의미를 먼저 정하고 실제 panel을 연결한다. 가짜 branch/dirty/PR·worktree 행 금지. |
| Agents·세션·사용량 — A01–A06, A08–A14, U14 | [sidebar activity/footer](../crates/flowmux/src/ui/sidebar.rs#L360), 아래 Agents 영역과 사용량 popup, [session panel](../crates/flowmux/src/ui/session_panel.rs#L50)의 검색·목록·preview·resume | Windows 알림/출력 관찰만 부분 구현. agent 분류·세션 history/resume·usage 서비스는 pending. **기능 작업 필요** | agent lifecycle와 실 세션 제공자를 구현하는 별도 기능 묶음이다. 알림 count를 Agents 상태로 재표시하지 않는다. |
| 원격·viewer·시스템 기능 — S01–S08, F14–F16, B12–B14, O07–O11 | SSH 연결/인증·remote pane, image/Markdown viewer, 업데이트/설치 등 고유 흐름 | 수용 기준에 다수 pending 또는 부분 항목. 현재 로컬 terminal/browser/editor가 전체 기능을 대체하지 않음 | 각각의 서비스와 화면을 함께 구현한다. 이번 chrome 조정의 완료 범위에 포함시키지 않는다. |

U06의 tab 새 창 분리와 U11의 프로젝트 명령은 별도 pending 작업이다. U08의 popup/dialog/drag preview는 각 화면의 소유권·수명·복귀 흐름에 걸친 공통 수용 기준이며, 위의 작은 chrome 변경으로 충족되지 않는다.

Linux footer에는 Options, Agents bar, usage, overview, worktrees, Files, terminal search, agent sessions가 있다([실제 순서](../crates/flowmux/src/ui/sidebar.rs#L333)). Windows의 Settings/Files/Search/Open file/Workspace overview 다섯 진입점은 **구현된 일부 기능의 임시 진입 구성**이다. 새 overview 진입은 숨김 native 검증을 통과했지만 전체 footer 동등성은 미완이다. 수와 위치가 비슷해져도 같은 footer가 아니다.

## 현재 구조 변경과 다음 완료 단위

1. **Files→Editor**: 주 창 오른쪽의 단일 표시 dock과 source pane 상태 분리는 소스에 반영되었다. 다음 완료 단위는 여러 pane/workspace에서 source 교체·상태 복원·Open 대상·닫기 복귀를 실제로 확인하는 것이다. Header/context action 변경이 기존 파일 작업 안전 조건·dirty close를 우회하지 않아야 한다.
2. **Options**: 비모달 소유 창과 General/Theme의 지원 설정을 기존 저장 서비스에 연결했다. 현재는 행별 Apply 방식이며, Linux와 같은 전체 General/Theme 범위·Keybindings/Update·그룹/즉시 반영 구성은 완료되지 않았다. 페이지·원문 값·저장 오류·동시 변경·Reset/Reload/Close 동작 검증이 먼저다.
3. **알림 popup**: bell anchor, 제목/본문/시간 행, unread snapshot→ack, All Clear/행 삭제/대상 열기의 첫 구현이 반영되었다. 숨김 native 검증에서 geometry·실제 HWND caption·store 상태·닫기/재열기를 함께 확인한다. 읽지 않은 snapshot을 실제 IME/마우스 검증과 혼동하지 않는다.
4. **Browser·검색**: browser 탐색 한 줄은 소스에 반영되었다. 작은 pane·주소/로딩·부속 popup 회귀 검증을 진행한다. 모든 터미널 검색은 모달 transient 구조·두 줄 결과·Show more를 구현했고 숨김 런타임 검증을 통과했다. UI 700개 제한 안내, 더블클릭 활성화, 미리보기 글꼴 차이는 위 검색 행에 남긴다.
5. **주 창의 남은 화면**: pane 도구 순서·pane 메뉴 분리와 실제 workspace overview overlay의 첫 구현을 반영했다. 이번 숨김 검증에 따라 U07만 pending에서 partial로 변경했으며 완료된 기능은 없다. Workspace 목록 스크롤·패널 전환·터미널 밖 overview 단축키·키보드 복귀는 계속 확인해야 한다. Agents/Worktrees/세션/usage와 전체 Settings 범위는 실제 기능 구현과 함께 별도 완료 단위로 추적한다.

이번 단계는 기존 Rust 서비스·Win32 controls·WebView2 host를 재사용한다. GTK 전환이나 전체 앱 재작성 없이 **화면의 소유권·배치·기능 연결**부터 맞춘다. Native control의 구조 변경, HTML 패널 도입, 미구현 기능 추가는 같은 작업으로 간주하지 않는다.

각 단위는 최소한 `진입 위치 / 소유 창 / popup·modal·modeless·dock·overlay 종류 / 정상·빈·로딩·오류 상태 / 선택·변경·취소·닫기 / 원래 pane 복귀 / 실제 서비스 연결`을 명시하고 구현한다. I01–I12의 한글·IME, T01–T24의 터미널 동작, O01–O11의 CLI·수명·배포는 이 화면표와 교차하는 수용 기준이다. 이 문서의 화면 행 수를 114개 기능의 완료 수로 집계하지 않는다.

## 백그라운드 검증 범위

사용자가 다른 작업을 하므로 테스트는 소유한 숨김 HWND·분리 config/state·명시적인 pipe만 사용한다. 실제 UI 상태 변경은 기존 생산 handler로 전달하고, GUI와 서비스 결과를 함께 확인한다. CLI는 5초, 알림 startup/출력/종료 조건은 8초로 제한하고, 짧은 전체 예산과 종료·Job cleanup 증거를 둔다. 무기한 WaitForExit를 사용하지 않는다. 오류가 나면 같은 요청을 재전송하며 무기한 기다리지 않는다.

- 가능한 증거: 창/부모/스타일·controls·native captions·tooltip 값·geometry, 생산 renderer의 offscreen 결과, 직접 지정된 소유 HWND 메시지 처리, 실제 모델/파일/terminal/editor 결과와 안정적인 IDs/PIDs, 취소·닫기 이후 상태.
- 별도 미검증: 실제 popup activation·hover·외부 클릭 dismiss·키보드 focus 복귀, 물리 마우스/IME 후보·조합, high contrast, 접근성, per-monitor DPI 전환, GPU/WebView를 합성한 전체 화면. HWND 정보나 숨김 GDI 그림으로 이 항목을 통과시키지 않는다.
- NFC 표시용 복사본은 원문 모델·HWND caption·파일 경로·입력·저장 codepoint와 구별한다. 모양이 같다는 사실은 실제 IME 조합이나 원문 정규화를 허용하는 근거가 아니다.
- 완료 보고에는 해당 화면의 **실제 흐름 검증과 남은 물리 입력 경계**를 함께 적는다. 단위 테스트·빌드·패키지 생성만으로 GUI 동등성을 완료 처리하지 않는다.

이번 구조 변경의 숨김 검증은 Options 5개, Files dock 2개, 알림 9개, browser 8개 및 기존 파일 작업 회귀 5개 묶음이 통과했다. [검증 기록](evidence/2026-09-28/ui-structure.md)의 실행 파일·소스 범위와 물리 입력 제한을 따른다. 이는 Linux 전체 외형·기능 동등성 완료를 뜻하지 않는다.

검색 4개, pane 도구/전체 닫기 2개, overview 흐름 5개/dirty 보호 2개/browser 픽셀 2개, chrome 회귀 5개로 총 20개의 고유 검증 묶음이 통과했다. 캡처 성공을 물리 화면·터미널 글자 렌더링 검증으로 간주하지 않는다. 상세 source/staging 범위와 남은 제한은 [이번 기록](evidence/2026-09-28/ui-parity-next.md)을 따른다.
브라우저 카드 안에서는 fixture의 실제 색상 픽셀을 검증했다. 후속 혼합 화면에는 터미널 글자도 보였지만 첫 캡처의 갱신 시점과 전체 glyph 보존은 아직 미검증이다.

마지막 코드 검토에서 발견한 pane 메뉴 닫기·overview 카드 재구성 후 focus 복귀 경로도 보완했다. 관련 7개 묶음을 다시 확인했으며, 기존 20개에 중복 합산하지 않는다. [최종 보완 기록](evidence/2026-09-28/ui-parity-focus.md)은 최신 바이너리와 원래 검증 단계의 경계를 구분한다. 실제 키보드 focus 복귀는 백그라운드 제약상 계속 미검증이다.
