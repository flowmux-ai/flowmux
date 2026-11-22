<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Flowmux 상주 채팅 봇 설계와 구현 계획

설계 개정안 · 구현 전 검토 · 2026-11-22

제품 의도와 기존 검토 이력은 [원 설계](chat-agent-bots-design.md)에 기록되어 있다.
검토 기준은 [PR #13](https://github.com/flowmux-ai/flowmux/pull/13)의 `b2e7be78`와
`main`의 `aaa633ee`다. 코드 경로는 이 기준에서 확인했다. 아래 계약과 수용 기준은
구현 제안이다.

## 1. 제품 범위와 첫 출시 조건

봇마다 이름, 역할(`SOUL.md`), 기억(`MEMORY.md`), 전용 workspace를 둔다. 사용자는
고정된 봇 pane의 Flowmux 채팅 client에서 봇과 대화한다. 새 사용자 입력, 예약,
로컬 이벤트, 다른 봇의 메일, 작업 보고는 서비스가 접수한다. 봇의 workspace에는
채팅 pane과 별도 작업 영역이 있고, 작업 영역에서 터미널·브라우저·worker를
사용하는 것이 최종 목표다.

| 단계 | 사용자가 할 수 있는 일 |
| --- | --- |
| 첫 출시 | 봇 생성·일시정지·삭제, 역할·기억 편집, Chat 열기·복원·취소, 반복 일정·1회 예약, 기록과 실패 상태 확인. 검증된 provider의 응답 전용 실행만 제공한다. |
| 확장 | Items와 Needs you, 읽기 전용 Look around, 기억 정리 제안, 봇 간 메일과 오퍼레이터, 승인 후 새 worker 위임, 작업 영역의 제한된 브라우저 조작, 로컬 이벤트와 제한된 자동 실행. 기능별 검증을 통과한 순서로 켠다. |
| 별도 설계 | 외부 커넥터·웹훅·다른 사용자의 요청, 공유·템플릿, GUI가 모두 닫힌 상태의 실행, Claude Code·Codex 이외 provider. |

초기 provider는 Claude Code와 Codex를 목표로 한다. 판정은 provider 버전,
플랫폼, 패키징, 인증 방식, 실행 모드별로 내린다. 한 조합만 검증되면 그 조합만
제공한다. 인증이나 지원이 확인되지 않으면 설정 화면과 실행 이력에 이유를 표시한다.
provider 자동 전환, 사용자 hook 설정이나 trust의 수정, 기존 TUI에 타이핑해 봇을
깨우는 동작은 제품 범위에서 제외한다.

## 2. 검토 결과

| 우선순위 | 초안에서 발견한 문제 | 설계에 반영한 결정 |
| --- | --- | --- |
| P0 | `cwd`, CLI 토큰, workspace ID만으로는 파일·프로세스·브라우저 권한을 강제할 수 없다. 기존 GUI 소켓은 같은 OS 사용자의 일반 CLI를 받는다. 저장소 스크립트가 턴과 같은 권한으로 실행되면 토큰 또는 일반 소켓을 사용할 수 있다. | 응답 전용 실행과 도구 실행을 분리한다. 사용자 workspace 접근은 명시적 공유 범위로 제한한다. 터미널 실행에는 플랫폼에서 검증한 격리를 요구한다. |
| P0 | Codex 승인 RPC는 모든 실행의 사전 심사 지점이 아니다. Claude 도구 목록, 사용자 설정, hook, MCP와 인증 방식은 별개의 동작이다. | provider 승인은 별도 계층으로 둔다. 읽기 전용 턴은 Flowmux가 수집한 제한된 자료만 받는다. 승인 RPC와 문자열 명령 필터를 읽기 전용 보장의 근거로 삼지 않는다. |
| P0 | 서비스 소유 창과 대상 workspace 소유 창이 다를 수 있다. 여러 GUI의 직접 쓰기, 고정 PID 소켓, 카드 처리·화면 이동이 섞이면 권한과 실행 상태가 갈라진다. | 서비스가 봇 DB를 단독으로 쓴다. 대상 창은 별도 등록과 소유권 세대로 찾아간다. 승인 예약과 실제 적용 양쪽에서 세대를 검사한다. |
| P0 | 실행 시작, 외부 효과 반영, 완료 기록 사이에 장애가 나면 결과를 알 수 없다. 부모 종료나 PID 일치만으로 전체 자식 정리까지 보장할 수 없다. | 시작 의도, 실행 핸들, 확정 결과를 구분한다. 재실행은 미전달이 입증되거나 수신 측이 중복 제거하는 경우에만 허용한다. 이전 실행의 종료가 확인되기 전에는 같은 native session을 막는다. |
| P1 | 턴 토큰은 작업자 수명보다 짧다. `NeedsInput`에는 사용자 입력과 권한 대기가 섞인다. 활성 tab 대상 입력과 지연된 lifecycle Stop은 정확한 전달·완료 계약이 아니다. | 새 관리 worker의 시작 프롬프트를 먼저 사용한다. 작업별 보고 권한과 안정적인 발송 ID를 둔다. 기존 세션 연결은 provider가 대기·접수 신호를 제공할 때 별도 검증한다. |
| P1 | 기억 자동 병합이 편집 주체를 추정하며, 두 대화의 동시 쓰기와 여러 파일 정리가 충돌할 수 있다. | SQLite revision을 원본으로 하고, 버전이 붙은 편집과 확인 가능한 정리 제안을 지원한다. 파일은 내보내기와 명시적 가져오기에 사용한다. |
| P1 | 예약·메일·이벤트 순서와 일시정지 정의가 충돌한다. `PortListening` 발행 경로가 없고 `GitInfo`에는 HEAD OID가 없다. | 대화 접수 순서와 예약 occurrence를 분리한다. 일시정지와 재개 정책을 명시한다. 실제 발행 지점과 중복 제거를 검증한 로컬 이벤트부터 추가한다. |

코드 근거는 [IPC 소켓과 peer PID](../crates/flowmux-ipc/src/server.rs#L100),
[GUI 처리 경계](../crates/flowmux/src/ipc_handler.rs#L316),
[활성 터미널 입력](../crates/flowmux/src/ui/window/pane_commands.rs#L1056),
[workspace 소유권](../crates/flowmux-state/src/lib.rs#L536),
[workspace 및 포트 메타데이터](../crates/flowmux-core/src/lib.rs#L224),
[GitInfo](../crates/flowmux-core/src/lib.rs#L351),
[NeedsInput](../crates/flowmux-core/src/lib.rs#L2337),
[Codex 사용량 프로브](../crates/flowmux/src/usage/codex.rs#L49)에서 확인할 수 있다.

## 3. 런타임과 코드 소유권

~~~mermaid
flowchart LR
    UI["각 창의 Bots UI / 승인 UI"] -->|"사용자 제어 채널"| Service
    Chat["Flowmux Chat client"] -->|"대화 한정 접수·구독"| Service
    subgraph Owner["봇 서비스 소유 GUI의 Tokio 런타임"]
      Service["flowmux-bots 서비스"]
      Service --> DB[("bots.sqlite")]
      Service --> Runner["턴 임대·provider adapter"]
      Service --> Broker["허용된 작업 요청·승인 검사"]
    end
    Runner -->|"구조화된 stdio"| Provider["provider 프로세스"]
    Provider -->|"텍스트·구조화 제안"| Runner
    Service -->|"창 등록·소유권 조회"| Window["대상 workspace 소유 GUI"]
    Broker -->|"검증된 요청 / 대상 세대"| Window
    Window -->|"GTK main thread / 명시적 surface"| View["workspace·pane·browser"]
    Window -->|"managed launch"| Host["Agent Host worker"]
    Host -->|"작업에 한정된 보고"| Service
~~~

| 구성 | 구현 위치와 책임 |
| --- | --- |
| 봇 코어 | 새 GTK 없는 `flowmux-bots` crate. `store`가 SQLite 트랜잭션을, `service`가 큐·스케줄·복구를, `provider`가 자식 프로세스와 이벤트 정규화를 맡는다. worker 및 도구 연결은 해당 단계에 추가한다. |
| GUI 연결 | `crates/flowmux/src/`의 창 시작·종료와 Bots UI에서 서비스 소유권, 사용자 제어, workspace 연결을 관리한다. 기존 `GuiHandler → Bridge → WindowController` 경로에서 실제 GTK 작업을 수행한다. SQLite·provider I/O는 main thread 밖에서 처리한다. |
| 공유 타입·CLI | `flowmux-ipc`는 봇에 필요한 요청·이벤트만 추가하고, `flowmux-cli`는 Chat, 상태 조회, 검증된 작업 보고 client를 구현한다. `flowmux-core`와 `flowmux-state`는 workspace binding 및 상태 버전을 관리한다. |
| 기존 기반 | `flowmux-config::paths`의 XDG/Flatpak 경로, `flowmux-procmon`의 프로세스 시작 식별자, `flowmux-notify`를 재사용한다. 관리 worker에는 기존 `LaunchAgent`와 Agent Host를 사용한다. TUI가 없는 봇 턴은 Host에 얹지 않는다. |

| 서비스 경계 | 초기 계약 |
| --- | --- |
| Chat 접수 | 대화 ID·요청 ID·입력 → DB commit 뒤 turn ID와 접수 sequence. 취소는 turn ID를 받는다. |
| 상태 구독 | 조회 범위·마지막 event ID → 누락 이벤트, 또는 새 snapshot과 기준 event ID. |
| 사용자 설정 | 명령·요청 ID·예상 설정 revision → 적용한 revision 또는 충돌. |
| 사용자 승인과 창 적용 | 카드 ID·결정은 사용자 채널에서만 받는다. 대상 창에는 일회 실행 ID·권한 revision·대상 세대·정규화된 인자를 보낸다. |
| worker 보고 | job ID·report ID·보고 상태와 내용 → 이미 받은 보고 또는 새로 확정한 보고의 event ID. |

### 3.1 서비스 소유권과 창 라우팅

- 같은 state/runtime 범위에서 한 GUI가 전용 `bots.lock`을 잡고 DB 쓰기, 만기
  처리, provider 실행을 담당한다. 현재 `InstanceLock`의 public 진입점은
  `state.lock` 전용이다. 별도 경로에 대한 non-blocking 획득을 추가하고 기존
  `state.lock`의 짧은 보유 계약을 유지한다. 두 lock을 함께 잡은 상태에서 I/O를
  기다리지 않는다. [현재 lock 구현](../crates/flowmux-state/src/instance_lock.rs#L63)
- 소유 창은 runtime의 짧은 고정 서비스 주소에서 연결을 받는다. lock 획득 →
  스키마 확인·마이그레이션 → 이전 epoch 복구 → 서비스 공개 순서다. 새 소유자가
  생성한 epoch를 모든 제어 응답에 포함한다. 소유권 교체 중 UI는 연결 복구 상태와
  전송되지 않은 입력을 보여 준다. 접수 여부를 잃은 client는 같은 요청 ID로 조회
  또는 재접수한다. 생존 창은 최대 30초 간격으로 인수 가능성을 확인한다.
- 소유권은 정규화된 DB 경로에 귀속된다. 같은 DB를 사용하면서 서로 다른
  runtime 범위에 있는 창에는 연결 불가를 표시한다. 지원되는 같은 범위의 창은
  기존 per-PID 소켓과 창 instance를 대조한 등록 채널로 연결한다. 채널 종료,
  창 재시작 또는 소유권 변경 시 기존 등록을 폐기한다.
- 모든 창의 생성·설정·예약·카드 조작은 서비스 명령이다. 창은 구독 시작 시
  snapshot과 event sequence를 받고, 재접속할 때 누락분을 이어 받는다. 보존
  구간이 지났으면 snapshot을 다시 읽는다. 서비스가 없는 동안 GUI는 수정을
  접수한 것으로 표시하지 않는다.
- workspace는 기존 창 소유권을 따로 유지한다. 소유 창이 소유권 변경 때 등록을
  갱신한다. 라우팅에는 창 instance/세대와 실제 workspace ID를 사용한다. 대상이
  닫혔거나 다른 창으로 이동했으면 대기 또는 재확인으로 전환한다. 다른
  workspace나 활성 pane으로 대체하지 않는다.
- 정상 종료는 접수·dispatch를 멈추고 진행 중인 봇 턴을 취소·정리한 후 lock을
  넘긴다. 마지막 창이 닫히면 예약은 저장된 예정 시각으로 남는다. 이미 접수된
  Agent Host 작업은 봇 턴과 별도 수명이며 §7에 따라 복구한다.

### 3.2 저장 단위와 workspace

| DB 그룹 | 원본과 고유 키 |
| --- | --- |
| 봇·대화 | `bots`에 provider·설정 revision·lifecycle, `conversations`에 내부 ID와 provider/profile/native session ID. native session은 활성 대화 하나에만 연결한다. |
| 메시지·실행 | `turns`에 대화별 sequence, source, dedup key와 상태. `runs`에 attempt ID, owner epoch, provider 핸들, 시작 identity, 실패·취소 이유. 재연결용 이력은 단조 증가 event ID로 읽는다. |
| 예약·업무 | `schedules`와 `occurrences`에 일정 revision/예정 UTC 시각과 접수 결과. 후속으로 카드·허용 규칙·job·Items·메일을 해당 단계에서 추가한다. |
| 기억 | 봇별 `memory_revision`와 사용자·봇·정리 제안의 출처가 있는 이력. 기억 본문은 provider 프로세스에 필요한 범위만 제공한다. |

모든 DB 변경은 소유자의 짧은 트랜잭션이다. 예를 들어 예약 occurrence 기록과
턴 접수, job 보고 접수와 후속 턴 생성은 같은 트랜잭션으로 묶는다. WAL은 읽기
동시성에 사용한다. 스키마 갱신은 owner만 담당하며 지원하지 않는 버전이면 DB를
열어 쓰지 않는다. [SQLite WAL](https://sqlite.org/wal.html),
[SQLite 트랜잭션](https://sqlite.org/lang_transaction.html)

사용자에게 접수 성공을 응답하기 전에 고유 요청 ID와 결과를 저장한다.
같은 ID에 다른 인자가 오면 충돌로 반환한다. 재연결용 이벤트는 제한된
기간만 보존하고, Chat 이력은 별도 보존한다. 실행 진단에는 토큰·환경 전체·
원문 도구 출력을 남기지 않는다. 봇을 지울 때 Flowmux 이력·홈과 provider
자체 세션 기록의 삭제 범위를 확인 화면에서 각각 표시한다.

봇 홈은 `data_dir()/bots/<bot_id>/` 아래 둔다. 표시 이름은 경로에 쓰지 않는다.
`SOUL.md`와 `MEMORY.md`는 서비스가 만든 사람이 읽을 수 있는 사본이다.
`memory/`와 `skills/`는 각 파일을 권한·출처 모델에 포함한 뒤 도입한다.
provider가 설정 파일이나 skill을 자동 로드하는 디렉터리와 사용자 편집용 봇 홈을
분리한다. 봇 DB·권한 정보는 실행 환경에 전달하지 않는다.

workspace binding은 `bot_id`와 고정 chat pane ID를 기존 상태에 저장한다. 새
binding을 저장할 때 상태 스키마를 올리고 구버전 writer의 덮어쓰기까지 검증한다.
고정 pane 보호는 UI 클릭, 일반 workspace 명령, 상태 복원 경로에 모두 적용한다.
Chat 프로세스는 pane 복원 시 다시 연결한다. 탭을 닫아도 서비스가 이미 접수한
턴은 계속된다. 고정 pane을 접거나 크기를 조절할 수 있다.

DB와 홈 및 창 상태 파일의 변경은 한 번에 commit할 수 없다. 생성은 `creating`,
복구 가능한 파일 준비와 workspace 연결, `active` 순으로 진행한다. 삭제는 확인
카드 뒤 `deleting`에서 새 실행과 권한을 차단하고 진행 중 작업을 정리한다.
서비스가 관리하는 파일·binding만 정리하며, 사용 중이거나 변경 사항이 남은
worktree는 별도 사용자 판단을 기다린다. 재시작 시 중간 단계에서 이어간다.
경로 검사는 디렉터리 핸들 기준으로 수행하고 symlink를 따라 쓰거나 삭제하지
않는다.

## 4. 실행 권한과 사용자 승인

### 4.1 실행 주체

| 주체 | 접근 범위 |
| --- | --- |
| 사용자 GTK UI | 실제 사용자가 만든 새 요청과 승인. 승인 제어 채널은 창 프로세스 안에서만 보유한다. 일반 소켓이나 Chat의 텍스트 입력으로 승인을 흉내 낼 수 없어야 한다. |
| Chat client | 지정한 대화에 입력 제출, 취소 요청, 이력 구독. 터미널 출력은 제어문자를 제거한 텍스트로 표시한다. 승인과 봇 관리 버튼은 GTK UI에서 연다. |
| provider 턴 | 초기에는 필요한 입력만 전달하고 답변·구조화된 제안만 받는다. 후속 도구 호출은 서비스에서 선언한 동작과 인자 스키마만 사용할 수 있다. 화면·파일·모델 출력은 데이터로 취급한다. |
| 관리 worker | 승인된 repo·worktree·작업 범위에서만 실행한다. 보고 권한은 자신에게 배정된 job의 보고에 한정하며, 사용자 승인이나 일반 봇 조작 권한을 상속하지 않는다. |

위협 모델은 provider 바이너리와 Flowmux를 신뢰하고, 모델 출력·저장소 프로그램·
페이지·worker 입력을 별도의 신뢰 수준으로 취급한다. 같은 OS 계정으로 직접
로그인한 사용자는 관리 주체다. provider 설정 격리, 도구 비활성화와 경로 제한은
필요한 실행 전제다. 모델이 자식 프로세스를 실행할 수 있는 모드는 OS가 그
프로세스의 파일/소켓 접근, 자격증명, 네트워크 및 자식 프로세스 수명을 통제해야
한다. 환경 변수 제거, 파일 모드 `0600`, 프로세스 그룹, Git worktree만으로는
동일 UID 실행과 사용자 제어 경로를 분리할 수 없다.

터미널/worker 도입 단계에서 Linux 배포 형태와 macOS 각각에 대해 격리된 계정
또는 검증된 OS sandbox를 선정한다. 일반 GUI/Host·제어 소켓과 인증 브라우저
프로필은 실행 영역에서 접근할 수 없어야 한다. 필요한 연결은 job 보고 및
허용된 도구용 별도 채널로 제공한다. 이 경계를 실제 패키지에서 입증할 수 없는
조합에서는 모델에게 셸·worker 실행을 노출하지 않는다. 사용자 확인 카드도
이 전제를 완화하지 않는다.

### 4.2 Flowmux 동작 적용

- 허용 목록은 의미가 정해진 동작으로 구성한다. 예를 들어 자기 workspace의
  작업 상태 읽기, 승인받은 목적지의 브라우저 snapshot, 새 job 요청을 개별
  동작으로 정의한다. 모델에게 범용 `Request` 전달, 셸 문자열 필터,
  `browser eval`, 사용자 브라우저 세션·쿠키 가져오기를 노출하지 않는다.
- 봇의 고정 chat pane은 모델이 쓸 수 없다. 다른 봇과는 메일을 사용한다.
  사용자 workspace는 기본 비공개이며, 읽기도 사용자가 명시적으로 공유한
  workspace와 데이터 종류만 허용한다. 사용자 workspace의 쓰기는 정확한
  동작·대상에 대한 사용자 카드 또는 사용자가 만든 좁은 규칙이 필요하다.
- 작업 카드는 immutable 요청 ID, 정규화된 인자, 요청자, 대상 workspace/pane/
  surface와 세대, 만료를 함께 저장한다. UI에는 실제 적용 내용·범위를 표시한다.
  사용자가 여러 창에서 같은 카드를 승인해도 한 요청만 실행 후보가 된다.
  승인, 만료, 취소, 거절은 경쟁하는 단일 상태 전이다. `이번만`을 먼저 제공하고,
  영구 규칙은 초기에는 제공하지 않는다. 후속 규칙은 봇·동작·정확한 대상·만료로
  scoping하고 설정 UI에서만 생성·철회한다.
- 승인 예약과 실제 적용을 구분한다. 대상 창이 적용 준비를 확인하면 서비스가
  최종 권한 revision을 검사하고 짧게 유효한 일회 실행 허가를 기록한다.
  대상 창은 GTK에서 대기 없이 허가와 대상 세대를 다시 검사하고 적용한다.
  비동기 browser 입력은 실제 구동 callback에서도 surface, 문서·원점 및
  snapshot 수명을 확인한다. 승인 이후 문서·대상이 달라지면 다시 승인받는다.
  최종 허가 전 철회는 적용을 막고, 이후 철회는 이미 예약된 효과에 소급되지
  않는다. UI에서 발송 중과 적용 완료를 구분한다.
- 대상 창은 같은 등록 세대에서 실행 ID를 중복 처리하지 않는다. 외부 효과의
  ACK가 유실되고 대상이 최초 실행 여부를 증명할 수 없으면 서비스는
  `delivery_unknown`으로 닫고 자동 재발송하지 않는다. provider 승인과
  Flowmux 작업 승인은 서로 대체할 수 없다.
- 사용자 프로필과 분리한 브라우저에서 승인된 원점에만 접근한다. 주소 이동과
  새 창·리다이렉트에도 정책을 적용한다. native 입력에 표시·포커스가 필요하면
  사용자가 작업 영역을 열 때까지 대기한다. 봇이 workspace 포커스를 바꾸거나
  native 입력을 JavaScript 호출로 대신하지 않는다.

## 5. 대화와 provider 계약

### 5.1 접수·실행·복구

1. Chat 입력, 예약, 메일, 작업 보고는 접수 트랜잭션에서 대화 sequence와
   source ID를 얻는다. 같은 대화에서는 접수 순서대로 하나씩 실행한다. 반복
   예약과 Look around는 기본적으로 독립 대화로 보내고, 사용자가 특정
   대화를 지정한 경우 그 대화의 순서를 따른다. 자동 결과의 출처를 Chat에 표시한다.
2. 첫 출시 기본은 봇당 동시 한 턴이다. 사용자와 예약 대화가 모두 실행 가능하면
   다음 실행 순서는 사용자 입력을 우선하되 이미 접수된 같은 대화의 순서를
   바꾸지 않는다. 실행 중 턴을 사용자가 취소할 수 있다. 두 번째 슬롯은
   provider 격리와 상태 경합 검증을 통과한 후 검토한다.
3. 상태는 `queued → starting → running ↔ waiting_approval → terminal`이다.
   terminal 사유는 `completed`, `failed`, `cancelled`, `interrupted`,
   `delivery_unknown` 등으로 구별한다. 정상 응답의 수신·Chat 이력 기록·다음
   실행 가능 상태 전환을 같은 DB 트랜잭션에서 확정한다. 이어지는 native session
   저장 여부가 불명확하면 그 대화를 보류하고 기존 이력을 사용자에게 보여 준다.
4. 실행 전에 attempt ID와 시작 의도를 저장한다. provider 실행 핸들과 출생
   식별자를 저장하고, 등록 전에는 모델 입력·도구 호출을 열지 않는다. 핸들
   등록 이전까지 자식 수명을 관리하는 시작/종료 계약을 provider별로 검증한다.
   외부 효과가 가능한 후속 실행에는 별도 미확정 상태가 필요하다.
5. 사용자 취소와 shutdown은 먼저 provider의 취소 계약을 사용하고, 기한이
   지나면 소유가 확인된 프로세스·격리 단위만 정리한다. PID는 출생과 실행
   식별자를 함께 검사한다. `PR_SET_PDEATHSIG`나 process group은 보조 수단이다.
   이전 실행 종료를 입증하지 못하면 같은 session을 다시 쓰지 않는다. 관리
   프로세스 이탈을 보장할 수 없는 플랫폼은 도구 실행 공개를 미룬다.
   [Agent Host의 macOS 자식 회수 잔여 검증](agent-runtime-review.md#미해결-액션-아이템--완료-판정을-막는-항목)

반복 예약과 메일은 30분, Look around는 10분, Tidy-up은 5분을 초기 실행
한도로 둔다. 사용자 턴은 별도 자동 만료 없이 취소할 수 있다. 인증·한도 오류가
확정되면 해당 봇의 자동 dispatch를 멈추고 Needs you에 이유를 남긴다. 사용자가
재개하거나 provider가 준 재개 가능 시각 이후에만 새 항목을 실행한다. 시작이
확정된 기존 턴의 재실행에는 §5.1의 복구 조건이 그대로 적용된다.

별도 TUI에서 같은 native session을 동시에 열면 내부 큐로 직렬화할 수 없다.
첫 출시에서는 Flowmux Chat이 관리하는 session만 지속한다. 후속 Open in
provider는 검증된 managed lease 경로, 또는 지원이 검증된 별도 session 복제로
제공한다. provider·인증 profile 변경은 새 대화로 시작하고 기존 이력을 보존한다.

provider 자식은 검증한 실제 실행 파일로 시작한다. 평소 터미널의 shell/profile,
Flowmux 소켓·pane 환경 및 사용자 credential을 묶음으로 상속하지 않는다.
지원할 인증 경로와 설정 출처를 별도로 정한다. 새 대화와 resume에 동일한
실행 정책을 적용하고, 과거 session의 도구 설정이 복원되는 경우를 검증한다.

### 5.2 Adapter 경계와 확인된 근거

공통 인터페이스는 시작·재개, 정규화된 이벤트 스트림, 취소, 실행 별 capability를
제공한다. 이벤트에는 최소한 run ID와 provider session/turn 식별자를 싣는다.
표시용 텍스트 delta에는 크기·버퍼 상한을 적용한다. 종료 메시지와 프로세스 EOF를
구분하고, 프로세스 시작 실패·인증·quota·프로토콜 실패를 사용자에게 구별한다.

| provider | 권장 연결 | 구현에 앞서 확인할 항목 |
| --- | --- | --- |
| Claude Code | 실행마다 `-p`, 구조화 stream-json, 명시적 session ID 또는 resume. 해당 실행의 정상 result와 종료를 함께 확인한다. 선택적 승인은 Flowmux 전용 MCP permission prompt 경로를 검증한 뒤 지원한다. | 로컬 2.1.330에서 `--tools`, `--strict-mcp-config`, 세션 플래그를 확인했다. 문서상 `--bare`는 일반적인 OAuth/Keychain 경로에 제약이 있고 managed plugin hooks가 남을 수 있다. 응답 전용에서 일반 로그인, 설정 출처, MCP/skill/hook 차단, 취소 후 resume을 각각 검증한다. [CLI](https://code.claude.com/docs/en/cli-reference), [권한](https://code.claude.com/docs/en/permissions) |
| Codex | 처음에는 동시 실행 attempt별 `app-server` stdio 프로세스로 격리한다. `initialize → initialized → thread/start 또는 thread/resume → turn/start`. 일치하는 thread/turn의 `turn/completed`와 terminal status를 확인한다. 취소는 `turn/interrupt` 후 해당 턴의 종료를 기다린다. | 로컬 0.171.0 생성 스키마에서 `thread/start`·`thread/resume`의 `config` 및 `turn/start`의 `sandboxPolicy`를 확인했다. 이 필드가 있다는 사실만으로 hook/MCP/plugin/도구 차단과 자식 정리가 입증되지는 않는다. 버전별 startup 설정, 응답 전용, auth, resume, 서버 발신 요청 처리 fixture가 필요하다. [App Server](https://learn.chatgpt.com/docs/app-server) |

기존 Codex 사용량 코드는 실행 파일 탐색·초기화의 참고 또는 공용화 후보로
사용한다. 장기 양방향 요청, 승인의 요청 ID 매칭, 종료 및 실패 처리까지 그
프로브에 얹지 않는다. 공유 app-server 풀은 계측에서 필요성이 확인되고 실행
격리 정책이 동일할 때 검토한다.

응답 전용 capability는 도구, 외부 MCP·plugin, 전역·프로젝트 hook 및 skill,
별도 브라우저 호출이 끼어들지 않는 구성을 실제 실행으로 검증한 조합에만
부여한다. 조직 정책과 충돌하면 해당 기능을 비활성화한다. 모델 요청 자체에
필요한 provider 네트워크 접근은 허용 정책에 포함한다. 읽기 전용 작업은
Flowmux가 허용 자료를 수집해 응답 전용 실행에 전달한다. 승인 기능을 켠 후에는
서버 요청 ID, thread/run, item을 카드에 묶고 사용자 결정이나 만료 후 딱 한 번
응답한다. 모델이 보낸 텍스트는 승인으로 처리할 수 없다.

## 6. 예약, 이벤트, 기억

### 6.1 스케줄과 자동 실행

- `every`는 고정 UTC 간격, `daily HH:MM`·`weekdays HH:MM`은 예약에 저장한
  시간대 기준, `--once`는 확정된 UTC 시각이다. DST로 없는 시각은 건너뛰고,
  한 시각이 두 번 오면 첫 시각에 한 번 실행한다. 시스템 시간대 변경은 새
  예약부터 기본값에 반영한다.
- `(schedule_id, revision, scheduled_at_utc)`로 occurrence를 중복 제거한다.
  다음 시각 갱신과 턴 접수를 같은 트랜잭션에 넣는다. 반복 일정은 앱 중단 동안
  쌓인 후보 중 grace 안의 가장 최근 한 건, 1회 예약은 자기 occurrence 한 건을
  실행한다. 기본 grace는 각각 60분/24시간이며 해당 범위를 벗어나면 놓침으로
  기록한다. 실행 전 취소 여부와 예약 revision을 다시 검사한다.
- 일시정지 시 신규 실행과 이미 받은 턴의 dispatch를 보류한다. 실행 중 턴은
  사용자 취소로 별도 제어한다. 재개 시 받은 사용자 입력·메일은 큐를 이어가고,
  예약·이벤트는 유효기간을 재평가한다. 일시정지·소유권 이동·절전 중의 시계
  변화를 테스트한다. 절전 복귀 뒤 자동 실행은 2분 안정 대기 이후 시작하며
  사용자 입력은 예외다.
- 시간 정확도는 깨어 있음, 활성 상태, provider 실행 슬롯이 비어 있는 조건에서
  예정 시각 후 1분 이내의 시작으로 검증한다. 다른 경우에는 대기 이유를 표시한다.
  대기 시간과 실제 실행 시간을 따로 기록한다.
- 이벤트는 첫 확장에서 durable job report와 검증된 lifecycle 전이부터 받는다.
  lifecycle의 상태 변화만으로 사용자 입력 준비나 작업 완료를 판정하지 않는다.
  `port.listening`·`git.head_changed`는 발행 지점과 실제 변경 증거를 확보한 뒤
  추가한다. 이벤트는 출처 instance, event ID, causation ID로 중복을 제거한다.
  자신이 유발한 되먹임과 반복 handoff를 차단한다. 30초 모음 창, 최대 대기량과
  초과 표시를 둔다.
- `suggest`에서는 이벤트와 Look around가 제안만 남긴다. 사용자가 Start를 눌러
  실행할 때 새 권한 판단을 한다. `auto`를 켜도 동작 승인·격리 정책은 유지한다.
  사용자 입력을 제외한 scheduler/이벤트/메일/후속 실행은 봇당 시간당 6턴·동시
  1개를 시작값으로 하며 지속 이력으로 계수한다. 논리 작업 ID 기준 연속 실패
  3회 시 Needs you에 올린다. 큐 상한·유효기간을 넘긴 항목은 이유를 기록한다.

### 6.2 기억과 읽기 자료

- 사용자 편집은 revision을 갖는 서비스 명령이다. 동시 편집 시 최신 내용과
  충돌을 표시한다. 파일 편집은 명시적 가져오기에서 diff와 base revision을
  확인한다. 턴 시작 때 역할·기억 snapshot을 읽고 사용자가 지운 내용은 다음
  턴에 다시 넣지 않는다. 삭제된 내용을 모델이 직접 다시 제안하는 경우에도
  이전 기억과 함께 자동 병합하지 않고 사용자에게 새 제안으로 표시한다.
- 모델은 출처가 기록된 기억 추가·삭제 제안을 반환한다. Tidy-up은 해당 revision에
  대한 도구 없는 별도 실행으로 받고, 정리 제안은 사용자가 검토한다. 승인 때
  revision이 다르면 다시 생성한다. 여러 topic으로 옮기는 동작도 하나의 DB
  revision으로 적용한 뒤 파일 사본을 갱신한다. 이력은 민감 정보를 다시 노출할
  수 있으므로 사용자 삭제 시 어느 범위가 지워지는지 표시한다.
- 기억 제안은 현재 대화에 필요한 정보만 사용한다. 토큰이나 자격증명처럼
  제품이 민감하다고 표시하는 필드는 전달·기록 단계에서 제외한다. 외부 자료의
  문장을 별도 권한이나 운영 지시로 옮기지 않는다.
- Look around는 봇의 일정·Items 상태와 사용자가 공유한 제한된 작업 메타데이터만
  서비스가 수집한다. 초기에는 터미널 화면, 브라우저 본문, GitHub 조회를 제공하지
  않는다. 해당 데이터 종류는 공유 정책·최소 수집·provider에 전달될 내용을
  검증한 뒤 하나씩 추가한다. 과거 사용자의 승인 결정을 자료로 읽어도 현재
  동작의 권한으로 사용할 수 없다.
- 후속 Git diff/show 수집기는 Flowmux가 구성한 고정 argv로 실행한다. 항상
  `--no-textconv`와 `--no-ext-diff`를 붙인다. 저장소 설정·attributes가 외부
  프로그램을 부를 수 있는 다른 경로도 검증하고, 차단에 실패하면 수집기를
  열지 않는다. 회귀 fixture는 변환기와 외부 diff 드라이버가 설정된 저장소에서
  결과를 읽을 때 두 helper 모두 호출되지 않는지 검사한다.

## 7. worker와 job

초기 경로는 사용자가 승인한 repo/worktree와 **새 Agent Host 관리 worker**의
시작 프롬프트다. 현재 `LaunchAgent`에는 위치, argv, 환경, 실행 요청 ID가 있다.
Host의 실행 및 중복 제거를 재사용하되, job 상태와 사용자 승인은 봇 서비스가
소유한다. 현재 `flowmux-vcs::worktree`의 목록·삭제 API와 별개로 안전한 생성
경로가 필요하다. 생성 대상과 기준 커밋을 확정하고, 권한 범위 밖의 경로나
변경 사항이 남은 worktree를 정리 대상으로 삼지 않는다. [LaunchAgent](../crates/flowmux-ipc/src/agent_host.rs#L80),
[worktree API](../crates/flowmux-vcs/src/worktree.rs#L62)

- `job_id`는 원래 대화, 승인된 repo, Host request ID, agent instance/epoch에
  연결한다. 상태는 `queued → launching → running → reported`를 기본으로 하고
  `delivery_unknown`, `ended_without_report`, `cancelled`와 실패를 별도로 기록한다.
  전달 ACK와 작업 결과를 구분한다.
- 보고는 턴 토큰 대신 job ID, 대상 실행, 만료에 한정한 권한을 사용한다.
  시작한 봇 턴이 끝나도 이 권한은 유지되며, job의 종료나 권한 만료로 폐기한다.
  `(job_id, report_id)`로 재접수를 중복 제거하고, 보고와 원래 대화의 후속 턴을
  한 트랜잭션에서 기록한다. 새 소유자는 검증된 작업 실행과 보고 권한만 복구한다.
  중복 보고는 이전 접수 결과를 응답하고, 다른 실행이나 종료된 작업의 권한은
  거절한다. 보고 상태 `completed`·`blocked`·`failed`와 본문을 원 제출자 표시와
  함께 저장한다. 작업 결과가 검수됐는지는 별도 UI 상태다.
- Host가 살아 있는지, 해당 실행이 실제로 존재하는지 확인해 재연결한다.
  launch 응답 유실 시 같은 Host request ID로 계약 범위 내에서 확인하며 새
  request ID를 만들어 재발송하지 않는다. 신원·전달을 확정할 수 없으면 사용자
  확인 상태로 남긴다. 작업 보고가 없을 때 Stop은 관찰 신호이며 완료 조건은
  명시적 보고다. 프로세스가 종료되면 무보고 종료로 기록한다.
- 기존 agent에 보내는 Claude 메시징이나 checked PTY 입력은 별도 단계다.
  surface/native session/agent instance·epoch, 현재 prompt 세대와 **일반 새
  입력 준비**를 실행 직전에 확인하고 provider의 접수 증거를 요구한다. 현재
  `NeedsInput`은 사용자 질문·권한 대기까지 포함하고, `PaneSendKeys`는 활성 tab에
  쓴다. 이 조합으로는 안전한 전달이나 성공 판정을 만들 수 없다. 근거를
  제공할 수 없는 provider에서는 새 worker 경로를 사용한다.

## 8. 후속 제품 연결

- Items는 일 상태와 사용자 결정을 추적한다. Needs you에 대한 사용자의 답,
  상태 전환, 후속 턴을 함께 기록한다. 단순 댓글은 자동으로 Needs you를
  해제하지 않는다.
- 봇 간 메일은 `status`, `question`, `handoff`로 시작한다. 일시정지된 봇은
  받은 메일을 보관한다. 답은 원래 대화에 연결하고 message ID와 최대 전달
  깊이로 자동 왕복을 제한한다.
- 오퍼레이터는 사용자 대화를 입력받아 형식화된 관리 제안만 반환한다.
  생성·삭제·페르소나 변경은 사용자 GTK 제어 경로에서 적용한다. 페르소나
  변경은 revision 이력으로 되돌릴 수 있다. 다른 봇에서 온 메시지는 생성
  제안으로 표시할 수 있으며 관리 권한을 얻을 수 없다.
- 페르소나 제안은 `{bot_id, expected_soul_revision, markdown}`을 구조화된
  응답으로 전달한다. 서비스가 대상·본문 크기·revision을 검사해 카드와 diff를
  만든다. 사용자가 확인할 때 revision을 다시 검사하고 적용·이력 기록을 같은
  트랜잭션으로 처리한다. 테스트는 파일·셸 도구를 쓰지 않는 제안 제출부터
  사용자 승인까지 검증하고, 동시 편집으로 오래된 제안이 된 경우를 포함한다.
- 권한 모드는 설정 UI에서 사용자가 결정한다. 초기에는 응답 전용만 노출한다.
  도구 모드가 생기면 provider별 지원 조합을 표시하고, 오퍼레이터는 권한 모드나
  허용 규칙을 변경할 수 없다.
- Needs you, 완료, 실패 알림은 run/job ID로 중복을 줄이고 해당 항목을 연다.
  앱 장애 전송의 중복 가능성은 기록과 메시지 고유 ID로 줄이며 OS 알림의
  한 번 전달까지 보장하지 않는다.

## 9. 구현 순서와 통과 기준

| 단계 | 작업 범위 | 다음 단계로 넘어가는 기준 |
| --- | --- | --- |
| 0. 계약 확인 | Claude·Codex 실제 설치본의 설정 출처·인증·도구 비활성화·완료/취소·resume을 검증하고 비밀값을 제거한 fixture를 만든다. macOS/일반 Linux/Flatpak 조합별 지원 차이를 기록한다. | 지원 대상으로 표시할 조합에서 도구와 사용자 hook이 호출되지 않고 새 대화, resume, cancel이 동작한다. 구성이 불분명하면 기능이 차단된다. |
| 1. Chat 수직 구현 | 단독 소유 서비스, DB와 접수·구독, 사용자 UI 연결, bot workspace binding, 역할·기억의 사용자 편집과 revision, provider 하나로 생성 → Chat → 닫고 다시 열기. 이후 두 번째 provider는 같은 계약에 연결한다. | 격리 인스턴스에서 이력 재연결과 취소가 성공한다. 두 GUI의 동시 생성·요청 재접수·소유권 교체에서 봇/턴이 중복되지 않는다. 활성 사용자 workspace와 옛 상태의 로드·저장이 회귀하지 않는다. |
| 2. 예약과 첫 출시 | 1회·반복 예약, 일시정지, 기록·취소·알림, 켜짐 상태와 복구 이유 UI. | 두 GUI에서 만기와 인수가 겹쳐도 occurrence가 하나다. 절전·DST·시계 변경·지연·grace·취소 경합 시험이 통과한다. 앱을 정상 종료하거나 강제 종료한 후 결과 불명 턴을 자동 재실행하지 않는다. |
| 3. 사용자 결정과 읽기 기능 | Items, 기록과 승인 카드, 기억 제안·Tidy-up, 제한된 Look around, 메일, 오퍼레이터. | 카드 이중 클릭·재전송·만료와 철회, 권한 없는 접근, 기억 동시 편집·삭제, 메일 되먹임이 모두 검증된다. 사용자 입력과 UI 승인 경로가 분리돼 있다. |
| 4. 실행 기능 | 플랫폼별 프로세스 격리와 전용 보고 채널을 먼저 검증한다. 승인된 새 worker, 결과 수신, 제한된 브라우저 작업을 연결한다. | 격리 실행에서 일반 GUI/Host 소켓, 다른 workspace, 미승인 사용자 파일·브라우저 프로필에 접근할 수 없다. 두 창 사이 이동·새 문서·세션 교체 또는 최종 허가 전 철회 시 오래된 요청이 적용되지 않는다. worker 보고 전에 서비스가 인수돼도 보고가 한 번만 기록된다. |
| 5. 이벤트와 자동화 | 검증된 로컬 이벤트와 인과관계 차단, 자동 실행 예산, repo 작업 → reviewer → 브라우저 확인. | 이벤트 중복/폭주와 job→event 순환을 차단한다. 예산 소진에도 사용자 입력이 실행되고, 잘못된 대상 입력·완료 승격이 없다. 승인이나 격리가 필요한 조합은 UI에서 이유와 대기 상태를 보여 준다. |

테스트는 DB 트랜잭션과 정책 단위 검사, 버전 고정 provider fixture, 두 GUI 및
crash/인수 검사, 실제 provider와 GTK end-to-end 순서로 구성한다. 프로세스 시작
전후, 큐 commit 전후, 외부 효과 ACK 유실 및 창 이동의 경계에서 실패를 주입한다.
native 검증은 활성 사용자 세션과 분리된 인스턴스에서 수행한다. 구현 단계의
최종 판단은 관련 범위의 `scripts/test-ci.sh linux extended`와 macOS native
gate 결과를 포함한다. Flatpak은 지원한다고 표시하는 기능만 패키징 환경에서
검증한다.

### 제품 판단과 남은 검증

- GUI 소유 구조, 흐름이 끊기지 않는 Chat, 봇별 workspace, 명시적 job 보고는
  기존 방향을 이어 간다.
- 초기 응답 전용 모드, 단독 DB 쓰기, GTK 승인, 단일 동시 턴, 읽기 공유 opt-in,
  기억 변경 확인은 이번 검토에서 제안한 결정이다. 광범위한 영구 규칙이나
  사용자 전체 workspace 읽기를 활성화하려면 별도 사용자 권한 모델이 필요하다.
- 현재 확인 범위는 PR·소스·공식 문서, 로컬 CLI 버전/help, Codex 안정 스키마
  생성이다. 실제 로그인 턴, 도구/설정 비활성화, provider 취소와 resume,
  플랫폼별 격리 및 GUI 실행 검증은 구현 착수 기준에 남아 있다.
