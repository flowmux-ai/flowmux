<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Windows 터미널 미니맵

독립 `windows/`에 미니맵, 설정, CLI 탐색을 추가했다. 기존 Linux/macOS 코드와
공유 Rust core/root Cargo는 수정하지 않았다. 기존 VTE 미니맵의 preview window,
viewport 이동, width/opacity/alternate gutter 계약을 읽고 xterm 공개 cell API로
구현했다. 114개 기능·13개 게이트의 전체 목표를 유지하며 T14/T15는 partial이다.

미니맵은 글자 모양 대신 셀별 색 블록으로 normal buffer의 한 구간을 그린다.
완성형·분해형 한글·이모지의 폭은 xterm cell width를 사용하고 폭 0인 continuation
cell은 중복해서 그리지 않는다. ANSI 16/256색, RGB foreground/background,
bold의 밝은 ANSI 색, inverse, dim, invisible을 처리한다. Unicode를 재분해하거나
정규화하지 않는다. 구성된 theme와 고정 xterm 6 기본 팔레트를 사용하며, 앱이 OSC로
바꾼 팔레트를 미니맵에 반영하는 경로는 아직 없다.

Preview는 물리 행 하나당 CSS pixel 하나이며 pane 높이와 2048행 중 작은 범위만
방문한다. 한 refresh의 최대 방문량은 2048 × 1000 cell이며 전체 scrollback을
문자열로 만들지 않는다. Canvas scale은 최대 2로 제한한다. 따라서 아주 높은 창은
전체 세로 길이보다 작은 preview를 표시하고 200%를 넘는 DPI의 선명도는 미검증이다.
색상 통계는 처음 32색까지만 보관하지만 raster는 나머지 색도 그린다.

출력·크기·설정 변경은 첫 100 ms deadline을 유지해 계속된 출력으로 refresh를
무기한 뒤로 미루지 않는다. 비활성 탭은 timer와 raster를 정리하며 parsing은 계속한다.
탭을 다시 표시하면 최신 buffer로 다시 그린다. 렌더링은 동기적으로 제한된 행 범위를
읽는다. 이 상한은 지속 다중 pane 작업에서 응답성·CPU 예산이 충족된다는 증거는
아니며, 큰 창/많은 pane 부하 검증과 추가 최적화는 남아 있다.

미니맵 위 휠은 한 이벤트당 preview만 25행 이동한다. Click/drag는 pointer에 해당하는
행을 실제 terminal viewport 중앙으로 이동한다. 키보드는 미니맵에 focus가 있을 때
방향키·PageUp/Down·Home/End를 처리하고 ARIA scrollbar 값과 preview 범위를 제공한다.
조합 중에는 탐색을 가로채지 않는다. 물리적인 Windows 마우스/키보드/IME/접근성
검증은 이번 작업에서 수행하지 않았다.

기본 설정은 enabled/width 40/opacity 50이다. width 12–96, opacity 0–100를 native
설정 메뉴와 CLI에서 저장하고 기존 설정 worker로 공유한다. Active composition과
history restore는 기존 Settings controller가 적용을 지연한다. Gutter는 켜짐/너비
설정으로만 바뀌며 alternate 화면 전환에서는 그대로 둔다. Alternate에서는 raster가
숨겨지고, normal 복귀 시 다시 그린다. 비활성화는 gutter와 raster를 해제하고 preview
위치를 초기화한다. xterm 기본 scrollbar는 고정 버전의 CSS selector로 숨기며 fit
계산의 scrollbar allowance를 변경하지 않는다.

`minimap --surface ID read|preview ROWS|seek ROW`는 output parser barrier를 기다리고
keyboard focus를 요청하지 않는다. Preview의 음수는 과거 방향이다. Seek는 현재
retained buffer의 zero-based 물리 행을 중앙으로 이동하며, 그 좌표는 eviction/reflow
이후의 안정적인 참조가 아니다. Read는 dirty인 표시 중 preview를 갱신한 뒤 geometry,
cell/색 통계와 실제 canvas pixel의 FNV checksum을 제공한다. 숨김·비활성화·alternate·
조합 중에는 탐색을 거부한다. Read는 이 상태도 조회하며 숨긴 raster는 비워져 있다.
Native는 identity/sequence 및 bounded 응답을 검사하고 pending 요청 상한·만료·탭 닫기
오류 처리를 적용한다. 선택 문자열은 탐색 중 보존한다.

`native-minimap-background.json`의 실제 숨김 WebView2/ConPTY 9개 검사:

1. 기본 40/50 설정, 한글 wide cell, RGB 색과 실제 canvas checksum.
2. 같은 셀을 다른 색/문자로 덮어쓴 뒤 raster 변경.
3. 2,500행 중 bounded preview만 방문하고 preview 이동이 실제 scroll을 보존.
4. Seek로 실제 과거 화면 조회 위치 이동, 탐색 후 선택 문자열 보존.
5. Alternate 전환에서 gutter·행/열 유지, 숨김 raster 해제, normal 복귀와 ConPTY 크기 확인.
6. 96/12 너비, 0/100 opacity, 켜기/끄기 실시간 적용과 파일 저장, ConPTY 크기 확인.
7. 잘못된 설정과 없는 행 번호를 오류로 처리하고 기존 설정 파일 유지.
8. 비활성 탭은 계속 parsing하되 raster를 그리지 않으며 복귀 시 최신 내용으로 재생성.
9. History clear, workspace 이동, 프로세스 종료 뒤 raster/동일 PID/남은 화면 유지.

모든 live 검증은 숨김 debug host와 소유한 ConPTY에서 수행한다. 사용자 키보드,
마우스, clipboard, foreground 창을 건드리지 않는다. 검증용 probe는 private file로
출력만 제어한다. 설치 파일은 빌드만 하며 실행하지 않는다.

검증 중 기존 screen probe의 `Move-Item -Force`가 파일 교체에서 실패했다. 당시 7개
화면 조회 항목 뒤 테스트 제어 파일에서 발생했으며 제품 IPC 오류는 아니었다.
원본 실패 JSON은 `native-screen-minimap-control-failure.json`으로 보존했다.
Screen/Selection/Minimap probe의 읽기 핸들은 Delete sharing을 허용하고 제어 파일은
`MoveFileEx(REPLACE_EXISTING)`으로 원자적으로 교체하도록 수정했다. 이를 통해 읽는
중에도 파일을 교체할 수 있고 PowerShell의 삭제/이동 중간 단계를 제거했다.
그 후 같은 native screen 시나리오와 관련 probe를 다시 검증했다.

Rust 검사는 미니맵 기본값·설정 범위·old JSON 복원, signed preview CLI와 잘못된
metadata 거부를 포함한다. 새 검사에서 unit enum의 예상 밖 필드가 허용되는 것을
발견해 read action을 빈 struct variant로 바꾸고 불필요한 필드를 거부하도록 했다.
프런트엔드는 cell paint/geometry, refresh deadline, 비활성 raster, preview/seek,
selection/focus/조합 guard와 settings 지연 적용을 검사한다. DOM/input은 테스트 객체이며
실제 Microsoft IME 검증으로 세지 않는다.

최종 검사 결과는 frontend 53개, Linux에서 실행한 독립 Windows Rust crate 31개,
실제 숨김 Windows Rust 58개 통과다. Native screen 10개, selection 10개, settings
7개군도 최종 실행 파일에서 통과했다. 각 `*-minimap-background.json`과 test/build
로그를 별도로 보존했다. Release/Clippy/NSIS 빌드가 통과했으며
`artifacts-minimap.json`은 7개 실행 파일·설치 파일의 SHA-256이다.
`cleanup-minimap.json`은 이번 테스트 host/probe PID 및 해당 부모 PID를 가진 프로세스가
남아 있지 않은 확인 기록이다. 사용자 WSL flowmux PID 787은 실행 중이었다.

실제 pointer/keyboard/IME/menu/accessibility, DPI/multimonitor 시각적 품질, 앱의 OSC
palette 변경, 큰 pane·지속 다중 pane 부하·frame latency, 모든 close/timeout/reload
경쟁 조건과 기존 플랫폼 전체 live 회귀는 미완료다. G06을 포함해 전체 release gate가
완료됐다고 판단하지 않는다.
