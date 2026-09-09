<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# SSH workspace 구현·검증 기록

검증일: 2026-09-09. 환경: Ubuntu, OpenSSH 9.6, GTK/VTE/WebKitGTK.
기준 commit: `16b6586e4831659e25b9e16b382afb05831217a1` 위의 작업 트리.
설치된 사용자 바이너리를 교체하지 않고 `target/debug/flowmux`와 일치하는
`target/debug/flowmuxctl`로 검증했다.

## 구현된 사용자 흐름

side panel의 빈 영역 또는 workspace 행을 우클릭하면 메뉴 첫 항목으로
`New workspace`, `New SSH Workspace`가 나온다. 기존 행 동작은 구분선 아래에
유지된다. `New workspace`, `+`, Ctrl+N은 기존 local 생성 동작이다.
`New SSH Workspace`는 연결 설정 dialog를 열며 workspace는 같은 GUI 창에 생성된다.
OpenSSH 인증은 해당 GUI에 종속된 Authentication terminal에서 진행한다.
인증 창은 자동으로 열리지 않는다. 입력이 필요하면 toolbar의 Authentication으로 연다.

workspace마다 전용 OpenSSH master를 소유하고 모든 terminal tab과 split은
그 master의 별도 원격 channel로 실행된다. 연결 전이나 연결이 끊긴 상태에서
만든 tab에 local shell을 실행하지 않는다. toolbar에서 연결·해제·인증 화면·Ports에
접근한다. command palette와 `flowmux ssh connect`도 같은 생성 경로를 사용한다.

새 tab/split은 원격 cwd를 상속한다. 일반 SSH shell은 재접속 시 새 shell이 되고,
tmux 옵션은 저장한 session에 attach한다. 사라진 tmux session을 자동 재생성하지 않는다.
앱 복원은 UUID/layout/설정을 유지하고 disconnected에서 시작한다.

## 자동 검사

| 검사 | 결과 |
|---|---|
| core/state/IPC/CLI 합동 단위 검사 | 512 passed, 1 ignored |
| 이후 추가한 SSH CLI alias·정식 명령 검사 | 4 passed |
| state 전체 | 39 passed; 위 합동 검사와 중복 |
| daemon 전체, ignored stress 포함 | 174 passed |
| 격리 Xvfb/D-Bus GUI 전체 | 675 unit + 1 browser integration passed |
| 변경한 6개 crate의 all-targets clippy | `-D warnings` 통과 |
| GUI·CLI debug build, rustfmt, diff whitespace | 통과 |

검사 수를 합산하지 않는다. 일부 실행은 중복이다. GUI 전체 기록은 이 세션의
`/tmp/flowmux-gui-suite-latest.log`에 있다. quoting 검사는 실제 `/bin/sh`로
공백·따옴표·달러·백틱·개행·빈 인자의 literal 전달과 cwd 실패 시 명령 미실행을 확인한다.
state 검사는 schema v3 migration, byte-exact 백업, 미래/손상 schema 덮어쓰기 방지,
local/SSH surface 불변식을 확인한다.

GUI 회귀 검사에는 split/collapse 시 기존 terminal widget/PID 보존과
만료된 preview의 native WebKit 탐색 차단이 포함된다.

## 실제 GUI와 sshd 통합 검사

재현 스크립트: [`ssh-workspace-fixture.py`](../scripts/ssh-workspace-fixture.py),
[`test-ssh-workspace-gui.py`](../scripts/test-ssh-workspace-gui.py).
임시 키·known_hosts·sshd·HTTP 서버, 독립 Xvfb/D-Bus/XDG 디렉터리를 사용한다.
사용자의 HOME, SSH 설정, 실행 중인 일반 flowmux 창은 변경하지 않는다.

| 시나리오 | 확인한 결과 |
|---|---|
| 새 tab + 양방향 split | 서로 다른 원격 shell 4개, 로컬 FLOWMUX socket 환경 미전달 |
| 원격 OSC 7 cwd | 공백이 포함된 원격 cwd를 새 terminal에 상속 |
| 첫 tab 닫기 | 나머지 원격 PID, master, HTTP forwarding 유지 |
| 활성 forward 제거 | 포트 재사용 서버를 띄워도 preview navigate/back/reload 요청 0회 |
| Disconnect와 새 tab | forwarding 제거, local shell fallback 없음, reconnect 후 원격 실행 |
| tmux reconnect | 원격 shell PID 유지 |
| GUI 창 2개 | 명시 socket의 IPC/CLI 요청이 지정 창에만 적용 |
| 테스트 GUI 재시작 | UUID/layout 유지, disconnected 및 비활성 preview, tmux PID 유지 |
| 사라진 tmux session | attach 실패, 자동 session 재생성 없음 |
| 일회성 시작 명령 | 최초 1회 기록; create request UUID 재전송·재접속에도 중복 실행 없음 |

최종 10개 시나리오 모두 통과했다. 증거는 이 세션의
`/tmp/fm-gui-p3kstbt7/events.jsonl`, `final-tree.json`, GUI 로그에 있다.
동일 request UUID의 중복 생성 방지는 현재 GUI runtime 내 계약이며,
서버 장애까지 포함하는 분산 exactly-once 실행 보장을 뜻하지 않는다.

별도 실제 Authentication VTE 검사에서 새 host key fingerprint를 fixture 키와
비교하고 승인했으며, 암호화된 개인 키의 passphrase 입력으로 연결했다.
두 번째 tab은 재인증 없이 열렸다. ProxyJump 경유 원격 shell도 확인했다.
증거: `/tmp/fm-gui-a0yn3gnj`.

마우스로 빈 영역·행 우클릭 메뉴, 정확한 두 label, SSH dialog 입력과 Connect,
같은 창에 SSH workspace 생성까지 확인했다. SSH workspace가 선택된 상태의
`New workspace`도 원격 cwd를 재사용하지 않고 local workspace를 생성했다.
스크린샷: `/tmp/flowmux-ssh-menu.png`, `/tmp/dialog-specific.png`,
`/tmp/flowmux-ssh-row-menu.png`.

위 `/tmp` 경로는 이번 세션의 로컬 증거이며 배포 artifact가 아니다.

### 후속: 인증 팝업과 한글 입출력

연결 시작 때 Authentication 창을 강제로 열던 동작을 제거했다. 인증 PTY는 숨긴
상태에서 유지하며, 입력이 필요할 때 toolbar의 Authentication 버튼으로 연다.
격리 GUI의 X11 창을 연결·재접속 중 관찰해 인증 창이 표시되지 않는 것을 확인했고,
버튼을 마우스로 누르면 같은 인증 terminal이 열리는 것도 확인했다.
증거: `/tmp/fm-gui-0ybpakcp`, `/tmp/flowmux-auth-hidden.png`.

테스트 서버에 LANG이 없어서 locale이 POSIX였고, 실제 연결된 tmux client의
`client_utf8` 값도 0이었다. UTF-8 VTE에 맞춰 새 tmux session과 재접속 모두
`tmux -u`로 실행하도록 수정했다. fixture도 `LANG=C.UTF-8`을 제공하도록 보완했다.
기존 원격 shell의 환경은 자동으로 변경되지 않는다. 이번 fixture의 기존 tmux
연결은 reconnect 후 shell에서 `export LANG=C.UTF-8`로 보완할 수 있다.

하네스에 한글 문자열 입력 → 한글 한 글자 backspace → 출력 비교를 추가했다.
일반 SSH, tmux 최초 연결과 reconnect 모두 통과했다. OS IME의 조합 키 이벤트까지
자동화한 검사는 아니며, terminal의 UTF-8 입력·편집·출력 경로를 검증한다.
확장된 11개 GUI 시나리오 기록: `/tmp/fm-gui-euh9ax3s/events.jsonl`.

### 후속: SSH agent 감지

`4271c5c` 위의 변경으로 기존 화면·OSC title 감지를 SSH terminal에도 연결했다.
원격 helper 없이 Agents에 `flowmux:screen` 출처로 표시하고, 원격 PID나 session
정보를 로컬 hook/process 감지에 전달하지 않는다. 연결 종료와 channel 종료 시
이전 화면이 남아 있어도 항목을 제거한다. 화면·title 형식에 의존하는 추정이므로
정확한 원격 process 생존 여부나 native lifecycle event까지 보장하지 않는다.

확장된 실제 GUI/sshd 하네스 14개 검사를 통과했다. 원격 Python TUI로 Codex 화면과
Claude/Codex/OpenCode/Cline/agy title, 숨겨진 tab의 blocked 상태와 일반 화면으로의
전환, Disconnect 후 재등록 방지, reconnect와 channel 종료를 확인했다.
기록: `/tmp/fm-gui-yn4ej0ly/events.jsonl`.

별도의 SSH workspace에서 실제 Codex 0.153.4를 실행하고 모델 요청 없이
`codex / idle / flowmux:screen` 등록을 확인했다. 기록은
`/tmp/fm-gui-27e7qd7j/screen.txt`, `tree.json`에 있다.
daemon 단위 검사 170개를 통과했으며, SSH 화면 감지 후에도 로컬 PID/hook/lifecycle
보고와 process sweep이 해당 항목을 덮어쓰지 않는 회귀 검사를 포함한다.
격리 GUI 검사 676개(675 unit + 1 browser integration), GUI/daemon all-targets
clippy `-D warnings`, rustfmt와 diff whitespace 검사도 통과했다.

## 재현 방법

먼저 GUI·CLI를 함께 빌드한다. sshd, ssh, tmux, Xvfb, D-Bus와 프로젝트 GUI
빌드 의존성이 필요하다. 배포판 패키지를 임시 디렉터리에 추출한 경우 fixture의
`--sshd`, `--tmux`, `--library-path`로 실행 경로를 지정할 수 있다.

```bash
rtk cargo build -p flowmux -p flowmux-cli
rtk proxy python3 scripts/ssh-workspace-fixture.py --keep
```

fixture가 출력한 JSON의 값을 아래 인자에 넣고 별도 terminal에서 실행한다.
`--protected-pid`는 보존할 기존 GUI PID로 지정하고 여러 번 사용할 수 있다.

```bash
rtk proxy python3 scripts/test-ssh-workspace-gui.py \
  --ssh-config /tmp/FIXTURE/ssh_config \
  --remote-cwd '/tmp/FIXTURE/remote workdir' \
  --http-port HTTP_PORT \
  --protected-pid EXISTING_GUI_PID
```

하네스는 자신이 시작한 GUI만 종료한다. 완료 후 fixture 프로세스에 SIGTERM을
보내면 자신의 sshd와 private tmux 서버를 정리한다. 사용자 tmux 서버를 종료하지 않는다.
GUI 단위 검사는 별도 Xvfb/D-Bus/XDG 환경에서 FLOWMUX 환경을 제거하고 실행해야 한다.
그 환경의 PATH 앞에 저장소 `target/debug`를 넣어 이전 설치본 flowmuxctl이 선택되지 않게 한다.

## 현재 제한과 미검증 범위

- 원격 Files/Worktrees/Git, remote helper/hooks, local agent resume, 파일 drop 업로드는 지원하지 않는다.
- 원격 login shell은 POSIX 호환이어야 한다. cwd는 UTF-8 절대 경로 또는 생략만 지원한다.
- 재접속은 명시 동작이다. 일반 shell의 작업 지속은 보장하지 않으며 tmux도 원격 재부팅을 견디지 않는다.
- preview는 Linux에서만 지원한다. macOS에는 동등한 native 탐색 만료 방어가 구현되기 전까지 생성·복원을 허용하지 않는다.
- preview 복원은 새 forwarding의 root URL로 돌아간다. 탐색한 path/query/fragment는 보존하지 않는다.
- forwarding은 양 끝의 loopback TCP만 지원한다. 전체 browser를 원격 proxy로 동작시키지 않는다.
- password/MFA는 OpenSSH 입력 경로를 사용하지만 이번 실환경 검사에서 수행하지 않았다. macOS·다른 배포판·강제 crash 복구 전체도 미검증이다.
- host 설정 변경 UI, 원격 tmux 종료 UI, cwd 관측 시각 표시와 명령 실행 여부 불명 전용 표시는 후속 항목이다.

현재 Codex가 실행되는 GUI PID `1401465`와 해당 socket을 보존했다.
현재 창을 종료·재시작하거나 설치본을 교체하지 않았으므로, 이 창 자체에는 새 메뉴가 아직 적용되지 않는다.
