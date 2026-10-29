<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Flowmux 안정성 보강 및 부하 검사 — 2026-10-29

시작 코드는 `f9974e68`이며 main에 작업별 커밋을 쌓았다. macOS arm64,
Rust 1.99.0, GTK 4.22.3, libadwaita 1.9.0의 debug 빌드로 검사했다.
사용 중인 앱·터미널·에이전트 세션을 재시작하지 않았으며, 테스트 앱은 별도
XDG 디렉터리와 `/tmp`의 PID별 소켓을 사용했다. 설치된 앱은 교체하지 않았다.

최근 macOS 진단 폴더에는 이전 점검 이후의 새 Flowmux 크래시 보고가 없었다.
따라서 사용자가 겪은 자연 종료의 단일 원인을 확정한 결과가 아니다.
Agent Office의 할당·캐시·소유권, 브라우저 렌더러 완료 처리, 터미널·PTY,
IPC 용량 제한과 상태 저장소를 점검하고 아래 위험 경로를 방어했다.

## 수정

| 커밋 | 문제 및 변경 | 검증 |
| --- | --- | --- |
| `94179982` | Agent Office 캐시 제한은 확대 버퍼를 만든 뒤 적용되어 임시 할당량을 제한하지 못했다. 출력 및 중간 확대 버퍼를 각각 32MiB 이내로 제한한다. 배경 확대 실패 시 원본 밀도로 표시한다. | 수정 전 할당 제한 테스트 실패, 수정 후 통과. 중간 버퍼만 초과하는 경우와 배경 표시 유지도 검사했다. |
| `65c5b572` | 응답 없는 WebKit 렌더러의 JS 완료 콜백이 호출자·UI 참조를 기한 없이 보유할 수 있었다. 양 플랫폼에 동기 30초·비동기 300초의 호스트 기한을 적용하고, 정상 완료 순서와 단일 콜백 호출을 유지한다. | 정상 결과·오류·콜백 소멸·기한 만료·늦은 결과 단위 검사. 실제 WKWebView 무한 루프에서 GTK 타이머 동작, 기한 오류, 참조 해제와 창 종료를 검사했다. |
| `94ceb208` | 기존 임시 부하 스크립트를 재현 가능한 검사로 정리했다. 페이지 도착·출력 완료·IPC 오류를 검증하며, PTY 자식 종료 후 숫자 FD와 RSS·스레드·자식 수를 비교한다. | 성장 감지기의 정상/초과 self-check 및 GL/Cairo 실제 앱 검사. self-check를 공통 CI에 추가했다. |

32MiB는 개별 선택적 확대 버퍼의 한도이며 앱 전체 메모리 한도가 아니다.
JS 기한은 페이지 실행을 중단하거나 효과를 되돌리지 않는다. 늦은 결과는
다시 처리하지 않으며 자동 재시도도 하지 않는다.

## 실제 GUI 부하

`scripts/test-macos-stability.py --cycles 12 --leaks`를 GL과 Cairo에서 각각
실행했다. 각 실행은 터미널 240만 줄·OSC 제목 변경 2,400회, 6단 분할·리사이즈·
닫기 12회, canvas 페이지의 브라우저 생성/닫기 12회 및 탐색 72회,
에이전트 이벤트 2,400회, 알림 1,200건, 16개 스레드의 IPC 19,200회를 포함한다.
매 시나리오의 처음 3회는 워밍업으로 제외하고, 나머지 9회의 앞/뒤 각 3회
중앙값을 비교했다. 자식 프로세스의 종료 유예가 끝나기 전 값을 누수로
판정하지 않도록, 반복마다 원래 자식 수로 돌아오는지 먼저 기다린다.
기준 샘플부터 정착 샘플까지는 실행별 약 60초이며, 장시간 soak 검사를
대신하는 수치가 아니다.

| 항목 | GL (최종 빌드 재검사) | Cairo |
| --- | ---: | ---: |
| GUI RSS 시작 → 정착 | 188.9 → 220.5 MiB | 205.8 → 221.1 MiB |
| 시나리오별 최대 RSS 중앙값 증가 | 0.3 MiB | 1.8 MiB |
| 정착 FD 시작 → 종료 | 30 → 31 | 23 → 29 |
| 워밍업 후 시나리오 내 FD·자식 수 증가 | 0 | 0 |
| IPC p95 / 최대 | 1.56 / 225.69 ms | 1.65 / 150.55 ms |
| IPC 오류·GUI panic/CRITICAL/ERROR | 0 | 0 |

초기 브라우저 구동과 캐시 예열에 따른 증가는 위 전체 시작/종료 차이에
포함된다. RSS는 메모리 회귀 신호이며 누수 부재의 증명이 아니다.
초기 GL 실행도 같은 기준을 통과했으며, 표는 최종 실행 파일의 GL 재검사 값이다.
자식 수는 직접 자식만 센다. launchd가 관리하는 WKWebView 프로세스 메모리의
합계나 GPU 전체 메모리를 측정한 결과가 아니다.

Agent Office 전용 검사는 실제 표시된 창에 32명·4개 사무실을 넣고 상태 변경,
사무실 선택, 이벤트 시작, 네이티브 리사이즈, 퇴장·재등록과 열기/닫기를 12회
반복했다. 각 종료 후 world·DrawingArea·stage의 약한 참조와 스프라이트 캐시
0바이트를 검사했다. Cairo/GL 모두 통과했고 최대 조작 구간은 각각 98/106ms
(80ms 의도적 대기 포함)였다. Office 단위 검사 30개도 통과했다.

## 추가 회귀 검사

- pane tree 무작위 조작: 2개 seed × 5,000회, 불변식 통과.
- IPC 서버: 32클라이언트 × 200회, ID 혼선·프레이밍·대형 요청 방어 포함 4개 통과.
- 상태 저장소: 8개 task × 1,500회, 생성/삭제 후 ID 정리 포함 2개 통과.
- ignored stress: 깊은 pane tree, daemon 제목 20,000회·워크스페이스 규모 변화,
  OSC 약 54만 payload 처리 검사 통과.
- `cargo clippy --workspace --all-targets --locked -- -D warnings`: 통과.
- 격리된 XDG/runtime 환경의 `cargo test --locked` 전체 기본 headless crate 및
  doc-test: 종료 코드 0, 실패 0. GUI는 이 명령의 기본 대상에 포함되지 않는다.

최종 코드 `686afec1`의 원격 검증:

- [Sanitizers](https://github.com/flowmux-ai/flowmux/actions/runs/40788939484): 성공.
  Linux headless/Lottie ASan·LSan, GUI ASan, ThorVG ASan·UBSan·LSan 모두 통과.
- [Test](https://github.com/flowmux-ai/flowmux/actions/runs/40788939451): 성공.
  editor web, macOS 전체 gate, Linux 전체 테스트·커버리지 모두 통과.
  커버리지 line 85.19%, region 85.09%, function 84.15%로 기존 기준을 충족했다.
  예약/수동 전용 headless stress 작업은 push에서 skipped이며, 해당 스트레스
  검사는 위 로컬 실행에서 별도로 통과했다.

이 보고서의 후속 커밋은 문서만 추가한다. CI가 검증한 제품·테스트 코드는
`686afec1`이며, 문서만 바뀐 push는 기존 워크플로 경로 필터에서 제외된다.

최종 원격 macOS gate의 45개 단계가 모두 통과하여 `CI_GATE_EXIT=0`으로 완료됐다.
`MACOS_POINTER_CANCEL_RELEASE_OK`, `MACOS_EDITOR_RENDERER_RETRY_OK`,
`OFFICE_STABILITY_OK cycles=12 residents=32 worst_step_ms=136`,
`MACOS_BROWSER_SCRIPT_DEADLINE_OK`를 로그에서 확인했다. Office와 완료 콜백,
실제 WKWebView 기한 검사가 생략 없이 통과했다.

첫 원격 Test(`40785595268`)의 macOS 작업은 앱 테스트 전에
`ci-runner-tests`가 180초 제한을 넘어 실패했다. 로그에는 14개 검증 중
12개 진행 뒤 `CI_GATE_EXIT=124`가 기록됐다. 같은 검사는 로컬에서
130.95초에 14개 모두 통과했다. `304875d0`은 확장된 실패/marker 검증 행렬의
프로세스 기동 비용을 반영하여 이 단계만 300초로 조정했다. 제품 기능 테스트의
성공 기준은 바꾸지 않았다. 실패 로그를 보존한 뒤 구 커밋의 남은 CI 작업은
취소하고 새 커밋의 전체 워크플로를 실행했다.

다음 Test(`40786288413`)는 기존 WebDriver 취소 테스트에서 AppKit 전달이
끝난 직후 페이지의 `pointerup` 횟수를 0으로 읽어 실패했다. 로컬 기존 검사는
3회 통과하여 동일 실패를 재현하지 못했다. `686afec1`은 기존 5초 제한 안에서
페이지의 release 관측까지 기다리고, 정확히 1회·메서드 복구·버튼 해제 검증을
유지한다. 변경 후 실제 네이티브 검사 3회와 Clippy/format이 통과했다.
`304875d0`의 Sanitizers(`40786288401`)는 모든 작업이 통과했다.
같은 커밋의 Linux 전체 gate도 종료 코드 0으로 완료됐고, 새 Office mapped
반복 검사와 완료 콜백 검사가 실제 실행된 것을 로그에서 확인했다.

로컬 첫 전체 gate는 실행 중인 셸 파일의 수정으로 읽기 위치가 바뀌어
`_tab: command not found`로 중단됐다. 이 실행은 성공으로 세지 않았으며,
스크립트 내용을 메모리에 고정한 새 프로세스로 다시 실행했다. 두 번째
실행의 브라우저 입력 단계는 `browser did not render a stable pointer target`로
실패했고, 같은 단계의 단독 재실행도 같은 결과였다. macOS가
`CGSSessionScreenIsLocked=Yes`를 보고하는 것을 확인했다. 잠금 상태의 로컬
GUI 결과를 성공으로 처리하지 않았다. 잠금 확인 이후 로컬 GUI 반복은 중단했고,
이후 OS에서 잠금 해제를 확인한 뒤 전체 gate를 새로 실행했다.
새 실행에서는 브라우저 전체 검사와 기본 네이티브 앱 검사가 통과했으나,
editor-keyboard의 `native keyboard window active` 대기에서 실패했다.
당시 OS가 다시 `CGSSessionScreenIsLocked=Yes`를 보고했다. 화면 잠금 중
입력 조건을 우회하지 않았으며, 이 로컬 전체 gate도 성공으로 세지 않는다.

## 메모리 진단 및 범위

GL/Cairo 실행의 `leaks`는 각각 418건, 36,400바이트를 보고했다.
XPC 연결 순환 참조가 포함되며, 이 수치를 Flowmux의 누수 0으로 해석하지 않는다.
`MallocStackLogging=1`로 터미널·브라우저를 각각 9회 반복한 별도 실행도
성장 기준을 통과했다. 이 실행의 417건·46,928바이트는 할당 스택상
Apple AppIntents/LinkServices의 NSXPCConnection 순환 참조 416건과,
`dyld → glib_init → g_quark_init → g_malloc`의 시작 시점 할당 1건(20KiB)으로
분류됐다. Flowmux의 반복 조작 콜백이 원인인 스택은 이 출력에서 찾지 못했다.
이는 운영체제/GLib 할당을 앱 수정 대상으로 삼을 근거가 아니며, 검사 대상의
보안 제한으로 `leaks`가 일부 메모리 내용에 접근하지 못했다는 경고도 있었다.
스택 로깅 실행은 측정 오버헤드가 있으므로 위 성능 표에는 포함하지 않는다.

원시 로그, JSONL 자원 샘플, `leaks` 출력은 로컬
`target/stability-20261029/`에 보존했다. 이 폴더는 Git 추적 대상이 아니며
`cargo clean`으로 제거될 수 있다. 새 스크립트와 CI 검사는 저장소에 남는다.
GUI ASan은 기존 CI 설정에서 `detect_leaks=0`이므로 GUI 전체의 누수 부재를
증명하는 검사가 아니다.
수일 방치, OS 전체 OOM/GPU reset, 실제 사용자 페이지·세션에 대한 검사는
이번 측정에 포함되지 않는다.
