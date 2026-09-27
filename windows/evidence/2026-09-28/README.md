<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Windows 전체 터미널 출력 검색 검증

이 단계는 Windows의 독립 Rust 호스트에 전체 터미널 검색을 추가한다. 전체
114개 기능·13개 게이트의 완료 증거는 아니다. 기존 Linux·macOS 소스, root
Cargo 설정·lockfile 및 공유 core는 수정하지 않았다.

`native-output-search-background.json`은 Windows 11 build 22623, PowerShell
5.1, WebView2 112.0.1722.48에서 실행한 숨김 호스트 기록이다. 터미널 창과
새 검색 창이 표시되거나 foreground를 차지하지 않았음을 확인했다. 키보드·
마우스 입력이나 클립보드를 사용하지 않았다.

검색은 raw transcript가 아닌 xterm의 파싱된 grid를 읽는다. 128개 물리 행마다
MessageChannel로 실행을 양보하며, 읽는 동안 출력/크기가 바뀌면 해당 터미널을
갱신 필요로 표시한다. 다른 검색 시작·취소 시 이전 응답과 결과 참조는 만료된다.
한 논리 행에서 첫 일치를 결과로 표시하고 전체 일치 행 수와 500행 페이지를
반환한다. 숨은 tab 및 다른 workspace도 활성화 없이 읽는다.

실제 Windows에서 다음을 확인했다.

- 2개 workspace의 터미널 3개에서 4개 일치 행을 찾고 native LISTBOX 행 수와
  비교했다. 검색 전후 활성 surface가 같았다.
- 한글이 soft-wrap 경계를 건너는 결과를 열어 정확한 문자열을 선택했다.
  이후 출력 추가와 같은 크기의 workspace 이동 뒤에도 같은 결과를 열었다.
- pane 분할로 열 수가 바뀐 결과는 재배치된 다른 위치를 선택하지 않고 거부했다.
- 분해형 자모·결합 악센트·이모지의 선택 문자열을 Ordinal 비교했다.
  U+0130의 소문자 변환으로 UTF-16 길이가 늘어도 뒤쪽 검색 위치가 맞았다.
- 260개가 넘는 물리 행으로 감긴 긴 논리 행의 검색·결과 이동이 통과했다.
- 503개 일치 행은 500개와 3개 페이지로 반환됐다. 새 검색은 이전 참조를
  만료시켰고 취소는 native 목록과 retained marker를 정리했다.
- 같은 커서 위치에서 덮어쓴 결과는 거부했다. 사전 검증이 실패하면 활성
  tab을 바꾸지 않는다. 오류 문구가 주기 timer에 의해 지워지지 않는 것도
  숨김 native STATIC control에서 확인했다.
- alternate 화면 결과는 normal 복귀 뒤 거부했고 해당 문자가 normal
  검색에 섞이지 않았다. 10,100행 출력 뒤 scrollback에서 밀려난 결과도 거부했다.

결과를 열 때 먼저 내용과 marker를 확인한 뒤 화면을 활성화한다. 활성화로
크기가 바뀌거나 두 확인 사이 출력이 바뀌면 두 번째 확인에서 거부할 수 있다.
이는 같은 모양의 다른 행을 대신 선택하는 것을 막기 위한 동작이다. 각 페이지는
현재 출력을 다시 검색하므로 출력 중인 셸에서는 페이지 사이 총수가 달라질 수 있다.

JavaScript 자동 검사는 case-fold 위치, wide-cell wrap padding, 페이지/marker
제한, append 허용, rewrite/trim/resize 거부, 사전 확인의 선택 불변, chunk 사이
취소·출력 변경 시 혼합 결과 차단을 검사한다. 실제 검색창 Microsoft IME 조합,
마우스·키보드 조작, 시각적 배치·DPI는 사용자 요청에 따라 보류했다. native
LISTBOX 문자열·상태 검사가 시각적/접근성 검증을 대신하지 않는다.

Linux의 독립 Rust 테스트 9개와 프런트엔드 테스트 17개가 통과했다
(`linux-rust-tests-output-search.txt`, `frontend-tests-output-search.txt`).
Windows release build와 Clippy도 통과했다(`release-clippy-output-search.txt`).
같은 최종 debug 빌드에서 기존 단일 찾기, 상태 저장·복원, 탭 이동 40회를
다시 확인했다(`native-find-output-search-background.json`,
`native-state-output-search-background.json`,
`native-tab-move-output-search-background.json`).
`artifacts-output-search.json`은 실행 파일과 갱신한 개발용 설치 파일의
크기·SHA-256이다. 새 설치 파일을 실행하거나 설치 완료로 판정하지 않았다.

SSH UI, 다른 애플리케이션 창의 검색, 지속 출력 중 장시간 부하·메모리, 모든
한글 폭·줄바꿈 조합은 별도 작업이다. 전체 구현 목표는 계속 진행 중이다.

후속 pane 크기·방향 이동·최대화 단계는 [별도 검증 기록](panes.md)에 정리했다.
검색 단계의 기존 증거와 artifact 해시는 유지했다.
