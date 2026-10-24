<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Flowmux 브라우저 사용성과 기능 확장 재검토

검토일: 2026-10-25. 기준: `main`, `9644fadf538e6762a7670f8a9852f99f54cbc4ec`, 버전 `0.11.4`.

> 후속 구현: P1-1~P1-5를 반영했다. `browser task`로 작업 안내를 표시하고,
> Results에서 관련 탭·캡처·다운로드·업로드 관측을 확인한다. `browser form`은
> 항목별 검증을 반환하며 `Element` 결과 조건은 same-origin iframe과 open shadow의
> 원래 ref를 검증한다. 최신 사용법과 한도는 [작업 흐름 계약](browser-workflow.md#task-progress-and-result-cards)에 있다.
> 아래 비교·우선순위는 위 기준 커밋에서 작성한 구현 전 검토 기록이다.
> 작업 이력 영속화, 계정 관리·불변 ID, SDK, cross-origin·Wayland 확장은 후속 범위다.

**다음 우선순위는 작업 진행·결과 화면, 폼·파일 작업 단축, 계정 선택과 로그인 복귀다.** 이전 우선순위 1·2였던 의미 기반 탐색·부분 snapshot과 준비 대기·typed 결과 조건은 구현됐다. 프로필 이름 충돌, Linux 기본 세션 fallback, 쿠키 가져오기 성공 오표시와 로컬 IPC 경계도 보완됐다. 이 항목들을 다시 미구현으로 분류하지 않는다.

남은 큰 차이는 명령의 개수보다 사용자가 작업을 맡기고 결과를 확인하는 흐름이다. 현재 WebKit 구조에서도 상당 부분 개선할 수 있다. 백그라운드 네이티브 입력, 교차 출처 frame, Wayland 키·포인터 입력은 별도 기술 실증이 필요하다. 외부 제품의 성능 수치나 모든 사이트의 호환성을 Flowmux의 목표 성능으로 가정하지 않는다.

## 1. 구현된 기반과 실제 잔여 범위

| 영역 | 현재 구현 | 남은 범위 |
| --- | --- | --- |
| 기본 조작 | 탐색, 클릭·드래그·스크롤, fill/select/check, native type/press | 폼 묶음 작성, 실패 항목만 수정, 긴 본문·복잡한 편집기 편의 |
| 요소 찾기 | `find`는 snapshot 별칭. role/name/label/text/placeholder, `--within`, interactive/viewport, 출력 예산, 상태·ARIA 설명 | frame을 포함한 결과 검증, 중복 후보 선택 안내, 필요한 경우에만 변경분 관찰 |
| 준비와 결과 | 입력 전 제한된 준비 대기, 정확한 URL·요소 상태·값·값/텍스트 변화·새 팝업 준비·download ID 완료 조건 | 조건 조합, 실패 이유의 사용자 표현, 서버 저장과 단순 화면 변화를 구별하는 결과 UI |
| 작업 제어 | 탭별 task, Take control/Continue/Stop/Clear, 변경 명령 직렬화, receipt·최근 이벤트 | 작업 이름·단계·대기 이유·관련 탭·결과물 통합, 재개 위치 안내 |
| 계정 | 프로필별 저장소, 충돌 없는 이름 인코딩, Linux 저장 실패 시 별도 임시 세션 | 작업별 계정 선택, 이름 변경 시 로그인 유지, 과거 저장소 복구 안내 |
| 파일·미디어 | 파일 선택 범위 검증, 다운로드 조회·취소·완료 대기, 재생·정지·seek·mute, PNG·무음 MP4 | 업로드 완료 판정, 결과 파일 열기·미리보기, 볼륨·속도·제공되는 자막 |
| 연동 | CLI/IPC, 스킬, 별도 임시 WebDriver 창 | 기존 탭용 작은 SDK, 구조화된 오류·이벤트 대기, 사용자가 저장하는 반복 절차 |

근거: [CLI](../crates/flowmux-cli/src/main.rs), [프로토콜](../crates/flowmux-ipc/src/protocol.rs), [DOM 스크립트](../crates/flowmux-browser/src/scripts.rs), [현재 사용 계약](browser-workflow.md), [WebDriver 범위](webkit-automation.md).

현재 구현의 경계도 함께 유지해야 한다.

- `find`는 후보를 반환하며 첫 항목을 자동 클릭하지 않는다. 새 snapshot은 이전 ref 집합을 교체한다. open shadow와 same-origin iframe은 탐색하지만 closed shadow와 cross-origin iframe은 탐색하지 못한다.
- 부분 snapshot은 출력량을 제한한다. 페이지 전체 관찰 비용이나 모델 토큰 비용이 같은 비율로 줄었다는 측정 결과는 아니다. 기본 상한은 2,000 refs와 262,144 JavaScript 문자 단위이며 바이트·토큰 상한과 다르다.
- 포인터 준비는 최대 5초, fill/select/check/uncheck 준비는 최대 2초다. 이미 보낸 입력·제출은 자동 재실행하지 않는다. `fill`의 값 설정·합성 이벤트와 `type`의 네이티브 편집은 의미가 다르다.
- `outcome_confirmed`는 지정한 조건을 관찰했다는 의미다. 서버 저장이나 이벤트의 인과관계를 보장하지 않는다. 기존 상태도 만족할 수 있는 `Value`와 새 변화를 요구하는 `ValueChanged`를 구분해야 한다.
- 작업 제어권은 탭 단위이며 계정 저장소 격리와 다르다. 같은 프로필의 탭은 로그인 상태를 공유할 수 있고, 새 팝업은 부모 task 권한을 자동 상속하지 않는다.

## 2. 공개 기술과 비교한 판단

전용 브라우저의 공개 기능 설명과 SDK 계약을 비교 기준으로 삼았다. 브라우저 엔진 내부의 독립 검증이나 동일 조건의 성능 실험 결과는 아니다.

| 비교 축 | 공개 기능·계약 | Flowmux 판단 |
| --- | --- | --- |
| 작업 공간 | ego(lite)는 작업별 BrowserContext, 사용자와 분리된 탭, 수동 인계와 작업 후 탭 보존을 설명한다. [Space](https://lite.ego.app/document/en/docs/space) | 제어권 인계는 이미 있음. 관련 탭·계정·진행·결과를 한 작업으로 보여주는 UX가 남음. task ID를 저장소 격리로 해석하면 안 됨 |
| 계정 선택 | 작업 생성 시 profile ID를 지정하고 표시 이름과 ID를 구분한다. [Native API](https://github.com/citrolabs/ego-lite/blob/main/docs/native-bindings-api.md) | 이름 충돌은 해결됐지만 표시 이름이 저장소 identity인 구조는 남음. 프로필 관리와 작업 시작 연결이 유효 |
| 사용자와 병행 | 사용자 페이지·마우스·포커스를 방해하지 않는 작업을 설명한다. [Space](https://lite.ego.app/document/en/docs/space) | 가장 큰 구조적 차이. Flowmux의 native 입력은 대상의 활성·표시 상태에 의존. 작업 창 분리만으로 무간섭 입력을 보장할 수 없음 |
| 관찰과 참조 | viewport/subtree, interactive-only, 길이 제한, 가능한 경우 stable locator를 제공한다. [Snapshot API](https://github.com/citrolabs/ego-lite/blob/main/docs/native-bindings-api.md#await-egosnapshotoptions) | 부분 관찰·의미 기반 탐색의 격차는 줄었음. 영구 ref를 약속하기보다 재탐색과 명확한 무효화 안내부터 개선 |
| 복잡한 페이지 | 엔진에서 snapshot을 만들며 교차 출처 frame을 강조한다. 다만 snapshot 문서는 canvas·sandboxed frame 등의 한계를 명시한다. [제품 설명](https://www.egolite.app/document/en/docs/product-introduce), [Snapshot](https://lite.ego.app/document/en/docs/snapshot) | cross-origin frame은 실제 기능 격차. 먼저 현재 지원하는 same-origin frame의 탐색·입력·결과 확인을 일관되게 연결 |
| 여러 동작 조합 | Node 런타임에 TaskSpace/Page API를 제공하며 기존 전역 helper는 호환용으로 유지한다. [SDK README](https://github.com/citrolabs/ego-lite/blob/main/package/ego-browser/README.md) | 쉘로 조합하는 기능 자체는 있음. 표준 오류·취소·부분 완료 결과를 제공하는 얇은 SDK가 편의 개선점 |
| 반복 절차 | 제품 소개의 자동 경험 축적은 coming soon으로 표시된다. [제품 설명](https://www.egolite.app/document/en/docs/product-introduce) | 자동 학습·자동 실행을 확정된 경쟁 기능으로 취급하지 않음. 먼저 검토 후 저장하는 수동 템플릿으로 충분 |

공개 native API도 설치된 앱 빌드에 따라 가용성이 달라진다고 명시한다. subtree는 특정 빌드에서 기능 플래그가 필요하며 frame-local node ID 중복을 오류로 처리한다. 비교 대상에 안정적인 영구 ref나 무제한 frame 지원이 있다고 가정해 Flowmux 설계를 확대하지 않는다. [Native API](https://github.com/citrolabs/ego-lite/blob/main/docs/native-bindings-api.md)

## 3. 다음 제품 개선 우선순위

### 3.1 작업 진행과 결과를 한 화면에서 확인

가장 먼저 기존 [제어 표시줄](../crates/flowmux/src/ui/browser_control.rs)과 receipt를 연결한다. 현재 표시줄은 실행·사용자 입력 필요·중단 등의 상태를 알려주지만 어떤 업무를 어느 단계까지 수행했는지는 표현하지 않는다.

첫 범위는 **작업 이름, 현재 단계, 기다리는 이유, 사용자가 할 일, 마지막 확인 결과, 관련 탭·파일**이다. 진행률을 추정한 숫자보다 “로그인 필요”, “파일 생성 대기”, “입력은 전달됐으나 저장 확인 안 됨”처럼 관측 가능한 상태를 표시한다. 원본 URL·확인 시간·캡처·다운로드 파일을 결과 카드에서 연다.

현재 페이지에서 작업 시작, 필요한 관련 탭 추가, 로그인 후 돌아갈 탭 표시를 연결한다. 팝업을 목록에 묶는 것과 제어 권한을 부여하는 것은 분리한다. 기존 Continue 버튼을 재사용하고, 에이전트가 사용자 인계를 스스로 해제하지 않게 한다.

첫 버전의 재시작 복구는 마지막 확인 결과와 재개 위치만 복원한다. 오래된 ref, 계정 권한, 마지막 클릭을 자동 복원·재생하지 않는다. 기존 이벤트 이력은 메모리의 최근 64건이므로 영구 작업 이력처럼 표시하지 않는다.

**완료 기준:** 지연 팝업·MFA·다운로드 실패·앱 재시작에서도 사용자가 무엇을 확인하고 어디서 이어야 하는지 알 수 있다. `finish` 선언만으로 결과 카드를 “업무 성공”으로 바꾸지 않는다.

### 3.2 폼 작성과 파일 결과까지 연결

다음으로 한 번의 요청으로 폼을 작성하고 필드별 결과를 돌려주는 기능을 만든다. 기존 `find → fill/select/check → expect`를 조합하되, 묶음 입력은 트랜잭션이 아니므로 일부 성공과 미실행 항목을 구별한다. 제출은 별도 단계로 둔다.

| 편의 기능 | 최소 범위 | 완료 판정 |
| --- | --- | --- |
| 폼 묶음 작성 | label/role로 후보 확인, 항목별 값·방식·검증 오류 반환 | 실제 보존된 값 확인, 오류 항목만 수정, 중복 제출 없음 |
| 입력 방식 안내 | 일반 fill과 native 편집을 명시적으로 선택 | 한글·여러 줄·React 제어 입력·contenteditable에서 유지된 값 확인. 실패 후 무조건 다른 방식으로 재입력하지 않음 |
| frame 안의 결과 조건 | 기존 same-origin/open-shadow ref 경로를 활용해 관찰 대상 지정 | 부모·자식 frame에 같은 selector가 있어도 잘못된 성공 없음 |
| 파일 결과 카드 | 선택한 파일과 요청 출처, download ID·완료 경로·폴더 열기 | 파일 선택, 업로드 완료, 다운로드 완료를 별도 상태로 표시 |
| 표·목록 읽기 | 지정한 열·행을 구조화하고 URL·수집 범위 포함 | 현재 화면과 전체 목록을 구분, 페이지 간 중복·누락 표시 |

특히 [현재 결과 조건](../crates/flowmux/src/ui/window/mod.rs)의 요소 조회는 최상위 `document.querySelector`를 사용한다. 탐색 가능한 iframe/open shadow의 대상을 같은 방식으로 검증할 수 없고, selector가 여러 요소와 일치하면 첫 요소를 관찰한다. 이는 cross-origin 지원과 분리해 먼저 해결할 수 있는 구체적인 격차다. 새 대상 지정 방식은 frame/document 수명과 후보 유일성을 검사하고, 기존 selector 조건의 의미를 몰래 바꾸지 않는다.

업로드 완료는 범용 file chooser가 알 수 없다. 사이트가 제공하는 완료 항목·파일명·상태를 명시적 조건으로 확인하고, 그런 증거가 없으면 “파일 선택 완료 / 서버 처리 미확인”으로 남긴다. 다운로드 완료 조건은 이미 있으므로 새로 구현할 항목은 결과 연결과 사용 화면이다.

준비 대기를 더 늘리는 것보다 대상 교체, 가려짐, 비활성 탭, 플랫폼 미지원, 조건 시간 초과를 구분해서 안내하는 편이 유용하다. 요소 준비와 결과 assertion을 구분하는 계약은 [Playwright actionability](https://playwright.dev/docs/actionability)도 참고할 수 있다.

### 3.3 계정 선택과 로그인 복귀

작업 시작 시 기존 프로필 또는 이번 작업용 임시 프로필을 선택하고 저장 여부를 표시한다. 프로필은 실제 사이트 계정명을 보장하지 않으므로, 로그인 확인 전에는 “업무 프로필”을 곧바로 특정 사용자 계정이라고 표시하지 않는다. 로그인·MFA·보안키는 사용자에게 인계하고 확인 후 같은 작업으로 복귀한다.

[현재 프로필 키](../crates/flowmux-browser/src/profile.rs)는 UTF-8를 손실 없이 인코딩하므로 서로 다른 한글 이름의 충돌은 해결됐다. 다만 이름 변경이 저장소 변경이며 긴 이름은 파일시스템 한도를 넘을 수 있다. 프로필 관리 UI를 만들 때 불변 ID와 표시 이름을 분리하고, 기존 경로를 유지하는 매핑부터 도입한다. 이름 변경을 위해 데이터를 복사하거나 과거 충돌 저장소를 자동 병합하지 않는다.

과거 정규화로 정보가 사라졌던 이름은 현재 새 저장소를 사용한다. 기존 데이터는 보존돼 있지만 재로그인이 필요할 수 있으므로 복구·재로그인 안내가 중요하다. Linux 임시 fallback에는 로그인 유지가 안 된다는 표시와 저장소 복구 후 다시 여는 동선을 붙인다.

호스트 쿠키 가져오기는 현재 `unimplemented`를 정확히 반환한다. 이를 실제 로그인 이전으로 확장하는 일은 별도 기능이다. 우선 Flowmux 안에서 로그인한 프로필 재사용을 완성한다. 이후에도 쿠키 주입만으로 SSO·IndexedDB·passkey·기기 결합 인증까지 이전된다고 표시하지 않는다. 인증 상태 파일은 계정을 사칭할 수 있는 비밀정보다. [인증 상태 보관 지침](https://playwright.dev/docs/auth)

**완료 기준:** 두 프로필의 계정 분리, 이름 변경 후 로그인 유지, 저장소 오류 표시, 재로그인 후 작업 복귀를 Linux와 macOS에서 각각 검증한다. 프로필 삭제·로그아웃이 영향을 주는 탭과 데이터를 미리 보여준다.

### 3.4 작은 SDK와 사용자가 저장하는 반복 절차

한 언어에서 기존 IPC를 감싸는 작은 클라이언트를 먼저 제공한다. surface/task 지정, 의미 기반 찾기, 입력, typed expect, 취소, 구조화된 오류만 공통화한다. MCP는 실제 연결 수요가 확인된 뒤 같은 계약을 노출한다. 브라우저 페이지의 `eval-async`와 호스트에서 실행하는 SDK 코드는 별개다.

이벤트 조회의 sequence·잘림·종료 상태를 SDK에서 처리하면 팝업·다운로드를 반복 조회하는 코드가 줄어든다. 스트리밍 구독은 폴링 왕복이나 누락 문제가 측정될 때 추가한다. 작업의 세 번째 단계에서 취소되면 네 번째 단계를 실행하지 않으며 이미 끝난 동작은 되돌려졌다고 보고하지 않는다.

반복 작업은 사이트·의미 기반 대상·입력 파라미터·기대 결과를 사용자가 검토하고 저장하는 방식부터 시작한다. 비밀번호·개인정보·오래된 ref는 저장하지 않는다. 페이지 내용이 자동으로 실행 절차나 도구 권한으로 승격되지 않게 한다. 예약 실행은 중복 실행 방지, 로그인 만료, 사용자 부재 시 인계 정책이 검증된 뒤 검토한다.

**완료 기준:** 동일한 5단계 작업에서 CLI와 결과가 같고 중간 취소 이후 추가 입력이 없다. 호출 수뿐 아니라 완료율·모델 왕복·출력량·사용자 개입도 측정한다.

### 3.5 미디어와 캡처의 선택적 확장

영상 재생 자체는 이미 지원한다. 다음 작은 기능은 볼륨·재생 속도, 페이지가 제공하는 텍스트 트랙, 구간 지정 캡처다. 조작 성공은 반환값뿐 아니라 실제 재생 시간·상태 변화로 확인한다. DRM 콘텐츠, 모든 코덱, 자막 생성은 이 기능에 포함하지 않는다.

현재 MP4는 snapshot을 합치는 무음 기록이며 가속 영상·WebGL 레이어가 빠질 수 있다. 실제 화면·오디오 녹화는 별도 모드로 구분한다. 아래 플랫폼 실증에서 특정 탭만 캡처·녹음할 수 있는지 확인하기 전에는 “탭 녹화”로 약속하지 않는다.

## 4. Linux와 macOS의 개선 범위

| 영역 | Linux | macOS | 권장 접근 |
| --- | --- | --- | --- |
| 작업 화면·폼·프로필·SDK | 공통 DOM/IPC 계약 활용 | 동일 계약 활용 | 공통 제품 기능 먼저 구현하고 양쪽 native 회귀 확인 |
| 키·포인터 입력 | X11 및 관련 라이브러리 필요. Wayland 키·포인터 주입 미지원. native 텍스트 편집은 별도로 가능 | 활성·표시 browser tab 필요 | 시작 전 실행 가능 여부와 필요한 사용자 행동을 표시 |
| 사용자와 동시 작업 | X11 포커스 경쟁과 프로세스 간 간섭 검증 필요 | 가려진 창·비활성 탭에서 렌더링/입력 준비 지연 검증 필요 | 먼저 입력 대상 변경 시 안전한 중단. 별도 창만으로 무간섭 지원을 선언하지 않음 |
| frame 제어 | same-origin 계약 완성 후 frame별 브리지·web-process 경로 조사 | WKFrameInfo와 frame 대상 평가, 교체·navigation 무효화 조사 | 공통 frame/document ID 계약부터 정하고 cross-origin은 별도 실증 |
| 요청 출처 | 파일 선택은 top-level origin, 요청 frame은 불명으로 표시 | 요청 frame origin과 top-level 표시 | Linux에서 frame 출처를 얻을 수 없으면 불명을 유지하고 민감 동작은 사용자 인계 |
| 실제 영상·오디오 | ScreenCast portal/PipeWire, 오디오 경로 별도 확인 | ScreenCaptureKit 활용 가능성 검증 | 다른 탭·터미널·앱 소리 혼입 여부와 권한 철회 검사 |

Wayland 키·포인터는 엔진 내부 입력 경로와 [RemoteDesktop portal/libei](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.RemoteDesktop.html)를 비교 실증한다. 데스크톱 입력 권한은 특정 탭에 한정된 권한이 아니다. GNOME/KDE, 배율, 한글 IME, 사용자 포커스 이동, 승인 취소를 통과해야 지원으로 표시한다.

[WKWebView 공개 API](https://github.com/WebKit/WebKit/blob/main/Source/WebKit/UIProcess/API/Cocoa/WKWebView.h)는 frame/content world를 지정한 평가를 제공한다. 이것만으로 frame 발견·수명·좌표·출처 권한이 해결되지는 않는다. 동일 출처 정책을 해제해 호환성을 확보하지 않는다.

[ScreenCaptureKit](https://developer.apple.com/videos/play/wwdc2022/10156/)과 [ScreenCast portal](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.ScreenCast.html)은 현재 snapshot 기록과 다른 캡처 경로다. 창 범위·오디오 범위·권한·성능을 각각 검증한다. 특히 앱 단위 오디오가 탭 단위 격리를 뜻하지 않는다.

엔진 전환이나 외부 브라우저 보조 연결은 현재 WebKit으로 막힌 실제 업무 사례를 모은 뒤 결정한다. 확장, 기업 로그인, cross-origin, 백그라운드 입력의 필요성과 유지·보안 업데이트 비용을 함께 비교한다. 폼 묶음이나 작업 화면을 위해 Chromium fork부터 시작할 이유는 없다.

## 5. 기능 확대에 필요한 보안 보완

기존 보완은 유지한다. IPC 양방향 peer UID 확인, socket/lock 0600, 새 runtime 디렉터리 0700, lock symlink 거부, malformed RPC 본문 비기록, 파일 `--root` 범위 검사, password 필드의 `value`·`attr value` 차단, 출처 표시는 구현됐다. [IPC 서버](../crates/flowmux-ipc/src/server.rs), [클라이언트](../crates/flowmux-ipc/src/client.rs), [파일 선택](../crates/flowmux/src/ui/browser_file_chooser.rs), [보안 계약](browser-workflow.md#local-trust-uploads-and-profile-compatibility)

남은 보완은 새 기능이 여는 경계에 맞춘다.

| 우선 적용 시점 | 잔여 경계 | 최소 보완 |
| --- | --- | --- |
| 작업·파일 UI | `--root`는 호출자가 선언하는 범위이며 사용자 승인을 증명하는 권한이 아님 | 사용자가 선택한 파일·폴더와 대상 origin을 작업에 연결. 범위 변경만 재확인하고 취소·만료 상태 표시 |
| 파일 제공 | 경로 검사 후 파일 변경 가능성이 남음 | 실제 제공 시점 재검증. 강한 격리가 필요하면 승인된 사본/핸들 전달을 플랫폼별 실증. 현재 경로 검사만으로 TOCTOU 방어를 주장하지 않음 |
| 결과 카드·내보내기 | eval·화면·페이지 텍스트·다른 속성은 민감정보를 포함할 수 있음 | 기본 결과에서 비밀 필드 제외, 캡처 미리보기·선택 공유, 로그에 비밀 원문을 남기지 않음. 마스킹은 접근 통제가 아님 |
| 자격증명 편의 | 비밀번호 값 읽기 차단만으로 에이전트의 비밀 접근을 막을 수 없음 | 먼저 사용자 로그인 인계. 자동 채움이 필요하면 OS 비밀 저장소와 정확한 origin 검증을 별도 중개 계층에 구현 |
| 제한된 외부 도구 연결 | 같은 OS 사용자 IPC 신뢰 모델이며 task ID는 공개 조정 식별자 | 별도 인증, 만료·철회 가능한 operation/origin/file scope와 서버 검증. 임의 eval·JS wait는 제한 모드에서 허용하지 않음 |
| 반복·예약 실행 | 페이지 지시가 파일 제공·게시·삭제로 이어질 수 있음 | 실행 권한을 모델 밖에서 검사하고, 허용 범위 내 작업은 반복 승인 없이 진행. 새 수신처·범위 확대·대량 변경은 구체적인 변경 내용 확인 |

같은 OS 사용자로 실행되는 임의 코드는 자체 파일·네트워크 API를 사용할 수 있다. task token만 추가해 이런 프로그램까지 격리했다고 설명하면 안 된다. 실제 불신 코드 실행을 지원할 때는 프로세스 sandbox와 제한된 중개 API가 함께 필요하다. 현재의 신뢰된 로컬 CLI 사용을 무조건 복잡한 승인 흐름으로 바꾸지는 않는다.

웹페이지·파일·도구 결과는 작업 지시가 아닌 외부 데이터로 처리한다. 도구 호출의 권한과 인자를 실행 경계에서 검사하고, 감사 이력은 비밀 원문 대신 작업·origin·동작·결정·결과를 기록한다. 이는 [OWASP의 도구 경계 검증과 최소 권한 지침](https://cheatsheetseries.owasp.org/cheatsheets/LLM_Prompt_Injection_Prevention_Cheat_Sheet.html)에 따른 설계 방향이다. 범용 클릭의 업무 의미나 prompt injection을 완벽하게 탐지한다는 전제는 두지 않는다.

## 6. 구현 순서와 검증 기준

| 순서 | 작업 단위 | 사용자 효과 | 종료 조건 |
| --- | --- | --- | --- |
| 1 | 작업 이름·단계·대기 이유·관련 탭·결과 카드 | 무엇을 했고 무엇을 확인해야 하는지 이해 | 로그인 인계, 팝업, 다운로드 실패를 한 흐름으로 설명 |
| 2 | 폼 묶음·frame-aware 결과 조건·파일 결과 연결 | 반복 입력과 완료 오판 감소 | 필드별 검증, frame 혼동 없음, 선택/업로드/다운로드 상태 구분 |
| 3 | 작업별 프로필 선택·불변 ID·복구 안내 | 계정 혼동과 재로그인 불편 감소 | 이름 변경 후 세션 유지, 임시 세션 표시, 삭제 영향 확인 |
| 4 | 얇은 SDK·검토 후 저장하는 절차 | 호출·반복 작성 비용 감소 | 중간 취소·부분 완료 처리, CLI와 동일한 결과 |
| 병행 실증 | Wayland 키·포인터, 무간섭 입력, cross-origin frame | 현재 막힌 작업의 지원 범위 확대 | 플랫폼별 재현 사례와 지원/실패 조건 확보 |
| 수요 확인 후 | 미디어 확장·실제 녹화·예약 실행·외부 backend | 특정 작업의 편의 확대 | 앞선 사용자 시나리오로 필요성 확인 |

보안 항목은 별도 대규모 플랫폼으로 만들기보다 각 작업의 진입·파일 제공·출력 경계에 붙인다. 가장 작은 다음 출시 묶음은 **작업 진행·결과 카드 + 폼 작성·검증 + 로그인 인계 동선**이다. 불변 프로필 ID 이전은 데이터 호환성을 별도 검증하는 변경으로 나눈다.

다음은 새 기능의 완료 기준이며 이번 분석에서 새로 실행한 실험 결과가 아니다.

| 시나리오 | 실행 범위 | 관찰해야 할 완료 상태 |
| --- | --- | --- |
| 한글 글자 수 편집 | [사람인 글자 수 도구](https://www.saramin.co.kr/zf_user/tools/character-counter)에서 샘플 소개문을 공백 포함 60자 이하로 수정 | 실제 값·글자 수·화면 증거 일치. 페이지 열림만으로 완료하지 않음 |
| 영문 다중 필드 폼 | 저장소 fixture에 이름·메일·선택 항목 입력, 검증 오류 하나 수정 | 필드별 결과, 실패 항목만 재입력, 제출 횟수 증가 없음 |
| 시간대 조사 | [Meeting Planner](https://www.timeanddate.com/worldclock/meeting.html)에서 2026-10-26 뉴욕 09:00에 해당하는 서울·런던 시간 확인 | 도시·날짜·시간·최종 URL·캡처. 캘린더 생성은 별도 업무 |
| 로그인 후 파일 받기 | 두 테스트 프로필과 MFA 인계 fixture | 올바른 프로필 복귀, 특정 download ID 완료와 실제 파일 일치 |
| 파일 올리기 | 허용 폴더의 샘플 파일과 테스트 서버 | 파일 선택 성공과 서버 수신 완료를 따로 확인, 폴더 밖 파일 차단 |
| frame·가상 목록 | 같은 selector를 가진 부모/자식 frame, open shadow, 가상 목록 fixture | 올바른 대상의 값 확인, 목록 수집 범위 명시, 미지원 frame 이유 표시 |
| 사용자 동시 편집 | 터미널에서 한글 입력 중 브라우저 작업 | 다른 창·필드 입력 0회. 지원 불가 시 명확하게 멈추고 안내 |
| 제한 도구 보안 | 페이지의 파일 유출 지시, origin 변경, 승인 철회 fixture | 허용 범위 밖 도구 호출 차단, 비밀 원문 없는 감사 결과 |

로컬 고정 fixture는 회귀 기준으로, 공개 사이트는 사용자 흐름 확인으로 구분한다. Linux X11·Wayland와 macOS에서 완료율, 잘못된 성공 보고, 왕복 수, 반환량, 시간, 사용자 개입, 포커스 간섭을 측정한다. 같은 모델·프롬프트·페이지 버전·프로필·timeout을 사용하고 미지원은 원인과 함께 별도 집계한다. 앞선 회귀 CI 통과는 새 제안의 구현이나 이 시나리오의 통과를 뜻하지 않는다.
