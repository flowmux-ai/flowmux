<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Ubuntu 22.04 네이티브 지원 계획

수정·검증일: 2026-09-22 · 앱 기준 소스: `28ae700254ae06d8db0ef5781eae3b1855312b0c`

## 1. 결정과 범위

**Ubuntu 22.04 amd64에서 빌드한 네이티브 `.deb`를 제공한다. 부족한 GUI 라이브러리는 `/opt/flowmux`에 앱 전용으로 설치하고, 셸·SSH·Git·에이전트는 기존 호스트 실행 경로를 유지한다.** Flatpak 설치, GNOME Flatpak runtime, `flatpak-spawn`을 요구하지 않는다. 24.04 이상은 기존 네이티브 배포를 유지한다.

이 문서는 2026-09-21의 Flatpak 제안을 대체한다. 기존 링크 유지를 위해 파일명은 유지했다. [이전 검증 JSON](ubuntu-22.04-support-evidence-2026-09-21.json)은 역사적 실험 기록이며 현재 배포 결정의 근거 문서가 아니다. 새 근거는 [네이티브 검증 JSON](ubuntu-22.04-native-support-evidence-2026-09-22.json)에 기록한다.

현재 완료 범위는 **구현 계획과 선행 호환성 조사**다. 전체 GUI 빌드·설치·실행 통과나 제품 지원 완료를 뜻하지 않는다. 아래 G0–G4를 통과한 뒤 지원 범위에 22.04를 추가한다. ARM64, WSLg, 다른 배포판은 이번 범위에서 제외한다.

## 2. 직접 확인한 제약

| 항목 | 확인 결과 | 적용 |
|---|---|---|
| glibc | Jammy는 2.35. 기존 호스트 빌드 `flowmuxctl`은 Jammy에서 `GLIBC_2.39 not found`로 실패 | GUI·CLI·daemon·viewer·번들 DSO 모두 Jammy에서 빌드 |
| CLI 재빌드 | Jammy에서 `cargo build --offline --locked -p flowmux-cli` 성공, `flowmux 0.10.0` 실행. 최대 요구 GLIBC_2.34, 의존성은 Jammy 시스템 라이브러리로 해소 | glibc 기준 재빌드 경로 확인. dev CLI 결과이며 GUI·release artifact 검증과 구분 |
| GTK / libadwaita | 앱은 GTK ≥4.12 / adwaita ≥1.5 요구. Jammy 후보는 4.6.9 / 1.1.7 | 설치 버전 검사 삭제나 시스템 패키지만으로 해결 불가 |
| VTE | 앱은 GTK4 VTE ≥0.76 요구. Jammy에 GTK4 개발 패키지 없음 | GTK4 VTE를 전용 빌드. GTK3 VTE 0.68로 대체 불가 |
| WebKitGTK | Jammy updates/security에 `libwebkitgtk-6.0-4` 2.50.4가 존재 | 시스템 WebKit 및 같은 패키지의 helper 재사용을 우선 검증 |
| GLib / Wayland | GTK 4.14.5 upstream은 GLib ≥2.76, Wayland ≥1.21, protocols ≥1.31 요구. Jammy는 2.72.4 / 1.20 / 1.25 | GTK만 복사하는 방식 불가. GLib·Wayland 전이 의존성까지 처리 |
| 빌드 도구 | GLib 2.80.5는 Meson ≥1.2 요구. 프로젝트 MSRV는 Rust 1.93 | 고정된 최신 빌드 도구를 빌더에만 공급. Rust 1.97 실행은 Jammy에서 확인 |
| Ubuntu 패치 소스 | noble-updates에서 GLib `2.80.0-6ubuntu3.9`, GTK `4.14.5+ds-0ubuntu0.10`, VTE `0.76.0-1ubuntu0.1` 확인 | LTS 패치 소스를 Jammy에서 재빌드하는 초기 후보로 사용 |

GTK upstream의 Pango 최소 요구는 1.50이지만 Noble 패키지의 Build-Depends는 ≥1.52다. Jammy Pango 1.50.6 재사용을 확정하지 않는다. 패치 적용 후 configure·링크·텍스트 회귀 결과에 따라 필요한 경우 Pango도 전용 빌드한다. libadwaita의 AppStream, VTE의 보조 C++ 라이브러리, GTK의 media/renderer 의존성도 같은 방식으로 확정한다.

검증 환경은 공식 SHA256을 대조한 Ubuntu Base 22.04.5 amd64를 PRoot에서 실행한 **Jammy 사용자 공간**이다. 실제 22.04 GNOME 데스크톱이나 GA 커널 검증이 아니다. 패키지와 도구는 임시 작업 디렉터리와 rootfs 안에서만 준비했으며 사용자 호스트의 GTK/GLib와 실행 중인 flowmux 창은 변경하지 않았다.

CLI 검증에서는 PRoot 5.1의 파일 상태 조회 오류를 5.4로 갱신해 해결했고, Rust 기본 `rust-lld` 충돌을 피해 Jammy GNU bfd 링커를 사용했다. 이 환경별 실패와 성공 명령은 검증 JSON에 보존했다. 정식 빌드는 PRoot 대신 Jammy VM 또는 검증된 CI 빌더를 사용한다.

## 3. 라이브러리 소유권과 배치

| 소유자 | 구성 | 원칙 |
|---|---|---|
| Jammy 시스템 | glibc, libstdc++ 등 ABI 기반, Mesa/DRI/NVIDIA, compositor, WebKitGTK/JavaScriptCore와 helper, TLS/인증서·D-Bus·IBus | Jammy 패키지와 보안 업데이트 사용. 더 최신 glibc나 GPU 드라이버를 앱에 복사하지 않음 |
| 앱 전용 | GLib/GObject/GIO, GTK4, libadwaita, GTK4 VTE, 필요한 Wayland 라이브러리, ThorVG 및 추가로 확인된 의존성 | `/opt/flowmux/lib`에 Jammy 빌드 결과 설치. 시스템 `/usr/lib`를 덮어쓰지 않음 |
| 빌드 전용 | Meson, Rust, wayland-protocols, 컴파일러·헤더·필요한 shader 도구 | 런타임 패키지에 불필요한 SDK를 넣지 않음 |

초기 기준 계열은 GLib 2.80 / GTK 4.14 / adwaita 1.5 / VTE 0.76으로 한다. 보안 패치가 없는 옛 upstream tarball을 영구 고정하지 않는다. Ubuntu LTS 패치 소스와 적용 목록을 관리하고 Jammy에서 재빌드한다. Noble `.deb` 바이너리를 Jammy에 설치하거나 Noble의 `dpkg-buildpackage`가 수정 없이 동작한다고 가정하지 않는다. 최소 도구 버전과 패키징 규칙을 Jammy에 맞춘다.

권장 배치는 다음과 같다. 실제 `flowmuxctl`은 GUI와 같은 디렉터리에 두어 기존 sibling 탐색을 재사용한다.

```text
/usr/bin/flowmux                 -> /opt/flowmux/bin/flowmux
/usr/bin/flowmuxctl              -> /opt/flowmux/bin/flowmuxctl
/usr/bin/flowmux-md-viewer       -> /opt/flowmux/bin/flowmux-md-viewer
/opt/flowmux/bin/                 GUI, CLI, viewer, daemon
/opt/flowmux/lib/                 앱 전용 공유 라이브러리와 필요 모듈
/opt/flowmux/share/               전용 schemas, resources, translations, licenses
/usr/share/applications/         기존 desktop 진입점
/usr/share/icons/                기존 아이콘
```

전용 라이브러리를 시스템 `ld.so.conf`에 등록하지 않는다. 실행 파일은 `$ORIGIN/../lib`, 각 전용 DSO는 자신의 전이 의존성을 찾는 `$ORIGIN` 기반 **DT_RUNPATH**를 가진다. 실행 파일의 RUNPATH만으로 손자 의존성이 해결된다고 가정하지 않는다. 하위 모듈 경로와 `dlopen()`으로 찾는 ThorVG도 검사한다. `LD_LIBRARY_PATH`를 전역 설정하거나 자식 셸에 상속시키지 않는다.

## 4. 구현 순서와 핵심 위험

### 4.1 Jammy 빌드와 의존성 확정

1. 깨끗한 22.04 amd64 빌더를 이미지 digest로 고정한다. updates/security 패키지 버전, Rust·Meson 버전, 소스 archive와 SHA256, 패치를 기록한다. 개발자 머신의 `target/`는 사용하지 않는다.
2. GLib → 필요한 Wayland/Pango 등 → GTK → adwaita/VTE → ThorVG → 앱 순서로 전용 prefix에 빌드한다. GTK의 X11·Wayland와 접근성은 유지한다. Vulkan/media 등 기능을 빌드 편의를 위해 조용히 끄지 말고 기존 기능과 비교해 필요한 설정을 확정한다.
3. Rust는 기존 `Cargo.lock`과 고정된 소스를 사용한다. 실제 Rust C 바인딩은 GIR 런타임을 요구하지 않으므로 introspection/docs/demo는 빌드 그래프 확인 후 제외할 수 있다. 관련 도구가 다른 단계에 필요하면 빌드 전용으로 공급한다.
4. 전체 ELF와 `DT_NEEDED`/RUNPATH를 점검한다. glibc 요구는 ≤2.35, GLIBCXX 요구는 설치 대상 Jammy libstdc++ 제공 범위 안이어야 한다. 부족한 경우 Jammy toolchain으로 다시 빌드하며 시스템 ABI 기반 교체로 우회하지 않는다.
5. `cargo-deb`의 기존 자산·의존성 목록을 Jammy variant로 분리한다. `$auto`가 전용 DSO를 Noble 시스템 패키지 의존성으로 생성하지 않는지 확인한다. Jammy 런타임 패키지 의존성과 앱이 포함한 DSO 목록을 실제 ELF 결과로 대조한다.

### 4.2 시스템 WebKit과 전용 GTK/GLib

GUI 프로세스는 전용 GTK/GLib와 시스템 `libwebkitgtk-6.0`을 함께 로드하고, WebKit helper는 Jammy 패키지의 실행 파일과 시스템 라이브러리를 사용하는 구성을 우선한다. 이는 **아직 검증해야 할 핵심 가설**이다. 라이브러리 파일 존재나 `ldd` 성공만으로 통과 처리하지 않는다.

G1에서 실제 URL/TLS 로딩, browser CLI, editor, helper 시작·종료·crash 복구, 미디어·GPU를 검사한다. GUI와 helper가 같은 WebKit 패키지 버전을 사용하고, `/proc/<pid>/maps`로 의도한 라이브러리만 로드했는지 확인한다. Ubuntu 보안 업데이트 후에도 반복한다. 기존 WebKit sandbox 제약을 해결했다고 주장하지 않으며 새 sandbox 우회를 추가하지 않는다.

이 조합이 실패하면 심볼·모듈·helper 경계에서 원인을 먼저 확정한다. 해결에 WebKit 자체 번들이 필요하면 비용·보안 유지보수 범위를 재산정하고 지원 선언을 보류한다. Flatpak으로 되돌리거나 브라우저 기능을 제외해서 통과시키지 않는다.

### 4.3 GIO, schemas, 입력기와 자식 환경

전용 prefix의 GLib는 시스템 TLS·dconf 모듈과 GTK schemas를 자동으로 모두 찾는다고 보장할 수 없다. GLib의 `gio_module_dir` 빌드 옵션으로 Jammy `/usr/lib/x86_64-linux-gnu/gio/modules` 사용을 우선 시험한다. TLS backend, 프록시, GSettings/dconf, 인증서 검증을 실제 호출로 확인한다.

GTK 전용 schemas는 전용 경로에서 컴파일하고 GUI·viewer에만 적용한다. `GSETTINGS_SCHEMA_DIR`나 `XDG_DATA_DIRS` 보완이 필요하면 **원래 사용자 값을 저장하고** 외부 자식 실행 시 복원한다. PTY뿐 아니라 Git, SSH, usage provider, Codex app-server, 외부 파일/URL 열기 등 모든 launch 경로를 감사한다. 사용자 환경을 일괄 삭제하지 않는다. 내부 helper와 호스트 외부 도구의 환경 계약을 구분한다.

IBus 모듈, 한글 preedit·확정·삭제·후보창, popup 후 focus 복귀를 실제 Jammy X11/Wayland에서 검사한다. 새 GTK를 사용한다는 사실만으로 IME 문제가 해결된 것으로 취급하지 않는다.

### 4.4 기존 네이티브 기능과 업데이트

기존 native shell/PTY, `/proc` 조회, SSH, Git, agent hook/shim, socket과 PID 라우팅을 재사용한다. 호스트 PTY 브리지, host process broker, namespace 대응 인스턴스 ID를 새로 만들 필요가 없다. 셸·CLI와 GUI 전용 라이브러리의 환경 분리만 필요한 범위에서 수정한다.

`current_exe()`가 `/opt/flowmux/bin/flowmux`를 반환해도 `dpkg -S`로 설치 출처가 `.deb`로 식별되어야 한다. 현재 update gate는 `.deb`를 release page로 보내므로 이를 재사용한다. 22.04 asset 이름을 `flowmux_<version>+ubuntu22.04_amd64.deb`처럼 구분하고 설치 안내·향후 자동 asset 선택에서 OS 버전을 확인한다. 기존 24.04 tarball이나 source installer로 잘못 전환하지 않는다. 두 OS 패키지의 Debian 버전 정렬과 업그레이드 경로도 테스트한다.

기존 XDG 설정·상태·IPC 경로를 유지한다. 패키지 교체 시 실행 중인 창을 강제 재시작하지 않으며 변경은 새 창부터 적용된다고 안내한다. 제거 시 `/opt/flowmux`의 패키지 파일만 제거하고 사용자 데이터를 보존한다.

## 5. 단계별 완료 조건

구현 1명과 리뷰/실환경 QA 1명 기준 **13–22 작업일, 약 3–5주**를 초기 추정으로 둔다. 완성된 GUI 빌드가 없는 상태의 추정이며 G1에서 WebKit 공존·전이 의존성에 문제가 있으면 재산정한다.

| 단계 | 작업 | 통과 조건 | 추정 |
|---|---|---|---|
| G0 | Jammy 빌더, source/patch lock, 의존성 목록 | 전체 toolchain 사용 가능, ABI 검사, 실제 데스크톱 VM 준비 | 1–2일 |
| G1 | 전용 GUI 스택과 모든 앱 바이너리 빌드 | 설치된 앱 시작, system WebKit 공존, GIO/TLS/schema/helper 검사 통과 | 4–6일 |
| G2 | `.deb` variant, launch 환경, desktop/CLI/update 연결 | clean install, 시스템 패키지 보존, 호스트 셸·도구 환경 보존 | 2–4일 |
| G3 | 실제 Jammy GUI/IME/SSH/GPU 및 기존 실패 재검토 | 아래 수락 검사 통과, baseline 대비 회귀 없음 | 4–6일 |
| G4 | release job·문서·업데이트·rollback·라이선스 | 검증 artifact와 대응 소스 공개 준비, 24.04 회귀 통과 | 2–4일 |

수정 대상은 기존 [release workflow](../.github/workflows/release.yml), [deb metadata](../crates/flowmux/Cargo.toml), [설치 검사](../install.sh), [호환성 검사](../scripts/check-ubuntu-compat.sh), 필요 시 [update origin](../crates/flowmux/src/update/origin.rs)과 실제 자식 실행 경로다. Jammy 빌드 recipe는 새로 필요하지만 런타임 플랫폼 추상화나 별도 daemon은 추가하지 않는다.

## 6. 출시 수락 검사

| 영역 | 필수 증거 |
|---|---|
| 설치/ABI | clean Jammy에 개발 패키지 없이 `apt install ./…deb`, GUI/CLI/viewer/daemon 실행, 전체 DSO symbol floor와 실제 로드 경로, 시스템 GTK/GLib 패키지·파일 보존 |
| 실행 환경 | 22.04 GNOME Xorg/Wayland, GA 5.15 및 지원 HWE 커널. Intel/AMD Mesa와 NVIDIA 대표 환경. PRoot·Xvfb는 이 검사를 대체하지 않음 |
| 터미널/도구 | bash·사용자 zsh/fish, 한글/공백 cwd, `/dev/tty`, Ctrl-C/Z, resize·job control·출력 부하·종료 정리, git/SSH/agents가 시스템 라이브러리와 기존 자격 증명 사용 |
| 다중 창 | 원래 창의 socket으로 CLI/hook/알림 전달, 2개 이상 인스턴스와 종료·복원·shim/tmux 호환 |
| WebKit/editor/viewer | HTTPS·인증서·프록시, browser CLI, editor 저장/미저장 종료, Markdown·ThorVG 이미지, helper 수명, 미디어·GPU 및 WebKit 패키지 업데이트 후 재검사 |
| IME/접근성 | 실제 IBus 한글 조합·Enter/Backspace·후보창, popup과 pane 사이 focus 왕복, AT-SPI·DPI/resize. Fcitx5는 별도 통과 후 지원 표시 |
| 배포/회귀 | 이전→신규→이전 `.deb` rollback과 상태 백업/복원, uninstall 데이터 보존, OS에 맞는 asset 안내, 기존 24.04 전체 검사와 live GUI 회귀 |

기존 [성능 구현 검증 기록](performance-implementation-2026-09-21.md)의 전체 GUI 불안정, VTE 동기화 출력, IME focus 왕복은 패키징으로 해결된 문제가 아니다. 기준/변경 빌드에서 같은 시나리오를 반복하고 실패 로그·화면·재현 빈도를 남긴다. 회귀는 수정하고, 기준에서도 발생한 실패는 별도 이슈·영향·출시 판단을 명시한다. flaky 검사를 삭제하거나 재시도 성공만으로 전체 통과를 선언하지 않는다.

GTK 자동 검사는 기존 Xvfb/private D-Bus와 `GDK_BACKEND=x11 GTK_A11Y=test G_DEBUG=fatal-criticals` 설정을 재사용한다. 실제 IME/접근성 검사는 실제 session 서비스를 사용한다. 제품 UI 수정은 실행 중인 테스트 인스턴스에서 같은 사용자 동작으로 확인한다.

## 7. 배포·보안 유지보수와 대안

- 기존 release workflow에 Jammy 전용 빌드/검증 job과 분리된 Cargo cache를 추가한다. 기존 24.04 artifact를 대체하지 않는다. 첫 배포는 기존 GitHub Release의 `.deb`와 checksum·빌드 provenance·대응 소스 방식으로 충분하다. 별도 APT 저장소는 초기 필수 작업으로 만들지 않는다.
- 번들한 GLib/GTK/adwaita/VTE/Wayland/ThorVG 등의 패치 감시·재빌드 책임은 flowmux에 있다. 시스템 apt 보안 업데이트가 `/opt/flowmux` 사본을 갱신하지 않는다. 월별 검토와 보안 공지 발생 시 우선 패치·재검증을 수행한다. 소스·patch 목록·SBOM·라이선스를 artifact에 연결한다.
- Ubuntu 22.04 표준 보안 유지보수 종료인 **2027년 5월**을 초기 지원 목표로 둔다. Ubuntu Pro/ESM 지원은 별도 결정한다. 시스템 WebKit 공급과 전용 라이브러리 유지보수가 안전하게 지속되지 않으면 지원 기간을 재검토한다.
- AppImage도 glibc·WebKit helper·라이브러리 번들 문제를 해결하지 않으므로 초기 `.deb`와 중복 구현하지 않는다. GTK 하향 포팅, PPA, Noble 바이너리 혼합, 시스템 GTK/GLib 교체, 새 glibc 번들은 제외한다.

## 8. 근거와 재검증

- [앱 버전 요구/패키징](../crates/flowmux/Cargo.toml), [CLI 탐색](../crates/flowmux/src/main.rs), [ThorVG 빌드](../scripts/install-thorvg.sh)
- [GUI/SSH 검사](../scripts/test-ssh-workspace-gui.py), [렌더링 검사](../scripts/test-terminal-rendering-gui.py), [실제 IBus 검사](../scripts/test-terminal-ime-gui.py)
- [Ubuntu Base 및 SHA256](https://cdimage.ubuntu.com/ubuntu-base/releases/22.04/release/), [Ubuntu 지원 주기](https://ubuntu.com/about/release-cycle)
- [Noble 업데이트 소스 목록](https://archive.ubuntu.com/ubuntu/dists/noble-updates/main/source/Sources.xz), [GTK 4.14 소스](https://download.gnome.org/sources/gtk/4.14/), [GLib 2.80 소스](https://download.gnome.org/sources/glib/2.80/)

재검증은 깨끗한 Jammy 사용자 공간에서 `getconf GNU_LIBC_VERSION`, `apt-cache policy`, `pkg-config --modversion`, `readelf --version-info`를 기록하고, 최종 `.deb`는 별도 Jammy 데스크톱 VM에 설치해 수행한다. 패키지 후보 확인과 CLI 실행은 전체 GUI 지원 증거와 구분한다. 이번 조사 로그·source metadata는 새 검증 JSON에 해시와 함께 연결했다.
