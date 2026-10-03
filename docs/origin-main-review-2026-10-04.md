<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# origin/main 대비 변경 검토 — 2026-10-04

검토 시작 범위는 `8c967fe9` (`origin/main`, fetch 후 확인)부터
`c7f7a665`까지의 8개 커밋, 48개 파일이다. 아래 개선은 로컬 main에 추가했다.
사용 중인 FlowMux 앱과 에이전트는 재시작하지 않았으며 원격 push는 하지 않았다.

## 검토 범위

- 프로세스/화면 기반 에이전트 감지와 상태 저장소의 수명·잠금·폴링 경계.
- Git 비교 범위, 파일 목록과 patch 해석, 코멘트 위치 재검증.
- SQLite revision 충돌 처리, 비동기 조회 결과, 창 간 코멘트 동기화.
- pane별 리뷰 소유권, 레이아웃 재구성, 종료 보호, 토글과 포커스.
- 대상 세션·PID·상태·입력 확인에서 PTY 전달까지의 연결 경로.
- macOS IME 변경, 메뉴·아이콘·단축키 연결, 추가된 테스트와 문서.
- 추가된 skill 자료의 실행 코드와 lock 메타데이터. 외부 서비스 연결은 실행하지 않았다.

## 재현과 메타리뷰 후 채택한 개선

| 문제와 실제 영향 | 수정 전 증거 | 메타리뷰와 선택한 수정 |
|---|---|---|
| `diff.suppressBlankEmpty=true`에서 빈 문맥 줄을 세지 않아 코멘트 위치가 한 줄씩 밀림 | `configured_blank_context_keeps_comment_line_numbers`: 3번 줄이 2번 줄로 계산되어 실패 | 파서 분기를 추가하는 대신 내부 Git 호출에만 `diff.suppressBlankEmpty=false` 적용. 사용자 설정 파일은 수정하지 않음 |
| 삭제를 stage한 경로에 새 파일을 만들면 같은 경로가 D와 ? 두 항목으로 표시됨. 경로로 코멘트를 찾는 검증이 다른 항목을 선택할 수 있음 | `staged_deletion_and_recreated_file_share_one_review_entry`: 예상 1개, 실제 2개로 실패 | 경로별 항목을 하나로 묶고 삭제/추가 patch를 모두 보존. patch 경계에서 줄 카운터 초기화. Git index를 수정하거나 임시 index를 만들지 않음 |
| 레이아웃 재구성 때 리뷰는 옛 Stack에 남고 새 레이아웃에는 터미널만 표시됨 | 실행 중인 native 앱에서 `layout reconstruction must reattach the visible review` assertion 실패 | 기존 pane별 리뷰 객체를 새 Stack에 연결하고 표시 여부·복귀 대상 유지. 새 상태 저장 체계 없이 기존 소유권 재사용 |
| MCP 평가 스크립트가 여러 tool_use 중 하나만 처리하고, SDK content 객체 목록을 JSON으로 직렬화하지 못함 | 오프라인 테스트: 결과 ID가 `1,2,3` 대신 `1`만 존재 | 한 배치의 모든 호출에 결과를 반환하고 `model_dump`로 content 직렬화. 개별 호출 실패도 오류 결과로 전달하고 나머지 호출 계속 처리 |

코드 커밋:

- `513d3be8`: Git 빈 줄 및 재생성 파일 코멘트 처리.
- `df1eb51f`: 레이아웃 재구성 후 리뷰 유지, 실패 시 진단 정보 보강.
- `8320953b`: MCP 평가 도구 결과 누락 및 직렬화 수정.

재생성 파일은 `D/?` 한 항목 아래 삭제와 추가를 각각 표시한다. 기존 파일과 새
파일의 줄 번호를 각각 유지하며, 두 영역에 작성한 코멘트 모두 재검증한다.
합친 patch에도 기존 8 MiB 표시 제한을 적용한다. 경로 병합은 해시 맵으로
조회하여 untracked 파일 수에 따른 제곱 비용을 피한다.

## 수정하지 않은 후보

- 코멘트 Reattach 후 본문이 같으면 변경을 잃을 것이라는 가설은 실제 native
  검사에서 반증됐다. 기존 임시 anchor가 이미 미저장 변경으로 인식되므로
  추가 플래그를 도입하지 않았다.
- Git 표시 설정 중 실제 출력에 영향을 주지 않은 후보는 제외했다.
- 이번 범위 밖의 SSH/PTY 실패와 기존 lint를 개선 효과로 포함하지 않았다.
- 대규모 클래스 분리, 저장 스키마 변경, 별도 전역 리뷰 상태는 필요성이
  입증되지 않아 추가하지 않았다.

## 검증 결과

- `cargo test --locked --no-fail-fast`: 1,034개 통과, 5개 실패, 8개 ignored.
  실패 항목은 아래의 별도 origin/main 빌드에서도 재현됐다.
- Git review 테스트 15개 통과. 빈 줄의 정확한 위치, 재생성 경로의 양쪽 코멘트,
  staged/unstaged 범위 분리를 추가 검증했다.
- Code Review 전용 macOS native suite 통과:
  `CODE_REVIEW_LAYOUT_REATTACH_OK`, `CODE_REVIEW_TOGGLE_PRESERVES_DRAFT_AND_TERMINAL_OK`,
  `DIFF_REVIEW_PANE_ROOT_ISOLATION_OK`, `MACOS_NATIVE_SMOKE_OK`.
- 전체 macOS native suite 재실행 통과: 테마, 브라우저, 포커스, 리뷰 저장·동기화·
  PTY 전달, IPC 포화/긴 경로, 종료 취소·저장 재시도 포함.
  첫 실행의 전달 테스트는 타임아웃이었다. 실패 진단 정보를 추가한 재실행에서
  전달 및 전체 검사가 통과했다. 타임아웃 원인을 확정하거나 무시 처리하지 않았다.
- `/tmp/fm-audit-live/FlowMux.app`의 격리된 실제 GUI에서 빈 문맥 줄 이후 3번 줄,
  재생성 경로의 단일 항목 및 삭제/추가 표시, 추가된 1번 줄의 코멘트 저장 확인.
- MCP 평가 오프라인 테스트 통과: 세 호출 결과, content 직렬화, 중간 호출 실패
  이후 다음 호출, 호출별 metrics. 실제 API 요청이나 메시지 전송은 하지 않았다.
- `cargo fmt --all -- --check`, `git diff --check` 통과.
- VCS/state/procmon의 `cargo clippy --all-targets -- -D warnings` 통과.
  workspace 전체 일반 clippy는 완료됐으나 경고가 남아 있다. strict clippy는
  기존 daemon `collapsible_match`부터 실패하며 origin/main에서도 같은 실패 확인.

## 기준 버전에서도 확인된 실패와 검증 한계

`/tmp/fm-audit-origin`에 `origin/main`을 별도 checkout하고 독립 target으로
core/CLI를 빌드해 비교했다. 현재 실패한 다음 다섯 테스트가 기준 버전에서도
실패했다. 원인을 수정했다고 주장하지 않으며 전체 테스트가 모두 녹색인 상태는 아니다.

- `remote_bootstrap_preserves_literal_arguments_and_rejects_missing_cwd`
- `pty_tee_restores_shared_stdout_flags`
- `pty_tee_outer_eof_kills_signal_ignoring_inner_group`
- `pty_tee_preserves_input_queued_before_startup`
- `pty_tee_delivers_final_output_in_order_after_outer_eof`

Linux GTK 실행, 모든 외부 agent 버전의 실제 응답 생성, 모든 OS 입력기 조합은
이 macOS 검증에 포함되지 않는다. Native PTY 전달 검증은 격리된 수신 프로세스에
대한 것이다. 사용자 에이전트에 테스트 프롬프트를 전송하지 않았다.

재현 로그는 작업 머신의 `/tmp/flowmux-audit-*.log`에 있다. 주요 파일은
`blank-before`, `recreated-before`, `vcs-final`, `layout-before`, `layout-after`,
`native-full-retry`, `all-final`, `origin-tests`, `origin-clippy`, `focused-clippy`다.
임시 로그 경로는 이 문서의 결론을 재실행 없이 보장하는 CI artifact가 아니다.
