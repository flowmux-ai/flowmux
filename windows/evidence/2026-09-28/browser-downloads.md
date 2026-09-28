<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Windows browser downloads — partial acceptance

Windows 전용 native download manager를 추가했다. WebView2 DownloadStarting을
처리하고 기본 다운로드 UI를 숨긴 뒤, Windows Downloads 폴더 안의 UUID staging
폴더로 저장한다. 숨김 debug 검증에서는 소유 테스트 state/downloads만 사용한다.
완료 파일은 worker가 이름 충돌을 피하며 이동한다. 기존 파일을 교체하지 않는다.
Native toolbar의 Downloads 버튼 및 `downloads list/show/cancel/remove/clear`를
제공한다. Remove/Clear는 완료 기록만 지우며 다운로드 파일은 보존한다. Clear는
진행 중 항목을 유지하고 Remove는 진행 중 항목을 거부한다. 파일 열기는 사용자의
명시적 native UI 동작으로만 가능하고 background host에서는 거부한다.

기본 이름은 WebView2가 제안한 이름이다. 유효한 UTF-8 Content-Disposition filename*
값의 확장자가 browser 선택 확장자와 일치하면 원래 codepoint를 보존한다. 실제
WebView2 112의 기본 제안은 NFD 자모와 combining accent를 NFC로 정규화했기 때문에
명시적 extended parameter 처리가 필요했다. 경로 부분, Windows 금지 문자,
DOS device 이름을 처리하고 stem/extension을 180/32 UTF-16 단위로 제한한다.
Clipping 이후에도 끝 공백/마침표 및 예약 이름을 재검사한다.
Surrogate pair는 나누지 않지만 grapheme cluster는 나뉠 수 있다. 잘못된/중복
parameter, UTF-8 이외 charset, 확장자 불일치는 native 이름으로 fallback한다.
파일 payload는 해석하거나 정규화하지 않는다. 이는 실제 Microsoft IME 검증이 아니다.

WebView2에는 일반 Win32 destination 경로를 전달한다. Rust canonicalize의 extended
namespace 경로를 그대로 넘긴 첫 실행은 FILE_FAILED로 실패했으며, Wry의 기존
adapter와 같은 경로 단순화를 적용한 뒤 전송됐다. Native 다운로드는 기존 파일을
덮어쓸 수 있으므로 앱이 먼저 고유 staging 경로를 만들고, 완료 뒤 MoveFileExW를
replacement 없이 실행한다. 최대 10,000개 충돌 후보를 확인한다. 최종 저장 실패는
완료 staging bytes를 보존하고 경로/오류를 표시한다.

한 창에서 활성 transfer/worker는 8개, 보유 기록은 50개다. 준비 worker는 15초 후
취소를 요청하지만 실제 종료 전까지 slot을 유지한다. Native Cancel 성공 후에는
StateChanged를 무기한 기다리지 않고 참조를 해제한 뒤 소유 staging을 정리한다.
기존 1초 timer가 진행 상태를 확인한다. 내부 전송 재시도가 새 native operation을
만들 수 있으므로 동일 surface/navigation/URI의 미완료 transfer가 다시 시작되면
재시도를 거부하고 원본을 실패 처리한다. 새 navigation으로 명시적 재시도가 가능하다.
이 정책은 같은 navigation에서 의도적으로 중복한 동일 URI 다운로드도 거부할 수 있다.
HTTP 요청 자체가 항상 한 번이라는 보장은 없다. 잘린 응답 fixture에서 수정 전에는
6개 요청과 중복 활성 기록이 관찰됐고, 수정 후에는 2개 요청과 실패 기록 하나,
활성 slot 0개로 정리됐다. CLI는 명령을 자동 재전송하지 않는다.

탭 닫기 전에 해당 download COM 객체와 observer/deferral을 해제한다. 이전 코드는
WebView를 먼저 닫은 뒤 timer에서 operation을 조회하며 host가 0xC0000005로 종료됐다.
종료 순서를 고친 동일 숨김 시나리오는 통과했다.
병렬 코드 리뷰 후 전역/surface 종료 flag로 재진입을 차단하고, 대기 중인
DownloadStarting queue도 borrow 밖에서 해제한다. Cancel API가 요청을 거부하면
취소 전 상태로 되돌려 사용자가 다시 취소할 수 있다. 해당 HRESULT 실패 및
아직 Start event가 처리되지 않은 정확한 종료 race는 native fixture에서 강제하지
않았으므로 모든 오류/종료 interleaving을 통과했다고 주장하지 않는다. 전체 host 종료도 download controller를
browser controller보다 먼저 해제한다. App exit/crash 때 staging 정리와 restart resume는
아직 보장하지 않는다. 최종 저장 worker가 시작된 뒤 취소는 거부한다.

최종 debug binary에서 전체 다운로드 8개군이 15.374초에 통과했고,
활성 다운로드 중 host 종료 1개군은 5.025초에 통과했다.
이는 [다운로드 runner](download-diagnostics/reviewed-downloads-runner.json)와
[종료 runner](download-diagnostics/reviewed-shutdown-runner.json)의 전체 소요 시간이다.
`native-browser-downloads-background.json`과 `native-browser-downloads-shutdown.json`이
근거다. 일반 다운로드 검증 종료 시 활성 항목 및 staging 폴더는 0개였다.
별도 host 종료 검증은 crash 없는 종료를 확인하며 staging 정리 보장은 포함하지 않는다.

- 목적지에 같은 이름의 파일이 있을 때 준비 실패/원본 보존/복구.
- 한글·NFD 자모·combining accent·emoji 파일명과 UTF-8 payload의 ordinal 일치.
- 동일 이름 충돌, 기존 bytes 보존, 빈 파일 및 Content-Length 없는 전송.
- 숨김 native 목록 표시와 기록 제거 시 파일 보존.
- 활성 항목의 remove 거부/clear 유지, 취소/임시 파일 제거/반복 취소.
- 잘린 응답의 중복 native 시작 거부, 원본 실패 및 slot 회수.
- 8개 동시 전송, 9번째 거부 및 전부 취소.
- 원본 terminal identity를 보존하며 browser tab close 취소.
- 별도 검증: 활성 전송 중 host 종료 시 native 객체 해제 순서.

Browser 기본 동작 6개군과 viewport PNG capture 6개군도 최종 debug binary에서 통과했다.
이번 프로세스 개선 때 기존 browser/key 38개군은 앞선 다운로드 binary에서 통과했으며,
그 결과를 최종 binary 전체 재검증으로 표시하지 않는다.

`download-diagnostics/`에는 실패 원본과 각 수정의 축소 재현 결과를 보존했다.
검증 과정 개선은 [별도 기록](verification-process/README.md)에 있다. 실패 후 제한
시간을 늘리거나 전체 suite를 반복하지 않고 broken/close 등 해당 case만 먼저 실행했다.

실제 UI의 위치/DPI/접근성, 파일/폴더 열기와 운영체제 연결 프로그램,
SmartScreen/antivirus/MOTW, 목적지 선택, pause/resume, 재시작 복원,
network filesystem 및 reparse-point 경쟁, 대량/장기 부하와 모든 runtime 조합은
미검증 또는 미구현이다. B10 및 G09는 partial 범위이며 전체 Windows 완료가 아니다.
기존 Linux/macOS 코드는 이 변경의 수정 대상이 아니다.

Native API의 [DownloadStartingEventArgs](https://learn.microsoft.com/en-us/microsoft-edge/webview2/reference/win32/icorewebview2downloadstartingeventargs?view=webview2-1.0.4129.50),
[DownloadOperation](https://learn.microsoft.com/en-us/microsoft-edge/webview2/reference/win32/icorewebview2downloadoperation?view=webview2-1.0.2849.39),
[Microsoft custom download 설계](https://github.com/MicrosoftEdge/WebView2Feedback/blob/main/specs/CustomDownload.md)를 참조했다.
Filename* 처리 기준은 [RFC 6266 §4.3](https://www.rfc-editor.org/rfc/rfc6266.html#section-4.3)과
[RFC 8187 §3.2.1](https://www.rfc-editor.org/rfc/rfc8187.html#section-3.2.1)이다.

최종 검증에서 Windows 독립 crate Linux 테스트 100개, 실제 Windows native Rust
테스트 130개, release all-targets Clippy(`-D warnings`)와 release build가 통과했다.
로그는 [Linux tests](browser-downloads-linux-tests.txt),
[native tests](native-rust-tests-browser-downloads.txt),
[Clippy](browser-downloads-clippy.txt), [release build](browser-downloads-release-build.txt)에 보존했다. 이 수치는 기존 GTK/macOS
애플리케이션 전체 회귀 테스트의 결과가 아니다. 새 IME/desktop/installer 조작은
수행하지 않았다. 기존 사용자의 WSL flowmux는 그대로 실행 중이다.

최종 NSIS 개발판 설치 파일을 다시 생성했고 GUI/CLI/console alias의 release doctor
3종도 숨김 실행에서 통과했다. 설치 프로그램 자체는 실행하지 않았다.
[산출물 SHA256](artifacts-browser-downloads.json),
[release 진입점](native-browser-downloads-release-entrypoints.json),
[installer 생성 로그](browser-downloads-installer-build.txt)를 함께 보존했다.
