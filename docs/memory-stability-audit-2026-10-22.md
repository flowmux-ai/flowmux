<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Flowmux 메모리·종료 안정성 점검 — 2026-10-22

초기 점검 대상은 원격 main과 일치했던 `1f18cc27a21d0455db82323ef36f777669025148`
(0.11.3)이다. 범위는 일반 사용 중 메모리 사용량, 누수, 크래시와 비정상
종료다. 이후 사용자가 개선 구현을 승인하여 아래 세 변경을 각각 커밋했다.
설치된 실행 파일과 사용자 세션은 변경하지 않았다.

사용 중인 창은 PID 746615, Linux 시작 시각 토큰 62031502로 식별했다.
테스트는 별도 D-Bus, XDG 상태·설정·캐시·런타임 디렉터리와 명시적인
테스트 소켓을 사용했다. 기본 화면은 Xvfb이며, GL 비교 검사는 실제
데스크톱에 새 테스트 창을 열었다. 종료는 테스트가 만든 프로세스에만
적용했다. 빌드와 테스트에도 별도 systemd cgroup 메모리 상한을 적용했다.

환경: Linux x86_64, Rust 1.99.0, sanitizer nightly-2026-09-21,
GTK 4.14.5, VTE 0.76.0, WebKitGTK 2.52.6, libadwaita 1.5.0.
GUI 측정은 최신 debug 빌드로 실행했다. 기본 회귀 검사는 Cairo,
실제 데스크톱 비교는 GL 렌더러를 사용했다.
원시 로그와 재현 프로그램은 `target/memory-audit-20261022/`에 남긴다.
이 디렉터리는 Git 추적 대상이 아니며 `cargo clean`으로 없어질 수 있다.

## 개선 구현 및 재검증

구현 브랜치: `fix/memory-stability`.

| 커밋 | 변경 | 확인 결과 |
| --- | --- | --- |
| `2471450` | PNG/JPEG/WebP를 기존 Rust 디코더로 통일하고 64MiB 디코딩 예산 적용. 실패 시 오류와 명시적인 외부 앱 열기 버튼 제공 | 이미지 단위 테스트 17개, 512MiB cgroup의 실제 GUI 검증 통과 |
| `53101d1` | 문서·검색·복구 스냅샷의 실제 읽기량 제한. 저장 전 비교는 기존 내용 길이로 제한하고 복구용 파일 해시는 스트리밍 계산 | 에디터 일반 및 ASan+LSan 각각 96개 통과, 기존 3개 benchmark ignored. 실제 창과 Monaco 충돌 UI 검사 통과 |
| `a86f319` | ThorVG 1.0.6의 JerryScript enum 값 범위 수정. 설치 스크립트에 패치 적용, 네이티브 ASan·UBSan·LSan CI 추가 | 네이티브 84개 테스트/13,817 assertion과 Lottie 10개 fixture의 전 프레임 통과. 계측 라이브러리로 실제 GUI 재생·종료 통과 |

이미지 검증은 초기 OOM 입력과 같은 10,000×10,000 PNG(317,133바이트)를
터미널 링크로 열었다. 512MiB cgroup에서 GUI RSS는 198,572KiB에서
오류 표시 후 210,224KiB로 증가했다. 오류를 닫고 같은 파일을 32×32
PNG로 교체하자 뷰어가 정상적으로 열렸다. 뷰어 종료 후 RSS는
201,944KiB였고 IPC 응답 및 창의 정상 종료를 확인했다. 64MiB는
디코더의 예산이며 GUI 전체 RSS의 절대 상한이 아니다.

에디터 검증은 열린 파일을 외부에서 1GiB로 확장한 상황을 사용했다.
실제 GUI의 RSS 변화는 12KiB, 터미널 IPC 응답은 약 0.7ms였다.
Monaco 검사는 충돌 배너, 저장 거부, 미저장 텍스트 보존과 디스크 크기
유지를 확인했다. 기존 3초 지연 저장·동시 편집 및 FIFO I/O 검사도 통과했다.

ThorVG 검증은 같은 `test5.lot`을 `.lottie`에 넣어 실제 창에서 재생했다.
별도 mount namespace 안에서만 패치한 라이브러리를 읽도록 하고,
프로세스 maps의 inode로 로딩을 확인했다. 설치된 사용자 라이브러리는
덮어쓰지 않았다. 12초 재생과 정상 종료 중 UBSan/ASan 오류가 없었다.
이 GUI 실행은 기존 Fontconfig 종료 보고 때문에 leak detection을 끈
ASan 검사이며, 별도 네이티브 C API 검사는 LSan도 켜고 통과했다.

소스 설치 사용자는 ThorVG 설치 helper를 다시 실행해야 패치를 받는다.
현재 실행 중인 사용자 창과 기존 ThorVG 설치에는 이 변경을 적용하지
않았다. 호환성 probe는 이 패치의 설치 여부까지 구별하지 않는다.

변경 후 격리된 전체 workspace 회귀 검사는 1,978개 통과, 실패 0개,
기존 ignored 13개로 완료했다.
`cargo fmt --all -- --check`, workspace all-target Clippy(`-D warnings`),
설치 helper 테스트 12개 및 스크립트 구문 검사를 통과했다.

구현 검증 로그 (`target/memory-audit-20261022/`):
`fix-image-512m-2.log`, `fix-editor-gui.log`, `fix-editor-ui.log`,
`fix-editor-asan.log`, `fix-native-2.log`, `fix-lottie-gui-4.log`,
`fix-workspace-tests.log`. GUI 캡처는 `/tmp/fm-gui-5_d9ockt/rejection.png`,
`/tmp/fm-gui-6_2ws8h7/lottie.png`에 있다. 초기 GUI 검사 스크립트의 상태
조회 명령과 native GUI 격리 환경 설정 오류는 수정 후 재실행했으며,
실패한 시도를 제품 검증 성공으로 계산하지 않았다.

Fontconfig 종료 할당은 제품 소유 누수로 입증되지 않아 강제 전역 정리나
suppression을 추가하지 않았다. Markdown 입력/AST/HTML 및 state.json
전체 읽기에는 여전히 상한이 없다. 정상 문서·세션을 거부할 임계값의
근거가 없어 이번 변경에서 임의 제한을 도입하지 않았다. macOS 실기,
장시간 soak 및 GPU VRAM 검증은 수행하지 않았다.

아래는 수정 전 초기 점검 기록이다.

## 확인된 문제와 해석

### 1. 큰 래스터 이미지의 순간 메모리 증가와 OOM 종료 — 재현됨

일반 PNG(10,000×10,000, 단색 RGB, 파일 크기 317,133바이트)를 새 창의
터미널 파일 링크로 열었다. 원본 파일을 처리할 때 최고 RSS가
198,280KiB에서 594,204KiB로 증가했다. 표시 완료 시 212,088KiB,
뷰어 종료 후 201,612KiB로 감소했다. 압축 파일 크기가 작아도
디코딩 순간에는 약 387MiB가 추가로 필요했다.

동일 입력을 650MiB 테스트 cgroup에서는 열 수 있었으나, 512MiB에서는
커널이 **테스트 GUI PID 692137을 OOM 종료**했다. 커널 로그의
`CONSTRAINT_MEMCG`와 서비스 `fm-audit-20261022-image-512m.service`가
테스트 상한에 의한 종료임을 확인한다. 실제 사용자 리포트의 원인이
이 경로라는 뜻은 아니다. 재현 후 원래 사용자 GUI의 동일 시작 토큰을
다시 확인했다.

원인 경로는 [`image_viewer.rs`](../crates/flowmux/src/ui/image_viewer.rs)의 `load_viewer_content` →
`render_native_with_size` → `tvg_picture_load`다. 디코딩 뒤에 화면에
맞는 크기를 적용한다. `render_raster`도 `.decode()` 뒤에 축소한다.
뷰어 크기 1200×900 제한은 원본 디코딩 메모리 제한이 아니다.
실행 위치도 GUI 프로세스이므로 디코딩 실패가 창 전체에 영향을 준다.

권장 후속 작업: 디코딩 전에 원본 픽셀 수·예상 메모리를 검사하고,
큰 정상 이미지에는 명확한 안내 또는 메모리가 제한된 별도 디코더를
사용한다. 이 조사에서는 동작을 바꾸지 않았다.

증거: `runtime.log`, `image-lowmem.log`, `image-512m.log`,
`image-512m-kernel.log`, `image-probe.py`.

### 2. ThorVG 1.0.6 Lottie 표현식의 UBSan 오류 — 재현됨

설치 스크립트가 선택하는 ThorVG 태그 `v1.0.6`
(`73045df5398690eb1c6c0946e0b2301032337776`)를 Clang 18의
ASan+UBSan으로 별도 빌드했다. CPU 렌더러, 전체 로더, C API와
Lottie 표현식을 활성화했고 OpenMP는 포함하지 않았다.

기존 upstream `Lottie Coverages` 테스트에서 JerryScript의
`ecma-objects.cpp:1291`이 `ecma_object_base_type_t`에 허용되지 않는 값
10을 읽는다는 오류로 종료했다. Flowmux와 동일한 C API 호출 순서
(load_data → set_size → canvas_add → set_frame → draw/sync)를 사용하는
별도 프로그램에서도 upstream `test5.lot`으로 같은 오류가 발생했다.
나머지 `test3.lot`, `test4.lot`, `test6.lot`–`test12.lot`은 통과했다.

이는 실제 Lottie 렌더링 경로의 미정의 동작 진단이다. 일반 설치본에서
그 파일이 반드시 크래시한다거나 사용자 OOM 리포트를 설명한다고
단정하지 않는다. Flowmux의 기존 단순 Lottie ASan/LSan 테스트 5개는
통과했으므로 표현식이 포함된 입력을 별도로 다룰 필요가 있다.
초기 점검 당시 [sanitizer CI](../.github/workflows/sanitizers.yml)는 ThorVG를 일반 빌드로 설치하고 Rust를 계측한다.
native UBSan 빌드와 표현식 fixture를 추가하는 것이 필요한 후속 검증이다.

Lottie 태그를 제외한 native 테스트는 78개, 13,670개 assertion을
통과했다. 최초 native 전체 실행은 실패로 유지하며, 제외 실행을
전체 통과로 해석하지 않는다. UBSan 오류를 숨기는 suppression은 없다.

증거: `native.log`, `native-rest.log`, `lottie-fixtures.log`,
`lottie-5.log`, `lottie-capi.cpp`, `native.sh`.

### 3. GUI 종료 시 LSan 보고 — Fontconfig 할당 경로

`gui-leaks releases --no-fail-fast -- --test-threads=1`에서
객체 해제를 검증하는 20개 테스트의 assertion은 모두 통과했지만,
프로세스 종료 시 23,235개 할당, 834,473바이트의 누수가 보고되었다.
모든 보고의 첫 할당자 다음 프레임은 `libfontconfig.so.1`이다.

따라서 GUI LSan 실행은 **실패**다. Flowmux를 링크하지 않은 GTK C
대조 프로그램에서 label/window를 20회 생성·파괴한 경우에도
Fontconfig 경로에서 378,297바이트/10,606개 할당이 보고되었다.
이 대조는 시스템 GUI 라이브러리에서도 종료 시 같은 종류의 보고가
발생함을 확인한다. 두 실행의 양을 단순히 빼서 Flowmux 누수량으로
계산할 수는 없다. 아래 반복 측정에서도 무제한 증가가 관찰되지는
않았다. GUI 전체의 누수 부재를 증명한 것으로 표현하지 않는다.

증거: `gui-leaks.log`, `gtk-control.c`, `gtk-control.log`.

## 반복 사용과 종료 동작

테스트용 셸은 사용자 초기화 파일을 읽지 않았다. 터미널마다 약
2.52MB의 정상 텍스트(20,000줄)를 출력하고 완료 표시를 실제 화면에서
확인한 뒤 탭을 닫았다. 이후 로컬 HTML 브라우저 열기·snapshot·닫기를
반복했다. 최종 테스트 창은 정상 종료했다.

| 시점 | GUI RSS (KiB) | GUI FD | 관찰 |
| --- | ---: | ---: | --- |
| 시작 | 199,628 | 23 | 테스트 셸 1개 |
| 터미널 5회 종료 | 203,072 | 23 | 출력 처리 완료 |
| 터미널 30회 종료 | 203,092 | 23 | 이후 증가 거의 없음 |
| 브라우저 5회 종료 | 246,884 | 35 | 엔진 초기화 비용 포함 |
| 브라우저 20회 종료 | 247,924 | 35 | WebKit 하위 프로세스 없음 |
| 30초 안정화 | 223,548 | 34 | 유지된 자식은 셸과 PTY proxy뿐 |

이 유한 반복에서는 지속적인 탭·프로세스·FD 누적이 관찰되지 않았다.
장시간 누수 부재나 임의의 웹 콘텐츠에 대한 메모리 상한을 증명하지는
않는다. PSS는 다른 병렬 테스트와 공유하는 라이브러리 영향으로
변동하므로 비교 표는 RSS를 사용했다.

첫 실험의 `data:` URL이 검색 URL로 정규화되어 readiness가 실패했다.
테스트를 로컬 `file://` fixture로 수정하고 전체 반복을 다시 실행했다.
첫 실행은 제품 실패나 성공 증거로 사용하지 않는다.

증거: `soak.log`, `soak.py`, `/tmp/fm-gui-kcg4ea7m/memory.json`.

실제 데스크톱 `:0`의 별도 GL 테스트 창에서도 동일한 터미널 30회와
브라우저 20회 반복 및 정상 종료를 확인했다. 렌더링 중 테스트 프로세스가
`/dev/dri/renderD128`을 연 것도 확인했다. RSS는 시작 239,876KiB,
터미널 30회 후 245,092KiB, 브라우저 5회/20회 후
287,028/287,204KiB, 30초 안정화 후 282,508KiB였다.
브라우저 반복 중 FD는 51개로 일정했고 마지막에는 50개였다.
종료된 WebKit 하위 프로세스가 누적되지 않았으며 원래 사용자 창은
유지되었다. GL 초기화 후 남는 메모리 전체를 누수로 분류할 근거는 없다.
증거: `desktop-gl.log`, `/tmp/fm-gui-2p_85vin/memory.json`.

실시간 회귀 검사에서는 파일 대화상자 반복 종료·다운로드, PTY 생성
실패 복구, 두 IPC 소켓의 FD 부족 복구, 3초 지연 저장 중 UI 응답과
동시 편집 보존, 숨긴 브라우저·편집기 탭의 지연 복원이 통과했다.
각 결과와 테스트별 임시 디렉터리는 `runtime.log`에 있다.

## 코드 검토 범위와 남은 메모리 한계

| 영역 | 초기 점검 당시 보호 또는 수명주기 | 당시 한계 |
| --- | --- | --- |
| PTY 출력·OSC | 출력/입력 pending 기준 256KiB, 이벤트 큐 16개, backpressure·coalescing | 총 터미널 수에 따른 사용량은 별도 |
| 터미널 scrollback | 기본 5,000줄, 설정 최대 1,000,000줄; 저장 내용 256KiB 제한 | VTE 런타임 메모리는 저장 바이트 제한과 다름 |
| IPC·GTK bridge | IPC 행 1MiB, 연결 64+16, 동시 mutation 32; GTK 큐 64+8+8 | 메시지 개수 제한이 모든 payload 메모리의 단일 상한은 아님 |
| 브라우저 | weak NetworkSession 공유, 종료 시 signal 해제, WebView 해제 테스트 | 페이지 자체와 WebKit 캐시는 프로세스별 사용량 측정 필요 |
| 알림·다운로드 | 알림 50개, 완료 다운로드 이력 50개 정리 | 진행 중인 다운로드는 이력 상한으로 중단하지 않음 |
| 편집기·복구·검색 | 문서 16MiB, 검색 파일 2MiB, 복구 파일 metadata 크기 검사 | `fs::read`/`read_to_end` 중 파일이 커지면 읽기량이 상한을 넘을 수 있음; 이 조사에서 실제 종료는 재현하지 않음 |
| Markdown | 별도 viewer 프로세스, 파일 전체 읽기 후 HTML 생성 | 입력 파일·AST·HTML 총량 상한 없음; 큰 정상 문서에서 사용량 증가 가능 |
| 상태 복원 | 저장 scrollback 제한, 숨긴 웹 탭 지연 생성 | state.json 전체 문자열·JSON 트리를 메모리에 읽음 |
| 이미지·Lottie | Lottie JSON 실제 읽기 16MiB 제한, canvas/animation/engine 순서대로 해제 | 원본 래스터 디코딩 메모리와 native 라이브러리 내부 동작은 별도 |
| macOS native view | Drop·타이머·delegate·script handler 정리 경로와 기존 회귀 테스트 확인 | macOS 실행 환경이 없어 이번 실행 검증에는 포함되지 않음 |

기존 로컬 crash report에서 반복된 Tokio `EnterGuard` 종료 순서 panic은
최신 코드의 `notification_coordinator::in_tokio_runtime`이 각 poll 안에서
guard를 해제하도록 수정되어 있다. 현재 전체 일반 테스트에서도 해당
회귀 검사가 통과했다. 과거 로그만으로 최신 버전의 미해결 문제라고
분류하지 않는다.

Rust panic hook은 OOM killer의 SIGKILL이나 네이티브 segfault를 수집하는
장치가 아니다. 따라서 crash 디렉터리에 기록이 없다는 사실만으로
메모리 부족 종료를 배제할 수 없다.

## 실행 결과

| 검사 | 결과 |
| --- | --- |
| `cargo fmt --all -- --check` | 통과 |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | 통과 |
| `cargo build --workspace --locked` | 통과 |
| 격리된 전체 일반 workspace 테스트 | 1,973 통과, 0 실패, 기존 13 ignored |
| 헤드리스 ASan+LSan, Rust 표준 라이브러리 포함 | 1,143 통과, 0 실패, 기존 9 ignored |
| 추가 ASan stress | 깊이 1,001 pane tree, 16MiB OSC 스트림 통과 |
| 기존 Lottie ASan+LSan | 5 통과 |
| GUI 전체 ASan (leak detection 제외) | 830 통과, 0 실패, 기존 4 ignored |
| GUI LSan 객체 해제 검사 | assertion 20 통과; Fontconfig 누수 보고로 종료 실패 |
| native ASan+UBSan | Lottie 표현식에서 실패; 나머지 결과는 위에 명시 |

Rust nightly는 `-Zsanitizer=undefined`를 지원하지 않는다. 이를 실제
컴파일러로 확인했고, UBSan은 C/C++ native 경로에 적용했다. Rust의
ASan과 debug assertion/UB precondition 검사는 UBSan 전체의 대체재가
아니다. 배포된 GTK/VTE/WebKit 전체를 sanitizer로 재빌드하지 않았다.

실제 사용자 리포트의 OS·버전·오류 메시지·직전 작업은 아직 제공되지
않아 해당 리포트와 발견 사항의 인과관계는 미확정이다. macOS 실기,
사용자 작업을 그대로 재현한 장기 soak, GPU VRAM 계측, 전체 데스크톱 엔진 내부의
누수 부재는 이번 결과로 주장하지 않는다.
