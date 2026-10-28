<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Online-Mind2Web·BU Bench V1 실행 시도와 파일럿

**후속 진행:** 2026-10-28 14:16 KST에 기존 Codex 로그인으로 BU Bench V1 전체
100개 배치를 시작했다. [전체 실행 기록](browser-agent-bu-v1-run-20261028.ko.md)을 참고한다.
아래는 배치 시작 전의 최초 확인 기록이며, Online-Mind2Web 데이터 인증 제약은 남아 있다.

확인일: 2026-10-28. **Flowmux의 두 벤치마크 전체 성공률은 아직 측정하지 못했다.**
Online-Mind2Web은 원본 데이터 인증에서 막혔고, BU Bench V1은 과제 100개를
확보했지만 대량 실행 모델 설정과 공식 채점 연결이 없다. BU 실제 과제 1개는
현재 Codex 세션에서 수행해 페이지의 성공 상태를 확인했다. 독립 채점은 하지 않았다.

| 대상 | 확보한 과제 | 실행 | 공식 채점 | 전체 성공률 |
| --- | ---: | ---: | ---: | --- |
| Online-Mind2Web | 0 / 300 | 0 | 0 | **미측정** |
| BU Bench V1 | 100 / 100 | 파일럿 1, 나머지 99 미실행 | 0 | **미측정** |

미실행·미채점을 실패로 세거나 성공률 0%로 표기하지 않았다. 결과 JSON의
`success_rate_percent`는 `null`이다. 파일럿 1건으로 전체 점수나 신뢰구간을
추정하지 않는다. 이전 [고정 시나리오 검사](browser-automation-evaluation-186afac29f8c.ko.md)의
100% 통과율도 이 두 벤치마크 점수가 아니다.

## 실제 확인한 내용

- **BU Bench V1 데이터:** 공식 암호화 파일을 배포자의 로딩 방식으로 메모리에서
  읽어 100개 고유 task ID와 5개 범주별 20개를 확인했다. V2.1 데이터는 사용하지 않았다.
  과제 지시문·정답은 저장소 결과물에 포함하지 않았다.
- **Online-Mind2Web 데이터:** Hugging Face 메타데이터는 공개지만 데이터는
  `gated: auto`다. 고정 revision의 실제 JSON 다운로드가 HTTP 401과
  “You must have access to it and be authenticated”를 반환했다.
- **채점 연결:** 현재 프로세스의 `HF_TOKEN`, `OPENAI_API_KEY`, `GOOGLE_API_KEY`,
  `ANTHROPIC_API_KEY`는 설정되지 않았다. 별도 배치 에이전트 모델도 지정되지 않았다.
  현재 대화 세션을 사용할 수 있다는 사실과 별도 실행기·채점 API를 사용할 수
  있다는 사실은 구분했다. 다른 위치의 인증 정보를 탐색하거나 재사용하지 않았다.
- **Flowmux:** 주어진 세션 소켓은 존재하지 않았다. 별도 Xvfb/X11과
  runtime/config/data/state/cache 디렉터리에서 Flowmux를 실행했다.
  WebKitGTK 2.52.6과 native input 준비 상태를 확인했다.
- **실제 파일럿:** `c1f60ee6-506c-4ddb-b4eb-64106930667b`를 실행했다.
  입력·선택·파일 전달·제출 후 페이지 성공 메시지와 스크린샷을 확인했다.
  답안이나 페이지 내부 성공 플래그를 주입하지 않았다. `eval`은 드롭다운 옵션의
  표시 문구·값을 읽는 데 한 번 사용했고, 조작은 Flowmux 명령으로 수행했다.

파일럿은 원본 순서의 첫 InteractionTests 과제를 고른 편의 표본이다. 전체 과제나
범주를 대표하지 않는다. 현재 대화의 판단과 디버깅을 포함하므로 모델·프롬프트를
고정한 자동 실행 결과도 아니다. 관찰·디버깅·최종 캡처를 포함한 CLI 호출은 20회였다.
그중 폼 명령 JSON 형식 오류 1회, 드롭다운을 연 뒤 `Escape` native key 입력의
완료 대기 timeout 1회를 기록했다. 이후 조작으로 과제를 마쳤다. 이 timeout이
반복되는 제품 결함인지는 재현하지 않았으며, 이번 작업에서 제품 코드를 고치지 않았다.

제출 직후 snapshot과 `pilot-0016.png`에 성공 알림이 남았다. 뒤늦게 저장한
`pilot-final-full.png`에서는 일시적인 성공 알림이 사라졌고 입력값만 보인다.
완료 근거는 제출 직후 기록이며, 나중의 전체 화면만으로 성공을 판정하지 않았다.

초기 탐색은 기록기 시작 전에 수행됐다. 병렬로 실행한 읽기 전용 probe 3개는
임시 기록기의 step 번호가 겹친다. JSONL 줄 순서와 실제 스크린샷 연결은 남아 있지만,
이를 검증된 공식 제출 trajectory로 취급하지 않는다. 초기 탐색·모델 추론을 포함한
총 수행 시간이나 모델 토큰·비용은 측정하지 않았다.

## 발표된 점수와 비교할 때의 조건

두 벤치마크는 브라우저 제어 명령 자체의 통과율이 아니라 **모델·에이전트·도구·브라우저·
웹사이트 상태를 합친 과제 완료율**을 측정한다. Flowmux에는 이 전체 조건과 독립적인
고유 성공률이 있는 것이 아니다.

| 공개 근거 | 발표된 값 | 이번에 확인한 범위 |
| --- | --- | --- |
| Aside, Online-Mind2Web | 297/300 = 99.0%; 불가능 과제 제외 297/299 = 99.3% | 공개 README. `gpt-5.5`, high, grader `gpt-5.4` |
| Aside, BU Bench V1 | `gpt-5.5` 93/100 = 93.0%; 불가능 제외 93/99 = 93.9% | 공개 README. 실행·채점 재현은 하지 않음 |
| Browser Use, Online-Mind2Web | Cloud `bu-max` 95.3% | 공식 저장소 차트. 개별 과제 재채점은 하지 않음 |
| Browser Use, BU Bench V1 | Cloud API v4 `bu-v4-opus-4-8` 85/100 = 85.0% | 공식 집계 JSON의 completed/successful 필드 |

출처: [Aside README](https://github.com/at-inc/aside-benchmarks/blob/7cde728f6af4ba10065fc9e0557762d0e156d3dd/README.md),
[Browser Use Online-Mind2Web 차트](https://github.com/browser-use/benchmark/blob/77b4df339fe8fc293fb48896cfa1c73dd743af42/online-mind2web/official_plots/success_rate_light.png),
[BU V1 집계 JSON](https://github.com/browser-use/benchmark/blob/77b4df339fe8fc293fb48896cfa1c73dd743af42/official_results/BrowserUseCloudAPI_v4_browser_integrated_model_bu-v4-opus-4-8.json).
위 값은 공급자 발표값이며 같은 모델·같은 실행일·같은 채점 조건으로 재측정한 순위가 아니다.

특히 [Online-Mind2Web의 WebJudge 안내](https://github.com/OSU-NLP-Group/Online-Mind2Web/blob/b422075b059f0bba670ddda168444eb1d326aa45/README.md#webjudge-update)는
기존 `o4-mini` 대신 다른 채점 모델을 쓰면 과거 결과와 직접 비교할 수 없다고 명시한다.
문서 아래쪽에 남은 과거 모델 권장을 그대로 실행 조건으로 채택하면 안 된다.
또한 README의 WebJudge agreement 수치는 사람 판정과의 일치율이며 에이전트 성공률이 아니다.

BU Bench의 현재 기본 실행은 200개 V2.1이다. 요청한 V1은 100개 과제와 이진 채점기를
사용한다. [현재 V1 채점 경로](https://github.com/browser-use/benchmark/blob/77b4df339fe8fc293fb48896cfa1c73dd743af42/evaluation.py)는
`gemini-2.5-flash`를 기본으로 하고 `GOOGLE_API_KEY`를 요구한다.
기존 runner를 그대로 실행하면 Flowmux가 아니라 다른 브라우저·에이전트를 평가하므로,
Flowmux 명령을 실행하고 결과를 반환하는 어댑터도 필요하다.

## Flowmux에 대해 말할 수 있는 범위

파일럿으로 확인한 것은 실제 외부 과제에서 관찰→입력→파일 전달→제출→성공 상태 확인이
가능하다는 점이다. 범용 탐색·조사·추론 과제의 완료율은 알 수 없다. BU의 InteractionTests는
전체의 20%이고 나머지 80%에는 웹 조사와 추론이 포함된다.

실행 중 `browser diagnose`는 `cross_origin_frames: false`, 호스트 쿠키 삽입 미지원을
보고했다. 인증과 프레임 과제에서 확인해야 할 제약이지만, 관련 과제를 실제로 실행하지
않았으므로 실패율이나 점수 상한으로 환산하지 않는다. X11의 이번 결과를 Wayland나
macOS 결과로 일반화하지 않는다.

## 전체 점수를 얻기 위해 남은 작업

1. [Online-Mind2Web 데이터 페이지](https://huggingface.co/datasets/osunlp/Online-Mind2Web)에서
   접근 동의를 마친 계정의 토큰 또는 승인받아 내려받은 데이터 파일을 제공한다.
2. 대량 실행할 에이전트 모델·reasoning과 채점 모델/API 연결을 정한다.
   API 키 값은 문서·결과물·채팅에 기록하지 않는다.
3. Flowmux용 과제 실행·기록 어댑터를 연결하고, 과제마다 새 프로필을 사용한다.
   모델에는 과제 지시만, 채점기에는 정답·증거를 전달한다. Online-Mind2Web은 지정
   사이트에서 시작하며 사실적인 행동 기록과 단계별 스크린샷을 남긴다.
4. 고정된 300개·100개를 동일한 timeout·재시도 예산으로 실행하고 별도로 채점한다.
   첫 시도 실패, timeout, CAPTCHA, 인증, 실행 오류, 채점 오류와 미실행을 구분한다.
   모든 과제 판정이 갖춰진 뒤 전체 성공률과 범주별 성공률을 계산한다.
5. 경쟁 제품과 제어 기술의 차이를 비교하려면 같은 모델과 채점기로 같은 과제를 다시
   실행한다. 과거 발표값과의 단순 차이를 Flowmux 제어 기술의 효과로 해석하지 않는다.

전체 배치 실행기·공식 제출 변환기 구현과 400개 과제 채점은 아직 완료하지 않았다.
외부 입력 없이 점수를 만드는 대신, 확인된 범위와 막힌 조건을 이 문서에 남긴다.

## 재현 근거

- [상태·환경·HTTP 401 기록](evaluations/browser-agent-preflight-20261028/status.json)
- [BU V1 revision·파일 해시·100개 ID 목록](evaluations/browser-agent-preflight-20261028/bu-v1-manifest.json)
- [파일럿 결과·오류·증거 해시](evaluations/browser-agent-preflight-20261028/pilot-summary.json)

BU 저장소는 `77b4df339fe8fc293fb48896cfa1c73dd743af42`, 암호화 데이터 SHA-256은
`a93f3e5c019cb7853b4b497fcf05f3836a6dd2a7e0f53f038ae8558300bc096e`로 고정했다.
Online-Mind2Web 데이터 메타데이터 revision은 `eacad896a84dc5b65e29b0b06e4699ab0544d701`이다.
실제 내용을 받지 못했으므로 그 데이터의 내용 해시는 없다.

Flowmux는 기존 `target/release/flowmux`와 `flowmuxctl`을 사용하고 SHA-256을 기록했다.
시작 시 checkout에 편집기 변경 사항이 있었고, 작업 중 HEAD가 `6cdad285baef`에서
`42651f543afd`로 바뀌었다. 바이너리 해시는 기록 전후 동일했지만 바이너리 빌드 commit과
checkout commit의 일치는 확인하지 않았다. 이 작업은 기존 편집기 변경 사항을 수정하지 않았다.

원시 CLI 출력·스크린샷은 복호화된 벤치 내용을 포함할 수 있으므로 저장소에 넣지 않고
로컬 `/tmp/flowmux-benchmark-session-20261028/`에 보관했다. 이 경로는 임시 파일이므로
영구 보존을 보장하지 않는다. 파일럿 전체 화면은 같은 경로의 `pilot-final-full.png`다.
