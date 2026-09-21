<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Ubuntu 22.04 지원 최종 계획

작성·검증일: 2026-09-21 · 기준 소스: `28ae700254ae06d8db0ef5781eae3b1855312b0c`

## 1. 결정

**Ubuntu 24.04 이상은 기존 네이티브 배포를 유지하고, Ubuntu 22.04 amd64는 GNOME 50 기반 Flatpak으로 추가 지원한다.** 기존 Rust/GTK 코드를 하위 버전으로 내리거나 호스트 시스템 라이브러리를 교체하지 않는다. Flatpak에서도 호스트 셸, 개발 도구, 에이전트 연동을 유지하는 것을 지원의 필수 조건으로 삼는다.

현재 상태는 **구현 경로와 주요 위험을 검증한 최종 실행 계획**이다. 완성된 22.04 지원 제품이나 전체 Flatpak 빌드 검증 결과가 아니다. 아래 G0–G5를 모두 통과하기 전에는 README의 지원 범위에 22.04를 추가하지 않는다.

배포 방식에 별도 제약이 제시되지 않아 Flatpak 허용을 전제로 결정했다. `.deb` 필수 요구가 생기면 3절의 별도 네이티브 라이브러리 번들 방안으로 다시 산정한다. 기본 아키텍처 범위는 현재 배포와 같은 amd64이며, ARM64·WSLg·다른 배포판은 이번 지원 선언에 포함하지 않는다.

## 2. 확인된 제약과 검증 결과

수치·버전·증거 범위는 [검증 JSON](ubuntu-22.04-support-evidence-2026-09-21.json)에 보존했다. 원시 로그와 임시 실험 파일은 `/tmp/flowmux-ubuntu2204-plan/`에 있다.

| 항목 | 직접 확인한 사실 | 계획에 반영한 내용 |
|---|---|---|
| GTK / libadwaita | 코드 요구 GTK ≥4.12, libadwaita ≥1.5. Jammy 저장소 후보는 4.6.9 / 1.1.7 | `install.sh`의 버전 검사만 제거하는 방안 제외 |
| VTE | 코드 요구 GTK4 VTE ≥0.76. Jammy에 `libvte-2.91-gtk4-dev` 없음. GTK3 VTE는 0.68 | GTK3 VTE를 대체재로 취급하지 않음 |
| WebKitGTK | 현재 Jammy 업데이트 저장소에는 `libwebkitgtk-6.0-dev` 2.50.4가 있음 | “22.04에는 WebKit 6 자체가 없다”는 가정 배제. 다른 의존성 제약은 여전히 남음 |
| 바이너리 ABI | 실제 22.04 사용자 공간에서 현재 `flowmuxctl --version`이 `GLIBC_2.39 not found`로 실패. Jammy glibc는 2.35 | 24.04/SDK 빌드 실행 파일을 호스트에 복사하지 않음 |
| 기존 Flatpak 런타임 | manifest의 GNOME 48은 원격 메타데이터상 2026-03-24 지원 종료 | 지원 중인 GNOME 50 사용. 런타임 수명 점검을 운영 항목에 추가 |
| GNOME 50 실물 | Platform을 내려받아 확인: GTK 4.22.4, libadwaita 1.9.3, WebKitGTK 2.54.0. **VTE와 ThorVG 공유 라이브러리 없음** | VTE·ThorVG를 `/app`에 직접 빌드·포함 |
| Rust 빌드 환경 | 기존 manifest는 Rust 경로만 추가하고 `sdk-extensions`를 선언하지 않음. 프로젝트 MSRV 1.93 | GNOME 50과 맞는 `rust-stable` extension 선언·설치·컴파일러 버전 검사 |
| 실제 host PTY | Jammy Python 3.10.12에서 기존 Python 브리지의 `/dev/tty`, resize, Ctrl-C, 종료 코드 23 통과 | 기존 브리지를 확장하는 경로 채택 |
| PTY 출력 정체 | 출력 소비를 멈추면 기존 브리지의 입력 전달 실패. 소비 재개 후 전달됨 | 양방향 비차단 큐 필요. 임시 시제품에서는 같은 검사 통과 |
| 기존 호환성 스크립트 | 22.04에서 낮은 패키지 버전을 확인할 뿐 앱을 빌드·실행하지 않음 | 이 검사를 “22.04 지원 통과” 근거로 사용하지 않음 |

**검증의 한계:** Jammy 검사는 SHA256을 공식 목록과 대조한 Ubuntu Base 22.04.5를 PRoot로 실행한 것이다. 호스트 커널은 24.04 환경의 커널이며 22.04 데스크톱 VM이 아니다. 현재 환경에서 실제 Flatpak 실행은 `bwrap: loopback: Failed RTM_NEWADDR: Operation not permitted`로 차단됐다. 따라서 portal, GPU, 실제 호스트 프로세스 경계, Flatpak 1.12.7에서의 GNOME 50 실행은 아직 입증하지 못했다. manifest schema 검사는 통과했지만 빌드 성공과 구분한다.

## 3. 대안 비교

| 대안 | 장점 | 실제 비용·제약 | 결정 |
|---|---|---|---|
| Flatpak + 지원 중인 GNOME 런타임 | 현대 GUI 스택 유지, 호스트 glibc와 분리, 기존 manifest/브리지 재사용 | VTE 번들, 호스트 실행·PID·IPC 경계 수정 및 실제 데스크톱 검증 필요 | **채택** |
| GTK 4.6 / libadwaita 1.1로 코드 하향 | 시스템 패키지 기반 설치 | libadwaita AlertDialog 등 API와 VTE 텍스트 추출 경로 재작성. 기능·테스트 분기 증가 | 제외 |
| 22.04에서 빌드한 전용 `.deb` + `/opt/flowmux` 라이브러리 | 호스트 셸·프로세스 접근이 자연스러움 | GTK/GLib/libadwaita/VTE와 전이 의존성, WebKit 보조 프로세스의 로더 경로 및 보안 업데이트를 직접 관리 | `.deb` 필수일 때만 별도 프로젝트 |
| AppImage | 단일 파일 배포 | glibc 하위 호환·WebKit 실행 프로세스·GUI 라이브러리 번들 문제를 자동 해결하지 않음 | 이번 경로에서 제외 |
| PPA·시스템 라이브러리 교체 | 초기 실행을 빨리 시도할 수 있음 | 사용자 GNOME 데스크톱과 의존성 충돌, 업데이트·복구 부담 | 제외 |
| 컨테이너 내부 개발 셸만 제공 | 패키징이 단순함 | 호스트의 Git/SSH/에이전트 설정과 개발 환경을 유지하지 못함 | 기능 동등성 미충족으로 제외 |

Flatpak은 빌드 호환성을 해결하는 배포 수단이다. 현재 host 연동 문제까지 자동으로 해결한다는 전제로 일정을 잡지 않는다.

## 4. 최종 구성과 수정 범위

### 4.1 빌드와 실행 의존성

`packaging/flatpak/com.flowmux.App.yml`을 보완한다.

1. `runtime-version: '50'`, `sdk: org.gnome.Sdk`, `sdk-extensions: [org.freedesktop.Sdk.Extension.rust-stable]`를 사용한다. 확인한 GNOME 50 SDK의 extension 기반 브랜치는 **25.08**이다. Rust 경로 추가뿐 아니라 extension을 실제 설치한다. `rustc --version`이 MSRV를 만족하는지 빌드 시작 시 검사한다.
2. VTE **0.84.1**을 최초 후보로 `/app`에 빌드한다. 공식 tarball SHA256: `aca1caa8478aebcdbb1d67897fb3511eb7601debae6810e16a15b6fa25f31ac8`. 이 버전의 Meson 소스에서 GTK ≥4.14, GLib ≥2.72, fmt ≥11, simdutf ≥6.2 요구를 확인했다. GNOME 50의 GTK/GLib는 조건을 충족한다.
3. VTE 빌드는 `-Dgtk3=false -Dgtk4=true -Dapp=false -Ddocs=false -Dgir=false -Dvapi=false -Dglade=false`로 필요한 GTK4 라이브러리에 집중한다. 접근성은 유지한다. simdutf와 fast_float 소스를 사전 공급하고 `--wrap-mode=nodownload`를 적용한다. 소스에 선언된 fallback은 simdutf `v8.2.0`, fast_float `v8.1.0`, fmt `12.1.0`이다. GNOME SDK가 fmt 11.2 헤더를 제공하면 재사용하며, 부족한 경우만 고정된 fallback을 공급한다. 실제 SDK의 헤더/`.pc` 존재 확인이 필요하다.
4. 이미지 뷰어 동등성을 위해 기존 `scripts/install-thorvg.sh`의 **v1.0.6, C API, CPU 엔진, loader 설정**을 Flatpak module로 옮긴다. 호스트의 `/usr/local`에 설치하지 않는다. `/app/lib/libthorvg-1.so` 로딩과 기존 이미지 형식을 검사한다.
5. 모듈 순서는 필요한 VTE 보조 라이브러리 → VTE → ThorVG → flowmux로 한다. Rust crates는 `Cargo.lock`에서 생성한 고정 source 목록 또는 `cargo vendor --locked`로 사전 공급하고, Cargo 설정까지 포함해 `cargo build --release --locked --offline`을 실행한다. Meson subproject의 네트워크 다운로드도 남기지 않는다.
6. 개발용 `type: dir`와 배포용 소스 입력을 구분한다. 배포는 검토된 소스 commit/archive와 checksum으로 고정하고 dirty 작업 트리를 넣지 않는다. 런타임/SDK/Rust extension의 실제 commit도 빌드 증거에 기록한다.
7. `/app/bin/flowmux`, `/app/bin/flowmux-md-viewer`, `/app/lib/flowmux/flowmuxctl` 및 기존 CLI 링크·아이콘·desktop 파일·라이선스를 유지한다. 새로 번들한 VTE/ThorVG/보조 라이브러리의 소스와 라이선스도 배포한다. AppStream 메타데이터는 별도 추가한다.

GUI 의존성이 없는 프로그램까지 모두 시스템에 설치하는 방식은 사용하지 않는다. GUI와 `flowmuxctl`은 런타임 안에서 실행하고, 호스트 측 연결에는 기존 Python 3와 얇은 실행 wrapper를 사용한다. 호스트에 Rust 컴파일러를 요구하지 않는다.

### 4.2 호스트 셸과 명령 실행

현재 `default_shell_argv()`의 Flatpak 경로는 기존 Python PTY 브리지를 실행한다. 그러나 명시적인 shell argv가 있으면 이 경로를 거치지 않는다. `workspace_view.rs`의 shell 존재 검사도 샌드박스 파일 시스템 기준이다.

- 기본 셸·사용자 지정 셸·새 탭·분할·복원·프로젝트 명령을 **같은 호스트 실행 경로**로 모은다. 호스트 셸의 존재 검사도 호스트에서 수행한다. argv를 문자열 shell 명령으로 합치지 않는다.
- 기본 셸은 기존처럼 호스트 passwd를 기준으로 정한다. 사용자가 지정한 shell/argv는 브리지 인자로 전달하고 그대로 `exec`한다. 공백·한글 경로를 포함한 cwd는 `flatpak-spawn --directory=...`에 명시한다.
- `flatpak-spawn`의 host 환경 전달은 상속을 가정하지 않는다. 소스상 D-Bus 환경 map은 `--env` 입력으로 만들어진다. `FLOWMUX_PANE_ID`, `SURFACE_ID`, `WORKSPACE_ID`, `TAB_ID`, 정확한 `SOCKET_PATH`, 터미널 식별·색상·커서 관련 변수를 명시적으로 전달한다.
- sandbox의 `PATH`, `LD_LIBRARY_PATH`, `FLATPAK_ID`, `/app/...` 실행 경로를 호스트에 그대로 전달하지 않는다. host PATH에 **호스트에서 접근 가능한 shim 디렉터리**만 추가한다. `FLOWMUX_BUNDLED_CLI_PATH`도 실제 실행 가능한 host wrapper를 가리키게 한다.
- host wrapper는 `flatpak run --command=flowmuxctl`로 들어오되, 원래 인스턴스의 socket과 pane/surface context를 명시적으로 전달한다. “현재 창” fallback으로 원래 대상 창을 잃어버리지 않게 한다. 사용자가 명시적으로 `flowmux fix`를 실행했을 때만 기존 정책대로 agent hook을 설치한다.
- Python 브리지는 양방향 비차단 read/write와 bounded queue를 사용하고 부분 write·EAGAIN·EOF를 처리한다. 출력이 막혀도 입력과 resize를 계속 처리해야 한다. close는 HUP 이후 유예 시간과 KILL을 가진다. 정상/시그널 종료 상태를 보존하고 호스트 자식을 남기지 않는다.

임시 큐 시제품은 입력 정체 개선의 가능성만 입증했다. EOF 직전 출력 보존, OSC를 포함한 큐 한도, 시그널 종료 변환, HUP 무시 자식 정리는 아직 생산 코드 수준으로 검증하지 않았다. 그대로 제품에 복사하지 않는다.

### 4.3 프로세스·SSH·개발 도구의 기능 동등성

샌드박스의 `/proc`로 호스트 에이전트의 생존·자손·포트를 판정하면 안 된다. `flowmux-procmon`에는 이 경로들이 있고 Git 실행·Codex app-server 등도 직접 `Command`를 사용한다.

- 브리지 시작 시 실제 host shell PID와 start-time을 surface/실행 세대에 연결해 등록한다. VTE가 관찰한 `flatpak-spawn` PID와 구분한다.
- Flatpak에서 필요한 process snapshot은 host 측 Python의 `/proc` 조회로 가져오고, 이름·활동 해석은 기존 Rust 규칙을 재사용한다. PID 재사용 방지를 위해 start-time을 함께 비교한다. native 경로에는 이 왕복을 추가하지 않는다. 초기에는 기존 polling 주기에 제한된 조회를 붙이고, 비용 측정 없이 별도 상주 daemon을 만들지 않는다.
- Git/worktree, tig, Codex app-server 등 **사용자의 설치·자격 증명이 필요한 명령**은 공통 host 실행 함수로 모은다. browser/editor의 내부 helper처럼 `/app`과 런타임에 속하는 명령은 sandbox에서 실행한다.
- SSH는 호스트의 `ssh`·`~/.ssh/config`·agent·ProxyCommand·ControlMaster 경로를 사용하는 방향으로 정한다. 현재 `ssh-pty-tee`의 cwd/env 격리 계약과 원격 세션·forward 수명 관리를 보존한다. 호스트 실행 wrapper 적용을 그 계약 아래에서 시험한다.
- hook 이벤트를 process snapshot보다 임의로 우선/후순위로 바꾸지 않는다. 강제 종료·완료·권한 대기·세션 재개 등 기존 Agent Bar 식별/생존 규칙이 같은 결과를 내야 한다.
- 프로젝트가 `$HOME` 밖에 있으면 전체 host filesystem 접근을 기본으로 늘리지 않고 해당 경로의 명시적 Flatpak 권한을 안내한다. 디렉터리 접근과 호스트 cwd 가능 여부를 함께 검사한다.

### 4.4 인스턴스 식별, IPC, 설정

현재 Flatpak은 공유 경로의 `flowmux-<process::id()>.sock`을 사용하고 stale 정리·알림 라우팅도 PID를 전제로 한다. 서로 다른 PID namespace에서는 같은 숫자의 PID가 존재할 수 있다. **다중 Flatpak 인스턴스 충돌 위험이며 아직 실제 충돌 재현은 하지 않았다.**

- Flatpak에서는 프로세스 수명 동안 고정된 인스턴스 식별자를 만든다. 공유 경로의 이름을 `instance-id + local PID` 또는 UUID로 고유하게 만들고 모든 PTY·hook에 동일한 경로를 전달한다. 각 호출 때 새 UUID를 만들지 않는다.
- socket 이름 변경만으로 끝내지 않는다. `main.rs`의 stale 정리, notification action target, 창 발견과 instance lock의 PID 의미를 함께 검토한다. 다른 namespace에서 `kill(pid, 0)`가 실패했다고 살아 있는 socket을 삭제하지 않는다. 연결 확인과 인스턴스 소유권 기준을 사용한다.
- 기존 숫자 PID 기반 native 라우팅은 유지하며, Flatpak 알림 target에는 안정적인 인스턴스 식별자를 사용한다. A/B 창에 각각 알림·CLI 요청을 보내 잘못 이동하지 않는지 검사한다.
- 공유 runtime 디렉터리는 사용자만 접근하도록 만들고, 심볼릭 링크·socket 교체는 기존 소유권과 생존 여부를 검사한다. 기존 `$HOME/.cache/flowmux`가 실제 host↔sandbox에서 같은 위치인지 설치된 앱으로 확인한다. 다르면 두 쪽에서 접근 가능한 app 전용 경로를 명시적으로 제공한다.
- 기본 앱 설정/상태는 Flatpak XDG 위치에 둔다. 기존 native 설정을 자동 덮어쓰지 않는다. 최초 migration은 백업 후 명시적인 일회성 import로 제공하고 schema 호환성을 확인한다. rollback은 실행 파일만 내려도 되는 경우와 상태 백업 복원이 필요한 경우를 구분한다.

### 4.5 화면·IME·권한

- 기존 Wayland + fallback X11, DRI, network, host 실행 권한을 기반으로 실제 22.04 GNOME 세션에서 검사한다. `--filesystem=host`나 전체 session-bus 접근을 문제 해결용 기본값으로 추가하지 않는다.
- host 명령 실행 권한이 있는 개발 터미널임을 설명한다. Flatpak 포장만으로 호스트와 강하게 격리된 보안 앱이라고 안내하지 않는다.
- IBus 조합 표시, 한글 입력·Backspace·Enter/Shift-Enter·연타·후보창을 실제 portal 경로에서 확인한다. 환경 변수만으로 Flatpak 분기를 켠 native 테스트는 대체 증거가 아니다. Fcitx5는 별도 지원 표시 전 추가 검사한다.
- Intel/AMD Mesa와 NVIDIA 대표 환경에서 browser/editor 스크롤·재그리기·GL을 점검한다. 기존 `FLOWMUX_WEBKIT_HW_ACCEL=never`, `WEBKIT_DISABLE_DMABUF_RENDERER=1`은 진단용 옵션으로 활용하되 모든 사용자에게 GPU 비활성화를 강제하지 않는다.
- 현재 코드의 WebKit 내부 sandbox 비활성화는 별도 기존 제약이다. 22.04 지원을 이유로 새로운 sandbox 우회를 추가하지 않는다. 런타임 변경 중 WebKit process sandbox 복원이 가능하면 별도 회귀 검증 후 적용하며, 이 계획의 실행 가능성 입증으로 보안 문제가 해결됐다고 주장하지 않는다.

## 5. 실행 순서와 완료 조건

역할은 구현 1명 + 리뷰/실환경 QA 1명 기준이다. 아래 소요일은 측정된 작업 시간이 아닌 계획 추정이며, runner·배포 저장소가 준비돼 있다는 전제다.

| 단계 | 작업과 주요 파일 | 통과 조건 | 추정 |
|---|---|---|---|
| G0 환경 확정 | 22.04.5 GNOME VM, GA 5.15/HWE 커널, distro Flatpak 1.12.7, GNOME 50 실행 확인 | 실제 `flatpak run`과 host-spawn/portal 동작. 호스트 GTK/GLib 교체 없음 | 1–2일 |
| G1 패키지 빌드 | `packaging/flatpak/`, Cargo sources, VTE/ThorVG modules | 깨끗한 SDK에서 네트워크 없는 빌드. installed app에서 모든 공유 라이브러리 로드, GUI/CLI/viewer 실행 | 2–4일 |
| G2 셸/IPC | `ghostty_pane.rs`, `workspace_view.rs`, `paths.rs`, `main.rs`, CLI wrapper/hook | 기본·사용자 지정 셸, duplex PTY, 종료 정리, 다중 창 routing, shim context 통과 | 4–6일 |
| G3 host 도구 | `flowmux-procmon`, `flowmux-vcs`, `ui/window/ssh.rs`, usage/provider 실행 경로 | Git/worktree/SSH/에이전트/usage의 native 대비 기능 동등성 | 3–5일 |
| G4 배포/갱신 | release workflow, AppStream, update origin, 설치 안내, signed repository | 신규 설치·업데이트·rollback·제거·라이선스 검증 | 2–3일 |
| G5 정식 검증 | 기존 GUI/IME/SSH harness 확장, 실제 22.04 X11/Wayland 검사 | 아래 수락표 전부 통과, native 24.04+ 회귀 없음 | 3–5일 |

합계 **15–25 작업일, 약 4–6주**를 초기 예산으로 잡는다. G0/G1에서 SDK 또는 Flatpak 최소 버전 문제가 확인되면 바로 재산정하며, host 연동을 생략해서 일정만 맞추지 않는다.

G0에서 Jammy 기본 Flatpak으로 실행되지 않으면 실제 필요한 최소 버전과 실패 원인을 먼저 확정한다. 검증된 업데이트 설치 경로가 없으면 기본 22.04 지원 선언을 보류한다. 사용자 시스템 GTK/GLib 교체로 우회하지 않는다. G1의 VTE 후보가 GTK/IME 회귀를 만들면 다른 **유지보수 중인** VTE 버전을 시험하고 증거를 갱신한다.

## 6. 출시 수락 검사

자동 검사는 SDK unit/integration + 설치된 Flatpak smoke, 사용자 동작 검사는 실제 22.04 데스크톱에서 수행한다. `FLATPAK_ID=...`만 주입한 실행은 실제 sandbox 검사를 대체하지 않는다.

| 영역 | 반드시 확인할 시나리오와 증거 |
|---|---|
| 설치/ABI | 깨끗한 22.04 amd64에서 호스트 GUI dev 패키지 없이 설치·시작. `/app`/runtime library closure 확인. `flatpak --version`, runtime/app commit, kernel 기록 |
| 기본 화면 | Xorg 및 Wayland에서 시작·분할·탭 이동·종료·상태 복원. 설치된 artifact로 실행한 화면/IPC 기록 |
| 터미널 | 기본 bash, 호스트에만 있는 zsh/fish·사용자 shell, 공백/한글 cwd, `/dev/tty`, job control, Ctrl-C/Z, resize, tig/vim/less. sandbox 셸로 대체되지 않는지 확인 |
| PTY 부하/수명 | 출력 읽기를 500ms 중단해도 독립 입력 수신 스레드와 resize가 정체 중 진행. 1MiB 출력 무손실·queue 상한·EOF 후 마지막 출력·종료 코드·시그널 상태·HUP 무시 자식 KILL. 반복 close 뒤 host orphan/zombie 없음 |
| context/다중 창 | 2개 이상 Flatpak 인스턴스에서 socket 고유성, pane/surface/workspace 정확성, CLI/hook/알림의 원래 창 전달. A 종료가 B의 socket/state를 제거하지 않음 |
| 에이전트 | Claude/Codex/OpenCode hook, shim, tmux 호환, process 탐지, 강제 종료, 권한 대기, 세션 재개, usage provider. native 대비 누락·오인식 없음 |
| SSH/Git | 호스트 SSH config·agent·ProxyCommand·forward·재연결·종료, Git/worktree·서명/자격 증명. 기존 SSH fixture를 installed Flatpak 대상으로 실행 |
| browser/editor/viewer | URL 로딩·browser CLI·에디터 편집/저장/미저장 종료·한글 파일명·Markdown·ThorVG 이미지. 내부 helper의 `/app` 실행과 외부 host 열기 구분 |
| IME/접근성/GPU | 실제 IBus 한글 preedit/확정/삭제/후보창, popup focus 복귀, accessibility, DPI/resize. X11·Wayland, Mesa·NVIDIA 대표 환경 |
| 배포/회귀 | 이전 버전→새 버전→이전 버전 rollback과 상태 보존. 제거 시 사용자 데이터 정책 명확. 기존 24.04 네이티브 full suite와 live GUI/SSH 검사 통과 |

CI의 GTK unit 실행은 `GDK_BACKEND=x11 GTK_A11Y=test G_DEBUG=fatal-criticals`와 private Xvfb/D-Bus를 사용한다. 실제 IME/접근성 검사는 해당 테스트 backend 대신 실제 session 서비스를 사용한다. Wayland 검사는 별도 실제 GNOME/지원 compositor 세션에서 수행한다.

`scripts/check-ubuntu-compat.sh`의 22.04 package-floor 검사는 정보 출력으로 유지하고, 새 artifact의 설치·실행 gate를 별도 추가한다. “낮은 라이브러리 버전 확인 성공”을 제품 실행 성공으로 집계하지 않는다. 검증 결과에는 소스/runtime/SDK commit, distro/Flatpak/kernel/GPU, 수행 명령·로그·화면을 연결한다.

## 7. 배포·업데이트·지원 운영

- 기존 `.deb`/tarball과 병렬로 Flatpak job을 추가한다. 24.04 네이티브 release를 Flatpak으로 강제 전환하지 않는다.
- beta는 GitHub Release의 서명된 `.flatpak` bundle과 checksum·대응 소스를 제공한다. 정식 채널은 프로젝트가 통제하는 **서명된 OSTree repository + `.flatpakref`**로 시작해 update/rollback을 검증한다. 저장소 URL·서명키 보관·키 교체·보존 기간은 G4에서 확정한다. Flathub 등재는 별도 검토/승인이 필요하므로 최초 지원의 선행 의존성으로 두지 않는다.
- `update/origin.rs`에 Flatpak 설치 출처를 명시한다. 이 설치에서는 source installer나 native `.deb` 업데이트로 전환하지 않는다. 앱의 업데이트 UI는 Flatpak 업데이트 경로를 안내한다. release bundle의 수동 업데이트와 repository 기반 업데이트를 혼동하지 않는다.
- 설치 문서는 Flatpak/portal/Python 3 선행 조건, 내려받기 크기, 추가 프로젝트 경로 권한, 선택적인 `flowmux fix`, 제거·데이터 보존을 설명한다. 설치가 hook 신뢰 설정을 자동 승인하지 않는다.
- 현재 관찰한 GNOME 50 Platform은 다운로드 약 **420MB**, 설치 약 **1.1GB**이며 앱·locale·GL extension은 별도다. 최종 설치 크기는 실제 artifact로 다시 측정한다.
- GNOME 런타임 EOL과 VTE/ThorVG 보안 업데이트를 월별 점검한다. runtime branch 고정은 재현성을 위한 것이며 보안 업데이트를 영구 정지하지 않는다. 최소 분기마다 다음 지원 runtime으로의 회귀 검사를 수행한다.
- 공식 Ubuntu 지원 표에서 22.04 표준 보안 유지보수 종료는 **2027년 5월**로 확인했다. 초기 지원 목표도 이 시점까지로 두고, 그 전에 종료/연장 정책을 공지한다. Ubuntu Pro/ESM 전용 지원은 자동으로 약속하지 않고 별도 결정한다. 현행 runtime을 22.04에서 더 이상 안전하게 실행할 수 없으면 공지 후 지원 범위를 조정한다.

## 8. 검증 후 개선한 결론

초기 가정은 “기존 Flatpak manifest를 빌드하면 22.04 지원이 된다”였다. 검증으로 **EOL runtime, 빠진 VTE, Rust extension 선언 누락, host ABI 불일치, Python 브리지의 backpressure 문제, custom shell/프로세스/다중 창 경계 위험**을 확인해 계획을 수정했다.

현재 증거는 **현대 GUI를 Flatpak runtime으로 분리하고 기존 host PTY 브리지를 보완하는 경로가 구현 가능한 방향**임을 뒷받침한다. 특히 원본/시제품 비교로 host PTY 부하 개선을 실제 22.04 Python에서 확인했다. 다만 완성된 앱의 전체 build와 실제 Jammy Flatpak/portal/GPU 검증은 남아 있다. 이를 이미 통과한 것으로 표시하지 않고 G0/G1 및 G5의 출시 차단 조건으로 명시했다.

이 계획의 완료는 계획서·근거·보완안의 완성을 뜻한다. **제품의 Ubuntu 22.04 지원 완료는 G0–G5 통과와 정식 artifact 배포 시점**이다.

## 9. 근거 위치

- 요구 버전: [GUI Cargo manifest](../crates/flowmux/Cargo.toml), [설치 검사](../install.sh)
- 기존 포장: [Flatpak manifest](../packaging/flatpak/com.flowmux.App.yml), [release workflow](../.github/workflows/release.yml), [호환성 검사](../scripts/check-ubuntu-compat.sh)
- host 경계: [PTY/브리지](../crates/flowmux/src/ui/ghostty_pane.rs), [shell 선택](../crates/flowmux/src/ui/workspace_view.rs), [공유 경로](../crates/flowmux-config/src/paths.rs), [시작/알림/정리](../crates/flowmux/src/main.rs), [process 조회](../crates/flowmux-procmon/src/lib.rs)
- 회귀 harness: [GUI/SSH](../scripts/test-ssh-workspace-gui.py), [터미널 렌더링](../scripts/test-terminal-rendering-gui.py), [실제 IBus](../scripts/test-terminal-ime-gui.py)
- [Flatpak runtime 지원 정책](https://docs.flatpak.org/en/latest/available-runtimes.html), [host-spawn 및 배포 명령](https://docs.flatpak.org/en/latest/flatpak-command-reference.html), [Ubuntu 지원 주기](https://ubuntu.com/about/release-cycle)
- [Ubuntu Base 이미지/체크섬](https://cdimage.ubuntu.com/ubuntu-base/releases/22.04/release/), [VTE 0.84.1 소스/체크섬](https://download.gnome.org/sources/vte/0.84/), [flatpak-spawn 소스](https://github.com/flatpak/flatpak-xdg-utils/blob/1.0.6/src/flatpak-spawn.c)

최소 재확인 명령(빌드/호스트 별 환경 준비 필요):

```sh
# 사용자 환경에 실제 등록된 Flathub remote에서 버전/EOL 확인
flatpak remote-info flathub org.gnome.Platform//48
flatpak remote-info flathub org.gnome.Platform//50
flatpak remote-info --show-metadata flathub org.gnome.Sdk//50

# 22.04 VM 또는 rootfs 안에서 실행; 앱 실행 검증과는 별개
getconf GNU_LIBC_VERSION
apt-cache policy libgtk-4-dev libadwaita-1-dev libwebkitgtk-6.0-dev flatpak
apt-cache show libvte-2.91-gtk4-dev

# schema 검사만 수행하며 build 성공을 의미하지 않음
flatpak-builder --show-manifest packaging/flatpak/com.flowmux.App.yml
```
