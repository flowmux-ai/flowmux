<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Team workspace 프롬프트 사례집

각 사례의 **사용자 프롬프트**를 Team workspace의 오케스트레이터(리드 에이전트)에
입력하세요. 리드가 작업을 나누고, 서브 에이전트의 결과를 확인한 뒤 같은 에이전트에
후속 지시를 보내는 흐름입니다. 사용자가 worker pane마다 직접 입력할 필요는 없습니다.

아래 대화는 **기대 동작을 설명하는 예시이며 실제 모델 대화 기록은 아닙니다.**
현재 검증된 범위는 실제 GUI pane에서 모의 에이전트로 실행한 전달·응답·세션 재사용입니다.
실제 Codex의 승인 화면을 감지하고 원래 리드에게 입력 대기를 알리는 동작도 검증했습니다.
실제 Claude/Codex 모델로 끝까지 실행한 결과는 아직 확보하지 못했습니다.
막힌 원인과 검증 기록은 문서 마지막에 있습니다.

## 시작하기

1. Flowmux의 **New Team Workspace**에서 프로젝트를 열고 리드 에이전트를 실행합니다.
2. **Options → Skills → Flowmux Team**에서 리드가 사용할 스킬을 설치하거나 업데이트합니다.
3. 인증 및 초기 설정을 마친 Claude Code 또는 Codex CLI가 필요합니다.
   아래 프롬프트는 worker로 Codex를 지정합니다. Claude를 쓰려면 프롬프트의
   `Codex`를 `Claude Code`로 바꾸세요. 실제 실행에는 계정 사용량이 발생합니다.
4. 리드는 `team.py context`로 Team 모드를 확인해야 합니다. 일반 workspace에서
   리드 혼자 수행한 결과는 이 사례의 통신 검증 성공으로 세지 않습니다.

새 Codex 리드는 갱신된 Flowmux shim을 통해 `--no-daemon`으로 시작합니다.
바이너리만 업데이트했다면 `flowmux hooks refresh-shims`로 기존 shim을 갱신하거나, 의도한 pane에서
`codex --no-daemon`을 직접 실행하세요. 기존 공유 daemon 세션은 그대로 유지됩니다.
그 세션의 연결 정보가 없다면 재조회만으로 복구되지 않으며, 활성 세션을 강제로
종료하거나 다른 창의 경로·제목으로 위치를 추측하지 않습니다.
Linux/macOS 설치 스크립트는 기존 wrapper를 자동 갱신합니다. 기존 TUI 안에서
새 대화만 시작하면 공유 daemon 연결은 그대로이므로 새 프로세스 실행과 구분하세요.

역할별 worker는 첫 요청에서 한 번 생성합니다. 이어지는 관련 요청에는 원래 job의
`followup`을 사용합니다. 사용자 후속 메시지도 **같은 리드 대화**에 입력하세요.
다른 Team workspace나 리드 pane에서 보낸 `followup`은 입력 전에 거부됩니다.
`wait`가 `waiting_input`(종료 코드 2)을 반환하면 `input_required`에 표시된
worker pane에서 사용자가 승인 또는 입력을 처리한 뒤 같은 job을 다시 기다립니다.
`input-required.json`은 이 기록을 보관하며, 작업의 최종 `blocked` 보고서와 구분됩니다.
리드가 응답을 마친 뒤 스스로 깨어나는 기능은 없으므로, 아래의 두 번째 사용자
프롬프트는 사용자가 실제로 보내야 진행됩니다.

## 사례 1. 조사 → 리뷰 → 사용자 조건 추가 → 수정 → 재리뷰

### 사용자가 처음 보낼 프롬프트

```text
flowmux-team 스킬로 Team workspace에서 아래 작은 조사·리뷰 작업을 진행해줘.
너는 오케스트레이터이고, Codex 조사 담당과 Codex 리뷰 담당을 별도 pane에 한 명씩 띄워줘.

먼저 임시 사례 디렉터리에 service.txt를 만들고 다음 내용을 저장해:
Product: Flowmux
Daily capacity: 25
Region: Seoul

조사 담당에게 이 파일에서 제품명과 일일 수용량을 읽고 출처 경로와 함께 보고하게 해줘.
리뷰 담당에게는 실제 조사 결과 파일과 원본 경로를 전달해서 두 값이 맞는지 확인하게 해줘.
너도 원본과 두 보고서를 확인한 뒤 제품명, 수용량, 검증 결과를 알려줘.
두 worker는 읽기 전용으로 두고, 원래 job과 pane/session 및 보고서 경로를 보관해줘.
이후 내가 조건을 추가하면 같은 worker에게 후속 지시를 보낼 수 있도록 pane을 유지해줘.
각 요청은 최대 300초만 기다리고, 입력이나 권한 문제로 막히면 원인을 보고해줘.
```

### 그다음 같은 리드에게 보낼 프롬프트

```text
지역도 필요해. 방금 조사한 같은 담당자에게 기존 리뷰와 원본을 다시 읽고
제품명·수용량·지역을 모두 포함하도록 수정해달라고 해줘.
그 수정 결과를 기존 리뷰 담당자에게 다시 검증시켜줘.
새 worker를 만들지 말고 기존 job의 followup으로 진행해줘.
너도 최종 값이 Flowmux / 25 / Seoul인지 확인하고,
처음 보고서와 수정 보고서 경로, 담당자별 같은 session을 재사용했는지 알려줘.
이번 수정과 재리뷰는 한 번씩만 진행하고, 미달 항목은 그대로 보고해줘.
```

### 어떤 지시와 응답이 오가는가

아래 `<원본>`, `<조사 결과>` 등의 자리는 리드가 실제 파일 경로로 채웁니다.

| 순서 | 보내는 쪽 → 받는 쪽 | 지시 또는 기대 응답 |
|---|---|---|
| 1 | 리드 → 조사 담당 | “`<원본>`에서 제품명과 수용량을 읽고 출처를 보고해. 파일은 수정하지 마.” |
| 2 | 조사 담당 → 리드 | “Flowmux, 일일 수용량 25. 출처: `<원본>`.” |
| 3 | 리드 → 리뷰 담당 | “`<조사 결과>`를 `<원본>`과 대조해서 제품명과 수용량을 검증해.” |
| 4 | 리뷰 담당 → 리드 | “두 값 모두 원본과 일치함.” |
| 5 | 사용자 → 리드 | 두 번째 프롬프트로 지역 조건 추가 |
| 6 | 리드 → **같은 조사 담당** | “`<리뷰 결과>`를 읽고, `<원본>`을 다시 확인해서 지역까지 포함해 보고해.” |
| 7 | 조사 담당 → 리드 | “Flowmux / 25 / Seoul. 출처: `<원본>`.” |
| 8 | 리드 → **같은 리뷰 담당** | “`<수정 결과>`의 세 값을 `<원본>`과 대조하고 값과 불일치 여부를 보고해.” |
| 9 | 리뷰 담당 → 리드 | “Flowmux / 25 / Seoul 확인. 불일치 없음.” |
| 10 | 리드 → 사용자 | 원본 대조 결과, 최초·수정 보고서 경로, 세션 재사용 근거 보고 |

**완료 기준:** worker 2명, 총 4개 작업 응답. 최종 값에 `25`와 `Seoul`이 있고,
담당자별 pane·surface·session이 유지되어야 합니다. 최초 보고서는 덮어쓰지 않습니다.
리드가 두 보고서를 전달만 하고 원본을 확인하지 않았다면 검증이 끝난 것이 아닙니다.

## 사례 2. 정보 부족 → 사용자에게 질문 → 같은 담당자가 재개

### 사용자가 처음 보낼 프롬프트

```text
flowmux-team으로 예약 잔여 좌석을 계산하는 작은 사례를 진행해줘.
너는 리드이고, Codex 계산 담당 한 명에게 위임해줘.
임시 디렉터리에 capacity.txt를 만들고 다음 내용을 저장해:
Daily capacity: 25
Requested seats: unspecified

담당자에게 파일을 읽고 잔여 좌석을 계산하도록 요청해줘.
신청 좌석 수가 없으면 추측하지 말고 task_status: blocked로 부족한 정보를 보고하게 해줘.
너는 그 보고서를 확인한 뒤 나에게 필요한 숫자를 질문해줘.
답을 받으면 파일을 갱신하고 같은 담당자에게 followup으로 다시 계산시켜줘.
최초 blocked 보고서와 후속 결과를 모두 보관하고 각 요청은 최대 300초만 기다려줘.
```

### 리드가 신청 좌석 수를 물으면 보낼 프롬프트

```text
신청 좌석은 9석이야. 기존 계산 담당자에게 이어서 맡겨줘.
계산 결과와 최초·후속 보고서 경로도 알려줘.
```

### 어떤 지시와 응답이 오가는가

| 순서 | 보내는 쪽 → 받는 쪽 | 지시 또는 기대 응답 |
|---|---|---|
| 1 | 리드 → 계산 담당 | “`<capacity.txt>`를 읽고 잔여 좌석을 계산해. 신청 수가 없으면 blocked로 보고하고 추측하지 마.” |
| 2 | 계산 담당 → 리드 | `blocked`: “신청 좌석 수가 필요함.” |
| 3 | 리드 → 사용자 | “신청 좌석은 몇 석인가요?” |
| 4 | 사용자 → 리드 | “9석.” |
| 5 | 리드 | 파일의 `Requested seats`를 실제 사용자 답인 `9`로 갱신 |
| 6 | 리드 → **같은 계산 담당** | “부족한 값이 `<capacity.txt>`에 추가됐어. 다시 읽고 잔여 좌석과 계산식을 보고해.” |
| 7 | 계산 담당 → 리드 | `completed`: “25 − 9 = 16석.” |
| 8 | 리드 → 사용자 | “잔여 16석”, 계산 확인 결과와 두 보고서 경로 |

**완료 기준:** worker 1명, 총 2개 작업 응답. 첫 응답은 실제 `blocked`, 후속 응답은
`completed`이고 결과는 `16`입니다. 리드가 사용자 답을 받기 전에 9를 임의로 넣으면
이 수동 사례는 실패입니다. 뒤의 자동 실행기는 재현을 위해 사용자 답 9를 미리 공급합니다.
권한 승인 화면에서 막힌 상태는 이 사례의 “정보 부족”과 다릅니다.

## 사례 3. 코드 조사 → 리드가 실패 재현 → 수정 지시 → 리드가 재검증

### 사용자가 보낼 프롬프트

```text
flowmux-team으로 무료 배송 경계값 버그를 고치는 사례를 진행해줘.
너는 리드이고, Codex 구현 담당 한 명을 사용해줘. 기존 프로젝트 파일은 건드리지 말고
임시 사례 디렉터리에서만 작업해줘.

shipping.py에 다음 코드를 만들어:
def free_shipping(total):
    return total > 50

정책은 “50 이상이면 무료 배송”이야. 너는 check_shipping.py에
49 → False, 50 → True, 51 → True를 검사하는 assert 테스트를 만들어줘.
먼저 담당자에게 현재 shipping.py의 동작만 설명하게 하고 아직 수정하지 않도록 해줘.
첫 보고서를 받은 뒤 네가 테스트를 실행해서 50에서 실패하는 실제 출력을 저장해줘.
그 실패 로그와 테스트 경로를 같은 담당자에게 followup으로 전달하고,
shipping.py만 고치게 해줘. 테스트 수정은 허용하지 마.
수정 보고서를 받으면 네가 같은 테스트를 다시 실행하고 테스트 파일도 바뀌지 않았는지 확인해줘.
수정 시도는 한 번, 각 응답 대기는 최대 300초로 제한해줘.
마지막에는 수정 내용, 실패·성공 로그 경로, 세 경계값 결과와 세션 재사용 여부를 알려줘.
```

### 어떤 지시와 응답이 오가는가

| 순서 | 보내는 쪽 → 받는 쪽 | 지시 또는 기대 응답 |
|---|---|---|
| 1 | 리드 → 구현 담당 | “`<shipping.py>`의 현재 동작을 설명해. 아직 수정하지 마.” |
| 2 | 구현 담당 → 리드 | “50을 초과할 때만 True이므로 50은 False.” |
| 3 | 리드 | 테스트 실행 → 50에서 assertion 실패 → `<실패 로그>` 저장 |
| 4 | 리드 → **같은 구현 담당** | “정책은 50 이상이야. `<실패 로그>`와 `<테스트>`를 읽고 `<shipping.py>`만 수정해. 테스트는 수정하지 마.” |
| 5 | 구현 담당 → 리드 | “조건을 `total >= 50`으로 수정함.” |
| 6 | 리드 | 기존 테스트를 직접 재실행하고 테스트 파일 변경 여부 확인 |
| 7 | 리드 → 사용자 | “49=False, 50=True, 51=True 통과”, 실패·성공 로그 및 변경 내용 보고 |

**완료 기준:** worker 1명, 총 2개 작업 응답. 수정 전 50에서 실패한 로그와 수정 후
세 검사에 통과한 로그가 모두 있어야 합니다. 테스트 파일은 그대로여야 합니다.
담당자의 “수정 완료” 응답만으로 리드가 성공을 선언하면 실패입니다.

## 사례 4. 여러 Team workspace에서 원래 리드에게 후속 요청

사례 1의 첫 보고서를 받은 뒤 다른 Team workspace를 열거나 포커스를 옮깁니다.
두 workspace가 같은 프로젝트 경로나 이름을 사용해도 됩니다. 이어서 **처음 요청한
리드 대화**에 다음 프롬프트를 입력하세요.

```text
다른 Team workspace도 열려 있어. 기존 조사 담당자에게 지역을 추가로 확인시켜줘.
원래 job의 followup을 사용하고, 첫 영수증의 socket·workspace·source_pane과
현재 리드의 위치가 일치하는지 확인해줘. 포커스나 프로젝트 경로로 대상을 고르지 마.
기존 worker의 pane·surface·session이 유지됐는지 결과와 함께 알려줘.
응답은 최대 300초만 기다려줘.
```

**완료 기준:** 원래 workspace의 같은 worker가 응답하고 최초 보고서가 보존됩니다.
다른 리드에서 같은 job으로 `followup`을 시도하면 입력 전에 거부되어야 합니다.
이때는 원래 리드 대화로 돌아가세요. 환경 변수나 영수증을 바꿔 우회하지 않습니다.
대상 선택은 포커스와 독립적이지만, 작업 중 화면 포커스가 그대로 유지된다는 뜻은 아닙니다.

## 사례 5. 승인·입력 대기 → 사용자 처리 → 같은 작업 결과 수집

사례 1–3 실행 중 제공자 UI가 승인을 요구할 때 사용하는 흐름입니다. 승인 화면이
나타나는지는 로컬 설정에 따라 다르므로 이를 만들려고 권한이나 신뢰 설정을 바꾸지 마세요.

### 처음 작업에 덧붙일 프롬프트

```text
담당자가 승인이나 입력을 기다리면 대신 처리하지 마.
waiting_input의 사유와 worker pane·surface, 원래 리드 위치를 알려주고
현재 job 또는 turn 경로와 input-required.json을 보관해줘.
이 상태를 작업의 task_status: blocked 보고서로 간주하지 마.
```

사용자가 표시된 worker pane에서 요청 내용을 확인하고 제공자의 UI로 직접 결정합니다.
처리한 뒤 **원래 리드 대화**에 다음 프롬프트를 입력하세요.

```text
담당자 화면의 입력 요청을 처리했어. 보관한 같은 job 또는 turn을 최대 300초 다시 기다려줘.
아직 끝나지 않은 작업에 start나 followup을 보내지 마.
실제 최종 보고서를 확인하고 다음 단계를 진행해줘. 거부된 권한은 우회하지 마.
```

**완료 기준:** `waiting_input`은 종료 코드 2와 `input-required.json`으로 남고,
최종 작업 보고서를 만들거나 덮어쓰지 않습니다. 입력 처리 뒤에는 **같은 요청**을
`wait`로 다시 수집합니다. 계속 입력을 요구하거나 시간이 초과되면 그 상태를 보고합니다.
사례 2라면 이후 실제 `task_status: blocked` 보고서를 받은 다음 신청 좌석 수를 묻고,
사용자의 답을 파일에 반영한 뒤 idle 상태의 같은 worker에게 `followup`을 보냅니다.

`session_verified: false`는 pane의 UI 대기는 관찰했지만 native hook으로 세션을
확인하지 못했다는 뜻입니다. UI 확인까지만 가능하며, 후속 입력에는 정확한 세션 식별이
필요합니다. `observation_error`가 있으면 먼저 기록된 pane과 세션을 진단하세요.
side panel의 `blocked` 표시는 최종 `task_status: blocked` 보고서의 증거가 아닙니다.

## 자동으로 같은 흐름 재현하기

위 프롬프트는 리드 에이전트가 판단하고 조작하는 수동 사례입니다. 아래 실행기는
사례 1–3을 정해진 순서로 보내는 회귀 검증용이며, 자연어 프롬프트를 받은 리드의
판단 능력까지 검증하지는 않습니다. `review`의 지역 추가와 `clarify`의 사용자 답 9는
실행기에 포함되어 있습니다.

Team workspace의 터미널에서 `TEAM_SKILL`을 설치된 스킬의 절대 경로로 지정하세요.
리드 pane을 `team.py context`로 식별할 수 있어야 합니다.

```bash
export TEAM_SKILL=/absolute/path/to/flowmux-team
python3 "$TEAM_SKILL/scripts/team.py" context
python3 "$TEAM_SKILL/scripts/examples.py" --case review --agent codex
python3 "$TEAM_SKILL/scripts/examples.py" --case clarify --agent codex
python3 "$TEAM_SKILL/scripts/examples.py" --case repair --agent codex
```

교차 제공자 리뷰는 `review --agent codex --review-agent claude`로 실행합니다.
기본값은 같은 제공자의 별도 세션 두 개입니다. 실행기는 제공자 자동 대체를 사용하지 않습니다.

출력의 `ARTIFACTS` 디렉터리에서 다음을 확인할 수 있습니다.

| 파일 | 확인할 내용 |
|---|---|
| `request-N.txt` | worker에게 보낸 실제 작업 지시 |
| `request-N-dispatch.log` | job·pane·surface 등 전달 영수증 |
| `request-N-status.log` | 해당 요청의 상태와 결과 경로 |
| `run.json` | 역할별 원래 job, 각 후속 turn, 완료한 검증 목록 |
| `failing-check.txt`, `passing-check.txt` | repair 사례에서 리드가 실행한 실제 테스트 출력 |
| `failure.txt` | 실행 중단 원인(실패한 경우) |

작업별 대기 한도는 `--timeout`으로 지정하며 기본값은 300초입니다.
worker pane은 종료하지 않아 대화를 확인할 수 있습니다. 진행 중에는 worker pane에
직접 입력하지 마세요. 인증·신뢰·권한 화면이나 세션 식별 오류가 있으면 원인을 확인하고
중단합니다. 불확실한 요청을 자동 재전송하거나 승인을 대신 입력하지 않습니다.
`waiting_input`에서도 실행기는 실패로 종료하며 전체 사례를 자동 재개하지 않습니다.
사용자 처리 뒤에는 `run.json`의 해당 요청 영수증에 있는 `job`으로 `team.py wait`를
실행하세요. 실행기를 처음부터 다시 돌리면 새 worker가 생길 수 있습니다.

## Use the continuation contract directly

```bash
# Save the initial start receipt and its original job path as WORKER_JOB.
python3 "$TEAM_SKILL/scripts/team.py" followup "$WORKER_JOB" \
  --task-file /absolute/followup.txt > /absolute/followup-receipt.json
# Read the returned .job, then pass that exact turn directory:
python3 "$TEAM_SKILL/scripts/team.py" status /worker/job/turns/0001
python3 "$TEAM_SKILL/scripts/team.py" wait /worker/job/turns/0001 --timeout 60
```

The lead's follow-up should name the changed requirement, actual prior evidence,
allowed edits, and acceptance checks. Keep it short. A result's `completed`
status means the worker finished that assignment; the lead still checks it.

| Observation | Lead action |
|---|---|
| Initial `context`: `No unique live Flowmux pane` | Retry `context` once in a separate tool call so the first call's lifecycle hook can report the session. If unresolved, retain both errors and stop; never redirect to a guessed window. |
| `dispatch_unknown` | Receipt is not proven yet; inspect/wait on the same turn. Do not resend. |
| `running`, `received: true` | The user prompt is in the pinned transcript; continue bounded waiting. |
| `completed` | Inspect evidence and acceptance checks; revise or finish. |
| Task report `blocked` | Supply actual missing information, then continue when idle. |
| `waiting_input`, exit 2 | Retain `input-required.json`, report the recorded worker pane and reason, and ask the user to handle the provider UI. Then wait on the same job/turn; do not dispatch another task. |
| `session_verified: false` | UI observation only; establish the exact live session binding before follow-up input. |
| `observation_error` / UI busy / missing session | Diagnose the recorded pane and session; do not type or fabricate state. |
| Follow-up from a different lead/workspace | No input is sent. Return to the original lead; do not rewrite its origin. |
| Wait exits 124 | Deadline elapsed, task not cancelled. Keep evidence and decide whether to wait more. |
| Malformed report / changed transcript | Stop automated handoff; inspect the retained answer and session. |

`followup` rejects concurrent dispatch and unresolved earlier assignments.
It preserves uncertain delivery rather than risking a duplicate. It does not
queue into a busy TUI, interrupt a running task, replace a failed session, or
restart the lead after the lead has ended its own turn.

## Reproduce verification from the source checkout

```bash
python3 scripts/test-agent-team.py
cargo build -p flowmux -p flowmux-cli
cargo test -p flowmux-cli --lib codex_shim_first_tool_gui -- --ignored --nocapture
python3 scripts/test-team-pingpong-gui.py
# Explicit live-account checks, same examples and real provider CLIs:
python3 scripts/test-team-pingpong-gui.py --real-agent claude
python3 scripts/test-team-pingpong-gui.py --real-agent codex
# Verify a provider approval/input handoff without approving it:
python3 scripts/test-team-pingpong-gui.py --real-agent codex --cases clarify --expect-input --timeout 300
```

The GUI harness uses a separate Xvfb display, D-Bus and XDG state. It retains
terminal screens, workspace trees and logs and closes only its test-owned GUI.
Default provider fixtures validate deterministic transport and state handling in
real panes, not model quality or account access. The `--real-agent` runs validate
native CLI conversations and content checks; report those results separately.
`--expect-input` requires a real provider input/approval wait and verifies only that
handoff. It never approves the request and fails if the task completes without one.
Linux requires the Xvfb/python3-xlib dependencies of the existing GUI harness.

## Verification record — 2026-10-10, Linux

| Check | Observed result |
|---|---|
| Helper regression tests | 37 passed, including original-lead enforcement, input handoff without terminal input, missing/mismatched identity, repeated follow-ups and immutable results. |
| Isolated GUI + deterministic providers | All 6 runs passed: 3 cases × Claude/Codex transcript formats, 16 total assignment turns. Additional Team workspaces did not redirect creation or follow-ups; each provider's approval fixture returned its recorded pane to the lead. |
| Skill bundle | Rust installation tests and the live Settings install/update/remove and session restoration suite passed with all five resources. |
| Native Claude Code 2.1.296 / Haiku | Attempted clarification case; stopped at initial theme/onboarding UI before producing a task result. Native ping-pong remains unverified. |
| Native Codex 0.162.1 / GPT-6 Luna | Approval handoff passed: after sandbox startup failed, the actual permission dialog appeared, the sidebar showed blocked and `wait` returned `waiting_input` with the lead/worker IDs. No approval was supplied; native ping-pong remains unverified. |
| Missing socket and title jobs | A closed window's socket was removed; exact-session lookup recovered the current pane after its hook. Ephemeral title completions were ignored, including legacy notify from dedicated TUIs. |
| New local lead, before any session hook | The generated Codex shim and live GUI terminal ownership lookup passed first-tool discovery in two same-directory Team workspaces, independent of focus. The test initially failed because agent presence had not yet been polled; direct terminal PID lookup fixed it. |
| Native Codex first-tool probe after that repair | The local TUI started, but its read-only shell tool failed with `bwrap: loopback: Failed RTM_NEWADDR: Operation not permitted` before running `team.py context`. No approval or sandbox relaxation was supplied; this native first-tool check remains blocked. |

Earlier native runs timed out; the updated helper hands off a detected input wait
immediately. No sandbox or trust setting was relaxed. Resolve native onboarding/sandbox prerequisites in the
user's environment before claiming end-to-end model validation. Mock-provider
passes establish the transport/state contract, not model behavior.

Local evidence retained by this run (temporary paths, not distributed assets):
`/tmp/fm-gui-wi8o527f` contains the six passing GUI logs, trees and screenshots;
`/tmp/fm-gui-i0t0jqep` contains the bundle/lifecycle checks;
`/tmp/fm-gui-hoqe2qdd` contains the socket/title regression;
`/tmp/fm-gui-2m3qyu7e` contains the actual Codex approval screen and handoff record.
The later first-tool regression failed in `/tmp/fm-gui-qtw25u0e` and passed after
repair in `/tmp/fm-gui-ayhe32gl`, including shared-daemon rejection on the final
PID-hardened build; `/tmp/fm-gui-muu000tw` records the native sandbox blocker.
The structural review used a real Codex worker with three follow-ups in session
`01a1263d-29a5-7da3-8e35-4034d423bf3d`; all four reports were retained in
`~/.local/state/flowmux/team-jobs/job-a2k63gum`. This confirms delegation from an
already-bound lead, not native first-tool bootstrap or completion of cases 1–3.

## Verification record — 2026-10-11, Linux

Session `01a12658-9502-72b3-ac0b-359dcd56b3a9` failed both source lookups before
worker creation. The binary had been updated, but both installed Codex wrappers
still lacked `--no-daemon`. The restored TUI continued using the shared daemon.
The final lifecycle hook later established an exact binding; that does not prove
first-tool discovery worked.

The installer now refreshes existing managed and legacy wrappers without touching
provider settings. Auto-resume repairs wrapper precedence after login startup;
the session panel preserves that precedence when restoring the provider's PATH.
The isolated live GUI check in `/tmp/fm-gui-3ncg9drf` passed wrapper upgrade,
first-tool lookup, shared-daemon rejection and automatic restore with reordered
PATH. Native new-lead model execution remains subject to the sandbox blocker
recorded above; fixture success is not full native cookbook success.
