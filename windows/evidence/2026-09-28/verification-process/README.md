<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Verification process repair

검증 스크립트의 종료 처리에 있던 무기한 `WaitForExit()`와 아직 끝나지 않은
stderr Task의 `.Result` 읽기를 현재 browser/key 경로에서 제거했다.
단계 전체는 새 Windows Job Object 실행기로 제한한다. 정상·실패·시간 초과
모두 소유 자식 프로세스를 정리하며, 5초마다 상태를 출력한다. WSL 빌드는
별도 POSIX process group 실행기로 제한한다. 자동 재시도하지 않는다.

`runner-self-check.json`은 새 실행기 안에서 다시 실행한 실제 Windows 검증이다.
성공/exit 7 보존/3초 timeout을 구분했고, timeout은 3.036초에 종료했다.
미완료 diagnostic Task 읽기가 반환하는 것도 확인했다. 각 Windows 결과의
`activeAfterCleanup`은 0이다. POSIX 성공/exit 7/2초 timeout도 확인했고,
실행 PID 266968 및 자식 266969가 남아 있지 않았다. 사용자 WSL flowmux
PID 787은 실행 상태를 유지했다. Desktop input, clipboard 접근은 없었다.

변경된 기존 검증 여섯 개를 실제 숨김 WebView2/ConPTY host에서 한 번씩 실행했다.
`regressions.json`의 38개군이 통과했다. 실제 elapsed는 각 7.1~14.2초,
합계 67.531초다. Browser wait의 27초 transport check 한 개는 빠른 실행에서
명시적으로 deferred로 기록했다. `-Extended` 실행은 여전히 같은 시간을 검증하며,
transport 변경 또는 최종 릴리스 때 필요하다. 이 deferred 항목을 이번 통과 수에
포함하지 않았다. 현재 Rust 다운로드 작업을 포함한 당시 debug binary로 실행했으며,
이 프로세스 개선 커밋은 제품 Rust 코드나 Linux/macOS 소스를 변경하지 않는다.

개선 뒤 다운로드 검증을 재개한 첫 결과도 보존했다. 27.833초에 실패를 보고했고
소유 프로세스 cleanup은 0개 잔류였다. Timeout snapshot에서 잘린 응답의 추가
native download records가 active slot을 차지함을 확인했다. 이는 다운로드 기능의
미해결 결함이며 테스트 통과로 처리하지 않았다. 관련 구현은 별도 진행 중이다.

실행 순서, 기본 제한 시간, 필요한 경우에만 수행하는 확장 검증은
[VERIFICATION.md](../../../scripts/VERIFICATION.md)에 정의되어 있다.
