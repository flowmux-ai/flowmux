<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

**터미널 성능 개선 구현·검증 기록**

기준 소스는 `9d5b903`. 수정 전 debug/fast GUI와 CLI를 각각
`/tmp/flowmux-implementation-20260921/baseline-debug`, `baseline-fast`에 보존했다.
사용자 창 PID 6619를 보호하며 별도 프로세스·소켓·D-Bus·XDG 상태에서 검증한다.
현재 실행 중인 사용자 창에는 바이너리를 주입하거나 재시작하지 않는다.

| 개선안 | 진행 상태 |
|---|---|
| alternate 화면 전환 시 geometry 고정 | 구현 및 실제 GUI 검증 완료 |
| 미니맵 작업량/그리기 비용 | 대기 |
| 워크스페이스 pane/surface 복원 | 구현 및 실제 GUI 검증 완료 |
| 전체 스크롤백 저장 | 대기 |
| PTY backpressure 중 입력/resize | 대기 |
| IME focus cycle 및 redraw | 대기 |
| synchronized output | 대기 |
| 최종 통합 A/B·회귀·설치 | 대기 |

**alternate 화면 geometry**

- 변경: `set_minimap`만 terminal margin을 설정하고, `set_alternate_screen`은 overlay 표시만 바꾼다. 불필요해진 minimap getter를 삭제했다.
- 기준: 실제 자식 PTY의 3회 alternate 왕복에서 `92×37 → 98×37 → 92×37` 반복. `/tmp/fm-gui-sx0b35xo/geometry.json`.
- 수정: 같은 조건에서 모든 샘플 `92×37`. `/tmp/fm-gui-kcfo_jcw/geometry.json`.
- 정상 동작: alternate에서는 미니맵 숨김/스크롤바 표시가 유지되며, 옵션 재적용도 geometry를 바꾸지 않는다. 기존 widget lifecycle 단위 테스트에 해당 조건을 추가해 통과했다.
- 검증: `scripts/test-terminal-rendering-gui.py`의 실제 GUI/PTY 검사, 관련 GTK 단위 테스트, `cargo fmt --all -- --check` 통과.
- 판단: 불필요한 terminal resize 제거를 확인했으므로 유지. 공간 예약은 normal/alternate 모두 동일하다.

**워크스페이스 복귀**

- 변경: 첫 pane의 마지막 탭을 강제로 활성화하던 코드를 제거했다. mapping 전에 기존 pane MRU를 읽고, 해당 workspace에 여전히 속한 pane을 복원한다. 유효한 MRU가 없을 때만 첫 pane으로 돌아간다.
- 기준: 두 탭과 두 pane을 만든 뒤 workspace 왕복 시 active tab 보존=false, 실제 키 입력의 마지막 pane 전달=false. `/tmp/fm-gui-4tbzo1vj/workspace.json`.
- 수정: 두 조건 모두 true. `/tmp/fm-gui-0kt9adom/workspace.json`. 화면 상태뿐 아니라 XTest로 입력한 문자가 마지막 pane에 도착하는지 확인했다.
- 회귀: 새 workspace 진입의 focus fallback 및 Agent Bar/activity의 명시적 pane/surface 목적지 테스트 통과. 포맷 검사 통과.
- 판단: 기존 active tab과 키보드 대상이 유지되므로 변경 유지.

다음 항목들은 구현 후 기준 바이너리와 동일 조건으로 비교하고, 효과 부재나 회귀가 확인된 후보는 수정하거나 되돌린 뒤 결과를 기록한다. 이 문서는 진행 기록이며 전체 작업 완료를 뜻하지 않는다.
