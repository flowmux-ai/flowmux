<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Agent runtime 구현 검증과 액션 아이템

기준: `origin/main..agent/runtime-audit-20261117`의 managed Agent Host 변경과
[활성 Agents 설계](active-agent-runtime-design.md). 사용자 실행과 설치본은 유지하고,
별도 XDG state/runtime·D-Bus·display에서 검증한다. 액션 아이템마다 수정과 회귀
검증을 하나의 커밋에 담는다. 최종 심각도 판정은 관측한 결과에만 적용한다.

## 독립 검토 후속 — 2026-11-18 KST, AR-100–107

- `c6a513db`를 독립적으로 다시 읽어 확인한 8개 항목이다. 항목마다 별도
  worktree에서 수정하고, 통합 tip 위의 후보 커밋에서 format·workspace Clippy·
  관련 crate test·해당 live phase를 다시 실행한 뒤 fast-forward로 쌓았다.
  후보 검증은 순차 실행이며 worker의 병렬 실행 결과를 통과 근거로 쓰지 않았다.
  CI dispatch는 하지 않았다.
- 이 문서의 「푸시를 보류했다」 이후에도 `0e17ae65..c6a513db`의 12개 커밋이
  원격에 있다. 그중 제품 수정 `d7c23115`·`15c123f7`·`c6a513db`는 run
  [44199328889](https://github.com/flowmux-ai/flowmux/actions/runs/44199328889)의
  Linux regression·headless stress·editor web 성공으로 확인했고 **macOS job은
  skip**이다. 아래 항목도 macOS에서 컴파일·실행하지 않았다.

| ID | 커밋 | 변경 | 통합 전 재검증 |
| --- | --- | --- | --- |
| AR-100 | `367d0578` | Host를 시작할 수 없거나 launch가 `Failed`이면 Sessions Resume과 Session Flow resume/fork를 이전의 일반 tab 실행으로 수행한다. timeout·응답 유실·응답하는 비호환 Host에서는 넘어가지 않는다. New Agent는 Host 전용 | CLI 143·189, GUI 972, runner 30(1 skip), live 5개(tab-fallback 2·context·session-flow·new-agent) |
| AR-101 | `15003899` | 자식으로 다시 실행되는 provider의 hook을 수락한다. 실행 안에서 루트까지 같은 provider로만 이어지는 프로세스를 처음 수락한 PID로 고정하고, 도구 shell을 거친 중첩 실행·다른 provider는 거절한다. CLI는 전달 실패만 warn | daemon 300, CLI 142·189, agent-native-hooks 30개(아래 AR-088 재발 포함) |
| AR-102 | `88cd04dd` | 같은 실행의 managed 명령이 view gate를 공유한다. attach·commit·ready·abort·detach만 배타이며 대기 없이 Busy | daemon 299, IPC 97, agent-host 34개, agent-cli-controls |
| AR-103 | `e47be214` | **동작 변경 없음.** 거부된 Codex Stop 뒤의 이어진 작업과 완료 뒤 늦은 progress가 reducer 입력으로 구분되지 않음을 테스트 4개로 고정 | daemon 298 |
| AR-104 | `5ab68988`, `3d78d585`, `ea623777` | vendor patch 기록에 `grid/resize.rs` 추가(0.26.0 crate와 diff로 두 파일만 다름을 확인). 공용 gate가 `CLAUDE_CONFIG_DIR`·`CODEX_HOME`·`GEMINI_CLI_HOME`·`CLAUDE_CODE_ENTRYPOINT`를 지운다. 사용자 문서와 이 절 | runner 30(1 skip), skills 17개 |
| AR-105 | `61a98b2e` | 다른 protocol의 Host를 「연결 불가」와 구분하는 typed 오류. Agents 영역 고지, New Agent·Resume·Session Flow·`flowmux agent`가 같은 이유를 표시. Host 교체·두 번째 Host·일반 tab 우회 없음 | IPC 99, daemon 304, CLI 143·189, GUI 974, agent-host 34개 |
| AR-106 | `04b0f401` | 루트 종료 시 presence를 지우고 exit status를 공개한다. 실행은 Running을 유지하고 행은 「Provider exited; background processes still running」 | daemon 301, GUI 행 test, agent-root-exit |
| AR-107 | `f896ce9c`, `2ce666a9` | GUI launch가 terminal theme 색을 싣고 Host가 provider 시작 전에 적용한다. relay는 attach 뒤 `ViewColors`를 보낸다. **protocol 16**. response-limit fixture의 고정 version도 16으로 수정 | IPC 98, daemon 304, CLI 143·189, GUI 972, live 4개(agent-host·agent-ready·new-agent·tab-fallback), agent-gui-response-limit 5개 |

남은 결정과 제한은 다음과 같다.

- **AR-103은 결정이 필요하다.** 현재는 grace가 지난 뒤 같은 turn의 root
  progress를 거절하므로, 다른 hook이 Stop을 거부해 이어진 작업은 다음 Stop까지
  Idle로 보이고 완료 알림이 먼저 나간다. 반대로 받아들이면 완료 뒤 늦은 hook이
  끝낼 사건 없이 Working을 되살린다. `hook_seq`·사건 종류·`stop_hook_active`
  모두 표시 시점에 두 경우를 가르지 못한다. 일반 tab에도 같은 reducer가 적용된다.
- **AR-088은 변경 전 제품에서도 로컬 재현된다.** agent-native-hooks의
  「notification focuses the moved pane for native input」 timeout이 AR-101 후보와
  보완 실행에서 한 번씩 났다. 같은 검사를 번갈아 6회씩 실행하면 `c6a513db` 제품은
  2회, 통합본은 3회 실패했고 모두 같은 지점이다. 실패 시 접근성 focus는
  terminal이 아닌 scroll pane이며 Agent view는 visible·ready다. 이번 수정이 만든
  실패로 보지 않으며 원인 미확정을 유지한다.
- AR-101: 설치된 Gemini CLI가 자식 프로세스로 다시 실행되는 구조는 확인했지만
  실제 provider의 managed live는 실행하지 않았다. exec하지 않는 wrapper가
  루트인 launch는 지원하지 않으며 Host가 실행마다 한 번 warn한다.
- AR-105: Hello 요청의 형태 자체가 바뀌면 이전 Host는 거절 사유를 줄 수 없다.
  실제 이전 버전 Host와의 GUI live는 실행하지 않았다.
- AR-106: `exit_status`는 wait status 원값이다(`exit 3`은 768). 종료 행의 숫자
  표시는 고치지 않았다.
- AR-107: `flowmux agent run`은 색을 싣지 않는다. 연결된 채 바꾼 theme은 다음
  attach에 반영된다. 응용 프로그램이 바꾼 palette(OSC 4)는 view에 재생하지 않는다.
- 검증 중 Team worker 환경의 provider home 변수 때문에 `flowmux-cli` test가 실제
  hook 설정과 설치된 Team helper를 다시 썼다. helper는 installer backup으로
  복구했고 gate는 `3d78d585`로 고쳤다. hook 설정의 이전 내용은 backup이 없어
  대조하지 못했으며 설치된 CLI의 `hooks doctor`는 Installed로 보고한다.

### 전체 Linux gate — 연속 통과 없음, 보완 실행으로 범위 확인

`bash scripts/test-ci.sh linux extended`를 네 번 실행했고 모두 exit 1이다. 매번
workspace test는 **2,476개 통과·실패 0·13개 ignored**였다.

| 실행 | source | 통과한 regression phase | 멈춘 phase와 원인 |
| --- | --- | --- | --- |
| 1 | `61a98b2e` | 6 | browser-control. host PATH에 ffmpeg·H.264 decoder가 없다. 로컬 도구의 wrapper·`GST_PLUGIN_PATH`·`LD_LIBRARY_PATH`로 해결(환경) |
| 2 | `61a98b2e` | 9 | browser-feedback 「selection captured」 timeout. 단독 재실행 4회 통과, 원인 미확정 |
| 3 | `61a98b2e` | 11 | skills의 Team sample. gate가 `CLAUDE_CODE_ENTRYPOINT`를 지우지 않았다. `ea623777`로 수정 |
| 4 | `ea623777` | 25 | agent-gui-response-limit. fixture가 protocol 15를 보냈다. `2ce666a9`로 수정 |

4차 실행은 사전 9개와 regression 1–25번째를 연속 통과했다. `2ce666a9`에서
26–55번째 30개 phase를 실패해도 계속하는 방식으로 이어 실행해 **29개 통과**,
agent-native-hooks 1개 실패(위 AR-088)다. ssh-workspace를 포함해 끝까지 실행됐고
AR-098의 GUI 종료 timeout은 이번에 나타나지 않았다. 원인 해결로 단정하지 않는다.

이 결과는 **단일 연속 gate 통과가 아니다.** 4차 실행의 앞부분과 보완 실행을
합친 범위 확인이며, 그 사이의 변경은 test fixture 한 줄이다. macOS는 실행하지
않았다. [기록](design-assets/agent-runtime-independent-review-20261118.json)에
실행별 phase 결과·로그 hash·agent-native-hooks 비교를 보관한다.

## 최신 로컬 검증 — 2026-11-18 KST, 최종 SSH 종료 실패

- 최종 제품·테스트 source `0e17ae65`와 동일한 제품으로 Linux extended 전체 gate를
  로컬에서 한 번 연속 실행했다. **52/53개 phase 통과 후 마지막 SSH GUI 종료가
  20초 timeout으로 실패해 전체 exit 1**이다. GUI unit은 969개 통과·실패 0개·
  ignored 4개다. 이 결과를 단일 전체 gate 성공으로 보고하지 않는다.
- 실제 managed 드래그 24개·창 밖 분리 9개·native hook 30개·New Agent 10개·
  종료 14개는 통과했다. 출력 부하 native 클릭 100회 누락 0회·상태 피드백
  p95 39.133ms, 7-Agent peak 1,934,224KiB·재연결 7.889–10.839초다.
- [실패 증거](design-assets/agent-runtime-local-linux-uninterrupted-failure-20261118.json)에
  모든 phase receipt와 원본 로그 hash를 보관한다. 최초 SSH 도구 경로 누락의
  preflight exit 2도 보존한다. 실패한 gate가 상세 GUI 파일을 삭제한 문제는
  AR-097로 수정했으며, GUI 종료 원인은 AR-098로 별도 추적한다.
- SSH 전체 단독 후속은 exit 0이며 원격 코드 리뷰·키링 재시작·피드백 Send까지
  통과했다. 실제 프로그램 상태 검사 직후 GUI 정상 종료도 별도 live로 통과했다.
  후속 정상 종료는 최초 원인 해결이나 전체 gate 성공으로 합산하지 않는다.
  AR-098의 다음 timeout에는 요청 창 ID·남은 창 상태·스레드 대기를 cleanup 전에
  기록한다. [후속 증거](design-assets/agent-runtime-ssh-close-followup-20261118.json)에
  코드 버전 차이와 검사 범위를 명시했다. 최초 runner 환경을 복원한 SSH phase도
  251.614초·exit 0이다. 두 격리 GUI가 저장을 공유하며 연속 출력 중 닫는 표적
  live도 0.446초에 정상 종료했다. 원인 미확정 상태를 유지하고 실패 후 새
  푸시·CI dispatch는 하지 않았다.
- 후속 조사에서 IPC `ping`을 X11 창 준비로 오인하는 검사 도구 결함(AR-099)을
  재현·수정했다. 수정 전 새 회귀 2개 실패, 수정 후 harness unit 12개와 격리
  live 검증이 통과했다. 최초 SSH 종료 실패의 원인으로 단정하지 않는다.
- AR-006의 terminal 최대 부하에 lifecycle ledger 상한을 함께 채운 격리 live
  3개가 통과했다. Host peak 357,200KiB·출력 중 제어 응답 최대 0.029531초와
  원래 PID/birth·단일 spawn·새 한글 입력·자연 종료·Host 회수를 확인했다.
  이 조합 검증을 전체 allocation 회계나 AR-098 해결로 확대하지 않는다.
- 기존 동일 제품 source의 CI run `44166374894`는 4개 job 성공이다. 이번 로컬
  실패를 그 결과로 덮지 않는다. **미해결 medium은 기존 7건과 AR-098의 8건**이며,
  계획 전체 완료가 아니다. 사용자 GUI PID `1663486`과 설치본은 유지한다.

## 이전 로컬 검증 — 2026-11-18 KST, AR-091 이전 binary

- `bash scripts/test-ci.sh linux extended`가 단일 연속 실행으로 **exit 0**이다.
  사전 gate 9개와 regression 52개 phase, workspace **2,456개 통과·13개 ignored**를
  확인했다. SSH 도구는 AR-090에서 확인한 격리 경로를 명시했다.
  [전체 기록](design-assets/agent-runtime-local-linux-full-20261118.json)에 명령,
  각 phase receipt와 원본 로그 hash를 보존한다. 후속 확인에서 64개 증거 파일의
  hash도 원본과 일치했다.
- 이 전체 gate의 제품 binary는 AR-091 가로 배치 수정 전이다. AR-091 이후의
  빌드·관련 회귀·live 검증은 해당 항목에서 별도로 기록하며 전체 gate 결과와
  혼합하지 않는다. 전체 gate 실행 중 추가한 종료 예외 진단도 기록에 명시했다.
- AR-091 최종 binary의 후속 native drag 24개·종료 14개·native hooks 30개가
  통과했다. 같은 binary의 7개 실행 전체·양쪽 IPC pool 동시 부하 검사도
  128.399초·exit 0이며 Host VmHWM은 1,983,304KiB다. 아래 후속 항목에 기록한다.
- `git fetch origin` 후 main은 여전히 `82554c97`이며 현재 작업 이력에 포함된다.
  로컬 검증 뒤 `53f36ee8`까지 push하고 단일 확인 run
  [44145082591](https://github.com/flowmux-ai/flowmux/actions/runs/44145082591)을
  요청했다. 이 run은 새 7개 실행 fixture의 추가 전 source다.
  사용자 GUI와 설치본은 유지하며 추가 fixture도 로컬 검증 뒤 커밋·push한다.
- **medium 6건(AR-006·008 macOS·010·071·073·088)은 미해결**이다. 로컬 통과로
  과거 CI 실패의 원인이 해결됐다고 판정하지 않으며 계획 전체 완료도 선언하지 않는다.

## 이전 상태 — 2026-11-17 17:02 UTC (11월 18일 KST)

- 로컬 검증 후 push한 `428e3b81`의 단일 run
  [44131022038](https://github.com/flowmux-ai/flowmux/actions/runs/44131022038)은
  editor web·headless stress 성공, Linux 진행 중, macOS 대기 중이다. 후속
  harness 변경은 이 run의 source에 포함되지 않으며 새 dispatch는 하지 않았다.
- AR-006 후속으로 최대 폭 resize 캐시와 rich 두 화면을 6개 실행에 동시에
  채우고 기존 실행·전송 6개·regular/control IPC pool 포화를 함께 검사했다.
  격리 Host/GUI **33개·exit 0**, peak **1,444,024KiB**다. same-process 새 입력·
  tab/GUI 종료 뒤 복원·예산 재사용·자연 종료와 Host 회수도 통과했다.
  [검증 기록](design-assets/agent-runtime-concurrent-max-width-20261118.json)에
  workload·source/binary/log hash와 한계를 보존한다. 전체 allocation 회계인
  AR-006은 열려 있으며 이 검사를 전체 gate 성공으로 보고하지 않는다.

- 최신 `origin/main` `82554c97` 포함을 fetch 후 재확인했다. AR-089에서 Linux의
  보이는 pane을 선택 즉시 포커스하고 이전 지연 요청이 덮어쓰지 않도록 수정했다.
- 같은 제품 소스의 로컬 Linux extended 실행에서 workspace **2,456개 통과·
  13개 ignored**, 실동작을 포함한 regression **50개 phase 통과**다. 마지막 SSH는
  도구 누락으로 exit 2였으며, 도구를 명시한 SSH 전체 GUI 후속 실행이
  **258.549초·exit 0**으로 통과했다. 합산 coverage 51개이며 **단일 전체 gate
  성공이 아니다**. [새 로컬 검증 기록](design-assets/agent-runtime-local-linux-20261118.json)에
  최초 실패와 실제 phase별 receipt·원본 log hash를 보존한다.
- AR-090으로 공용 gate와 Linux runner가 빌드 전에 SSH 도구를 실행 검사한다.
  fixture 회귀 4개·runner 29개(28개 통과·1개 skip), 실제 누락 환경의 빠른
  실패와 Flowmux shim 거절을 검증했다. 제품 소스는 이 검사 보완으로 바뀌지 않았다.
- workspace all-target Clippy `-D warnings`, ignored stress 5개, editor web
  41개·build·12개 asset 검증, 느린 GTK에서 알림 입력·종료 14개도 통과했다.
  Wayland native managed 경로 24개는 통과했으며 AT-SPI 메뉴 재열기 실패는
  Flowmux 없는 GTK 축소 재현과 함께 별도 기록했다.
- 사용자 GUI PID `1663486`과 설치본은 유지했다. 기존 원격 결과는 아래 이력이며,
  새 변경의 CI 성공으로 보고하지 않는다. 푸시는 위 로컬 검증 완료 뒤 진행한다.
- **medium 6건(AR-006·008 macOS·010·071·073·088)은 미해결**이다. 후속 통과로
  과거 간헐 실패의 원인을 해결 처리하지 않는다. 계획 전체 완료와 medium 0건을
  선언하지 않으며 자동 idle 종료도 비활성으로 유지한다.

## 이전 상태 — 2026-11-17 15:44 UTC

- 현재 브랜치 `agent/runtime-audit-20261117`는 최신 `origin/main` `82554c97`를
  포함한다. 후속 제품 변경 AR-087은 Host 중계 응답의 JSON 구조 상한을 추가한다.
- push 전 로컬 Linux의 필수 phase **51개가 모두 통과**했다. 최초 Session Flow
  실패와 후속 SSH 실행 파일 누락을 수정하고 미실행 단계를 직렬로 이어 검증한
  결과이며 **단일 전체 gate 통과는 아니다**. 증거 파일 67개의 hash와 전체 phase
  집합은 [최종 로컬 기록](design-assets/agent-runtime-local-linux-final-20261117.json)에
  보관한다. workspace 전체·all-target Clippy·실제 native drag·탭/GUI 종료와
  동일 PID/birth 재연결·SSH·에디터·입력 부하 검증을 포함한다.
- 위 로컬 검증 뒤 push한 `3fc7f647`의 run
  [44099774658](https://github.com/flowmux-ai/flowmux/actions/runs/44099774658)은
  editor web·headless stress가 성공했다. Linux는 사전 8개와 regression 21개
  phase 통과 뒤 `agent-host`의 만료된 control 연결에서 실패했다. 같은
  `BrokenPipe`를 로컬에서 재현하고 AR-086으로 수정·회귀 검증했다. 이 결과를
  원격 전체 gate 통과로 보고하지 않는다.
  macOS는 14개 phase 통과 뒤 `macos-dmg-update`에서 AR-071이 재발해 exit 1이다.
  기존 앱·실행은 유지됐으나 fixture DMG device가 남았고 updater stdout/stderr는
  비어 있었다. attach/detach 개별 상태가 없어 원인은 아직 특정하지 못했다.
  [새 macOS 실패 증거](design-assets/agent-runtime-final-macos-ci-20261117.json)를
  보관하며 editor-recovery 등 이후 phase의 통과를 주장하지 않는다.
- 추가 로컬 검증은 현재 Host fixture 33개와 실제 Codex 짧은 전체 시나리오
  5개 통과다. 수정 후 실제 Codex 31분 전체 재실행도 **5개 통과·exit 0**이다.
  94개 표본·1,860.003초 생존 뒤 같은 실행의 새 한글 응답과 명시적 종료·
  빈 Host 회수까지 검증했다. 최초 재접속 검사 실패도 기록에 유지한다.
  [AR-085 기록](design-assets/agent-runtime-codex-headless-work-20261117.json)과
  [AR-086 기록](design-assets/agent-runtime-host-idle-reconnect-20261117.json)에
  실패와 통과 범위를 구분한다.
- 로컬 검증 뒤 push한 `1ffd7c26`의 run
  [44109493204](https://github.com/flowmux-ai/flowmux/actions/runs/44109493204)은
  macOS **48개 phase·전체 gate exit 0**이다. 기본·One Dark·GitHub Light의
  실제 에디터 상태 대비 최솟값 5.0524와 `editor-recovery` 통과를 확인해
  AR-068을 닫는다. DMG가 이번에 통과한 사실로 AR-071의 원인 미확정을
  해소하지 않는다. 이 CI는 후속 IPC 구조 제한 변경 전 source다.
- 같은 run의 Linux는 **43개 phase 통과 뒤** `agent-native-hooks`의 알림
  native 입력 검사에서 실패했다. 로컬 전체 30개와 같은 알림 시나리오
  10회가 통과했지만 원인이 확인되지 않아 AR-088 medium으로 추적한다.
  실패 시 포커스·키 상태·pane 화면·Host와 GUI 로그를 보존하도록 보강했다.
  Linux 전체 gate 성공을 주장하거나 CI를 다시 실행하지 않는다.
- 후속 AR-087의 IPC 구조 제한 수정은 로컬 IPC 106개, workspace Clippy·build,
  격리 GUI 5개·실제 Codex 5개 검증 후 커밋했다. 이전 CI의 증명과 구분한다.
- 미해결 medium은 **6개**(AR-006·008 macOS·010·071·073·088)다.
  [미해결 표](#미해결-액션-아이템--완료-판정을-막는-항목)가 현재 잔여 조건이다.
  AR-069는 해결됐으며 이전 상태 기록에 등장하는 미해결 개수는 당시 기준이다.
  provider 전체 완료와 안전한 idle 종료 계약이 없는 자동 회수는 비활성이다.

## 이전 상태 기록 — 각 후속 항목의 검증 시점 기준

- run [44073400067](https://github.com/flowmux-ai/flowmux/actions/runs/44073400067)의
  macOS job은 `macos-dmg-update`에서 실패했다. CLI가 빠진 DMG의 설치 거절과
  기존 앱·실행 보존은 통과했으나 `hdiutil info`에 fixture 이미지가 남았다.
  AR-071로 등록하고 다음 실패에서 updater stdout/stderr와 남은 device 정보를
  보관하도록 한다. 정확한 원인은 미확정이며 native 해결로 판정하지 않는다.
  에디터·Session Flow 단계 전 실패여서 AR-068·069도 유지한다. Linux는 진행
  중이며, AR-070 source의 run
  [44075770328](https://github.com/flowmux-ai/flowmux/actions/runs/44075770328)은
  기존 실행 뒤에서 대기 중이다. 새 전체 gate를 dispatch하지 않았다.
  현재 미해결 medium은 **6개**(AR-006·008 macOS·010·068·069·071)다.

- AR-006의 full diff와 재연결 snapshot이 같은 화면을 각각 할당하던 경로를
  하나의 immutable Arc로 공유한다. 최초 화면·alternate 전환·resize·history
  삭제와 부분 diff를 회귀로 고정했으며 수정 전 공유 검사는 실패했다.
  daemon **293개 통과·1개 ignored**, 최대 폭 unit peak **295,600KiB**,
  같은 격리 Host/GUI 부하 **3개 통과·exit 0**, peak **346,424KiB**다.
  원래 PID/birth·새 한글 입력·자연 종료와 빈 Host 회수를 확인했다.
  [검증 기록](design-assets/agent-runtime-shared-full-frame-20261117.json)에
  범위를 보관한다. 전체 회계인 AR-006은 열려 있으며 진행 중인 두 원격 run은
  이 후속 수정 전 source다.

- AR-070은 GUI 작업 완료 후 응답을 읽지 않는 caller가 큰 결과를 계속 보유하는
  결함이다. 한 managed 실행의 느린 연결 56개에서 Host peak **529,756KiB**를
  재현했다. 응답 예약을 socket 전송 완료/실패까지 유지해 7개만 수락하고 나머지
  49개는 GUI 효과 전에 거절한다. 같은 격리 live **5개 통과·exit 0**, peak
  **107,660KiB**, 연결 해제 뒤 용량 재사용·원래 PID/birth와 새 한글 입력·자연
  종료 및 빈 Host 회수를 확인했다. IPC **96개**, IPC/daemon Clippy와 matching
  GUI/daemon/CLI build도 통과했다. 아래 기존 CI는 이 수정 전 제품을 검증한다.
  전체 메모리 회계인 AR-006과 미해결 medium 5개는 유지한다.

- AR-070 이전 제품 `f8f149d1`의 전체 gate는
  [44073400067](https://github.com/flowmux-ai/flowmux/actions/runs/44073400067)에서
  시작됐다. macOS 후속 실패와 나머지 job의 최신 상태는 위 AR-071 기록을 따른다.
  이전 제품 `e9c8dd13`의
  run `44071483159`는 web·stress 성공 후 native 두 job을 취소했다. 같은 workflow의
  직렬 대기를 해소하고 두 수정을 포함한 최신 source로 검증을 모으기 위한
  취소이며, native 성공이나 실패로 판정하지 않는다.
  직전 run의 Linux `managed-session-flow` 선택 실패는 AR-069로 등록한다.
  canvas 접근성 역할·이름·설명을 추가하고 추정 좌표 클릭을 실제 Tab 탐색과
  포커스·선택 변경 관측으로 바꿨다. 수정 전 제품은 새 회귀에서 canvas 조회로
  실패하고, 수정 후 격리 시나리오는 9개 모두 통과했다. 이전 CI의 정확한
  포커스 상태는 확보하지 못했으므로 원인을 확정하거나 remote 성공으로
  바꾸지 않는다. 이 검증 시작 당시 미해결 medium은
  **5개**(AR-006·008 macOS·010·068·069)다.

- AR-068은 후속 macOS gate의 에디터 대비 실패다. 기본 테마의 오류 글자 대비
  **4.4515**를 격리 Flowmux WebKit의 제품 CSS에서도 재현했다. dark 오류색을
  밝게 조정한 뒤 기본 **5.0503**, One Dark **5.5429**, GitHub Light **5.6556**이다.
  native 검사는 dark/light 구분만 기다리던 조건을 실제 배경·글자 변수 적용
  대기로 바꾸고 기본 테마도 포함한다. web **41개**, 자산 rebuild·verify와 GUI
  build·format은 통과했다. [검증 기록](design-assets/agent-runtime-editor-status-contrast-20261117.json)에
  근거를 남긴다. **macOS 재검증 전이므로 AR-068은 열려 있다.**

- AR-067은 Host의 GUI 전달 응답이 무제한으로 커질 수 있는 경로를 제한한다.
  실제 managed 실행의 5MiB browser eval 결과가 수정 전 그대로 반환됐고,
  수정 후에는 4MiB wire 상한 오류와 outcome-unknown·재시도 없음 안내로 바뀐다.
  작은 한글 응답·효과 1회·같은 PID/birth와 새 한글 입력·자연 종료 및 빈 Host
  회수의 격리 live **3개**가 통과했다. daemon **292개·1개 ignored**, IPC
  **96개**, Clippy·build도 통과했다. [검증 기록](design-assets/agent-runtime-gui-response-limit-20261117.json)에
  근거를 남긴다. 전체 allocation 회계인 AR-006 등 기존 medium 3개는 유지한다.

- AR-006의 registry 경계를 확인했다. 60KB lifecycle 문구를 가진 실제 활성
  실행 **7개**, 종료 이력 **64개**, 느린 일반 조회 **60개**에서 목록 wire
  **551,474 bytes**, 격리된 exact test process peak **94,024KiB**, **1개 통과**다.
  실제 실행 하나의 종료가 `presence`를 비우는지 확인하고 그 종료 metadata를
  복제해 이력 수를 채웠다. 활성 문구 원문·완료 상태는 보존한다.
  [검증 기록](design-assets/agent-runtime-registry-text-memory-20261117.json)에
  범위를 남긴다. 제품 수정 없이 테스트만 추가하며 전체 Host 회계는 미완료다.

- AR-006의 최대 폭 회귀에 **80×500→79×500→80×500**을 추가했다.
  primary snapshot 세 개와 geometry 변경의 전체 diff·임시 복사본을 겹치며,
  unit **1개 통과**, peak **328,628KiB**다. 같은 실제 격리 Host·GUI도
  **3개 통과·exit 0**, peak **392,576KiB**, 제어 응답 최대 **0.000691초**다.
  원래 PID/birth·새 한글 입력·자연 종료와 빈 Host 회수를 확인했다.
  [검증 기록](design-assets/agent-runtime-resize-full-frames-20261117.json)에 범위를
  보관한다. 관측된 예약 여유는 **640KiB**뿐이며 전체 회계의 증명이 아니다.
  제품 변경은 없고 AR-006은 열려 있다.

- AR-067 이전 제품 커밋 `55a7da31`의 Linux/macOS 전체 gate는
  [44066722901](https://github.com/flowmux-ai/flowmux/actions/runs/44066722901)에서
  Linux는 `managed-session-flow`의 child 선택 실패(AR-069), macOS는
  `editor-recovery` 대비 실패(AR-068)로 끝났다. editor web과 headless stress는
  성공했다. 아래 이전 run 결과로 대신 판정하지 않는다.
  이 run은 AR-067의 원격 검증을 포함하지 않는다.

- AR-066은 계속 읽을 수 있는 PTY가 제어 요청 처리를 지연시키는 결함이다.
  같은 실제 Host 부하에서 heartbeat가 **20.654초** 걸렸고 GUI 관리 연결도
  재연결됐다. actor가 매 순환 양보하도록 수정한 뒤 제어 요청 최대
  **0.128초**와 메모리 peak **337,616KiB**, GUI 재연결·새 한글 입력·종료
  검증이 통과했다. Linux 공통 gate에 이 회귀를 포함한다.

- AR-064의 후속 macOS run
  [44061947951](https://github.com/flowmux-ai/flowmux/actions/runs/44061947951)은
  `0d6ee75d`에서 **전체 성공·CI_GATE_EXIT=0**이다. 표시되는 제품 상태 CSS의
  4.5 대비 검사와 native GUI crash·Sessions Flow가 통과하여 AR-064를 해결했다.
  [원격 검증 기록](design-assets/agent-runtime-macos-gate-20261117.json)에 phase와
  artifact hash를 보관한다. 이 run은 AR-065의 제품 수정 이전이다.

- AR-065는 history가 없는 grid의 resize cache가 셀 payload를 보관하는 결함이다.
  79×499→79×500→80×500 변경 후 두 화면과 primary snapshot 세 개를 겹치자
  terminal 단독 peak **410,768KiB**로 실행 예약 **393,216KiB**를 초과했다.
  resize 후 비표시 cache를 제거하면 같은 검사 peak는 **301,064KiB**이며,
  daemon **291개·1개 ignored**, upstream **132개**, Clippy·build가 통과했다.
  격리된 실제 Host·GUI에서도 peak **339,464KiB**, 원래 PID/birth 재연결·새
  한글 입력·자연 종료와 빈 Host 회수 **3개 통과·exit 0**으로 AR-065를 해결했다.
  [검증 기록](design-assets/agent-runtime-resize-cache-memory-20261117.json)에
  수정 전후 근거를 보관한다. 전체 Host 회계인 AR-006은 계속 미해결이다.

- AR-006의 최대 폭 검사에 저장 제목 4,096개·긴 링크 입력·16MiB primary
  snapshot과 alternate snapshot 세 개를 함께 보관하는 부하를 추가했다.
  초기 geometry는 **1개 통과·exit 0**, peak **245,508KiB**다.
  이후 1×500→80×500의 유효한 높이 변경으로 두 grid의 cache를 채운 경우도
  **1개 통과·exit 0**, peak **329,588KiB**였다.
  [조합 검증 기록](design-assets/agent-runtime-combined-max-width-memory-20261117.json)에
  범위를 남긴다. terminal 단일 부하이며 전체 Host 회계의 증명은 아니다.

- AR-006의 최대 폭 회귀를 추가했다. 유효한 80×500(4만 셀)의 두 화면·32개
  결합 문자·보관 snapshot 세 개를 겹친 격리 검사가 **1개 통과·exit 0**, peak
  **186,156KiB**, 현재 snapshot **7,252,152 bytes**였다.
  [검증 기록](design-assets/agent-runtime-max-width-memory-20261117.json)에 범위를
  남기며, 이 단일 부하로 전체 Host allocation 회계나 macOS를 증명하지 않는다.

- AR-063은 Agents 실행별 `Automatic cleanup paused` 안내와 안전한 완료·유휴
  종료 확인 미지원 사유를 추가했다. GTK 회귀 **1개**, 실제 격리 GUI의 native
  cross-window drag·tab 종료 후 Background 생존·표시·같은 PID 입력·crash 복원
  **11개 통과·exit 0**이다. 자동 회수 구현이나 AR-010 해결로 간주하지 않는다.

- AR-061은 Sessions 검사 driver가 부모 X 화면의 다른 접근성 버스에 연결되는
  실패를 재현·수정했다. 하네스 **7개**, 같은 실패 조건의 실제 Session Flow
  **9개**가 통과했다. Linux 후속 run
  [44054768870](https://github.com/flowmux-ai/flowmux/actions/runs/44054768870)은
  `92e4e334`에서 **전체 성공·CI_GATE_EXIT=0**으로 완료됐다.
  [원격 검증 기록](design-assets/agent-runtime-linux-gate-20261117.json)에 phase 결과와
  artifact 해시를 보관한다. AR-056을 해결로 전환한다. AR-062의 숨은 행 캐시까지 채운 후속 Host live는
  **34개 통과·exit 0**, 7실행·6전송·76 IPC peer의 peak **1,424,804KiB**였다.
  전체 allocation 회계 검증을 대신하지 않는다.

- AR-060은 넓은 history를 좁힐 때 upstream 행 capacity가 남는 결함을 수정했다.
  같은 회귀의 peak 증가가 **331,976→19,668KiB**로 감소했고 daemon library
  **290개 통과·1개 ignored**, upstream library **132개 통과**다. 상세 live 근거는
  아래 AR-060에 기록한다. AR-006의 전체 회계 검증을 대신하지 않는다.

- 최신 Host 제품에는 AR-048 lifecycle ledger 제한과 AR-053 형제 경로 누적 제거,
  AR-059 Linux 자식 PID map 제거를 적용했다. daemon library **289개 통과·1개 ignored**,
  daemon all-target Clippy·build·format·공백 검사를 통과했다.
- AR-053 이후 실제 중첩 cgroup의 fast-detach·자식 회수·실행별 종료 **3개**와
  격리 GUI의 부모 종료·같은 자식 PTY 재연결·새 입력·컨텍스트·알림 **5개**가
  통과했다. 상세 로그와 artifacts는 아래 AR-053에 기록한다.
- 앞선 전체 사용자 서비스/Host GUI **33개**와 native managed Agents drag
  **24개** 통과 근거는 AR-048 및 이전 절에 유지한다. 새 scope 순회 변경의
  장시간 검증으로 이 결과를 확대하지 않는다.
- AR-055의 managed Working spinner를 적용했다. 최신 제품의 native 클릭 100회·
  누락 0회·semantic feedback p95 **40.35ms**, spinner pixel 변화와 native managed
  drag **24개**를 확인했다. GTK 회귀·build·Clippy도 통과했다. frame time의 전체
  수용 검증은 아니며 상세 근거는 AR-055에 기록한다.
- AR-052의 **31분 실제 시간 관측과 후속 live 검사 34개가 통과·exit 0**이다.
  GUI 없이 같은 Host·Agent의 PID/birth와 Working·진행 증가를 372회 확인했다.
  이후 GUI 재접속·새 한글 입력·격리 GUI SIGKILL 후 재연결·자연 종료와 빈 Host
  회수까지 통과했다. [검증 기록](design-assets/agent-runtime-headless-31min-20261117.json)에
  로그·관측 파일의 해시와 제품 provenance를 보관한다. 이 Host는 AR-053 이전
  제품이며, 실제 provider/model 또는 안전한 자동 회수 검증이 아니다.
- 원격 run [44034889037](https://github.com/flowmux-ai/flowmux/actions/runs/44034889037)는
  stress·editor web 통과, Linux와 macOS 실패다. Linux의 초기 패널 진입 실패는
  AR-051로 고쳐 로컬 Session Flow **9개**를 통과했다. macOS 두 번째 Resume의
  fixture Host PATH 누락은 AR-049로 수정했다. 후속 run `44038755253`의 macOS는
  두 번째 launchd 시작 timeout으로 실패했다. AR-054는 기본 재시작 throttle을
  명시적 launch 정책에 맞추며 native 재검증 전에는 AR-037을 닫지 않는다.
  후속 Linux run [44040064061](https://github.com/flowmux-ai/flowmux/actions/runs/44040064061)도
  세션 패널 진입 버튼의 SHOWING 조회 timeout으로 실패했다. AR-056은 불필요한
  viewport 전제를 제거하고 실패 evidence를 추가한다. 로컬 중첩 D-Bus 실행은
  **9개 통과·exit 0**이나 remote 성공은 아직 확인하지 못했다.
  후속 Linux run [44046279141](https://github.com/flowmux-ai/flowmux/actions/runs/44046279141)은
  제품 검사 전에 CI runner의 단계 수 assertion으로 실패했다. AR-057에 원인과
  수정·검증을 기록하며, 이 실행은 AR-056의 원격 재검증 근거가 아니다.
  macOS run `44039159185`는 cancelled로 끝났으며 이유는 확인하지 못했다.
  AR-054를 포함한 [44042747337](https://github.com/flowmux-ai/flowmux/actions/runs/44042747337)는
  **macOS extended gate 전체 성공·CI_GATE_EXIT=0**이다. custom home·quoted cwd·
  긴 PATH·login shell PATH 및 두 번째 managed Resume·자연 종료·빈 Host
  비재시작을 통과하여 AR-037을 해결로 전환한다. AR-055 이전 source인 점과
  macOS containment·GUI 종료 생존 미검증 범위는 유지한다.
- 최신 로컬 Host GUI 검사는 동시 최대 실행·두 화면·결합 문자·저장 제목·보관 중
  전송 부하를 보강해 **33개 통과·exit 0**이다. 실행 7개·snapshot 전송 6개에서
  Host peak **815,296KiB (796.19MiB)**를 측정했다. 후속으로 일반 IPC 60개·제어
  IPC 16개의 1MiB 요청 버퍼를 함께 보관한 같은 33개 검사는 peak
  **993,748KiB (970.46MiB)**로 통과했다. 전체 할당 회계 증명은 아니며
  AR-006은 유지한다. 상세 근거는 아래 동시 부하 절과 검증 기록에 있다.
- AR-057 수정 커밋의 Linux run [44047212972](https://github.com/flowmux-ai/flowmux/actions/runs/44047212972)은
  stress·editor web 성공, Linux workspace의 config 경로 검사 실패로 끝났다.
  AR-058에서 공용 환경변수 잠금 누락을 수정했다. 이후 GUI 단계에 도달하지
  않아 AR-056 재검증 근거는 아니다. 후속 [44049485060](https://github.com/flowmux-ai/flowmux/actions/runs/44049485060)은
  `2afa06d5`에서 같은 Sessions 조회로 실패했다. 빈 접근성 tree를 확보했고
  AR-061에서 부모 X 화면의 접근성 버스가 잘못 선택되는 조건을 재현·수정했다.
  전체 remote gate 성공은 아직 대기 중이다.
- 실행 중인 사용자 GUI PID `1663486`과 설치본은 유지했다. 수정 제품은
  `target/debug`에 있으며 사용자 앱에는 아직 반영되지 않았다.
- macOS run [44051408083](https://github.com/flowmux-ai/flowmux/actions/runs/44051408083)의
  production GUI SIGKILL·같은 Host 연결·Agent identity·진행·저장 tab 복원·새 한글
  입력·자연 종료·빈 Host 회수가 **통과**했다. Sessions Flow도 통과했다. 전체
  gate는 이후 `editor-recovery`의 `github-light` 대비 assertion으로 실패했다.
  [native 검증 기록](design-assets/agent-runtime-macos-gui-crash-20261117.json)에
  revision·phase 결과·artifact 해시와 검증 한계를 기록한다. 일반 GUI 종료와
  macOS detached descendant containment까지 검증한 결과는 아니다.
- **전체 계획은 미완료다.** 미해결 medium **6개**(AR-006·008 macOS·010·068·069·071)를
  유지한다. provider의 안전한 자동 종료 계약이 없어 자동 회수는 비활성이며
  실제 provider/model 장시간 작업·macOS/Flatpak 수용 검증도 남는다.

## 검토 기록

### AR-001 · medium · GUI가 없는 동안 provider 화면 판정 누락

- 원인: Gemini·Antigravity의 잠정 Stop을 확정하는 ready 화면 판정이 GUI의
  `StateStore` 경로에만 있었다. Host는 Codex grace만 처리했다.
- 재현: 수정 전 `test-agent-native-hooks-gui.py`가 GUI 종료 후 Gemini의
  stable ready 화면에서 Idle 전환을 기다리다 실패했다.
  `/tmp/fm-headless-screen-before-live.log`, private root `/tmp/fm-gui-xyrxj82t`.
- 조치: 기존 화면 판정을 owner 공통 runtime으로 옮겨 GUI와 Host가 함께 사용한다.
  Host는 기존 terminal emulator의 최근 live 80행·현재 title을 관측한다.
  ANSI style·과거 scrollback·다른 화면의 내용은 판정에 섞지 않는다.
- 보존: native session·wait·sequence·600ms 안정화·continuation·outcome 및 알림
  중복 제거. UI 완료는 전체 provider 완료로 승격하지 않으며 자동 종료하지 않는다.
- 검증: daemon library 267개 통과·1개 ignored. 수정 후 격리 live 27개 통과·exit 0
  (`/tmp/fm-headless-screen-after-live.log`, `/tmp/fm-gui-ccvk5d5b`).
  재시작한 GUI의 Agents 완료·실패 표시, 동일 PID/birth·spawn 1회·새 입력도
  확인했다. 두 private runtime에 잔여 process가 없다. Workspace Clippy
  (`--all-targets -- -D warnings`), format·diff 공백 검사도 통과했다.

### AR-002 · medium · 승인 대기 전의 ready 안정화 시간 재사용

- 원인: 잠정 Stop의 ready frame을 관측한 후 승인 대기에 들어갔다가 해제되면,
  이전 `idle_frame`의 시간을 그대로 사용했다. 대기 해제 직후 완료로 표시할 수 있었다.
- 재현: 공통 reducer의 InstanceId 회귀 검사가 수정 전 실패했다
  (`/tmp/fm-runtime-wait-before-tests.log`).
- 조치: 대기 진입·해결 전이에서 provider 화면 안정화 기록을 무효화한다.
  승인·입력·session 대기와 같은 공통 ledger의 경계를 사용하므로 GUI가 없는
  경우와 일반 terminal에 같은 규칙을 적용한다.
- 검증: provider 관련 단위 검사 4개 통과. Gemini·Antigravity의 실제 Host에서
  대기 해제 직후 Working 유지와 새 ready grace를 확인했다. 격리 live 전체
  29개 통과·exit 0 (`/tmp/fm-runtime-wait-after-live.log`,
  `/tmp/fm-gui-rc0y4z93`). 해당 runtime의 잔여 process가 없다.

### AR-003 · medium · 화면에서 확인한 headless 실패의 오류 알림 누락

- 원인: Host의 전이 알림은 완료와 입력 대기에만 인과 ID를 부여했다.
  Cline·Gemini의 화면 실패는 Idle/Failed로 표시돼도 오류 알림이 없었다.
- 재현: GUI 없는 Host에서 실패 전이를 반복한 회귀 검사가 알림 0건으로 실패했다
  (`/tmp/fm-runtime-failure-before-tests.log`).
- 조치: Idle/Failed도 Host의 응답별 알림 receipt를 사용하고 Error로 보관한다.
  managed Antigravity의 native 실패 hook은 추가 Notify를 보내지 않아 중복을 막는다.
  일반 Tab-bound 실패 알림과 입력 대기의 NeedsInput 동작은 유지한다.
- 검증: 실패 반복·새 작업의 별도 실패 회귀 검사와 managed/Tab-bound CLI 검사
  2개 통과. 격리 live 30개 통과·exit 0
  (`/tmp/fm-runtime-failure-after-live.log`, `/tmp/fm-gui-5xr7w63n`). 실제
  Cline·Gemini 화면 실패와 Antigravity native 실패의 Error 1건 보관, GUI의
  bell 복원과 desktop 재전송 방지를 확인했다. stdout/event PASS 수가 일치하며
  해당 runtime의 잔여 process가 없다. 전체 Linux gate 결과는 별도로 기록한다.

### AR-004 · medium · 비활성 자동 회수를 작동하는 설정처럼 표시

- 원인: provider 전체 완료와 안전한 유휴 종료 계약이 없어 자동 회수가 비활성인데,
  Options는 timeout을 편집하게 하고 설정 시간 후 회수한다고 안내했다.
- 조치: 값을 보존하며 입력을 비활성화하고, 보이는 행에 `not active`를 표시한다.
  tooltip은 자동 회수가 없으며 Agents의 명시적 종료가 필요하다고 안내한다.
- 검증: Options·terminal search GTK 검사 21개 통과. 격리 New Agent live
  10개 통과·exit 0 (`/tmp/fm-runtime-options-after-live2.log`,
  `/tmp/fm-gui-f24occ1i`). 실제 표시·spin 비활성·30분 값 보존, 동일 PID/birth의
  tab·GUI 종료 생존과 복원을 확인했다. stdout/event PASS 수 일치와 잔여 process
  부재도 확인했다. 이는 자동 회수 기능의 구현이 아니다.
- fixture: Options 검사는 실행을 만든 후 수행한다. 실행 없는 Host는 10초 후
  종료하므로, launch 전 Options 검사로 시간을 늘리면 fixture가 이전 연결을 사용했다.

### AR-005 · medium · 공유 gate의 검색 스크롤 관측 경합

- 관측: 첫 Linux gate는 GUI 958개 통과·1개 실패·4개 ignored 뒤 exit 101이었다.
  `output_search_chunks_preserve_wraps_duplicates_and_alternate_screen`이 고정
  50ms 후 두 번째 검색 위치가 첫 번째보다 큰지 확인하다 실패했다
  (`/tmp/fm-runtime-review-shared-ci.log`). 같은 binary의 격리 단독 실행은 통과했다
  (`/tmp/fm-search-before-isolated.log`). 제품의 검색 결함으로 확정하지 않는다.
- 조치: 첫 검색의 scrollback 진입과 두 번째 검색의 전진을 GTK main loop에서
  각각 최대 1초 관측한다. 두 검색의 위치 비교 조건을 유지하고 실패 위치를 출력한다.
- 검증: 수정 후 Options·terminal search GTK 21개 통과
  (`/tmp/fm-runtime-options-search-tests.log`). 전체 gate의 workspace에서도 GUI
  959개 통과·0개 실패·4개 ignored로 재검증했다. 해당 gate는 이후 AR-012의
  전체 시간 제한에 도달했으므로 전체 통과로 기록하지 않는다.

### AR-012 · medium · 전체 Linux gate 예산이 순차 live 검증에 부족

- 관측: 두 번째 공유 gate는 workspace·build·browser·Team·Agent 연결·binding·
  드래그·새 창 분리를 통과한 뒤, native hooks 실행 중 outer 2400초 제한에
  도달했다. `regression.json`은 timeout·exit 124·2400.099초다
  (`/tmp/fm-runtime-review-shared-ci-final.log`). 제품 assertion 실패는 없었다.
- 조치: 전체 Linux regression 예산을 3600초로 늘린다. 개별 phase의 제한·
  실패 조건·테스트 범위는 유지한다. 이어지는 변경 후 공유 gate를 다시 실행한다.

### AR-007 · medium · owned PID마다 반복한 전체 프로세스 스캔

- 조치: procmon의 `ProcessTree`가 Linux `/proc/*/stat`에서 parent·session·birth를
  한 번 읽는다. Host 안의 모든 actor가 최대 200ms 동안 같은 inventory를 공유한다.
  재사용된 PID와 세션 ID는 birth가 일치하는 소유 프로세스로 검증한다. 이미 추적한
  detached 자식과 root 종료 후 같은 세션의 orphan은 계속 포함한다.
- 검증: procmon 17개·daemon 269개 통과(1개 ignored), Clippy 경고 0개.
  격리 Agent 2개·자식 16개에서 VTE 새 입력 10회가 모두 응답했고 최대 107ms였다.
  해당 2.56초 관측의 Host CPU는 0.47초였다. 이는 장시간·최대 32개 부하 보증은 아니다
  (`/tmp/fm-runtime-inventory-shared-live.log`, `/tmp/fm-gui-kuoopo9f`).
  Host 전체 live 회귀도 통과했다
  (`/tmp/fm-runtime-inventory-shared-host-live.log`).
- 제한: 첫 관측 전 빠르게 다른 세션으로 분리되는 자식의 containment는 AR-008로
  남는다. 이 최적화가 그 소유권 문제를 해결한 것은 아니다.

### AR-013 · medium · CI 정리 오류가 원래 결과를 덮어씀

- 관측: AR-012 실행은 timeout 124였으나, EXIT trap의 임시 `doc`·`gvfs`
  디렉터리 삭제 실패 때문에 shell 종료 코드는 1이 됐다.
- 조치: cleanup 실패 경로와 잔여 경로를 출력하고 최초 gate 종료 코드를 보존한다.
  공유 gate는 임시 경로 삭제를 계속 시도하며 제품 검증 실패를 숨기지 않는다.
- 검증: 실제 gate entry를 mock 도구로 실행해 cleanup 오류 73을 주입하고,
  정상 0·검증 실패 17·timeout 124가 각각 유지되는 회귀 검사를 통과했다
  (`/tmp/fm-runtime-ci-cleanup-tests2.log`). Shell 구문 검사도 통과했다.

### AR-011 · medium · 생성 순서와 종료 이력 표시 누락

- 조치: Host admission 순서를 유지하고 상태 변경으로 행을 재정렬하지 않는다.
  Exited/Failed 행은 `Recently ended`로 옮기며 종료 상태·코드를 표시한다.
  닫기는 같은 epoch의 종료 실행만 숨기고 launch receipt·최종 frame·native 대화
  기록은 보존한다. 요청은 idempotent이며 Host protocol은 v14다.
- Host 종료 후에도 마지막 종료 이력을 이 창에서 닫을 수 있다. 이 경우 bar와
  side panel의 로컬 숨김 상태를 공유한다. 다른 GUI로 offline 숨김을 전파하거나
  전체 binding reconciliation을 완료한 것은 아니다(AR-009).
- 실제 검사에서 새 실행의 모든 메뉴 항목이 비활성인 동안 GTK 4.14의 popup
  focus traversal이 null focus를 참조해 종료되는 문제를 재현했다. 메뉴 map 시
  필요한 경우 메뉴 버튼으로 focus를 복구하고, 포커스가 없는 Starting 행의
  popup 회귀 검사를 추가했다. fatal GTK 검사를 비활성화하지 않았다.
- 검증: Host·GTK 회귀 각각 1개 통과. 격리 live 14개 통과·exit 0
  (`/tmp/fm-runtime-recently-ended-verified-live.log`, `/tmp/fm-gui-wsjrpxno`).
  tab·GUI 종료 후 같은 실행 생존·입력, 정상/강제 종료·자식 reap·원래 shell 보존,
  종료 이력 닫기·Host 자연 종료·offline 닫기를 확인했다. stdout/event PASS 수가
  일치하고 두 GUI log에 GTK critical이 없다. Workspace Clippy 경고 0개,
  format·diff 공백·Python AST 검사를 통과했다. 전체 Linux gate는 별도 재실행한다.

### AR-014 · medium · 동기화 출력·REP의 숨겨진 화면 결합 문자 상한 우회

- 원인: parser 호출 이후에만 결합 문자를 제한해, 동기화 buffer 해제 중 화면이
  바뀌거나 REP가 문자를 반복하면 숨겨진 셀의 누적 문자를 제한하지 못했다.
- 재현: 이전 구현에 새 회귀 검사를 적용하면 셀에 결합 문자 1,000개가 남아
  상한 32개를 초과했다 (`/tmp/fm-runtime-combining-reproduced-before-tests.log`).
- 조치: 기존 ANSI handler를 위임하며 각 Unicode 입력 직후 해당 셀을 제한한다.
  명시적 동기화 해제·timeout 해제·REP에 같은 경계를 적용한다.
- 검증: terminal 13개·daemon 271개 통과(1개 ignored), workspace Clippy 경고 0개.
  격리 Host/VTE live 18개 통과 (`/tmp/fm-runtime-combining-host-live2.log`,
  `/tmp/fm-gui-oanrrl2j`). 실제 결합 문자 100,000개 출력·화면 전환 후 Host의
  snapshot 32개 상한, VTE 복원·새 입력, tab/GUI 종료·강제 종료 후 같은 실행
  복원을 확인했다. VTE 자체의 더 작은 결합 문자 제한은 별도로 허용한다.
- 범위: AR-006의 결합 문자 우회만 해결했다. 누적 hyperlink·Host 전체 메모리
  budget 검증은 아래 미해결 항목으로 유지한다.

### AR-015 · medium · 사용자 서비스 live 검사의 D-Bus 대상 오염

- 원인: 접근성 검사를 위해 process 환경을 private D-Bus로 바꾼 뒤,
  `--service-start`가 그 주소를 사용자 서비스 관리자의 주소로 재사용했다.
- 재현: 실제 `agent run` 경로가 서비스 관리자에 연결하지 못해 exit 1로 실패했다
  (`/tmp/fm-runtime-audit-20261117-service-before.log`). 제품의 launch 결함과 구분한다.
- 조치: 서비스 제출 client에만 Linux 사용자 관리자의 `/run/user/<uid>/bus`를
  지정한다. GUI·접근성 검사·상태 저장은 기존 private D-Bus/XDG를 유지한다.
- 검증: 같은 `--service-start` 검사 19개 통과·exit 0
  (`/tmp/fm-runtime-audit-20261117-service-after.log`). GUI와 Host의 cgroup 분리,
  Host와 Agent의 service cgroup 일치, Unicode 출력 상한, tab/GUI 종료·SIGKILL
  이후 동일 PID/birth·화면·입력 복원과 Host 자연 종료를 확인했다.

### AR-016 · medium · 화면에서 숨긴 과대 hyperlink를 Host가 계속 보관

- 원인: projection에서 4KiB 초과 URI를 제외해도 terminal cell·cursor template은
  원래 URI와 ID를 보관했다. ID는 projection에 나타나지 않아 같은 검증도 없었다.
- 재현: 60KB URI/ID 1,200개를 출력하면 Host RSS가 21,392→91,116KiB로
  69,724KiB 증가했다 (`/tmp/fm-runtime-hyperlink-before-live.log`). 숨겨진 화면과
  cursor template 회귀 검사도 수정 전에 실패했다.
- 조치: ANSI handler에서 URI와 ID 모두 기존 4KiB·제어문자 검증을 통과해야
  보관한다. 화면 전환·동기화 출력에도 적용하며 일반 hyperlink와 본문은 보존한다.
- 검증: daemon library 272개 통과·1개 ignored, workspace Clippy 경고 0개.
  격리 Host/VTE 19개와 실제 사용자 서비스 실행 20개가 모두 통과했다.
  RSS는 각각 19,600→20,196KiB, 19,200→19,820KiB였다
  (`/tmp/fm-runtime-hyperlink-after-live.log`,
  `/tmp/fm-runtime-hyperlink-service-after-live.log`). 새 한글 입력·GUI 종료·SIGKILL
  이후 동일 PID/birth 복원·자연 종료를 포함한다. 전체 메모리 budget(AR-006)을
  완료한 것으로 해석하지 않는다.

### AR-017 · medium · 표시 전 terminal focus가 managed 재연결 때 GUI를 종료

- 재현: Linux 공유 gate의 `agent-session-resume --context`가 이미 연 실행을
  다른 pane에 다시 열 때 exit 1이었다. 격리 단독 실행도 같은 위치에서 반복 실패했다
  (`/tmp/fm-runtime-audit-20261117-ci.log`,
  `/tmp/fm-runtime-context-crash-before.log`). 해당 gate는 workspace 2,396개
  통과·13개 ignored 뒤 실패했으므로 전체 통과가 아니다.
- 원인: 아직 한 번도 표시되지 않아 unrealized인 VTE에 `grab_focus`가 적용됐다.
  이후 ready 상태로 입력을 활성화하면 VTE 0.76이 없는 IMContext에
  `gtk_im_context_focus_in`을 호출했다. native stack은
  `refresh_agent_connections → vte_terminal_set_input_enabled`였고, 직전 진단은
  `realized=false, mapped=false, focus=true, registered=true`였다. 제거된 terminal의
  비동기 갱신이라는 가설은 이 관측과 달라 채택하지 않았다.
- 조치: 공통 terminal focus 함수는 mapped widget에만 focus를 적용한다.
  workspace/tab 활성화 때의 기존 focus 경로를 유지한다. input ready 검증이나
  GTK fatal-critical 검사를 완화하지 않는다.
- 검증: 숨겨진 terminal의 focus·첫 표시·input enable/disable·PTY 종료 회귀 검사가
  수정 전에 실패하고 수정 후 통과했다. terminal GTK 57개 모두 통과했다
  (`/tmp/fm-runtime-focus-unit-before.log`, `/tmp/fm-runtime-focus-unit-after.log`).
  기존 실패 시나리오 live 18개와 native tear-off live 9개도 통과했다
  (`/tmp/fm-runtime-context-focus-after-live.log`,
  `/tmp/fm-runtime-focus-tearoff-after-live.log`). 동일 PID/birth·spawn 1회·이동 후
  새 입력·tab/창 종료 후 Background 및 재연결을 확인했다. 전체 Linux gate는
  이 수정 이후 다시 검증한다.

### AR-018 · medium · pane 종료 후 논리·실제 키보드 포커스 유실

- 재현: AR-017 이후 전체 gate에서 중첩 `A | (B / C)`의 C를 닫아도
  논리 포커스가 B로 바뀌지 않았다. 단독 GTK 검사도 수정 전에 실패했다
  (`/tmp/fm-runtime-nested-focus-before.log`). 새 native 입력 검사에서는 C를
  닫은 뒤 B에 입력할 수 있어도, 이어 비활성 A를 닫으면 입력이 사라졌다
  (`/tmp/fm-runtime-nested-focus-live.log`).
- 원인: 종료 후 상태 갱신이 native focus 이벤트에만 의존했다. 살아남은 위젯도
  재배치 중 unmap되면서 실제 포커스를 잃었으며 논리 포커스 검사는 이를 놓쳤다.
- 조치: 기존 `focus_pane`으로 논리 상태와 native 요청을 함께 갱신한다.
  살아남은 하위 위젯이 실제 포커스를 가졌다면 재배치 후 mapped 상태와 pane
  선택을 확인하고 복원한다. 표시 전 VTE focus를 막는 AR-017은 유지한다.
- 검증: 종료 관련 GTK 13개 통과
  (`/tmp/fm-runtime-nested-focus-after.log`). 격리 GUI live 7개 통과·exit 0
  (`/tmp/fm-runtime-nested-focus-live-final.log`, `/tmp/fm-gui-tlzy61l6`).
  실제 X11 키 입력으로 활성·비활성 pane 종료를 모두 확인하며 기존 숨김·큰 화면
  read-screen 검사를 유지한다. Agent 재연결 live 18개도 통과했다
  (`/tmp/fm-runtime-nested-context-after.log`). 앞선 전체 gate는 workspace 단계에서
  실패했으므로 이후 live 단계가 실행된 것으로 기록하지 않는다.

### AR-008 · Linux 실행별 containment와 빠른 분리 자식 회수

- 재현: provider가 첫 관측 전에 double fork·`setsid`로 분리되고 root가 종료되면,
  자식이 살아 있어도 `Exited`로 표시했다. 새 live fixture가 수정 전에 실패했다
  (`/tmp/fm-runtime-containment-before.log`, `/tmp/fm-containment-7uzky1zh`).
- 조치: 기존 GUI의 systemd scope 코드를 공통 headless 모듈로 옮기고 검증·오류
  처리를 유지했다. Host는 실제 사용자 관리자 bus를 사용하며, provider의 pre-exec
  gate를 열기 전에 UUID별 scope에 배치한다. scope가 비기 전에는 종료로 판정하지
  않는다. SIGTERM 유예를 유지하며 명시적 Force는 해당 scope의 `cgroup.kill`을
  사용한다. containment를 확보하지 못하면 gate를 취소하고 Failed를 반환한다.
- scope 첫 관측 전에 끝난 orphan도 standalone Host의 subreaper가 회수한다.
  root PTY의 전용 waiter는 PID/birth로 보존한다. launch/actor 취소도 소유한
  scope만 정리한다. Linux에 polling fallback을 두지 않았다.
- 검증: daemon library 278개 통과·1개 ignored. 새 격리 live 3개 통과·exit 0
  (`/tmp/fm-runtime-containment-after.log`, `/tmp/fm-containment-k1247kh2`).
  root 종료 후 분리 자식 생존, 정상 종료 유예·명시적 Force·좀비 회수,
  다른 Agent PID/birth 보존과 빈 Host 종료를 확인했다. 실제 사용자 서비스 live도
  20개 통과·exit 0 (`/tmp/fm-runtime-containment-service-live.log`,
  `/tmp/fm-gui-vzvc_cao`). Host·Agent scope 분리, 탭·GUI 종료·SIGKILL 이후
  동일 실행과 새 입력을 확인했다. 기존 Linux gate에 containment fixture를 추가했다.
- Linux는 cgroup v2·사용자 관리자와 쓰기 가능한 `cgroup.kill`이 필요하다.
  macOS containment는 구현하지 않았으며 AR-008의 플랫폼 잔여로 유지한다.
  전체 Linux gate는 이 변경 이후 다시 실행한다.

### AR-019 · medium · 새 containment phase와 CI 검증 목록 불일치

- 관측: AR-008 후 전체 gate는 runner 자체 검사에서 regression·extended 두
  경우가 실패했다. 새 phase를 추가하면서 기대 목록과 단계 수 갱신이 빠졌다
  (`/tmp/fm-runtime-audit-20261117-scoped-ci.log`, entry phase exit 1).
  이 실행에서는 workspace·제품 live 검사를 실행하지 않았다.
- 조치: 기존 Python 호출 형식을 사용하고 containment를 두 suite의 필수 검사
  목록에 추가한다. regression 43개·extended 45개라는 정확한 단계 수를 확인한다.
- 검증: 해당 runner 회귀 검사 1개 통과·두 suite 모두 확인
  (`/tmp/fm-runtime-containment-ci-matrix.log`). 전체 gate는 수정 뒤 재실행한다.

### AR-020 · medium · admission ACK 유실 후 승인된 relay 종료

- 재현: Host가 목적지 lease를 승인한 뒤 GUI 응답 연결만 끊으면 실제 relay가
  종료됐다. 기존 GUI tab도 이미 닫힌 상태였다. 새 격리 GUI/PTY 검사는 수정 전에
  실패했다 (`/tmp/fm-runtime-ack-before-live.log`, `/tmp/fm-gui-xky29biq`).
- 조치: ACK 오류 뒤 같은 control 연결로 Host 목록을 한 번 조회한다. 같은 epoch·
  instance·lease·generation이 ready일 때만 승인 완료로 인정하고 relay를 유지한다.
  승인 mutation을 재전송하지 않는다. 미승인·교체된 lease·Host 변경·조회 실패는
  기존 오류 경로를 유지한다. 전역 저장 binding 조정은 AR-009로 남는다.
- 검증: 응답 유실 단위 검사가 수정 전에 실패하고 수정 후 4가지 상태를 확인했다.
  CLI library 140개 통과·1개 ignored, CLI binary 186개 통과 및 해당 integration
  target들이 모두 통과했다 (`/tmp/fm-runtime-ack-cli-suite.log`). 격리 GUI/PTY live는
  21개 통과·exit 0 (`/tmp/fm-runtime-ack-after2-live.log`, `/tmp/fm-gui-gs2oo81x`).
  실제 GUI 원본 tab 종료 뒤 응답 disconnect·timeout 각각에서 목적지 relay 생존과
  새 한글 입력, 같은 실행 PID/birth·spawn 1회 및 이후 GUI 재연결을 확인했다.
  stdout/event PASS 수가 일치하며 GUI log에 GTK critical이 없다. 첫 수정 후
  fixture는 한글 셀 사이의 ANSI style을 본문으로 비교해 실패했고, style을 제거한
  동일 본문 비교로 바로잡았다 (`/tmp/fm-runtime-ack-after-live.log`).
  CLI all-targets Clippy 경고 0개, format·diff·Python 구문 검사를 통과했고 세
  runtime에 잔여 process가 없다.

### AR-021 · medium · 부모 종료 후 살아 있는 자식 PTY에 재연결 거절

- 재현: root가 종료되고 자식이 같은 PTY에서 입력을 기다릴 때 Host는 Running을
  표시하지만 attach·ready·interrupt 요청은 root 종료만으로 거절했다. 새 단위 검사와
  실제 GUI 검사가 수정 전에 실패했다 (`/tmp/fm-runtime-root-unit-before.log`,
  `/tmp/fm-runtime-root-live-before.log`, `/tmp/fm-gui-rmwfggb5`).
- 조치: 네 요청 경로에서 root 종료를 실행 전체 종료와 구분한다. stopping 요청은
  계속 거절하며 root 회수·owned set·scope·PTY EOF를 사용하는 종료 조건을 유지한다.
- 검증: daemon library 279개 통과·1개 ignored, all-targets Clippy 경고 0개.
  격리 GUI live 3개 통과·exit 0 (`/tmp/fm-runtime-root-live-after.log`,
  `/tmp/fm-gui-r6mlyjd6`). 부모 회수 뒤 같은 자식 PID/birth의 화면·한글 입력,
  탭 종료 후 재연결·새 입력, 자식 종료 후 reap·Host 종료를 확인했다. stdout/event
  PASS 수가 일치하고 GTK critical과 두 runtime의 잔여 process가 없다. Linux CI에
  새 fixture를 포함했고 suite 목록 검사도 통과했다 (regression 44·extended 46단계).

### AR-022 · medium · 전체 GUI 검사에서 zoom 전제 조건 대기 누락

- 관측: 3842d49c의 전체 Linux gate는 workspace GUI 검사 954개 통과·1개 실패·
  4개 ignored로 중단됐다. `team_spawn_preserves_background_workspace_focus_and_zoom`이
  TeamSpawn 이전 zoom 진입에 실패했다 (`/tmp/fm-runtime-audit-20261117-scoped-final2-ci.log`).
  이후 live phase는 실행하지 않았다.
- 조치: fixture의 고정 100ms 대기를 mapped pane과 유효한 source/destination 배치의
  조건 대기로 바꾼다. zoom은 배치가 없으면 요청을 반환하므로 사후 대기만으로
  재시도되지 않는다. 기존 zoom·실제 focus·background TeamSpawn 검증은 유지한다.
- 검증: 원본의 단독 실행도 통과했으므로 제품 zoom 결함으로 단정하지 않는다.
  변경 후 격리 단독 검사 1개 통과 (`/tmp/fm-runtime-zoom-after.log`). 전체 gate에서
  같은 전제 조건과 기존 결과 assertion을 재검증한다.

### AR-023 · medium · 긴 제목의 저장 스택 누적

- 재현: 60KB 제목을 설정하고 저장하는 ANSI 명령을 1,200회 반복하면 Host RSS가
  25,012KiB에서 95,680KiB로 증가했다. 새 live 검사는 32MiB 증가 상한에서 실패했다
  (`/tmp/fm-runtime-title-before-live.log`, `/tmp/fm-gui-uajg35ha`). 단위 검사도
  현재 제목의 길이 검증에서 수정 전에 실패했다.
- 조치: ANSI Handler의 제목 입력을 UTF-8 문자 경계에서 4KiB로 제한한다. 엔진의
  4,096개 저장 제목 스택에도 제한된 제목만 복사되므로 제목 문자열 저장량은 약
  16MiB로 제한된다. 기존 중첩 저장·복원은 유지한다. 이 항목은 전체 VT/Host
  메모리 budget(AR-006)의 완료가 아니다.
- 검증: daemon library 280개 통과·1개 ignored, all-targets Clippy 경고 0개.
  수정 후 실제 GUI live 22개 통과·exit 0 (`/tmp/fm-runtime-title-after-live.log`,
  `/tmp/fm-gui-db5ot13n`). 같은 부하의 RSS는 24,024KiB에서 29,284KiB로 증가했고
  새 입력·응답 유실·tab/GUI 종료 뒤 같은 PID 재연결·자연 종료를 확인했다.
  stdout/event PASS 수 일치, GTK critical과 두 runtime의 잔여 process 없음.
  기존 Linux agent-host fixture에 부하 검사를 포함했다.
- 통합 바이너리의 실제 `agent run` 서비스 시작 경로도 23개 통과·exit 0
  (`/tmp/fm-runtime-title-final-service.log`, `/tmp/fm-gui-m_e573wx`). 같은 title
  부하의 RSS는 23,332→28,564KiB였다. GUI·Host service·실행별 scope 분리,
  GUI 정상 종료·SIGKILL 뒤 같은 PID/birth·spawn 1회·새 입력을 확인했다.
  stdout/event PASS 수가 일치하고 GTK critical과 해당 runtime의 잔여 process가 없다.

### AR-024 · medium · 부모 종료·재부모화 이후 자식의 컨텍스트·알림 거절

- 재현: AR-021의 살아 있는 자식 PTY는 재연결·입력이 가능했지만, 자식에서
  실행한 `identify`가 `managed context caller does not belong to this execution`으로
  실패했다 (`/tmp/fm-root-context-before.log`). 명시적 알림도 같은 검증 경로다.
- 원인: `require_execution_caller`는 root의 생존·부모 관계만 확인했다. Linux의
  실행별 containment가 유지되어도 root 종료나 `setsid`·재부모화로 관계가 사라지면
  살아 있는 소유 자식을 거절했다.
- 조치: Linux는 provider gate 해제 전에 확보한 실행별 cgroup 경로를 보관한다.
  호출자의 현재 cgroup이 해당 경로 또는 하위 경로인지 확인하며, 조회 전후 PID
  생성 시각도 검증한다. 다른 실행·외부 프로세스와 복사한 환경은 허용하지 않는다.
  native session을 바꾸는 root lifecycle 검증은 그대로 유지한다.
- 검증: daemon library 280개 통과·1개 ignored. 격리 GUI live 5개 통과
  (`/tmp/fm-root-context-after.log`, `/tmp/fm-gui-b6xqxg7b`). 최초·재연결한
  자식의 같은 PID/birth·spawn 1회·실제 VTE 입력·현재 pane/surface 조회·알림과
  외부 호출 거절, 자식 회수·빈 Host 종료를 확인했다. macOS의 기존 ancestry 경로는
  유지하며, 그 플랫폼의 containment 제한은 AR-008로 남는다.

## 계획 대비 재검토 경계

2026-11-17의 후속 검토는 아래 코드와 수용 조건을 대조했다. 표의 Linux live는
격리 fixture의 결과이며, 실제 provider 모델 작업·다른 플랫폼의 보장과 구분한다.

| 영역 | 확인한 코드와 결과 | 남은 한계 |
| --- | --- | --- |
| PTY 수명·소유 프로세스 | `TerminalActor::run`의 root 회수·owned set·scope·PTY EOF 조건과 `signal_owned`; Linux cgroup·부모 종료·재연결 live | macOS polling의 빠른 분리 자식은 AR-008 |
| 이동·입력 소유권 | `PrepareAttach`·`CommitAttach`·`GuiViewReady`·`ViewReady`, 정확한 ready lease·relay PID/birth와 GTK 목적지 검증; 취소·오류·cross-window·실제 drag | ACK와 registry 조회가 모두 실패한 복구 및 전역 영속 조정은 AR-009 |
| VT 저장·전송 | `BoundedTerm`, `Terminal::frame`, `Projection`, `Transfer`; OSC·Unicode·title 부하와 한글·alternate screen live | 개별 상한 외 누적 link·동시 projection/transfer 복사본의 전체 budget은 AR-006 |
| 저장·복원 | `save_window_owned_to`의 잠금·window별 merge, managed binding의 조회 우선·자동 native resume 금지, GUI의 superseded view 정리 | 늦은 source save·offline binding은 아래 영속 fencing으로 개선; ACK/조회 유실과 목적지 layout 복구는 AR-009 |
| lifecycle·회수 | 공통 reducer·headless 화면 판정·Stop grace와 `RetirementPolicy` 호출부 대조 | 전체 완료 검증과 원자적 idle 종료 수락의 production 연결은 AR-010; 자동 회수는 비활성 |
| GUI·일반 terminal 공존 | managed/Tab-bound 표시, 기존 tab 이동과 registry drag, close·detach 분리; native 입력과 같은 PID·spawn 1회 확인 | 사용 중인 구버전 GUI에 새 빌드를 적용하거나 기존 PTY를 전환하지 않음 |
| 빌드·공유 gate | locked build·workspace·Clippy, clean CI helper 경로, native smoke crate root와 플랫폼별 fixture | 원격 macOS 전체 결과·Flatpak 수용 검증은 별도 확인 필요 |

계획 §8의 spinner/출력 부하에서 native click 100회 누락 0회·feedback p95 100ms
목표와 실제 provider의 30분 작업은 이번 통과 수치에 포함하지 않는다.
현재 테스트의 짧은 수명·가상 시각·provider fixture를 그 검증으로 대체하지 않는다.

후속 AR-055에서 로컬 native click 100회·semantic feedback p95와 spinner pixel
변화를 검증했다. compositor frame time과 실제 provider 장시간 검증은 남아 있다.

## 미해결 액션 아이템 — 완료 판정을 막는 항목

아래 항목은 기존 설계의 남은 구현과 이번 코드 검토에서 확인한 제한이다.
통과한 fixture가 이 조건까지 검증한 것으로 해석하지 않는다. 심각도는 현재
구현의 범위와 실패 영향을 기준으로 하며, 자동 회수는 계속 비활성으로 유지한다.

| ID | 심각도 | 다음 조치와 완료 조건 |
| --- | --- | --- |
| AR-006 | medium | terminal 개별 상한에 더해 Host launch admission의 공유 예약을 구현했다. actor·동시 lease transfer·최종 frame이 예약 수명을 공유하며 최대 7개 예약을 허용한다. 여러 실행의 ANSI/Unicode 부하·거절·예산 재사용을 live로 확인했다. 예약 크기의 worst-case 적정성과 control/metadata·IPC 역직렬화 peak를 포함한 전체 회계 검증은 남아 있으며, 회계 예약을 OS RSS 강제 상한으로 해석하지 않는다. |
| AR-008 | medium (macOS 잔여) | Linux는 위 실행별 cgroup containment·격리 live 검증으로 수정했다. macOS는 여전히 birth 기반 polling이므로 빠른 `setsid`·double fork 자식의 소유권·독립 종료를 보장하는 native containment 구현과 실제 macOS 검증이 필요하다. |
| AR-010 | medium | [회수 정책](../crates/flowmux-daemon/src/agent_host/retirement.rs): provider 전체 작업 완료의 정규화와 안전한 idle 종료 수락 계약을 연결한다. 공급자의 응답 완료·화면 idle만으로 전체 완료를 단정하지 않으며, 계약 미지원이면 자동 종료를 활성화하지 않는다. 실제 30분·설정 변경·재접속·새 작업 경합을 검증해야 한다. |
| AR-068 | 해결 (에디터 상태 대비 범위) | run `44109493204`의 macOS `editor-recovery`와 48개 phase 전체 gate가 통과했다. 기본·One Dark·GitHub Light 실제 상태 대비 최솟값은 5.0524다. [증거](design-assets/agent-runtime-macos-contrast-final-20261118.json)를 보관한다. 이 run은 AR-087 전 source다. |
| AR-069 | 해결 (Session Flow 범위) | 수정 포함 Linux run `44073400067`의 `managed-session-flow`가 통과했다. 리베이스 후 main의 fork 전용 권한 정책에 fixture 기대값을 맞춘 격리 검사도 9개 통과했다 (`/tmp/fm-runtime-rebase-session-flow-aligned.log`). 전체 gate와 종료 실패 AR-073의 통과를 뜻하지 않는다. |
| AR-071 | medium (macOS 원인 확인 필요) | run `44073400067`의 DMG 검증 실패 뒤 fixture 이미지가 남았다. 진단 추가 revision의 run `44077266019`에서는 `macos-dmg-update`가 통과했지만 최초 실패 원인은 미확정이다. 실패 원인 특정과 해당 조건의 재현·수정·검증이 필요하다. |
| AR-073 | medium | 원격 Stopping 대기 실패와 로컬 높은 병렬도의 launch·relay 실패 원인을 특정한다. 후속 로컬 통과와 로그 보강은 재현 조건의 수정 증명이 아니다. |
| AR-088 | medium | run `44109493204`의 알림으로 이동한 Agent를 연 뒤 native 입력이 도달하지 않았다. 로컬 전체 30개와 알림 단독 10회는 통과했다. 실패 순간 GTK/X 포커스·입력 준비·키 상태를 확인해 원인을 재현하고 수정해야 한다. 진단 수집만으로 해결 처리하지 않는다. |
| AR-098 | medium (Linux 종료 원인 미확정) | 최종 로컬 전체 gate의 SSH 기능 검사 뒤 GUI가 20초 안에 종료되지 않았다. 단독 후속에서는 같은 종료 지점이 통과했으나 최초 상세 로그가 cleanup으로 삭제됐다. 종료 요청 전달·GTK 저장/종료·runtime 정리 중 어느 단계가 지연됐는지 재현과 진단이 필요하다. |
| AR-094 | medium (최초 원인 미확정) | run `44145082591`의 New Agent 메뉴 클릭이 `(17, 5)`에 전달되고 Cancel 대기가 만료됐다. 안정된 양수 geometry를 기다리도록 수정한 뒤 로컬 live와 run `44166374894`의 New Agent 10개 검사가 통과했다. 최초 실패의 raw geometry가 없어 당시 원인이 완전히 확정된 것으로 처리하지 않는다. |

AR-010의 종료 계약은 현재 연결된 provider 관측만으로 증명할 수 없다. UI 완료를
근거로 SIGTERM을 보내는 방식으로 대체하지 않는다. 따라서 이번 수정과 전체
Linux gate가 통과해도 **계획 전체 완료나 medium 이상 0건을 선언하지 않는다.**
실제 provider/model 장시간 실행과 macOS·Flatpak 수용 검증도 별도로 필요하다.

AR-009의 사용자 안내도 이 제한에 맞췄다. admission 전 실패는 기존 화면을
유지하지만, Host가 owner를 바꾼 뒤 ACK가 유실되면 source tab이 이미 닫혔을 수
있다. `agent-sessions.md`의 모든 timeout이 기존 화면을 보존한다는 설명을
수정하고 Agents에서 실제 위치 확인·같은 실행 재연결을 안내한다. 문서 수정은
응답 유실 recovery나 전역 저장 binding 조정의 구현을 대신하지 않는다.

### AR-008의 격리 프로세스 재현

Linux 수정 전 debug Host에서 provider가 첫 관측 전에 `setsid`·double fork를 수행하고
분리된 자식의 표준 입출력을 닫게 했다. root는 유지한 뒤 제품의 명시적
`terminate`를 요청했다. Host는 실행을 `Exited`로 보고했지만 분리된 자식은
동일 PID/birth로 살아 있었다 (`/tmp/fm-runtime-fast-detach-repro.log`,
`/tmp/fm-fast-detach-c357on6v/result.json`). 따라서 현재 단위·GUI fixture의
일반 자식 회수 통과를 빠른 분리까지 확장할 수 없다.

재현 후에는 전용 marker 환경과 birth를 검증한 자식만 종료하고, private Host와
함께 reap했다. 사용자 process에는 신호를 보내지 않았다. 실행별 containment는
자식 생성 전 소유권 설정, root 종료 후 생존 판정, 다른 Agent와 분리된 종료를
모두 보장해야 한다. Host 전체 subreaper나 더 빠른 polling만으로 해결하지 않는다.

### AR-025 · medium · 시작 중인 registry snapshot을 실행 종료로 오판

- 재현: 전체 Linux gate의 `agent-native-hooks`에서 OpenCode 연결이 실패했다.
  같은 시나리오를 반복하면 Claude·Cline에서도 발생했다. 진단을 추가한
  `/tmp/fm-native-launch-status.log`에서는 GUI가 `Agent ended before the view
  connected`를 반환했지만 Host의 같은 실행은 `Running`, exit status 없음이었다.
- 원인: launch 응답은 `Running`이어도 독립 관리 연결의 직전 registry snapshot은
  `Starting`일 수 있다. 공통 `wait_for_agent_view`가 Running 이외의 모든 상태를
  종료로 취급했다. 이 경로는 CLI launch·New Agent·재연결·이동이 함께 사용한다.
- 조치: `Starting`도 기존 10초 연결 대기의 대상으로 인정한다. 성공은 기존의
  ready lease·실제 relay PID/birth·목적지 검증을 그대로 요구한다. Stopping·Exited·
  Failed는 여전히 거절한다. fixture 실패에는 stderr·Host registry·화면을 남긴다.
- 검증: GTK 회귀 검사에서 Starting과 Running 동안 연결 대기를 유지하고 실제
  Exited 전이에는 오류를 반환했다. 별도 XDG·D-Bus·Xvfb 실행 1개 통과
  (`/tmp/fm-starting-view-unit-isolated.log`). 수정 후 native hooks 격리 live는
  6개 provider의 전체 30개 확인 항목을 통과했다(exit 0,
  `/tmp/fm-starting-view-after.log`, `/tmp/fm-gui-9i3c2ks8`). tab·GUI 종료 생존,
  동일 PID/birth·spawn 1회·새 입력·알림 복원을 포함한다. 실제 모델 실행은 아니다.

### AR-026 · medium · 새 checkout의 GTK 검사에서 CLI helper 누락

- 재현: 원격 Actions run `44011030787`의 Linux workspace 검사에서 managed
  relay signal·restored binding cleanup 두 검사가 `flowmuxctl is unavailable`로
  실패했다. 로컬 PATH의 기존 설치가 clean checkout의 누락을 가렸다.
- 원인: `cargo test --workspace`는 일반 CLI 바이너리를 만들지 않는데 공유
  Linux gate는 전체 빌드를 GTK 검사 뒤에 실행했다. test executable은
  `target/debug/deps` 아래 있으므로 helper의 형제 경로 탐색도 실패한다.
- 조치: 전체 locked 빌드를 workspace 검사 전에 실행하고, 그 debug 디렉터리를
  PATH의 앞에 둔다. `find_flowmuxctl`이 읽지 않는 환경 변수로 대체하지 않는다.
  빌드 실패는 workspace·GUI 검사 전에 원래 exit code로 중단한다.
- 검증: 빌드되지 않은 CLI를 거절하는 runner 회귀 검사는 수정 전 exit 91을
  재현했다. 수정 후 runner 검사는 25개 통과·1개 skipped, exit 0
  (`/tmp/fm-runtime-ci-cli-prerequisite-after2.log`). 실제 clean CI 재검증은 별도로 진행한다.
  기존 원격 실패는 삭제하거나 성공으로 바꾸지 않는다.

### AR-027 · medium · timeout으로 끊긴 IPC 응답을 다음 요청에서 유실

- 재현: `Client`가 응답 prefix를 읽은 상태에서 read를 취소하고 다음 요청을 보내면
  suffix를 새 JSON으로 파싱했다. 표적 회귀 검사는 수정 전 JSON 오류로 실패했다
  (`/tmp/fm-runtime-partial-ack-before.log`). Relay의 ready ACK timeout 뒤 같은 연결로
  lease를 조회하는 AR-020 복구 경로에도 적용된다.
- 조치: 연결이 부분 응답 bytes를 소유하고 cancellation-safe `read_until`로 이어 읽는다.
  완료된 envelope는 기존 request ID로 구분한다. 쓰기 도중 취소·실패한 연결은
  재사용을 거절하여 요청 조각이나 불명확한 effect를 다시 보내지 않는다.
- 검증: IPC library 92개·기존 integration 9개 통과. Relay의 정확한 lease 확인 검사는
  부분 ACK·뒤따르는 KeepAlive를 포함한 5가지 결과를 통과했다. source·destination의
  실제 VTE를 이용한 격리 live에서도 partial read를 주입하고 같은 실행의 새 입력·
  source binding 제거·재시작 시 소유권 보존·destination 종료 뒤 Background를 확인했다.
  수정 전 CLI의 같은 live는 새 VTE 입력 대기에서 실패했다(exit 1,
  `/tmp/fm-runtime-partial-ack-before-live2.log`, `/tmp/fm-gui-btt944kj`). 수정 후
  동일 live는 9개 확인 항목과 2.8초 이상 timeout 경과 조건을 통과했다
  (`LIVE_EXIT=0`, `/tmp/fm-runtime-partial-ack-after2-live.log`, `/tmp/fm-gui-ri9dw0_j`).
  전체 workspace all-targets Clippy(`-D warnings`)도 통과했다.

### AR-028 · medium · macOS native 검사에서 managed Host 모듈 누락

- 재현: Actions run `44011030787`의 macOS 앱 빌드는 성공했지만 `macos-dmg-update`
  단계에서 native test target이 `crate::agent_host`를 찾지 못해 컴파일에 실패했다.
  이 target은 제품 소스를 별도 crate root로 포함하므로 `main.rs`의 선언을 상속하지 않는다.
- 조치: native target에도 실제 `agent_host.rs` 모듈을 포함한다. 대역 구현이나 검사
  생략으로 우회하지 않는다.
- 검증: Linux에서도 `cargo check -p flowmux --test macos_native --features native-smoke
  --locked`가 통과했다(`/tmp/fm-runtime-native-module-check.log`). macOS 실제 native
  실행의 통과 여부는 후속 원격 run으로 검증하며, 이 cross-platform check로 대신하지 않는다.

### AR-029 · medium · 원격 Linux 제한이 내부 전체 검사 예산보다 짧음

- 코드 검토: AR-012는 내부 regression 예산을 3,600초로 늘렸지만 Actions의
  Linux step은 여전히 55분(3,300초)이었다. 사전 phase 예산 960초까지 합치면
  허용된 내부 실행을 원격 제어기가 먼저 취소할 수 있다. 현재 실행의 timeout이
  관측된 것은 아니므로 실제 실패로 기록하지 않는다.
- 조치: Actions의 Linux step 상한을 90분(5,400초)으로 늘려 내부 phase 합계
  4,560초와 종료·증거 보관 여유를 확보한다. 각 phase 예산·검사 항목·실패 조건은 유지한다.
- 검증: 실제 스크립트의 phase budget을 합산해 이전 상한은 부족하고 새 상한은
  더 큰지 확인했다. 실행 중인 로컬 고정 checkout과 원격 run의 설정은 변경하지 않는다.

### AR-030 · medium · 중단 입력 검사가 `od`의 플랫폼별 열 너비에 의존

- 관측: 원격 run `44013562644`의 macOS `agent-unit`에서
  `explicit_stop_checks_epoch_and_waits_for_user_force_after_grace`가
  `INTERRUPT:   3:END`를 기다리다 실패했다. fixture가 GNU/BSD `od`의 공백을
  그대로 응답에 넣으므로 실제 Ctrl-C 바이트와 출력 열 정렬을 함께 검사했다.
- 조치: fixture에서 숫자 앞뒤 공백을 제거하고 `INTERRUPT:3:END`를 확인한다.
  실제 PTY의 단일 바이트 읽기·Interrupt 요청·다른 실행 보존·강제 종료 유예 조건은 유지한다.
- 검증: shell pipeline의 실제 byte 3 변환을 확인했다. Linux Host 회귀와 macOS
  원격 재검사 결과는 후속 검증 기록에 추가하며, 이 수정만으로 macOS 실패 전체가
  해결됐다고 판정하지 않는다.

### AR-031 · medium · 부모 종료 검사의 자식 수명이 고정 0.8초

- 관측: 같은 macOS run의 `natural_root_exit_waits_for_owned_background_child_and_reaps_without_idle_timeout`도
  실패했다. 기존 검사는 자식을 0.8초만 살려 두고 350ms sleep 뒤 생존을 요구하여,
  부하로 timer가 늦게 재개되면 이미 자연 종료한 자식과 잘못 조기 회수한 실행을 구분하지 못한다.
  이 실패만으로 macOS 제품 원인이 확정된 것은 아니다.
- 조치: 자식이 시작했다는 파일과 root PID/birth의 종료를 실제로 기다린다. 자식은
  별도 디렉터리 gate를 제거할 때 종료하므로, 보존 assertion 전에 자연 종료하지 않는다.
  assertion 후 gate를 해제하고 기존 Exited·actor 회수 조건을 그대로 검사한다.
  panic 시 임시 디렉터리 정리로도 자식 gate가 해제된다.
- 검증: Linux Host 표적 회귀 32개가 통과했다(실패·ignored 0개). macOS 원격 실행으로
  다시 확인한다. 이 fixture 변경은
  AR-008의 macOS fast-fork containment 구현이나 통과를 대신하지 않는다.

### AR-032 · medium · CLI 종료 검사가 두 이력을 한 번에 닫는 것으로 기대

- 관측: 고정 checkout `46b6d541`의 전체 gate는 `agent-cli-controls`에서 실패했다.
  이 경로는 실행 두 개를 종료하여 두 이력을 유지하지만 한 번 닫기 후 section 전체가
  없어야 한다고 검사했다. 제품이 다른 이력을 보존하는 정상 동작을 실패로 판정했다.
- 조치: 실제 보이는 이력 수를 확인하고 하나씩 닫는다. 각 클릭마다 정확히 한 행만
  제거되고, 남은 행이 있으면 section을 보존하며 마지막 닫기 후 숨기는지 확인한다.
  종료된 Host가 재시작되지 않는지도 확인한다. 제품 코드는 바꾸지 않는다.
- 검증: 별도 systemd·Xvfb·D-Bus·XDG와 WebKit sandbox·fatal GTK critical 조건에서
  CLI live 14개 확인 항목이 통과했다(exit 0, `/tmp/fm-runtime-ar032-cli.log`).
  사용자 GUI PID `1663486`은 보존했다. 원래 실패를 전체 gate 성공으로 덮어쓰지 않는다.

### AR-033 · medium · native hook 검사가 확정된 turn의 지연 progress를 재개로 기대

- 관측: 원격 run `44016375422`는 수정된 Host 검사와 native·keyboard·restore
  smoke를 통과한 뒤 `agent-hooks`에서 실패했다. 종료가 확정된 `root-3`의
  뒤늦은 `running`에 대해 fixture가 `Running`을 기대했다.
- 원인: 공통 reducer는 이미 회수한 turn ID의 지연 시작·progress가 완료나
  다음 turn의 wait를 되돌리지 않게 거절한다. 해당 규칙의 단위 검사와 live
  fixture의 기대가 서로 달랐다. Linux에서 짧은 순간 상태를 관측한 결과를
  지속적인 재개 보장으로 사용하지 않는다.
- 조치: 지연 progress 후 완료 유지와 500ms 뒤의 안정 상태를 검사한다.
  실제 `stop_hook_active` 재시도 수락·완료 중복 방지와 다음 turn·자식·wait
  검사는 유지한다. 제품 reducer를 느슨하게 하지 않는다.
- 검증: 별도 GUI의 전체 6-provider native hook replay가
  `LIVE_NATIVE_HOOK_MATRIX_OK`, exit 0으로 통과했다
  (`/tmp/fm-runtime-ar033-hooks.log`). macOS 수정본의 재검증은 아직 필요하다.

### AR-009 후속 · 승인된 화면의 영속 fencing

- Host는 ready/input owner를 공개하기 전에 `state.agent-views.<epoch>.json`에
  InstanceId별 승인 SurfaceId를 기록한다. window snapshot과 분리하여 이전
  GUI·구버전 GUI의 늦은 저장이 승인을 덮어쓰지 못하게 한다. 기록은 기존
  `state.lock`과 tmp-file·fsync·rename 경로를 사용하며 native session의 자동
  재실행을 요청하지 않는다.
- `load_from`·최종 저장은 같은 기록을 대조한다. 정확한 epoch/instance에서
  다른 SurfaceId인 managed tab만 제거하고 일반 tab·이긴 binding·다른 epoch를
  유지한다. 빈 split·workspace와 저장된 순서·선택도 함께 정리한다. authority가
  손상되면 저장을 중단하여 원본을 보존한다.
- admission 기록 실패 시 ready를 승인하지 않고 기존 입력 lease와 PTY 크기를
  보존한다. 실제 GUI relay 검사의 손상 파일 주입에서도 기존 화면의 새 입력이
  처리됐고, 재시도 성공 뒤 이전 lease는 거절됐다.
- 격리 GUI의 Visible·Background 두 경우에서 각각 5개 live 검사 통과
  (`/tmp/fm-runtime-authority-{live,background}.log`). source GUI를 SIGKILL하고
  낡은 저장 slice를 주입한 뒤 다른 창의 저장만으로 이를 제거했다. 재연결·승자
  GUI crash 복원·탭 닫기에서 같은 PID/birth·spawn 1회와 실제 VTE 입력을 확인했다.
- 실제 native managed drag 24개 통과·exit 0
  (`/tmp/fm-runtime-authority-drag.log`). pane·tab strip 재정렬·취소·workspace
  목적지·Background 재연결·GUI 종료 후 같은 프로세스 생존을 확인했다.
- state 94개·daemon 280개(1개 ignored)·procmon 17개 통과. state 검사는
  Host 조회 없이 raw stale snapshot 복원·반복 late save·split 축약·파일 손상을
  포함한다. workspace Clippy(`--all-targets -- -D warnings`)와 format·diff 공백·
  변경 Python AST 검사가 통과했다.
- **AR-009 전체를 닫지 않는다.** admission ACK와 registry 조회가 함께 유실될
  때 relay의 불확실한 결과 복구, destination layout 저장 전 crash의 완전한
  복원은 남아 있다. authority는 실제 프로세스 생존 증명이 아니며, Host·provider
  재시작이나 실행 중인 구버전 GUI 갱신도 수행하지 않는다. epoch별 authority
  파일은 stale writer fencing을 위해 유지한다.

### AR-034 · medium · macOS의 범위 밖 PID를 process-group 생존으로 오판

- 코드 검토: `pid_alive`는 `u32`를 Unix `pid_t`로 변환하기 전에 상한을
  검사하지 않았다. macOS에서 `u32::MAX`는 `kill(-1, 0)`으로 해석되어
  존재하지 않는 window owner를 살아 있는 것으로 판단할 수 있었다.
- 조치: `process_start_time`과 같은 양수 `i32` 범위를 적용하고 0·현재 PID·
  상한 초과 두 값의 검사를 추가했다. 호출자는 바꾸지 않았다.
- 검증: 격리 환경의 procmon library 17개 통과
  (`/tmp/fm-runtime-pid-range-tests.log`). macOS 실기 재검증은 원격 gate에서
  확인해야 하며 Linux 결과를 macOS 실행 결과로 표시하지 않는다.

## 통합 검증 기록

2026-11-17, 제품·검사 코드 `46b6d541`의 Linux 격리 X11 검증을 진행한다.
전체 gate는 `/tmp/fm-runtime-audit-final6`의 고정된 checkout에서 실행하며,
이후 문서 갱신이나 다른 작업으로 실행 중인 스크립트가 바뀌지 않게 했다.

- 이전 final5의 workspace는 2,403개 통과·실패 0개·13개 ignored였지만,
  실행 중 Bash 스크립트를 AR-026 때문에 편집하여 team-recovery 이후 gate가
  exit 127로 중단됐다. 이 실행을 전체 통과 근거로 사용하지 않는다.
- 새 전체 공유 gate는 별도 사용자 서비스
  `flowmux-runtime-audit-20261117-final6.service`에서 실행했으며 AR-032에서 exit 1로 종료됐다. WebKit sandbox·
  fatal GTK critical 검사를 유지하며 전체 44개 중 중단 지점까지의 순서·개수·결과를 확인했다. workspace는
  2,405개 통과·실패 0개·13개 ignored다. 미실행 후속 단계는 별도로 검증한다.
- 수정본의 IPC 검사 101개·표적 relay 검사·partial ACK의 실제 VTE 검사 9개 통과.
  workspace all-targets Clippy(`-D warnings`), Rust format·diff 공백 검사 통과.
- CI와 같은 cargo-deny 0.20.2의 advisories·licenses·sources 정책, 자산 license·
  notice 검사도 기존 검증에서 통과했다. 이후 의존성·자산은 바꾸지 않았다.
- 원격 Actions run `44011030787`은 stress·editor web 성공, Linux·macOS 실패다.
  Linux의 helper 누락은 AR-026, macOS native target의 모듈 누락은 AR-028로 수정했다.
  새로운 source revision의 원격 결과를 별도로 확인하며 기존 실패는 보관한다.

- 후속 원격 run `44013562644`에서 editor web 41개와 별도 headless stress 5개가
  통과했다. macOS 제품 build·native `macos-dmg-update`·state·CLI도 통과했으나,
  `agent-unit`의 daemon 270개 통과·2개 실패·1개 ignored로 후속 native smoke는 실행되지 않았다.
  실패 두 건을 AR-030·031로 각각 수정했으며, 전체 macOS 성공으로 표시하지 않는다.
- 수정본 `b507e247`의 Linux Host 표적 검사는 32개 통과·실패 0개다.
  별도 원격 run `44016375422`가 시작됐고 editor web 검사를 통과했다. 이미 macOS가 실패한 이전 run의
  Linux job은 수정본 실행의 concurrency를 해제하려고 중단했으며, 전체 통과로 기록하지 않는다.
  테스트 수정본 daemon all-targets Clippy(`-D warnings`)도 통과했다.
  별도 systemd 서비스·private XDG의 agent 검사도 808개 통과·실패 0개·2개 ignored다.
  이전 원격 Linux의 workspace도 2,405개 통과·실패 0개·13개 ignored였으므로
  AR-026은 clean runner에서 확인됐다. 이후 job 중단을 전체 gate 통과와 구분한다.

이전 실패 gate·수정 전후 재현·로그 digest·정리 결과는
[검증 근거 JSON](design-assets/agent-runtime-linux-audit-20261117.json)에 보관한다.
일반 workspace이므로 현재 agent가 검토했으며 별도 agent의 독립 리뷰는 아니다.
남은 medium 4개는 위 표의 AR-006·008(macOS)·009·010이다.

## 완료 전 확인할 범위

- 새 변경을 포함한 Linux 공유 CI 전체와 runtime live phase.
- 계획서의 미완료 항목(전체 provider 완료·원자적 idle 종료 계약, offline binding
  조정·응답 유실, 자원 상한·프로세스 containment).
- 실제 provider 장시간 작업과 macOS·Flatpak은 이 Linux fixture의 검증 대상이 아니다.
- `.claude/`의 별도 worktree와 Python bytecode는 runtime 구현 커밋에 포함하지 않는다.

전체 계획 완료, medium 이상 이슈 0건, 배포 가능 여부는 아직 확정하지 않았다.

### `2899be53` 이후 로컬 통합 재검증

- 격리 X11·D-Bus·private XDG에서 `cargo test -j 2 --workspace --locked
  --no-fail-fast`가 exit 0으로 끝났다. **2,406개 통과·실패 0개·13개 ignored**이며,
  내부 subprocess가 재실행한 표적 검사 2개의 summary는 중복 계산하지 않았다.
  GTK/UI 본체는 956개 통과·4개 ignored다. 로그는
  `/tmp/fm-runtime-authority-workspace.log`다.
- 앞선 workspace all-targets Clippy 후 변경한 state·procmon 표적을 다시
  `--all-targets -- -D warnings`로 검사하여 통과했다. native drag 24개와
  Visible·Background 저장 복원 각 5개도 모두 exit 0이며, 추적 GUI·Host·provider
  process가 남지 않았다. 사용자 GUI PID `1663486`와 설치본은 유지했다.
- 현재 변경의 저장·복원·입력 소유권 경로를 다시 대조했다. 전체 계획의 남은
  medium은 AR-006·008(macOS)·009(불확실한 응답/목적지 layout)·010이다.
  이번 결과는 전체 Linux shared gate·macOS native gate·실제 모델 장시간 작업을
  모두 통과했다는 뜻이 아니다. 수정별 커밋과 검증 근거를 전용 branch에 보관한다.

### AR-035 · medium · managed Session Flow 검사의 시작 입력 전제 미확인

- 원격 run `44016375422`의 Linux `managed-session-flow`는 첫
  `Claude sessions` 선택 전에 timeout으로 실패했다. 제품 실행·복원 단계에
  도달하지 않은 실패이며, 동일 revision 계열의 로컬 재현은 9개 항목을 통과했다.
- 기존 검사는 창 resize 후 0.2초를 기다리고 X focus 요청 직후 단축키를 보냈다.
  고정 sleep을 제거하고, X 요청 flush 및 AT-SPI의 해당 GUI frame `ACTIVE`
  관측 뒤 실제 단축키를 보낸다. 입력 재전송·대기시간 증가는 하지 않는다.
- 수정 후 격리 `--session-flow --protected-pid 1663486`은 **9개 통과·exit 0**.
  Host fork·Background 재연결·GUI 종료/복원에서 같은 PID/birth와 실제 입력을
  확인했다. 로그 `/tmp/fm-runtime-session-panel-ready.log`, artifacts
  `/tmp/fm-gui-aogb14tw`에 보관한다. 원격 실패 원인이 focus였다는 확정이나
  원격 재검증 성공을 뜻하지 않으며, 최종 원격 실행 결과는 별도로 확인한다.
- 조사 중 시도한 footer 버튼 클릭은 scroll 영역 밖의 좌표를 선택해 실패했다.
  해당 시도는 반영하지 않았으며 성공 근거에 포함하지 않는다.

### AR-006 후속 · VT projection의 전송 byte 예산

- 개별 URI·셀 수 제한만으로는 긴 유효 링크가 반복되는 snapshot 크기를
  제한하지 못했다. 한 projection에 내보내는 URI 합계를 256KiB로 제한하고,
  현재 화면을 먼저 보존한 뒤 최신 history부터 16MiB 전송 예산 안에 담는다.
  cursor·mode·diff 주소에는 별도 여유를 남긴다. 오래된 history는 잘리고,
  URI 예산을 넘은 셀은 글자·스타일을 유지하며 링크 메타데이터를 생략한다.
- 최대 허용 크기인 100×400 화면에 ANSI·Unicode 부하를 주어 snapshot과
  다음 diff가 16MiB 이내인지 확인했다. 현재 화면과 남긴 history 양 끝의
  순서도 다시 파싱한 terminal과 비교했다. 긴 링크 반복의 현재 글자 보존과
  링크 예산도 별도 검사했다.
- daemon library **282개 통과·1개 ignored**, all-targets Clippy(`-D warnings`),
  format·diff 공백·변경 Python AST 검사 통과. 실제 격리 GUI/VTE **23개 통과**:
  유효한 긴 링크 1,200개 출력·snapshot chunk 조회·새 입력·동일 PID/birth,
  탭 닫기와 정상 종료/SIGKILL 후 복원·자연 종료/Host 회수를 확인했다.
  `/tmp/fm-runtime-projection-budget-{daemon,live,clippy}.log`에 보관한다.
- **AR-006 전체는 열려 있다.** VT engine 자체가 보유한 누적 hyperlink,
  동시 projection/transfer와 실행들을 합친 Host 전체 메모리 예산은 이번
  개별 전송 제한으로 증명되지 않는다. 사용자 설치본과 실행 중인 GUI도 유지했다.

### AR-036 · medium · 유효한 hyperlink의 VT 누적 보유량 제한 누락

- 원인: URI/ID 각각의 4KiB 검증과 projection 전송 예산을 통과한 링크도
  terminal의 history·숨겨진 화면·저장 cursor에 계속 쌓일 수 있었다.
- 조치: terminal마다 CellExtra의 약한 참조로 링크 보유량을 추적하고 URI/ID와
  추적 비용을 보수적으로 계산한 4MiB 예산을 적용한다. 결합 문자·밑줄 속성·
  wide spacer 덮어쓰기의 복사도 추적한다. 셀이 제거되면 예산을 재사용하며,
  한 번의 회수는 최대 32개 참조만 확인한다. reset은 두 화면을 비운 뒤 예산도
  초기화한다. 예산 초과 시 문자·스타일은 유지하고 링크 정보만 생략한다.
- 검증: terminal 표적 **20개**, daemon library **285개 통과·1개 ignored**,
  제품 build·daemon all-targets Clippy(`-D warnings`)·format·diff 공백·Python
  AST 검사를 통과했다. 숨겨진 화면·저장 cursor·화면 삭제 후 예산 재사용과
  ASCII로 wide spacer를 덮어쓸 때의 링크 보유를 회귀 검사에 포함했다.
- 격리 실제 GUI/VTE **24개 통과·exit 0**. 약 4KiB의 유효 URI 1만 개에
  결합 문자·밑줄을 함께 출력했을 때 Host RSS는 25,228→31,732KiB였다.
  새 한글 입력·tab 닫기·GUI 정상 종료/SIGKILL 이후 같은 PID/birth·spawn 1회
  재연결·자연 종료와 Host 회수를 확인했다. stdout/event PASS 수가 일치하고
  추적 process가 남지 않았다. `/tmp/fm-runtime-retained-link-{tests,daemon,
  build,live,clippy}.log`, `/tmp/fm-gui-2h8y2wh7`에 근거를 보관했다.
- AR-006의 Host 전체 budget·AR-008의 macOS containment·AR-009의 불확실한
  응답/목적지 layout 복원·AR-010의 provider 종료 계약은 남아 있다. 전체 계획
  완료나 medium 이상 0건으로 표시하지 않는다. 사용자 GUI PID `1663486`와
  설치본을 유지했으므로 사용 중인 앱에 수정본을 적용한 결과도 아니다.

### `00d37569` 최종 Linux 통합 검사와 원격 macOS 실패

- 커밋 후 private XDG/runtime·Xvfb·D-Bus에서 `cargo test -j 2 --workspace
  --locked --no-fail-fast`가 **exit 0, 2,411개 통과·실패 0개·13개 ignored**였다.
  GTK/UI 본체 956개 통과·4개 ignored를 포함하며 subprocess summary 2개는
  중복 계산하지 않았다. 로그 `/tmp/fm-runtime-retained-link-workspace.log`.
- 이번 링크 예산 변경의 자체 검토에서 추가 medium 이상 결함을 확인하지 않았다.
  이는 기존 미해결 설계 항목이나 아래 원격 실패를 해소했다는 뜻이 아니다.
- 원격 run `44022308230`(`ce372d25`)의 macOS는 native `agent-hooks`까지
  통과한 뒤 `session-paths`에서 exit 101로 실패했다. 기존 AR-033의 delayed
  progress 검사는 통과했으며, editor web·headless stress도 성공했다. Linux
  job은 이 기록 시점에 실행 중이다. 이 run은 후속 링크 예산 코드의 검증이 아니다.

### AR-037 · medium · macOS Session Paths의 managed runtime 연결 누락

- 재현: 원격 native main-thread 검사에서 Session Resume 뒤 30초 후
  `custom session resume failed: Agent Host runtime unavailable`로 실패했다.
  `/tmp/fm-runtime-run44022308230-macos.log`, job `133834563929`, phase
  `session-paths`에 근거를 보관했다. 해당 위치 뒤의 native 검사는 미실행이다.
- 코드 근거: `macos_smoke.rs`는 Tokio runtime을 만들지만 공통
  `WindowController::new`에는 `None`을 전달한다. `sessions.rs::running_session`
  은 이 핸들이 없으면 즉시 위 오류를 반환한다. 새 managed Resume 경로의
  전제를 fixture가 충족하지 못한다.
- 후속 경계: 핸들을 추가해도 `flowmux-cli/src/agent_host.rs::connect_for_launch`
  의 독립 Host 시작은 Linux 전용이며 macOS에서는 명시적으로 unsupported다.
  native fixture에는 production에 맞는 Host·GUI management/relay 초기화와
  안전한 종료가 필요하다. 기존 shell 실행으로 우회하거나 성공 조건을 약화하지 않는다.
- 상태: **미해결**. 핸들만 전달하는 수정을 검증된 해결책으로 커밋하지 않았다.
  macOS 시작 경로 구현·native 실기 재검증이 필요하다. 최신 미해결 medium은
  **AR-006·008·009·010·037의 5개**이며, 전체 계획 완료 조건은 미충족이다.

### AR-037 후속 · launchd 시작과 native managed fixture 연결

- macOS의 명시적 launch는 launchd GUI domain에 private 임시 plist를 등록한다.
  기존 helper 탐색과 최소 환경만 사용하며 `RunAtLoad=true`, `KeepAlive=false`,
  `Umask=0077`을 적용한다. 등록 후 임시 파일을 지우고 로그인 자동 시작 파일은
  설치하지 않는다. 종료 job의 재개에는 `kickstart`만 사용하며 `-k`, bootout,
  기존 Host 종료나 GUI 자식 spawn으로 우회하지 않는다. 관측·복원은 시작하지 않는다.
- 공통 native controller에 Tokio 핸들과 production GUI management를 연결하고
  Session Paths에 GUI IPC를 추가했다. fixture provider는 입력까지 살아 있게 하여
  실제 managed tab 연결·입력·자연 종료를 확인한다. 빈 Host가 회수된 뒤 최소
  500ms 동안 재시작하지 않는지 검사하고, 두 번째 Resume으로 종료 job 시작도
  검사한다. Unicode home·quoted cwd·긴 PATH·login shell PATH의 기존 검증은 유지한다.
- Linux에서 CLI Host 표적 **6개 통과**, CLI build·all-targets Clippy(`-D warnings`),
  format·diff 공백 검사를 통과했다. 실제 사용자 서비스로 시작한 격리 GUI/VTE
  **25개 통과·exit 0**: 같은 PID/birth의 tab 닫기·GUI 종료/SIGKILL 재연결,
  입력·자연 종료·빈 Host 회수를 확인했다. `/tmp/fm-runtime-launchd-{cli,
  build,clippy,linux-live}.log`, `/tmp/fm-gui-nowbyhlw`에 근거를 보관했다.
- **macOS 실기는 아직 미검증**이므로 AR-037을 닫지 않는다. launchd GUI 수명
  분리·native containment(AR-008)을 Linux 검사로 보장하지 않는다. 기존 사용자
  GUI PID `1663486`와 설치본은 유지했으며 전체 계획 완료 조건도 그대로 남는다.

### AR-006 후속 · Host launch admission과 최종 frame의 공유 메모리 예약

- 3GiB 회계 예산에서 control/metadata용 384MiB를 제외하고 실행마다 384MiB를
  예약한다. 기존 32개 instance 상한에 앞서 **7개 memory reservation**이 실질
  admission 한도가 된다. 새 launch가 예산을 넘으면 receipt·process 생성 전에
  `Busy`로 거절하며, 같은 request ID 재조회는 기존 결과를 반환한다.
- Tokio의 owned semaphore permit을 actor·active/pending transfer가 공유한다.
  actor가 종료되어도 최종 frame이 남으면 예약을 유지한다. 저장소 field를 먼저
  drop하고 마지막에 permit을 해제하여 동시 launch에 조기 반환하지 않는다.
  실행 실패·자연 종료·최종 frame detach 후에는 예산을 다시 사용할 수 있다.
- 새 회귀 검사는 admission 거절 때 marker/receipt 미생성, full budget에서
  request 재조회, 입력·최종 frame 보존과 detach 뒤 예산 재사용, exec 실패 뒤
  예산 반환을 실제 Host RPC와 process로 확인한다. daemon library **286개 통과·
  실패 0개·1개 ignored**, build·all-targets Clippy(`-D warnings`) 통과.
- 최종 제품 binary의 실제 사용자 서비스·격리 GUI/VTE **26개 통과·exit 0**.
  100×400 terminal 7개에서 ANSI·Unicode 동시 출력 후 Host RSS는
  **22,560→120,352KiB**였다. 8번째 실행 거절 중 기존 GUI 입력, 자연 회수 뒤
  재사용, tab 닫기·GUI 정상 종료/SIGKILL 후 같은 PID/birth 재연결을 확인했다.
  로그 `/tmp/fm-runtime-host-budget-{daemon,admission,clippy,final-build,final-live}.log`,
  artifacts `/tmp/fm-gui-461xzhtg`에 보관한다.
- 첫 live 시도는 종료된 부하 agent들의 이력을 추가한 뒤 기존 `len==1` 전제가
  실패했다. 최종 검사는 전체 이력 수를 추가 실행 수와 대조하고 Running이
  원래 instance 1개인지 별도로 검사한다. 실패 시도를 성공 근거에 넣지 않는다.
- **AR-006은 열려 있다.** 이 예약은 kernel RSS/provider 메모리 제한이 아니며,
  전체 최대 payload·동시 역직렬화와 metadata peak의 회계 적정성은 추가 확인해야
  한다. AR-008·009·010·037도 남는다. 사용자 GUI와 설치본은 유지했다.

### AR-038 · medium · macOS launchd 시작 코드의 UID 기능 의존

- 원격 run `44026067490`의 macOS job `133847087429`는 native 검사 전에
  `launchd.rs:72`의 `nix::unistd::getuid`에서 E0425로 build 실패했다. workspace의
  nix `user` feature가 비활성이라 Linux에서 제외된 start 함수의 오류를 놓쳤다.
  로그 `/tmp/fm-runtime-run44026067490-macos.log`에 근거를 보관했다.
- 기존 libc 의존성의 `getuid()`를 직접 사용한다. UID 선택과 launchctl 인자는
  그대로이며 새 의존성이나 feature는 추가하지 않는다. Linux의 CLI Host 표적
  검사는 launchd job 인자·환경 검증에 한정하고 실제 macOS build/native 통과는
  수정 revision의 원격 검사에서 별도로 확인한다. AR-037은 계속 열어 둔다.

### AR-009 후속 · ACK와 조회의 연속 유실 뒤 relay 보존

- ACK가 없고 첫 registry 조회까지 타임아웃되면 relay가 반환하여 이미 승인된
  입력 lease를 잃었다. 이제 동일 연결에서 읽기 전용 List를 재조회한다. 정확한
  epoch·instance·generation·lease의 ready 확인 전 입력은 보내지 않는다.
  조회 Busy/Io도 다시 확인하되 다른 소유권·Host epoch·종료·transport EOF에서는
  오류로 끝낸다. admission/launch mutation은 재전송하지 않는다.
- 확인 대기는 relay의 HUP/TERM/INT로 취소할 수 있다. GUI가 없는 동안에도
  실행을 재시작하거나 입력을 재전송하지 않는다.
- CLI binary 전체 **186개 통과·실패 0개**, relay 표적 **2개 통과**. 표적은
  분할 ACK, ACK+조회 timeout, 조회 Busy/EOF, pending·superseded·다른 epoch를
  확인한다. 처음 실행의 분할 응답 실패는 성공 근거에서 제외했다. IPC와 CLI를
  다시 컴파일한 뒤 진단 출력을 제거한 표적·전체 검사 모두 통과했다. 처음
  실패의 캐시·타이밍 원인은 확정하지 않았으며 상위 IPC 자체 partial-read 회귀도
  별도로 1개 통과했다.
- 실제 사용자 서비스·격리 GUI/VTE **27개 통과·exit 0**. ACK를 버린 상태에서
  fixture Host만 SIGSTOP으로 6.8초 정지해 첫 조회도 타임아웃시켰다. SIGCONT 뒤
  같은 relay가 새 한글 입력을 처리했고 agent PID/birth·spawn 1회를 유지했다.
  artifacts `/tmp/fm-gui-jrk3q670`, 로그 `/tmp/fm-runtime-admission-recovery-
  {live,tests-final,cli-final,build-final,clippy-final}.log`. 추적한 fixture process는
  남지 않았고 사용자 GUI PID `1663486`·설치본은 유지했다.
- **AR-009는 계속 열어 둔다.** 목적지 layout 저장 전 crash의 완전한 복원과
  새 workspace 생성 뒤 불확실한 연결 결과에 대한 cleanup은 아직 남아 있다.
  다른 미해결 medium도 이 수정으로 닫지 않는다.


### AR-009 후속 · 연결 대기 실패 뒤 새 workspace 보존

- 새 workspace 생성 후 `open_agent_view`가 오류를 반환하면 workspace 전체를
  삭제했다. 오류는 admission 전 실패뿐 아니라 결과가 불확실한 timeout에도
  발생하므로, 승인된 화면과 대기 중 사용자가 추가한 tab을 잃을 수 있었다.
- 연결 오류에 따른 workspace 자동 삭제를 제거했다. Agent binding·다른 tab을
  보존하고 기존 오류 안내와 명시적 재연결·닫기 경로를 사용한다. 연결에 앞선
  프로젝트·목적지 유효성 검사는 유지한다.
- 실제 격리 GUI 두 창에서 **9개 통과·exit 0**. native Agents drag·Escape 취소·
  목적지 선택·tab 닫기 뒤 Background 생존을 재확인했다. 새 workspace의 relay
  snapshot을 10초 넘게 지연하고 대기 중 일반 tab을 추가했다. timeout 뒤 두 tab
  보존, 같은 binding 재연결·새 입력 ACK·동일 PID/birth를 확인했다.
  artifacts `/tmp/fm-gui-89rqepfl`, 로그
  `/tmp/fm-runtime-workspace-retain-live-final.log`.
- 첫 시도는 Background menu의 `Open in…`을 `Move to…`로 찾는 fixture 오류로
  새 검증 전에 실패했다(`/tmp/fm-gui-sr_jo6ke`). fixture를 수정한 최종 결과만
  성공 근거로 사용한다. 두 격리 runtime의 잔여 process가 없음을 확인했다.
- 제품 build·GUI all-targets Clippy(`-D warnings`)·format·diff 공백·Python AST
  검사를 통과했다. 로그 `/tmp/fm-runtime-workspace-retain-{build,clippy}.log`.
  이전 relay 오류 문맥의 format 누락은 `359c1dde`로 별도 정리했다.
- **AR-009는 아직 열려 있다.** 목적지 layout 저장 전 crash 복원은 별도 문제다.
  사용자 GUI PID `1663486`과 설치본은 교체·재시작하지 않았다.


### AR-009 후속 · Host 승인 전 목적지 layout 영속 저장

- GUI가 정확한 epoch·instance·lease·generation·window owner의 snapshot을 준비한다.
  Host는 인증한 GUI PID와 checkpoint를 대조하고 동일 state lock 안에서 목적지
  layout을 먼저, authority를 나중에 원자 교체한다. 두 번째 쓰기 실패에도 원래
  source snapshot과 authority를 유지하며 입력 lease·PTY 크기를 되돌린다.
- checkpoint 자체는 authority가 아니다. 성공·거절·만료·연결 종료 때 정확한
  lease만 정리한다. 같은 GUI widget 이동도 성공 응답 전에 기존 저장 경로를
  기다린다. readiness 계약 변경에 따라 Host protocol을 15로 올렸다.
- state **95개 통과**, daemon library **286개 통과·1개 ignored**. 두 번째
  authority 쓰기만 실패시키는 검사는 candidate가 원시 파일에 남더라도 원래
  화면만 복원되는지, 성공 뒤 late source save와 명시적 닫기도 확인한다.
  로그 `/tmp/fm-runtime-destination-checkpoint-tests-final.log`.
- 실제 격리 GUI **10개 통과·exit 0**, artifacts `/tmp/fm-gui-30iq6qfk`, 로그
  `/tmp/fm-runtime-destination-checkpoint-live.log`. native Agents drag·Escape·
  탭 닫기·연결 실패 rollback을 포함한다. 목적지 GUI의 일반 state rename만
  거절하고 Host 저장은 허용했다. 승인 후 해당 GUI만 SIGKILL하여 정확한
  workspace·pane·Agent tab·추가 일반 tab 복원과 새 입력 ACK·동일 PID/birth를
  확인했다. 추적 process는 남지 않았으며 사용자 PID `1663486`은 유지했다.
- 이 결과로 **계획 전체 완료 또는 medium 0개를 선언하지 않는다.** AR-009의
  동시 layout 변경 경계 검토와 AR-006·008(macOS)·010·037이 남는다.
  macOS 원격 run `44027300607`은 이전 revision `3f021296` 검사 중이며 최신
  checkpoint 변경의 검증 근거가 아니다. 실행 중 job 로그 조회는 404였다.
- 제품 build·변경된 GUI/state/daemon/IPC all-targets Clippy(`-D warnings`)·
  format·diff 공백·Python AST 검사를 통과했다. 로그
  `/tmp/fm-runtime-destination-checkpoint-{build,clippy}.log`.
- 실제 GTK의 stale admission 회귀 **1개 통과**, 로그
  `/tmp/fm-runtime-destination-checkpoint-gtk.log`.


### AR-039 · medium · 준비된 화면이 더 최신 window 저장을 덮어씀

- AR-009 후속 코드에서 checkpoint 준비와 Host 승인 사이 일반 저장이 끝나면
  Host가 준비 당시의 window 전체 snapshot으로 최신 내용을 덮어쓸 수 있었다.
  수정 전 state 회귀가 stale checkpoint의 승인을 재현했다. 로그
  `/tmp/fm-runtime-window-revision-repro.log`(실패는 재현 근거).
- `SavedWindow.save_id`는 owned 저장마다 새 UUID를 받는다. GUI는 기존 save
  lock 안에서 최신 snapshot을 준비하고, Host는 state lock 안에서 해당 창의
  저장 UUID가 준비 당시와 같은지 확인한다. 다르면 기존 실행·입력 소유권과
  최신 디스크 내용을 유지한 채 이동을 거절한다. 다른 창의 저장은 방해하지
  않는다. 구형 state의 누락된 UUID는 nil로 읽으며 새 의존성은 없다.
- state 전체 **95개 통과**, 다른 창의 저장·두 번째 authority 쓰기 실패·최신
  window 변경 보존을 포함한 최종 표적 **1개 통과**. daemon library **286개
  통과·1개 ignored**. 첫 수정 후 검사는 Agent tab만 있는 fixture workspace가
  기존 fencing으로 제거되는 전제를 놓쳐 실패했다. 일반 tab을 함께 둔 최종
  fixture가 통과했으며 실패를 성공 근거로 세지 않는다.
- 최초 GUI 시도(`/tmp/fm-gui-aqyu6a6o`)는 승인 대기 중 새 tab 명령이 정규
  queue에서 대기하므로 fixture timeout으로 실패했다. 수정된 fixture는 native
  tab 생성 뒤 일반 저장의 실패·재시도를 제어해 준비 후 저장 완료를 재현한다.
- 수정 후 실제 GUI **11개 통과·exit 0**, artifacts `/tmp/fm-gui-cwo8qkyf`,
  로그 `/tmp/fm-runtime-window-revision-live-final.log`. native로 일반 tab을 만든
  뒤 일반 저장 실패·재시도를 제어해 checkpoint 준비 후 자동 저장을 완료했다.
  오래된 승인 거절·일반 tab 영속 보존·원래 입력 lease의 새 입력 ACK·동일
  PID/birth를 확인했다. 이후 selector·native drag·tab 닫기·timeout workspace
  보존·GUI SIGKILL 복원도 통과했다. 실패·최종 fixture 모두 잔여 process가 없다.
- 제품 build·workspace 전체 all-targets Clippy(`-D warnings`)·최종 state
  Clippy·format·diff 공백·Python AST 통과. 로그
  `/tmp/fm-runtime-window-revision-{build,clippy,state-clippy-final}.log`, state
  `/tmp/fm-runtime-window-revision-state-{final,targeted-final}.log`, daemon
  `/tmp/fm-runtime-window-revision-tests.log`의 daemon 결과만 성공 근거로 사용한다.
- **AR-039 해결.** 준비 시점 snapshot과 나중 owned 저장의 경합은 창별 저장
  UUID로 검출한다. UI mutation queue가 직렬화되는 경우에도 background 저장의
  완료 순서가 승인 조건을 바꾸는 것은 별도로 검사한다.

### AR-009 완료 대조 · 현재 남은 medium은 4개

| 요구 경계 | 구현과 실제 근거 |
| --- | --- |
| late/offline source 저장이 승인 화면을 되살리지 않음 | Host authority 기반 `reconcile`, state 저장·복원 회귀와 이전 Visible/Background live |
| ACK와 List timeout의 불확실한 결과 | `cfca42dc`, Host SIGSTOP 뒤 같은 relay의 read-only 재조회·동일 PID 입력 복구 27개 live |
| 새 workspace 대기 실패에 따른 사용자 tab 손실 방지 | `344b0bda`, timeout 후 Agent binding·일반 tab 보존·명시적 재연결 live |
| 목적지 저장 전 GUI crash | `17deac70`, Host의 layout→authority 저장 순서와 두 번째 쓰기 실패 회귀; GUI 저장 거절·SIGKILL 후 정확한 위치·추가 tab·입력 복원 |
| 준비 후 최신 창 저장의 덮어쓰기 | AR-039의 UUID 검사·state 회귀와 이번 autosave 순서 제어 live |
| 같은 GUI 안의 이동 완료 후 복원 | 성공 응답 전 기존 `save_now` 대기; 저장 전에 실패하면 성공을 보고하지 않음 |

위 범위의 **AR-009를 해결로 전환한다.** 과거 절의 미해결 문구는 당시 결과다.
이로써 계획 전체가 완료된 것은 아니다. 최신 미해결 medium은 **AR-006·008(macOS)·
010·037의 4개**다. macOS와 실제 provider 종료 계약·전체 메모리 peak의 근거를
Linux GUI 결과로 대체하지 않는다. 사용자 GUI PID `1663486`과 설치본은 유지했다.

### AR-040 · medium · macOS launchd 제출 파일과 가려진 실패 원인

- 원격 run `44027300607`의 macOS job `133852192128`은 build와 native
  agent-hooks를 통과했지만 Session Paths가 35.331초 뒤 exit 101로 실패했다.
  Resume 오류는 `launchctl kickstart`의 서비스 없음(exit 113)뿐이었으며, 앞선
  bootstrap 실패의 status·stderr가 버려졌다. 실패 artifact는
  `/tmp/fm-macos-evidence.zip`의 `session-paths.log`다.
- private 임시 제출 파일을 숨김 `.tmp*` 대신 `flowmux-agent-host-*.plist`로
  만든다. Apple 공개 launchctl의 `path_goodness_check`는 일반 파일의 `.plist`
  확장자를 검사한다. 최신 native 실패의 직접 원인이 이 검사인지 아직
  증명하지 않았으므로 해결로 선언하지 않는다. bootstrap과 kickstart가 모두
  실패하면 두 명령의 status·stderr를 보존한다. 기존 Host 대체·자동 재시작은 없다.
- 파일 확장자·비숨김 이름·0600 권한·literal JSON·drop 후 삭제 회귀 **2개 통과**.
  로그 `/tmp/fm-runtime-launchd-plist-tests.log`. Linux의 파일 검증은 실제
  macOS launchd 동작 검증을 대체하지 않는다. AR-040은 AR-037의 시작 실패
  하위 항목이며 native 재검증까지 열어 둔다.
- 같은 run의 Linux gate도 `managed-session-flow`의 초기 native 창 활성 대기에서
  실패했다. 실행 전 단계의 fixture 실패이며 제품 성공으로 세지 않는다.
  해당 경로는 별도 재현·수정으로 추적한다.

### AR-041 · medium · managed Session Flow 검사의 창 활성 대기

- 같은 run의 Linux job `133852191750`은 `managed-session-flow` 시작 후
  AT-SPI `FRAME.ACTIVE` 대기 30초로 실패했다. 직전 Session Flow·Sessions 저장
  검사는 통과했다. 기존 fixture의 로컬 재실행은 **9개 통과·exit 0**이므로
  제품 실패를 재현했다고 주장하지 않는다. 로그
  `/tmp/fm-runtime-managed-flow-repro.log`, artifacts `/tmp/fm-gui-tca4a9yq`.
- fixture는 자체 Xvfb에 window manager 없이 창을 띄우고 `SetInputFocus`한다.
  키보드 입력 전 조건을 AT-SPI의 창 활성 표시 대신 실제 X 서버의
  `GetInputFocus`가 목적지 window인지 확인하도록 바꿨다. 이어지는 shortcut과
  Resume·Fork는 계속 native 입력으로 검증한다. 실패를 무시하거나 wait를
  늘리지 않는다. 이 조건은 사용자 display와 분리된 fixture에만 적용한다.
- 변경 후 같은 실제 GUI 경로 **9개 통과·exit 0**. Resume·Fork·Background
  재연결·종료된 node 새 실행·GUI 종료 후 동일 PID/birth 복원·새 입력 ACK·
  자연 종료와 빈 Host 회수를 확인했다. 로그
  `/tmp/fm-runtime-managed-flow-native-focus.log`, artifacts `/tmp/fm-gui-owwx3jcx`.
  원격 실패 환경 재검증은 다음 CI 결과로 확인한다. 제품의 미해결 medium
  4개를 이 fixture 수정으로 닫지 않는다.

### AR-042 · medium · 좁은 이력을 넓힐 때 예약을 초과하는 임시 할당

- `Terminal::resize`가 새 열 수의 history 한도 적용 전에 Alacritty grid를
  확장했다. 2열에 hard line 50,000개를 채우고 500열로 넓히면 버려질 이력까지
  넓힌 후 제거했다. 일반·대체 화면의 격리 회귀에서 최대 RSS 증가
  **598,476KiB(약 584MiB)**를 재현하여 실행별 384MiB 예약을 초과했다.
  로그 `/tmp/fm-runtime-resize-peak-repro-corrected.log`.
- 기존 `set_options`를 `resize` 앞으로 옮겨 새 열 수에 맞는 오래된 이력을
  먼저 제거한다. 대체 화면이 활성일 때도 inactive primary grid에 적용된다.
  새 abstraction·의존성은 없다. 넓어진 행 예산을 넘는 오래된 history는
  제거하지만 현재 화면과 최신 출력은 보존한다.
- 수정 후 동일 격리 회귀의 최대 증가 **11,900KiB(약 11.6MiB)**. 별도 자식
  test process에서 kernel `VmHWM`을 측정하여 다른 병렬 검사의 할당과 분리했다.
  daemon library **287개 통과·1개 ignored**, 로그
  `/tmp/fm-runtime-resize-peak-daemon.log`. 첫 fixture 시도는 채우지 않은 visible
  행 수와 줄바꿈된 ALT 문자열의 전제를 잘못 세워 실패했고, 그 결과는 메모리
  재현 근거에 포함하지 않는다.
- 실제 사용자 서비스·격리 GUI에서 좁은 Background history를 열어 같은
  PID/birth·새 입력 ACK·기존 Agent 보존을 확인했다. 해당 단계의 Host 최대
  RSS는 **22,876→34,828KiB**였다. artifacts `/tmp/fm-gui-k9mttu9h`, 로그
  `/tmp/fm-runtime-resize-peak-live.log`. 이후 ACK 유실 검사가 protocol 15에서
  필수인 prepared checkpoint 없이 직접 GUI 승인을 보내 exit 1로 실패했다.
  **전체 live gate 통과로 세지 않는다.** 이 별도 fixture 결함은 AR-043으로
  수정하고 재검증한다. 새 메모리 시나리오·화면/입력 검증 단계는 통과했다.
- 제품 build·daemon all-targets Clippy(`-D warnings`)·format·diff 공백·Python
  AST 검사를 통과했다. 로그 `/tmp/fm-runtime-resize-peak-{build,clippy}.log`.
  AR-042의 resize 순서 결함은 수정했으며 후속 전체 live gate 결과는 AR-043에
  기록한다. 사용자 GUI PID `1663486`과 설치본은 유지했다.
- 이 수정은 AR-006 전체 회계 증명이 아니다. IPC 역직렬화·control/metadata와
  destination checkpoint의 전체 peak 검증, macOS containment와 provider 종료
  계약은 그대로 남는다. 현재 실행 중인 CI `44031706354`는 `702c0253` 대상이며
  이 resize 수정의 검증 근거로 사용하지 않는다.

### AR-043 · medium · ACK 유실 fixture의 protocol 15 checkpoint 누락

- AR-042의 전체 GUI gate가 실제 relay와 mock GUI 사이의 ACK 유실 검사에서
  `unable to persist Agent view ownership: io: No such file or directory`로
  실패했다. mock GUI가 필수 destination checkpoint 없이 `GuiViewReady`를
  전송했다. 제품이 준비되지 않은 목적지를 거부한 것이며 검증을 우회하지 않는다.
- mock GUI는 정확한 lease·instance·epoch·surface·인증된 현재 PID와 새 window
  owner를 가진 private checkpoint를 원자적으로 준비한다. 승인 뒤 Host가 이를
  소비했는지도 assert한다. 새 window이므로 이전 save UUID는 없다.
  이 fixture는 private 저장 형식에 결합되며 형식 변경 시 함께 갱신해야 한다.
- 수정 후 `test-agent-host-gui.py --service-start --protected-pid 1663486`는
  **28개 통과·exit 0**. artifacts `/tmp/fm-gui-4n60v19t`, 로그
  `/tmp/fm-runtime-resize-checkpoint-live-final.log`. 세 가지 ACK 유실·동일 relay의
  새 한글 입력·탭 이동/재연결·GUI 종료/SIGKILL 뒤 동일 실행 복원·자연 종료와
  빈 Host 회수를 확인했다. AR-042의 live resize 최대 RSS는
  **21,808→32,692KiB**로 증가했다.
- Python AST·diff 공백 검사를 통과했다. fixture가 시작한 실행 8개는 모두
  회수됐고 사용자 GUI PID `1663486`은 유지했다. 이 결과로 AR-042·043의
  전체 live 후속 검증을 완료한다. 나머지 medium 4개와 macOS CI 대기는 남는다.

### AR-044 · medium · IPC byte 상한 안의 과도한 JSON 구조

- IPC의 1MiB line 상한은 작은 값이 매우 많은 JSON의 serde 임시 구조를
  제한하지 않았다. 300KB의 Ping에 무시되는 배열 값 150,001개를 넣은 회귀는
  수정 전 실제 handler의 Pong을 받아 실패했다. 로그
  `/tmp/fm-runtime-json-structure-repro.log`. 이 검사는 기존 경로의 허용을
  재현한 것이며 Host 전체 예산 초과 RSS를 측정한 결과로 해석하지 않는다.
- 공통 서버는 역직렬화 전에 문자열 밖의 JSON 구분자 수를 32,768개로 제한한다.
  인용부호·이스케이프를 고려한 상수 공간 scan이며 JSON 문법 검증은 계속 serde가
  담당한다. 초과 연결은 byte 상한과 같이 handler 호출 전에 닫는다. 변경 명령을
  재시도하지 않으며 payload를 로그에 남기지 않는다. 정상 최대 16KiB relay 입력과
  구분자·인용부호·역슬래시가 많이 포함된 문자열의 수용 회귀도 통과했다.
- IPC library **94개 통과**, IPC all-targets Clippy(`-D warnings`)·제품 build·
  format·diff 공백·Python AST 통과. 로그
  `/tmp/fm-runtime-json-structure-{ipc,clippy,build}.log`.
- 실제 격리 사용자 서비스·GUI **29개 통과·exit 0**. 32개의 과도한 요청을 거부한
  뒤 기존 PID/birth·live VTE의 새 한글 입력·목록을 확인했다. 이후 resize·7개 실행
  admission·ACK 유실·GUI 종료/SIGKILL 복원·자연 종료도 통과했다. artifacts
  `/tmp/fm-gui-nfwlc2to`, 로그 `/tmp/fm-runtime-json-structure-live.log`.
  fixture process 8개가 모두 회수됐고 사용자 PID `1663486`과 설치본은 유지했다.
- **AR-044 해결, AR-006 전체는 미완료.** 저장 state/checkpoint의 전체 읽기와
  control/metadata의 누적 peak까지 검증해야 한다. 원격 CI `44031706354`는
  이 수정 전 `702c0253` 대상으로 계속 진행 중이다.

### AR-045 · medium · macOS Resume fixture의 셸 identity와 순간 status 의존

- 원격 run `44031706354`의 Session Paths는 managed binding·첫 marker의 home·cwd·
  argv 검증을 지난 뒤 `Cannot load sessions: The agent is no longer running in this tab`으로
  실패했다. 로그 `/tmp/fm-runtime-mac-latest/session-paths.log`, exit 101.
  launchd 시작 실패는 이 실행에서 재현되지 않았지만 이후 자연 종료 단계는 실행되지 않았다.
- fake `codex`는 `/bin/sh`에서 marker를 쓰고 대기했다. macOS의 셸 comm/argv는
  Codex identity가 아니며, 공통 procmon은 셸 인자를 agent 실행 증거로 인정하지 않는다.
  fixture를 인식 가능한 `codex.js`의 Node exec로 바꿨다. marker는 exec 이후에만
  쓰고 Node의 절대 경로를 미리 확보해 Finder의 짧은 PATH 검사도 유지한다.
- `Session opened.` 순간 문구 대신 새 managed target을 명시적으로 refresh해
  실제 Codex history 1건을 기다린다. 제품의 PID/birth·provider 식별 검증은 유지하며
  자연 종료·빈 Host 비재시작·두 번째 login PATH Resume의 assertion도 유지한다.
- 로컬 source 추출 self-check는 `--help`, 짧은 PATH의 두 launch, 한글·따옴표 cwd/home·
  argv·exec 후 Node identity·입력 후 exit 0을 확인했다. procmon library **17개 통과**
  (`/tmp/fm-runtime-mac-fixture-procmon.log`), format·diff 공백 검사 통과.
  이 환경은 Linux이므로 **AR-045 native 검증 및 AR-037은 원격 결과 대기**다.

### AR-046 · medium · Host admission 파일의 무제한 읽기·쓰기

- prepared checkpoint·authority와 Host가 병합하는 GUI state를 크기 제한 없이
  읽었다. 수정 전 유효 JSON checkpoint에 32MiB 공백을 덧붙인 회귀 검사가
  `accepted oversized …agent-view-prepared…json`으로 실패했다. 로그
  `/tmp/fm-runtime-state-limit-repro.log`. 이는 과도한 파일 수용 재현이며 전체
  Host RSS 예산을 초과했다고 측정한 결과는 아니다.
- admission 경로의 파일별 byte 상한을 32MiB로 둔다. metadata의 조기 거절 외에
  실제 읽기도 상한+1 byte로 제한한다. 출력은 streaming serializer의 writer에서
  제한하며, 초과·직렬화 실패 시 임시 파일을 제거하고 기존 파일을 유지한다.
  checkpoint의 준비·discard·Host commit, authority 읽기·쓰기, Host의 GUI state
  읽기·병합 저장에 같은 경계를 적용한다. 일반 GUI state 저장·복원은 계속 허용한다.
- 상태 library **97개 통과**, all-targets state Clippy(`-D warnings`)·GUI/CLI/Host
  제품 build·format·diff 공백·Python AST 통과. 로그
  `/tmp/fm-runtime-state-limit-{state,clippy,build}.log`. 회귀는 checkpoint·state·
  authority 각각의 초과 거절과 기존 source/authority byte 보존, 정상 크기 복구 후
  성공을 확인한다. 한글·JSON escape를 포함한 정확한 출력 경계와 임시 파일 정리도
  확인하며, 큰 일반 state의 GUI load는 유지한다.
- Host 회귀 **33개 통과·exit 0**, 로그 `/tmp/fm-runtime-state-limit-host.log`.
  기존 lease handoff·GUI admission·자연 종료·종료된 launch의 용량 반환도 확인했다.
- 격리 사용자 서비스·실제 GUI **30개 통과·exit 0**, 로그
  `/tmp/fm-runtime-state-limit-live.log`, artifacts `/tmp/fm-gui-d1lhai_t`.
  실제 relay의 과도한 checkpoint를 거절한 뒤 저장 state·authority·기존 lease·
  PID/birth와 새 한글 VTE 입력을 보존했다. 정상 checkpoint로 복구한 뒤 같은
  relay의 admission과 ACK 유실·탭/GUI 종료·SIGKILL 복원·자연 종료도 통과했다.
  fixture process 8개는 모두 종료됐고 사용자 GUI PID `1663486`은 유지했다.
- **제한:** 32MiB를 넘는 큰 창 상태는 일반 복원이 가능해도 managed 연결/이동은
  거절될 수 있다. 이 변경은 파일 buffer 상한이며, JSON 구조 확장·여러 epoch의
  authority map·동시 IPC까지 합친 전체 peak 회계는 AR-006으로 남는다.
  macOS CI `44034889037`은 이 변경 전 `7a4c26a3` 검증이다.

### AR-047 · medium · admission JSON 구조와 여러 authority의 누적 읽기

- AR-046의 byte 상한만으로 많은 작은 JSON 값의 임시 구조를 제한하지 못했다.
  checkpoint에 131,073개 값의 무시되는 배열을 추가한 회귀는 수정 전
  `accepted excessive JSON structure …agent-view-prepared…json`으로 실패했다.
  로그 `/tmp/fm-runtime-state-structure-repro.log`. 이는 수용 재현이며 전체 RSS
  예산 초과를 측정한 것으로 해석하지 않는다. reconciliation은 각 epoch 파일의
  개별 byte 상한만 검사해 전체 map의 합계도 제한하지 않았다.
- IPC의 기존 상수 공간 검사기를 core의 chunk 기반 `JsonStructureBudget`으로
  옮겨 재사용한다. IPC의 32,768개 상한은 유지하며 admission 파일은 문자열 밖
  구분자 131,072개로 제한한다. 읽기와 streaming 출력 모두 적용해 큰 임시 JSON
  구조와 다음 연결에서 읽지 못할 checkpoint의 저장을 막는다. 문자열 escape의
  모든 chunk 분할 위치를 검사한다. JSON 문법 검증은 계속 serde가 담당한다.
- 같은 reconciliation의 모든 epoch authority raw byte 합계는 32MiB까지만
  허용한다. 초과하면 state/authority를 쓰지 않고 명시적 오류를 반환한다.
  각각 17MiB인 두 유효 authority의 합계 초과와 정상 파일 복구를 검증했다.
- core **182개**, IPC **94개**, state **98개**, 합계 **374개 통과**, 로그
  `/tmp/fm-runtime-state-structure-tests.log`. 세 crate의 all-targets Clippy
  (`-D warnings`)·제품 build·format·diff 공백·Python AST도 통과했다. 로그
  `/tmp/fm-runtime-state-structure-{clippy,build}.log`.
- 실제 격리 사용자 서비스·GUI **31개 통과·exit 0**, 로그
  `/tmp/fm-runtime-state-structure-live.log`, artifacts `/tmp/fm-gui-8k7fz05r`.
  JSON 구조 초과 checkpoint를 거절한 뒤 기존 state·authority·lease·PID/birth와
  한글 VTE 입력을 유지했다. 정상 checkpoint로 복구한 동일 relay의 admission,
  ACK 유실·GUI 종료/SIGKILL 후 복원·자연 종료까지 통과했다. fixture process
  8개는 모두 종료됐고 사용자 GUI PID `1663486`과 설치본은 유지했다.
- **AR-047 해결, AR-006 전체는 열려 있다.** 이 상한은 메모리 회계의 입력 경계를
  보완한다. VT·동시 IPC·lifecycle ledger·metadata를 합친 worst-case peak와
  예약 크기의 적정성 검증은 아직 필요하다. 원격 CI `44034889037`은 이 변경 전
  `7a4c26a3`의 macOS Resume 수정을 검증 중이다.

### AR-048 · medium · hook ledger의 해결 ID·자식 순서 기록 무제한 누적

- `WaitResolved`의 해결 ID와 Codex child별 turn/sequence tombstone은 지연 hook이
  대기를 다시 열거나 작업을 잘못 완료하지 않도록 보관한다. 각 사건이 64KiB
  이하여도 이 기록이 session 중 계속 누적됐다. 수정 전 실제 Host RPC 회귀는
  `unbounded resolved tool IDs were retained`로 실패했다.
  로그 `/tmp/fm-runtime-lifecycle-limit-before.log`.
- 공통 runtime에서 문자열 capacity와 map slot을 회계하며, managed Host는
  사건 적용 후 charge가 8MiB를 넘으면 `Unknown`과
  `Observation limit reached; execution continues`를 투영한다. 상한을 넘긴
  사건의 완료·알림과 이후 hook·화면/grace 완료를 적용하지 않는다. 최대 한
  사건의 추가 할당이 가능하며 이 charge는 OS RSS의 강제 상한이 아니다.
- 기존 wait·해결 ID·child 기록과 실행·PTY를 보존한다. GUI 종료/재접속이나
  같은 native session의 중복 시작으로 불확실성을 지우지 않는다. 검증된 새
  native session 또는 종료 경계에서 ledger를 정리하고 관측을 복구한다.
  일반 tab-bound runtime에는 이 Host 제한을 적용하지 않는다.
- daemon library **288개 통과·1개 ignored**, 로그
  `/tmp/fm-runtime-lifecycle-limit-daemon.log`. 이후 확장한 회귀에서도 일반 tool ID와
  상태 전이 없는 Codex child Stop의 누적 상한, 이후 Stop의 무효·완료 알림 부재·
  같은 PID/birth와 새 session 복구를 통과했다.
  로그 `/tmp/fm-runtime-lifecycle-limit-after.log`. daemon all-target Clippy
  (`-D warnings`)·제품 build·format·diff 공백·Python AST도 통과했다.
  로그 `/tmp/fm-runtime-lifecycle-limit-{clippy,build}.log`.
- 첫 격리 live에서는 새 상한·GUI 재접속·입력과 새 session 복구까지 통과했으나,
  마지막 기존 Host 부재 검사에서 이름을 바꾼 helper의 호출 한 곳을 놓쳐
  `NameError`로 exit 1이었다. 해당 호출을 수정하고 같은 suite를 재실행했다.
  로그 `/tmp/fm-runtime-lifecycle-limit-live.log`.
- 최종 격리 사용자 서비스·GUI **33개 통과·exit 0**, 로그
  `/tmp/fm-runtime-lifecycle-limit-live-final.log`. 상한 도달 후 기존 승인 대기·
  같은 PID/birth·새 한글 입력·spawn 1회를 유지했고, 실제 Agents 표시와 GUI
  종료/재접속 후 불확실성 보존, 중복 session 시작 거절과 새 session 복구를
  확인했다. 이후 자연 종료·빈 Host 정리·Host 부재 복원까지 통과했다. 기록된
  fixture process 9개는 모두 종료됐으며 사용자 GUI PID `1663486`은 유지했다.
- **AR-048 해결, AR-006 전체는 열려 있다.** 일반 tab-bound runtime의 크기나
  모든 동시 할당의 peak까지 제한한 것으로 해석하지 않는다.
- AR-006의 전체 peak 회계·macOS containment·provider 종료 계약과 native macOS
  결과는 이 변경으로 닫지 않는다.

### AR-049 · medium · macOS 두 번째 Resume fixture의 Host helper 탐색 누락

- 원격 run `44034889037`의 Session Paths 단계는 첫 managed Resume와 자연 종료,
  빈 Host 비재시작을 통과한 뒤 `selected session uses login shell PATH`에서
  30초 timeout으로 실패했다. 원본 evidence는
  `/tmp/fm-runtime-mac-44034889037/session-paths.log`다.
- Cargo의 native harness는 `target/debug/deps`에 있고 Host는 `target/debug`에
  있다. 첫 실행은 PATH에서 Host를 찾지만, 두 번째 실행을 위해 PATH를 시스템
  경로로 제한하면 Host 탐색이 실패한다. 설치 앱의 sibling helper 배치와 다르다.
- 실제 `flowmux_terminal::find_flowmux_daemon`을 링크한 임시 probe를 `deps`에서
  실행해 PATH 제거 전 탐색 성공·제거 후 실패를 재현했다. 수정된 fixture PATH는
  built Host 디렉터리를 유지하면서 provider `codex`가 여전히 없음을 assert한다.
  이 probe는 수정 후 Host 탐색 성공도 확인했다.
  로그 `/tmp/fm-runtime-helper-path-probe.log`.
- 두 Resume 모두 timeout 시 panel 오류와 활성 terminal 화면을 출력하도록 기존
  진단을 공유했다. fixture에 Host 탐색과 provider 부재 assert를 추가했다.
  제품 helper 탐색 규칙이나 provider의 login shell PATH 동작은 변경하지 않았다.
- format·diff 공백 검사는 통과했다. 이 Linux probe는 native launchd·zsh 실행을
  검증하지 않으며 AR-049의 macOS 실기 결과와 AR-037은 계속 대기한다.

### AR-050 · 검증 경로 개선 · 플랫폼별 수동 CI 분리

- run `44038755253`은 `pending`, 앞선 run `44034889037`의 Linux job은
  `in_progress`였다. 같은 branch의 수동 검증 전체가 concurrency group 하나를
  공유하므로 macOS 러너가 비어도 새 native 수정 검증이 이전 Linux를 기다린다.
- 수동 Test workflow에 `platform=all|linux|macos` 선택을 추가하고 플랫폼별
  concurrency group을 사용한다. 기본값과 push·PR·주간 검사는 기존 전체 범위를
  유지한다. macOS 선택은 native job 전체를 실행하며 Linux gate나 stress를
  성공한 것으로 처리하지 않는다. 실행 중 검사를 종료할 필요도 없다.
- YAML을 읽어 전체/각 플랫폼 수동 실행 및 push·PR·schedule의 job 선택 6개
  조합과 concurrency 분리를 확인했다. 로그
  `/tmp/fm-runtime-ci-platform-check.log`. diff 공백 검사도 통과했다.
  이 검사는 GitHub 실행 엔진이나 native 결과를 대신하지 않는다.


### AR-051 · medium · 원격 Session Flow 검사의 단축키 진입 재실패

- run `44034889037`의 Linux gate는 `managed-session-flow` 시작 직후
  `Claude sessions`를 찾지 못해 실패했다. 앞선 AR-035의 X focus receipt만으로
  GTK 단축키 처리를 보장하지 못했다. 전체 gate 성공으로 기록하지 않는다.
- 같은 제품의 로컬 재현은 **9개 통과·exit 0**이었다. 로그
  `/tmp/fm-runtime-session-flow-ci-repro.log`, artifacts `/tmp/fm-gui-dxj4g1zx`.
  원격 실패가 제품 단축키 버그인지 초기 입력 경합인지는 확정하지 않았다.
- fixture의 최초 패널 열기만 기존 `Agent sessions` 버튼의 AT-SPI Action으로
  바꿨다. 접근성 트리에서 버튼이 준비될 때까지 기다리고 native action 성공을
  assert한다. 화면 밖 footer 좌표나 단축키 재전송은 사용하지 않는다.
  이후 실제 마우스 선택·Host fork·Background 재연결·종료/복원 검사는 유지한다.
- 수정 후 격리 **9개 통과·exit 0**, 로그
  `/tmp/fm-runtime-session-panel-action.log`, artifacts `/tmp/fm-gui-maezsvc5`.
  같은 PID/birth·새 입력·자연 종료·빈 Host 정리를 확인했고 사용자 GUI를 유지했다.
  Python AST·diff 공백 검사도 통과했다. 단축키 자체의 native 수용 검증이나
  원격 gate 재통과를 뜻하지 않는다.


### AR-052 · 수용 검증 · GUI 없는 31분 실시간 작업 생존

- Host live fixture에 `--headless-seconds`를 추가했다. GUI 정상 종료 후
  Working 실행을 실제 시간으로 관측한다. 음수는 거절하고 기본 suite 시간은
  늘리지 않는다. 진행 파일은 기존 fixture의 atomic rename 경로를 사용한다.
- 매 5초 Host와 Agent의 PID/birth·Running/Working·view/attachment 부재·
  증가하는 진행 값·spawn 1회·보호된 사용자 GUI 생존을 assert하고
  `headless-duration.jsonl`에 기록한다. 이후 hook IPC의 30초 idle timeout을
  고려해 새 연결을 열고, 기존 GUI 재접속·새 입력·비정상 종료 검증을 계속한다.
- `--service-start --protected-pid 1663486 --headless-seconds 1860`의 전체 suite가
  **34개 통과·exit 0**으로 끝났다. 로그 `/tmp/fm-runtime-headless-31min-live.log`,
  artifacts `/tmp/fm-gui-4n014rog`. GUI 없는 실제 **1860.001초·372회** 관측에서
  Host `40297`/birth `14695769`, Agent `40325`/birth `14695775`를 유지했다.
  첫·마지막 진행 값은 **629 → 18,963**이며 모든 인접 관측에서 증가했다.
- 이후 저장된 tab 자동 재연결·새 한글 입력·격리 GUI SIGKILL 후 같은 실행
  재연결·관측 한도 도달 시 wait 보존과 Unknown 표시·새 native session의 관측
  복구·자연 종료·빈 Host 회수·Host 부재 시 자동 재시작 금지까지 통과했다.
  spawn은 1회였으며, 기록된 fixture process와 Host·Agent의 잔여 PID는 없다.
  보호된 사용자 GUI `1663486`은 유지했다.
- [검증 기록](design-assets/agent-runtime-headless-31min-20261117.json)에
  모든 pass 항목·관측 요약·로그/관측 SHA256·실행 중 수집한 binary SHA256을
  보관한다. 장시간 관측 Host는 AR-053 이전 제품이므로 최신 source 전체의
  장시간 수용 검증으로 확대하지 않는다.
- Python AST·diff 공백 검사는 통과했다. 이 실행은 실제 PTY workload fixture이며
  실제 provider/model 작업이나 완료 후 안전한 자동 회수를 검증하지 않는다.


### AR-053 · medium · cgroup 관측의 형제 디렉터리 경로 누적

- Linux `ExecutionScope::observe`는 자식 디렉터리 경로를 모두 Vec에 넣은 뒤
  하나씩 순회했다. 자식 cgroup이 많으면 관측용 임시 경로가 그 수만큼 누적돼
  terminal·lifecycle의 개별 제한 밖에서 Host 메모리를 늘렸다.
- 같은 synthetic cgroup 검사에서 형제 20,000개와 각각의 중첩 child를 읽었다.
  변경 전후 기능 검사 모두 통과했으며 `/usr/bin/time -v`의 최대 RSS는 각각
  **17,504KiB → 12,880KiB**였다. 한 번의 비교 측정이며 전체 Host 상한이나
  일반 성능 수치로 확대하지 않는다. 로그
  `/tmp/fm-runtime-scope-wide-{before,after}.log`.
- 형제 경로를 모으는 대신 현재 경로와 조상 디렉터리의 `ReadDir` cursor만
  유지한다. 누락·삭제 경합과 I/O 오류는 보수적으로 처리하며 birth 확인과
  PID 중복 제거를 유지한다. 열린 cursor는 깊이에 비례하며 시스템 FD 오류도
  관측 실패로 처리한다. PID map과 전체 동시 peak 회계는 AR-006으로 남긴다.
- 작은 회귀 검사에서는 형제/중첩 cgroup 64개, 같은 PID/birth 중복 제거,
  이미 수거된 하위 디렉터리와 symlink 제외, 잘못된 PID 때 비어 있다고
  오판하지 않는 조건, scope 소멸을 확인한다. daemon library **289개 통과·
  1개 ignored** (`/tmp/fm-runtime-scope-walk-daemon.log`), daemon all-target
  Clippy·build·format·Python AST·공백 검사도 통과했다.
- 실제 Linux containment fixture의 분리 자식을 자기 실행 scope 아래 `nested`
  cgroup으로 옮기도록 강화했다. 중첩 자식의 부모 종료 생존, 정상/강제 종료와
  다른 실행 보존, 빈 Host 회수 **3개 통과·exit 0**.
  로그 `/tmp/fm-runtime-scope-walk-nested-final.log`, artifacts
  `/tmp/fm-containment-u8l048f7`; 기록된 잔여 소유 process는 없다.
- 격리 실제 GUI **5개 통과·exit 0**: 부모 종료 뒤 같은 자식 PTY의 첫 연결과
  재연결, 새 한글 입력, 현재 위치 조회·알림과 위조 identity 거절, 자연 종료와
  빈 Host 회수를 확인했다. 로그 `/tmp/fm-runtime-scope-walk-root-exit-live.log`,
  artifacts `/tmp/fm-gui-ecv78i98`. 사용자 GUI PID `1663486`은 유지했다.

### AR-054 · AR-037 후속 · Host 시작 제한보다 긴 launchd 기본 throttle

- 원격 run [44038755253](https://github.com/flowmux-ai/flowmux/actions/runs/44038755253)의
  macOS job `133890070025`는 첫 managed Resume·history·자연 종료·빈 Host
  비재시작을 통과한 뒤 두 번째 Resume에서 실패했다. AR-049의 진단으로
  `launchctl bootstrap` exit 5 뒤 `kickstart`의 **5초 시작 timeout**을 확인했다.
  로그 `/tmp/fm-runtime-mac-44038755253/session-paths.log`와 artifact
  `14310230942`를 보관했다. 이 run의 macOS gate는 실패다.
- [Apple의 launchd.plist 문서](https://github.com/apple-oss-distributions/launchd/blob/main/man/launchd.plist.5)는
  `ThrottleInterval`을 생략하면 기본적으로 10초 안에 다시 spawn하지 않는다고
  명시한다. 빈 Host를 바로 종료한 뒤 명시적 launch하는 제품 정책과 5초 시작
  제한에 맞춰 새 job의 `ThrottleInterval`을 0으로 지정한다. `KeepAlive=false`와
  기존 Host가 있으면 교체하지 않는 경로, `kickstart`의 `-k` 금지는 유지한다.
- 이 정책 충돌을 고치는 변경이며, 관측된 `kickstart` timeout의 유일한 원인이
  throttle이라는 주장은 하지 않는다. 이미 등록된 이전 job의 설정도 바꾸지 않는다.
  실제 macOS의 즉시 두 번째 Resume와 이후 빈 Host 비재시작 검증이 필요하다.
- Linux에서 launchd job의 literal 인자·private 파일·`KeepAlive=false`·
  `ThrottleInterval=0` 검사 **2개 통과**. 로그
  `/tmp/fm-runtime-launchd-throttle-tests.log`. CLI all-target Clippy
  (`-D warnings`)·format·공백 검사도 통과했다. 로그
  `/tmp/fm-runtime-launchd-throttle-clippy.log`. 이 검사는 실제 launchd 동작을
  대체하지 않으며 AR-037은 계속 열어 둔다.

### AR-055 · medium · managed Working 행의 정적 아이콘

- 일반 Agents 행은 Working에 GtkSpinner를 쓰지만 managed registry 행은 정적
  Image만 표시했다. 설계의 애니메이션 유지 조건을 충족하지 못했다. 같은 행의
  GTK 회귀 검사는 수정 전 GtkImage 때문에 실패했다
  (`/tmp/fm-runtime-managed-spinner-before.log`).
- 기존 row/button identity를 보존하면서 Working·Running일 때만 spinner를
  표시하고 애니메이션을 켠다. 다른 상태와 연결 손실 때는 정적 상태 아이콘을
  사용한다. 기본 presentation 역할 대신 상태 image 역할과 `Agent working`
  접근성 이름을 제공한다. row 재생성·애니메이션용 polling은 추가하지 않는다.
- 수정 후 GTK 회귀 **1개**, product build, GUI all-target Clippy(`-D warnings`),
  format·Python AST·shell 구문·공백 검사를 통과했다. 로그
  `/tmp/fm-runtime-managed-spinner-{unit,build,clippy}-final.log`.
- 실제 격리 Flowmux/VTE에서 Working·ANSI/한글 연속 출력 중 XTest native
  메뉴 클릭 **100회·누락 0회**, semantic feedback **p95 40.35ms**였다.
  진행 값은 **128 → 23,200**이며 같은 Agent PID/birth·spawn 1회를 유지했다.
  매 10회 클릭 구간의 spinner 영역 pixel hash는 각각 10개가 달랐다.
  사용자 GUI `1663486`을 유지했고 기록된 fixture PID는 모두 회수됐다.
  로그 `/tmp/fm-runtime-managed-spinner-click-final.log`, artifacts
  `/tmp/fm-gui-yjh1goxv`; [검증 기록](design-assets/agent-runtime-click-load-20261117.json).
- fixture는 출력 시작을 명시적으로 제어하고 native Escape로 각 메뉴를 닫는다.
  닫힌 popup 자식의 AT-SPI SHOWING이 남는 현상을 확인해, 열림/닫힘은 toggle의
  PRESSED 전이로 확인하며 열린 항목의 SHOWING도 검사한다. 이 표적 검사를
  Linux 공통 gate에 추가했다. 실패 시 accessibility/Host 상태를 보관한다.
- latency는 AT-SPI 상태 피드백이며 compositor 표시 시각이나 frame time이 아니다.
  spinner 관측은 표본 pixel 변화이며 모든 frame의 스케줄링 보장이 아니다.
  실제 provider/model과 전체 remote gate의 수용 검증으로 확대하지 않는다.
- 같은 제품의 native managed drag 회귀 **24개 통과·exit 0**. 행/Agents bar의
  pane·tab 순서·workspace 이동, 취소·목적지 소멸·Background 재연결, GUI 종료 뒤
  같은 실행 생존을 확인했다. 로그 `/tmp/fm-runtime-managed-spinner-drag-regression.log`,
  artifacts `/tmp/fm-gui-rgpbv69f`; 기록된 fixture PID는 모두 회수됐다.

### AR-056 · medium · 원격 Sessions 진입 재실패와 진단 자료 누락

- Linux run [44040064061](https://github.com/flowmux-ai/flowmux/actions/runs/44040064061)의
  job `133892194413`은 workspace와 앞선 live phase를 통과한 뒤
  `managed-session-flow`의 `Agent sessions` 버튼 조회에서 30초 timeout·exit 1로
  실패했다. stress·editor web은 성공했으나 전체 gate는 실패다. artifact
  `14309888564`를 `/tmp/fm-runtime-linux-44040064061`에 내려받아 확인했다.
  시작 실패의 제품 log와 accessibility tree가 업로드 대상에서 빠져 있어
  원격 원인을 확정할 자료가 부족했다.
- 같은 source를 CI와 같은 중첩 Xvfb/D-Bus·`GTK_A11Y=test` 부모 환경에서
  실행한 로컬 재현은 **9개 통과·exit 0**이었다. 로그
  `/tmp/fm-runtime-session-flow-nested.log`, artifacts `/tmp/fm-gui-cwj514t2`.
  따라서 일반적인 제품 진입 실패나 원격 원인을 확정하지 않는다.
- Sessions 버튼은 가로로 스크롤되는 footer 끝에 있다. 최초 진입은 기존
  AT-SPI Action을 쓰므로 viewport SHOWING을 전제로 할 필요가 없다. 이 버튼의
  존재 조회에서만 조건을 제거하며, 이후 Claude sessions·실제 선택/버튼·상태의
  SHOWING 조건과 lifecycle assertion을 유지한다. 버튼/Action 자체가 없거나
  실패하면 검사를 계속하지 않는다.
- 시작 실패 때 전체 접근성 tree의 role/state와 격리 display screenshot을
  보관한다. 진단 실패는 원래 assertion을 덮어쓰지 않는다. CI evidence에
  시작 자료와 `session-resume.log`, 클릭 부하의 제품 log·원시 측정도 추가했다.
- 변경 후 같은 중첩 환경에서 **9개 통과·exit 0**, 로그
  `/tmp/fm-runtime-session-flow-action-offscreen.log`, artifacts `/tmp/fm-gui-pumti21w`.
  Host Resume·Fork·Background 동일 실행 재연결·GUI 종료 후 같은 PID/birth와 새
  입력·자연 종료·빈 Host 회수를 확인했다. 기록된 fixture PID는 모두 회수됐다.
  Python AST·diff 공백 검사를 통과했다.
- **원격 gate 성공과 실패 원인 확정은 미완료다.** 이 조건 제거가 원격 실패를
  해결했다고 단정하지 않으며 해당 run을 성공으로 변경해 기록하지 않는다.


### AR-057 · medium · 클릭 부하 단계 추가 뒤 CI 자체 검사 수 불일치

- run [44046279141](https://github.com/flowmux-ai/flowmux/actions/runs/44046279141)의
  Linux job `133912647586`은 `ci-runner-tests`에서 실패했다. AR-055가 추가한
  `managed-agent-click-load`를 예상 단계 수에 반영하지 않아 regression은
  **45 != 44**, extended는 **47 != 46**이었다. 제품 build·workspace·GUI 검사는
  시작하지 않았으며 stress·editor web 성공을 Linux gate 성공으로 확대하지 않는다.
- 로컬의 같은 기존 검사에서도 두 assertion 실패를 재현했다.
  `/tmp/fm-runtime-ci-click-count-before.log`, **1개 실행·2개 subtest 실패·exit 1**.
- 예상 단계 수를 실제 등록된 45/47개에 맞춘다. 숫자만 바꿔 검사를 통과시키지
  않도록 두 suite 모두 `--managed --click-load` 호출·`--host` 전달과
  `managed-agent-click-load` timing 기록을 별도로 요구한다. 고유 단계 이름·
  모든 exit code·다른 기능 검사와 stress 분리 검증은 유지한다.
- 수정 후 CI runner 전체 **26개 실행·25개 통과·1개 macOS 전용 skip·exit 0**
  (108.218초). 두 Linux suite와 기존 실패 전파·timeout·격리 검사를 포함한다.
  로그 `/tmp/fm-runtime-ci-click-count-after.log`; diff 공백 검사 통과.
- **AR-057은 로컬 검증으로 해결했다.** 제품 로직 변경이 없어 AR-055의 같은
  binary로 수행한 native GUI 100회 클릭·managed drag 24개 근거를 유지한다.
  이 mock runner 검사는 실제 GUI 수용 검증을 대신하지 않으며 전체 원격 gate와
  AR-056은 재검증 대기다. 사용자 GUI PID `1663486`과 설치본은 유지했다.


### AR-006 후속 · 동시 화면·저장 제목·느린 전송을 결합한 메모리 검증

- 기존 부하는 각 추가 실행의 출력 완료를 기다린 뒤 다음 실행을 시작했고,
  결합 문자 하나·현재 RSS 증가만 검사했다. 전체 예약 수의 동시 출력 및 두
  화면·saved title·snapshot 동시 보관에 대한 근거가 부족했다.
- 기존 live fixture의 같은 admission 구간을 보강한다. 원래 실행 1개와 추가
  실행 6개를 먼저 예약하고 공통 barrier로 출력을 시작한다. 각 100×400 terminal에
  4,096개 저장 제목·셀당 32개 4-byte 결합 문자와 색상 변경·alternate 100행 및
  primary 400행을 출력한다. 마지막 DSR의 Host 답변을 provider가 읽은 뒤에만
  준비 완료로 세어, PTY write 완료를 실제 parsing 완료로 오인하지 않는다.
- 6개 실행의 snapshot을 모두 보관하며 첫 chunk 다음의 동일 sequence 전송이
  계속되는지 확인한다. 이 상태의 VmHWM을 **Host 전체 3GiB 예산**과 비교한다.
  이전의 작은 순차 부하 전용 256MiB 증가 assertion은 이 더 큰 결합 부하에
  적용하지 않는다. 8번째 실행 거절·원래 GUI의 새 입력·자연 종료 후 예약
  재사용 검증은 유지한다. 실패해도 release marker를 먼저 기록하여 provider를
  대기 상태로 남기지 않는다.
- 실제 Linux 사용자 서비스·격리 GTK/VTE **33개 통과·exit 0**. Host RSS는
  **30,520→815,296KiB**, peak도 **815,296KiB**였다. 이후 GUI 재연결·격리 GUI
  SIGKILL·같은 PID/birth·새 입력·자연 종료·빈 Host 회수까지 통과했다.
  기록된 fixture PID 15개는 모두 회수됐고 사용자 GUI PID `1663486`은 유지됐다.
  로그 `/tmp/fm-runtime-concurrent-memory-live.log`, artifacts `/tmp/fm-gui-o8fgkouw`.
  [검증 기록](design-assets/agent-runtime-concurrent-memory-20261117.json)에 binary와
  로그의 해시·부하 값·검증 한계를 보관했다. 최종 Python AST·diff 공백 검사 통과.
- **AR-006 전체는 열려 있다.** 이번 측정은 최대 IPC 역직렬화·process/metadata
  동시 peak까지 모든 할당 경로를 증명하지 않으며 OS의 RSS 강제 상한도 아니다.
  실제 provider/model·macOS·Flatpak 검증을 대신하지 않는다. 제품 binary는
  변경하지 않았으며 사용자 앱과 설치본도 교체하지 않았다.


### AR-006 후속 · 두 IPC pool 포화와 terminal·전송 부하 결합

- 같은 live fixture에서 일반 socket 64개 pool의 남은 60개와 제어 socket
  16개를 Ping으로 admission 확인 후 유지한다. 각 peer는 30,000개 배열 원소가
  있는 정확히 1MiB JSON을 실제로 parse하고 Pong을 받은 뒤 다음 요청의
  newline을 보류한다. 이전 String capacity와 새 bounded read buffer를 함께
  보유하며 두 pool 모두 추가 연결을 거절하는지 확인한다.
- 실행 7개·무거운 terminal 6개·snapshot 전송 6개를 유지한 상태에서 모든
  peer의 요청을 순차 완성하고 다시 미완성 요청을 채운다. snapshot continuation과
  기존 GUI의 새 입력도 성공했다. 모든 역직렬화가 동시에 peak를 갖는 증명은 아니다.
- 실제 사용자 서비스·격리 GTK/VTE **33개 통과·exit 0**. RSS
  **31,716→993,748KiB**, VmHWM **993,748KiB (970.46MiB)**로 Host 3GiB
  예산 이하였다. 후속 GUI crash·같은 PID/birth 복원·자연 종료·빈 Host 회수도
  통과했다. 기록된 fixture PID는 모두 회수됐고 사용자 GUI `1663486`은 유지됐다.
- 로그 `/tmp/fm-runtime-concurrent-ipc-memory-live.log`, artifacts
  `/tmp/fm-gui-u8h5f4pa`; [검증 기록](design-assets/agent-runtime-concurrent-ipc-memory-20261117.json).
  최종 Python AST·diff 공백 검사 통과. 제품 Rust와 설치본은 변경하지 않았다.
  **AR-006은 전체 allocation/process metadata 회계가 남아 열려 있다.**


### AR-037 · native managed 시작·재시작 검증 완료

- macOS arm64 run [44042747337](https://github.com/flowmux-ai/flowmux/actions/runs/44042747337),
  job `133906463716`, revision `e59af70959b5300d57da195452bc899685540a9a`의
  **extended gate 전체 성공**, `CI_GATE_EXIT=0`을 authoritative job 결과와
  다운로드한 artifact `14312656924`로 확인했다. Session Paths는 **5.522초·exit 0**,
  `MACOS_SELECTED_SESSION_PATH_OK`·`MACOS_SESSION_PATHS_OK`를 기록했다.
- native main-thread fixture의 production Sessions Resume 경로가 Unicode
  CODEX_HOME·따옴표 cwd·MAX_CANON보다 긴 PATH와 명시적 YOLO args를 보존했다.
  실제 managed binding의 PTY에 입력한 뒤 process exit와 빈 Host socket 제거를
  확인하고 500ms 동안 비재시작을 검사했다. 이어 Finder 형태의 PATH에서 login
  `.zprofile`로 provider를 찾아 두 번째 managed Resume와 동일 종료 검사를 통과했다.
- **AR-037을 해결로 전환한다.** launchd throttle 조정이 과거 실패의 유일한
  원인이었다고 단정하지 않는다. 이 revision은 AR-055 이전이며 macOS
  fast-fork containment·전체 GUI process 종료 생존·실제 provider 작업을 검증하지 않는다.
  [검증 기록](design-assets/agent-runtime-macos-session-paths-20261117.json)에 출처·해시와 한계를 보관한다.


### AR-058 · medium · 경로 검사 reader의 공용 환경변수 잠금 누락

- Linux run [44047212972](https://github.com/flowmux-ai/flowmux/actions/runs/44047212972),
  job `133915795855`, revision `ca21b097`는 workspace 단계의
  `runtime_socket_for_pid_does_not_collide_with_legacy_default`에서 실패했다.
  config **103개 통과·1개 실패**, gate exit 101이다. 비교한 parent는 격리
  runtime과 `/home/junsu/.cache/flowmux`로 달랐다. GUI 단계 전이므로 AR-056은 유지한다.
- 같은 process에서 Flatpak 테스트가 공용 잠금을 잡고 `FLATPAK_ID`와 HOME을
  바꾸는데, 여러 runtime 경로를 비교하는 reader에는 잠금이 없었다. 따라서
  비교 도중 sandbox mode가 바뀔 수 있었다. 네 path reader 테스트도 기존
  `test_env::env_lock`을 잡고 검사가 끝날 때까지 유지하도록 수정했다.
  공용 helper 설명을 reader·writer 모두에 적용했다. 제품 경로 선택 로직은 유지한다.
- 수정 전 로컬 32-thread 전체 config 및 30회 반복은 통과하여 경합이 재현되지
  않았다. 원격 실패를 primary evidence로 보관한다. 수정 후 같은 config library
  **104개 통과·exit 0**, format·diff 공백 검사 통과.
  `/tmp/fm-runtime-config-env-{before,after}.log`,
  `/tmp/fm-runtime-linux-44047212972`의 원격 artifacts를 확인했다.
- **AR-058은 잠금 누락을 수정했다.** 기존 live 제품과 사용자 앱·설치본은 변경하지
  않았다. Linux 전체 gate와 Sessions 진입의 성공은 후속 run으로 별도 확인한다.


### AR-059 · medium · Linux 자식 수에 비례하는 Host PID map 복사

- AR-006 검토에서 Linux `ExecutionScope::observe`가 매 tick 전체 cgroup PID
  파일과 `HashMap<PID,birth>`을 만들고 actor가 다시 `owned` map에 복사함을
  확인했다. 실행 admission은 실행 개수만 제한하므로 자식 수가 늘면 이 메모리도
  늘었다. cgroup이 이미 소유권과 subtree 생존을 제공하여 중복 inventory는 불필요했다.
- Linux는 `cgroup.events`의 populated로 생존을 판단하고 root birth만 보관한다.
  명시적 정상 종료 때 `cgroup.procs`를 buffered line 단위로 순회하며 각 PID를
  pidfd·birth 검증 후 신호 처리한다. 강제 종료는 기존 atomic `cgroup.kill`,
  orphan reap은 standalone Host의 기존 공용 reaper가 담당한다. 형제 PID나 경로
  전체를 쌓지 않고 조상 directory cursor만 유지한다. 관측 오류는 빈 실행으로
  간주하지 않으며 macOS의 기존 polling 경로는 바꾸지 않는다.
- 중첩 형제 64개가 모두 방문되는 회귀와 잘못된 PID·population·scope 소멸 검사를
  포함한 daemon library **289개 통과·1개 ignored**. daemon all-target Clippy,
  제품 build·format·diff 공백 검사 통과. Clippy의 중첩 if 지적은 동등한 단일
  조건으로 정리했으며 이후 최종 binary로 아래 live 두 시나리오를 다시 확인했다.
- 실제 중첩 cgroup의 fast double-fork/setsid 자식 부모 종료 생존·정상/강제 종료·
  다른 실행 보존·빈 Host 회수 **3개 통과·exit 0**. 실제 격리 GTK/VTE의 같은
  자식 PTY 최초 연결·재연결·한글 입력·컨텍스트·알림·위조 identity 거절·자연
  종료 **5개 통과·exit 0**. containment 잔여 소유 process는 없고 기록된 GUI
  fixture PID도 모두 회수됐다. 사용자 GUI `1663486`과 설치본은 유지했다.
- 로그 `/tmp/fm-runtime-scope-stream-final-{containment,root-exit}.log`, artifacts
  `/tmp/fm-containment-xothuohq`·`/tmp/fm-gui-qiejd9w8`.
  [검증 기록](design-assets/agent-runtime-scope-stream-20261117.json).
  **AR-059 해결**. 전체 Host allocation 회계(AR-006)와 macOS containment,
  provider의 안전한 자동 종료는 별도 미완료 항목으로 유지한다.


### macOS GUI crash 검증 공백 · portable production GUI fixture

- 기존 macOS Session Paths 성공은 같은 GUI 안에서 managed Resume와 자연
  종료를 검사했으며 GUI process 자체가 종료된 뒤의 생존을 입증하지 않았다.
- `scripts/test-agent-gui-crash.py`는 private XDG·짧은 `/tmp` runtime에서 실제
  제품 GUI를 띄우고 product `agent run`으로 실행한다. fixture가 만든 GUI만
  SIGKILL한 뒤 기존 Host observer 연결을 유지하며 진행 증가를 확인한다.
  같은 private state로 새 GUI를 띄워 동일 instance·epoch·PID·birth와 spawn
  1회·저장 pane/tab·VTE 화면·새 한글 입력을 확인한다. 마지막 자연 종료와
  빈 Host socket 제거를 요구하며 실패해도 fixture provider를 해제한다.
- 저장 domain tree보다 GTK widget 생성이 늦으므로 복원 중 정확한 해당 pane
  `not_found`만 제한 시간 안에서 기다린다. 다른 IPC 오류는 즉시 실패한다.
  최초 fixture의 두 실패는 이 대기 누락이었고 제품 결함으로 분류하지 않는다.
- Linux의 실제 systemd 사용자 서비스·격리 GTK/VTE에서
  `AGENT_GUI_CRASH_RECONNECT_OK`와 `AGENT_GUI_CRASH_OK`·exit 0을 확인했다.
  사용자 GUI PID `1663486`은 살아 있고 fixture runtime process 잔여는 없다.
  [검증 기록](design-assets/agent-runtime-portable-gui-crash-20261117.json)에 binary·
  script·로그 hash와 artifacts를 기록한다.
- macOS shared gate에 독립 `agent-gui-crash` 단계를 연결했다. 실행 실패와
  성공 marker 누락 모두 gate를 실패시키는 회귀 검사를 포함한다. CI runner 전체
  **27개 실행·26개 통과·1개 macOS 전용 skip·exit 0**, Python AST·Bash 구문·diff
  공백 검사를 통과했다. 로그 `/tmp/fm-runtime-crash-gate-full.log`. macOS
  native 실행 결과는 대기 중이며 Linux 결과로 대체하지 않는다. 정상 GUI 닫기·
  macOS descendant containment·실제 provider/model·자동 회수를 입증하지 않는다.


### AR-060 · medium · 폭 축소 reflow가 이전 행 capacity를 계속 보유

- 재현: `alacritty_terminal` 0.26.0의 `Row::shrink`는 `split_off` 뒤 원래
  vector capacity를 유지했다. 500열·100,000-cell history를 2열로 바꾸면
  새로 생긴 각 행에도 큰 capacity가 남았다. 격리 subprocess의 primary·alternate
  회귀는 peak 증가 **331,976KiB(324.2MiB)**로 128MiB headroom 검사를 실패했다.
  projection·ledger·저장 제목을 더하기 전의 수치이므로 실행별 384MiB 예약의
  적정성을 훼손한다. 실제 Host 3GiB 초과를 측정했다는 뜻은 아니다.
- 수정: shared `Row::shrink`의 split 직후 `shrink_to_fit` 한 줄로 보유 용량을
  반환한다. 중간 폭으로 여러 번 resize하는 우회는 cursor·reflow 의미를 바꾸므로
  사용하지 않는다. crates.io 최신 release도 0.26.0이고 upstream current source에도
  수정이 없어 해당 library source만 `vendor/alacritty_terminal`에 보관한다.
  upstream 대비 source 차이는 이 한 줄뿐이다. Apache-2.0 원문·저자·README와
  버전·commit·checksum 및 제거 조건을 유지한다. 46MiB recording corpus와
  그 reference-test target은 제외하며 library tests는 유지한다.
- 수정 후 같은 격리 회귀 peak 증가 **19,668KiB(19.2MiB)**, 두 화면의 문자와
  history cell 제한을 보존했다. daemon library **290개 통과·1개 ignored**,
  upstream library **132개 통과**, daemon all-target Clippy·제품 build 통과.
- live fixture는 기존 2열 history의 GUI 연결에 더해 500열 full history를
  2열로 축소하고 모든 snapshot chunk에서 마지막 문자가 보존됨을 확인한 후
  실제 GTK/VTE에 다시 연결하여 동일 PID/birth·새 입력·자연 종료를 검사한다.
  최초 fixture assertion은 첫 chunk만 읽어 마지막 문자를 놓쳤다. chunk를 모두
  읽도록 고쳤으며 제품 화면 손실로 분류하지 않는다. 실패 시 추가 resize fixture
  실행도 회수하도록 cleanup을 보강하고 최초 private 실행의 잔여 provider도 정리했다.
  후속 전체 검사는 fixture 실행 수가 한 개 늘어난 데 대한 기존 고정 count assertion에서
  실패했다. `resize_agents` 수를 반영했고, PTY query 응답 뒤에만 history 준비 marker를
  기록하여 전체 출력 처리를 확인한 후 resize하도록 했다. 제품 코드 변경은 없다.
- 최종 실제 Linux 사용자 서비스·격리 GTK/VTE 전체 **34개 통과·exit 0**.
  2열 history의 GUI 연결 peak **21,396→33,356KiB**, 500→2열 축소·GUI 재연결
  peak **33,356→48,760KiB**였다. 두 화면·저장 제목·동시 실행 7개·보관 중
  transfer 6개·일반 IPC 60개와 제어 IPC 16개 부하도 3GiB 예산 내에서 통과했다.
  탭 닫기/재열기·GUI 정상/강제 종료·같은 identity 재연결·새 한글 입력·자연 종료·
  빈 Host 회수도 통과했다. 기존 사용자 GUI `1663486`은 유지되고 세 private
  실행의 Host socket은 모두 제거됐다.
- 로그 `/tmp/fm-runtime-narrow-live-verified.log`, artifacts `/tmp/fm-gui-q8ajfk2f`.
  [검증 기록](design-assets/agent-runtime-narrow-reflow-memory-20261117.json)에
  source·binary·로그 hash를 보관한다. format·Python AST·diff 공백 검사와
  license notice **4개**·기존 license asset 검사도 통과했다.
  **AR-060 해결. AR-006 전체 회계와 macOS containment·안전한 자동 회수는 미완료다.**

### AR-061 · medium · AT-SPI driver가 부모 X 화면의 접근성 버스를 조회

- run `44049485060`의 Linux job `133923394137`은 workspace·앞선 live 검사를
  통과한 뒤 Sessions 버튼 조회에서 실패했다. 업로드된 screenshot은 실행 중인
  GUI를 보여주지만 `session-resume-startup.json`은 `[]`였다. artifact
  `14315287133`을 `/tmp/fm-runtime-ci-44049485060`에 확보했다.
- 원인: fixture는 GUI용 private display/bus를 만들고 Python driver에는 새
  D-Bus만 지정했다. libatspi는 `AT_SPI_BUS_ADDRESS` 다음으로 X root의
  `AT_SPI_BUS`를 읽으며, 없을 때만 D-Bus를 조회한다. 부모 display에 버스
  주소가 있으면 GUI와 driver가 다른 registry에 연결된다. 근거는 upstream
  `GNOME/at-spi2-core/atspi/atspi-misc.c`의 `atspi_get_a11y_bus`, source blob
  `dff12ba6e5475d9bb70cc812eeb88ee4cbd60219`이다.
- 재현: 별도 outer Xvfb/D-Bus에 접근성 버스를 만들고 X root property를 유지한
  X11 환경에서 같은 Sessions timeout·빈 tree를 재현했다. 처음 로컬 조건은
  상속된 `WAYLAND_DISPLAY` 때문에 X 조회를 건너뛰어 통과했다. 이 조건도 제거했다.
  수정 전 로그 `/tmp/fm-runtime-ar056-held-xroot-before.log`, artifacts
  `/tmp/fm-gui-fxueplwa`다. 사용자 display에는 property를 쓰지 않았다.
- 수정: 공용 `Harness.start_display`가 AT-SPI 사용 시 driver의 display와 bus를
  함께 지정한다. driver와 GUI의 상속된 AT-SPI 주소·display 및 Wayland override도
  제거한다. 기존 fixture callers가 같은 경계를 사용하며 제품 동작은 변경하지 않는다.
- 검증: 하네스 단위 검사 **7개 통과**, 같은 outer 접근성 버스를 유지한 실제
  GUI Session Flow **9개 통과·exit 0**. Resume·Fork·Background 재연결·GUI 종료 후
  같은 PID/birth·새 입력·자연 종료·빈 Host 회수를 확인했다. 수정 후 로그
  `/tmp/fm-runtime-ar056-held-xroot-after.log`, artifacts `/tmp/fm-gui-r9h_3zk2`.
  Python AST·diff 공백 검사 통과. **AR-061 해결, AR-056 전체 remote gate는 대기**다.

### AR-062 · medium · 메모리 부하가 에뮬레이터의 숨은 행 캐시를 채우지 않음

- `grid/storage.rs`는 논리 history와 별도로 최대 1,000개의 캐시 행을 둔다.
  기존 primary 400행 출력은 이 행들이 모두 사용된 뒤의 보유 메모리를 검사하지
  못했다. 기존 동시 실행·두 화면·snapshot·IPC 부하는 유지하고 primary 출력을
  1,500행으로 늘려 캐시까지 순환시킨다. 제품 코드 변경은 없다.
- 최초 foreground 실행은 메모리 assertion을 통과한 후 SIGTERM·exit 143으로
  중단됐다. 전체 통과로 기록하지 않았고 private 실행의 Host·provider·GUI를
  정리했다. 이후 분리한 실행 감시 process에서 같은 검사를 끝까지 실행했다.
- 실제 Linux 사용자 서비스·격리 GTK/VTE 전체 **34개 통과·exit 0**, 87.97초.
  7실행·6보관 전송·일반 IPC 60개·제어 IPC 16개에서 Host RSS와 peak는
  **1,424,804KiB (1.36GiB)**로 3GiB 예산 이하였다. 탭/GUI 정상 종료·강제 종료·
  동일 PID/birth 복원·새 한글 입력·자연 종료·빈 Host 회수까지 통과했다.
  사용자 GUI `1663486`은 유지했고 두 private Host socket은 제거됐다.
- 로그 `/tmp/fm-runtime-row-cache-verified.log`, artifacts `/tmp/fm-gui-2h1peg4i`.
  [검증 기록](design-assets/agent-runtime-row-cache-memory-20261117.json)에 부하·
  exit code·binary/script/log hash를 보관했다. Python AST·diff 공백 검사 통과.
  **AR-062 해결. AR-006 전체 allocation 회계·macOS·실제 provider 검증은 남는다.**


### AR-063 · medium · Agents 실행별 자동 종료 보류 사유 누락

- 설계 §5는 안전한 완료/종료 계약이 없는 provider의 실행을 보존하며 이유를
  표시하도록 요구한다. 기존 Options 안내만으로는 어떤 실행이 자동 종료되지
  않는지 Agents에서 확인할 수 없었다.
- managed 실행 행에 `Automatic cleanup paused`를 표시하고 tooltip으로 안전한
  완료·유휴 상태에서만 종료하는 계약이 없으며 자연 종료 또는 명시적 종료까지
  유지됨을 안내한다. 종료 이력에서는 숨긴다. 현재 모든 provider의 계약이
  미지원이므로 새 capability 계층이나 종료 동작을 추가하지 않는다.
- GTK 회귀 **1개 통과**, GUI build·Clippy 경고 0개·format·Python AST·diff
  공백 검사 통과. 실제 격리 GUI `--gui-replacement` **11개 통과·exit 0**이며
  native cross-window 드래그 후 tab 닫기로 Background가 된 같은 실행의 표시를
  AT-SPI SHOWING으로 확인했다. 취소·이동 실패의 기존 입력 보존·정상 이동·
  원래 PID/birth·새 입력·SIGKILL 복원도 통과했다.
- 로그 `/tmp/fm-runtime-retention-live.log`, artifacts `/tmp/fm-gui-4d2h0549`.
  [검증 기록](design-assets/agent-runtime-retention-ui-20261117.json)에 source·binary
  hash를 기록한다. fixture process 잔여와 GTK critical이 없으며, 하네스가
  종료시킨 Host의 private socket 경로는 남지만 연결을 거절한다. 사용자 GUI
  PID `1663486`과 설치본은 유지했다. **AR-063 해결**, AR-010의
  안전한 자동 회수와 macOS의 이번 표시 변경 검증은 여전히 미완료다.


### AR-064 · medium · macOS 전체 gate의 편집기 밝은 테마 대비 실패

- run `44051408083`·job `133929889341`·revision `aa1a26d6`은
  `editor-recovery` 단계에서 `editor status contrast in github-light`의
  `false != true`로 실패했다. 앞선 production GUI crash·Session Flow는
  통과했지만 전체 gate 성공으로 확대하지 않는다.
- 현재 로그는 실패한 색상이나 대비 수치를 제공하지 않으므로 제품 색상 문제와
  관측 오류를 구분할 수 없다. 기존 4.5 기준·실패 처리를 유지하면서 배경색,
  warning/danger의 실제 계산색과 각각의 대비를 assertion 메시지에 기록한다.
  모든 색상을 계산해 첫 실패 뒤의 진단도 남긴다. 제품 palette는 바꾸지 않는다.
- Rust format·구문 검사는 통과했다. 이 호스트는 Linux이므로 WKWebView의
  native 실행은 원격 macOS gate 결과로만 확인한다. **AR-064 미해결**이며
  다음 native 결과의 계산색을 근거로 원인을 수정한 후 같은 단계를 재검증한다.

### AR-006 후속 · 최대 허용 폭과 보관 프레임의 격리 회귀

- `TerminalSize::validate`는 행 300·열 500 각각의 상한에 더해 총 4만 셀을
  제한한다. 300×500 probe는 이 검증에서 거절됐다. 제품이 허용하는 최대 폭의
  80×500으로 교정하여 기존 100×400 부하와 다른 행 cache 폭을 확인했다.
- primary에 1,500행, alternate에 80행의 셀별 속성·4-byte 결합 문자 32개를
  입력했다. 최초 화면과 두 번의 새 출력 뒤 snapshot Arc 세 개를 동시에
  보관하고, 각 화면의 텍스트가 유지되는지 확인했다. 격리 subprocess의 VmHWM은
  **186,156KiB**, 최종 frame은 **7,252,152 bytes**, 검사 **1개 통과·exit 0**다.
- 제품 변경은 없고 단위 회귀만 추가했다. 이 peak는 한 terminal 부하의
  측정이며 Host ledger·IPC·저장 admission·모든 allocator peak를 포함하지 않는다.
  따라서 AR-006은 열린 상태다. 로그와 source hash는 위 검증 기록에 남겼다.

### AR-008 후속 · macOS containment 대안의 확인된 제한

- Apple의 [launchd.plist 문서](https://github.com/apple-oss-distributions/launchd/blob/d448a1c8f70a61202f8705f94337f686b87c30c4/man/launchd.plist.5)는
  `AbandonProcessGroup`의 기본 정리가 job과 **같은 process group ID**를 대상으로
  한다고 명시한다. 실행별 LaunchAgent를 만드는 것만으로 `setsid`·double fork
  자식의 소유권과 종료가 보장된다고 해석하지 않는다.
- XNU의 [coalition syscall](https://github.com/apple-oss-distributions/xnu/blob/f6217f891ac0bb64f3d375211650a4c1ff8ca1ea/bsd/kern/sys_coalition.c#L220)은
  create·terminate·reap 전에 `task_is_in_privileged_coalition`을 요구하며 아니면
  `EPERM`을 반환한다. 같은 소스의 terminate 설명은 이미 소속된 process의 fork를
  계속 허용한다고 명시한다. 따라서 이름만 보고 `coalition_terminate`를 실행별
  kill이나 fork 차단의 대체로 사용하지 않는다.
- 이 확인은 공개 소스의 제한이며 현재 native runner에서 권한·동작을 검증한
  결과가 아니다. 새로운 containment 구현은 추가하지 않았고 AR-008은 열린 상태다.

### AR-056 완료 · Linux 전체 원격 gate 통과

- run `44054768870`·job `133941183853`·revision `92e4e334`의 Linux extended gate가
  **CI_GATE_EXIT=0**으로 완료됐다. stress·editor web도 성공했다. regression phase는
  **2,442.845초·exit 0**이며 Sessions·native drag·GUI replacement·tab detach를 포함한
  전체 phase 결과를 위 검증 기록에 보관한다.
- 이전 실패 이력은 유지한다. 이 revision 이후 AR-063 UI는 별도 격리 live로
  검증했으며, 원격 성공을 이후 변경이나 실제 provider/model 검증으로 확대하지 않는다.

### AR-064 후속 · 실제 표시되는 상태 CSS 측정

- run `44056791302`·revision `96c72b22`의 native 진단에서 `github-light` 배경은
  `color(srgb 0.931294 0.932863 0.934745)`, 두 상태색은 모두 `rgb(171, 178, 191)`로
  관측됐다. 각 대비는 `1.83366`이다. palette를 바꾸거나 기준을 낮추지 않았다.
- 기존 표본은 전체 높이 editor 뒤에 붙인 빈 inline span이었다. 새 검사는 열린
  recovery dialog 안에 텍스트와 실제 `.document-state`·`.is-conflict` class를 가진
  표본 두 개를 배치하고 layout 크기를 확인한다. dialog 배경은 표본 배경과 같은
  panel이므로 투명한 상태 배경의 아래 레이어까지 반영한다. root 변수와 실제
  Discard Recovery 버튼의 계산색도 진단에 남긴다.
- Linux WebKitGTK에서 새 JS·제품 CSS를 실행하면 두 표본의 layout이 존재하고
  warning 대비 `5.4950`, danger 대비 `5.6556`, 실제 discard 버튼도 같은 danger
  색상으로 확인됐다. format·diff 검사는 통과했다. macOS의 기존 관측 오류 여부는
  아직 확정하지 않았으며 **AR-064 미해결**로 native 결과를 기다린다.

### AR-006 후속 · 최대 폭의 제목·링크·두 화면 snapshot 동시 보관

- 최대 폭 검사의 기존 alternate snapshot 세 개에 primary snapshot을 더했다.
  primary는 **16,761,452 bytes**, alternate는 **7,252,152 bytes**다. 두 grid와
  1,000행 cache를 채우고 4KiB 제목 4,096개·긴 유효 링크 500개 입력도 겹쳤다.
  링크 500개 모두의 보관은 요구하지 않으며 기존 링크 admission 제한을 유지한다.
- 격리 검사 **1개 통과·18.60초·exit 0**, VmHWM **245,508KiB**로 terminal의
  부하는 기존 실행 예약 393,216KiB 안에 있었다. daemon all-target Clippy 경고
  0개·format·diff 공백 검사 통과. 제품 코드는 바꾸지 않았다.
- 이는 빠졌던 조합의 peak 측정이며 actor ledger·IPC·state admission과 여러 실행을
  합친 전체 Host allocation 증명이 아니다. **AR-006 미해결** 상태는 유지한다.

- 같은 최대 폭 검사를 1×500에서 80×500으로 높인 뒤 두 화면에 각각 1,500행을
  입력하도록 보강했다. 높이 증가로 alternate grid에도 생긴 cache를 채우기 위한
  조건이다. **1개 통과·30.47초·exit 0**, VmHWM **329,588KiB**로 예약 안이었다.
  원래 측정과 후속 source/log hash를 위 조합 검증 기록에 함께 보관한다.

### AR-065 · medium · history 없는 화면의 resize cache 메모리 초과

- 허용된 79×499에서 history를 채운 뒤 79×500→80×500으로 바꾸면 history가
  없는 alternate grid에도 비표시 행 cache가 생겼다. 양쪽 화면에 셀별 속성과
  4-byte 결합 문자 32개를 가진 1,500행, 저장 제목 4,096개, 긴 링크 500개를
  입력하고 primary snapshot 세 개를 겹치면 terminal 단독 peak가
  **410,768KiB**, 회귀 **exit 101**로 실행 예약을 초과했다.
- 공유 `Grid::resize`에서 `max_scroll_limit == 0`이면 기존 storage의 `truncate`를
  호출한다. 현재 보이는 행은 유지하고 비표시 cache의 셀 payload를 해제한다.
  history가 있는 primary의 scrollback은 유지한다. 같은 회귀는
  **301,064KiB·28.78초·1개 통과·exit 0**으로 바뀌었다.
- daemon library **291개 통과·1개 ignored**, upstream library **132개 통과**,
  daemon all-target Clippy `-D warnings`, locked GUI·CLI·daemon build, format과
  diff 공백 검사를 통과했다. 이 수정으로 전체 Host allocation의 상한이 증명된
  것은 아니며 **AR-006은 미해결**로 유지한다.
- 새 `scripts/test-agent-terminal-budget.py`는 기존 격리 Harness·HostClient를
  재사용한다. 같은 크기 변경과 부하 뒤 active/pending frame을 겹쳐 유지한
  실제 Host peak는 **339,464KiB**였다. Relay를 해제하고 실제 GUI에서 원래
  PID/birth의 실행에 연결해 새 한글 입력의 ACK, 단일 spawn, 자연 종료와 빈 Host
  socket 제거까지 **3개 통과·exit 0**이다. 사용자 GUI PID `1663486`은 유지했다.
  로그 `/tmp/fm-runtime-terminal-budget-live-verified.log`, artifacts
  `/tmp/fm-gui-9l1a3buy`와 source·binary hash를 검증 기록에 보관한다.
- 부하 중 GUI management 재연결 경고가 있었으며 이 검사는 부하 중 응답시간의
  상한을 검증하지 않는다. 초기 하네스의 heartbeat timeout·chunk 내용 위치·빈
  workspace 전제 오류는 수정했으며, 해당 실패를 제품 회귀 통과로 세지 않는다.

### AR-064 완료 · macOS 전체 gate와 표시 상태 대비 통과

- run `44061947951`·job `133965800117`·revision `0d6ee75d`의 전체 macOS gate가
  **CI_GATE_EXIT=0**으로 완료됐다. `editor-recovery`는 **5.233초·exit 0**이며
  `MACOS_EDITOR_STATUS_CONTRAST_OK`를 남겼다. 실제 표시되는 상태 CSS의 layout과
  warning/danger 대비 **4.5** 기준을 유지하고 제품 palette 변경 없이 통과했다.
- native GUI crash 생존·원래 실행 재연결, Sessions Flow와 나머지 gate도 통과했다.
  성공 로그는 개별 계산색 수치를 출력하지 않으므로 Linux의 색상 측정값을 macOS
  측정으로 대신 쓰지 않는다. 이전 빈 span 관측의 WebKit 내부 원인까지 증명한
  결과는 아니다. **AR-064 해결**, AR-006·008 macOS·010은 미해결이다.
- artifact `14318819838`과 phase·로그 hash를 위 원격 기록에 보관한다. 이 run은
  AR-065 이전이므로 최신 resize-cache 수정의 macOS 검증으로 확대하지 않는다.

### AR-066 · medium · 대량 PTY 출력 중 제어 소켓 응답 지연

- AR-065의 bulk output 중 GUI management 재연결 경고를 별도로 측정했다.
  기존 Host는 같은 부하의 KeepAlive 응답에 최대 **20.653886초**가 걸렸다.
  GUI 관리 RPC의 제한은 **1초**이며, 출력이 계속 준비된 AsyncFd 읽기 경로는
  각 16KiB read가 끝나도 executor에 양보함을 보장하지 않았다.
- `TerminalActor::run`의 매 순환 끝에 `tokio::task::yield_now().await`를 둔다.
  기존 read 크기·입력/응답 큐·종료와 lease 처리는 유지한다. timeout을 늘리거나
  대량 출력을 버리는 방식으로 대응하지 않는다.
- 같은 격리 Host·GUI 부하에서 **20회** heartbeat를 관측했다. 회귀가 측정하는
  전체 call 시간은 최대 **0.127987초**로 GUI의 1초 제한을 통과했다. 보조 wrapper의
  RPC 내부 측정은 최대 0.009467초였으며 더 큰 전체 call 시간을 판정에 사용한다.
  메모리 peak **337,616KiB**, 원래 PID/birth의 GUI 연결·새 한글 입력·단일 spawn·
  자연 종료와 빈 Host 회수 **3개 통과·exit 0**이다. 사용자 GUI는 유지했다.
- daemon library **291개 통과·1개 ignored**, daemon all-target Clippy·locked
  GUI/CLI/daemon build·format·diff 공백 검사도 통과했다. 실제 live script의
  heartbeat 지연을 1초 미만으로 assert하고 Linux 공통 테스트에 포함한다.
  전체 Host 회계·모든 실행/플랫폼의 공정성 상한을 증명한 것으로 확대하지 않는다.

### AR-006 후속 · geometry 변경의 전체 diff와 보관 snapshot

- 기존 A/B 문자 갱신은 작은 diff만 만들었다. 보관 primary snapshot 사이에
  유효한 80×500→79×500→80×500 변경을 넣어 전체 snapshot을 만드는 diff 경로와
  임시 복사본이 겹치도록 한다. unit에서 두 diff가 reset을 포함한 전체 frame임을
  assert하고, 실제 Host에서도 active/pending 전송 사이에 같은 resize를 수행한다.
- unit **1개 통과·exit 0**, peak **328,628KiB**. 실제 격리 Host·GUI는 **3개
  통과·exit 0**, peak **392,576KiB**, bulk 제어 응답 최대 **0.000691초**다.
  원래 PID/birth와 단일 spawn·새 한글 입력·자연 종료·빈 Host 회수도 통과했다.
- 제품 수정 없이 회귀만 보강했다. 384MiB 예약까지 남은 관측 여유는 **640KiB**로,
  이 fixture가 모든 allocation과 동시 실행의 상한을 보장한다고 해석하지 않는다.
  AR-006·008 macOS·010은 유지하며 사용자 GUI와 설치본은 변경하지 않았다.

### AR-006 후속 · 활성 lifecycle 문구와 종료 이력의 조회 복제

- 종료 이력에도 큰 상태 문구를 남기는 초기 가설은 실제 도달 가능한 상태가
  아니었다. `TerminalActor::finished`는 `entry.agent.presence = None`을 수행한다.
  그 불변식을 무시한 fixture의 504,140KiB는 제품 결함 재현으로 사용하지 않는다.
  임시 표시 문구 제한은 제거했으며 이 가설은 결함으로 등록하지 않는다.
- 새 회귀는 실제 한 실행에 60KB 완료 event를 수락한 뒤 종료시켜 `presence`
  해제를 확인한다. 그 종료 entry로 64개의 종료 metadata를 구성하고 실제 7개
  활성 실행의 24KB 상태·36KB 메시지를 유지한 채 60개의 느린 조회를 겹친다.
- **1개 통과·exit 0**, 목록 wire **551,474 bytes**, exact test process peak
  **94,024KiB**다. 활성 원문과 Completed 판정은 유지하며 daemon all-target
  Clippy·format도 통과했다. 제품 코드는 변경하지 않았다.
- 이 검사는 최대 VT·대형 수신 버퍼·저장 파일 역직렬화와 합친 검증이 아니므로
  전체 회계인 AR-006을 닫지 않는다. 자세한 범위와 로그 hash는 위 기록에 있다.

### AR-067 · medium · GUI 전달 응답의 무제한 수신

- 원인: `DispatchContext`는 실행별 gate로 동시 전달을 최대 7개로 제한하지만,
  GUI client의 `read_until`은 응답 바이트 상한 없이 `Vec`을 키운다.
  browser eval은 임의 길이 문자열을 반환할 수 있어 실행 예약과 별개로 Host
  메모리를 늘릴 수 있었다. 일반 IPC의 1MiB 요청 상한은 응답을 제한하지 않는다.
- 실제 격리 Host·GUI의 managed provider에서 5MiB 문자열을 반환하는 eval을
  실행했다. 수정 전 stdout **5,242,912 chars·exit 0**으로 전달됐으며 같은
  eval의 전역 counter는 1이다. 이는 도달 가능한 경로의 재현이며 Host 전체
  3GiB 초과나 OOM을 관측했다는 뜻은 아니다.
- 조치: Host의 GUI 전달 client에만 envelope 포함 **4MiB wire 상한**을 둔다.
  최대 7개 전달의 수신 wire를 약 28MiB로 제한하며 실제 allocator capacity와
  파싱·재직렬화 복사본은 별도다. bounded reader는 기존 지속 버퍼와 취소 시의
  부분 응답을 보존하고, 초과한 연결은 재사용을 거절한다. 일반 client와 terminal
  frame 전송의 크기 계약은 유지한다. 큰 eval은 작은 결과나 요약을 반환해야 한다.
- 검증: IPC **96개**, daemon **292개·1개 ignored**, 두 crate의 all-target
  Clippy `-D warnings`, GUI·CLI·daemon locked build 통과. 무종결 응답의 중단,
  경계값·기존 대형 응답 허용·취소 후 읽기·거절 연결 재사용 방지를 검사했다.
  실제 격리 live **3개·exit 0**으로 작은 한글 응답, 5MiB 응답의 명확한 오류,
  효과 1회·재시도 없음, 같은 PID/birth와 새 한글 입력, 자연 종료·빈 Host 회수를
  확인했다. `scripts/test-agent-gui-response-limit.py`를 Linux 공통 gate에 넣었다.
- CI 하네스의 phase 수 기대값 두 subtest는 새 phase 추가 전 값으로 실패했다.
  49/47로 조정한 뒤 해당 검사(regression·extended) 재실행은 통과했다.
  사용자 GUI PID `1663486`과 설치본은 유지했다. 원격 Linux/macOS 전체 gate의
  이전 제품 결과는 이 수정의 native 검증을 대신하지 않는다. **AR-067 해결**이며
  전체 회계·macOS containment·provider 안전 종료의 미해결 medium 3개는 유지한다.

### AR-068 · medium · 기본 dark 에디터 상태 대비와 theme 적용 대기 누락

- 관측: `55a7da31`의 macOS job `133982184891`은 `editor-recovery`에서
  `one-dark` 오류색 대비 **4.451547**로 실패했다. 이전 AR-064 성공 run을
  이번 제품의 성공으로 대신하지 않는다. 실패 artifact는 `14322286958`이다.
- 기본 theme의 `#282c34`·`#ffffff`와 제품 CSS를 격리 Flowmux 브라우저에
  적용하자 같은 background와 **4.451547**을 재현했다. One Dark 자체의
  `#abb2bf` foreground를 적용하면 **4.885735**다. native 검사는 이미 dark인
  상태에서 `colorScheme === 'dark'`만 기다려 queued theme 적용 전에 측정할 수
  있었다. 이 경합과 별개로 실제 기본 테마의 오류색 대비도 부족했다.
- 조치: 정적 CSS와 동적 theme 적용의 dark 오류색을 `#f09999`로 밝히고
  배포 bundle도 재빌드했다. native 검사는 대상 `--ink`·`--text` 적용을 기다리고
  기본·One Dark·GitHub Light를 검사한다. 4.5 기준은 유지하고 진단에 root 색을 남긴다.
- 검증: 실제 격리 Flowmux WebKit에서 제품 stylesheet와 대상 변수로 측정한
  오류색 대비는 기본 **5.050302**, One Dark **5.542890**, GitHub Light
  **5.655556**이다. warning도 모두 4.5 이상이다. 이 CSS fixture는 Monaco 문서
  복구나 macOS native 실행을 대신하지 않는다. web **41개**, TypeScript check,
  자산 rebuild·12개 verify, locked GUI build, format·diff 검사를 통과했다.
- **미해결·native 재검증 대기.** Linux에서는 macOS WKWebView/main-thread
  recovery harness를 실행할 수 없다. `a5275f8f`의 queued run `44070754736`은
  이번 수정 전이므로 취소했고, 두 수정을 포함한 최신 source의 후속 전체 run으로
  확인한다. 사용자 GUI와 설치본은 유지했다. 미해결 medium은 4개다.

### AR-069 · medium · Session Flow의 조작 대상 식별과 포커스 관측

- Linux run `44066722901`·job `133982184828`은 fork view 종료 직후 child
  선택으로 실패했다. 기존 검사는 Fit 기준 추정 좌표를 클릭하고 100ms 뒤
  label을 조회해 canvas 포커스나 키 처리 여부를 구분하지 않았다.
- 같은 제품의 로컬 기존 검사는 9개 통과했다. 접근성 snapshot에는 canvas의
  이름·설명·선택 label이 없었고, 새 회귀를 수정 전 제품으로 실행하면
  `accessible Flow canvas` 조회가 timeout으로 실패했다. 이 결과만으로 이전
  CI에서 어떤 위젯이 키를 받았는지 단정하지 않는다.
- canvas에 Group 역할·이름·고정 조작 설명을 제공한다. 검사는 Fit부터 native
  Tab으로 canvas까지 이동하며 각 대상의 FOCUSED를 확인하고, Down 이후 실제
  canvas label 변경을 기다린 뒤 선택한 child만 Enter로 연다. 실패 시 접근성
  tree와 screenshot을 보관한다. 기존 상태 label을 키 처리 영수증으로 쓰지 않는다.
- 수정 후 별도 XDG·D-Bus·Xvfb의 live 9개·exit 0으로 Background의 동일
  process 재연결·새 입력, ended resume, GUI 종료·복원, 자연 종료와 빈 Host
  회수를 통과했다. 격리 GTK canvas 회귀 1개·locked GUI build·all-target Clippy·format도 통과했다.
  [검증 기록](design-assets/agent-runtime-flow-keyboard-focus-20261117.json)에 근거를
  보관한다. 이전 원격 run `44071483159`는 이 수정 전 revision으로 취소했고,
  수정 포함 revision `f8f149d1`의 run `44073400067`에서 재검증 중이다.
  사용자 GUI·설치본은 유지했고 medium 5개는 열려 있다.

### AR-010 후속 · 설치된 Codex 프로토콜의 안전 종료 계약 확인

- 설치된 `codex-cli 0.169.0`에서 experimental JSON schema를 생성했다. 모델
  요청·기존 app-server 연결·sign-out·hook trust 변경은 수행하지 않았다.
  184개 request method 중 `thread/backgroundTerminals/list`가 존재하며 조회
  인자는 thread ID와 pagination이다. background terminal 조회가 불가능하다고
  단정하지 않는다.
- `thread/unsubscribe`·`thread/archive`는 thread ID만, `turn/interrupt`는 thread와
  turn ID를 받는다. 확인한 schema는 전체 작업 완료나 새 작업 수락과 원자적인
  idle-only process 종료를 보증하지 않는다. 목록 조회 뒤 unsubscribe 또는
  signal을 보내는 순서만으로 그 계약이 성립하지 않는다.
- 이 결과는 schema inspection이며 undocumented server 동작의 부재를 증명하지
  않는다. 현재 PTY 실행을 app-server adapter로 바꾸거나 자동 종료를 켜는
  근거로 사용하지 않는다. [계약 점검 기록](design-assets/agent-runtime-provider-contract-20261117.json)에
  version·선택한 인자·schema hash·한계를 기록한다. AR-010은 계속 미해결이다.

### AR-070 · medium · 완료된 GUI 응답이 느린 caller마다 누적

- `DispatchContext`의 attachment gate는 GUI 응답을 받은 뒤 해제된다. 바깥 IPC
  server의 직렬화·socket 전송은 그 뒤이므로 한 실행에서도 큰 응답을 동시에
  보유할 수 있었다. 각 wire 응답의 4MiB 제한과 실행 수 7개만으로 이 수를
  제한할 수 없다. 실제 managed provider가 56개 연결의 응답을 읽지 않자 모두
  수락됐고 GUI 효과도 56회, Host peak는 **529,756KiB**였다.
- IPC server에 별도 7개 응답 예약을 둔다. dispatch 전에 즉시 예약하고 결과
  직렬화와 socket write의 성공·오류·취소까지 유지한다. 포화 시 queue 없이
  `request not started` Busy를 반환한다. hook·제어·일반 GUI 요청은 이 예약을
  사용하지 않으며 기존 per-response wire 제한과 no-replay 계약을 유지한다.
- 같은 격리 GTK/WebKit 시나리오는 **5개 통과·exit 0**. 수락 7개·사전 거절
  49개·실제 GUI 효과 7회와 Host peak **107,660KiB**를 확인했다. 포화 중 목록
  조회는 1초 미만이며, 느린 연결을 닫으면 새 forwarding이 성공한다. 같은
  PID/birth와 새 한글 입력, 자연 종료와 빈 Host 회수도 확인했다. 기존 Linux
  gate의 response-limit fixture를 확장하므로 phase를 추가하지 않는다.
- IPC 단위 **96개**, IPC/daemon all-target Clippy `-D warnings`, matching 세
  binary build·format·외부/provider Python AST·공백 검사가 통과했다.
  [검증 기록](design-assets/agent-runtime-slow-forwarded-responses-20261117.json)에
  source와 제품·로그 hash 및 세 격리 실행의 process 정리를 남긴다.
- 첫 수정 후 검사는 response 부하 전에 초기 Agent view admission에서 실패했다.
  정확한 relay 원인은 확보하지 못했다. 세 matching binary를 모두 빌드한 최종
  실행은 통과했으나 이를 이전 실패의 원인 확정으로 해석하지 않는다. 이후 실패
  시 terminal 화면을 저장하도록 했다. 이전 실패는 성공 합계에 넣지 않는다.
- **AR-070 해결**, AR-006의 전체 allocation 회계는 미완료다. 이 로컬 결과는
  실제 provider/model·macOS·Flatpak이나 전체 원격 gate의 통과를 뜻하지 않는다.
  사용자 앱 PID `1663486`과 설치본은 유지했다.

### AR-072 — 최신 main 리베이스 통합

작업 원본 `51d419b5f8c82f7f7ae0a5a9235399ec12ebeaf7`을
`backup/runtime-audit-pre-rebase-20261117`에 보존하고, 121개 커밋을
`origin/main`의 `82554c97e3fc98645fc77cacb3eb65d3742bb1cf` 위로 옮겼다.
main의 등록된 detached workspace·일반 탭 이동과 호출 출처 검증을 유지하고,
managed Agent의 별도 창 takeover·Host hook 경로를 통합했다. 분리 창의 relay
종료 판정은 현재 registry pane의 managed binding을 조회한다.

통합 빌드에서 Session Flow operation kind가 제거된 `fork` 변수를 참조하는
오류를 확인했다. launch와 동일한 `fork_source.is_some()` 조건으로 수정하고,
main의 fork에만 YOLO를 적용하는 정책을 유지했다. CI 목록은 main의
`tab-windows`와 Agent 검사를 모두 포함하며 helper build는 workspace test 전에
한 번 수행한다. macOS 대비 검사는 main의 canvas readback 없는 색상 계산을
유지하고, 기본 테마·실제 배경/글자색 적용 대기를 보존했다.

`cargo build --workspace --locked` 통과. 사용자 GUI PID `1663486`은 재시작하지
않았다. 리베이스 후 live·정적 검증 결과는 후속 기록으로 남긴다.

### 실제 Codex TUI — 리베이스 후 연결·종료·복원

선택 실행용 `scripts/test-agent-codex-live.py`를 추가했다. 설치된 Codex와 기존
로그인을 사용하므로 일반 CI에는 포함하지 않는다. 격리 display·D-Bus·XDG·Host에서
`codex --no-daemon --sandbox read-only`의 실제 모델 응답을 확인하고, 탭과 마지막
격리 GUI를 닫은 뒤 같은 PID/birth로 새 GUI에 재연결한다. 이전 화면 복원에 더해
새 nonce·한글 응답을 확인하며, 마지막에는 이 실행만 명시적으로 종료한다.
사용자의 provider 신뢰·승인 설정은 바꾸지 않는다.

리베이스 후 source `9994da97`의 바이너리에서 4개 검사 통과. 실패 시에만 화면을
추가 출력하는 진단 wrapper로 스크립트의 main을 호출했으며, 통과 경로에 수동
입력이나 재시도는 없었다. [검증 기록](design-assets/agent-runtime-real-codex-reconnect-20261117.json)에
binary hash와 artifact 위치를 남겼다. 앞선 연결 실패도 별도로 보존하며, 이
통과를 부하 안정성·31분 실제 작업·자동 회수·다른 플랫폼 검증으로 확장하지 않는다.

### AR-073 — 종료·launch 실패의 증거 보존 (미해결 medium)

리베이스 전 Linux run `44073400067`은 `managed-session-flow`를 통과했으나
`agent-termination`의 두 번째 정상 종료 요청 뒤 `Stopping registry` 대기에서
실패했다. 이 테스트의 실제 후반 GUI는 `session-resume-reopened.log`를 사용하지만
CI는 최초 GUI 로그만 보존했다. 후반 GUI와 Host 로그도 업로드하도록 수정했다.
리베이스 후 로컬 종료 검사는 정상·대기·강제 종료를 포함해 통과했으며, 원인을
확정하거나 원격 실패가 해결됐다고 선언하지 않는다.

같은 로컬 검증 중 높은 병렬도에서 daemon의 synchronized-output 테스트 launch가
`Failed`를 반환했고 managed relay 연결 실패도 관측했다. daemon·state의
`--test-threads=4` 재검사는 각각 294개·100개 통과했으며, 실제 Codex와 managed
tear-off의 후속 검사도 통과했다. 최초 실패를 삭제하거나 재검사로 대체하지 않는다.
launch receipt의 error와 CLI stdout/stderr를 실패 assertion에 포함해 다음 발생의
실제 원인을 보존한다. 완료 조건은 실패 원인 특정과 해당 조건의 재현·수정·검증이다.

### AR-074 — main의 Session Flow fork 정책과 fixture 정합

main은 fork 실행에 `--allow-dangerously-skip-permissions --permission-mode
bypassPermissions`를 추가한다. 리베이스 후 제품은 이 정책을 유지했지만 fixture는
이전 argv를 기대해 실패했다. fork 기대값만 main 정책과 맞췄으며 일반 resume의
argv 검사는 유지했다. 격리 live 검사 9개가 통과했다
(`/tmp/fm-runtime-rebase-session-flow-aligned.log`, `/tmp/fm-gui-x8oeoc12`).
호출 횟수·같은 PID 재연결·GUI 종료 후 복원·자연 종료와 빈 Host 회수를 포함한다.

원격 run `44077266019`는 DMG phase를 통과한 뒤 macOS editor-recovery의
`github-light` 대비 검사에서 실패했다. 이 run은 리베이스 전 source이며 현재
main의 canvas 없는 새 rendered sample 검사와 다르다. 이 결과만으로 현재
제품 색상이나 현재 검사가 수정됐다고 단정하지 않고 AR-068을 열어 둔다.

### AR-075 — 일반 창 드롭 fixture가 동일 이름의 탭을 선택

리베이스 후 직렬 live 검사에서 workspace 행 대신 tab bar의 `(334, 62)`로
드롭했다. `ROW_TARGET`은 workspace와 활성 pane tab 모두에 표시되므로 첫 label
조회만으로 side panel을 식별할 수 없었다. 실제 결과도 목표 workspace가 아닌
현재 표시된 workspace에 탭을 넣었다. label의 `LIST_ITEM` 조상을 확인하고
새 workspace 생성·Escape 후 좌표를 다시 조회하도록 fixture를 수정했다.

수정 후 실제 행 좌표 `(142, 90)`를 사용해 14개 native 검사와
`TAB_WINDOWS_GUI_OK`·exit 0을 확인했다
(`/tmp/fm-runtime-rebase-tabs-row.log`, `/tmp/fm-gui-z9i_zode`).
workspace 행 hover·Escape·명시한 목적지 drop, 창 사이 body·split·tab-bar 이동,
rename 후 drag, 메뉴 이동, foreign-process 거절과 동일 shell PID를 포함한다.
앞선 실패 로그 `/tmp/fm-runtime-rebase-tabs-final.log`도 보존한다. 제품 코드의
변경 없이 잘못된 검사 대상을 수정한 결과이며, Wayland나 macOS 통과를 뜻하지 않는다.

### 리베이스 후 검증 정리 — 2026-11-17

base `82554c97`와 production build `9994da97`에서 locked workspace build와
all-target Clippy `-D warnings`가 통과했다. 이후 변경은 fixture·진단·문서이며
최종 format·Python AST·공백 검사도 통과했다. daemon·state의 제한 병렬도
재검사는 각각 294개(ignored 1개)·100개 통과했다. 최초 높은 병렬도 launch
실패는 AR-073에 남아 있으며 전체 workspace test 통과로 보고하지 않는다.

격리 live 검사는 실제 Codex 4개, 명시적 종료 14개, Session Flow 9개, managed
창 분리 9개, 일반 창 이동 14개와 managed Agent drag 24개가 통과했다. 로그별 개수와
hash·사용한 binary hash는 [검증 기록](design-assets/agent-runtime-rebase-checks-20261117.json)에
보관한다. 사용자 GUI PID `1663486`은 유지했으며 설치본을 교체하지 않았다.
이 결과는 원격 전체 gate·macOS·Wayland·Flatpak·실제 31분 workload를 검증하지
않는다. 미해결 표의 medium 6건은 계속 열려 있으며 계획 전체 완료가 아니다.

### AR-076 — 분리 창의 Agents 갱신과 표시 상태 누락 (medium → 수정)

- 재현: 일반 terminal 탭을 native drag로 분리한 뒤 그 창에 managed 실행을
  열었다. 메인 창에만 `Drag running agent to a pane` 행이 나타났고 분리 창에는
  없었다 (`/tmp/fm-detached-agent-before.log`, `/tmp/fm-gui-qoih_2y1/detached-repro.json`).
- 원인: 같은 controller가 소유한 분리 창의 sidebar를 activity 갱신에서 누락했고,
  Host로 보내는 view visibility도 항상 메인 창과 메인 stack을 조회했다.
- 조치: 창별 sidebar를 한 번씩 갱신하고 기존 `workspace_window`와
  `visible_workspace_in_window`로 실제 소속 창의 mapped/minimized/active 상태를
  사용한다. 실행 identity·lease·저장 ownership은 변경하지 않는다.
- 회귀 검사: `test-tab-windows-gui.py --managed`가 별도 Host와 두 창을 사용해
  두 Agents 행, 분리 창의 Visible/Open/Visible 전이와 동일 PID를 검사한다.
  Linux 공유 gate에도 이 옵션을 연결했다. 사용자 GUI는 종료·교체하지 않는다.
- 검증 결과: locked GUI build 후 격리 native 검사 15개·`TAB_WINDOWS_GUI_OK`·exit 0
  (`/tmp/fm-detached-agent-after.log`, `/tmp/fm-gui-989mr7wl`). 기존 일반 창 이동
  14개와 새 managed 검사를 포함한다. Wayland·macOS 결과로 확대하지 않는다.

### AR-077 — 리베이스 후 Linux gate 검사 수 기대값 누락 (low → 수정)

`test-ci-runner.py` 전체 27개 검사에서 regression/extended의 phase 수만 각각
48 대 47, 50 대 49로 실패했다 (`/tmp/fm-detached-ci-runner.log`). 공유 gate와
필수 script 집합에는 `tab-windows`가 포함됐지만 총개수 기대값은 이전 값이었다.
기대값을 48/50으로 맞췄고 두 suite를 검사하는 해당 test가 통과했다
(`/tmp/fm-detached-ci-count-retest.log`, exit 0). 전체 검사에서는 27개 test 중 이 phase 수 subtest 2개만 실패하고 1개가
skip됐다. 이번 표적 재검사가 전체 CI 통과를 뜻하지 않는다.

### AR-078 — 분리 창의 Agent 종료 확인이 메인 창에 열림 (medium → 수정)

- 재현: AR-076 수정 뒤 분리 창의 `Agent actions → Terminate agent…`를 눌렀다.
  확인창의 AT-SPI 최상위 창은 `flowmux - sh` 대신 메인 창이었다
  (`/tmp/fm-detached-confirm-before2.log`, `/tmp/fm-gui-teg06atl/confirmation-repro.json`).
  최초 재현 fixture는 같은 이름의 label을 눌러 실패했으므로 toggle-button으로
  대상을 한정한 뒤 실제 확인창의 소속을 비교했다.
- 원인: 제어 명령에 호출 창이 없었고 confirmation과 오류 처리에 controller의
  메인 창을 고정 사용했다.
- 조치: 메뉴 button의 실제 root 창을 `SendWeakRef`로 보내 GTK thread에서만
  upgrade한다. 닫힌 호출 창은 무시하고 confirmation·native browser suspend·
  오류 dialog에 같은 창을 사용한다. 종료 확인과 명시적 동의 조건은 유지한다.
- 회귀 검사: registry row unit 검사가 전달 창 identity를 확인한다.
  격리 native 시나리오도 분리 창 메뉴에서 확인창의 실제 소속을 비교하고
  Cancel 후 같은 실행·PID가 Running인지 확인한다.
- 검증 결과: locked workspace build·all-target Clippy `-D warnings`·registry row
  unit 1개가 통과했다. Clippy 최초 실행은 테스트의 Adw trait import 누락으로
  실패했고 import 후 재검사했다. 격리 native 검사 16개와 `TAB_WINDOWS_GUI_OK`·
  exit 0도 확인했다 (`/tmp/fm-detached-control-after.log`, `/tmp/fm-gui-397_ndkj`).

### AR-079 — Agent 목록 클릭이 메인 창을 다시 띄움 (medium → 수정)

- 관측: 메인 창에서 실행 행을 누르면 분리 창의 같은 managed 탭은 Visible이
  됐지만 bare Xvfb의 input focus는 메인 창에 남았다
  (`/tmp/fm-detached-focus-before.log`, `/tmp/fm-gui-7a0xlb5j/focus-repro.json`).
  수정 후에도 input-focus 검사는 실패했다 (`/tmp/fm-detached-focus-after.log`).
  이 fixture에는 focus 요청을 처리하는 window manager가 없으므로 최초 관측만으로
  실제 desktop의 포커스 버그를 증명했다고 해석하지 않는다.
- 원인: `activate_workspace_at`가 실제 분리 창을 활성화한 뒤 managed·일반 activity
  열기 경로가 모두 `self.window.present()`로 메인 창을 다시 띄웠다.
- 조치: 두 경로의 마지막 present를 기존 `workspace_window(workspace)` 조회로
  바꾼다. 목적지·surface 검증과 동일 실행 재사용은 유지한다.
- 회귀 검사: 메인 창의 실행 행을 누른 뒤 분리 창의 live tab·동일 PID·X11
  stacking order를 확인한다. 실제 window manager/compositor의 input-focus 수락은
  이 fixture의 검증 범위가 아니다.
- 검증 결과: locked workspace build·all-target Clippy와 bare Xvfb native 검사
  17개·`TAB_WINDOWS_GUI_OK`·exit 0가 통과했다
  (`/tmp/fm-detached-presentation-after.log`, `/tmp/fm-gui-w3z6luj3`).
  추가로 임시 Openbox를 별도 Xvfb에서 실행해 main 행 클릭 후 실제
  `_NET_ACTIVE_WINDOW`가 분리 창이고 input focus의 부모 체인에 그 창이
  포함되는지 확인했다 (`/tmp/fm-detached-focus-openbox-final.log`,
  `/tmp/fm-gui-n7uqz59w/focus-repro.json`). GTK의 focus child를 top-level과
  직접 비교한 앞선 임시 검사는 잘못된 assertion이었으며, 실제 focus의
  ancestry로 바로잡았다. Openbox 패키지는 `/tmp`에만 풀었고 설치본과 사용자
  GUI를 변경하지 않았다. Wayland·다른 compositor·macOS 검증은 남아 있다.

### AR-080 — 긴 GUI 실패 뒤 cleanup이 원래 오류를 가림 (low → 수정)

AR-079의 잘못된 focus assertion이 30초 대기한 뒤 Host의 유휴 control 연결이
만료됐다. finally의 `list`가 BrokenPipe로 실패해 최초 assertion을 가렸다
(`/tmp/fm-detached-focus-openbox-retest.log`). 기존 managed drag fixture와 같이
cleanup 직전 연결을 새로 연다. 별도 Host·fixture cwd 확인·명시적 종료 범위는
유지한다. Python AST 검사와 후속 Linux 실행의 managed tab-windows 17개 검사가
통과했고 cleanup도 exit 0였다 (`target/ci/linux-tail/tests/tab-windows.json`).
의도적 장시간 실패 경로 재검사는 아직 하지 않았다.

### AR-081 — Session Flow GUI fixture의 managed Fork 전환 누락 (medium → 수정)

- 로컬 `RUST_TEST_THREADS=4 bash scripts/test-ci.sh linux extended`의 사전 검사와
  workspace 전체는 통과했지만 `session-flow`의 새 fork pane 대기에서 실패했다.
  원본 전체 로그는 `/tmp/fm-runtime-linux-full-20261117.log`, 결과 사본은
  `/tmp/fm-runtime-linux-firstgate-report-20261117`, 시나리오 로그는
  `/tmp/fm-gui-__qn1h8i`에 보존한다. 이 최초 전체 gate는 exit 1이다.
- fixture가 managed launch에 필요한 private Host를 시작하지 않았다. 격리
  D-Bus의 systemd service 활성화가 실패했고 Fork operation은 pending이었다.
  Host를 처음에 시작한 표적 재검사도 이력 대기 중 빈 Host의 10초 회수로
  실패했다 (`/tmp/fm-runtime-session-flow-host-retest.log`).
- private Host를 Fork 직전에 시작하고 설정 shell의 `-lic`를 지원한다.
  두 번째 재검사에서는 실제 fork pane이 생겼지만 긴 YOLO argv의 줄바꿈과
  초기 PID의 viewport 이탈로 기존 화면 parser가 실패했다
  (`/tmp/fm-runtime-session-flow-host-final.log`). fixture가 PID·argv를 원자적
  파일로 기록하게 하고 실제 live 화면 출력은 별도로 확인한다.
- 최종 표적 재검사는 Fork 새 PID·정확한 argv·기존 실행 보존·Flow 표시 복원까지
  6개 검사와 exit 0를 확인했다 (`/tmp/fm-runtime-session-flow-local-fixed.log`,
  `/tmp/fm-gui-o1dvs_r8`). gate 전달 인자 검사도 두 suite에서 통과했다
  (`/tmp/fm-runtime-session-flow-runner-retest.log`). private 실행을 명시적으로
  종료하고 Host 정상 exit를 확인한다. 제품 Rust 코드는 변경하지 않았다.
- 앞서 통과한 같은 제품 revision의 검사 결과는 보존하고 미실행 단계는
  같은 공유 script의 명령·격리 설정으로 직렬 실행한다. 이 분할 검증을
  최초 monolithic gate가 통과한 것으로 기록하지 않는다. 전체 Linux phase
  coverage 확인 전에는 push하지 않는다.

### AR-082 — 로컬 SSH fixture 실행 파일 미지정 (low → 검증 환경 수정)

후속 Linux 실행은 앞선 32개 phase를 통과한 뒤 마지막 `ssh-workspace`에서
`sshd`를 찾지 못해 exit 2로 중단됐다. 원본은
`/tmp/fm-runtime-linux-tail-20261117.log`와 `target/ci/linux-tail`에 보존한다.
PATH의 `tmux`도 실제 바이너리가 아니라 Flowmux shim이었으므로 기존 `/tmp`
패키지의 `sshd`·`tmux`와 필요한 library path를 fixture 인자로 명시했다.
시스템 패키지·SSH 서비스·사용자 SSH 설정은 변경하지 않았다.

격리 SSH 전체 재검사는 exit 0·253.837초로 통과했다
(`target/ci/linux-local-final/ssh-workspace.json`,
`/tmp/fm-runtime-linux-final-checks-20261117.log`). 채널·포워딩·한글·tmux 생존,
native hook matrix·program status, 원격 코드 리뷰·접속 이력·실제 피드백 전송을
포함한다. 제품 수정이나 원래 후속 gate의 통과로 보고하지 않는다. 로컬 전체
실행 전에 [setup](setup.md)의 의존성뿐 아니라 실제 executable 경로도 확인한다.

### AR-083 — 로컬 실측이 Linux 전체 검사 제한을 초과 (medium → 수정)

초기 실행·후속 실행의 `tests.jsonl`에 실제 기록된 통과 phase만 선택하고 SSH
표적 재검사를 더하면 Session Flow를 제외한 50개 phase가 3,553.257초다.
수정된 Session Flow의 GUI 로그 자체도 69.625초이므로 전체 실행 시간의 하한이
3,622.882초로 기존 regression 제한 3,600초를 넘는다. 분할 실행의 각 통과를
그대로 모아도 원격의 단일 실행은 제한에 걸릴 수 있어 push 전에 수정했다.

내부 전체 제한을 4,200초로 조정했다. 사전 phase의 상한 합계 960초와 합쳐
5,160초이며 Actions step의 기존 5,400초 안에 정리 여유 240초가 남는다.
개별 검사·실패 조건·재시도 정책은 유지한다. 이 하한은 cold build 시간을
예측하거나 최초 실패한 gate를 성공으로 바꾸는 수치가 아니다.
`bash -n`과 Linux wrapper의 실패 전달·격리·재시도 금지 검사가 통과했다
(`/tmp/fm-runtime-linux-budget-retest.log`, exit 0). 최초 표적 호출은 test class
이름을 잘못 지정해 loader 오류가 났으며 제품 검사 실패와 구분한다.

### AR-084 — 재실행의 미실행 phase에 이전 PASS receipt가 남음 (medium → 수정)

최초 전체 실행의 `tests.jsonl`은 Session Flow까지 18개 phase만 기록했지만
같은 report 디렉터리에는 이전 실행의 후반 phase JSON도 남아 있었다. JSON
전체를 glob으로 합치면 현재 실행에서 하지 않은 검사를 통과로 오인할 수 있다.
이번 coverage 계산은 항상 실제 `tests.jsonl`의 phase만 선택하며 원본 실패도
그대로 남긴다.

새 gate 시작 시 해당 generated report 디렉터리의 phase JSON만 제거한다.
직접 `test-linux.sh`를 실행하는 경로도 자체 하위 phase JSON을 제거한다.
진단 log는 이 정리에서 제거하지 않는다. 이전 `passed` receipt를 심고 새 실행을
조기에 실패시키는 회귀 검사는 수정 전 두 경로 모두 실패했고
(`/tmp/fm-runtime-linux-stale-receipts-before.log`), 수정 후 두 경로와 cleanup
실패 시 원래 exit code 보존 검사가 통과했다
(`/tmp/fm-runtime-linux-stale-receipts-after.log`). 제품 실행 경로는 변경하지 않았다.
이후 runner 전체 28개 test가 exit 0로 통과했다(1개 skipped,
`/tmp/fm-runtime-linux-final-runner-20261117.log`). `bash -n`과 Python AST·공백
검사도 통과했다. AR-083·084는 검사 제어·결과 정리만 바꾸며 앞서 통과한 제품
바이너리와 phase의 실제 검사 명령은 유지한다.

### 최종 로컬 Linux 검증 — 푸시 전 2026-11-17

최신 `origin/main` base `82554c97` 위 product source `3f653299`의 extended
Linux 51개 phase가 모두 통과했다. 최초 전체 실행은 Session Flow에서 exit 1,
후속 실행은 SSH 의존 실행 파일 누락으로 exit 2였으며 그대로 보존한다.
각 실행의 `tests.jsonl`에 실제 기록된 통과 phase와 AR-081·082 수정 후 표적
통과를 합친 coverage다. 단일 전체 gate 통과로 보고하지 않는다. 로컬 GTK
4.14.5의 조건부 `gtk-icon-preload`도 포함했다.

locked workspace build·workspace 전체 test·all-target Clippy `-D warnings`,
별도 ignored headless stress, editor web 41개 test·build·12개 asset 검증·dist
무변경, 최종 CI runner 28개 test(1개 skip)가 통과했다. AR-083·084 변경 뒤에는
runner 전체와 shell syntax·Python AST·공백 검사를 확인했다. 이 변경은 검사
제어와 receipt 정리이며 product source와 live phase 명령은 바꾸지 않았다.

격리 native 시나리오는 managed drag 24개, tab-windows 17개, tear-off 9개,
termination 14개, 탭·GUI 종료와 SIGKILL 뒤 같은 PID/birth·단일 spawn 복원,
100회 click 누락 0회(p95 약 39.48ms), 부하 중 400개 입력 보존(p95 약
24.94ms), SSH 전체와 editor I/O·renderer 복구·유휴 안정성을 포함한다.
모든 GUI·Host는 별도 display·D-Bus·XDG·runtime에서 실행했고 사용자 GUI
PID `1663486`과 설치본을 유지했다.

명령·원본 실패·phase별 hash·한계는
[검증 기록](design-assets/agent-runtime-local-linux-final-20261117.json)에 남긴다.
GUI·CLI hash는 live 검증 binary이며 daemon은 후속 headless test가 같은 소스를
다른 feature 조합으로 재빌드한 뒤의 hash이므로 live Host hash로 단정하지 않는다.
macOS·Wayland·Flatpak·실제 provider 31분 작업과 전체 완료/idle 종료 계약은
미검증이며 **미해결 medium 6건과 계획 전체 미완료 판정을 유지한다.**

### AR-071 후속 원격 실패 — 2026-11-17

로컬 Linux 검증 뒤 한 번 dispatch한 run `44099774658`의 macOS job
`134096354123`에서 같은 `missing-cli` case가 재발했다. 기존 bundle 버전과 실행
보존 assertion은 통과했지만 `hdiutil info`에 fixture의 `/dev/disk5`·`disk6` 계열
4개 device가 남았으며 mount point는 없었다. updater stdout/stderr는 비어 있다.
설치 script가 nonzero로 끝났다는 assertion만 있어 attach 성공·검증 단계 진입·
detach 성공 중 어느 것도 이 로그로 확정할 수 없다. 따라서 단순 busy detach나
APFS device 정리, attach의 부분 실패를 원인으로 단정해 제품을 수정하지 않는다.

[원격 증거](design-assets/agent-runtime-final-macos-ci-20261117.json)에 source·
phase·실패 excerpt hash·업로드 artifact를 보관한다. 다음 native 재현에는 명령별
attach/detach 인자·exit status와 전후 device 정보를 확보해야 한다. 로컬 Linux에는
`hdiutil`과 실제 macOS disk image 동작이 없으므로 같은 live 재현은 불가능하다.
AR-071·068은 유지하고 이 실패의 단순 재실행은 하지 않았다.

테스트의 updater 실행을 `/bin/sh -x`로 바꾸고 남은 device assertion에 updater
반환 코드도 남긴다. 기존 stdout/stderr 캡처와 nonzero·설치본·사용자 실행 보존
조건은 유지하며 제품 updater는 변경하지 않는다. Linux에서 실제 fixture helper를
직접 호출해 명령 추적 캡처·exit 1·Darwin guard의 파일 변경 전 중단을 확인했다
(`/tmp/fm-runtime-dmg-trace-selfcheck-20261117.log`). Python AST와 공백 검사도
통과했다. 이는 진단 경로 검증이며 macOS attach/detach나 AR-071 수정 검증이 아니다.

### AR-086 — 긴 출력 대기 뒤 만료된 검사 연결 재사용 (medium → 수정)

run `44099774658`의 Linux `agent-host`는 concurrent memory pressure 출력을
기다린 뒤 `prepare_attach`에서 `BrokenPipe`로 실패했다. 마지막 launch로부터
30.004초 뒤 Host에 read deadline이 기록됐고 첫 후속 요청은 약 40초 뒤였다.
제품 IPC의 30초 유휴 연결 제한에 맞지 않는 fixture였다. 제품 timeout을
늘리거나 효과를 가진 요청을 재시도하지 않는다.

수정 전 provider 준비를 31초 늦추는 격리 재현에서 같은 요청·같은 예외로
exit 1을 확인했다. 출력 대기 후 새 control 연결을 열어 pool 부하 전에 확보한다.
같은 지연을 주입한 수정 후 전체 검사는 33개 통과·exit 0이었다. 직접 실행도
33개 통과·exit 0이며, 영구 회귀는 출력 파싱과 동시에 31초를 기다려 idle
deadline 경과를 항상 포함한다. 별도 display·D-Bus·XDG·Host를 사용하고 사용자
GUI PID `1663486`과 설치본은 유지했다.

메모리·dual pool 포화·retained transfer, 연결 교체 실패, 탭/GUI 종료·SIGKILL 뒤
동일 PID/birth·새 한글 입력·자연 종료와 빈 Host 회수를 유지했다. source·로그
hash·최초 CI 실패·최종 회귀 결과는
[검증 기록](design-assets/agent-runtime-host-idle-reconnect-20261117.json)에 보관한다.
기존 로컬 51개 phase 중 영향받은 Host 검사를 현재 fixture로 다시 검증한 것이며
이전 분할 실행을 단일 전체 gate 통과로 바꾸지 않는다. 제품 Rust는 변경하지
않았고 기존 미해결 medium 6건은 유지한다.

### AR-085 — 실제 provider의 긴 도구 작업과 재접속 검사

`test-agent-codex-live.py --work-seconds N`은 기존 로그인으로 실제 Codex에
읽기 전용 Python 명령을 요청한다. Agent cgroup 안에서 정확한 argv·PID/birth를
확인한 직후 탭과 마지막 격리 GUI를 닫고, 20초마다 같은 Host·Agent·도구의
생존과 attachment 부재를 확인한다. 사용자 GUI는 매 표본에서 보호한다.
도구 완료 뒤 재접속한 VTE의 최신 응답과 새 nonce·한글 응답을 검증하고,
마지막에는 이 fixture 실행만 명시적으로 종료한다.

최초 짧은 검사는 provider가 도구 출력을 버퍼링해 시작 marker를 즉시 표시하지
않는 조건에서 실패했다. 실제 소유 process의 정확한 argv로 시작을 확인하도록
수정했다. 1,860초 실행에서는 **94개 표본·1,860.003초 동안 같은 실행과 도구의
생존이 통과**했다. 그러나 이후 첫 FMRT 응답이 viewport 밖으로 밀려나 전체
검사는 exit 1이었다. 재접속 화면에 FMWORK·FMEND·FMDONE이 있었어도 새 입력
단계까지 실행하지 않았으므로 전체 31분 검사 통과로 기록하지 않는다.

긴 작업 경로에서는 최신 FMDONE을 기다리도록 고쳤다. 일반 짧은 재접속 경로는
기존 FMRT 복원 검사를 유지한다. 수정한 script의 `--work-seconds 1` 전체 검사는
실제 Codex·모델로 5개 통과·exit 0이며, 도구 종료와 새 nonce·한글 왕복 및
명시적 종료·빈 Host 회수를 포함한다. 이어 수정한 script의 `--work-seconds 1860`
전체 실행도 **5개 통과·exit 0**이다. 94개 표본·1,860.003초 모두 같은
Host·Agent·도구 PID/birth와 running·working·attachment 부재를 확인했다.
재접속 뒤 최신 FMDONE과 새 nonce·한글 응답, 명시적 종료·빈 Host 회수까지
통과했다. source `1ffd7c26`의 binary/script로 검증했으며 실행 도중 추가한
IPC 회귀 테스트 소스는 해당 binary에 포함되지 않았다.

[검증 기록](design-assets/agent-runtime-codex-headless-work-20261117.json)에 최초
실패·관측 JSONL·화면·script 및 binary hash를 보관한다. 이 부하는 실제 provider의
합성 도구 대기이며 유용한 개발 작업이나 전체 작업 완료의 증명이 아니다.
자동 idle 종료 계약인 AR-010과 기존 medium 6건은 유지한다.

### AR-073의 높은 병렬도 표적 확인 — 원인 미확정

별도 XDG/runtime에서 daemon library를 `--test-threads=32`로 한 번 실행했다.
294개 통과·1개 ignored, exit 0이며 기록은
`/tmp/fm-runtime-launch-high-concurrency-audit-20261117.log`다. 기존 launch·relay
실패는 재현되지 않았다. 과거 원격 Stopping 실패에는 후반 GUI/Host 상태가 없어
원인을 특정할 수 없다. 통과한 재실행을 수정 증명으로 삼거나 AR-073을 닫지 않는다.

추가로 GTK 애니메이션을 `GTK_SLOWDOWN=8`로 늦춘 별도 X11·D-Bus·XDG
종료 시나리오도 **14개·exit 0**이다. 정상 종료·다시 시작한 실행의 Stopping·
timeout·강제 종료 취소/확인·자식 회수·원래 shell 보존을 확인했다. 제품과
기존 fixture의 대기 시간은 바꾸지 않았다. 실패 시 상태를 보존하는 임시
wrapper는 통과 경로에서 수집을 수행하지 않았으므로 실패 수집 동작을 검증한
결과로 삼지 않는다. [기록](design-assets/agent-runtime-termination-slow8-20261118.json)에
명령·log/binary hash와 범위를 보존하며 **AR-073 medium은 미해결**이다.

원본 log SHA-256: `ce36d09f9bc1da25b155b3078472c7fd58737d7fce93e0602db61960bd841463`.

### AR-068 후속 native 검증 — 해결

로컬 Linux gate 검증 후 push한 source `1ffd7c26`의 run `44109493204`,
macOS job `134130884887`이 48개 phase·`CI_GATE_EXIT=0`으로 끝났다.
기본·One Dark·GitHub Light에서 warning/conflict 글자와 실제 배경색을 읽은
대비 최솟값은 **5.052386**이다. `MACOS_EDITOR_STATUS_CONTRAST_OK`와
`editor-recovery` 통과 및 이후 전체 gate 성공으로 AR-068의 남은 native
재검증 조건을 충족했다. [기록](design-assets/agent-runtime-macos-contrast-final-20261118.json)에
job·source·phase 결과와 실제 색상/대비 로그를 보관한다. 이 run의 DMG 통과로
과거 device 잔류의 원인이 밝혀진 것은 아니므로 AR-071은 유지한다.

### AR-087 · medium · Host 중계 응답의 JSON 구조 확장 — 해결

GUI 응답을 받는 제한된 IPC Client는 4MiB wire 상한만 확인하고 `Envelope`를
역직렬화했다. 약 200KB·40,001개 원소의 유효 응답을 주는 private Unix socket
회귀는 수정 전 **1개 실패·exit 101**로 과도한 구조 수용을 재현했다.
Host 전체 RSS 예산 초과를 측정한 결과는 아니다.

기존 서버의 32,768개 JSON 구분자 상한과 상수 공간 검사기를 제한된 응답에도
재사용한다. serde 호출 전에 거절하고 연결을 재사용하지 않으며 요청을
재전송하지 않는다. 일반 Client의 기본 동작은 유지한다. 실제 제품 caller는
Host의 `DispatchContext` 한 곳이며 사용 문서에도 구조 상한을 명시했다.

로컬 검증은 IPC **97개 unit·9개 integration**, workspace all-target Clippy
`-D warnings`, locked workspace build·format·diff 모두 통과했다. 격리 GUI
**5개·exit 0**에서 큰 응답 거절·효과 1회·느린 응답 7개/거절 49개·control
응답·같은 provider의 새 한글 입력·자연 종료와 빈 Host 회수를 확인했다.
Host peak는 **96,764KiB**다. 새 바이너리의 실제 Codex 짧은 작업·GUI 종료·
재접속·새 한글 응답·명시적 종료/Host 회수도 **5개·exit 0**이다.

[검증 기록](design-assets/agent-runtime-gui-response-structure-20261118.json)에
수정 전 실패, source/diff·binary·log hash와 통과 범위를 보관한다. 구조 초과
거절은 실제 Unix socket 회귀로 검증했고 native GUI가 그 응답을 생성하는
시나리오는 추가하지 않았다. 기존 로컬 51개 phase 분할 결과와 구분하며
AR-006의 전체 회계나 macOS 검증 완료로 확대하지 않는다. 사용자 실행과
설치본을 유지했고 로컬 검증 완료 전 push·새 CI dispatch는 하지 않았다.

### AR-088 · medium · 알림에서 이동한 Agent의 native 입력 실패 — 원인 미확정

source `1ffd7c26`의 run `44109493204`, Linux job `134130885408`은 43개 phase
통과 뒤 `agent-native-hooks`에서 실패했다. Code Review에서 알림으로 돌아온
이동 pane의 `ACK:<notice>`를 기다리다 timeout했다. 보존된 297개 화면 표본은
하나의 같은 초기 화면이며, 이 사실만으로 키가 어디로 갔는지 단정할 수 없다.
Host 로그는 시작 정보만 있고 당시 GUI 로그·포커스 상태는 artifact에 없었다.

후속 IPC 수정의 새 binary로 같은 로컬 native 전체 검사는 **30개·exit 0**이다.
같은 알림의 native 조작 순서를 유지한 표적 시나리오도 **10회 모두 exit 0**이다.
통과한 재실행은 CI 실패 원인의 해결 증명이 아니다. timeout·sleep·제품 focus
코드는 바꾸지 않았다. 실패 직후 접근성 FOCUSED/SHOWING/ENABLED, Host 목록,
pane tree/화면, X focus·눌린 키를 JSON으로 보관하고 기존 workflow artifact에
이 JSON과 해당 GUI 로그를 포함하도록 한다.

실제 격리 알림/키보드 시나리오 뒤 의도적 예외를 발생시킨 self-check는 예상한
exit 1과 원래 예외를 유지하며 **7개 진단 필드·capture_error 없음**을 확인했다.
Python AST·workflow YAML·upload path·diff 검사도 통과했다.
[검증 기록](design-assets/agent-runtime-native-notice-failure-20261118.json)에 CI
실패 artifact hash·43개 phase·로컬 전체/10회·의도적 수집 검사와 범위를 보관한다.
사용자 GUI·설치본은 유지했고 새 CI dispatch나 실패 job 재실행은 하지 않았다.
**AR-088은 열려 있으며 medium 6건과 계획 전체 완료 조건은 남아 있다.**

### AR-089 · medium · Linux pane 선택 뒤 키 입력 전 포커스 지연 — 수정

Linux의 `grab_registered_pane_focus`는 보이는 terminal도 idle callback으로
미뤘다. 같은 GTK turn에서 pane 선택 직후 VTE 포커스를 확인하는 새 회귀는
수정 전 실패했다 (`/tmp/fm-focus-before.log`, exit 101). macOS에서 이미 사용하던
즉시 포커스를 Linux에도 적용하고, 숨겨진 pane의 지연 요청은 최신 선택과
일치할 때만 실행한다. 호출자는 pane 선택·알림 이동·zoom 완료 경로다.

수정 후 동일 격리 GTK 회귀는 exit 0이다 (`/tmp/fm-focus-after.log`). 두 terminal의
즉시 전환과 숨김/재표시 사이의 이전 요청이 새 선택을 덮어쓰지 않음을 확인한다.
이 변경은 macOS의 기존 동작을 유지한다. native 입력과 전체 Linux gate의
최종 결과는 후속 검증 기록을 따른다.

AR-088의 원격 알림 입력 실패와 같은 원인이라고 판정하지 않는다. 수정 전
별도 CPU 하나에 test와 부하 process를 제한한 알림 시나리오 10회가 모두
통과했으며 (`/tmp/fm-runtime-native-notice-pressure-before-20261118.log`),
알림 뒤 고정 대기만 제거한 시나리오도 통과했다. 재현되지 않은 AR-088과 기존
medium 6건은 계속 열려 있다. 전체 로컬 Linux gate가 끝나기 전 push하지 않는다.

### AR-089 후속 Wayland 검증과 접근성 경로의 제한

같은 source `d5facd97`의 별도 GNOME Wayland에서 실제 포인터·키보드 managed
시나리오 **24개·exit 0**이다. 드래그·양방향 재정렬·반복 이동·연결 재생성·
Escape·목적지 취소/확인·다른 workspace 이동·탭/GUI 종료 뒤 같은 PID/birth와
새 입력을 확인했다. 메뉴는 포인터로 열고 Return으로 선택했으며 재시도하지 않았다.

메뉴를 AT-SPI `do_action`으로 여는 별도 실행은 목적지 취소 뒤 메뉴 재열기에서
실패했다. 1초 대기를 추가한 축소 재현도 실패했다. Flowmux 코드가 없는
`Gtk.MenuButton`·`Popover`·`Adw.AlertDialog`·`DropDown` 최소 앱에서도 같은
접근성 popup 재열기가 round 2에서 실패했다. GTK/GDK 내부 원인은 미확정이며
제품의 포인터 경로 통과로 접근성 경로의 실패를 대체하지 않는다.

[Wayland 기록](design-assets/agent-runtime-wayland-local-20261118.json)에 원래
실패·native 24개·축소 GTK 소스와 hash를 함께 보관한다. Wayland 전체 접근성
시나리오 통과나 기존 medium 6건 해결을 주장하지 않는다. 사용자의 실행과
설치본은 유지했다. Linux 전체 gate는 별도로 진행 중이며 아직 push하지 않았다.

### AR-090 · low · 전체 Linux 검사 뒤에야 SSH 준비 누락 확인 — 수정

AR-089 source의 로컬 extended 실행은 사전 8개와 regression 50개를 통과한 뒤
마지막 SSH fixture에서 `sshd` 부재로 exit 2였다. AR-082에서 사용한 격리 도구는
남아 있었지만 전체 gate에 경로를 전달하지 않았고 PATH의 tmux는 Flowmux shim이었다.
원본 gate와 51개 실제 실행 receipt는 `/tmp/fm-linux-incomplete-20261118`에
보존했다. 전체 실행의 실패를 통과로 변경하지 않는다.

fixture의 `--check`를 공용 gate와 Linux 직접 runner의 첫 검사로 사용한다.
OpenSSH와 실제 tmux의 `-V` 실행 결과를 확인하고 Flowmux shim을 거절한다.
`SSH_FIXTURE_SSHD`·`SSH_FIXTURE_TMUX`·`SSH_FIXTURE_LIBRARY_PATH`로 기존
격리 도구를 사전 검사와 실제 GUI fixture 모두에 전달한다. 시스템 설치·서비스·
사용자 SSH 설정을 변경하지 않는다.

fixture 회귀 4개와 runner 회귀 29개(28개 통과·플랫폼 조건 1개 skip)가 통과했다.
실제 누락 환경의 공용 gate는 빌드 전 **0.158초·exit 2**로 중단했고, 실제 설치
shim을 명시한 probe도 exit 2로 거절했다. 유효한 격리 도구의 사전 검사는
exit 0이다. shell syntax·format·diff 검사도 통과했다. SSH 전체 GUI의 후속
결과와 제품 검증 범위는 아래 최종 기록에서 별도로 보고한다.

### AR-073 후속 Linux CI — 정상 종료도 실패 (미해결)

단일 확인 run `44131022038`(source `428e3b81`)에서 macOS native 전체·editor web·
headless stress는 성공했고, Linux는 `agent-termination`의 첫 정상 종료 대기
(`normal termination and reap`, line 800)에서 실패했다. 이전 run의 두 번째 실행
`Stopping registry` 실패와 구분한다. Ctrl+C와 이후 입력은 통과했으며,
17:32:49.673 UTC의 확인 버튼 클릭 좌표는 `(926, 656)`이었다. 클릭 기록만으로
확인 응답·Host 종료 요청 수신을 단정할 수 없다. 실패 후 registry·dialog 상태는
기존 artifact에 없어 원인은 아직 특정하지 못했다.

[실패 증거](design-assets/agent-runtime-ci-termination-failure-20261118.json)에
원본 파일 hash·관련 native 클릭·Host/GUI 연결 로그를 보존한다. native hooks가
통과했어도 AR-088의 원인 해결을 뜻하지 않는다. CI 재실행은 요청하지 않았고,
로컬 Linux extended 전체 gate를 유지하면서 격리 환경에서 원인을 확인한다.

### AR-073 실패 직전 상태 수집 — 진단 경로 검증

종료 fixture가 실패하면 기존 정리 작업 전에 새 read-only Host 연결의 registry,
workspace tree, 접근성 control의 상태·영역, X focus·키 상태와 screenshot을 보관한다.
하나의 수집 실패가 다른 진단이나 원래 오류를 가리지 않도록 항목별로 기록한다.
Host는 새로 시작하지 않고 기존 socket에만 연결한다.

확인 버튼이 보인 직후 고의 오류를 주입한 격리 실행은 예상한 exit 1로 끝났다.
실행이 `running`이고 확인 버튼이 보이는 상태·180개 접근성 node·실제 screenshot을
모두 보존했으며 수집 오류는 없었다. screenshot도 직접 확인했다. 첫 주입 실험은
대화상자가 매핑되기 전에 실패했으므로, 두 번째 실험에서 표시 대기를 명시했다.
[검증 기록](design-assets/agent-runtime-termination-capture-20261118.json)을 보관한다.
진행 중인 전체 gate의 제품 binary는 원래 hash를 유지하며, 이 예외 진단만
managed Session Flow·종료 단계 전에 추가했다. managed Session Flow 정상 경로는
통과했다. 진단 수집 검증을 원래 CI 실패 재현·수정으로 해석하지 않는다.

### AR-091 · medium · 여러 managed Agent가 가로 막대에서 세로로 쌓임 — 수정

AgentBar는 ManagedAgents의 외부 box만 가로로 바꿨고, 실제 live·ended 목록은
세로 방향으로 남아 있었다. 표시 방향을 생성자에서 받아 내부 box까지 전달한다.
호출자는 AgentBar·Sidebar 두 곳이며, Sidebar의 세로 목록은 그대로 유지한다.

두 번째 Agent를 추가할 때 가로 막대 높이가 증가하지 않고 세로 목록 높이는
증가해야 한다는 기존 GTK 회귀의 추가 검사는 수정 전 **exit 101**, 수정 후
**exit 0**이다. 관련 AgentBar·Sidebar GTK 검사 **29개**, workspace all-target
Clippy `-D warnings`, locked workspace build·format·diff 검사도 통과했다.

수정본의 별도 XDG/runtime·Xvfb·D-Bus에서 실제 Host 실행 4개를 표시했다.
접근성 좌표는 x **439·657·875·1092**, y **730**으로 네 항목이 같은 가로줄에
있음을 확인했고 [실제 화면](design-assets/agent-runtime-bar-layout-20261118.png)도
직접 확인했다. 수정 전 높이 증가는 앞선 실패 로그에 보존한다.
[검증 기록](design-assets/agent-runtime-bar-layout-20261118.json)에 source·binary·
회귀/live 로그 hash와 수정본의 후속 native 검사 결과를 보관한다.

같은 수정본의 native managed 드래그 **24개·144.441초**, 종료 **14개·55.485초**,
provider native hook fixture **30개·116.413초**가 모두 exit 0이다. 반복 드래그·
양방향 순서 변경·Background 재연결·다른 workspace 이동·탭/GUI 종료 뒤 동일
PID/birth·새 입력·종료 취소/확인·자식 회수·알림에서 이동 후 native 입력을
포함한다. 실제 provider API 호출은 이 후속 fixture 범위에 포함되지 않는다.
검사 전후 제품 binary hash도 일치했다. 전체 Linux gate 뒤 UI 수정에 해당하는
검사를 추가로 실행한 결과이며, 수정본으로 52개 phase 전체를 재실행한 결과는 아니다.

사용자 실행과 설치본은 변경하지 않았다. 이 수정은 열린 medium 6건이나
과거 CI의 원인 미확정 종료·알림 실패를 해결 처리하지 않는다.

### AR-006 후속 — 7개 실행 전체와 두 IPC pool의 동시 부하

기존 단일 실행 fixture의 provider·격리 Harness를 재사용해 7개 실행 모두에
최대 폭 500열의 primary/alternate grid·결합 문자·title/link 부하를 채웠다.
각 실행의 active/pending frame을 겹친 채 일반 IPC 잔여 49개·control 16개를
채우고, 최대 1MiB 요청과 미완성 다음 요청을 유지했다. 이때 Host VmHWM은
**1,983,304KiB**로 3GiB 아래였으며 pending frame의 연속 전송도 유지됐다.

이후 7개 모두 실제 GUI에 재연결해 같은 PID/birth·spawn 1회와 새 한글 입력을
검증했고 자연 종료·빈 Host 회수를 확인했다. 전체 **128.399초·exit 0**이다.
[검증 기록](design-assets/agent-runtime-seven-host-budget-20261118.json)에
명령·source/binary/log hash와 측정 범위를 보관한다. 이 검사를 공통 Linux
`extended` 스위트에 추가했다. 제품 코드는 변경하지 않았다.
공유 runner 검사는 **28개 통과·플랫폼 조건 1개 skip**이며 regression/extended의
정확한 phase 집합·빌드 전 실패 중단·환경 격리를 확인했다.

이 부하는 lifecycle ledger·전체 metadata·forwarded GUI 응답의 최대 보유량까지
동시에 채운 증명은 아니다. 커널 socket buffer·별도 provider/GUI RSS도 Host
VmHWM에 포함되지 않는다. **AR-006과 나머지 medium 항목은 계속 미해결**이다.

### AR-073 후속 — 실패 진단 파일의 CI 업로드 누락 수정

종료 fixture가 생성하는 `session-resume-failure.json`·`.png`가 workflow의
artifact 경로에 빠져 있었다. Linux `always()` 업로드 목록에 두 파일을 추가했다.
로컬 YAML 파싱과 생성 파일명·업로드 경로의 일치 검사를 통과했다. 앞선 고의
오류 주입은 파일 생성까지만 검증했으므로 업로드 검증으로 확대 해석하지 않는다.
진행 중인 run `44145082591`에는 이 경로 수정이 없고 AR-073 원인도 미확정이다.

### AR-092 · medium · 복제된 descriptor가 상태 잠금 수명을 연장 — 수정

`InstanceLock`은 파일 close만으로 flock이 해제된다고 가정했다. 같은 open file
description을 공유하는 dup/fork descriptor가 남으면 guard drop 뒤에도 별도
open의 잠금 획득이 실패한다. dup를 보존한 회귀는 수정 전 exit 101이다.
guard의 Drop에서 명시적으로 unlock하여 guard 수명에 맞춰 잠금을 해제한다.

수정 후 기본 병렬 상태 library 102개 검사를 20회 모두 통과했다.
[검증 기록](design-assets/agent-runtime-state-unlock-20261118.json)에 로그 hash와
검사 범위를 보관한다. 이 반복은 후속 Agent-view 비차단 수정도 포함한 작업
트리를 검사했다. 과거 모든 병렬 실패의 원인을 이 현상으로 단정하지 않는다.
최종 source 전체 Linux gate와 실제 GUI 검증은 후속 기록으로 구분한다.

### AR-093 · medium · 상태 저장 잠금이 Host와 Agent 입력을 정지 — 수정

Agent-view prepare·commit·discard가 동기적인 blocking flock을 획득했다.
외부에서 `state.lock`을 보유하면 Host actor가 이동 취소 안에서 멈춰 기존
GUI 입력도 처리하지 못한다. 격리 live에서 abort_attach가 timeout으로 실패했다.
세 경로는 try-lock으로 경합을 즉시 반환하며 기존 입력 소유권을 유지한다.
일반 window 저장의 기존 잠금 동작은 유지한다. 취소의 checkpoint 삭제 실패는
기존처럼 기록되며 generation 검증과 다음 prepare의 교체로 처리한다.

bounded 회귀는 세 경로의 WouldBlock과 실패한 획득이 기존 소유자의 잠금을
해제하지 않음을 확인한다. 최종 상태 패키지 검사는 library 102개와 별도
integration/doc 검사를 통과했다. 비차단 수정의 실제 GUI 검사는 34개·123.272초
통과했고 잠금 보유 중 이동 취소·새 한글 입력·기존 lease 보존을 확인했다.
그 live는 AR-092 이전 binary이므로 최종 source 전체 gate를 별도로 실행한다.
[검증 기록](design-assets/agent-runtime-state-contention-20261118.json)을 보관한다.
기존 AR-073·AR-088의 간헐 실패 원인과 동일하다고 단정하지 않는다.

### AR-094 · medium · New Agent fixture의 메뉴 배치 전 좌표 사용 — 검증 중

run `44145082591`은 Linux `new-agent`에서 첫 Cancel 대기가 만료됐다.
`events.jsonl`은 Workspace creation options `(47, 26)` 다음 New Agent 클릭을
`(17, 5)`로 기록한다. click helper는 SHOWING만 기다리고 곧바로 좌표를
읽으므로 popup allocation 이전 geometry를 사용할 수 있다. 배치가 끝난
양수 geometry를 연속 두 번 확인하고 클릭하며 raw bounds도 event에 남긴다.
클릭을 자동 재전송하거나 timeout을 늘리지는 않는다.

0 크기 반복·초기 작은 rectangle·최종 배치로 바뀌는 입력을 주입한 회귀와 기존
harness 검사 **8개**를 통과했다. 실제 CI 실패의 raw rectangle과 screenshot은
그 run의 업로드 누락으로 보존되지 않았다. 업로드 경로는 앞선 AR-073 수정에
포함돼 있다. geometry 초기화 경합과 일치하는 좌표지만, 전체 최초 원인이
확정된 것으로 확대하지 않는다. 후속 live와 CI를 확인해야 한다.

진행 중인 최종 Linux gate는 이 fixture를 처음 호출하는 managed Session Flow
이전의 Team recovery 단계에서 검사 코드만 갱신했다. 제품 binary는 AR-093의
빌드를 유지하며 이 fixture의 모든 실제 호출은 수정 후 source를 사용한다.
변경 harness 단위 검사는 별도로 실행했으며 이전 preflight 통과에 포함시키지 않는다.

### AR-095 · medium · New Agent fixture가 종료된 빈 Host 연결을 재사용 — 수정

최종 로컬 Linux gate는 `new-agent`의 취소와 잘못된 경로 오류 확인 뒤
`BrokenPipeError`로 중단됐다. fixture는 GUI 준비 전에 Host를 시작했지만,
실행이 없는 Host는 시작 10초 뒤 정상 종료한다. 실제 로그의 연결 단절도
이 시각과 일치한다. 실행 수명 문제와 구분하며 제품 종료 정책은 변경하지 않는다.

Host 시작을 유효한 실행 요청 직전으로 옮겼다. 취소와 잘못된 경로는 Host
socket과 provider 시작 marker가 모두 없음을 검증한다. 수정 후 격리
New Agent GUI **10개 검사**를 통과했다. 같은 PID/birth·spawn 1회, 탭 닫기·
재접속, GUI 종료·복원 뒤 새 입력, 자연 종료와 빈 Host 회수도 확인했다.
[검증 기록](design-assets/agent-runtime-deferred-host-20261118.json)을 보관한다.
원래 전체 gate의 실패는 보존하고, 미실행 phase는 별도 로컬 실행으로 기록한다.


### AR-096 · medium · 최대 화면 복원과 10초 연결 제한의 충돌 — 수정

로컬 extended gate의 7-Agent Host 예산 검사는 RSS 1,949,780KiB로 상한을
통과했지만 세 번째 GUI 재연결은 11.836초 뒤 connection timeout으로 실패했다.
동일 binary의 진단 실행은 7개 모두 통과했으나 연결에 7.606–10.042초가 걸렸다.
지원 범위인 16MiB rich snapshot의 전송을 완료해야 readiness를 승인하는데,
GUI 대기·목적지 admission·Host 교체 예약에는 각각 10초 제한이 있었다.
재실행 성공만으로 원래 실패를 닫지 않는다.

공통 30초 attachment 제한을 GUI 대기·상태 표시·목적지 승인·Host 교체 예약에
적용한다. 새 창 전체 대기는 startup 여유 15초를 더하고 테스트 IPC도 이보다
짧게 끊지 않도록 한다. frame의 크기·Unicode·history와 token·lease·PID/birth·
source input 보존 검증은 유지한다. 자동 재시도와 provider 재실행은 추가하지 않는다.

기존 live snapshot-write 차단 fixture에서 11초 지연 후 GUI open과 cross-window
move의 승인·새 입력을 검사한다. 예약 만료는 31초 차단으로 따로 검사한다.
7-Agent 검사는 각 연결 시간을 기록하고 실패 시 Host 상태와 bounded 화면을
보관한다. 최종 source의 관련 격리 GUI 7개 phase가 모두 통과했다. 최대 부하에서
7개 연결은 7.734–10.591초였으며 같은 PID/birth·spawn 1회·새 한글 입력·자연
종료와 빈 Host 회수를 확인했다. peak RSS는 1,975,804KiB였다. 11초 지연 후의
open·창 간 승인과 31초 만료 시 원래 입력권 보존도 통과했다.

31초 negative 검사 중 관찰용 control 연결을 방치해 IPC의 30초 유휴 제한으로
끊긴 두 fixture 실패도 보존한다. 긴 대기에서는 기존 실행과 소유권을 반복 조회해
검증하며 효과가 있는 요청을 재시도하지 않는다. 드래그 24개·창 밖 분리 9개도
최종 binary에서 통과했다. [원래 실패·최종 검증](design-assets/agent-runtime-attach-deadline-20261118.json)에
receipt·연결 시간·로그 hash·binary hash를 보관한다. AR-006의 전체 worst-case
회계나 다른 미해결 medium을 해결한 것으로 확대하지 않는다.

### 푸시 전 Linux 로컬 검증 정리 — 2026-11-18

최신 `origin/main` `82554c97` 위의 제품 수정은 AR-092–AR-096을 각각 커밋했다.
CI 실패를 반복 푸시로 확인하지 않고 Linux GTK 4.14.5의 격리 Xvfb·D-Bus·XDG·
Host와 로컬 SSH fixture에서 먼저 검증했다. 사용자 GUI PID `1663486`과 설치본은
종료·재시작·교체하지 않았다.

`bash scripts/test-ci.sh linux extended`의 최초 전체 실행은 New Agent fixture에서
실패했다. 이어간 예산 검사도 10초 연결 제한에서 실패했다. 두 원래 실패와
관찰용 연결 유휴 만료·기존 GTK 만료 fixture의 중간 실패를 모두 보존한다.
**단일 전체 gate 통과 기록으로 바꾸지 않는다.** 수정과 순차 후속 실행을 합쳐
Linux extended의 53개 phase를 모두 다뤘으며 52개는 직접 PASS receipt가 있다.
workspace의 최종 확인은 아래 실제 결과로 분리한다.

- GUI 검사에서 968개 통과·기존 ignored 4개, 오래된 11초 만료 fixture 1개 실패.
  이를 공통 `ATTACH_TIMEOUT + 1초`로 맞춘 GTK 회귀 1개가 통과했다.
- 나머지 workspace 패키지의 `cargo test --workspace --exclude flowmux --locked
  --no-fail-fast -- --nocapture`는 1,487개 통과·기존 ignored 9개였다.
- 별도 browser navigation integration 2개가 통과했다. 위 분할 결과를 하나의
  새 전체 workspace PASS receipt로 합성하거나 과거 집계와 단순 합산하지 않는다.

AR-096 최종 binary의 관련 GUI 7개 phase·95개 검사, 미실행이던 안정성·에디터·
지연 복원·SSH 6개 phase가 통과했다. 최종 Session Flow 9개·세션 context 18개·
New Agent 10개도 통과했다. 드래그·창 밖 분리·탭/GUI 종료·SIGKILL 복원에서
동일 PID/birth·spawn 1회·새 입력과 자연 종료 후 Host 회수를 확인했다.
locked workspace build·all-target Clippy `-D warnings`·format·diff 검사도 통과했다.

[전체 로컬 검증 기록](design-assets/agent-runtime-local-linux-coverage-20261118.json)에
원래 실패·후속 receipt·로그 hash·검토 범위·미해결 medium을 보관한다. 마지막
browser integration 실행이 GUI를 같은 Rust source로 재링크했으므로, 실제 live에
쓴 GUI hash와 integration 후 hash도 구분한다. 일반 workspace에서 직접 검토했으며
독립 worker review를 수행한 것으로 주장하지 않는다.

AR-094의 안정된 좌표 검사와 현재 New Agent live는 통과했다. 최초 CI의 raw
geometry가 없으므로 그 실패 원인의 완전한 확정이나 CI 후속 통과로 처리하지
않는다. AR-006·AR-008·AR-010·AR-071·AR-073·AR-088도 기존 미해결 상태다.
실제 provider/model 장시간·macOS native·Flatpak 전체 수용 검증을 이번 Linux
결과로 대체하지 않으며, 계획 전체 완료 또는 medium 이상 0건을 선언하지 않는다.
다음 단계는 이 검증 결과까지 커밋한 뒤 별도 브랜치 푸시와 동일 source의 CI 확인이다.

### 로컬 검증 후 단일 CI 확인 — 2026-11-18

위 Linux 로컬 검증을 마치고 `agent/runtime-audit-20261117`에 source
`0e17ae65`까지 푸시한 뒤 Test workflow를 `platform=all`로 한 번 실행했다.
[run 44166374894](https://github.com/flowmux-ai/flowmux/actions/runs/44166374894)는
첫 attempt에서 **Linux·macOS·editor web·headless stress 4개 job 모두 성공**했다.
시작은 20:10:01 UTC, 최종 완료는 21:03:31 UTC다. 재실행과 CI 디버깅용
추가 제품 푸시는 없었다.

다운로드한 두 artifact의 SHA-256이 GitHub digest와 일치한다. Linux는 공용
gate exit 0이며 nested extended **53개 phase 전부 passed·exit 0**, 누락은 없다.
GUI unit은 **969개 통과·실패 0개·기존 ignored 4개**다. 실제 managed 드래그
24개·창 밖 분리 9개·New Agent 10개·명시적 종료 14개·native hook 30개 검사도
통과했다. 7-Agent Host 부하의 peak RSS는 **1,924,532KiB**, 재연결은
**6.202–9.198초**이며 같은 실행·새 입력·자연 종료와 빈 Host 회수를 확인했다.
macOS도 공용 gate exit 0, extended **48개 phase 전부 passed·exit 0**이다.

[최종 CI 검증 기록](design-assets/agent-runtime-post-local-ci-20261118.json)에
정확한 tested SHA·첫 attempt·job·artifact hash·phase 목록과 관련 live 결과를
보관한다. 로컬 최초 실패와 후속 범위 검증은 이전 기록 그대로 유지하며, 원격의
이번 단일 gate 통과로 로컬 기록을 소급 변경하지 않는다.

사용자가 요구한 로컬 우선 순서도 `CLAUDE.md`에 별도 커밋 `a44dd6e5`로 명시했다.
Agent runtime은 Linux extended 범위·격리 live 검증 후 푸시하고 CI에서 해당
리비전을 확인한다. 변경이나 새 실패로 무효화된 범위를 다시 검사하며, 후속
범위 검증과 단일 전체 gate 통과를 구분한다. CI 이후 변경은 개발 지침과 이
검증 기록뿐이며 제품·테스트 source를 변경하거나 CI를 다시 시작하지 않았다.

AR-094의 후속 CI 확인은 충족했지만 최초 raw geometry는 여전히 없다.
이번 정상 종료·알림·DMG 검사 통과를 AR-073·AR-088·AR-071의 최초 실패
원인 증명으로 확대하지 않는다. AR-006·AR-008·AR-010도 미완료이므로,
**미해결 medium 7건과 계획 전체 미완료 상태를 유지한다.** 사용자 실행 앱과
설치본은 교체·재시작하지 않았다.


### AR-097 · medium · 실패한 공용 gate가 격리 GUI 진단 파일을 삭제 — 수정

최종 로컬 Linux extended gate의 52개 phase는 통과했으나 SSH GUI의 마지막
정상 종료가 20초 timeout으로 실패했다. 공용 gate의 EXIT cleanup이 성공·실패를
구분하지 않고 `test_root`를 삭제해 상세 GUI 로그와 이벤트를 잃었다. 원본 stdout과
receipt는 보존했으나 최초 종료 지연 원인을 단정할 수 없다.

실패 시 격리 디렉터리를 `target/ci/<platform>/failure-fm-ci.*`로 이동해 기존 CI
artifact에 포함하고 `CI_FAILURE_ARTIFACTS`에 위치를 출력한다. 성공한 gate는
기존대로 정리한다. 실패·timeout exit code를 유지하며 재시도하지 않는다.
runner 전체 29개(28개 통과·1개 skip)와 최종 수정의 표적 3개가 통과했다.
실제 fixture 진단 bytes 보존·exit 17/124 유지·성공 cleanup 오류의 exit 0 유지도
검사했다. 제품 코드는 변경하지 않았다. 최초 SSH 종료 문제는 AR-098로 남긴다.


### AR-098 · medium · 로컬 SSH 검사의 최종 GUI 정상 종료 timeout — 원인 미확정

최종 전체 gate의 SSH GUI는 기능·native hook·프로그램 상태 검사를 마친 뒤
`Harness.close_window(first)`의 20초 종료 대기에서 실패했다. AR-097 이전 cleanup이
원래 GUI·이벤트 파일을 삭제했으므로 WM 요청 미전달, GTK 저장·종료 지연,
runtime 정리 지연 중 무엇이었는지 증명할 수 없다.

같은 제품 binary의 SSH 전체 단독 후속은 exit 0이다. 최초 실패 지점을 통과했고
원격 코드 리뷰·폼/키링·피드백 Send까지 마쳤다. 프로그램 상태 검사 뒤 GUI 종료도
별도 격리 live에서 exit 0이다. 이 통과를 원인 수정이나 전체 gate 성공으로
처리하지 않는다. 최종 진단 harness와 최초 runner 환경(`TMPDIR`, fatal-criticals,
locale, desktop 값, X11 lock)을 맞춘 SSH phase 후속도 **251.614초, exit 0**이다.
원래 종료 지점과 원격 리뷰·키링 재시작·피드백 전송까지 통과했다. 전체 gate는
반복하지 않았으며 최초 종료 지연 원인은 미확정이다.

공통 close fixture는 다음 timeout 시 요청한 X11 창 ID와 시각을 events에 남기고,
cleanup 전에 남은 창 이름·map state·각 스레드 wchan을 JSON으로 기록한다.
20초 제한과 실패는 그대로 유지하며 자동 재시도·대기 증가·사용자 GUI 조작은 없다.
최종 harness unit 9개가 통과하며 timeout 원본 전파·진단 파일·재시도/신호 없음도
검사한다. 이는 원인 진단 보강 커밋이며 AR-098 medium 해결 커밋이 아니다.

[후속 기록](design-assets/agent-runtime-ssh-close-followup-20261118.json)의 원본 hash와
범위를 따른다. 제품 코드는 CI 성공 source `0e17ae65`와 같다. 로컬 실패 뒤
푸시·CI dispatch를 보류했으며 미해결 medium은 8개다. 같은 전체 gate의 반복으로
진전을 대신하지 않고 이 종료 지연 조건의 원인 확인부터 이어간다.


### AR-099 · medium (검사 도구) · IPC 응답을 창 준비 완료로 오인 — 수정

격리 GUI의 `Harness.window`는 IPC socket과 `ping`만 기다렸다. 실제 live에서
아직 X11 창이 없는 시점에 반환됐고, 직후 닫기를 요청하면 `close_request`의
창 목록이 비어 있어 WM_DELETE_WINDOW를 전혀 보내지 않았다. 그런데도 20초
종료 대기를 시작해 요청 미전달을 GUI 종료 지연처럼 보고했다. 이는 새로 재현한
검사 도구 결함이며, 최초 전체 gate의 AR-098 원인이라는 증거는 없다.

공통 helper는 해당 PID의 X11 창이 viewable일 때까지 준비를 기다린다. 닫을 창이
없으면 요청을 보내지 않았음을 즉시 실패로 보고하며 종료 대기·재시도를 하지 않는다.
기존 20초 종료 제한은 유지한다. 또한 AR-098의 진단 파일 쓰기 실패가 원래
`TimeoutExpired`를 덮지 않도록 별도 커밋으로 수정했다.

새 회귀 2개는 수정 전 모두 실패했다. 최종 harness unit 12개는 모두 통과했다.
두 격리 GUI의 저장 공유·연속 출력 중 닫기도 0.628초에 정상 종료했다. 별도 live는
실제 창에 종료 요청을 전달한 뒤 state lock으로 저장을 지연시키고 진단 파일 쓰기를
고의 실패시켰다. 20초 원래 timeout이 보존됐고 lock 해제 뒤 GUI가 정상 저장·종료했다.
이 고의 지연을 제품의 자연 발생 실패로 집계하지 않는다.
최종 helper의 SSH 전체 phase도 **251.359초·exit 0**이다. 원격 리뷰·키링
재시작·피드백 Send를 마쳤고, main GUI의 닫기 요청 3회 모두 실제 X11 대상이
있었음을 확인했다. 전체 Linux gate를 반복하거나 새 CI를 시작하지 않았다.

[검증 기록](design-assets/agent-runtime-window-ready-20261118.json)에 수정 전 실패와
후속 범위·log hash를 보관한다. 제품 Rust source는 변경하지 않았다. 최초 SSH 종료
지연과 기존 미해결 medium 8개는 유지하며, 로컬 전체 gate 성공이나 계획 완료로
확대하지 않는다. 최신 fetch의 `origin/main`은 `82554c97`로 이미 HEAD에 포함돼 있다.

### AR-006 후속 — lifecycle ledger와 terminal 최대 부하의 동시 보관

기존 최대 terminal 검사는 큰 lifecycle ledger를 함께 보관하지 않았다. 같은
실제 Host에 60KB wait ID를 차례로 보내 140번째의 8MiB 회계 한도 초과를
확인하고, 제한된 ledger를 유지한 채 저장 제목·두 grid·resize cache·active와
pending frame을 채웠다. lifecycle 관측 한도 초과는 Unknown으로 표시되며
실행과 입력은 유지해야 한다. 별도 검사를 만들지 않고 기존 공용 Linux gate의
`test-agent-terminal-budget.py`에 이 조건을 추가했다.

격리 live **3개·exit 0**이다. Host VmHWM **357,200KiB**로 실행 예약
393,216KiB 아래였고 bulk 출력 중 제어 응답 최대 **0.029531초**였다. 실제 GUI가
원래 PID/birth로 연결해 새 한글 입력에 응답했고 spawn은 1회였다. 자연 종료 뒤
provider·Host process와 socket이 정리됐고 사용자 GUI PID `1663486`은 유지했다.
Python AST·diff 검사를 통과했으며 Rust 제품 source는 변경하지 않았다.

[기록과 hash](design-assets/agent-runtime-ledger-terminal-budget-20261118.json)에
명령·로그·binary·source를 보관한다. 관측 한도 초과 뒤에는 provider 화면의
완료 보정이 비활성이므로 그 경로의 할당까지 함께 측정한 것은 아니다. 전체
worst-case 회계 AR-006과 최초 SSH 종료 timeout AR-098은 계속 미해결이다.
전체 gate 반복·푸시·CI dispatch는 하지 않았다.
