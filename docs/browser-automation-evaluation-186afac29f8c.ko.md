<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Flowmux 브라우저 조작 검증 평가

평가일: 2026-10-26 KST. 대상 커밋: `186afac29f8cdd4482063706467726101856c54f`.

**후속 조치 완료:** 같은 날 P1의 탐색 완료 회귀 검사를 추가하고 P2의 중첩 화면 설정을 수정한 뒤, X11·Wayland 전체 검사가 모두 통과했다. 제품의 브라우저 동작과 API는 변경하지 않았다. 아래 최초 평가 수치와 실패 기록은 보존하며, 현재 상태는 [P1 P2 후속 조치](#p1-p2-후속-조치)를 참고한다.

**Linux X11의 기존 브라우저 기능 검사, 로컬 제어 시나리오 2,100회, 공개 사이트 고정 시나리오 15회가 통과했다. Wayland 전체 검사는 통과하지 못했다.** Wayland에서는 권한·입력·파일 선택 검사가 통과했지만, 800×600 중첩 데스크톱의 화면 밖으로 향하는 슬라이더 드래그가 경계에서 멈췄다. 자연어 업무를 수행하는 에이전트의 성공률·토큰·비용은 이번 결과에 포함하지 않는다.

[평가 방식 분석](browser-automation-performance-analysis.ko.md)의 기능 정확성, 조작 경로 지연, 업무 완료 성능을 구분해 적용했다. ego(lite)와 외부 에이전트 브라우저의 평가 구조를 참고했으며, 서로 다른 환경의 발표 수치와 속도 순위를 비교하지 않는다. 제품 소스와 기존 검사 파일은 수정하지 않았고, 반복 측정용 [실행기](../scripts/benchmark-browser-control.py)와 평가 자료만 추가했다.

## 판정과 범위

| 평가 | 실행 범위 | 결과 |
| --- | --- | --- |
| 브라우저 단위 검사 | `cargo test -p flowmux-browser --locked` | **34 passed**, 종료 코드 0 |
| X11 실제 GUI 기능 검사 | 기존 `test-browser-control.py` 전체 | **PASS**, `BROWSER_CONTROL_OK`, 종료 코드 0 |
| 로컬 제어 지연 | 21종 × 본 측정 100회 | **2,100/2,100 통과**, 준비 실행도 105/105 통과 |
| 공개 사이트 고정 workflow | 5종 × 3회 | **15/15 통과**, 준비 탐색 시간 제외 |
| Wayland 실제 GUI 기능 검사 | 기존 검사 및 초기 화면 전처리를 추가한 임시 사본 | **FAIL**, 원본은 권한 창 탐지, 전처리 후에는 슬라이더 드래그에서 중단 |
| 자연어 에이전트 업무 | 과제 선택·계획·복구·rubric·모델 사용량 | **미실행** |
| macOS 및 엔진 벤치마크 | WKWebView, Speedometer 등 | **미실행** |

반복 실행의 100%는 이 고정 시나리오의 관측 결과다. 독립적인 2,100개 업무에 대한 성공률이나 일반 웹 업무의 성공 확률로 해석하지 않는다. Wayland에서 중단 이후의 검사도 미실행으로 취급한다.

## 빌드와 실행 환경

| 항목 | 조건 |
| --- | --- |
| 빌드 | `cargo build --release --locked -p flowmux -p flowmux-cli` |
| OS | Linux x86_64, 커널 7.0.0-34-generic, Ubuntu noble 계열 라이브러리 |
| CPU 및 메모리 | Intel Core Ultra 9 185H, 논리 CPU 22개, RAM 약 64 GB |
| Rust | 1.99.0 |
| 브라우저 및 UI | WebKitGTK 2.52.6, GTK 4.14.5 |
| X11 | Xvfb 1280×1024×24, `GSK_RENDERER=cairo` |
| Wayland | Xvfb 1440×1000 안의 GNOME 중첩 데스크톱, 실제 중첩 출력 800×600, 소프트웨어 GL |
| 프로필 | 검사별 독립 runtime/config/data/state/cache, 로컬 반복 측정 중에는 같은 프로필과 GUI 유지 |
| 네트워크 | 로컬 HTTP fixture 및 공개 HTTPS 사이트; 네트워크 지연을 고정하지 않음 |
| 미디어 | 임시 경로의 ffmpeg/ffprobe 7.0.2, 임시 추출한 GStreamer libav 및 의존 라이브러리 |

GUI 바이너리 SHA-256은 `ef473b7b87eb7c3249cd34ab5f79467df3c17a9287670a03d822bcdd830bf7aa`, CLI는 `85ca438a0a3bf2bb6a380660abf982bf5e98f5a7ebb8e3d641187c28e24d5f7d`다. [환경 원문](evaluations/browser-186afac29f8c/environment.json), [빌드 로그](evaluations/browser-186afac29f8c/build.log), [의존성과 소스 해시](evaluations/browser-186afac29f8c/reproduction.json)를 보존했다.

사용자 앱을 재시작하거나 기존 세션에 입력하지 않았다. 필요한 라이브러리는 `/tmp`에 추출했으며 시스템 패키지 설치나 WebKit sandbox 해제는 하지 않았다. CPU 고정, 전원 모드 고정, 다른 프로세스의 부하 제거는 하지 않았으므로 이 값은 단일 호스트·단일 실행의 기준값이다.

## 기능 정확성

X11 전체 검사는 실제 release GUI와 CLI로 다음 상태를 확인했다.

- native 클릭·더블클릭·hover·키 입력·한글 및 편집 영역 입력의 값과 trusted 이벤트.
- 폼 일괄 입력, receipt, 작업 소유권·사용자 인계, 잘못된 대상·stale ref 거절, DOM을 변경하지 않는 snapshot.
- iframe·Shadow DOM·탭 지정·팝업, 파일 선택과 다운로드 내용, dialog ID 및 응답.
- 미디어 재생·seek·mute, WebGL과 디코딩된 영상이 포함된 캡처, 전체 문서·요소 캡처와 녹화.

[최종 X11 로그](evaluations/browser-186afac29f8c/functional-x11-codecs.log)에 `PASS:` 메시지 23개와 `BROWSER_*_OK` 마커 10개가 있다. 서로 중첩된 묶음이므로 33개의 독립 테스트로 합산하지 않는다. [브라우저 단위 검사 로그](evaluations/browser-186afac29f8c/browser-unit.log)는 RTK가 요약한 결과다.

처음 두 X11 실행은 환경 준비 과정에서 실패했다. Xvfb에서 GNOME 파일 선택 portal이 선택된 문제는 사설 설정에서 GTK portal을 지정해 해소했다. 이후 미디어 준비 실패는 H.264 디코더가 없는 환경에서 발생했으며, 임시 GStreamer libav를 로드한 뒤 원본 전체 검사가 통과했다. [첫 실행](evaluations/browser-186afac29f8c/functional-x11.log)과 [디코더 추가 전 실행](evaluations/browser-186afac29f8c/functional-x11-isolated.log)도 남겼다.

## 로컬 제어 지연

각 시나리오는 준비 실행 5회 후 본 측정 100회를 수행했다. 본 측정 시각은 16:44:52–16:48:12 KST다. 시간은 `perf_counter_ns`로 측정한 **CLI 프로세스 실행부터 응답 파싱·기록·최종 상태 확인까지의 workflow 시간**이다. 상태 초기화·사전 ref 획득은 제외한다. 개별 브라우저 이벤트 자체의 지연이나 모델 판단 시간은 아니다.

p50은 중앙값, p95는 성공 표본 100개의 nearest-rank 값이다. 아래 출력 크기는 측정 구간에 호출한 CLI의 stdout UTF-8 bytes 합계의 평균이며, `--json` 바깥 봉투를 포함하고 RTK 필터를 적용하기 전 값이다. 토큰 수로 환산하지 않았다. [요약 JSON](evaluations/browser-186afac29f8c/local-summary.json), [표본 JSONL](evaluations/browser-186afac29f8c/local-samples.jsonl.gz), [명령 JSONL](evaluations/browser-186afac29f8c/local-commands.jsonl.gz)에서 각 실행을 확인할 수 있다.

원시 명령 로그에는 시나리오 사이 준비 명령 15개가 직전 표본의 `stage` 이름을 유지한 기록상 오류가 있다. 시간·호출 수·bytes는 준비 명령 전에 확정돼 표의 집계에는 들어가지 않았다. 원시 로그를 재집계할 때는 각 표본의 `calls`만큼 해당 stage의 앞쪽 명령을 사용해야 한다. 모든 표본의 bytes와 호출 수를 이 경계로 대조했다. 재사용 실행기에는 측정 종료 후 stage를 초기화하는 수정과 self-check를 추가했으며, 기존 원시 로그는 그대로 보존했다.

| 시나리오 | p50 ms | p95 ms | 평균 출력 bytes | 평균 CLI 호출 |
| --- | ---: | ---: | ---: | ---: |
| 20개 버튼 full snapshot | 4.78 | 6.00 | 8,853 | 1 |
| 20개 버튼 compact snapshot | 4.87 | 6.29 | 6,623 | 1 |
| 20개 버튼 중 정확한 이름 find | 4.49 | 5.27 | 536 | 1 |
| 500개 버튼 full snapshot | 489.69 | 505.73 | 203,943 | 1 |
| 500개 버튼 compact snapshot | 489.83 | 498.63 | 146,751 | 1 |
| 500개 버튼 중 정확한 이름 find | 10.46 | 14.56 | 536 | 1 |
| native click 및 결과 확인 | 39.55 | 41.67 | 53 | 2 |
| receipt click 및 별도 결과 확인 | 39.51 | 41.83 | 727 | 2 |
| fill 및 값 확인 | 9.32 | 11.49 | 58 | 2 |
| native type 및 값·trusted 확인 | 13.05 | 15.68 | 94 | 3 |
| native press 및 선택 범위 확인 | 9.57 | 12.34 | 48 | 2 |
| 5개 필드 개별 입력 및 확인 | 29.09 | 33.13 | 173 | 6 |
| 5개 필드 form 일괄 입력 및 확인 | 16.00 | 19.09 | 925 | 2 |
| 250ms 갱신에 typed wait | 268.79 | 271.46 | 79 | 2 |
| 250ms 갱신에 snapshot 반복 조회 | 260.71 | 285.43 | 23,182.35 | 10.15 |
| stale ref 거절 및 잘못된 쓰기 없음 확인 | 7.66 | 10.14 | 152 | 2 |
| same-origin frame 필드 입력 및 확인 | 9.60 | 11.64 | 54 | 2 |
| open shadow 필드 입력 및 확인 | 9.04 | 12.40 | 54 | 2 |
| surface 1개 관찰 묶음 | 6.75 | 8.97 | 8,511 | 1 |
| surface 2개 동시 관찰 묶음 | 7.23 | 9.83 | 17,022 | 2 |
| surface 4개 동시 관찰 묶음 | 9.12 | 10.81 | 34,044 | 4 |

500개 버튼 페이지에서 `compact`는 출력 크기를 **28.04% 줄였지만 시간은 거의 같았다**. 두 snapshot 모두 필요한 버튼 ref 500개를 반환했으나 **`truncated: true`**였다. 여기서 통과는 대상 ref가 있다는 의미이며 전체 페이지 정보가 빠짐없이 반환됐다는 뜻은 아니다. 버튼과 본문을 함께 생성한 큰 페이지의 관찰 비용은 여전히 약 0.49초다.

같은 큰 페이지에서 `find --role button --name 'Target 0' --exact`의 중앙값은 full snapshot보다 **97.86% 작았다**. 특정 이름을 이미 아는 경우에 한정된 이점이다. 전체 내용을 읽는 작업을 대체하는 동일 정보량 비교로 볼 수 없다.

5개 필드를 `form`으로 입력하면 중앙값이 **45.00% 감소**하고 측정 구간의 호출은 6회에서 2회로 줄었다. 필드별 검증 결과가 포함돼 응답 bytes는 오히려 늘었다. typed wait는 snapshot 반복 조회보다 응답 bytes를 **99.66% 줄였지만 중앙값은 더 빠르지 않았다**. 두 대기 시간 모두 의도적으로 넣은 250ms 지연을 포함한다.

click 검사에서는 한 번의 trusted 클릭과 최종 카운터 1을, stale ref 검사에서는 거절과 잘못된 필드 쓰기 0회를 확인했다. 동시성 수치는 **각 surface ID에 고정한 읽기 전용 관찰 묶음의 완료 시간**이다. 서로 다른 탭에 동시에 native 포인터 입력을 보낸 결과가 아니다. 실행 순서는 고정됐으며, 과거 커밋과의 회귀 비교나 통계적 유의성 검정은 수행하지 않았다.

## 공개 사이트 고정 시나리오

[The Internet](https://the-internet.herokuapp.com)의 공개 테스트 페이지에서 시나리오별 3회씩 수행했다. 본 측정 시각은 16:51:33–16:52:13 KST다. 매회 페이지를 준비한 뒤 아래 조작과 결과 확인 시간을 측정했다. 표본이 작아 p95는 표시하지 않는다.

| 시나리오 | 성공 | p50 ms | 최소–최대 ms | 성공 판정 |
| --- | ---: | ---: | ---: | --- |
| 로그인 | 3/3 | 982.80 | 966.43–1,160.40 | `/secure` URL과 성공 메시지 |
| 동적 컨트롤 | 3/3 | 6,166.34 | 6,154.10–6,168.88 | 체크박스 제거, 입력 활성화, 입력값 일치 |
| 요소 추가·삭제 | 3/3 | 183.23 | 181.80–188.42 | 3회 추가·2회 삭제 후 버튼 1개 |
| 표의 최대 금액 추출 | 3/3 | 4.46 | 4.03–4.67 | Jason Doe와 $100.00인 행 |
| 로그인 세션의 reload | 3/3 | 721.42 | 656.57–827.87 | 새 문서 로드 후 인증 URL·메시지 유지 |

초기 페이지 탐색, reload 시나리오의 사전 로그인, 모델 추론은 측정에 포함하지 않았다. 표 추출은 **임의 DOM JavaScript 추출 허용 트랙**이며, 다른 시나리오도 JS를 최종 상태 검증에 사용한다. 동적 컨트롤의 약 6초에는 사이트가 의도한 지연이 포함된다. 테스트 사이트의 공개 계정만 사용했다.

이는 결정된 절차를 실행한 기능 workflow 결과다. 자연어 요청 해석이나 탐색 전략을 평가하지 않았으므로 Online-Mind2Web·Odysseys 등의 에이전트 점수로 발표할 수 없다. 근거는 [최종 요약](evaluations/browser-186afac29f8c/public-final-summary.json), [표본](evaluations/browser-186afac29f8c/public-final-samples.jsonl.gz), [명령](evaluations/browser-186afac29f8c/public-final-commands.jsonl.gz)이다.

## Wayland 실패와 확인된 범위

원본 [Wayland 검사](../scripts/test-browser-wayland.py)는 `private portal dialog did not appear`로 중단됐다. 중첩 GNOME의 Overview 초기 화면이 고정 픽셀 기반 권한 창 탐지와 맞지 않았다. 임시 사본에 Escape와 페이지 초점 초기화만 추가한 뒤 다음 세 마커가 통과했다.

- `WAYLAND_PORTAL_INPUT_OK`: 권한 거절·승인·취소 후 재요청, 입력 재생 방지, native 키·포인터 등.
- `WAYLAND_NATIVE_PICKER_RESPONSE_OK`: 실제 파일 선택·취소 응답과 키보드 초점 복귀.
- `WAYLAND_PICKER_EARLY_CLOSE_OK`: 조기 종료한 파일 선택 요청의 실제 portal 정리.

이후 원본 기능 검사를 계속하다 [슬라이더 assertion](../scripts/test-browser-control.py#L750)에서 실패했다. 슬라이더 사각형은 `x=32, width=240, right=272`, 요청한 드래그는 CSS 좌표 `x=40 → 260`이었다. 관측 이벤트는 `40 → 230`, 최종 값은 **85**, 기대 조건은 **90 초과**였다. 입력 이벤트는 trusted였다.

[실패 화면](evaluations/browser-186afac29f8c/normalized-wayland-live-failure.png)에서는 브라우저 왼쪽이 화면 x≈570이며 중첩 화면 오른쪽은 x=800이다. 요청 끝점 x≈830은 화면 밖이고, 관측 끝점은 경계 x≈800과 일치한다. **이 환경의 화면 경계에서 이동이 제한됐다는 설명과 증거가 일치한다.** 이를 모든 Wayland 환경의 드래그 결함으로 일반화하지 않는다.

테스트용 호스트 창에 1280×900 크기를 요청한 추가 실행에서도 실제 크기는 800×600으로 유지됐고 같은 값 85에서 실패했다. 임계값을 완화하거나 이 실행을 통과로 바꾸지 않았다. 유효한 화면 크기로 다시 검증하기 전에는 Wayland 전체 기능의 통과를 주장할 수 없다. 실패 이후의 기능 검사와 portal 연결 종료·미지원 경로 검사는 완료되지 않았다.

근거: [원본 실행 로그](evaluations/browser-186afac29f8c/functional-wayland.log), [초기 화면 전처리 후 로그](evaluations/browser-186afac29f8c/functional-wayland-normalized.log), [크기 변경 시도 로그](evaluations/browser-186afac29f8c/functional-wayland-expanded.log), [첫 전처리 patch](evaluations/browser-186afac29f8c/wayland-harness.patch), [크기 요청을 포함한 patch](evaluations/browser-186afac29f8c/wayland-expanded-harness.patch). 두 patch는 임시 사본에만 적용했다.

## 파일럿 실패와 집계 규칙

실패한 실행은 삭제하거나 최종 표본의 성공으로 치환하지 않았다. 아래 실행들은 조건 확인용 별도 집합으로 보존했고 최종 지연 표에는 섞지 않았다.

| 실행 | 관측 | 처리 |
| --- | --- | --- |
| 로컬 파일럿 | 42회 중 38회 통과, 준비 21회 중 19회 통과. 500개 버튼 snapshot에 `truncated=false`를 요구한 검사가 실패 | 전체 정보 반환과 대상 ref 확보를 구분해 통과 조건을 명시하고, `truncated`를 보존한 뒤 100회 본 측정 |
| 첫 공개 workflow | 15회 중 11회 통과. 반복 탐색에서 `Remove` 누락 2회, 이전 문서의 click 완료 핸들러 오류 2회 | 같은 URL일 때 `about:blank`로 이동해 문서 교체를 확인한 후 새 탐색 |
| 첫 reload 측정 | 약 13ms에 통과했으나 기존 문서의 ready state로 완료를 잘못 판단 | 전역 표식을 심고 새 문서에서 표식이 사라졌는지 확인한 뒤 전체 공개 시나리오 재실행 |

파일럿과 첫 공개 실행의 [요약](evaluations/browser-186afac29f8c/pilot-local-summary.json), [공개 실행 요약](evaluations/browser-186afac29f8c/public-summary.json), 각 `*-samples.jsonl.gz`·`*-commands.jsonl.gz`가 남아 있다. 같은 URL과 `ready-state=complete`만으로 새 탐색 완료를 판단하면 이전 문서를 관찰할 수 있다는 점은 실제 사용의 대기 조건에도 적용된다.

## 재현 방법

저장소 루트에서 해당 커밋을 사용하고 Xvfb, D-Bus, WebKitGTK, GTK portal, H.264 GStreamer 디코더, ffmpeg/ffprobe를 준비한다. Wayland에는 GNOME Shell·GNOME portal·xdotool·xwininfo도 필요하다. 이번 실측의 임시 패키지 목록과 환경 경로는 [reproduction.json](evaluations/browser-186afac29f8c/reproduction.json)에 있다. 다른 머신에서는 해당 파일의 `/tmp` 경로를 그대로 재사용할 수 없으므로 동일 의존성을 준비해야 한다.

```sh
rtk cargo build --release --locked -p flowmux -p flowmux-cli
rtk cargo test -p flowmux-browser --locked
rtk proxy python3 -B scripts/benchmark-browser-control.py --output /tmp/fm-unused-self-test --self-test

rtk proxy env XDG_CURRENT_DESKTOP=XFCE xvfb-run -a -s '-screen 0 1280x1024x24' dbus-run-session -- python3 -B scripts/test-browser-control.py target/release/flowmux

rtk proxy env XDG_CURRENT_DESKTOP=XFCE xvfb-run -a -s '-screen 0 1280x1024x24' dbus-run-session -- python3 -B scripts/benchmark-browser-control.py --binary target/release/flowmux --output /tmp/fm-local-186afac29f8c --samples 100 --warmup 5

rtk proxy env XDG_CURRENT_DESKTOP=XFCE xvfb-run -a -s '-screen 0 1280x1024x24' dbus-run-session -- python3 -B scripts/benchmark-browser-control.py --binary target/release/flowmux --output /tmp/fm-public-186afac29f8c --samples 3 --public

rtk proxy env LIBGL_ALWAYS_SOFTWARE=1 python3 -B scripts/test-browser-wayland.py target/release/flowmux
```

출력 디렉터리는 기존 결과를 덮어쓰지 않도록 매 실행 새 경로를 사용한다. X11에서 GNOME portal이 선택되면 사설 `xdg-desktop-portal/portals.conf`에 `default=gtk`, `org.freedesktop.impl.portal.FileChooser=gtk`를 지정하고 그 상위 설정 디렉터리를 `XDG_CONFIG_DIRS`로 전달한다. 임시 codec을 쓸 때는 manifest의 `LD_LIBRARY_PATH`, `GST_PLUGIN_PATH`, `PATH`도 함께 전달한다. 위 마지막 명령은 원본 Wayland 검사이므로 이 환경의 실패를 그대로 재현할 수 있다.

새 실행기는 표본·명령·실패·진단·GUI 로그를 기록한다. 연속 3회 실패 시 해당 시나리오의 나머지를 중단하고 `not_run`에 남긴다. 집계 self-check는 중앙값·p95·실패 집계를 확인한다. 결과 파일의 해시는 [SHA256SUMS](evaluations/browser-186afac29f8c/SHA256SUMS)에 있다.

## 적용할 판단

현재 커밋의 X11 제어 기능은 이 검사 범위에서 정상 동작했다. 이름을 아는 대상에는 `find`, 여러 필드에는 `form`, 지연 조건에는 typed wait를 쓰면 관찰 출력이나 호출 수를 줄일 수 있다. 큰 snapshot의 약 0.49초 비용과 출력 잘림은 남아 있으므로, `compact`만으로 관찰 지연이 해결된다고 보면 안 된다.

다음 플랫폼 검증은 Wayland 테스트 데스크톱의 실제 출력 크기를 먼저 확정한 뒤 전체 검사를 끝내는 것이다. 자연어 업무 성공률·rubric·토큰·비용 평가는 별도 모델 실행이 필요한 후속 범위다. 독립 에이전트 실행에는 명시적 허용이 필요하다는 세션 규칙에 따라 실행 허용을 요청했으며, 답변 전에는 시작하지 않았다. 현재 자료로 외부 에이전트 브라우저 대비 업무 성공률이나 비용 우위를 판단할 수 없다.

## P1 P2 후속 조치

2026-10-26 후속 검증에서 **P1·P2를 모두 완료했다**. 최초 평가와 동일한 release GUI·CLI 바이너리에 수정한 검사 스크립트를 실행했다. 브라우저 런타임과 API는 그대로이며, 변경 범위는 검사 두 파일·사용 안내·에이전트 스킬이다. 변경 내용과 바이너리·파일 해시는 [후속 메타데이터](evaluations/browser-186afac29f8c-followup/results.json), [patch](evaluations/browser-186afac29f8c-followup/changes.patch)에 기록했다.

| 항목 | 적용한 조치 | 검증 결과 |
| --- | --- | --- |
| P1 탐색 완료 | 서버 응답을 보류하는 fixture로 같은 URL 탐색과 reload를 검사. 새 문서 식별 및 readiness를 함께 기다리는 절차를 안내 | X11·Wayland 모두 재현 및 안전한 후속 입력 검증 통과 |
| P2 Wayland 환경 | Overview 종료, 사용 가능한 출력 모드 고정, 권한 검사 후 Mutter의 실제 출력을 1280×900으로 변경하고 크기 확인 | 기존 슬라이더 assertion과 전체 기능 검사 통과 |
| 스킬 배포 호환성 | `cargo test -p flowmux-cli --test agent_skills --locked` | 9/9 통과 |

P1에서 서버가 응답을 보내기 전에도 `ready-state=complete`는 두 플랫폼에서 모두 참이었다. 같은 시점의 새 문서 확인 조건은 거짓을 유지했고, `TextChanged` receipt는 `outcome_unknown`을 반환했다. 응답을 허용한 뒤에는 새 문서·정확한 URL·응답 세대·입력값을 확인했다. 서버 요청 횟수도 검사해 timeout이 탐색을 다시 실행하지 않았음을 검증했다. [안전한 대기 절차](browser-workflow.md#wait-for-a-new-document)를 문서와 내장 에이전트 스킬에 반영했다. 이 절차의 문서 표식은 전체 문서 탐색용이며 SPA·fragment 변경에는 해당 앱의 결과 조건을 사용한다.

P2에서는 X11 호스트 창 크기를 바꾸는 대신 `org.gnome.Mutter.DisplayConfig.ApplyMonitorsConfig`로 compositor 출력 모드를 변경했다. `WAYLAND_OUTPUT_GEOMETRY_OK 1280x900` 이후 슬라이더 값을 90 초과로 요구하는 기존 검사가 그대로 통과했다. 기존 실패는 이 실행 환경의 화면 경계 문제로 해소됐으며, 제품 포인터 코드 변경은 필요하지 않았다.

[X11 전체 로그](evaluations/browser-186afac29f8c-followup/x11.log)는 `BROWSER_NAVIGATION_COMPLETION_OK`와 `BROWSER_CONTROL_OK`, 종료 코드 0을 기록했다. [Wayland 전체 로그](evaluations/browser-186afac29f8c-followup/wayland.log)는 이에 더해 `WAYLAND_PICKER_CLEANUP_OK`, `WAYLAND_PORTAL_DISCONNECT_OK`, `WAYLAND_UNSUPPORTED_PORTAL_OK`까지 통과했고 종료 코드 0이었다. 이 로그들은 최초 평가 자료와 별도 보관했다.

재실행은 위 재현 방법의 X11·Wayland 전체 검사 명령을 사용한다. Wayland 출력 설정과 초기 화면 보정은 이제 저장소의 검사 스크립트에 포함돼 임시 patch가 필요 없다. macOS·다른 Wayland compositor·자연어 에이전트 업무 평가는 이번 후속 범위에 포함하지 않았으며 성능 반복 측정도 다시 실행하지 않았다.
