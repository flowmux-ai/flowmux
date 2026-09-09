<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# ADE 동향과 flowmux 기능 확장 검토

검토 시점: **2026-09-09 11:01 KST**. flowmux 기준: `16b6586e4831659e25b9e16b382afb05831217a1`.

**추천 방향은 작업 시작 → 실행 관찰 → 변경 검토를 하나의 workspace 흐름으로 연결하는 것이다.** 우선 worktree 생성·실행, Git diff·PR/CI 검토, 개발 서버 미리보기를 완성하고, 이어 활동 기록과 세션 검색을 강화하는 편이 투자 대비 효과가 크다. SSH는 중기 후보, 모바일·독립 실행 서비스·범용 플러그인 호스트는 별도 제품 단계로 평가한다.

공식 GitHub release API의 `latest`, 최근 릴리스 목록, main 커밋, README 및 공식 사이트를 확인했다. Orca·Paseo·cmux 사이트는 flowmux 브라우저에서 열어 확인했다. 경쟁 제품을 설치해 동작·성능을 비교한 결과는 아니다. 아래의 현재 기능은 공식 문서/릴리스의 주장, flowmux 현황은 코드 확인, 공수와 우선순위는 검토자의 추정으로 구분한다. 기능 구현 요청이 아닌 검토 요청이므로 제품 코드는 변경하지 않았다.

## 1. 프로젝트별 최근 동향

날짜는 GitHub `published_at`의 UTC 날짜다. 정식 릴리스, 베타, main 변경을 혼용하지 않았다.

| 프로젝트 | 확인한 배포 상태 | 최근 주목할 변화 | flowmux에 주는 시사점 |
|---|---|---|---|
| **Orca — stablyai/orca** | 정식 **v1.4.198, 9/8**. Android v0.0.48은 9/6 prerelease | Structured Chat 실험 설정에서 Codex 채팅·inline diff·개별 task 중지 개선. GitHub Projects Roadmap 타임라인. SSH/relay·worktree 실행 호스트 판별·성능 개선 | 병렬 실행 자체보다 결과 검토와 작업별 실행 위치 관리가 중요해졌다. 전체 채팅 UI보다 worktree·diff 연결을 먼저 가져올 가치가 큼 [O1] |
| **Paseo — getpaseo/paseo** | 정식 **v0.7.2, 9/2**. **v0.8.0-beta.1, 9/8** 별도 | 정식은 pi 실행 중 steering, 큰 diff·모바일 streaming 안정화. 베타는 provider/ACP, timeline, slash command, 설정, lifecycle hook까지 플러그인 API 확장. 0.7 플러그인과 호환되지 않는 변경 포함 | 장기적으로 구조화된 provider 제어가 유용하지만 SDK 호환성 유지 비용이 큼. flowmux는 우선 기존 CLI와 lifecycle 정보를 확장하는 것이 현실적 [P1][P2] |
| **cmux — manaflow-ai/cmux** | GitHub 최신 정식 **v0.64.22, 8/3**. main은 9/9까지 진행 | 정식은 SSH 시작, 세션 복원 환경, stale surface 대상 보호 등의 수정. 현재 SSH 문서는 원격 workspace·브라우저 네트워크·scp·재접속·Mosh·tmux를 설명. 9/9 main에는 Cloud Machines의 beta-toggle 제한 변경 | Linux에서 대응 가치가 큰 것은 SSH workspace와 원격 localhost 미리보기. 현재 문서의 모든 기능을 v0.64.22에 포함된 것으로 단정하면 안 됨 [C1][C2][C3] |
| **Superset — superset-sh/superset** | 정식 desktop **v1.27.0, 9/7**. desktop-canary는 9/9 별도 | PR review thread 응답, 이미지/binary diff, provider 모델 선택, plugin marketplace, detached 개발 서버 탐지, parent 아래 subagent 표시. Git watcher와 GitHub rate-limit 처리 개선 | flowmux가 보강할 구체적인 연결부: Changes 화면, 지속적인 PR/CI 상태, 포트 소유 workspace 판별, 가시성에 따른 조회 비용 조절 [S1] |
| **Coder Mux → Xum — coder/xum** | `coder/mux` API 요청이 `coder/xum`으로 연결됨. 정식 **v0.28.4, 9/2**, nightly는 9/8 별도 | 원격 서버의 브라우저 로그인, 프로젝트·memory 백업, 전송 중복 큐 처리, 느린 서버 상태 표시. v0.28.3에는 durable workflow cards와 agent 간 메시징 | custom agent loop를 가진 제품으로 flowmux와 책임 범위가 다름. 상태·백업·원격 연결 UX를 참고하고 자체 LLM 실행 엔진 도입은 별도 판단 [X1][X2][X3] |

Orca의 현재 README는 병렬 worktree, diff 코멘트, SSH, 모바일, UI 요소를 agent prompt로 보내는 Design Mode를 설명한다 [O2]. Paseo는 daemon에 desktop/mobile/web/CLI가 연결되는 구조, 선택 가능한 E2E 암호화 relay, 음성 입력을 설명한다 [P3]. Superset은 setup script와 terminal preset, 작업별 포트, 자동 실행을 함께 제공한다고 설명한다 [S2].

공통 추세는 **에이전트 수를 늘리는 기능에서, 작업을 격리하고 사람이 검토·응답하며 여러 장치에서 이어 가는 기능으로 확장**되는 것이다. 동시에 각 릴리스에서 큰 diff, background 작업, 연결 끊김, stale 상태, API 호출량을 지속적으로 보완한다. 이것은 기능 목록만큼 중요한 구현 비용의 근거다.

cmux `nightly`는 재사용되는 릴리스이므로 오래된 `published_at`만으로 빌드 시점을 판단하지 않았다. 확인 당시 본문은 `8a8dff1` 빌드를 가리켰다. Orca도 릴리스 노트에서 merge 후 배포까지 보통 48–72시간이 걸린다고 안내하므로, main에 있는 기능을 정식 배포 완료로 보지 않았다.

## 2. flowmux의 실제 출발점

README와 오래된 계획 문서만 보면 중복 기능을 제안하기 쉽다. 특히 기존 roadmap의 기준일은 6/14이며 같은 문서 안에서도 완료 목록과 이전 gap 표가 섞여 있다. 아래는 현재 소스에서 확인한 범위다.

| 영역 | 확인한 현재 구현 | 실제 확장 지점 |
|---|---|---|
| workspace/브라우저/CLI | pane·tab 조작, browser snapshot/action/wait/screenshot, read-screen, command palette | 기본 조작을 다시 만드는 대신 작업 흐름으로 연결 [F1] |
| worktree | Git porcelain 조회·변경 수 집계·안전한 삭제. UI 콜백은 정보·삭제·새로고침 | worktree 생성 → workspace 생성 → 에이전트 실행을 연결하는 관리 경로 [F2][F3] |
| 템플릿 | **Agent + tests + browser 내장 템플릿과 materialize 실행 경로가 이미 있음**. 프로젝트 명령은 argv/cwd/env/target/confirm 지원 | 사용자별 저장·편집, 생성 worktree에 템플릿 적용. 템플릿 기능 자체는 신규가 아님 [F4][F5] |
| 편집기/diff | Monaco 편집기와 `ShowDiff`; 현재 확인한 생산 경로는 저장 시 디스크 충돌 비교 | Git HEAD/index/working tree 비교와 변경 파일 탐색·리뷰 코멘트 [F6] |
| Git/PR | `inspect()`가 branch/remote/PR 번호·상태·URL을 조회. daemon의 workspace 생성 경로에서 best-effort로 호출 | CI/check/review 상태, 주기·수동 갱신, rate-limit/backoff. 현재 `gh` 인자는 `number,state,url,isDraft` [F7] |
| 포트 | `listening_ports()`의 Linux procfs 구현·테스트와 side panel 표시 필드가 있음 | 전체 crates 호출 검색에서 생산 코드의 감지 함수 호출·포트 값 갱신은 찾지 못함. 자동 감지·상태 갱신·클릭 연결을 완성할 후보 [F8] |
| 에이전트 활동/알림 | hook 기반 상태, 대기/완료, 프로세스 fallback. 활동과 알림 저장소는 각각 최대 50개의 메모리 항목 | 검색 가능한 영속 기록, 검토 대기 필터, 관측 출처·최종 시각 표시 [F9] |
| 세션 복원 | 레이아웃 복원과 provider별 세션 ID를 이용한 resume 명령 | 과거/외부 세션 선택·검색. 현재 resume와 GUI 종료 후 프로세스가 계속 실행되는 것은 다른 기능 [F10] |
| 사용량 | provider 사용량 popover·상시 bar·실패 시 last-known 처리 | 처음부터 비용 dashboard를 만들기보다 실패/오래된 값 표시와 작업별 귀속 정확도를 강화 [F11] |
| IPC/원격 | GUI 안의 daemon + Unix socket + GTK bridge. `Event` enum은 있지만 server는 요청에 대한 응답을 쓰는 구조 | 외부 event subscription·원격 인증·재접속은 별도 구현. enum 존재가 streaming API 완성을 뜻하지 않음 [F12] |

`pty-tee`는 별도 프로세스이지만 Linux에서 부모 사망 시 SIGHUP을 받도록 설정한다. 따라서 이 구조만으로 GUI 재시작을 넘어 작업이 계속 실행된다고 볼 수 없다 [F13].

## 3. 도입 후보와 구현 가능성

P0는 다음 개발 주기 추천, P1은 그다음, P2는 수요 확인 후 진행이다. 공수는 저장소에 익숙한 개발자 1명이 Ubuntu 24.04에서 **아래 최소 범위를 구현하고 집중 테스트·실행 검증하는 작업일** 추정이다. 약속된 일정이나 성능 측정값이 아니며 디자인·배포·장기간 upstream 호환성 유지 비용은 제외한다. 항목 간 재사용이 있으므로 단순 합산하지 않는다.

| 우선 | 후보 | 가치·참고 프로젝트 | 최소 구현 및 기존 코드 재사용 | 난이도 / 추정 |
|---|---|---|---|---|
| **P0** | **worktree 작업 시작** | 병렬 agent의 파일 충돌 감소. Orca/Superset/Paseo | branch/base/path 선택, `git worktree add`, workspace 연결, 기존 템플릿으로 agent 실행. 실패한 setup의 상태·재시도 제공 | 중 / **5–8일** |
| **P0** | **Changes·Git diff 검토** | 작업 결과를 앱에서 판단. Orca/Superset | 변경 파일 목록, HEAD/index/worktree 선택, Monaco diff 재사용. 초기에는 파일 단위 review와 로컬 코멘트 전달 | 중 / **7–12일** |
| **P0** | **개발 서버 포트 → 미리보기** | 실행한 웹앱을 바로 확인. cmux/Superset | procmon 감지 연결, workspace별 포트 상태 갱신, 클릭 시 기존 browser open. 수동 URL 등록도 제공 | 중하 / **3–5일** |
| **P0** | **PR/CI 상태 갱신** | 완료 알림 뒤 검증 실패를 놓치지 않음. Superset/Orca | 기존 `gh` 조회에 check/review 상태 추가, 선택 workspace 중심 갱신, 실패 시 오래된 값 표시·backoff | 중하 / **3–5일** |
| **P1** | **영속 활동 기록·attention 필터** | 여러 agent 중 사람이 응답할 작업 찾기. Orca/Paseo | 기존 ActivityStore·NotificationStore에 제한된 저장·검색 추가. 상태 전이와 요약만 기록 | 중 / **4–7일** |
| **P1** | **세션 검색·다시 열기** | flowmux 밖에서 시작한 작업도 이어가기. Paseo/Orca | 우선 flowmux가 기록한 session ID의 선택 UI, 이후 provider별 로컬 session index import | 중 / **4–7일**, 외부 import는 provider당 **2–4일** 추가 |
| **P1** | **사용자 workspace 템플릿·실행 프로필** | 반복 개발 환경 생성 시간 감소. Superset | 내장 materializer를 재사용해 argv/cwd/env/pane/browser URL 저장·편집. 초기 setup 명령은 명시 실행 | 중하 / **3–5일** |
| **P1** | **브라우저 요소를 prompt로 전달** | UI 버그 설명이 쉬워짐. Orca Design Mode | snapshot ref의 text/selector/URL 및 computed style 요약을 복사·전달. 선택적 viewport screenshot 첨부 | 중 / **4–7일** |
| **P1** | **관측된 subagent 표시** | 부모 작업의 실제 진행 상황 파악. Superset/Xum | 기존 Codex child ledger를 출발점으로 부모·자식·최근 관측 표시. 완전한 task tree라고 표현하지 않음 | 중 / **4–7일**, provider 확대 별도 |
| **P1/P2** | **추가 agent 지원** | Cursor Agent/Amp/Droid/pi 등 사용자 범위 확대 | 실행 프로필부터 제공. 안정된 native event/resume 계약이 있는 provider만 hooks·doctor/fix까지 확대 | 실행 프로필 **1–2일**, lifecycle 연동 provider당 **3–7일** |
| **P2** | **SSH workspace** | 원격 GPU·개발 서버 활용. cmux/Orca/Xum | OpenSSH 설정 재사용, 원격 terminal·식별·선택적 `-L` 포트 전달부터. 원격 Git/파일·hook 전달은 다음 단계 | 높음 / 기본 **8–15일**, 통합 **추가 3–6주** |
| **P2** | **GUI와 독립된 실행·재접속** | 앱 종료·재시작에도 작업 지속. Paseo | PTY/session 소유권을 장기 실행 프로세스로 이동, UI는 attach. 단순 headless binary 실행만으로 해결 안 됨 | 매우 높음 / **4–8주** |
| **P2** | **모바일·웹 companion** | 밖에서 진행 확인·후속 입력. Paseo/Orca | 먼저 VPN 내부의 인증된 읽기 전용 상태 UI, 그다음 대상 session이 명확한 입력. 외부 relay는 별도 | 높음 / 읽기 전용 **2–4주**, 제어·pairing·relay 포함 **6–12주 이상** |
| **P2** | **확장 API·MCP·ACP** | 다른 도구와 agent 통합. Paseo/Superset | 현 CLI/IPC를 감싸는 좁은 MCP 도구 또는 event API부터. ACP 기반 채팅은 provider 하나로 실증 | MCP **4–7일**, event stream **5–10일**, ACP UI·plugin host **4–8주 이상** |
| **P2** | **예약 실행·작업 자동화** | 정기 검증·triage. Orca/Superset/Xum | 기존 명령과 systemd user timer 연계부터. 앱이 꺼져도 실행하려면 독립 실행 소유권이나 외부 runner 필요 | 앱 실행 중 최소형 **3–5일**, durable workflow는 별도 |

### A. worktree 작업 시작

기존 `WorktreeList`, 삭제 보호, `StateStore::create_workspace`, `materialize_workspace_template`를 연결한다. 새로운 프로젝트 관리자나 범용 task 엔진은 필요하지 않다. Git 실행은 기존 `std::process` + blocking pool 관례를 따른다. 이 저장소는 GLib의 SIGCHLD 처리와 `tokio::process` 충돌을 피하기 위해 그 방식을 쓴다 [F2][F7].

최소 흐름은 저장소/기준 branch 선택 → 새 branch·worktree 생성 → 해당 root의 workspace 생성 → 템플릿 적용이다. 재시도 시 같은 작업이 중복 생성되지 않아야 한다. setup 실패 후 이미 생긴 사용자 파일을 자동 삭제하지 않고 경로와 실패 단계를 보여준다. worktree는 파일 작업 디렉터리 격리이며 비밀키·네트워크·OS 권한을 격리하는 sandbox는 아니다.

검증: 같은 저장소에서 두 작업이 서로 다른 cwd/branch를 쓰는지, 이름 충돌·공백 경로·setup 실패·재실행을 처리하는지, 실행 중인 작업의 worktree 삭제 보호가 유지되는지 실제 GUI에서 확인한다.

### B. Changes·PR/CI 검토

저장 충돌 비교용 Monaco diff를 그대로 Git 기능이라고 노출하면 안 된다. Git 원본·index·working tree의 출처와 변경 시점을 따로 전달해야 한다. 첫 단계는 읽기 중심의 파일 목록·diff·check 상태로 제한하고, 파일 stage/unstage·commit과 PR draft는 후속 단계에서 별도 버튼으로 제공한다. hunk stage, inline GitHub review 게시, 자동 merge까지 처음부터 포함하지 않는다.

로컬 review 코멘트에는 파일 경로·기준 revision·line/context를 담고, 지정한 agent에 전달할 텍스트를 먼저 보여준다. agent가 계속 파일을 수정했으면 코멘트 기준이 오래됐다는 표시가 필요하다. 무작정 활성 terminal로 문자열을 보내면 shell 또는 다른 세션에 입력될 수 있으므로 surface/session 일치를 재확인한다.

GitHub 조회는 관련 workspace가 보일 때와 명시 새로고침 위주로 실행한다. timeout·취소·동시 호출 제한을 두고 API 장애와 PR 없음은 분리한다. Superset의 최근 rate-limit·watcher 개선이 이 비용을 보여준다 [S1].

검증: staged/unstaged 동시 변경, rename/delete/untracked/binary, 큰 파일, 편집 중 갱신, GitHub 미인증·rate limit을 다룬다. CI가 pending/failed인 상태를 완료로 표시하지 않아야 한다.

### C. 포트 미리보기

`listening_ports()`를 terminal PID 집합에 적용하고 결과를 기존 workspace 필드에 반영하면 첫 구현이 가능하다. procfs 탐색은 GTK 메인 스레드 밖에서 수행하고 중복 scan을 제한한다. 모든 TCP 포트가 HTTP는 아니므로 자동으로 브라우저를 열지 않고 사용자가 URL/protocol을 선택할 수 있게 한다.

daemonize되어 부모가 바뀐 서버는 단순 자손 PID 탐색에서 빠질 수 있다. 최초 MVP는 이를 제한으로 명시하고 수동 URL 등록으로 보완한다. 이후 프로세스 시작 시각·소유 정보 등을 이용한 추적을 검토한다. 컨테이너/원격 network namespace의 localhost는 별도로 취급한다.

검증: 서로 다른 workspace에서 웹서버 두 개를 실행해 정확히 귀속되는지, 서버 종료 시 표시가 사라지는지, 포트 재사용·IPv6·비HTTP 포트를 잘못 연결하지 않는지 확인한다.

### D. 활동 기록·세션·agent 확대

현재 hook → 상태 판정 → 알림 체계를 유지하면서 bounded 영속 기록을 붙이는 편이 작다. 전체 prompt와 tool 출력을 수집하기보다 시작/대기/완료/종료, session ID, 출처, 시각을 기록한다. 중복 제거와 보관 기간을 두고, 오래된 항목에서 사라진 surface로 jump할 경우 session 재열기 경로를 제공한다.

현재 프로세스 이름 인식 목록에는 Cline·Aider·Goose도 있으나, **프로세스를 알아보는 것과 lifecycle/resume까지 지원하는 것은 다르다** [F14]. 추가 agent는 실행 가능 / 상태 관측 / 입력 요청 식별 / resume 지원을 분리해 표시한다. provider 한 개씩 실제 CLI 버전으로 검증하고 `doctor/fix` 설치·갱신·제거 경로까지 맞춘다.

subagent 표시도 같은 원칙이다. 기존 Codex ledger는 완료 판정 보조이며 관측 누락·재사용 가능성이 있다. terminal 문구로 빈칸을 추정해 완전한 팀 계보를 만들지 않는다. 부모의 turn 완료, 자식 작업 완료, 프로세스 종료를 구별한다 [F15].

### E. 브라우저 Design Mode와 원격 기능

flowmux의 WebKit snapshot·eval·screenshot으로 요소 설명과 computed style 수집은 구현 가능하다. 초기에는 기존 ref 선택과 viewport 이미지 전달로 충분하다. cross-origin iframe, Shadow DOM, element crop·좌표 변환은 추가 검증이 필요하다. snapshot 계약을 유지해 live DOM에 `data-flowmux-ref` 같은 속성을 쓰지 않는다.

Orca의 Chromium 기반 Design Mode를 그대로 옮기면 안 된다. WebKitGTK에는 CDP가 없으므로 Chrome 전용 network mocking/device emulation을 기존 API의 작은 확장이라고 산정하지 않았다. 이런 요구는 외부 도구 연결 여부를 별도로 판단한다 [F1][O2].

SSH의 첫 단계는 기존 OpenSSH 클라이언트와 known_hosts/config, 로컬 포트 전달을 재사용할 수 있다. 다만 local/remote cwd·PID·session을 구별해야 하며 remote hook을 로컬 socket에 연결하려면 안전한 transport와 대상 매핑이 필요하다. 재접속 시 명령을 중복 실행하지 않는 것도 필수다. Mosh의 terminal transport만으로 Git/파일/브라우저 제어까지 해결되지 않는 점은 cmux 문서도 분명히 설명한다 [C2].

모바일은 단순 GTK UI의 웹 변환 문제가 아니다. 인증·장치 폐기·대상 session 검증·재접속·출력 backpressure와 작업 소유권이 필요하다. 읽기 전용 companion은 GUI가 켜진 동안 독립적으로 실증할 수 있지만, 앱 종료 후 지속 실행까지 약속하려면 실행 서비스 분리가 선행돼야 한다.

## 4. 다른 프로젝트를 직접 지원하는 선택지

| 선택지 | 판단 |
|---|---|
| **Paseo를 선택적 외부 실행 backend로 사용** | flowmux terminal에서 Paseo CLI를 실행하는 구성부터 시험할 수 있다. 그러면 모바일·원격 실행 수명을 Paseo가 맡고 flowmux는 로컬 terminal/browser를 제공한다. 설치·로그인·session ID 매핑과 두 앱의 상태 불일치는 검증 필요. 제품 내부 통합으로 확인된 것은 아니며 별도 PoC 후보다. 전체 relay를 새로 만드는 것보다 먼저 평가할 가치가 있다. |
| **Orca/cmux와 CLI 상호 운용** | workspace 열기·URL 전달·명령 실행을 외부 명령으로 연결할 수 있다. socket schema나 saved state를 공유한다고 가정하지 않는다. 현재 `cmux.json`도 flowmux가 사용하는 일부 필드만 처리한다 [F5]. |
| **표준 도구 연계** | flowmux CLI 기반 MCP wrapper, GitHub는 이미 쓰는 `gh`, remote는 OpenSSH, 예약은 systemd user timer를 먼저 검토한다. 안정된 구체 사용 사례가 생긴 뒤 범용 plugin ABI나 marketplace로 확장한다. |

## 5. 코드·라이선스 재사용

GitHub의 SPDX 자동 판별은 Paseo/cmux/Superset에 `NOASSERTION`을 반환했지만 실제 LICENSE를 읽으면 다르다. 공개 소스라고 모두 동일한 조건은 아니다. 이번 검토는 제품 동작과 문서를 참고했으며 타 프로젝트 코드를 가져오지 않았다.

| 프로젝트 | 확인한 저장소 기본 라이선스 | flowmux 적용 원칙 |
|---|---|---|
| Orca | MIT | 파일별 고지·제3자 라이선스 확인 후 재사용 검토 가능 |
| Paseo | Apache-2.0, 제3자 구성요소 예외 | GPLv3 프로젝트와의 결합을 검토할 수 있으나 NOTICE 등 조건 유지 |
| cmux | GPL-3.0-or-later, 파일별 예외 및 제3자 고지 | flowmux와 기본 라이선스는 일치. Swift/AppKit 코드를 Rust/GTK로 그대로 재사용할 실익은 별도 |
| Superset | Elastic License 2.0 | GPL 코드로 단순 편입하지 않고 기능을 독립 구현. 직접 재사용에는 별도 권리 검토 필요 |
| Xum | AGPL-3.0 | GPL과의 결합 시 AGPL 의무가 관련될 수 있으므로 현재 GPL 배포물에 무심코 편입하지 않음. 패턴 참고·별도 프로세스 연계 우선 |

근거: 각 저장소의 [Orca LICENSE](https://github.com/stablyai/orca/blob/main/LICENSE), [Paseo LICENSE](https://github.com/getpaseo/paseo/blob/main/LICENSE), [cmux LICENSE](https://github.com/manaflow-ai/cmux/blob/main/LICENSE), [Superset LICENSE.md](https://github.com/superset-sh/superset/blob/main/LICENSE.md), [Xum LICENSE](https://github.com/coder/xum/blob/main/LICENSE). 실제 코드 도입 시 해당 revision의 파일별 고지를 다시 확인한다.

## 6. 권장 실행 순서와 완료 기준

1. **짧은 첫 배포: 포트 미리보기 + PR/CI 갱신.** 이미 있는 감지·표시·조회 경로를 연결해 개발 결과를 확인하기 쉽게 만든다. 기준: 웹서버/CI 상태 변화가 해당 workspace에서 정확히 보이고 UI가 멈추지 않는다.
2. **핵심 ADE 배포: worktree 생성 + 기존 템플릿 적용 + Changes 화면.** 기준: 작업 두 개를 격리해 시작하고 각 결과를 diff로 검토할 수 있으며 실패·재시도·삭제 시 작업 내용이 보존된다.
3. **운영 품질 배포: 영속 attention 기록 + 세션 검색 + 선택적 추가 agent.** 기준: 재시작 후 처리할 항목을 찾고 원하는 session을 명시적으로 다시 열 수 있다. 관측 불명 상태를 정상 완료로 오인하지 않는다.
4. **원격 PoC 두 개를 비교:** OpenSSH 기반 workspace와 Paseo 외부 backend. 실제 원격 사용자 흐름·설치 복잡도·상태 정확도를 보고 어느 쪽에 투자할지 결정한다. 모바일 자체 구현은 이 결과와 지속 실행 요구를 기준으로 결정한다.

처음부터 넣지 않을 범위는 자체 LLM 실행 엔진, 대규모 kanban/Linear 복제, cloud VM fleet, 전체 plugin marketplace, Chrome 수준 CDP 호환, 사용자 승인 없이 수행하는 자동 merge·권한 응답이다. 이 기능들은 구현 불가능해서가 아니라 현재 Linux terminal/browser 기반의 강점을 강화하는 작업보다 비용과 책임 범위가 크기 때문이다.

구현 단계에서는 관련 Rust/CLI/Monaco 검사를 수행한 뒤 **실행 중인 flowmux에서 동일한 사용자 흐름을 재현**해야 한다. 이번 검토는 코드 분석과 외부 문서 확인까지이며, 위 후보의 런타임 구현 가능성이 PoC로 입증됐다고 주장하지 않는다.

## 외부 근거

- [O1] [Orca v1.4.198 릴리스](https://github.com/stablyai/orca/releases/tag/v1.4.198)
- [O2] [Orca README — 검토 당시 main](https://github.com/stablyai/orca/blob/e80fae0c4d4d/README.md), [공식 사이트](https://www.onorca.dev/)
- [P1] [Paseo v0.7.2](https://github.com/getpaseo/paseo/releases/tag/v0.7.2)
- [P2] [Paseo v0.8.0-beta.1](https://github.com/getpaseo/paseo/releases/tag/v0.8.0-beta.1)
- [P3] [Paseo README — 검토 당시 main](https://github.com/getpaseo/paseo/blob/da8c1b5c94e7/README.md), [공식 사이트](https://paseo.sh/)
- [C1] [cmux v0.64.22](https://github.com/manaflow-ai/cmux/releases/tag/v0.64.22)
- [C2] [cmux SSH 공식 문서](https://cmux.com/docs/ssh)
- [C3] [cmux Cloud Machines beta-toggle main 변경](https://github.com/manaflow-ai/cmux/commit/164afa6a1943), [nightly](https://github.com/manaflow-ai/cmux/releases/tag/nightly)
- [S1] [Superset desktop-v1.27.0](https://github.com/superset-sh/superset/releases/tag/desktop-v1.27.0)
- [S2] [Superset README — 검토 당시 main](https://github.com/superset-sh/superset/blob/397c4c2bcab2/README.md)
- [X1] [Xum v0.28.4](https://github.com/coder/xum/releases/tag/v0.28.4)
- [X2] [Xum v0.28.3](https://github.com/coder/xum/releases/tag/v0.28.3)
- [X3] [Xum README — 검토 당시 main](https://github.com/coder/xum/blob/9f4144f77082/README.md)

## flowmux 코드 근거

행 번호는 검토 기준 commit의 위치다. 링크는 로컬 파일로 연결한다.

- [F1] [README](../README.md), [browser 계약](../AGENTS.md)
- [F2] [worktree.rs](../crates/flowmux-vcs/src/worktree.rs): 62 `list_worktrees`, 122 `remove_worktree`, 172 `git_output`
- [F3] [worktree_panel.rs](../crates/flowmux/src/ui/worktree_panel.rs): 288–300 정보/삭제/갱신 콜백; [worktrees.rs](../crates/flowmux/src/ui/window/worktrees.rs): 273 삭제, 415 조회
- [F4] [command_palette.rs](../crates/flowmux/src/ui/window/command_palette.rs): 99 내장 템플릿, 185 materializer, 667 실행 경로
- [F5] [cmux_json.rs](../crates/flowmux-config/src/cmux_json.rs): 15 `CmuxJson`, 32 `CustomCommand`
- [F6] [editor protocol](../crates/flowmux-editor/src/protocol.rs): 156 `ShowDiff`; [session.rs](../crates/flowmux-editor/src/session.rs): 672 충돌 비교; [Monaco main.ts](../editor/flowmux-editor-web/src/main.ts): 1045 `createDiffEditor`
- [F7] [VCS lib.rs](../crates/flowmux-vcs/src/lib.rs): 28 inspect, 69 linked_pr; [daemon handler.rs](../crates/flowmux-daemon/src/handler.rs): 108 workspace 생성
- [F8] [procmon lib.rs](../crates/flowmux-procmon/src/lib.rs): 511 listening_ports; [side panel](../crates/flowmux/src/ui/sidebar.rs): 2028 포트 표시; [IPC protocol](../crates/flowmux-ipc/src/protocol.rs): 843 PortListening. 생산 호출 부재는 전체 `crates/**/*.rs`의 함수 호출/필드 대입 검색으로 확인
- [F9] [activity.rs](../crates/flowmux/src/activity.rs): 10 최대 항목, 121 ActivityStore; [notifications.rs](../crates/flowmux/src/notifications.rs): 22 최대 항목, 86 NotificationStore
- [F10] [window mod.rs](../crates/flowmux/src/ui/window/mod.rs): 2838 restore_from_store; [agent_sessions.rs](../crates/flowmux-state/src/agent_sessions.rs): 43 resume_argv
- [F11] [usage_bar.rs](../crates/flowmux/src/ui/usage_bar.rs), [usage/mod.rs](../crates/flowmux/src/usage/mod.rs), [README](../README.md)
- [F12] [main.rs](../crates/flowmux/src/main.rs): 235 embedded handler; [ipc_handler.rs](../crates/flowmux/src/ipc_handler.rs): 262 GUI workspace 생성 경로; [IPC server.rs](../crates/flowmux-ipc/src/server.rs): 44 serve_one
- [F13] [pty_tee.rs](../crates/flowmux-cli/src/pty_tee.rs): 137 PR_SET_PDEATHSIG
- [F14] [procmon lib.rs](../crates/flowmux-procmon/src/lib.rs): 133 KNOWN_AGENT_COMMS
- [F15] [state_store.rs](../crates/flowmux-daemon/src/state_store.rs): Codex child ledger 및 `codex_parent_stop_waits_for_matching_active_subagent`; [CLAUDE.md](../CLAUDE.md): agent 상태 관측 원칙
