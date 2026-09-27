<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Windows IPC 연결 복구·다중 창 검증

이번 변경은 `windows/` 내부의 Windows named pipe 서버·CLI·발견 파일 처리에
한정한다. Linux·macOS 구현, root Cargo manifest/lockfile, 공유 core를 변경하지
않았다. 전체 Windows 구현이나 한글 IME 승인을 완료한 단계는 아니다.

클라이언트가 pipe를 연 직후 닫고, 서버가 그 뒤 `ConnectNamedPipe`를 호출하면
실제 Windows에서 `ERROR_NO_DATA`(232)가 발생했다. 기존 수신 함수를 그대로
추출한 회귀 검사는 이 오류로 실패했다(`native-ipc-before-fix.txt`). 수정한
함수는 해당 연결을 `DisconnectNamedPipe`로 정리한 뒤 다음 연결을 기다린다.
동일한 native 검사가 이제 후속 클라이언트의 연결 성공을 확인한다.
이는 Microsoft의 [ConnectNamedPipe 동작 설명](https://learn.microsoft.com/en-us/windows/win32/api/namedpipeapi/nf-namedpipeapi-connectnamedpipe)과 일치한다.

발견 파일은 첫 pipe를 바인딩한 뒤 임시 파일에 완전히 기록·동기화하고
`MoveFileExW`로 교체한다. 쓰기·교체 실패 시 임시 파일을 정리하며 기존 파일은
남긴다. listener thread 생성 실패 시 새 발견 파일을 제거한다. 회복하지 못하는
수신·다음 pipe 생성 오류는 `host.log`에 기록하고 해당 발견 파일을 제거한다.
서버의 모든 실패 경로를 자동 복구한다는 의미는 아니다.

동시 읽기와 40개 기록의 연속 교체를 검사하던 첫 실행에서는 교체가 오류 5로
거부됐다(`native-ipc-atomic-first-trial.txt`). 공유 위반과 접근 거부에 한해
10ms 간격, 최대 500ms의 재시도 기간을 추가했다. 최종 검사는 읽힌 JSON이
항상 완전한 이전/다음 기록인지 확인한다. 파일을 의도적으로 잠근 검사에서는
계속 실패하고 이전 바이트가 유지되며 임시 파일도 남지 않았다. 이 재시도는
발견 파일 교체에만 적용한다. 요청을 보낸 후 CLI 명령을 재전송하지 않는다.

CLI는 발견 파일을 4KiB까지 읽으며 파일명·기록 PID·pipe 이름 PID의 일치와
canonical local pipe 이름을 검사한다. 실제 연결 뒤에는 Windows가 반환한
서버 PID를 확인한 다음에만 요청 바이트를 보낸다. native 테스트에서는
이름에 다른 PID를 넣은 실제 pipe를 만들어 거부와 요청 바이트 미전송을
확인했다. 이는 실행 파일 서명이나 배포본 인증을 검증하는 기능은 아니다.
명시한 `--pipe` 또는 상속된 `FLOWMUX_PIPE_NAME`이 끊어졌을 때 다른 창으로
넘어가지 않으며, 원래 Windows 오류를 표시한다. 클라이언트는
`SECURITY_IDENTIFICATION`을 지정한다.
[Rust OpenOptionsExt 문서](https://doc.rust-lang.org/std/os/windows/fs/trait.OpenOptionsExt.html#tymethod.security_qos_flags)는 named pipe에서 이 보안 수준을 지정하는 의미를 설명한다.

`scripts/verify-ipc.ps1`은 debug 호스트 8개를 모두 숨김·임시 모드로 실행했다.
`native-ipc-background.json`에는 다음 여섯 검사군이 기록되어 있다.

- 동시에 실행한 두 창에 한글·분해형 자모·이모지를 포함한 서로 다른 이름을
  설정했다. 40회 식별 요청에서 지정한 PID가 유지됐고 이름도 섞이지 않았다.
- 요청 없이 닫거나 JSON 일부만 보내고 닫는 연결 400회와 잘못된 JSON 요청
  이후에도 정상 CLI 요청이 응답했다.
- 아무 요청도 보내지 않는 클라이언트 8개를 유지한 상태에서 다른 요청이
  응답했다. 이 검사는 무한 대기나 무제한 client 자원 사용을 해결하지 않는다.
- 없는 pipe를 명시하거나 환경 변수로 상속해 `new-workspace`를 호출하면
  연결 오류를 반환했고 두 실제 창의 workspace 개수는 바뀌지 않았다.
- 한 창의 quit 응답·프로세스 종료·발견 파일 제거 뒤 다른 창은 계속 응답했다.
  종료된 pipe로 보낸 변경 명령이 생존 창에 적용되지 않았다.
- 여섯 번의 추가 시작·종료에서 신선한 완전한 발견 파일과 실제 PID를 확인했다.
  각 실행에서 연결 중단 40회를 추가하여 총 640회의 비정상 종료 뒤 정상 요청을
  확인했다. 각 창은 표시되지 않았고 foreground도 아니었다.

Windows native Rust 테스트 29개와 Linux의 독립 Windows crate 테스트 15개가
통과했다(`native-rust-tests-ipc.txt`, `linux-rust-tests-ipc.txt`). 기존 workspace
6개 검사군과 상태 저장/복원 8개 검사군도 같은 debug 빌드에서 통과했다
(`native-workspaces-ipc-background.json`, `native-state-ipc-background.json`).
이 회귀 검사에는 한글 기록 복원, 안정적인 surface/PID, 실패한 저장 뒤 창 유지,
quit 응답 및 상태 파일 잠금 동작이 포함된다. Release build와 Clippy 기록은
`release-build-ipc.txt`, `release-clippy-ipc.txt`에 있다.
`artifacts-ipc.json`은 실행 파일과 갱신한 설치 파일의 해시다. 설치 프로그램은
실행하지 않았다. 창 표시·foreground 획득·OS 키보드/마우스 입력은 사용하지 않았다.

앞선 `workspace-startup-failed-trial.json`의 재시작 실패 당시에는 프로세스 상태와
발견 파일 시각이 기록되지 않았다. 이번 오류 재현이나 성공한 반복 실행을 그
실패의 원인 확정으로 해석하지 않는다. 실제 IME·한자 후보·물리 키보드·DPI·메뉴
승인도 이번 백그라운드 검사로 대신할 수 없다.

읽기/쓰기/flush 기한, 클라이언트 수·스레드 자원의 상한, listener와 worker의
협조적 취소/종료, pipe 할당 실패 후 복구, 강제 종료 후 오래된 발견 파일 정리,
자동 발견 선택의 전체 경쟁 조건 및 다중 사용자/배포 환경 검증은 남아 있다.
특히 동기 worker의 대기 시간은 완전히 제한되지 않는다. O02는 partial로
기록하고 전체 114개 기능·13개 게이트 범위를 유지한다.
