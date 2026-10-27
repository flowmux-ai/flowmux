<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Flowmux 과부하·크래시 경로 점검 — 2026-10-27 (macOS)

사용 중 앱이 스스로 종료되거나 멈춘다는 리포트에 대해, 재현 경로가 없는
상태에서 (1) 실제 크래시 기록, (2) 영역별 코드 점검, (3) 격리된 GUI 과부하
테스트를 함께 진행했다. [장기 유휴 점검](stability-audit-2026-10-27.md)과
[메모리 점검](memory-stability-audit-2026-10-22.md)에서 이미 수정한 항목은
다시 세지 않았다.

## 크래시 기록

`~/Library/Logs/DiagnosticReports`의 flowmux 보고 4건은 모두 개발 빌드이며
실행 5–16초 안에 종료됐다. 10-23의 두 건은 저장소에 없는 임시 디버그 모듈이고,
10-21 11:36은 코드 서명 제약 위반이다. 10-21 20:58은 GTK tick callback 안의
Rust panic이 C 프레임을 넘지 못해 `panic_cannot_unwind`로 abort된 경우다.
같은 날 디스크가 가득 차(`No space left on device`) 앱의 panic hook이 crash
파일을 쓰지 못했으므로 panic 메시지는 남아 있지 않다. 설치된 0.12.0 앱의
크래시 보고는 없었다. 앱 자체 crash 디렉터리의 9월 기록 5건은 이미 수정된
objc2 ivar 정렬 panic과 그 후속 `Once` poison이다.

## 코드 점검 범위와 결과

AgentOffice, 터미널·PTY, macOS WKWebView 브라우저·에디터, IPC→GTK dispatch,
창·사이드 패널·주기 작업·알림, daemon 상태 저장소, 그 외 UI(편집기, 파일
브라우저, 리뷰 창, 옵션, 이미지 뷰어)를 각각 전체 읽기로 점검했다. GTK/VTE
콜백에서 결정적으로 panic하는 경로는 찾지 못했다. 수정한 항목은 다음과 같다.

| 커밋 | 영역 | 문제 | 수정 |
| --- | --- | --- | --- |
| `f85aa43f` | 터미널 | VTE가 자식을 reap한 뒤에도 PTY가 같은 PID를 자기 프로세스 그룹으로 취급해, 셸이 이미 종료된 탭을 닫으면 재사용된 PID의 그룹에 SIGHUP·SIGKILL을 보냄 | child-exited에서 PTY에 종료를 기록하고 이후 close 경로의 시그널·reaper를 건너뜀. 테스트는 수정 전 실패, 수정 후 통과 |
| `944bb73b` | 창 | GTK 명령 루프는 detached glib task라 handler panic 시 glib가 panic을 삼키고 task를 끝냄. 창은 살아 있지만 이후 모든 CLI·hook 명령에 응답하지 않음 | 순서 lane과 control lane의 각 명령을 `catch_unwind`로 감싸 로그 후 다음 명령을 계속 처리. panic hook의 crash 파일 기록은 유지 |
| `bdc39e83` | 상태 저장 | 저장 실패 시 1초마다 전체 state 파싱·직렬화를 재시도. 10-21 로그에 ENOSPC가 1초 간격으로 기록됨 | 연속 실패마다 지연을 2배로 늘려 최대 60초, 성공 시 초기화 |
| `c220a093` | IPC | `browser wait`의 timeout·poll과 `eval-async` timeout에 상한이 없어, 연결을 끊은 클라이언트가 permit을 영구 점유하거나 1ms 간격으로 페이지 JS를 실행 가능 | wait 1..300000ms, poll 10ms 이상, async timeout 1..300000ms로 dispatch 전에 거부 |
| `b5391681` | macOS 브라우저 | 탭을 닫으면 delegate는 사라지지만 진행 중 다운로드는 10Hz 타이머가 `WKDownload`를 유지해 계속 디스크에 쓰고 타이머가 창 수명만큼 남음. 미디어 권한 프롬프트를 응답 없이 버리면 WebKit completion handler가 호출되지 않아 예외 | 다운로드 UI 항목 drop 시 취소, 권한 응답을 drop 시 Deny하는 guard 추가. 격리 GUI에서 수정 전 닫은 뒤 4초간 10MB 증가, 수정 후 증가 없음·서버 abort 확인 |
| `f918957e` | 명령 팔레트 | 모든 행 handler가 dialog를 강한 참조로 잡아 닫힌 창과 전체 위젯 트리(워크스페이스·pane·탭·키바인딩·파일 행마다 버튼·박스·라벨)가 열 때마다 누수 | 옵션 dialog와 같은 weak 참조로 변경. macOS `FLOWMUX_PALETTE_RELEASE_SMOKE_ONLY` 검사(수정 전 timeout, 수정 후 통과)와 Linux GTK 테스트 추가 |
| `0359ea90` | 에디터 | 두 백엔드 모두 web process 종료마다 무조건 reload·재초기화. 매번 OOM되는 문서는 끝없이 프로세스를 재생성 | 뷰당 3회까지만 reload 후 실패로 기록 |
| `c140b7e4` | UI | 알림 popover가 GTK가 돌려줄 수 있는 invalid position으로 인덱싱. 숨겨진 activity 패널의 layout tick이 매 프레임 계속 돌고 pending 플래그가 이후 layout을 막음 | 검사된 조회로 교체, 패널이 보이지 않으면 tick 종료 |

## 과부하 테스트

격리된 macOS GUI(별도 XDG·runtime 디렉터리, 사용자 세션 미접촉, 기본 GL
렌더러, debug 빌드)에 raw IPC로 다음을 반복했다: 터미널 탭 생성 → 200,000줄
출력 + OSC 제목 200회 → 닫기, pane 분할 6단·resize·닫기, 로컬 canvas 페이지
브라우저 열기 → 6회 이동·snapshot → 닫기, agent activity/session 갱신 200회,
알림 100건 생성·clear, 16스레드 × 100 IPC 동시 요청, 워크스페이스 5개 생성.
각 반복 후 RSS·VSZ·스레드·`lsof` 항목·직접 자식 프로세스 수를 기록했다.
아래 FD 열은 `lsof -p`의 출력 행 수(헤더 제외)로, 메모리 매핑도 포함하므로
숫자 파일 디스크립터만의 개수가 아니다. 자식 메모리 합계는 측정하지 않았다.

| 실행 | 구간 | GUI RSS (MiB) | 스레드 | FD | 자식 |
| --- | --- | ---: | ---: | ---: | ---: |
| 10회 × 7시나리오 (50초) | 기준 → 워크스페이스 50개 후 | 189.8 → 265.8 | 22 → 26 | 128 → 246 | 1 → 51 |
| 30회 × 6시나리오 (146초) | 기준 → 종료 전 | 189.7 → 226.3 | 22 → 25 | 128 → 146 | 1 → 1 |

30회 실행에서 첫 브라우저 열기 이후 RSS·FD·스레드 수는 반복과 무관하게
일정했고 IPC 오류 및 GUI 로그의 panic·CRITICAL·ERROR는 없었다. 워크스페이스 50개는 셸 50개에
해당하는 증가와 함께 관찰됐다. 이 결과만으로 누수 부재를 증명하지는 않는다. `leaks`는 418건 36KB를 보고했으나 모두
Apple XPC/LaunchServices 프레임이고 flowmux 프레임은 없었다.

이 측정은 수정 전 코드의 짧은 반복이며, 위 수정 대상 경로(이미 종료된 탭
닫기, 명령 팔레트 반복 열기, 진행 중 다운로드와 탭 닫기, handler panic)는
raw IPC로 재현할 수 없어 각 항목의 전용 검사로 확인했다.

## 검증 결과

모든 수정을 적용한 debug 빌드로 같은 과부하 시나리오를 20회 반복했다
(101초). 기준 190.3MiB에서 터미널·분할·브라우저·에이전트·알림·IPC 반복
구간 내내 224MiB 전후로 일정했고 워크스페이스 100개 생성 후 321.2MiB,
정착 후 308.0MiB였다. 스레드 23 → 23, FD 128 → 346(셸 100개), IPC 오류 0,
GUI 로그의 panic·CRITICAL·ERROR 0, `leaks` 416건 35KB(모두 Apple 프레임)였다.

전용 검사: PTY 테스트는 수정 전 reaper가 그룹을 종료해 실패하고 수정 후
통과한다. 다운로드 검사는 수정 전 탭 닫기 후 4초간 0.5MB → 10.1MB로
증가했고, 수정 후 0.5MB에서 멈추며 서버가 연결 중단을 관찰했다. 팔레트
해제 검사는 수정 전 10초 내 위젯 149개(창 1개는 참조 49개)가 남아 실패하고,
수정 후 통과한다. dispatch guard, 저장 재시도 지연, IPC 상한, 에디터 reload
상한은 단위 테스트로 확인했다.

`scripts/test-ci.sh macos` 전체 gate는 37개 phase 모두 통과했다(`CI_GATE_EXIT=0`, 새 `palette-release` phase 포함). 첫 실행은 CI runner 자체 검사가 새 phase를 몰라 실패했고, mock 등록과 phase 위치 조정 후 재실행했다. 원격 Linux Test·Sanitizers CI 결과는 아래 커밋 기록을 참고한다.

## 남은 항목

- 워크스페이스 overview의 선택·닫기는 dispatch 루프를 거치지 않고 직접
  실행된다. 진행 중인 명령이 await 뒤 재개되면 방금 제거된 워크스페이스의
  stack page·사이드 패널 행을 다시 만들 수 있다. 루프를 거치면 overview가
  즉시 닫혀 카드 애니메이션이 사라지므로 이번에는 바꾸지 않았다.
- 동기 `browser eval`·네이티브 입력은 호스트 측 기한이 없어 멈춘 페이지가
  mutation permit을 탭이 닫힐 때까지 점유한다. `TerminalOutput` 텔레메트리는
  mutation lane을 사용해 확인 대화상자가 열린 동안 모든 mutation이 `Busy`가
  될 수 있다.
- AgentOffice 리사이즈 중 매 프레임 배경을 다시 굽고, `Canvas::bake`는 Cairo
  할당 실패 시 draw 콜백에서 panic한다(작은 고정 크기 surface라 OOM에서만).
- Codex 자식 turn 시퀀스 맵은 세션이 끝날 때까지만 증가한다(항목당 수백 바이트).
- 여러 `RefCell` borrow가 GTK 시그널을 낼 수 있는 호출을 가로질러 유지된다.
  현재 동기 handler는 같은 cell을 건드리지 않아 panic하지 않지만, 재진입
  handler가 추가되면 abort된다. `SourceId::remove`는 이미 끝난 source에 대해
  panic하므로 저장된 ID는 Break 경로마다 비워야 한다.
- macOS 팔레트 순서 smoke(`FLOWMUX_PALETTE_ORDER_SMOKE_ONLY`)는 이번 변경과
  무관하게 HEAD에서 실패하며 CI에 포함되어 있지 않다.
- Linux 실기, 수일 방치, GPU reset, 실제 사용자 콘텐츠는 이번 측정에 없다.
