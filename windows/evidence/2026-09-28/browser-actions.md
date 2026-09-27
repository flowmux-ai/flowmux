<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Windows ref-based browser actions

이번 단계는 Windows의 click/dblclick/hover/focus/blur/scroll/fill/select/check/uncheck
10개 명령을 추가한다. 변경은 독립 windows/에만 적용하며 Linux/macOS 및 shared Rust
소스, root manifest/lock은 수정하지 않는다. 전체 114개 기능·13개 게이트 범위를 유지한다.
현재 Windows browser 명령은 30개지만 status/stop/zoom 추가분을 포함하므로 기존 플랫폼의
30개 명령과 동일한 범위를 뜻하지 않는다. Type/press/screenshot은 미완료이며 B04/B05/G09도
partial로 남긴다.

대상은 최신 snapshot의 eN/@eN ref다. Ref binding은 surface별 store를 사용하며 JS가
DOM revision, URL, unique selector를 검사한 뒤 원래 요소 하나를 조작한다. Snapshot이나
탭/문서 변경 뒤 이전 ref로 새 요소를 조작하지 않는다. Callback은 기존 navigation epoch,
closed surface, size/deadline 검사를 따른다. Snapshot과 추가 action은 같은 surface의
action callback이 대기 중일 때 거부한다. Browser status의 action_pending으로 이 상태를
읽을 수 있다. 일반 query/다른 surface/terminal은 이 action admission에 포함하지 않는다.

정상 응답은 plain ok 또는 JSON ok:true/surface다. 이는 DOM 코드의 동기 실행 결과이며
사이트 listener의 비동기 작업이나 탐색 완료를 보장하지 않는다. 클릭이 탐색을 시작하면
실제로 적용됐어도 callback은 문서 변경 오류를 반환할 수 있다. Dispatch 뒤의 오류에는
action may have executed (not retried)를 붙인다. CLI나 host는 mutation을 자동 재전송하지
않는다. 사용자가 명시적으로 다시 요청하는 것까지 막지는 않는다. DOM/snapshot이 그대로면
같은 ref로 반복 클릭하거나 이미 만족된 값/체크 상태를 확인할 수 있다. 실제 tree/attribute/
text 변경은 다음 ref 실행 시 거부된다. 초기 구현은 action마다 ref를 소모했지만 최종 구현은
기존 ref 계약에 맞춰 DOM이 바뀌지 않은 명시적 반복을 허용하며 최종 검증도 이 계약을 따른다.

Fill은 input/textarea의 native prototype value setter를 호출한다. Instance의 value setter를
우회해 일반적인 framework 추적과 연동할 여지를 보존하지만 전체 framework 호환성을 입증한
것은 아니다. 값이 다르면 cancelable beforeinput(insertReplacementText, isComposing:false)을
먼저 전송한다. 취소됐거나 listener가 요소를 교체/비활성/readonly로 바꾸면 적용을 중단한다.
값이 바뀐 경우 input/change를 보낸다. 같은 값에는 이벤트가 없으며 empty string도 지원한다.
File/checkbox/radio/button/hidden input 및 contenteditable은 fill 대상으로 거부한다.
Disabled fieldset, inert 및 aria-disabled 조상도 검사한다. OS 입력/DOM focus/keyboard 또는
composition 이벤트를 합성해 한글 입력을 흉내 내지 않는다.

완성형 한글, 분해형 자모, combining accent, emoji, quote/backslash/newline과 script처럼
생긴 텍스트를 StringComparison.Ordinal로 검사한다. Unicode 정규화 없이 JSON data로 넘긴다.
단일행 input의 LF 제거와 textarea의 LF 유지 등 native DOM value 규칙은 그대로 적용한다.
20,000자 한글(60,000 UTF-8 bytes)도 raw IPC로 정확히 전달됐다. Value는 64 KiB UTF-8,
인코딩된 action script는 128 KiB 한도다. Windows CLI의 전체 command-line 길이 제한이 더
작을 수 있다. 과대 입력/인코딩 검증 실패는 dispatch 전에 거부하고 현재 ref/value를 유지한다.
실제 IME 조합/확정/후보창/삭제/취소/글꼴 렌더링 검증을 이 문자열 시험으로 대신하지 않는다.

Select는 option의 정확한 value를 먼저 찾고 없으면 trim한 label text를 찾는다. Disabled
option/optgroup, 없는 항목, select가 아닌 요소를 거부한다. Multiple select는 기존 선택을
유지하며 지정 항목을 추가한다. Check는 checkbox/radio를 지원하고 radio group의 native
상호 배타적 checked 동작을 따른다. Uncheck는 checkbox만 허용한다. 값 변경에만 input/change를
보내며 이미 만족된 상태에서는 이벤트를 다시 만들지 않는다.

Click은 HTMLElement.click()을 호출한다. Dblclick은 합성 이벤트 하나이며 두 click/default
selection을 재현하지 않는다. Hover는 mouseenter/mouseover를 보내며 OS 포인터나 CSS :hover를
바꾸지 않는다. Scroll은 요소를 가운데로 스크롤하고 viewport offset을 즉시 적용한다.
Trusted input, native hit testing/occlusion, physical pointer/keyboard 동작, accessibility
widget 의미를 완전히 재현하지 않는다. Focus/blur 코드는 DOM 메서드를 사용하지만 background
host에서는 dispatch 전에 거부한다. 이 두 명령의 실제 focus 효과/IME 연동은 미검증이다.

첫 debug doctor 실행에서 main stack overflow를 재현했다. Host는 시작되지 않았다. Windows
기본 PE stack reserve는 1 MiB였고 unoptimized clap builder의 큰 stack frame이 원인이었다.
추가 action의 인자를 TargetArgs/ValueArgs/ScrollArgs로 공유해 builder를 분리했다. 스택 크기,
기존 CLI/JSON 형태를 바꾸지 않았으며 debug의 GUI/console/control 실행 파일 세 종류 모두
숨김 doctor 실행과 실제 action CLI 경로가 통과했다. Release 세 진입점도 별도로 확인한다.
실패 로그는 native-browser-actions-failed-debug-stack.txt에 보존한다.

두 번째 실행부터 선택 항목이 없다는 오류를 조사했다. Fixture HTML이 새 option 시작 태그
대신 value attribute가 붙은 닫기 태그를 포함해 브라우저가 해당 항목을 만들지 않았다.
초기 DOM, DOMParser와 Option constructor 결과를 비교해 fixture 오류로 분리하고 마크업을
수정했다. 실패 JSON 네 개도 보존한다. Product select의 탐색/비교 로직을 우회하지 않았다.

최종 숨김 action 검증은 8개군이다: Unicode fill/events/native setter/empty/idempotence;
disabled/readonly/file/cancel/replacement/reentrant disable; select value/label/multiple;
checkbox/radio; explicit click repeat/dblclick/hover/scroll/stale replacement refs; background
focus guard/cross-tab/navigation/terminal identity; pending-action status/snapshot barrier;
large Unicode/raw/script bounds와 복구. 테스트는 소유 hidden host, isolated config/state,
명시적 pipe 및 loopback fixture만 사용한다. 사용자의 창, OS keyboard/mouse/clipboard,
외부 사이트는 조작하지 않는다. 실패/성공 host는 소유 프로세스만 정상 종료한다.

최종 빌드·Rust 검사, 기존 DOM/wait/keys live 회귀, release entrypoints, 설치 패키지 hash와
정리 결과는 같은 디렉터리의 browser-actions 증거에 기록한다. NSIS는 생성만 하며 installer를
실행하지 않는다. Complete timeout/close/navigation race, 실제 focus/IME/DPI/접근성 및 기존
Linux/macOS 전체 live 회귀를 완료했다고 주장하지 않는다.

최종 Linux에서 독립 Windows crate 95개와 숨김 실제 Windows Rust 122개가 통과했다.
Action 8개군, DOM 6개군, wait 6개군 및 named-key 7개군이 최종 debug 바이너리에서
통과했다. Debug/release build, release Clippy all-targets -D warnings, fmt, NSIS 생성 및
release GUI/console/control 세 진입점도 통과했다. 설치는 실행하지 않았다.
artifacts-browser-actions.json은 debug/release/installer 7개 파일의 hash다.
cleanup-browser-actions.json은 기록된 소유 PID 56개 및 fixture 경로 12개를 읽기 전용으로
조회해 남은 일치 프로세스가 없음을 확인한다. 사용자 WSL flowmux PID 787은 계속 실행 중이다.
