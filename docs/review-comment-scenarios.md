<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# 리뷰 코멘트 추적 시나리오 검증

검증 기준 커밋은 `70353b37`이다. 저장된 코멘트가 파일 수정 후 올바른
위치로 이동하는지, 위치를 확정하지 못할 때 보존되는지, 새로고침·재열기·전송
경로에서 같은 규칙을 지키는지 확인했다. 아래 테스트는 실제 Git 저장소와
SQLite를 사용하며, 화면 테스트는 macOS native main-thread harness에서
실행 중인 GTK 창을 조작한다.

## Git·저장소 시나리오

소스: [review_comment_scenarios.rs](../crates/flowmux-state/tests/review_comment_scenarios.rs)

| 시나리오 | 검증 기준 | 결과 |
|---|---|---|
| 코멘트 저장 후 새 저장소 객체로 로드 | 줄 범위·선택 코드·앞뒤 문맥·내용 유지, 변경 없는 재검증은 revision 유지 | 통과 |
| 한국어 여러 줄 선택 앞에 줄 삽입·삭제 | 같은 코멘트 ID와 내용 유지, 새 줄 범위 저장 | 통과 |
| 여러 줄 문맥 중 일부의 들여쓰기 변경 | 삭제 표시 줄이 끼어도 현재 코드의 연속 범위를 추적 | 통과 |
| 중간 수정으로 분리된 diff 구간 합치기 | 선택 범위를 확정할 수 없으면 삭제 대신 재연결 상태로 보존 | 통과 |
| 대상 코드는 그대로지만 새 diff의 문맥 밖으로 이동 | 전체 문맥에서 코드 존재 확인 후 코멘트 보존 | 통과 |
| 추가 코드를 커밋한 후 근처를 다시 수정 | 추가 줄에서 문맥 줄로 바뀐 코드에 다시 연결 | 통과 |
| 동일한 코드와 주변 문맥 복제 | 위치 추측 금지, 저장 후 재열어도 보존, 전송 차단, 후보가 하나가 되면 복구 | 통과 |
| Git 읽기 실패 및 대상 코드 교체 | 실패 시 저장 데이터 유지, 코드가 없어졌음을 확인한 경우 정리 | 통과 |
| 삭제 쪽 여러 줄 사이에 추가 쪽 줄이 나타남 | 원본 코드의 줄 범위를 유지 | 통과 |
| 현재 코드가 삭제되고 원본 쪽에만 같은 텍스트 존재 | 원본 쪽으로 잘못 이동하지 않음 | 통과 |
| 전체 문맥이 8 MiB 제한 초과 | 오류를 반환하고 기존 저장 코멘트를 삭제하지 않음 | 통과 |
| 파일 변경을 모두 커밋하거나 파일 삭제 | 더 이상 리뷰할 변경이 없는 코멘트 자동 정리 유지 | 통과 |

## 실행 화면 시나리오

소스: [comments/scenarios.rs](../crates/flowmux/src/ui/review_window/comments/scenarios.rs)

| 시나리오 | 검증 기준 | 결과 |
|---|---|---|
| Refresh 버튼 | 줄 이동·탭 들여쓰기 변경 후 카드 위치 표시와 SQLite 위치 일치 | 통과 |
| 숨김 후 재열기 및 창 객체 재생성 | 메모리의 이전 위젯 없이 저장 코멘트로 위치 복구 | 통과 |
| 화면 갱신 없이 전송 직전 파일 수정 | 최신 줄 번호와 코드가 dispatch 메시지에 포함됨 | 통과 |
| 전송 직전 코드 복제 | 클립보드 불변, 에이전트 dispatch 없음, 코멘트 보존, Reattach 버튼 표시 | 통과 |
| 중복 후보 중 하나로 직접 재연결 | 코멘트 ID·내용 유지, 모호 상태 해제, 피드백 복사 재개 | 통과 |

새 화면 시나리오의 수신 대상은 테스트용 bridge다. 실제 제공자에 프롬프트를
보내지 않는다. 기존 native smoke의 별도 수신 PTY handoff 검사도 통과했다.

추가로 `/tmp/fm-scenario-live/FlowMux.app`을 새로 빌드해 별도 bundle ID,
config/state/runtime 디렉터리로 실행했다. 4번 줄에 코멘트를 직접 작성한 뒤
27번 줄만 수정해 4번 줄이 diff에서 사라지게 했다. 화면의 Comments 수가 1로
유지되고 Reattach 안내가 나타났으며 SQLite에도 코멘트가 보존됐다. 원래
구간을 다시 표시하고 앞에 줄을 추가하자 같은 코멘트가 새 5번 줄에 연결되고
저장 데이터의 재연결 상태도 해제됐다. 검증 창만 종료했다.

## 구현 리뷰와 재현된 결함

| 결함 | 수정 전 증거 | 채택한 수정 |
|---|---|---|
| 여러 줄 매칭에 반대쪽 diff 줄이 포함되어 추적 실패 | `multiline_context_survives_an_indentation_change_with_removed_diff_rows`: 예상 코멘트 1, 실제 0 | 같은 코드 쪽의 줄을 먼저 골라 연속 범위 비교 |
| diff 구간 합치기를 코드 삭제로 오인 | `additional_context_from_merged_hunks_does_not_delete_a_range_comment`: 예상 1, 실제 0 | 선택 코드 일부가 남으면 원래 내용과 위치 정보를 보존하고 재연결 요구 |
| diff 문맥에 없는 코드를 삭제된 것으로 오인 | `unchanged_target_outside_the_new_diff_context_is_not_deleted`: 예상 1, 실제 0 | 삭제 판정 전에 필요한 파일만 전체 diff 문맥 확인 |
| 이전 diff로 카드를 그리면서 전송 검증의 모호 상태를 지움 | native 시나리오의 `blocked feedback must expose recovery in the visible card` assertion 실패 | 렌더링 중 저장된 `needs_reattach` 상태 유지 |

기존 읽기 전용 Git 경로를 재사용해 외부 diff/textconv 비활성화, literal pathspec,
8 MiB 출력 제한과 30초 timeout을 유지했다. 전체 문맥은 기본 diff에서 위치를
못 찾고 코드 존재도 확인하지 못한 경우에만 조회하며, 같은 scope/path에서는
한 번 읽은 결과를 재사용한다. 모든 코멘트 검증이 성공한 다음 기존 SQLite
revision 검사를 거쳐 저장하므로 Git 읽기 오류나 다른 창의 저장을 덮어쓰지 않는다.
GTK 호출은 메인 스레드에, Git 및 저장소 읽기는 기존 blocking worker에 남는다.

줄 번호만 가까운 후보를 선택하는 방식은 채택하지 않았다. 같은 코드가 반복될
때 잘못 연결될 수 있기 때문이다. 위치를 유일하게 확인하지 못한 코멘트는
전송하지 않는다. 파일 경로 변경 추적과 임의의 코드 재작성에 대한 의미 분석은
이 검증 범위가 아니다. 위치 갱신은 새로고침·재열기·전송 시 수행되며 파일
시스템을 실시간 감시하는 기능은 아니다.

## 재실행과 결과

```sh
cargo test -p flowmux-state --test review_comment_scenarios --locked
cargo test -p flowmux-vcs --test review --locked
cargo test -p flowmux-state review_drafts --locked
TMPDIR=/tmp FLOWMUX_REVIEW_SMOKE_ONLY=1 \
  FLOWMUX_BUNDLED_CLI_PATH="$PWD/target/debug/flowmuxctl" \
  cargo test -p flowmux --test macos_native --features native-smoke --locked
cargo clippy -p flowmux-state -p flowmux-vcs --all-targets --locked -- -D warnings
cargo clippy -p flowmux --test macos_native --features native-smoke --locked
cargo fmt --all -- --check
git diff --check
```

저장소 시나리오 12개, 기존 VCS 리뷰 테스트 18개, 기존 draft 테스트 2개 통과.
Native smoke는 새 화면 시나리오 5개와 기존 편집·삭제·동기화·pane 격리·전달
검사를 포함해 `MACOS_NATIVE_SMOKE_OK`로 종료했다. VCS/state의 엄격한 Clippy는
통과했다. UI Clippy는 성공했으며 기존 `agent_runtime.rs`, `sessions.rs`,
`ghostty_pane.rs`, `macos_smoke.rs`의 경고 4개가 남았다. 변경 파일에서는 경고가
없었다. Linux 실행 화면 및 전체 workspace 테스트는 이번 범위에서 실행하지 않았다.
