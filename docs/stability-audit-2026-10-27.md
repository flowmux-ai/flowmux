<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Flowmux 장기 유휴·렌더링 안정성 점검 — 2026-10-27

장기간 방치 후 종료된다는 리포트에 대해 AgentOffice, 주기 작업, 터미널,
브라우저·에디터 렌더러와 객체 수명주기를 점검했다. 선택적 Cairo 리샘플링의
panic과 느린 CWD 조회의 작업 누적을 각각 재현하고 수정했다. 원래 리포트의
자연 종료 자체는 재현하지 못했으며, 두 발견과 그 리포트의 인과관계는 미확정이다.

기존 [메모리 점검](memory-stability-audit-2026-10-22.md)과
[운영 경로 점검](operational-memory-audit-2026-10-22.md)의 입력 크기 제한,
큐 상한, 객체 해제 검사를 재사용했다. 이미 수정된 문제를 새 발견으로
계산하거나, 근거 없는 전체 메모리 상한을 추가하지 않았다.

## 재현한 문제와 패치

### AgentOffice 리샘플링 실패가 draw callback의 panic으로 전파됨

[`sprite.rs`](../crates/flowmux/src/ui/agent_office/sprite.rs)의 선택적 확대·축소
캐시 생성에서 Cairo의 실패를 `expect`로 처리했다. 원본을 그대로 그릴 수
있는 상황에서도 중간 이미지 생성 실패가 GUI 콜백까지 전파됐다.

17,000×2 원본, device scale 2, 표시 배율 2.1을 사용하면 최종 출력 크기는
유효하지만 중간 정수 확대 이미지가 Cairo의 크기 한도를 넘는다. 수정 전
`enlarged sprite surface: InvalidSize` panic을 재현했다. 일반 AgentOffice
화면에서 이 크기가 만들어진다는 증거는 없으며, 실패 처리 경로의 회귀 입력이다.

`7acc61dd`는 크기 곱셈을 검사하고 Cairo 오류를 반환한다. 리샘플링이 실패하면
원본을 표시하며 실패한 복사본을 캐시에 넣지 않는다. 원본 픽셀 보존,
비정상 배율·정수 오버플로 입력, 실제로 매핑한 GTK 창의 반복 draw를 검증했다.
작은 고정 크기 원본 생성까지 포함한 전역 OOM 복구를 구현한 것은 아니다.

### 지연된 주기 작업이 다음 tick마다 쌓이고 창 종료 후에도 참조를 유지함

[`polling.rs`](../crates/flowmux/src/ui/window/polling.rs)의 CWD 조회와 에디터
세션 저장은 이전 실행의 완료 여부와 무관하게 새 비동기 작업을 만들었다.
파일시스템이나 프로세스 조회가 느려지면 대기 작업과 캡처한 컨트롤러가
누적될 수 있었다. 에이전트 조회와 scrollback 저장도 창 수명에 맞춰 종료할
필요가 있었다.

테스트 GUI PID에만 적용하는 `readlink` interposer로 `/proc/PID/cwd` 조회를
약 8초 지연시켰다. 수정 전에는 막힌 작업이 9개 생성됐고, 수정 후에는 1개였다.
지연 중 IPC 응답, 지연 해제 후 터미널 출력, 창의 정상 종료를 확인했다.

`95014d73`는 네 주기 작업을 각각 실행 완료 후 다음 간격을 기다리는 루프로
바꾼다. 창의 `unrealize`에서 태스크를 취소하고 핸들을 버린다. 숨김은 polling을
중단하지 않는다. 테스트는 지연 시 중복 실행 방지, 숨김 상태에서 재개,
창 파괴 시 캡처 해제를 확인한다. 이미 실행 중인 OS blocking 호출 자체를
강제 취소하지는 않으며, 해당 호출은 반환할 때까지 남을 수 있다.
정상 창 닫기에서는 기존 최종 CWD·scrollback·에디터 세션 저장을 완료한 뒤
창을 파괴하므로, 이 취소 처리는 최종 저장 이후에 실행된다.

## 지속 가능한 회귀 검사

[`test-idle-stability-gui.py`](../scripts/test-idle-stability-gui.py)는 기존
격리 GUI harness를 사용한다. 사용자 셸 초기화 파일을 읽지 않고, private
XDG 디렉터리·D-Bus·Xvfb와 테스트 소켓을 사용한다. 테스트가 생성한
프로세스만 종료한다. 실행 중인 사용자 Flowmux와 설치 바이너리는 변경하지 않았다.

- 3회 준비 실행 뒤 터미널 10,000줄 출력·탭 종료, 로컬 canvas 브라우저
  열기·종료, AgentOffice 진입·실제 창 크기 변경·복귀를 반복한다.
- WebKit과 bwrap 자식이 실제 종료했는지 확인한다. GUI와 각 자식의 RSS,
  PSS, private memory, FD·thread 수를 JSONL로 기록한다.
- 작업 중 1명과 휴식 중 2명을 유지해 AgentOffice를 표시하고, 이후 실제 창을
  unmap한 상태에서도 IPC 응답과 자원 사용량을 측정한다. 복귀 후 화면도 캡처한다.
- 테스트 소유권을 확인한 WebKit renderer만 pidfd로 강제 종료한다. GUI가
  살아 있고 Reload 후 페이지 본문이 돌아오는지 확인한 뒤 정상 종료한다.
- 준비 실행을 제외한 앞·뒤 1/3 구간의 중앙값을 비교한다. 기본 회귀 예산은
  GUI RSS 및 자식 합계 PSS 각각 +64MiB, FD·thread 각각 +4, 자식 수 +1이다.
  이 예산은 전체 프로세스의 메모리 상한이나 누수 부재의 증명이 아니다.

[`test-polling-delay-gui.py`](../scripts/test-polling-delay-gui.py)와
[`test-polling-delay.c`](../scripts/test-polling-delay.c)는 CWD 지연 재현을
보존한다. 새 두 GUI 검사를 Linux coverage gate에 포함하고, GitHub Actions가
메모리 샘플·요약·로그·전후 화면을 보관하도록 했다. 자원 증가 판정기에는
합성 RSS/FD/thread/자식 증가를 놓치지 않는 self-check가 있다.

Linux 재실행 예시(프로젝트 native 의존성 및 Python Xlib/Pillow/Atspi 필요):

```sh
rtk cargo build --profile fast --workspace --locked
rtk proxy python3 scripts/test-polling-delay-gui.py \
  --gui target/fast/flowmux --cli target/fast/flowmuxctl
rtk proxy python3 scripts/test-idle-stability-gui.py --self-check
rtk proxy python3 scripts/test-idle-stability-gui.py \
  --gui target/fast/flowmux --cli target/fast/flowmuxctl \
  --cycles 30 --idle-seconds 900
```

`--idle-seconds`는 표시·숨김 **각 단계**의 시간이다. `--renderer gl`은
Xvfb에서는 소프트웨어 GL일 수 있다. 실제 GPU 검증은 별도 테스트 창을
데스크톱 DISPLAY에서 실행하고 DRM FD를 확인했다. WebKit의 AppArmor
프로파일이 필요한 환경에서는 CI처럼 `aa-exec -p flowmux-webkit --`로 감싼다.

## 검증 결과

환경은 Linux x86_64, GTK 4.14.5, VTE 0.76.0, WebKitGTK 2.52.6,
libadwaita 1.5.0이다. 실행 파일은 `target/fast` 최적화 빌드이며,
sanitizer는 저장소의 nightly-2026-09-21 스크립트를 사용했다.

Cairo 측정은 준비 3회 + 측정 30회, 표시 900초 + 숨김 900초를 포함해
약 1,884초간 진행했다. 실제 GL 측정은 준비 3회 + 측정 15회, 표시 300초 +
숨김 300초를 포함해 약 650초간 진행했고 테스트 GUI의
`/dev/dri/renderD128` 접근을 확인했다. 두 실행 모두 창 복귀 화면,
renderer 종료·Reload 및 정상 종료까지 통과했다. 다음 값은 각 측정
구간의 앞·뒤 1/3 중앙값이다.

| 렌더러·구간 | GUI RSS 앞 → 뒤 (MiB) | FD 앞 → 뒤 | 자식 합계 PSS 변화 (MiB) |
| --- | ---: | ---: | ---: |
| Cairo, 30회 반복 | 236.89 → 233.73 | 42 → 42 | +3.05 |
| Cairo, 표시 15분 | 221.06 → 221.94 | 51 → 51 | -19.76 |
| Cairo, 숨김 15분 | 210.82 → 210.60 | 52 → 52 | +0.08 |
| 실제 GL, 15회 반복 | 282.74 → 282.84 | 59 → 58 | 0.00 |
| 실제 GL, 표시 5분 | 273.51 → 273.76 | 73 → 73 | -5.31 |
| 실제 GL, 숨김 5분 | 265.90 → 265.88 | 73 → 73 | +0.21 |

측정 구간의 최대 IPC ping 지연은 Cairo 1.07ms, GL 0.94ms 미만이었다.
두 실행의 각 구간에서 thread와 자식 프로세스 수의 중앙값 증가는 0이었다.
브라우저가 열린 유휴 구간은 브라우저를 닫고 측정한 반복 구간보다 FD가
많다. PSS는 동시에 실행한 다른 프로세스와의 공유 페이지 배분에도 영향을 받으므로 감소량을
제품 개선량으로 해석하지 않는다.

| 검사 | 결과 |
| --- | --- |
| 전체 일반 workspace 테스트 | 통과; GUI binary 860 통과, 기존 ignored 4개 포함 |
| 전체 workspace Clippy, `-D warnings` 및 rustfmt | 통과 |
| 헤드리스 ASan+LSan | 통과 |
| GUI 전체 ASan, leak detection 제외 | 통과; GUI binary 860개 및 Markdown viewer 검사 포함 |
| 별도 ignored stress | 16MiB OSC/541,201 payload, title 갱신 20,000회, 깊이 1,001의 pane tree 통과 |
| AgentOffice live 검사 | 8개 방, 한 방 32명, 탐색·resize·lifecycle 통과 |
| 단기 Cairo 및 실제 GL GUI 검사 | 반복 열기·닫기, 표시·숨김, renderer 종료·Reload, 정상 종료 통과 |
| Cairo 30분 / 실제 GL 10분 유휴 검사 | 위 측정 및 최종 renderer 종료·Reload 통과 |
| GUI ownership LSan | 21개 해제 assertion 통과; 아래 Fontconfig 종료 보고로 프로세스 실패 |
| 원격 Sanitizers CI | Rust ASan/LSan, Lottie, GUI ASan 및 ThorVG ASan/UBSan/LSan 통과 |
| 원격 macOS build/native smoke | 통과 |
| 원격 Linux 전체 테스트·coverage 및 에디터 웹 | 통과 |

최종 실행 코드 `60ca13dfd0c2a1ef1fcbebdc2acfcd9f91f39e2e`의
[Test CI](https://github.com/flowmux-ai/flowmux/actions/runs/40496917608)와
[Sanitizers CI](https://github.com/flowmux-ai/flowmux/actions/runs/40496917604)는
모두 성공했다. 이후 이 보고서만 별도 커밋하며 실행 코드에는 변경이 없다.

GUI ownership LSan은 **실패**로 남긴다. 23,235개 할당에 834,473바이트가
보고됐고 모든 첫 할당자 다음 프레임이 `libfontconfig.so.1`이었다. Flowmux를
링크하지 않은 GTK C 대조군도 8,912개 할당에 333,986바이트를 보고했다.
대조군은 외부 라이브러리에서 같은 유형의 종료 할당이 발생함을 보여주지만,
두 값을 빼서 Flowmux 누수량을 계산할 수는 없다. suppression이나 강제
Fontconfig 전역 정리를 추가하지 않았다. GUI ASan은 기존 스크립트대로
leak detection을 끄므로 이 LSan 실패를 해결했다는 뜻이 아니다.

로컬 Codex app-server의 조상 프로세스 감지가 CLI hook mock을 우회해 첫
전체 실행이 대기했다. 조상 관계가 분리된 systemd user service에서 **전체**
workspace를 다시 실행해 정상 종료를 확인했다. 제품 코드나 hook 테스트를
우회하도록 바꾸지 않았다. 처음의 AppArmor 미설정 ASan 실행과 GTK의 숨은
leader window를 잡았던 초기 resize/숨김 측정도 성공 증거에서 제외했다.
최종 harness는 PID와 mapped 상태·크기로 실제 창을 선택하고 geometry 및
map state 변경을 확인한다.

첫 원격 Test CI는 새 유휴 검사의 `AgentOffice` 접근성 버튼 조회에서
실패했다. `60ca13df`는 기존 GUI 검사와 동일하게 테스트 클라이언트의
DISPLAY를 격리 화면으로 맞추고, 상위 AT-SPI 주소를 GUI·클라이언트에서
제거한다. 외부 접근성 버스를 가진 중첩 Xvfb/D-Bus 실행에서 전체 단기
검사를 다시 통과했다. 또한 CI runner가 종료 시 지우는 TMPDIR 밖에
두 새 검사의 자료를 남겨 Actions 업로드에 포함되도록 했다.
첫 실패를 제품 크래시나 검사 성공으로 계산하지 않는다.
최종 CI 로그에서 `STALLED_CWD_CALLS=1`, 표시·숨김 각 30초,
renderer 복구 및 `IDLE_STABILITY_OK`를 확인했다. 업로드한
`linux-test-evidence`를 내려받아 두 검사 로그, 메모리 JSONL·요약과
전후 화면이 실제 포함된 것도 확인했다.

원시 로그는 `target/stability-20261027/`에 있다: `sprite-before.log`,
`polling-before.log`, `polling-after.log`, `workspace-detached.log`,
`asan-headless-detached.log`, `asan-gui-profile.log`, `lsan-gui.log`,
`gtk-control.log`, `stress.log`, `office-live.log`, `idle-ci-smoke.log`,
`idle-fixed-final.log`, `idle-desktop-final.log`, `nested-after.log`,
`ci-test-final.log`. Git에서 제외되는 로컬 로그와
`/tmp/fm-gui-*` 파일은 청소 시 없어질 수 있으므로 위 측정 요약을 함께 보존한다.
Cairo 결과의 `memory.jsonl`, `summary.json`과 전후 PNG는
`/tmp/fm-gui-wxc1rybl/`에 있다.
실제 GL 결과의 `memory.jsonl`, `summary.json`, 전후 PNG 및 `gpu-fds.json`은
`/tmp/fm-gui-bfejuxyu/`에 있다.

## 범위와 남은 한계

검토·회귀 범위에는 AgentOffice의 actor/room/cache 수명, 주기 polling,
PTY backpressure·scrollback, IPC 동시성, WebKit view/session 해제,
에디터 renderer 종료 시 미저장 데이터 복구, 이미지·Lottie·문서의 기존
입력 제한과 notification/download 수명 관리가 포함된다. 전체 테스트
통과를 모든 경로의 장애 주입 검증으로 표현하지 않는다.

수일·수주 방치, 실제 사용자 작업과 같은 콘텐츠, suspend/resume,
GPU VRAM 사용량과 GPU reset, macOS 실기 장기 실행은 이번 로컬 측정에
포함되지 않는다. 배포된 GTK/VTE/WebKit 전체를 sanitizer로 다시 빌드하지
않았으며, OS OOM killer나 native SIGSEGV는 Rust panic 보고에 남지 않을 수
있다. 원래 종료 원인을 확정하려면 해당 버전·OS·직전 작업과 시스템 종료
기록이 추가로 필요하다.
