<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# 코드·성능 감사 결과 — 2026-09-07

기준은 `fe25e62`이다. 호출 경로가 없는 보조 함수 10개, 미사용 직접 의존성
선언 15개를 제거하고 편집기의 본문 복사·경로 처리·검색 위치 계산을 개선했다.
저장 주기, 이벤트 순서, IPC 형식, 저장 스키마는 변경하지 않았다.

## 구조와 조사 범위

현재 workspace는 15개 Rust crate로 구성된다. Rust 파일은 151개이며 테스트를
포함해 123,951줄이다. 아래 경계별로 호출처, 반복 처리, 의존성과 미사용 후보를
조사했다. 모든 플랫폼의 모든 실행 경로를 동적으로 검사했다는 뜻은 아니다.

| 영역 | 역할과 주요 경로 | 조사 결과 |
|---|---|---|
| `flowmux` | GTK/VTE/WebKit UI, 내장 daemon, Tokio→GTK 명령 전달 | `window/polling.rs`에서 편집기 스냅샷을 250ms마다 수집. 여기서 호출하는 editor API의 비용을 줄임 |
| `flowmux-core` | workspace/pane/tab 모델, 상태 집계 | 사용하지 않는 생성·가변 접근 보조 함수 제거. 실제 집계·트리 변이는 유지 |
| `flowmux-daemon`, `flowmux-state` | 상태 변경, 수명주기, 지연 저장, 여러 창의 상태 병합 | 사용하지 않는 wrapper 제거. 현재 사용 중인 소유권 기반 저장과 PID 상관 검사는 유지 |
| `flowmux-cli`, `flowmux-ipc` | 명령 해석, Unix socket 요청·응답 | 프로토콜·명령은 유지. IPC의 미사용 의존성 제거 |
| `flowmux-editor` | 문서 I/O, 버전 검사, 복구, 검색, WebView protocol | 반복 본문 복사, 파일별 불필요한 canonicalize, 정렬 중 PathBuf 복사, 검색마다 같은 prefix 재계산 발견·개선 |
| `flowmux-terminal`, `flowmux-procmon` | PTY/input mode, 프로세스 트리 | 살아 있는 fallback과 탐색 한도를 유지. 미사용 의존성 제거 |
| `flowmux-browser`, `flowmux-cookies` | 브라우저 refs/profile/bookmarks, 쿠키 가져오기 | refs의 수명과 플랫폼 fallback 유지. cookies의 미사용 의존성 제거 |
| `flowmux-config`, `flowmux-vcs`, `flowmux-notify`, `flowmux-md-viewer` | 설정, Git/worktree, 알림, Markdown 렌더링 | 호출되지 않는 보조 함수와 직접 의존성 정리 |

## 작업 단위별 커밋

| 커밋 | 변경 |
|---|---|
| `2cc7c2c` | 문서 메타데이터를 빌려 읽어 본문 복사 제거 |
| `116a14e` | 호출처 없는 보조 함수 10개 제거 |
| `38577ca` | 파일 검색의 불필요한 경로 조회와 정렬 중 복사 제거 |
| `09b308f` | 미사용 직접 의존성 선언 15개와 lockfile의 대응 연결 제거 |
| `f2157c3` | 검색 결과 UTF-16 열 위치를 누적 계산 |

테스트 블록 이전의 Rust 코드만 비교하면 **40줄 추가, 102줄 삭제, 순 62줄 감소**다.
테스트·벤치마크와 이 보고서는 이 수치에 포함하지 않았다. 전체 diff의 줄 수가
줄어드는 것보다 동작 보존과 개선 효과를 재검사할 수 있게 하는 데 우선순위를 뒀다.

## 제거한 코드와 유지한 동작

제거 전 Rust 소스와 저장소 전체에서 이름·참조를 검색했다. 아래 함수는 선언 이외의
호출이 없었다. 해당 crate는 모두 `publish = false`인 프로젝트 내부 crate다.

| 제거한 함수 | 현재 동작을 담당하는 경로/유지한 데이터 |
|---|---|
| `BrowserEngine::builtin_order` | options dialog의 `engine_options` |
| `Options::with_system_notifications_enabled` | 실제 설정 필드, 설정 UI와 저장·읽기 |
| `paths::ghostty_config_path` | 현재 theme/config 로더의 경로 처리 |
| `HtmlDocument::body_contains` | 실제 `html` 데이터와 렌더러 |
| `flowmux_state::save_window` | daemon이 사용하는 `save_window_owned` |
| `StateStore::replace_listening_ports` | `listening_ports` 필드·직렬화·UI 표시는 유지 |
| `StateStore::clear_dead_agent_activity` | 현재 사용 중인 `clear_dead_agent_presence`와 상관 검사 |
| `StateStore::workspace_agent_blocks` | UI가 직접 호출하는 `Workspace::collect_agent_blocks` |
| `PaneContent::tabbed_editor` | 실제 생성 경로의 `PaneSurface::editor` |
| `PaneContent::active_surface_mut` | 실제 트리 접근·변이 메서드 |

직접 의존성 제거: procmon의 anyhow/tracing, IPC의 config/thiserror,
terminal의 thiserror, state의 anyhow/tracing, GUI의 nix, cookies와 VCS의 anyhow,
notify의 anyhow/thiserror/tokio/tracing/serde.
Cargo metadata 비교에서 **패키지 목록과 활성 feature 목록은 동일**하고 의존성 연결만
**1,516 → 1,501**로 감소했다. 사용하지 않던 코드·연결 제거를 런타임 가속이나
바이너리 크기 감소로 환산하지 않았다.

## 성능 측정

동일 머신의 Cargo `fast` 최적화 profile로 측정했다. fixture 생성·문서 열기는
측정 시간에서 제외했다. 표는 각 7회 측정의 중앙값이며 앱 전체 벤치마크가 아니다.

| 작업과 입력 | 개선 전 | 개선 후 | 해석 |
|---|---:|---:|---|
| 메타데이터 200묶음, 1KiB 문서 8개 | 0.402ms | 0.0848ms | 약 79% 단축 |
| 메타데이터 200묶음, 2MiB 문서 8개 | 309.503ms | 0.0907ms | 본문 크기에 비례하던 복사 제거 |
| 파일 1,000개에서 없는 문자열 검색 20회, 열린 문서 없음 | 70.058ms | 52.523ms | 약 25% 단축; 덮어쓸 열린 문서가 없을 때 파일별 canonicalize 생략 |
| 한글·이모지 포함 703KB 한 줄, 500개 매치, 검색 10회 | 529.016ms | 5.762ms | 약 92배; 결과마다 prefix를 다시 순회하던 비용 제거 |
| 파일 목록 1,000개 인덱싱 20회 | 16.976ms | 18.377ms | 전체 인덱싱 속도 개선은 확인되지 않음 |

메타데이터 한 묶음은 `session_snapshot`, `dirty_document_paths`, 메시지 버전 검사다.
기존에는 2MiB 문서 8개에서 한 묶음마다 18회의 본문 복사, 합계 36MiB가 발생했다.
수정 후 이 세 작업의 본문 복사는 0회이며 경로·뷰 상태만 반환한다. 예를 들어
같은 문서 구성에서 250ms 주기 `session_snapshot`만으로 생기던 72MiB/s의 본문
복사도 사라진다. 문서를 실제 열거나 전송·저장할 때 필요한 복사는 유지했다.

검색 위치 계산은 매치 수 M과 줄 길이 L에 대해 반복 prefix 순회의 최악
O(M×L) 비용을 누적 순회로 줄였다. 매치는 앞으로만 진행하므로 Unicode 경계와
빈 매치에서도 같은 열 위치를 얻는다. 기존 u32 포화 동작도 보존했다.

정렬에서는 comparator의 PathBuf 복사를 제거했다. 별도 동일 입력 정렬 측정
(1,000개 경로×100회, 입력 복사 포함, 15회 중앙값)은 12.398 → 10.918ms였다.
그러나 위 전체 파일 인덱싱에서는 개선되지 않았으므로 전체 빠른 파일 열기가
빨라졌다고 주장하지 않는다. 파일 시스템·캐시·동시 부하의 영향을 받는 측정이다.

재실행 명령:

```sh
rtk cargo test --profile fast -p flowmux-editor benchmark_ -- --ignored --nocapture --test-threads=1
```

## 회귀 검증

- 기존 전체 테스트: 1,639 passed, 0 failed. 이 측정은 기능 변경 전 코드에
  수동 메타데이터 벤치마크만 추가한 상태로, ignored 5개를 포함한다.
- 편집기 테스트: 85 passed, 0 failed, 수동 성능 측정 3개 ignored.
- 편집기 웹 테스트: 34 passed, 0 failed.
- 새 검사는 깊이→경로 정렬·제한·취소, symlink를 통한 열린 버퍼 우선 검색,
  다중 Unicode 매치·빈 정규식 매치·CRLF·결과 제한의 위치 동등성을 포함한다.
- 최종 workspace 테스트, clippy, 빌드, 라이브 검증 결과는 아래 완료 기록에 기재한다.

전체 Rust 테스트는 `GDK_BACKEND=x11 GTK_A11Y=none xvfb-run -a dbus-run-session --`
아래서 실행했다. 처음 backend를 지정하지 않은 실행에서는 변경 전부터
`shortcut_close_restores_focus_to_the_previous_widget` 테스트가 실패했다.
X11을 명시한 전체 기준 실행은 통과했으며, 테스트를 삭제하거나 실패를 무시하지 않았다.

## 변경하지 않은 후보와 이유

- `unreachable!`는 대부분 enum 명령 라우팅의 불변식 검사다. 호출되지 않는
  기능 코드로 간주해 제거하지 않았다.
- macOS/stub 구현과 ThorVG C enum의 미사용 variant는 플랫폼·ABI 계약에 필요하다.
- 프로세스 탐색의 루트 자식 목록 중복 조회는 후속 검토 후보다. 살아 있는
  프로세스 트리의 관측 시점과 fallback을 건드리는 변경은 이번에 하지 않았다.
- editor의 주기 자체를 줄이거나 dirty 캐시를 새로 넣지 않았다. 이벤트 누락·저장
  지연 가능성 없이 같은 호출 결과의 비용을 먼저 줄였다.
- pane 트리 중복 탐색과 agent 행 정렬의 MRU 재탐색도 남아 있다. 작은 트리에서의
  비용과 이벤트 순서 영향을 추가로 측정하기 전에는 구조를 교체하지 않았다.

## 완료 기록

검증 산출물은 `/tmp/flowmux-perf-audit-20260907/`에 보관했다.
기존 사용자 창 PID `3484653`과 해당 socket은 보존하고, 테스트에는
Xephyr `DISPLAY=:97`의 별도 창, 별도 D-Bus session 및 config/state/data/cache/runtime
디렉터리를 사용했다. 설치된 바이너리는 교체하지 않았다.

최종 검증:

| 검사 | 결과 |
|---|---|
| `cargo test --workspace --locked --no-fail-fast` (X11/Xvfb/D-Bus 격리) | **1,642 passed, 0 failed, 7 ignored** |
| `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings` | 통과 |
| `cargo fmt --all -- --check`, `git diff --check` | 통과 |
| `cargo build --workspace --locked` | 통과 |
| `npm test` (`editor/flowmux-editor-web`) | **34 passed, 0 failed** |
| 세 가지 수동 최적화 profile 벤치마크 | 통과 |

라이브 검증은 실제 GTK/VTE/WebKit 창과 IPC를 함께 사용했다.

1. 파일 열기→수정 후 `Unsaved` 표시, 저장 전 디스크 미변경 확인.
2. workspace 검색이 미저장 버퍼의 `live needle`까지 포함해 정확히 3개 결과를
   표시함을 확인. 디스크의 구버전 중복 결과는 없었다.
3. 편집기에 초점을 둔 Ctrl+S 뒤 파일의 모든 바이트를 기대 문자열과 비교했다.
4. 빠른 파일 열기에서 `a.txt`, `nested/b.txt`의 순서와 두 번째 파일 열기를 확인했다.
5. 검증 창의 정상 종료·재시작 후 파일 순서, 활성 파일, 커서·스크롤·줌 상태를
   저장된 기대값과 비교했다.
6. 최종 검색 코드로 `앞🙂needle 앞🙂needle`의 두 번째 매치를 클릭했다.
   검색 미리보기의 강조와 실제 문서 이동을 확인하고, UTF-16 시작 13 + 길이 6에
   해당하는 선택 끝 열 **19**가 session에 반영되는 것을 검사했다.
7. 터미널 IPC 입력·화면 읽기에서 `FLOWMUX_VERIFY_TERMINAL` 실행 결과를 확인했다.
8. 새 browser pane에서 example.com URL/ready-state 대기를 모두 통과하고,
   제목 `Example Domain`, interactive snapshot과 refs를 확인했다.
9. 원래 사용자 창은 PID·시작 시각·socket이 그대로이며 마지막 `ping`도 `pong`으로
   응답했다. 검증 도중 닫은 창은 모두 별도 검증 프로세스의 창이다.

이 검증 범위에서 회귀는 발견되지 않았다. Linux의 실제 실행 검증이며 macOS의
라이브 실행은 이 환경에서 수행하지 못했다. 모든 가능한 입력·타이밍·플랫폼에서의
무결함을 증명한 것으로 해석하지 않는다. 원래 존재하던 `.memsearch/`, `a.out`은
수정·커밋하지 않았다.
