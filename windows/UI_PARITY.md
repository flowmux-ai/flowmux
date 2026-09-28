<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Windows GUI 구조 동등성 작업표

2026-09-28 소스 기준. 목표는 Linux에서 같은 기능을 찾고 실행하는 화면 구성과 흐름을 Windows에 제공하는 것이다. 색상·아이콘·사이드바 폭의 유사성만으로 GUI 동등성을 완료 처리하지 않는다. 현재 작은 chrome 개선 단계도 **부분 구현**이다.

[acceptance.json](acceptance.json)의 114개 기능은 **partial 60 / pending 54**이며 완료된 기능 행은 없다. 이 문서는 그 기능들을 사용자 화면 단위로 묶는 구현 지침이다. CLI/IPC 성공, 내부 서비스 구현, 화면 진입점, 실제 대화상자·패널 동작은 각각 확인해야 한다. 기존 수용 기준이나 과거 증거의 상태를 바꾸지 않는다.

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
| Options — T01, T07, T14–T15, T20, U09–U10, O06, O08 | [Options](../crates/flowmux/src/ui/options_dialog.rs#L84)는 소유 창에 연결된 **비모달**, 독립 이동 가능한 760×720 창. [General / Theme / Keybindings / Update](../crates/flowmux/src/ui/options_dialog.rs#L318) 탭, 스크롤 그룹과 Reset/Close | [settings_menu](src/native/appearance.rs#L103)의 긴 popup과 [단일 값 editor](src/native/metadata_panel.rs#L192). 설정 저장·반영 서비스는 있으나 화면 구성이 다름. 키 재설정·업데이트 전체 경로는 pending. **내부 기능 있음 + 표현 구조 차이** | 기존 settings_submit/result를 연결한 통합 Options 창부터 구현한다. 지원 항목을 General/Theme에 묶고 누락 기능은 명확히 기록한다. 단일 값 창을 계속 늘리지 않는다. |
| 알림 bell — T19, U14, A07 | [bell](../crates/flowmux/src/ui/sidebar.rs#L279)에 붙는 폭320 popover. [목록](../crates/flowmux/src/ui/sidebar.rs#L1188)은 높이160–420 스크롤, 최신순, 빈 상태, All Clear, 행별 삭제/원래 대상 열기. 표시용 snapshot을 만든 뒤 unread를 ack | [notification panel](src/native/notification_panel.rs#L109)은 850×560 overlapped 창과 list/detail/actions. 알림 store·명령은 있음. [Show 처리](src/native/notifications.rs#L220)는 ack 후 refresh여서 첫 열기의 unread 표현도 다르다. **표현 구조·상태 전이 차이** | bell에 붙는 소유 popup을 구현하고 unread 표시→ack→다시 열기, 삭제/전체 삭제/대상 복귀를 유지한다. toast·agent 상태 구현과 혼동하지 않는다. |
| 모든 터미널 검색 — T11 | [검색창](../crates/flowmux/src/ui/window/terminal_output_search.rs#L188)은 **모달** transient 760×520. query, Match case, 상태, 결과 목록, Show more | [search panel](src/native/search_panel.rs#L119)은 modeless overlapped 창. 검색·페이지·결과 활성화 서비스 있음. **표현 구조 차이** | Linux의 검색 대화상자 구조와 부모 소유·닫기·결과 이동 흐름을 맞춘다. sidebar dock으로 바꾸는 것은 Linux와 다른 재설계다. |
| Files — F07–F10, F17 | **주 창 workbench 전체 오른쪽의 단일 dock**: [window split](../crates/flowmux/src/ui/window/mod.rs#L1828). [source_pane 교체](../crates/flowmux/src/ui/window/file_browser.rs#L244)로 같은 panel을 재사용하고 pane별 tree 상태를 저장한다. 표시 폭320, 작은 header/tree/context menu | [Windows files_layout](src/native/files.rs#L1276)는 각 pane 안에 개별 panel을 배치해 pane 폭을 줄인다. [상시 controls](src/native/files.rs#L317)도 tree 위214 DIP를 차지한다. 목록·선택·열기와 제한된 copy/rename/move 있음; clipboard/batch/directory/delete/외부 앱은 미완. **P0: dock 소유·위치 차이 + 표현 구조 차이 + 기능 작업 필요** | window 오른쪽의 단일 Files panel로 맞추고 source pane 변경·상태 복원·Open 대상·닫기 복귀를 연결한다. 그 위에 작은 header와 context action/조건부 입력창을 배치한다. 취소·stale token·열린 editor 보호는 유지한다. |
| Editor — F01–F06, F17 | [editor pane](../crates/flowmux/src/ui/editor_pane.rs), Files에서 문서 열기, Monaco 편집·찾기, dirty/외부 변경/저장 흐름 | [Windows editor](src/native/editor.rs)·[search](src/native/editor_search.rs#L157)에 실제 Monaco와 bounded 작업 있음. Monaco 내부 UI를 일괄 재작성할 필요는 없다. Files/Open의 대상 소유권과 Open/Save As/dirty close/외부 변경/Quick Open 전체 GUI 동선은 별도 검증 대상. **내부 기능 있음; 진입점·대화상자 대응 확인 필요** | Files→문서→편집→저장/충돌→닫기를 한 흐름으로 연결한다. CLI command 통과를 GUI 버튼·단축키·확인창 완료로 대체하지 않는다. |
| Browser — B01–B11 | [browser chrome](../crates/flowmux/src/ui/browser_pane_webkit.rs#L280)은 주소와 탐색·bookmark·download controls가 있는 가로 chrome; 각 기능의 별도 popup/panel을 사용 | [Windows browser chrome](src/native/browser_chrome.rs#L145)은98-DIP, 탐색 버튼·주소·상태가 세 줄. WebView2 navigation/DOM/download/find/popup 서비스는 부분 구현. **내부 기능 있음 + 표현 구조 차이** | 한 줄 탐색 chrome과 주소 입력/로딩/오류, download/find 진입과 닫기를 함께 정리한다. bookmarks/profile/권한 같은 미완 기능은 따로 구현한다. |
| Workspace overview — U07 | [overview](../crates/flowmux/src/ui/window/workspace_overview.rs#L19)는 주 창 overlay; [footer 진입](../crates/flowmux/src/ui/sidebar.rs#L383) 제공 | 수용 기준 pending. 현재 workspace 메뉴나 tab 목록이 overview를 대신하지 못함. **기능 작업 필요** | 실 workspace와 pane 상태를 읽는 overview·선택·닫기 흐름을 구현한 뒤 footer/단축키를 연결한다. |
| Worktrees·Git — F11–F13 | [worktree panel](../crates/flowmux/src/ui/worktree_panel.rs#L55)과 [window routing](../crates/flowmux/src/ui/window/worktrees.rs), footer 진입, 목록/정보/제거 흐름 | 관련 Windows 수용 기준 모두 pending. Files tree나 cwd 표시로 Git/worktree 지원을 주장할 수 없음. **기능 작업 필요** | 조회와 소유권·안전/강제 제거 의미를 먼저 정하고 실제 panel을 연결한다. 가짜 branch/dirty/PR·worktree 행 금지. |
| Agents·세션·사용량 — A01–A06, A08–A14, U14 | [sidebar activity/footer](../crates/flowmux/src/ui/sidebar.rs#L360), 아래 Agents 영역과 사용량 popup, [session panel](../crates/flowmux/src/ui/session_panel.rs#L50)의 검색·목록·preview·resume | Windows 알림/출력 관찰만 부분 구현. agent 분류·세션 history/resume·usage 서비스는 pending. **기능 작업 필요** | agent lifecycle와 실 세션 제공자를 구현하는 별도 기능 묶음이다. 알림 count를 Agents 상태로 재표시하지 않는다. |
| 원격·viewer·시스템 기능 — S01–S08, F14–F16, B12–B14, O07–O11 | SSH 연결/인증·remote pane, image/Markdown viewer, 업데이트/설치 등 고유 흐름 | 수용 기준에 다수 pending 또는 부분 항목. 현재 로컬 terminal/browser/editor가 전체 기능을 대체하지 않음 | 각각의 서비스와 화면을 함께 구현한다. 이번 chrome 조정의 완료 범위에 포함시키지 않는다. |

U06의 tab 새 창 분리와 U11의 프로젝트 명령은 별도 pending 작업이다. U08의 popup/dialog/drag preview는 각 화면의 소유권·수명·복귀 흐름에 걸친 공통 수용 기준이며, 위의 작은 chrome 변경으로 충족되지 않는다.

Linux footer에는 Options, Agents bar, usage, overview, worktrees, Files, terminal search, agent sessions가 있다([실제 순서](../crates/flowmux/src/ui/sidebar.rs#L333)). Windows의 Settings/Files/Search/Open file 네 아이콘은 **구현된 일부 기능의 임시 진입 구성**이다. 수와 위치가 비슷해져도 같은 footer가 아니다.

## 다음 구현 순서와 완료 단위

1. **P0 Files→Editor 화면 구조**: 각 pane 내부 dock을 window 오른쪽의 단일 Files panel로 정리한다. source pane 교체·pane별 tree 상태·Open 대상·닫기 복귀를 먼저 맞춘 뒤 header/context action을 정돈한다. 기존 파일 작업 안전 조건·dirty close를 유지한다.
2. **P0 Options 화면**: 기존 설정 서비스를 재사용해 비모달 소유 창, 탭/그룹, 현재 값, 오류, 즉시 반영, Reset/Close를 완성한다. 단일 값 popup을 통합하며 Keybindings/Update의 미구현 경계는 유지한다.
3. **P0 알림 popup**: bell anchor, 가변 높이 목록, unread snapshot→ack, All Clear/행 삭제/대상 복귀를 한 번에 구현한다. Options와 독립 파일로 병렬 진행할 수 있다.
4. **Browser·검색 화면**: browser의 탐색 한 줄과 부속 popup, 터미널 검색의 모달 대화상자를 각각 맞춘다. 같은 모양의 공통 panel로 강제하지 않는다.
5. **주 창의 남은 화면**: workspace scroll/overview와 패널 전환·키보드 진입을 정리한다. Agents/Worktrees/세션/usage는 실제 기능 구현과 함께 별도 완료 단위로 추적한다.

각 단위는 최소한 `진입 위치 / 소유 창 / popup·modal·modeless·dock·overlay 종류 / 정상·빈·로딩·오류 상태 / 선택·변경·취소·닫기 / 원래 pane 복귀 / 실제 서비스 연결`을 명시하고 구현한다. I01–I12의 한글·IME, T01–T24의 터미널 동작, O01–O11의 CLI·수명·배포는 이 화면표와 교차하는 수용 기준이다. 이 문서의 화면 행 수를 114개 기능의 완료 수로 집계하지 않는다.

## 백그라운드 검증 범위

사용자가 다른 작업을 하므로 테스트는 소유한 숨김 HWND·분리 config/state·명시적인 pipe만 사용한다. 실제 UI 상태 변경은 기존 생산 handler로 전달하고, GUI와 서비스 결과를 함께 확인한다. 각 단계는 5초 CLI/조건 제한, 짧은 전체 예산, 종료·Job cleanup 증거를 둔다. 오류가 나면 같은 요청을 재전송하며 무기한 기다리지 않는다.

- 가능한 증거: 창/부모/스타일·controls·native captions·tooltip 값·geometry, 생산 renderer의 offscreen 결과, 직접 지정된 소유 HWND 메시지 처리, 실제 모델/파일/terminal/editor 결과와 안정적인 IDs/PIDs, 취소·닫기 이후 상태.
- 별도 미검증: 실제 popup activation·hover·외부 클릭 dismiss·키보드 focus 복귀, 물리 마우스/IME 후보·조합, high contrast, 접근성, per-monitor DPI 전환, GPU/WebView를 합성한 전체 화면. HWND 정보나 숨김 GDI 그림으로 이 항목을 통과시키지 않는다.
- NFC 표시용 복사본은 원문 모델·HWND caption·파일 경로·입력·저장 codepoint와 구별한다. 모양이 같다는 사실은 실제 IME 조합이나 원문 정규화를 허용하는 근거가 아니다.
- 완료 보고에는 해당 화면의 **실제 흐름 검증과 남은 물리 입력 경계**를 함께 적는다. 단위 테스트·빌드·패키지 생성만으로 GUI 동등성을 완료 처리하지 않는다.
