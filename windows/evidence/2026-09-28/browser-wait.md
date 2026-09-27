<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Windows browser condition waits

이번 단계는 독립 windows/에만 적용하며 Linux/macOS 코드와 root manifest/lock을
수정하지 않는다. 이전 단계 79af440 뒤 browser wait를 추가했다. 전체 114개 기능·13개
게이트 범위는 유지하고 B07/G09는 partial이다. Browser CLI는 20개 operation을 제공한다.

`--selector`, `--text`, `--url`, `--ready-state`, `--js` 중 정확히 하나를 받는다.
Selector는 top-document 내 존재 여부(숨김 요소 포함), text는 rendered innerText의
정확한 substring, URL은 percent-encoded location.href의 substring이다. Text를 Unicode
정규화하지 않는다. Ready-state는 loading/interactive/complete만 허용한다. Ref token을
selector로 해석하지 않으며 iframe/shadow tree를 별도로 탐색하지 않는다.

일반 출력은 true/false이며 timeout false도 정상 종료 코드 0이다. JSON은 result와
처음 지정한 surface UUID를 반환한다. Invalid selector/options, script 오류/Promise,
닫힌 browser와 callback 오류는 오류로 종료한다. 대기 중 pane의 활성 탭이 바뀌어도
처음 지정한 surface를 유지한다. Native navigation epoch가 다른 callback은 성공으로
사용하지 않고 다음 검사에서 새 문서를 확인한다. Browser가 닫히면 pending wait를
제거하고 오류를 반환한다. Wait는 focus를 요청하거나 page input 이벤트를 발생시키지 않는다.

GUI를 차단하는 sleep 대신 별도 Win32 timer 2를 사용한다. 기존 terminal/state용 1초
timer 1은 그대로 유지한다. 대기별 하나의 WebView callback만 실행 중일 수 있고, callback
완료 후 poll interval을 두고 다음 검사를 시작한다. 최초에는 즉시 검사한다. Native
navigation이 진행 중이면 loading/interactive 조건 외에는 완료될 때까지 검사를 미룬다.
따라서 로딩 중에만 잠시 존재하는 요소나 polling 사이의 짧은 ready-state는 놓칠 수 있다.
목표 상태의 실시간 이벤트 구독이나 페이지 안정화 보장을 제공한다고 주장하지 않는다.

기본 timeout/poll은 기존 CLI와 같은 5000/100 ms이다. 허용 범위는 각각 1..120000과
1..10000 ms이며 OS timer 해상도 및 renderer scheduling 때문에 실제 polling은 더 느릴
수 있다. Window당 대기 최대 8개, predicate 64 KiB 및 인코딩된 script 128 KiB,
callback 결과 16 KiB, 단일 callback 12초 제한을 둔다. 전체 wait deadline을 timer와
callback 완료 양쪽에서 확인한다. Deadline 뒤 도착한 true 결과는 성공으로 바뀌지 않는다.
Timeout은 이미 시작된 JS를 취소하거나 실행 부작용을 되돌리지 않는다.

기존 IPC server/client command budget 15/25초보다 긴 대기를 위해 wait 명령에만
`max(기존 제한, timeout + 5/10초)`를 사용한다. CLI 및 raw IPC 모두 동일한 option/script
검증을 적용하고 bounded timeout이 검증된 뒤 dispatch한다. 일반 command, request/read/
write/close 및 connection 한도는 변경하지 않는다. 연결을 끊은 CLI의 대기는 현재 요청한
deadline까지 남을 수 있다. Prompt disconnect cancellation 및 모든 shutdown/close/queue/
navigation race 검증은 미완료다. Predicate는 반복 평가되므로 읽기 전용으로 작성해야 한다.

JS는 expression, function 또는 function body를 지원한다. Expression compile이 SyntaxError일
때만 body form을 compile한다. Runtime exception을 catch한 뒤 body로 재실행하지 않는다.
따라서 한 poll에서 사용자 코드를 예기치 않게 두 번 실행하지 않는다. Function 반환값을
호출한 뒤 Boolean으로 판정하되 thenable/Promise는 truthy로 처리하지 않고 거부한다.
Async predicates 및 JS 전체 실행 시간/메모리 sandbox는 미지원이다.

`verify-browser-wait.ps1`은 isolated config/state의 소유 숨김 host, 명시적 소유 IPC pipe,
loopback fixture만 사용한다. 테스트 전/후 host와 WebView/chrome이 숨겨져 있고 foreground가
아님을 조회한다. 새 streaming fixture는 HTML body를 나눠 보내고 defer script 응답을
지연시켜 loading/interactive/complete를 실제 WebView2에서 관찰한다. OS 키보드/마우스,
클립보드, 사용자 flowmux 창 및 외부 사이트를 조작하지 않는다.

최종 숨김 검증은 다음 6개군이다.

- 다섯 조건 및 JS expression/function/body, plain/JSON, 정확한 한글·분해형 자모·결합
  문자·emoji, NFC와 NFD의 구별, 읽기 조건에 의한 DOM/input 변화 없음.
- 지연 DOM 추가, streaming document의 loading/interactive/complete 및 defer 완료.
- timeout false, invalid raw input/CSS, runtime exception 시 정확히 1회 실행, Promise
  거부 및 100 ms deadline 뒤 350 ms에 돌아오는 true callback 무시와 후속 요청 정상 동작.
- Redirect를 포함한 문서 교체, 처음 대기한 탭이 숨겨져도 해당 surface 유지, close 취소.
  최종 fixture는 대기 시작 handshake와 숨김 뒤 CLI 미완료 상태로 순서를 확인한다.
- 대기 8개 동안 tree/eval 응답 및 기존 terminal PID 유지, 9번째 거부, 완료 후 용량 회복.
- 약 27초 동안 기다려 기본 server 15초와 client 25초 제한을 넘고도 같은 요청이 성공.

첫 cross-check에서 nested module의 항목 visibility 두 곳을 고쳤고, Clippy가 지적한
조건식 한 곳을 단순화했다. 첫 native 6개군도 통과했으며 최종 fixture에서 late callback과
숨김 순서 검증을 강화했다. Physical IME, glyph/후보창, desktop UI/DPI/접근성 검증은
수행하지 않았다. DOM actions/screenshots 및 전체 Windows/브라우저 기능 완료가 아니다.

최종 build/test 수와 artifact hash, 기존 DOM/keys/IPC 회귀 결과 및 소유 process 정리는
동일 디렉터리의 browser-wait 이름을 포함한 별도 증거 파일에 기록한다. NSIS는 package
생성만 하며 설치 프로그램을 실행하지 않는다. 기존 Linux/macOS 전체 live 회귀를 이
Windows 전용 검사 결과로 대신하지 않는다.

최종 Linux에서 실행한 독립 Windows crate 94개와 숨김 실제 Windows Rust 121개가 통과했다.
Wait 6개군, 기존 DOM 6개군, named-key 7개군, IPC limits 3개군이 최종 debug 바이너리에서
통과했다. 일반 CLI의 응답 제한은 25.12초에 종료됐고, 긴 wait는 27.03초에 성공했다.
Debug/release build, release Clippy all-targets -D warnings, fmt 및 NSIS 생성도 통과했다.
artifacts-browser-wait.json에 debug/release/installer 7개 파일의 hash를 기록했다.
cleanup-browser-wait.json은 소유 PID 23개와 fixture 경로 5개를 읽기 전용으로 조회해
남은 일치 프로세스가 없음을 확인한다. 사용자 WSL flowmux PID 787은 계속 실행 중이다.
