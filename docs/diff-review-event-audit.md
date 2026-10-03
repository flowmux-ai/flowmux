<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Diff view 이벤트 점검 — 2026-10-03

대상은 pane 내부 Diff 구현의 이벤트 진입점과 주요 정상/실패 상태 전이이다.
임의의 이벤트 순서, OS 입력기, 모든 외부 agent 버전까지 완전 검증했다는 의미는 아니다.

## 발견 및 수정

1. 댓글 입력칸을 클릭하면 바깥 읽기 전용 TextView가 포커스를 가져갔다.
   수정 전에는 클릭 후 `AFTERCLICK`이 입력되지 않았고, 수정 후에는 같은 위치를
   클릭해 입력·수정·저장할 수 있었다. 카드 내부에서 처리한 포인터 이벤트가
   바깥 Diff로 전파되지 않도록 했다. 스크롤과 키보드 전파는 유지한다.
   GTK 기본 클릭 처리의 `grab_focus`도 [GTK 원본](https://github.com/GNOME/gtk/blob/main/gtk/gtktextview.c)에서 확인했다.
2. 다른 파일로 이동한 뒤 Comment/Edit/Send를 누르면 숨겨진 입력칸에 포커스를
   주려고 했다. 이제 작성 중인 댓글의 파일로 돌아가 텍스트를 보존한다.
3. 파일 목록을 교체하는 중간 선택 변경이 댓글 이동 요청을 소비할 수 있었다.
   최종 선택에서만 로드하고, 없는 파일의 댓글을 중복 로드해 지우지 않도록 했다.
4. 댓글 카드를 다시 그릴 때 입력 포커스가 사라졌다. 같은 composer의 포커스를
   보존하고, pane에 다시 포커스를 줄 때도 내부의 현재 입력 위치를 존중한다.
5. 유효한 코드 선택 없이 Reattach를 누르면 저장할 대상 없이 텍스트만 남았다.
   연결에 성공한 경우에만 편집 상태로 전환한다. 저장 후 오래된 경고도 제거한다.
6. 초안 때문에 Send가 거부될 때 메뉴가 남았다. 먼저 메뉴를 닫고 초안으로 이동한다.
7. 최초 저장소 읽기에 실패하면 재시도 항목이 없었다. 초기 상태부터 재시도를
   제공한다. 저장 중 새로운 편집 진입도 막아 완료 콜백이 다른 초안을 지우지 않게 했다.

## 경로별 결과

- **GUI**: 별도 macOS 앱에서 실제 포인터/키보드 입력으로 확인.
- **Native**: 실행 중인 GTK 위젯, 콜백, 비동기 작업, 실제 Git/SQLite/PTY로 확인.
  버튼 signal 호출은 물리적인 마우스 전달 검증과 구분한다.
- **Model**: Git/댓글/저장소 테스트로 확인.
- **검토**: 코드 분기를 확인했으나 해당 실패/입력 조건을 직접 발생시키지는 못함.

| 이벤트/상태 | 확인 | 결과 또는 한계 |
|---|---|---|
| pane `… → View Diff` | GUI + Native | 메뉴 열기 후 Enter로 진입, 소유 pane 전달 확인. 항목 마우스 선택은 아래 한계 참조 |
| Ctrl+Alt+D | GUI | 현재 pane 내부에서 열림 |
| split의 다른 pane/root | Native | 실제 terminal cd/OSC7, pane별 파일·호스트 분리 |
| root 변경 중 미저장 댓글 | Native | 기존 초안 보존, 취소 후 새 root 열림 |
| SSH pane / Git root 탐색 실패 | 검토 + Model | SSH 진입 차단, 비저장소 탐색 오류 처리 |
| HEAD 대비 staged/unstaged/untracked | Model | 한 파일의 순 변경, 커밋 후 목록 갱신 포함 |
| 빈 저장소/첫 커밋 전/변경 없음 | Model + Native | 빈 비교와 새 파일 처리 |
| rename/delete/binary/symlink/특수 경로 | Model | 원래 경로 보존, symlink 외부 파일을 따라가지 않음 |
| 검색/선택/다른 파일에서 댓글 이동 | Native + GUI | 최종 선택과 원래 댓글 파일 유지 |
| 연속 파일 선택·비동기 완료 역전 | Native | 마지막 선택의 patch 유지 |
| Refresh / 다시 열기 | Native + GUI | 현재 상태 재조회, 저장 댓글 유지 |
| Git timeout/worker 실패의 UI 표시 | 검토 | 오류 표시·재시도 분기 있음. 실제 timeout/worker panic 주입은 미실시 |
| 긴 Diff/스크롤/inline 카드 배치 | Native + GUI | 20,000줄 연속 렌더링, 첫/마지막 줄 및 inline 위치 |
| n/p 이전·다음 hunk / 양 끝 | Native | 다음/이전 hunk 및 끝에서 위치 유지 |
| 줄 옆 + / 범위 댓글 / File comment | GUI + Native | 코드 위치 또는 파일 전체 anchor 생성 |
| c 댓글 단축키 | 검토 | 영문 c + modifier 없음 분기. 현재 한글 입력 상태에서 도구가 보낸 키는 Hangul_Cieuc로 관측되어 영문 단축키 실기 검증과 구분 |
| Tab/Shift+Tab, 일반 선택·붙여넣기 | 검토 + GUI 일부 | GTK 기본 처리. 클릭 커서 이동·문자 입력은 확인, 모든 조합과 실제 IME 조합은 미검증 |
| 잘못된 코드 선택 / 빈 댓글 저장 | Native + GUI | 상태 안내, 저장 및 고아 초안 생성 방지 |
| 입력칸 클릭 후 입력·커서 이동 | GUI | 수정 전 재현, 수정 후 실제 입력·저장 확인 |
| 댓글 카드의 Edit/Cancel/Save 클릭 | GUI + Native | 저장과 취소, 재편집 반영 |
| Ctrl+Enter 저장 | GUI | 수정 내용 저장 후 composer 닫힘 |
| Cmd+Enter 저장 | 검토 | 동일 처리 분기, 이번 실기 입력은 Ctrl+Enter만 수행 |
| Escape: 메뉴 → 초안 → Diff 닫기 | GUI + Native | 단계별 닫힘, 메뉴만 닫을 때 초안 유지 |
| Back/숨김/재진입 | Native + GUI | 기존 terminal 표면과 프로세스 보존 |
| 다른 파일에서 새 댓글 요청 | Native + GUI | 기존 작성 중 댓글로 돌아가 계속 입력 |
| redraw 중 composer 포커스 | Native | 위젯 재부착 뒤 window focus 유지 |
| 저장 중 편집/취소/재전송 | 검토 + Native | busy guard, 입력 잠금, 중복 작업 방지 |
| 저장 충돌 → reload → 재저장 | Native + Model | 실제 SQLite revision 충돌, 텍스트 보존 후 복구 |
| 최초 저장소 읽기 실패 → 재시도 | Native | DB 경로를 열 수 없게 만든 뒤 복구, 재시도 버튼 활성 |
| Delete (Resolve/Reopen 대체) | GUI + Native | 저장된 댓글 제거, 카운트 갱신, 다시 불러와도 삭제 유지, 다른 초안 보존 |
| 코드 이동 / 중복 문맥 / 파일 소멸 | Native + Model | 위치 재탐색, 불확실하면 보내기 차단, 사라진 파일 댓글 접근 |
| Reattach 실패 / 성공 / 재저장 | Native | 무효 선택은 무변경, 유효 선택은 동일 댓글 갱신·경고 해제 |
| Comments 메뉴 열기/닫기/댓글 이동/reload | GUI + Native | 표시·Escape 및 콜백 검증. 항목 마우스 전달은 미확정 |
| Send 메뉴 / agent 목록 갱신 | GUI + Native + 검토 | 표시, 동일 목록 재생성 방지, live target 재검사 |
| 미저장 상태 Send | Native | 메뉴 닫힘, 초안 유지와 포커스 복귀 |
| Copy feedback | GUI + Native | Enter로 실행, 실제 clipboard 내용과 결과 상태 확인 |
| idle/done agent로 전달 | Native | 실제 자식 PTY가 bracketed paste와 Enter를 받음 |
| busy/approval/종료/다른 session/기존 입력 | Native | 전송 거부, 기존 terminal 입력 보존 |
| bridge 연결 실패 / target terminal 없음 | 검토 | 안내 분기 있음. 큐 포화 등 장애 주입은 미실시 |
| pane/창 닫기와 미저장 댓글 | Native | 초안 보호, 닫힌 pane의 review 정리 |
| 모든 OS 입력기/터치/외부 agent 버전 | 미검증 | Unicode 내용 검증과 실제 IME 조합 입력은 별개 |

## 실행 증거와 남은 항목

격리 앱: `/tmp/flowmux-diff-events/FlowMux.app` (`com.flowmux.DiffEvents`).
기존 설치 앱과 사용 중인 ReviewUX 앱은 종료하거나 교체하지 않았다.

- `cargo test -p flowmux-vcs --test review --locked`: 13개 통과.
- `cargo test -p flowmux-state review_drafts --locked`: 1개 저장소 회귀 테스트 통과.
- macOS native suite: `MACOS_NATIVE_SMOKE_OK` 및
  `DIFF_REVIEW_DRAFT_NAVIGATION_EDIT_DELETE_CONFLICT_OK`, pane/root 분리와 PTY 전달 검사.
- 로그: `/tmp/flowmux-diff-events/{native,vcs,state,clippy,build}.log`.
- 이번 Linux GUI 재실행은 Docker socket 부재로 실행하지 못했다.

**팝오버 마우스 선택은 미확정이다.** 자동화에서 Send의 Copy feedback을 좌표로
클릭하면 메뉴만 닫히고 실행되지 않았다. 같은 항목을 Enter로 선택하면 정상 실행된다.
GTK 내부 위젯이 접근성 트리에 노출되지 않고 팝오버를 별도 입력 대상으로 지정할 수
없어, 도구의 입력 전달 문제와 앱의 포인터 문제를 아직 구분하지 못했다.
이 경로를 통과로 표시하지 않는다. 입력칸·카드 버튼의 마우스 검증과는 별도이다.

새 분기나 회귀를 추가할 때 이 표와 `comments::event_smoke`를 함께 갱신한다.

## 후속 변경: 댓글 삭제

Resolve/Reopen을 Delete로 대체했다. `/tmp/flowmux-review-delete/FlowMux.app`에서
댓글 저장 후 Delete를 마우스로 클릭하여 카드 제거, Comments/Send 카운트 0,
`Comment deleted` 상태를 확인했다. 네이티브 검증도 통과했으며, 삭제 후 재로드와
다른 작성 중 초안 보존을 검사한다. 로그: `/tmp/flowmux-review-delete-native.log`.

## Send 팝업 빈 공간 클릭 후 입력 불가 추가 조사 (2026-10-03)

- 확인한 별도 결함: `refresh_review_targets`가 상태 문자열 변경을 반영할 때 열린 팝업의 버튼을 전부 제거했다. GTK 네이티브 회귀 검사에서 포커스된 Send 버튼이 부모에서 분리되는 실패를 먼저 확인했다 (`/tmp/flowmux-popup-before.log`).
- 수정: Send 팝업이 보이는 동안 목록 교체를 보류하고, `closed`에서 최신 대상 목록 갱신을 요청한다. 실제 전송 시 세션/프로세스/상태 검증은 기존대로 수행한다.
- 검사: `DIFF_REVIEW_SEND_REFRESH_FOCUS_OK`를 추가했다. 버튼과 root focus 유지, 닫힌 뒤 목록 갱신을 검사하며 기존 전체 Diff 네이티브 smoke도 통과했다 (`/tmp/flowmux-popup-after.log`).
- 별도 앱 `com.flowmux.PopupRepro`에서 가짜 에이전트 상태를 IPC로 변경했다. 열린 목록이 유지되고 Tab/Enter로 `Copy feedback`을 실행해 `Feedback copied`를 확인했다. 다시 열면 최신 상태가 표시됐다. 실제 사용자 에이전트에는 전송하지 않았다.
- **원래 보고한 마우스 먹통 경로는 미확정이다.** 자동화로 팝업 버튼 사이를 클릭하면 팝업 내부 pointer 이벤트 없이 `unmap/closed`가 먼저 발생했다. 이후 일반 Edit 버튼은 동작했다. 따라서 이 클릭 경로는 사용자가 보고한 상태를 재현하지 못했으며, 위 수정으로 원래 증상이 해결됐다고 단정하지 않는다. 임시 이벤트 계측 코드는 제거했다. 사용 중인 앱은 재시작하거나 교체하지 않았다.

## 코멘트 집계와 팝업 기준점 수정 (2026-10-03)

- 사용자 저장 DB를 읽기 전용으로 확인한 결과 전체 5개 중 legacy `resolved=true`가 2개였다. 목록은 전체를 렌더링하지만 숫자는 3개의 unresolved 코멘트만 세고 있었다. 사용자 DB는 직접 변경하지 않았다.
- 저장 코멘트를 읽을 때 이전 Resolve 상태를 일반 코멘트로 정규화한다. 카운터는 목록과 동일한 `notes.len()`을 사용한다. 삭제하기 전까지 내용·목록·전송 대상으로 유지된다.
- 5개 중 2개가 resolved인 네이티브 fixture에서 수정 전 `Comments · 3` 실패를 확인했다 (`/tmp/flowmux-count-before.log`). 수정 후 목록 5개, 카운터 5, inline 카드 5개와 feedback 5개 포함을 검증했다.
- Comments/Send 메뉴의 수동 `anchor_at_click`을 제거했다. 버튼 자체를 화살표 기준으로 유지하고 팝업 본문은 `Align::End`로 정렬한다. 별도의 재열기 핸들러 없이 GtkMenuButton의 기본 토글을 유지한다.
- 별도 실행 앱 `com.flowmux.PopupRepro`에서 5개 목록과 `Comments · 5`/`Send · 5`를 확인했다. Send/Comments 각각 동일 좌표를 세 번 클릭하여 열림 → 닫힘 → 재열림과 버튼 아래 화살표 정렬을 확인했다. 이 검증은 버튼 재클릭 경로이며, 앞 절의 팝업 내부 빈 공간 클릭 먹통 재현과 구별한다.
- 최종 검사: `/tmp/flowmux-count-position-final.log`의 `DIFF_REVIEW_LEGACY_COUNT_AND_FEEDBACK_OK`와 `MACOS_NATIVE_SMOKE_OK`; `git diff --check` 통과. 사용 중인 앱 재시작/설치 교체는 하지 않았다.

## 편집 스크롤과 Send 차단 후속 검증 (2026-10-03)

### 재현한 원인과 수정

- 같은 위치의 코멘트에서 Edit를 누르면 편집 중 카드를 건너뛴 뒤 composer를 맨 마지막에 추가했다. 이제 해당 카드의 원래 순서에 composer를 넣는다. 수정 전 첫 카드의 위치 검사가 실패한 로그는 `/tmp/flowmux-scroll-before.log`이다.
- TextView의 자식 카드 높이가 확정되기 전에 코드 줄 기준으로 스크롤했다. 카드 배치 후 실제 경계와 viewport를 비교해 스크롤하며, 긴 문서의 추가 높이 계산으로 위치가 바뀌면 다음 프레임에서 보정한다. 새 탐색이나 다시 그리기는 이전 스크롤 요청을 무효화한다. 일반 코드 탐색은 `scroll_to_mark`를 사용한다. GTK도 즉시 `scroll_to_iter`를 호출하면 아직 계산되지 않은 줄 높이 때문에 잘못된 위치로 갈 수 있음을 명시한다: [GTK TextView 문서](https://docs.gtk.org/gtk4/method.TextView.scroll_to_iter.html).
- 기존 `has_unsaved_review`는 입력칸에 글자가 있으면 모두 미저장으로 보았다. 저장된 코멘트에서 Edit만 눌러도 원문이 입력칸에 들어가므로 Send가 대상 검증 전에 차단됐다. 이제 편집 중에는 저장 원문과 비교한다. 변경하지 않았거나 원문으로 되돌렸으면 전송하고, 실제 수정한 내용은 보존하면서 저장을 안내한다.
- 저장/전송 작업 중인 경우 먼저 busy guard로 반환한다. 이때 미저장 경고를 덮어쓰지 않는다.
- 사라진 파일의 코멘트로 이동하는 분기도 실제 카드 위치로 스크롤한다. 이 경로의 pending-note borrow를 렌더링 전에 분리하여 RefCell 재진입을 방지했다.

### 추가 시나리오와 증거

| 시나리오 | 검증 | 결과 |
|---|---|---|
| 같은 줄의 코멘트 5개 중 첫 번째/마지막 Edit | Native | 원래 카드 순서 유지, composer 전체가 viewport 안에 들어옴 |
| 20,000줄 Diff에서 카드 높이를 포함한 편집 이동 | Native | 실제 `compute_bounds`로 확인, `DIFF_REVIEW_COMMENT_SCROLL_GEOMETRY_OK` |
| 기존 코멘트 Edit → 내용 그대로 → Send | Native + GUI | 미저장 경고 없이 Copy/전달 성공 |
| Edit → 내용 수정 → Send | Native | 전송 차단, 수정 내용 유지, 작성 위치로 복귀 |
| Edit → 내용 수정 → 원문 복구 → Send | Native | 전송 허용, 편집기 닫힘 |
| Codex 대상 2개 → 두 번째 메뉴 항목 선택 | Native + GUI | 선택한 surface의 수신 PTY에 전달, source PTY는 무변경 |
| source와 destination 모두 Diff 표시 중 전달 | Native + GUI | 두 Diff가 닫히고 destination terminal 표시. Native는 destination focus도 검사 |
| 메뉴 콜백 → feedback 준비 → bridge → PTY → Enter | Native | 전체 연결 경로 통과, `DIFF_REVIEW_MENU_MULTI_CODEX_PTY_HANDOFF_OK` |
| legacy resolved 2개를 포함한 코멘트 5개 전달 | Native + GUI | 목록·카운트·전달 내용 모두 5개 |
| 기존 busy/approval/종료/session 변경/기존 입력 보호 | Native | 기존 provider handoff 회귀 검사 재통과 |
| 팝업 내부 항목의 좌표 마우스 클릭 | 미확정 | 자동화에서는 메뉴만 닫힘. 아래 한계 유지 |

GUI는 새로 빌드한 `/tmp/flowmux-popup-repro/FlowMux.app`에서 수행했다. 첫 카드의 Edit를 마우스로 누른 뒤 원래 위치와 입력칸/Save/Cancel의 노출을 확인했다. 두 pane에 Diff를 열고 왼쪽에서 변경하지 않은 Edit 상태로 Send를 열어 Tab/Enter로 오른쪽 대상을 선택했다. 양쪽 Diff가 닫히고 오른쪽 terminal에 `GUI_FEEDBACK_RECEIVED`가 표시됐다. `/tmp/flowmux-popup-repro/gui-receipt.bin`은 780 bytes이며 bracketed paste 시작/끝과 submit 키, 코멘트 1~5가 각각 정확히 한 번 포함됨을 확인했다.

이 대상들은 Codex presence를 사용하는 **격리된 테스트 수신 프로세스**다. 실제 사용 중인 Codex에게 테스트 메시지를 보내지 않았으며, 실제 Codex의 응답 생성까지 검증한 것은 아니다. GUI 메뉴 선택은 키보드로 수행했고, Native 메뉴 검사는 실제 버튼의 clicked signal을 사용했다. 앞서 기록한 팝업 좌표 클릭의 도구 입력 전달 문제와 앱 문제는 아직 구분하지 못했으므로 모든 마우스 경로가 해결됐다고 주장하지 않는다.

### 반복 실행

```sh
./scripts/test-diff-review-macos.sh
```

스크립트의 각 명령을 실행하여 Git 모델 13개, draft 저장소 1개, macOS Diff native suite가 통과했다. 스크립트 자체는 `bash -n`으로 검사했다. 최종 native 로그 `/tmp/flowmux-review-scenarios4.log`에 `MACOS_NATIVE_SMOKE_OK`와 위 두 새 마커를 포함한 11개 Diff 마커가 있다. 모델/저장소 로그는 `/tmp/flowmux-review-scenarios-{vcs,state}.log`다. GTK 테마 파서 경고는 있었으나 검사 실패는 없었다. 사용자 실행 앱은 설치 교체하거나 재시작하지 않았다.

## 전체 삭제와 같은 checkout의 코멘트 자동 동기화 (2026-10-03)

- Comments 메뉴의 `Reload saved comments` 바로 아래에 `Remove all comments`를 추가했다. 저장 성공 후 목록·inline 카드·Comments/Send 숫자와 현재 composer를 함께 정리한다. 빈 목록에서는 비활성이다. 기존 revision 검사와 저장 실패 처리를 재사용한다.
- 표시 중인 Diff는 500ms 간격으로 같은 root의 저장 revision을 백그라운드에서 확인한다. 변경된 경우에만 화면을 다시 그린다. 숨겨진 pane은 조회하지 않으며, view가 사라지면 타이머도 종료된다. 같은 로컬 저장소 DB를 사용하는 별도 창/프로세스도 동일한 저장 데이터를 읽는다.
- 미저장 새 코멘트의 텍스트와 편집 포커스를 보존한다. 동일 코멘트가 다른 pane에서 수정/삭제된 경우, 변경하지 않은 Edit는 최신 상태로 전환하고 실제 미저장 편집은 보존하면서 덮어쓰기를 차단한다. 다른 Git root의 코멘트는 섞이지 않는다.
- Comments/Send 메뉴가 열려 있는 동안에는 반영을 보류한다. 메뉴를 닫은 뒤 다음 조회에서 반영하여 포커스된 버튼이 제거되는 기존 문제를 피한다. 저장 중이거나 더 최신 revision을 이미 적용한 경우 오래된 조회 결과를 버린다.

| 추가 시나리오 | 결과 |
|---|---|
| 전체 삭제 → 재로드 → 빈 목록/카드/카운트 | Native 통과, `DIFF_REVIEW_REMOVE_ALL_COMMENTS_OK` |
| 별도 창 A에서 추가 → B 자동 반영, B에서 추가 → A 자동 반영 | Native 통과 |
| B의 미저장 새 초안 유지하면서 A의 저장 반영 | Native 통과 |
| 열린 Comments 메뉴의 버튼 유지 → 닫은 뒤 최신 내용 반영 | Native 통과 |
| 양쪽 동일 코멘트 수정 → 미저장 텍스트 보존·덮어쓰기 차단 | Native 통과 |
| 변경하지 않은 Edit 중 다른 창에서 전체 삭제 | Native 통과, 편집기 종료·0개 반영 |
| 다른 Git root의 창 | Native 통과, 코멘트 격리 |
| 실제 split 왼쪽 Edit/Save → 오른쪽 내용 갱신 | GUI 마우스 조작으로 확인 |
| 실제 split 오른쪽 Delete → 왼쪽 카드 제거·양쪽 5→4 | GUI 마우스 조작으로 확인 |
| Reload 바로 아래 전체 삭제 → 양쪽 4→0 및 Send 비활성 | GUI 메뉴 표시 후 Tab/Enter로 실행 확인 |

최종 native 로그: `/tmp/flowmux-live-sync-native.log`의 `DIFF_REVIEW_LIVE_COMMENT_SYNC_OK`, `DIFF_REVIEW_REMOVE_ALL_COMMENTS_OK`, `MACOS_NATIVE_SMOKE_OK`. 실제 GUI는 격리된 `com.flowmux.PopupRepro`의 새 빌드에서 확인했다. 사용자 앱은 교체/재시작하지 않았다. 팝업 항목의 마우스 선택에 관한 앞선 자동화 한계는 유지된다. 미저장 입력을 다른 pane에 실시간 복제하는 기능은 아니며, 저장된 코멘트 변경을 자동 반영한다.

## Diff 진입 단축키와 하단 아이콘 (2026-10-03)

- Linux/macOS 기본 `open-diff-review`를 `Ctrl+Alt+D`에서 `Ctrl+Alt+E`로 변경했다. 이전 절의 D 검증 기록은 변경 전 결과다.
- 사이드바 하단 AI 사용량 바로 오른쪽에 내장 Diff 아이콘을 추가했다. `win.open-diff-review`를 호출하므로 현재 포커스된 pane의 Git 경로를 사용한다.
- 설정 버튼은 왼쪽에 고정하고 나머지 아이콘을 가로 스크롤 영역으로 묶었다. 처음 표시하거나 영역 폭이 바뀌면 배치 완료 후 오른쪽 끝으로 정렬한다. 폭이 부족하면 왼쪽 아이콘부터 잘리며, 수동 휠/가로 스크롤 위치는 다음 크기 변경 전까지 유지한다.
- 네이티브 검사에서 아이콘 순서·액션·E accelerator, 좁은 폭의 왼쪽 clipping과 마지막 아이콘 노출, 수동 스크롤 유지, 확대 후 재축소의 오른쪽 정렬을 검사했다. `/tmp/flowmux-footer-native.log`: `DIFF_REVIEW_SIDEBAR_FOOTER_OK`와 기존 Diff suite의 `MACOS_NATIVE_SMOKE_OK`.
- `/tmp/fm-footer/FlowMux.app` (`com.flowmux.FooterRepro`)의 실제 창에서 Diff 아이콘 클릭 및 Back 이후 Ctrl+Alt+E로 `/private/tmp/fm-footer/project`의 Diff가 열림을 확인했다. 좁은 사이드바의 오른쪽 아이콘 우선 표시, 휠로 숨겨진 Agents/AI usage 아이콘 다시 노출도 확인했다. 자동화 드래그로 sidebar 폭 변경은 발생하지 않아 폭 변경 자체는 위 네이티브 GTK 검사로 검증했다.
- `cargo test -p flowmux-config keybindings --locked`: 19개 통과 (`/tmp/flowmux-footer-config.log`). `git diff --check` 통과. 사용 중인 사용자 앱은 교체하거나 재시작하지 않았다.
