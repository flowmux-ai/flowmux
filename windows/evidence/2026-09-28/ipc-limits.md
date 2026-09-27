<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Windows IPC 대기·자원 제한 검증

이번 단계는 기존 Windows 전용 IPC를 고정 크기 연결 풀과 취소 가능한 입출력으로
전환한다. 변경 범위는 `windows/`이며 Linux·macOS 코드와 root Cargo 파일·공유
core는 수정하지 않았다. 전체 114개 기능·13개 게이트의 완료 증거는 아니다.

기존 실행 파일의 숨김 임시 호스트에서 요청을 보내지 않는 클라이언트 24개를
연결했다. 모두 연결됐고 6초 뒤에도 모두 쓸 수 있었다. 호스트의 관측 스레드는
14개에서 38개로 늘었다(`native-ipc-limits-before.json`). 새 빌드에서 같은
검사는 16개를 허용하고 나머지 8개를 거부했다. 대기 연결이 유지되는 동안
관측 스레드 수는 증가하지 않았고, 6초 뒤 이전 연결은 모두 끊겨 있었다.
해당 PID의 전체 스레드 수는 WebView 등에도 영향을 받으므로 일반적인 메모리·
성능 수치로 해석하지 않는다.

서버는 시작 시 pipe·event·worker를 16개씩 만들고 연결이 끝나면 같은 pipe를
다시 사용한다. 보호된 own-user/SYSTEM DACL, 원격 접속 거부, PID 확인과 기존
JSON 한 줄 프로토콜을 유지한다. `ERROR_NO_DATA` 연결 복구 검사도 유지했다.
GUI에 제출한 요청 역시 최대 16개다. `Reply`의 모든 복제본이 해제될 때까지
permit를 보유하므로, 응답 시간 만료 후 반복 접속해도 멈춘 GUI의 요청 큐를
계속 늘릴 수 없다. 한도가 찬 경우 새 요청을 dispatch하기 전에 오류를 보낸다.

| 구간 | 제한 |
|---|---|
| 동시에 유지할 서버 연결·worker | 16개 |
| GUI가 보유한 미완료 IPC 요청 | 16개 |
| CLI 연결 슬롯 획득 | 3초 |
| 서버의 완전한 요청 수신 / CLI 요청 쓰기 | 각각 5초 |
| GUI 명령 응답 대기 | 15초 |
| 서버 응답 쓰기 | 5초 |
| 응답 후 peer 종료 대기 | 2초 |
| CLI의 완전한 응답 수신 | 25초 |
| 요청 / 응답 프레임 | 줄 끝을 포함해 1MiB / 16MiB |

읽기·쓰기 기한은 프레임 전체에 적용하며 조각이 들어올 때 갱신하지 않는다.
응답 JSON 직렬화도 크기 상한을 적용한다. 완전한 명령을 전달한 뒤 발생한 시간
만료는 취소나 미실행을 증명하지 않는다. CLI는 그 불확실성을 오류로 표시하고
전송한 명령을 자동 재전송하지 않는다. 연결 슬롯을 얻기 전의 경쟁만 제한 시간
안에서 재시도한다. 지연된 GUI 명령이 나중에 실행될 가능성은 남아 있다.

각 pipe에서는 한 번에 하나의 overlapped 작업만 진행한다. 시간 만료나 종료
신호를 받으면 `CancelIoEx`를 요청하고, `GetOverlappedResult`로 완료를 기다린
뒤 버퍼와 OVERLAPPED를 해제한다. 취소 요청 자체가 완료를 뜻하지 않는다는
[Microsoft 문서](https://learn.microsoft.com/en-us/windows/win32/api/ioapiset/nf-ioapiset-cancelioex)에 따른 순서다.
응답 뒤에는 peer 종료를 제한 시간 안에서 기다리고, 동기 `FlushFileBuffers`를
사용하지 않는다. 이는 [공식 overlapped pipe 예제](https://learn.microsoft.com/en-us/windows/win32/ipc/named-pipe-server-using-overlapped-i-o)의 종료 신호 방식을 따른다.
호스트는 터미널을 정리하기 전에 IPC를 중지하고 worker를 join한다.

`native-ipc-limits-background.json`은 숨김 실환경의 세 검사군이다.

- 위의 24개 대기 연결 시나리오에서 상한·만료와 이후 정상 CLI 응답을 확인했다.
- 테스트 스크립트 소유의 GUI 없는 pipe 서버가 요청을 읽은 뒤 아무 응답도
  보내지 않았다. 실제 `flowmuxctl`이 약 25초 뒤 오류로 종료했다. 이 서버는
  발견 파일을 게시하거나 받은 `new-workspace` 명령을 실행하지 않는다.
- 실제 숨김 호스트에서 15개 idle 연결과 quit을 보내고 응답을 읽지 않는 연결
  1개를 유지했다. 약 2초 뒤 호스트와 셸이 종료되고 발견 파일도 제거됐다.

Windows native Rust 테스트 38개가 통과했다
(`native-rust-tests-ipc-limits.txt`). 새 검사는 실제 Windows pipe를 사용해
읽기·쓰기 시간 만료와 인스턴스 재사용, accept/read/write/GUI 대기 중 종료,
용량 초과 프레임, 잘게 나눈 UTF-8 요청과 큰 한글 응답의 보존, 읽지 않은 큰
응답의 해제, 미완료 요청 permit와 복제본 수명, 읽지 않은 quit 응답을 검사한다.
pool 생성·해제를 10회 반복한 뒤 프로세스 handle 수가 검사 허용치(초기 수 + 2)를
넘지 않는지도 확인한다. 짧은 시간 제한을 사용하는 일부 native 단위 검사는
production의 5/15/25초 기한을 직접 증명하지 않으며, 그 값의 실제 CLI·호스트
동작은 위 별도 숨김 검사로 확인했다.

독립 Windows crate의 Linux 테스트 15개도 통과했다
(`linux-rust-tests-ipc-limits.txt`). 기존 다중 창·640회 연결 중단 검사를 다시
실행했다(`native-ipc-routing-limits-background.json`). 기존 상태 저장/복원
8개 검사군도 통과했다(`native-state-ipc-limits-background.json`). 특히 저장
실패 후 창 유지, quit 성공 응답, 강제 종료 후 복원, 한글·스타일 기록과 임시 모드
보존을 확인했다. Release build·Clippy 로그 및 설치 파일 해시는 같은 디렉터리의
`release-build-ipc-limits.txt`, `release-clippy-ipc-limits.txt`,
`artifacts-ipc-limits.json`에 기록한다. 설치 프로그램은 실행하지 않았다.

이 검사는 창 표시·foreground 획득·OS 키보드/마우스 입력을 사용하지 않았다.
실제 Microsoft IME·한자 후보·메뉴·DPI·접근성 승인은 여전히 미완료다. 부분적인
시작 실패의 fault injection, 치명적 pool 오류 뒤 자동 복구, 강제 종료의 발견
파일 정리, 장시간 부하·공정한 처리 순서, 다중 사용자·깨끗한 설치 환경 검증도
남아 있다. 일반적인 커널/드라이버 정지까지 일정 시간 내 취소를 보장한다는
의미가 아니다. 이전 workspace 재시작 실패의 역사적 원인 역시 확정하지 않았다.
O02와 전체 구현 목표는 계속 partial/pending으로 관리한다.
