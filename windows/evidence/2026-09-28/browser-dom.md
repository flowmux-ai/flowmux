<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Windows browser DOM snapshots and read-only queries

이번 변경은 독립 `windows/`에만 적용한다. 기존 Linux/macOS 코드와 root manifest/lock은
수정하지 않는다. 기존 `flowmux-browser`의 headless snapshot/ref 타입을 읽기 전용 path
의존성으로 사용하며 Windows lock만 갱신한다. 114개 기능·13개 게이트 범위는 유지하고
B03/B06/G09는 partial로 기록한다.

`snapshot`, `text`, `value`, `attr`, `is-visible`, `is-enabled`, `is-checked`, `count`를
추가해 browser CLI는 19개 operation을 지원한다. JSON snapshot은 기존 markdown/refs/page
구조에 surface, snapshot_id, dom_revision, node_count, omitted_refs, frame_count를 추가한다.
일반 출력은 snapshot Markdown 또는 조회 문자열/boolean/count이며 JSON은 구조를 유지한다.
페이지의 `error: not found` 문자열도 오류 envelope가 아닌 정상 데이터다.

Ref는 안정적인 browser surface UUID와 연결하며 window process 전체에서 단조 증가하는 eN
토큰을 사용한다. `eN`과 `@eN`을 허용하고 ref 조회에 raw CSS를 허용하지 않는다. 새 snapshot은
이전 refs를 먼저 버린다. 실패한 snapshot도 이전 refs를 되살리지 않는다. 탭 숨김/전환,
탐색/reload, history URL 변경, DOM의 tree/attribute/text 변경 뒤 기존 refs를 거부한다.
동일 visible surface를 다른 pane으로 이동하면 refs와 기존 terminal PID가 유지된다.
동시 snapshot의 늦은 callback은 snapshot UUID로 거부하고 navigation ID 및 deadline도
다시 확인한다. 모든 timeout/close/navigation 경쟁 조건을 검증한 것은 아니다.

Snapshot은 DOM에 추적 속성을 쓰거나 input/focus 이벤트를 발생시키지 않는다. 무작위 이름의
non-enumerable page property 하나와 MutationObserver로 document revision을 관리한다.
unique ID selector 또는 최대 64단계 nth-of-type 경로가 정확히 원래 요소 하나를 가리키는지
확인한다. 중복 ID/deep selector를 검증한다. Unique selector를 구성하지 못한 수는
omitted_refs로 보고한다. DOM property/CSSOM만 바뀌고 tree/attribute/text가 그대로인 경우
기존 refs로 live value/state를 조회할 수 있다.

Snapshot의 이름/제목/본문은 각각 최대 120/1000/4000 UTF-16 code unit의 짧은 문구다.
Intl.Segmenter의 grapheme 경계에서 잘라 분해형 자모·결합 악센트·ZWJ 이모지 묶음을
중간에서 끊지 않는다. 이 API가 없는 오래된 runtime의 fallback은 surrogate pair만
보호한다. Unicode 정규화는 하지 않는다. Full text/value/attr 조회는 결과 한도 내에서
원문을 보존한다. 단일행 input이 LF를 제거하고 textarea가 유지하는 등 DOM 자체의 값
규칙은 따른다. Snapshot name은 label 선택/줄바꿈 정리/공백 trim 때문에 원문의 대체물이
아니다. `text`는 rendered innerText이며 textContent/raw HTML 추출을 뜻하지 않는다.

숨김 fixture는 완성형 한글, 분해형 자모, e+combining accent, emoji, 따옴표, 역슬래시 및
줄바꿈을 StringComparison.Ordinal로 비교한다. 이름 119자/본문 3999자 뒤의 한글 자모,
결합 문자와 emoji/ZWJ 경계도 확인한다. 이는 문자열 처리 검증이다. 실제 Windows IME의
조합/확정/취소/backspace, 후보창, 글꼴 glyph, 주소창 키보드 입력은 여전히 미검증이다.

Snapshot은 top-document 요소 10,000개, refs 2,048개, JSON 결과 1 MiB를 넘으면 거부한다.
전체 조회는 128 KiB, CSS selector는 4,096 UTF-8 bytes, attribute 이름은 256 bytes로
제한한다. 기존 script source 128 KiB/동시 pending 16개/callback 12초 제한도 적용한다.
크기 제한 뒤 새 페이지에서 정상 snapshot이 복구되는지 확인한다. 이 제한은 페이지 JS의
실행 시간/메모리 전체를 제한하거나 timeout 이후 이미 실행한 코드를 취소하지 않는다.

현재 frame/shadow tree 내부는 탐색하지 않으며 frame_count는 바깥 document의 frame 수다.
ARIA name/role 추출은 일부 규칙만 처리한다. CSS visibility는 크기 및 ancestor style
검사이며 가림/클리핑/viewport hit-test 또는 완전한 접근성 tree를 뜻하지 않는다.
checked는 native checked property, enabled는 disabled/inert/자체 aria-disabled 기준이다.
Custom ARIA widget 상태와 모든 접근성 의미는 미완료다. Page JS는 신뢰하지 않으며 page가
DOM/JS API를 덮어쓰는 공격에 대한 격리 보장을 이 snapshot 기능으로 주장하지 않는다.
DOM actions, waits, screenshot, async eval과 전체 browser UI parity도 미완료다.

`verify-browser-dom.ps1`은 isolated config/state의 소유 debug host와 loopback HTTP fixture만
사용한다. HWND/WebView/chrome의 숨김 상태와 foreground가 아님을 조회하고 명시적 소유
pipe로 통신한다. OS 키보드/마우스/클립보드, 사용자 창, 외부 사이트와 installer는 조작하지
않는다. Native Rust test executable도 숨기고 stdout/stderr를 파일로 보낸다.

처음 두 fixture 실행에서 단일행 input의 LF 제거를 원문 전체와 비교한 오류와 CSP 때문에
inline hidden style이 적용되지 않은 오류가 발견됐다. Input의 DOM 규칙을 기대값에 반영하고
fixture에 style-src를 허용해 고쳤다. 실패 기록은 각각
native-browser-dom-failed-fixture-newline.json / native-browser-dom-failed-fixture-csp.json에
보존한다. Product 오류로 숨기거나 성공 결과에서 삭제하지 않았다.

최종 debug/release build, release Clippy all-targets -D warnings, cargo fmt가 통과했다.
Linux에서 독립 Windows crate 92개와 숨김 Windows Rust 119개가 통과했다. 최종 native DOM
6개군, 기존 browser foundation 6개군 및 terminal named-key 7개군의 숨김 live 검증이
통과했다. cleanup-browser-dom.json은 기록된 소유 PID 17개 및 fixture 경로 6개에 대해
남은 일치 프로세스가 없음을 읽기 전용으로 확인한다. 사용자 WSL flowmux PID 787은
계속 실행 중이다. regressions-browser-dom.json에 검사 결과를 모았다. Terminal frontend는 이번에
변경하지 않아 기존 56개 결과를 재실행 결과로 주장하지 않는다. NSIS 설치 파일 생성은
통과했으며 설치는 실행하지 않았다. artifacts-browser-dom.json은 debug/release 및
installer 7개 파일의 SHA-256이다. 기존 Linux/macOS 전체 live 회귀와 실제 IME 완료를
이 결과로 대신하지 않는다.
