<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# 테스트 시스템 점검 및 회귀 보강

2026-09-06. Linux의 실제 GTK/VTE/WebKit 경로를 포함해 검증한다.
테스트 개수는 커버리지 비율이 아니다. 아래는 확인한 행동과 실행 경계이며,
모든 입력·스케줄·플랫폼에서 회귀가 불가능하다는 의미는 아니다.

## 구조와 판단

| 계층 | 기존 검증과 이번 판단 |
|---|---|
| 도메인 (`flowmux-core`) | pane 분할·닫기·tab 추가/선택/이동·제목 규칙. 실제 트리 불변식을 검사하는 2개 seed × 5,000회 random walk를 기본 실행에 포함 |
| 저장 (`flowmux-state`, `flowmux-daemon`) | 실파일 저장/복구, 스키마 기본값·신버전 거부, 창 소유권, 프로세스 잠금, 다중 창 병합은 유지. 동시 mutator·생성/삭제 churn을 기본 실행에 포함 |
| IPC/CLI | 소켓 framing·크기 제한·부분 EOF·비요청 payload 거부, wire 이름/default, CLI 파싱, hook 설치·실행, tmux 호환 E2E 유지. 서버 동시 응답·잘못된 JSON 이후 복구·hostile peer 격리 기본 실행. 클라이언트 종료/손상 응답 및 동일 연결 동시 호출 보강 |
| 터미널·알림 | 실제 PTY 입출력·프로세스 정리, VTE 화면, 키 모드, OSC 스트림 파서 유지. OSC 초과 입력 이후 복구와 불규칙 chunk 경계 기본 실행 |
| 브라우저 | ref scope·북마크·프로필 단위 검사는 유지. mock 자체와 JS 소스 모양 검사를 실제 WebKit 행동으로 교체 |
| 에디터 | 실제 파일 버전·외부 변경/삭제·저장 충돌·권한·symlink·BOM/CRLF·경로 제한, 복구와 검색, 자산 HTTP 서버 검사 유지. 웹 쪽은 Node 내장 test runner 사용 |
| Markdown | CommonMark 652개 + GFM 672개 전체 예제에 정답 비교 적용. 빈 문자열 여부만 검사하던 상태를 해소 |
| 설정·외부 연동 | JSONC·기본값·설정 저장·키바인딩·SQLite 쿠키 fixture·로컬 Git/worktree·PID 검사 유지. 계정/외부 서비스 성공 경로를 로컬 fixture 통과로 주장하지 않음 |
| GUI | 실제 GTK 위젯 검사와 순수 로직 검사 혼재. GTK 초기화 실패를 조용한 성공으로 만드는 중복 return 제거. overview 포커스/미리보기와 런타임 없는 알림 경로 보강 |
| CI | `cargo test --workspace --locked`를 Xvfb+D-Bus에서 실행. 웹 에디터 test/build/verify와 committed dist 비교. GUI E2E도 workspace 실행에 자동 포함. Rust 계측 테스트 단계에 30분 timeout 적용 |

## 삭제·교체

- `flowmux-browser/controller.rs`: 제품을 호출하지 않고 `MockBrowser` 자신의
  구현만 검사하던 6개 테스트와 mock 구현 삭제. 에러 메시지 계약 검사는 유지.
  불필요해진 tokio dev dependency도 제거.
- `flowmux-browser/scripts.rs`: 함수명·문자열·단순 괄호 수를 검사하던 22개
  테스트 삭제. 입력 escaping의 직접적인 계약 검사는 유지.
- daemon의 같은 제목 burst 전후 시간 비율 검사는 서로 같은 작업을 비교해
  fast path를 입증하지 못하므로 삭제. 실제 제목 갱신 테스트에 반복 갱신의
  `None` 응답과 제목 보존을 추가.
- IPC garbage mutation 검사는 `serde_json`의 성공/실패를 모두 허용해 제품의
  거부 행동을 검증하지 못하므로 삭제. 필드 보존 sample 검사는
  `protocol_samples.rs`로 유지하되, 모든 variant의 호환성을 보장한다는 설명 제거.
- 중복된 IPC Event 거부 검사는 삭제. `server.rs`의 실제 소켓 검사 유지.
- daemon의 중복 다중 창 파일 저장 stress 검사는 삭제.
  `flowmux-state/tests/multi_window_merge.rs`와 `cross_process_lock.rs`가
  병합·소유권·별도 프로세스 잠금 행동을 기본 실행에서 검증한다.
- assertion 없이 소스만 출력하던 CLI `render_dump` 테스트 삭제.
- overview의 서로 다른 시점 전체 픽셀 비교를 고정된 pane 표식의 미리보기
  포함 여부로 교체. shell 출력·커서 깜빡임과 관계없이 잘못된 pane/빈 이미지를 검출.

## 새 행동 검증과 발견한 결함

`ui/browser_behavior_tests.rs`는 실제 `BrowserPane`에서 production JavaScript를
실행한다. 외부 웹사이트나 별도 Chromium을 사용하지 않는다.

- snapshot 접근 가능한 이름·특수/중복 ID selector, MutationObserver와 HTML
  비교를 통한 DOM 비변경, selector가 정확히 한 요소를 찾고 클릭하는지 검사
- 한국어·emoji·따옴표·개행·제어 문자 보존, 입력을 통한 JS 삽입 방지,
  input/change/keyboard 이벤트 내용과 순서
- 값/라벨에 의한 select, 없는 옵션 선택 시 기존 값 유지
- checkbox 반복 실행 멱등성, radio 배타성과 uncheck 거부
- pointer/focus/blur, text/value/attribute/count, 숨김·비활성·투명·크기 0,
  실제 스크롤, 없는 요소의 오류
- 문서 이동/back/forward/reload와 history 분기, 탐색 시 snapshot ref 무효화

이 검사에서 Linux WebKit의 링크 클릭 등 native 탐색이 이전 ref를 남기는
결함을 발견했다. `LoadEvent::Started`에서 해당 scope를 지우도록 수정했다.
`tests/browser_navigation.rs`는 별도 production GUI 프로세스와 실제 IPC를 통해
이를 재검증한다. 두 문서에 같은 ID를 두고 이전 ref는 거부되는지,
새 snapshot의 ref로만 새 요소를 클릭하고 텍스트를 바꿀 수 있는지 확인한다.
테스트 HOME/XDG/소켓을 격리하고 GUI를 종료·회수한다.

overview 전환은 GTK focus 이벤트에만 의존해 선택한 workspace의 pane 상태가
갱신되지 않았다. `focus_first_leaf_of`가 기존 `focus_pane`을 재사용하도록 해
상태와 실제 포커스를 함께 갱신했다. 실제 GTK 창에서 overview 선택·키보드
이동·전환 애니메이션·상태·해제/정리를 검증한다. 추가로 별도 production GUI를
실행해 Ctrl+Alt+K → Right/Enter → 두 번째 workspace 선택 후 입력한 표식이
두 번째 터미널에만 나타나는 것을 확인했다.

GUI suite에는 테스트가 통과하면서도 detached GLib 작업에서 zbus가
`no reactor running`으로 panic하는 문제가 있었다. 스택을 추적해 Tokio handle이
없는 컨트롤러의 desktop delivery 시도로 확인했다. 이 컨트롤러는 로컬 알림
상태만 사용하도록 하고, badge publish/close가 외부 연결이나 busy 상태를 남기지
않는 검사를 추가했다. production 시작 경로는 Tokio handle을 전달한다.

Markdown은 upstream fixture를 그대로 유지하고, 활성화된 확장에 따른 79개
고유 입력의 차이를 이유와 함께 별도 fixture로 기록한다. heading anchor,
front matter, code metadata, tagfilter, underline, task list, autolink,
구 GFM과 현대 CommonMark의 중첩 강조 차이이다. code metadata의 두 attribute
순서만 대안으로 허용한다. 원인 없이 현재 출력을 자동 승인하는 방식은 사용하지
않는다. 구체적인 출처·라이선스·갱신 규칙은 fixture README에 있다.

## 앞선 회귀 보강 (`7779c0c`)의 실행 증거

GUI 의존성(GTK4/libadwaita/VTE/WebKitGTK), D-Bus, Xvfb와 image viewer용 ThorVG가
필요하다. 단순 `cargo test`는 workspace default-members 때문에 GUI를 제외한다.

```bash
GDK_BACKEND=x11 GTK_A11Y=none RUST_BACKTRACE=1 \
  xvfb-run -a dbus-run-session -- cargo test --workspace --locked --no-fail-fast
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo fmt --all -- --check
```

`editor/flowmux-editor-web`에서:

```bash
npm ci
npm test
npm run build
npm run verify
```

저장소 루트에서 `git diff --exit-code -- editor/flowmux-editor-web/dist`로
빌드 결과가 committed assets와 일치하는지도 확인한다.

- WebKit 집중 실행: 14 passed. GUI/IPC navigation E2E: 1 passed.
- overview 집중 실행: 1 passed. 별도 GUI+실제 키보드 입력 시나리오도 통과.
- 웹 에디터: 33 passed, 빌드 성공, 10 assets 검증 및 dist 일치.
- 중간 전체 실행: 1,630 passed, 0 failed, 6 ignored. 이 실행에서 detached
  zbus panic을 발견했으므로 최종 정상 실행의 근거로 사용하지 않음.
- 최종 전체 실행: **1,633 passed, 0 failed, 4 ignored**. GUI 658개와 별도
  GUI/IPC E2E 1개 포함. detached panic 및 `no reactor running` 발생 없음.
  로그: `/tmp/flowmux-verified-suite.log`.
- ignored stress 4개도 debug 빌드에서 별도 실행해 전부 통과.
  `cargo test -p flowmux-core -p flowmux-daemon -p flowmux-notify --locked -- --ignored --nocapture`.
  로그: `/tmp/flowmux-stress-checks.log`. release 성능 측정 결과로 해석하지 않는다.
- 최종 `cargo clippy --workspace --all-targets --locked -- -D warnings`,
  `cargo fmt --all -- --check`, `git diff --check`: 통과.
  Clippy 로그: `/tmp/flowmux-verified-clippy.log`.
- production overview 키보드 확인: `/tmp/flowmux-overview-live.log`.
  GUI 프로세스를 격리 실행하고 종료했으며 기존 사용자 창은 유지했다.

## 검증 경계

- 남긴 ignored 4개는 제목 처리량/스케일링 2개, 16 MiB OSC 처리량 1개,
  깊이 1,000 pane tree probe 1개이다. 일반 행동 회귀와 분리한 stress 검사이며,
  `cargo test -p <crate> --release -- --ignored --nocapture`로 실행한다.
- Linux X11/Xvfb에서 실행했다. Wayland compositor, macOS WKWebView, 실제
  데스크톱 알림 서비스의 UI/권한 및 외부 계정 연결은 이 결과에 포함되지 않는다.
  macOS에도 같은 ref 누락이 있어 `didStartProvisionalNavigation` delegate에서
  scope를 지우도록 대응했다. 사용 중인 objc2-web-kit 0.3.2의 callback 시그니처와
  기능 flag는 확인했다. 이 Linux 호스트에는 macOS GUI/SDK가 없어 해당 변경의
  macOS 컴파일 및 실제 WKWebView 실행은 검증하지 못했다.
- 생성 CSS의 문자열 검사는 생성 결과 계약을 확인할 뿐, 모든 테마/배율의 픽셀
  가독성을 입증하지 않는다. overview는 실제 표식과 레이아웃·포커스를 별도로 검사한다.
- 어떤 커버리지 수치도 모든 production 분기와 회귀 검출을 보장하지 않는다.
  새 기능/버그가 추가되면 해당 사용자 흐름과 실패 경로를 기존 계층에 추가해야 한다.

## 코드 커버리지 후속 점검

판정: **모든 지원 기능의 회귀를 잡기에 충분하다고 볼 수 없다.** 도메인·저장·
프로토콜 검증에 비해 실제 화면, 프로세스 시작, GUI 명령 연결의 빈틈이 크다.
단위 테스트 통과와 사용자 흐름 전체의 통과를 구분해야 한다.

Rust 1.95.0, cargo-llvm-cov 0.6.24, Linux X11/Xvfb에서 측정했다.
기존 테스트의 줄 커버리지는 **54,799 / 69,028 = 79.39%**, 함수 78.86%,
region 78.80%였다. 첫 실행은 example 실행 파일이 없어서 교차 프로세스 잠금
검사에서 실패했다. example을 계측 빌드하고, 그 검사 및 뒤에 남은 state,
terminal, vcs 테스트를 통과시켜 합친 결과이다. 실패한 부분 실행만의 수치를
전체 커버리지로 사용하지 않았다.

| crate | 보강 전 줄 커버리지 | 보강 후 |
|---|---:|---:|
| flowmux (GUI) | 70.69% | 71.01% |
| flowmux-browser | 98.14% | 98.14% |
| flowmux-cli | 76.01% | 76.77% |
| flowmux-config | 92.61% | 92.61% |
| flowmux-cookies | 89.88% | 89.88% |
| flowmux-core | 93.27% | 93.27% |
| flowmux-daemon | 94.15% | 94.19% |
| flowmux-editor | 93.40% | 93.82% |
| flowmux-ipc | 93.24% | 93.24% |
| flowmux-md-viewer | 51.50% | 51.50% |
| flowmux-notify | 89.76% | 89.76% |
| flowmux-procmon | 84.64% | 84.64% |
| flowmux-state | 94.65% | 94.65% |
| flowmux-terminal | 89.86% | 89.86% |
| flowmux-vcs | 86.70% | 86.70% |

보강 후 줄 커버리지는 **55,000 / 69,028 = 79.68%**, 함수 **79.27%**,
region **79.06%**이다. 새 테스트뿐 아니라 자식 CLI의 profile 수집 보정도
포함된 상승이므로 전부 새 행동 검증의 효과라고 해석하지 않는다.

측정 범위에 다음 제한이 있다.

- cargo-llvm-cov 기본값에 따라 별도 `tests/`, `examples/`, `*_tests.rs` 파일은
  보고서에서 제외된다. production 파일 안의 inline test 모듈은 포함되므로
  위 수치는 **테스트 코드를 완전히 제거한 production 전용 비율이 아니다**.
- LLVM 보고의 `172 functions have mismatched data` 경고를 `llvm-cov show --dump`로
  확인했다. 모두 보고서에서 제외되는 외부 의존성(`memchr` 67, `tracing_core` 44,
  `tracing` 27, `tokio` 17, `aho_corasick` 15, `zip` 2)의 hash 0 심볼이며
  flowmux 심볼은 없었다. 경고를 숨기지 않고 하한 검사의 정상 종료를 확인했다.
- stable Rust의 줄/함수/region 계측이다. JSON의 branch count 0은 분기 검증이
  0%라는 뜻이 아니라 별도 branch instrumentation을 켜지 않았다는 뜻이다.
- Linux에서 컴파일되지 않는 macOS/stub 파일은 분모에도 없다. HTML/JS 문자열을
  만드는 Rust 코드 실행은 그 문자열 안의 모든 JS 분기 실행을 입증하지 않는다.
- 강제 종료하는 별도 GUI 자식 프로세스는 종료 시 profile을 flush하지 못한다.
  해당 E2E의 동작 통과는 별도 증거이며 Rust 커버리지 상승으로 모두 반영되지 않는다.

### 이번 보강과 CI

- `ui/editor_behavior_tests.rs`: 실제 `EditorPane`이 배포용 Monaco bundle을
  로드하고 ready 이전의 open 요청을 처리한다. 다국어 경로/문자 입력, Ctrl+S의
  실제 저장, dirty 상태와 flush, 외부 파일 변경의 충돌 UI, 덮어쓰기 거부,
  Reload from Disk 버튼, 웹 프로세스 종료 후 미저장 내용 복구, workspace 검색,
  외부 문서 탐색 거부를 확인한다. mock editor나 별도 Chromium은 사용하지 않는다.
- `tests/browser_navigation.rs`: production GUI와 실제 소켓에 pane 분할·resize·
  focus·send-keys·read-screen·close를 연결했다. 명령 echo를 출력으로 오인하지
  않도록 정확한 출력 행을 확인하고, 대상이 아닌 pane에 입력이 새지 않는지,
  마지막 pane 닫기 거부와 닫힌 pane 조회 오류도 검사한다. 브라우저 wait의 5가지
  조건, timeout의 false 반환, PNG screenshot 내용, browser tab의 terminal 조회
  거부를 기존 navigation/ref 검사에 추가했다.
- GTK/libadwaita 및 Markdown renderer의 초기화/표시 환경이 없을 때 테스트를
  성공으로 반환하던 경로를 제거했다. `cargo test --workspace`는 문서대로
  Xvfb+D-Bus에서 실행해야 한다. CI에는 OpenCode hook 실행용 Node도 명시했다.
- 웹 에디터의 불완전한 저장 응답·잘못된 sequence·비객체 메시지 거부, 미저장/
  읽기 전용 표시, Alt+일반 문자 처리 검사를 보강했다. Node 22에서 34개 통과,
  **줄 95.67% → 98.63%, 분기 88.96% → 93.87%, 함수 100%**.
  이 수치는 `.test-build`의 순수 로직 6개 모듈에 한정된다. DOM/Monaco를 다루는
  `main.ts`는 이 Node 수치에 포함하지 않고 실제 WebKit 테스트로 별도 검증한다.
- `.github/workflows/test.yml`: PR/main push의 Rust 전체 계측, HTML/JSON artifact,
  줄 79%·region 78%·함수 78% 하한, 웹 에디터 줄 98%·분기 90%·함수 100% 하한을
  추가했다. 계측은 `scripts/test-coverage.sh`에서 **원래 `cargo test --workspace`**
  를 실행해 잠금 검사의 example과 doctest도 유지한다. 매번 이전 profile과
  workspace 계측 산출물을 지워 삭제된 테스트의 데이터가 다음 실행에 섞이지 않게 한다.
  tmux E2E의 환경 초기화에서도 `LLVM_PROFILE_FILE`을 보존해 실제 자식 CLI의
  실행 데이터가 임시 디렉터리와 함께 사라지거나 소스 디렉터리에 누출되지 않게 했다.
  ignored stress 4개는 주간/수동 CI job으로 실행한다. debug stress 결과를 release
  성능 benchmark로 해석하지 않는다. 전체 하한은 큰 하락을 막는 기준이며 특정
  기능의 커버리지 하락을 각각 막는 기준은 아니다.

로컬에서도 CI와 같은 검증을 실행할 수 있다.

```bash
rustup component add llvm-tools-preview
cargo install cargo-llvm-cov --version 0.6.24 --locked
GDK_BACKEND=x11 GTK_A11Y=none CARGO_BUILD_JOBS=2 \
  xvfb-run -a dbus-run-session -- bash scripts/test-coverage.sh -- --nocapture
mkdir -p target/llvm-cov
# report 명령은 같은 profdata를 갱신하므로 동시에 실행하지 않는다.
cargo llvm-cov report --json --summary-only --output-path target/llvm-cov/summary.json
cargo llvm-cov report --html
cargo llvm-cov report --fail-under-lines 79 --fail-under-regions 78 --fail-under-functions 78
```

웹 에디터는 Node 22+에서 `npm ci && npm run test:coverage`를 실행한다.
HTML 보고서는 `target/llvm-cov/html/index.html`, CI에서는
`linux-workspace-coverage` artifact에 있다.

최종 검증 증거:

- CI와 같은 계측 스크립트: **1,634 passed, 0 failed, 4 ignored**. 별도 GUI/IPC
  프로세스와 실제 WebKit/Monaco 검사 포함. 로그 `/tmp/flowmux-coverage-final.log`.
  detached panic, `no reactor running`, WebKit/OpenCode skip 메시지는 없었다.
- 마지막 tmux profile 전달 및 screenshot assertion 수정 후 해당 E2E 2개를
  계측 재실행해 통과했다. 로그 `/tmp/flowmux-coverage-final-e2e.log`.
  소스 디렉터리에 새 `.profraw`가 생기지 않는 것도 확인했다.
- 별도 stress 4개 통과: `/tmp/flowmux-coverage-stress.log`.
- Clippy 전체 target, rustfmt, diff check, 웹 에디터 34개, build/verify 및
  committed dist 일치 확인 통과. Clippy 로그 `/tmp/flowmux-coverage-clippy.log`.
- HTML/JSON 보고서 생성 및 Rust 커버리지 하한 검사 통과.
  JSON `target/llvm-cov/summary.json`, 하한 로그 `/tmp/flowmux-coverage-floor.log`.
- workflow YAML/step 구조와 shell 구문을 확인하고 실행 명령을 로컬에서 검증했다.
  GitHub hosted runner의 원격 실행 결과를 확인한 것은 아니다.

### 남아 있는 우선순위

| 영역 | 근거와 다음 검증 |
|---|---|
| GUI 명령·shortcut·시작 흐름 | 기존 `window/command_palette.rs` 27.07%, `keybindings.rs` 39.29%, `main.rs` 44.56%. 핵심 회귀 검사는 있으나 모든 메뉴/shortcut 조합을 통과시킨 것은 아니다. 실제 입력으로 명령 선택, 키 재설정 후 동작, 앱 재시작 복원을 확장할 필요가 있다. |
| CLI hook 진입점 | `cmd_hooks.rs` 18.66%. parser/daemon lifecycle 검증과 실제 CLI 프로세스의 모든 이벤트 경로는 다르다. agent별 실제 stdin payload를 CLI→socket→화면까지 연결하는 검증이 다음 우선순위다. |
| 뷰어·설치/업데이트 | Markdown GUI 시작·PNG 출력·spec은 검증하지만 모든 대화형 탐색은 아니다. image viewer의 모든 loader·animation·오류 UI와 업데이트 다운로드/설치 실패 후 복구도 분리 검증이 필요하다. |
| 플랫폼·데스크톱 연동 | Wayland/WSLg, macOS WKWebView/IME, 실제 알림 클릭, Secret Service·외부 계정은 이 Linux Xvfb CI가 대신 검증하지 못한다. 해당 OS/서비스가 준비된 runner의 실행 증거가 필요하다. |
| 시각·접근성·성능 | 모든 테마/배율, CJK IME와 스크롤/미니맵의 실제 compositor 동작, 키보드 전용 탐색, 장시간 메모리 증가를 숫자 커버리지로 보장할 수 없다. 고정 입력/표식과 해당 환경의 회귀 시나리오로 검증해야 한다. |
