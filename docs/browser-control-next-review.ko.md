<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Flowmux 브라우저 사용성과 기능 확장 재검토

> 이 문서의 현황·위험 표는 `581058bb` 분석 기준이다. 이번 보완은
> 우선순위 1·2와 프로필/로컬 보안 경계에 한정하며, 현재 CLI 계약과 호환성
> 변경은 [browser-workflow.md](browser-workflow.md)에 기록한다. 의미 기반
> 탐색·부분 snapshot·입력 전 대기·typed 결과 확인·프로필 키 충돌 방지·Linux
> 격리 fallback·쿠키 미지원 응답·IPC UID/권한·파일 범위·비밀번호 출력·출처
> 표시를 구현했다. 불변 UUID 기반 프로필 이름 변경, 외부 클라이언트 인증,
> 임의 코드 sandbox, 교차 출처 frame 제어는 이번 구현 범위가 아니다.


검토일: 2026-10-24. 기준: `main`, `581058bbce8646b9b043ce0fd035cc7f8393722b`, 버전 `0.11.4`.

**다음 투자는 의미 기반 요소 탐색, 작업 결과를 보여주는 UI, 계정 재사용, 폼·파일 작업 단축에 우선 배분하는 것이 좋다.** 기본 클릭·입력·캡처·미디어 조작은 이미 구현되어 있다. 진단·제어권 인계·action receipt도 이번 main에 반영되었으므로 신규 기능 목록에서 제외한다. 남은 격차는 사용자가 작업을 쉽게 맡기고, 진행 상태를 이해하고, 로그인이나 실패 이후 이어서 끝내는 경험이다.

엔진 교체는 첫 단계로 권하지 않는다. 현재 WebKit 기반에서도 개선 가능한 편의 기능이 많다. 다만 **포커스를 빼앗지 않는 백그라운드 네이티브 입력, 교차 출처 프레임 제어, Wayland 입력**은 별도 실증이 필요한 구조적 격차다. 탭 제어권이나 SDK 추가만으로 해결되지는 않는다.

이 문서는 현재 소스와 공식 문서·공개 SDK를 비교한 분석이다. 외부 제품의 성공률·속도 수치는 재현하지 않았으며 Flowmux와의 성능 비교값으로 사용하지 않는다. 기준 커밋의 Linux·macOS 회귀 검사는 [Test CI](https://github.com/flowmux-ai/flowmux/actions/runs/40095327314), 메모리 검사는 [Sanitizers CI](https://github.com/flowmux-ai/flowmux/actions/runs/40095327340)에서 통과했다. CI 통과는 아래 신규 기능이나 모든 실제 사이트의 호환성을 보장하지 않는다.

## 1. 현재 기반과 남은 격차

| 영역 | 현재 Flowmux | 남은 사용자 경험 |
| --- | --- | --- |
| 기본 조작 | 탐색, ref 클릭·더블클릭·hover, fill/select, native type/press, 좌표 클릭·드래그, 스크롤 | 명령별 호출·관찰을 사용자가 조합해야 함. 고수준 폼 작성과 반복 작업 단축 부족 |
| 페이지 이해 | DOM 요약, 접근성 이름 일부, open Shadow DOM, same-origin iframe | role/label 기반 탐색 API, 영역별 요약, 변경분, 상태·검증 오류 표현 부족 |
| 실행 안정성 | stale ref 거부, 가려짐·비활성·이동 대상 검사, 두 렌더링 프레임 안정성 확인 | 일시적인 준비 지연에 대한 제한된 자동 대기와 구조화된 실패 안내 부족 |
| 작업 제어 | 탭별 task ID, Take control/Continue/Stop/Clear, 변경 명령 직렬화 | 작업 이름·단계·팝업 묶음·결과 카드·재시작 후 재개 계획은 별도 기능 |
| 결과 확인 | receipt, 텍스트/URL 기대 조건, 팝업·다운로드·대화상자 관측 | 다운로드 완료, 입력값 변화, 특정 요소 상태, 새 결과 발생을 표현하는 조건 부족 |
| 로그인 | 프로필별 지속 저장소, 저장하지 않는 세션 모드 | 계정 선택·분리·삭제 UX, 로그인 인계 안내, 실제 인증 상태 이전 미완성 |
| 파일·영상 | 파일 선택, 다운로드 상태/취소/폴더, media 재생·일시정지·seek·mute, PNG·무음 MP4 | 업로드 완료 확인, 다운로드 결과 카드, 재생 속도·볼륨·자막, 실제 화면·오디오 녹화 |
| 연동 | CLI/IPC, 에이전트 스킬, 별도 임시 WebDriver 창 | 기존 탭용 얇은 고수준 SDK와 이벤트 구독. WebDriver는 기존 pane에 붙지 않음 |

근거: [CLI 명령 정의](../crates/flowmux-cli/src/main.rs), [DOM 관찰·동작](../crates/flowmux-browser/src/scripts.rs), [작업 흐름 계약](browser-workflow.md), [WebDriver 범위](webkit-automation.md). `fill`은 값 설정과 합성 이벤트를 사용하는 경로이고 `type`은 네이티브 편집 경로다. 두 기능의 지원을 동일한 입력 의미로 설명하면 안 된다.

## 2. 공개 기술과 비교한 판단

비교 기준은 기능 제공 방식과 사용 흐름이다. 전용 브라우저의 엔진 변경과 Flowmux의 임베디드 WebView API는 구현 조건이 다르다.

| 비교 축 | ego(lite)의 공식 설명·공개 SDK | Flowmux와의 차이 및 판단 |
| --- | --- | --- |
| 작업과 계정 연결 | 작업 Space 생성 시 프로필 선택, 로그인 환경 재사용 | Flowmux도 프로필 저장소는 있음. 신규 엔진보다 계정 선택과 작업 생성 연결을 먼저 개선 |
| 사용자와 동시 작업 | 사용자 탭·마우스·포커스를 방해하지 않는 작업 공간을 설명 | Flowmux native 입력은 활성·표시 탭을 요구하고 GUI 프로세스 안에서 직렬화. 기능상 큰 격차 |
| 관찰 비용 | 엔진 snapshot과 SDK의 subtree·locator 기능 | Flowmux는 전체 순회·Markdown·ref map·페이지 텍스트를 반환. 부분 관찰과 필요한 정보만 반환하는 API가 유효 |
| 참조 수명 | SDK는 frame/document/node에 묶인 ref와 부분 snapshot 참조 관리를 설명 | Flowmux는 새 snapshot이 이전 ref 집합을 교체. 우선 의미 기반 재탐색과 명확한 무효화 이유를 제공 |
| 실행 단위 | Node.js에서 Page/TaskSpace API로 여러 동작을 조합 | Flowmux도 쉘로 여러 CLI를 묶을 수 있음. 차이는 호출 가능 여부보다 표준화된 결과·오류·중단 처리의 편의 |
| 복잡한 페이지 | 교차 출처 프레임·엔진 snapshot을 강조 | Flowmux의 top-document DOM 접근은 inaccessible frame을 읽지 못함. 별도 frame context 경로가 필요 |
| 확장 생태계 | Chromium 기반 일반 브라우저 기능을 활용 | Flowmux의 WebKit profile 이름은 Chrome/Firefox 엔진 전환이 아님. 확장·DRM·기업 인증 호환성은 실제 사이트별 검증 필요 |
| 운영체제 | 조사한 제품 소개 문서는 macOS 제공, Linux 계획으로 설명 | Linux 기능 우위를 추정할 수 없음. Flowmux의 Linux/X11 지원과 Wayland 제한을 별도로 평가 |

근거: [제품 소개](https://www.egolite.app/document/en/docs/product-introduce), [Space](https://www.egolite.app/document/en/docs/space), [Snapshot](https://www.egolite.app/document/en/docs/snapshot), [공개 SDK](https://github.com/citrolabs/ego-lite/blob/d392204d1ea1a8027a591ab2b0129c2e11d85f26/package/ego-browser/README.md), [네이티브 API 계약](https://github.com/citrolabs/ego-lite/blob/d392204d1ea1a8027a591ab2b0129c2e11d85f26/docs/native-bindings-api.md).

공개 문서에도 off-screen 내용·canvas·sandboxed cross-origin iframe의 관찰 한계가 명시되어 있다. API 계약에는 subtree의 frame-local node ID 충돌 주의사항도 있다. 따라서 “전용 엔진이면 모든 페이지가 완전하게 제어된다”는 전제로 설계하지 않는다. 공개 SDK 계약과 제품 설명은 브라우저 엔진 내부의 독립 검증과도 구별한다.

## 3. 사용자 편의 중심의 우선 개선

### 3.1 의미 기반 찾기와 필요한 영역만 관찰

가장 먼저 권하는 공통 기능이다. 사용자가 “배송지의 우편번호를 입력하고 저장”이라고 요청할 때 전체 snapshot에서 ref를 반복적으로 찾는 비용을 줄인다.

- role+name, label, text, placeholder와 부모 영역을 조합해 후보를 반환한다. 여러 후보가 있으면 임의로 첫 항목을 클릭하지 않는다.
- snapshot에 선택적으로 disabled, checked, expanded, focused, editable, invalid, 설명 텍스트와 frame 정보를 넣는다. 현재 이름 계산은 재사용한다.
- 영역·viewport·interactive-only·출력 예산 옵션을 제공하고 잘림 여부를 명시한다. 현재 페이지 텍스트는 4,000자로 제한되지만 전체 ref/tree의 총량 제한과는 다르다.
- 동일 노드임을 검증한 ref만 사용한다. stale ref를 이름이 비슷한 새 노드로 자동 치환하지 않는다.
- 변경분 관찰은 후속 단계다. 문서 세대·frame·snapshot 기준점과 삭제된 ref를 함께 전달해야 한다.

첫 구현은 전체 접근성 트리 교체보다 기존 DOM 이름 계산과 ref 검증에 탐색·필터를 붙이는 범위가 적절하다. CSS 생성 텍스트나 closed shadow까지 지원한다고 표시하지 않는다. role/label 중심 탐색은 [Playwright locators](https://playwright.dev/docs/locators)의 공개 계약을 참고할 수 있다.

**합격 기준:** 같은 이름의 버튼 두 개, 한글 label, 중첩 iframe, SPA 노드 교체를 포함한 fixture에서 대상이 유일할 때만 실행한다. 전체 snapshot 대비 반환 바이트·모델 입력 토큰·왕복 횟수를 측정하되, 의미 정보 손실과 오조작을 함께 검사한다.

### 3.2 제한된 준비 대기와 더 강한 결과 조건

현재는 안정성 검사에서 이동·가림을 발견하면 오류를 반환한다. 로딩 표시가 곧 사라질 때마다 에이전트가 snapshot과 클릭을 반복하는 불편을 줄일 수 있다.

준비 대기는 입력을 보내기 전, 동일 대상의 visible/enabled/stable/receives-events 조건에만 제한한다. 제어권·탭·문서가 바뀌면 취소한다. 이미 전달한 클릭·제출을 다시 수행하는 재시도와 분리한다. [자동 대기 조건](https://playwright.dev/docs/actionability).

receipt에는 다음과 같은 형식화된 기대 조건을 추가하는 것이 좋다.

| 조건 | 사용자에게 확인할 결과 |
| --- | --- |
| 요소 visible/hidden/enabled, 값 equals | 편집 결과 반영·모달 닫힘·검증 성공 |
| 정확한 URL 또는 origin+path | 단순 부분 문자열보다 명확한 도착 위치 |
| 값·텍스트의 before→after 전이 | 기존에 있던 “저장됨” 문구를 새 성공으로 오인하지 않음 |
| 새 팝업 ID와 준비 상태 | 팝업 생성과 사용 가능한 문서를 구별 |
| 특정 download ID의 complete | 다운로드 시작과 파일 완성을 구별 |

현재 `outcome_confirmed`는 지정한 조건을 관찰했다는 의미다. 사이트 업무의 성공을 독립적으로 검증했다는 뜻은 아니다. 이벤트의 시간적 동시 발생을 인과관계로 단정하지 않는 기존 계약도 유지한다.

**합격 기준:** 클릭 전에 성공 문구가 존재하는 경우, 지연 팝업, 다운로드 실패, 서버 응답 지연에서 성공을 잘못 보고하지 않는다. 결과 불명 상태에서 제출 횟수는 증가하지 않아야 한다.

### 3.3 작업 패널과 여러 탭의 작업 묶음

기존 제어 버튼과 이벤트 이력을 사용자에게 읽히는 작업 화면으로 연결한다. 첫 화면은 **작업 이름 / 현재 단계 / 기다리는 이유 / 사용자에게 필요한 행동 / 마지막 결과**면 충분하다.

팝업·관련 탭은 작업 목록에 묶어 보여주되, 자식 탭에 제어 권한이 자동으로 생기지는 않게 한다. 사용자가 허용한 origin 범위와 함께 작업에 명시적으로 편입한다. 로그인·MFA·보안키·권한 요청을 감지하면 이유와 돌아갈 위치를 보여주고 기존 Continue 흐름을 이용한다.

재시작 후에는 목표·마지막 확인 결과·재개 위치만 복원한다. 오래된 ref와 실행 권한을 복원하거나 마지막 클릭을 자동 재생하지 않는다. 초기 버전은 영속 실행 엔진 없이 사용자가 확인하는 체크포인트 목록으로 충분하다.

**합격 기준:** 부모/자식 탭 전환, 즉시 닫히는 팝업, MFA 중단, 앱 재시작 후에 어떤 탭을 이어서 확인해야 하는지 알 수 있다. 일반 탭과 작업 탭의 소유권이 섞이지 않아야 한다.

### 3.4 계정과 프로필을 작업 생성에 연결

프로필 기능은 새로 만드는 것이 아니라 현재 저장소와 설정을 완성하는 작업이다. 작업 시작 시 “기존 업무 계정 / 다른 계정 / 이번 작업만”을 고르고, 현재 계정 범위와 데이터 저장 여부를 표시한다. 첫 단계는 Flowmux 안에서 직접 로그인한 상태를 재사용하는 방식이 적절하다.

선행 보완이 세 가지 있다.

1. **저장소 식별자:** [profile.rs](../crates/flowmux-browser/src/profile.rs)의 `sanitize`는 ASCII 이외의 이름을 제거한다. 한글만 있는 서로 다른 이름은 `custom-unnamed`가 될 수 있고, 공백·기호 차이도 같은 slug로 합쳐질 수 있다. 사용자 표시 이름과 불변 저장소 ID를 분리해야 한다. 기존 저장소는 자동 병합·이동하지 않고 매핑을 유지한다.
2. **Linux 실패 처리:** [프로필 세션 생성](../crates/flowmux/src/ui/browser_pane_webkit.rs)의 저장 디렉터리 생성 실패 경로는 기본 NetworkSession으로 대체한다. 계정 격리를 약속한 작업에서는 명시적 실패 또는 별도의 임시 세션을 제시해야 한다. 이 분기의 존재는 확인했으나 실제 다른 계정 유출을 재현한 결과는 아니다.
3. **쿠키 이전 결과:** [주입 함수](../crates/flowmux/src/ui/window/mod.rs)의 `inject_cookies_into_webkit`는 placeholder이고 개수만 반환한다. [IPC 처리](../crates/flowmux/src/ipc_handler.rs)는 이를 `CookiesImported`로 응답한다. 진단에서 미지원임을 알려주는 것에 더해 실제 명령 결과도 추출·주입·로그인 확인을 구별해야 한다.

이후 인증 상태 이전은 사용자가 선택한 출처와 프로필에 한정한다. 쿠키 복사만으로 localStorage·IndexedDB·SSO·MFA·passkey·기기 결합 인증까지 이동한다고 약속하지 않는다. 인증 상태 파일은 자격증명으로 취급한다. [인증 상태 재사용과 노출 위험](https://playwright.dev/docs/auth).

**합격 기준:** “업무”와 “개인”이 서로 다른 저장소를 사용하고, 저장소 오류에도 계정이 합쳐지지 않는다. 로그인은 유지하되 로그아웃·프로필 삭제의 영향을 사용자가 이해할 수 있어야 한다.

### 3.5 폼과 파일 작업을 한 번에 끝내는 편의 기능

| 제안 | 최소 범위 | 완료 확인 |
| --- | --- | --- |
| 폼 항목 묶음 입력 | label→대상 해석, 항목별 방식·결과, 제출은 별도 단계 | 입력값과 검증 메시지, 일부 실패 항목 표시 |
| 네이티브 교체 입력 옵션 | 기존 fill 호환성을 유지하면서 선택 영역 교체·취소 결과 제공 | React/마스크 입력/contenteditable에서 실제 보존된 값 |
| 명시적인 붙여넣기 | plain text부터 제공, OS 클립보드 보존 | 한글·줄바꿈·긴 본문·취소 이벤트 |
| 중첩 영역 스크롤 | 현재 ref 기반 스크롤에 컨테이너·방향·완료 조건 추가 | 가상 목록에서 새 항목이 실제로 관찰됨 |
| 파일 선택·업로드 결과 카드 | 선택 파일·요청 출처·전송 상태와 실패 안내 | 파일 선택과 서버 업로드 완료를 구별 |
| 다운로드 완료 대기 | ID 기반 대기, 파일 위치·폴더 열기 | matching download의 complete 상태 |
| 표·목록 읽기 | 필요한 열/행과 출처 URL을 구조화해서 반환 | 누락·중복·페이지 이동 범위를 표시 |
| 미디어 추가 조작 | 볼륨·속도, 제공되는 텍스트 트랙·구간 캡처 | 실제 상태·재생 시간 변화. DRM 우회나 자막 생성은 별도 범위 |

HTML drag-and-drop의 `DataTransfer` 의미와 현재 네이티브 포인터 드래그를 구별한다. rich HTML paste, OS 파일 드래그, 임의 사이트 업로드 완료 감지는 후속 검증 대상으로 둔다. 현재 media 구현은 재생·정지·탐색·mute를 제공하므로 영상 재생 자체를 신규 과제로 분류하지 않는다.

### 3.6 여러 단계를 묶는 얇은 SDK와 반복 작업 저장

기존 IPC 위에 `find → fill → click → expect`를 조합하는 작은 클라이언트 계층을 제공하면 호출마다 출력 형식을 해석하는 부담이 줄어든다. 먼저 가장 자주 쓰는 에이전트 언어 하나에서 검증하고, 필요하면 같은 계약으로 MCP를 제공한다.

단계마다 task/surface 검증, 취소, 결과 확인을 유지한다. batch는 트랜잭션이나 rollback이 아니며, 오류가 나면 완료 단계와 아직 수행하지 않은 단계를 구별한다. 페이지의 `eval-async`와 호스트 SDK 실행도 구별한다. 임의 JS를 허용하는 새 런타임이 간단한 작업의 기본값이 될 필요는 없다.

반복 작업에는 의미 기반 대상·입력 파라미터·기대 결과만 저장하고, 로그인 비밀·실제 개인정보·오래된 ref는 저장하지 않는다. 사이트에서 읽은 텍스트가 자동으로 실행 가능한 “학습된 절차”가 되지 않게 한다.

**합격 기준:** 동일한 5단계 폼 작업에서 CLI 조합과 결과가 같고, 3단계 취소 후 4·5단계가 실행되지 않는다. 줄어든 도구 호출·모델 왕복과 실제 완료율을 함께 측정한다.

## 4. Linux와 macOS의 별도 개선 범위

| 과제 | Linux | macOS | 판단 |
| --- | --- | --- | --- |
| 탐색·폼·결과 UI·SDK | 공통 DOM/IPC 계약 활용 | 동일 계약 활용 | 공통 기능 우선 개발 |
| 사용자와 동시 작업 | X11 입력은 현재 포커스 의존. Wayland 키·포인터 미지원 | WKWebView native 입력은 활성·표시 탭 필요 | 전용 작업 창은 간섭을 줄일 수 있지만 무간섭 병렬 입력의 증거는 아님 |
| Wayland 입력 | 엔진 경로 조사 후 RemoteDesktop portal/libei 실증 | 해당 없음 | compositor·권한 취소·IME·배율별 검증이 필요 |
| 교차 출처 프레임 | 프레임별 스크립트/브리지, 필요 시 web-process 확장 평가 | WKFrameInfo 대상 평가·frame lifecycle 연결 조사 | 공통 frame/document ID 계약부터 정하고 플랫폼별 구현 |
| 실제 화면 녹화 | ScreenCast portal/PipeWire. 오디오는 별도 경로 검증 | ScreenCaptureKit의 화면·오디오 경로 검토 | 현재 무음 snapshot MP4와 별도 모드 |
| 기존 로그인 브라우저 연결 | 명시적 외부 backend 실증 가능 | 동일한 선택지 평가 | WebKit으로 막힌 실제 업무가 있을 때만 보조 연결 |

Wayland의 [RemoteDesktop portal](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.RemoteDesktop.html)은 사용자 승인과 허용된 입력 장치를 다룬다. 데스크톱 입력 권한이 곧 특정 브라우저 탭만의 권한이라는 뜻은 아니다. 입력 전 대상·포커스 재검증, 승인 취소와 세션 종료 시 즉시 정지가 필요하다.

교차 출처 프레임은 동일 출처 정책을 해제하는 방식으로 구현하지 않는다. [WKWebView API](https://github.com/WebKit/WebKit/blob/main/Source/WebKit/UIProcess/API/Cocoa/WKWebView.h)는 특정 frame/content world를 대상으로 평가하는 API를 제공하지만, 그것만으로 frame 탐색·교체 시 무효화·좌표 변환·허용 origin 계약이 완성되지는 않는다. isolated world도 페이지 DOM과 분리된 별도 데이터 저장소가 아니다.

실제 화면 캡처는 [ScreenCaptureKit](https://developer.apple.com/videos/play/wwdc2022/10156/)과 [ScreenCast portal](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.ScreenCast.html)을 이용하는 별도 실증이 적절하다. 창 캡처에 터미널·다른 탭이 섞이지 않는지 확인하고 저장 전에 미리보기를 제공한다. 앱 단위 오디오가 특정 탭의 오디오만 의미하지 않을 수 있으므로 탭별 녹음 격리도 따로 검증한다.

**엔진 변경 판단 기준:** 고정 viewport, 백그라운드 native 입력, 기업 로그인·확장 호환성 등 실제 차단 사례를 모으고 구현·업데이트 비용을 비교한다. 단순 snapshot 축소나 폼 묶음 입력을 위해 Chromium fork부터 시작하지 않는다. 봇 탐지 회피와 CAPTCHA 우회는 개선 목표에 포함하지 않는다.

## 5. 기능 확장과 함께 필요한 보안 보완

현재 stale ref 검사, frame/노드 검증, 입력 전 가려짐 검사, 권한 요청의 기본 거부, 다운로드 덮어쓰기 방지, 별도 임시 WebDriver 환경은 유지할 가치가 있다. 추가 보안은 이 방어를 재작성하기보다 새 기능이 여는 경로에 적용한다.

| 보완점 | 현재 근거·위험 | 권장 조치 |
| --- | --- | --- |
| 작업 ID의 의미 | `control status`에서 ID를 읽을 수 있고 읽기 작업은 소유자 밖에도 허용. 현재는 협력적 동시 실행 제어 | ID를 인증 비밀로 설명하지 않음. 외부 agent 연결을 넓힐 때 별도 세션 인증·scope 적용 |
| 로컬 IPC 권한 | socket bind에서 명시적 mode 설정이 없고 peer credentials는 PID 추출에 사용. runtime 경로/umask에 보호 수준 의존 | 전용 디렉터리 소유권·mode, socket 권한과 peer UID 검증. 공유 디렉터리·symlink·느슨한 umask fixture 추가 |
| 프로필 계정 격리 | 이름 정규화 충돌과 Linux 기본 세션 fallback | 불변 profile ID와 명시적 실패 처리. 기능 확장 전에 보완 |
| 파일 업로드 | 읽을 수 있는 경로 선택은 해당 파일을 사이트에 제공하는 동작 | task별 허용 파일/폴더·요청 origin·파일 목록 표시. 디렉터리 업로드와 symlink 대상 재검증 |
| 출력과 자격증명 | `value`·eval·화면·파일 결과는 민감정보를 담을 수 있음 | 비밀 입력은 OS 저장소/사용자 입력 경로 검토. 일반 snapshot·로그·export에서 비밀값 제외. 마스킹만으로 접근 차단을 주장하지 않음 |
| 권한 요청의 출처 | Linux는 현재 WebView URI의 host, macOS media는 요청 origin의 host 표시 | 가능하면 실제 요청 frame origin과 top-level origin, scheme/port를 함께 표시. 출처 불명일 때 안전하게 인계 |
| 동작 허용 범위 | raw eval과 페이지 JS는 click 이름만으로 제한할 수 없음 | 제한 모드는 임의 eval·JS predicate를 차단하고 typed 관찰 API 제공. 필요할 때 권한 확대를 명시 |
| 전송·삭제·게시 | 잘못된 제출이나 prompt injection은 정상 기능을 오용할 수 있음 | 사용자 승인 범위를 작업에 기록. 새 수신처·대량 변경·허용 범위 밖 부작용은 실행 전 구체적인 미리보기 |

근거: [작업 제어 구현](../crates/flowmux/src/ui/browser_control.rs), [IPC 서버](../crates/flowmux-ipc/src/server.rs), [runtime 경로](../crates/flowmux-config/src/paths.rs), [Linux 권한 요청](../crates/flowmux/src/ui/browser_pane_webkit.rs), [macOS 권한 요청](../crates/flowmux/src/ui/browser_pane_macos.rs). IPC 항목은 코드상의 강화 필요점이며 다른 OS 계정에서의 침입을 재현했다는 의미는 아니다. 같은 OS 사용자 권한의 임의 코드를 capability token 하나로 완전히 격리할 수도 없다.

모든 클릭에 승인을 요구하면 사용성이 나빠진다. 이미 승인된 작업 범위는 유지하고, 파일 제공·대량 변경·외부 전송 등 의미 있는 경계에서만 다시 확인한다. 범용 클릭 API만으로 모든 결제·삭제를 완벽하게 판별할 수 있다고 약속하지 않는다. 제한 모드의 “읽기”도 GET 요청의 부작용이나 방문 기록까지 없는 상태는 아니다.

웹페이지의 지시는 사용자 명령이나 정책 변경 권한이 아니다. 페이지 본문과 도구 결과를 신뢰할 수 없는 입력으로 구분하고, origin·파일·도구 범위는 모델 밖에서 검사해야 한다. [OWASP의 도구 경계 검증·최소 권한 지침](https://cheatsheetseries.owasp.org/cheatsheets/LLM_Prompt_Injection_Prevention_Cheat_Sheet.html). 탐지 모델이나 금칙어만으로 완전한 방어를 주장하지 않는다.

## 6. 권장 구현 순서

| 순서 | 작업 단위 | 사용자 효과 | 상대 난이도 |
| --- | --- | --- | --- |
| 선행 | 쿠키 가져오기 결과 정정, 프로필 ID 충돌·fallback 보완, IPC 경계 점검 | 로그인·계정 분리의 신뢰성 | 중간. 기존 저장소 호환성 주의 |
| 1 | 의미 기반 find + 부분 snapshot + 상태 정보 | 적은 관찰로 정확한 대상 선택 | 중간 |
| 2 | 준비 대기 + typed expect + 다운로드 완료 | 불필요한 재시도와 완료 오판 감소 | 중간 |
| 3 | 작업 이름·진행 이유·결과 카드·팝업 묶음 | 작업 상태와 사용자 역할이 명확해짐 | 중간 |
| 4 | 계정 선택 + 폼 묶음 + 파일 결과 카드 | 실제 업무에서 반복 입력 감소 | 중간 |
| 5 | 얇은 SDK + 명시적으로 저장하는 반복 절차 | 에이전트 호출·작성 비용 감소 | 중간 |
| 병행 실증 | Wayland, 교차 출처 frame, 무간섭 입력, 실제 화면 녹화 | 현재 못 하는 작업의 범위 확대 | 높음. 플랫폼별 성공 조건 필요 |

난이도는 코드 구조에 따른 상대 평가이며 일정 추정이 아니다. 1~4가 주된 제품 투자이고, 보안은 이 기능의 진입·출력 경계에 붙인다. 전면 엔진 교체·자체 에이전트 플랫폼·범용 워크플로 엔진을 이 단계에 함께 넣지 않는다.

## 7. 완료 판정을 위한 시나리오

아래는 다음 구현을 평가할 시나리오다. 이번 분석에서 새 기능이 성공했다고 기록하는 실험 결과가 아니다.

| 시나리오 | 관찰해야 할 완료 상태 |
| --- | --- |
| 한글 글자 수 편집 | 공개 글자 수 도구에서 제한 이하의 실제 값과 화면 증거. 제출은 하지 않음 |
| 영문 다중 필드 폼 | 로컬 공개 fixture의 label 기반 작성, 필드별 오류, 중복 제출 0회 |
| 계정 선택·MFA 인계 | 테스트 계정 두 개의 저장소 분리, 사용자 로그인 이후 같은 작업으로 복귀 |
| 일정 조사 | 공개 시간대 페이지에서 날짜·도시·최종 결과 URL과 캡처. 캘린더 생성은 별도 판정 |
| 팝업·파일 다운로드 | 일시 팝업을 놓치지 않고 부모 유지, download ID와 완성 파일 일치 |
| iframe·가상 목록 | same-origin/open shadow는 성공, 미지원 frame은 이유와 인계, 다른 frame 오조작 0회 |
| 사용자 동시 편집 | 사용자가 터미널에 한글 입력 중 에이전트가 잘못된 창에 입력하지 않음. 백그라운드 입력 지원을 주장하려면 별도 검증 |
| 보안 fixture | 페이지의 파일 유출 지시·다른 origin 이동·승인 변경 시 제한된 도구에서 범위 밖 실행 차단 |

같은 모델·프롬프트·페이지 버전·프로필·viewport·timeout 조건에서 완료율, 잘못된 성공 보고, 모델 왕복, 출력 토큰, 수행 시간, 사용자 개입, 포커스 간섭을 함께 측정한다. 로컬 고정 fixture는 회귀 기준으로, 공개 사이트는 실제 사용성 확인으로 구분한다. Linux X11·Wayland와 macOS에서 같은 조건을 반복하되 미지원은 실패 원인과 함께 별도 집계한다.

가장 작은 다음 제품 묶음은 **의미 기반 찾기 + 부분 snapshot + 결과 조건 확장 + 작업 결과 카드**다. 로그인 신뢰성을 먼저 보완하고 이 묶음을 완성한 뒤, Wayland와 frame 제어 실증 결과로 큰 플랫폼 투자의 순서를 결정하는 것이 적절하다.
