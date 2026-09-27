<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# 한글 입력 48개 사례 — 설정별 원시 입력 비교

[검토 보고서](../../korean-input-audit-2026-09-27.md) · [원시 바이트와 실행 정보](../../korean-input-audit-evidence-2026-09-27.json)

PASS는 해당 단계의 PTY 바이트가 기대값과 일치한다는 뜻이다. FAIL 뒤에는 실제 수신값을 표시한다. focus report는 비교에서 제거하고 원본 JSON에 보존했다. 이 표는 셸의 편집 결과나 후보창 검사 결과가 아니다. 각 사례는 스크립트의 단계 순서에 의존한다.

| 사례 | 기대값 nav on | 기대값 nav off | sync 0 / on | sync 0 / off | sync 1 / on | sync 1 / off |
|---|---|---|---|---|---|---|
| english | `"abc"` | `"abc"` | PASS | PASS | PASS | PASS |
| empty_enter | `"\r"` | `"\r"` | PASS | PASS | PASS | PASS |
| fast_three_enter | `"\r\r\r"` | `"\r\r\r"` | PASS | PASS | PASS | PASS |
| hidden_cursor_preedit | `"가"` | `"가"` | PASS | PASS | PASS | PASS |
| enter_after_preedit | `"나\r"` | `"나\r"` | PASS | PASS | PASS | PASS |
| sentence_enter | `"안녕하세요\r"` | `"안녕하세요\r"` | PASS | PASS | PASS | PASS |
| backspace_composing | `"한\u007f"` | `""` | PASS | PASS | PASS | PASS |
| symbol_after_backspace | `"?"` | `"하?"` | PASS | PASS | PASS | PASS |
| shift_enter | `"가\u001b\r"` | `"가\u001b\r"` | PASS | PASS | PASS | PASS |
| space | `"한글 "` | `"한글 "` | PASS | PASS | FAIL `"한 글"` | FAIL `"한 글"` |
| number | `"아1"` | `"아1"` | PASS | PASS | FAIL `"1아"` | FAIL `"1아"` |
| comma | `"안녕,"` | `"안녕,"` | PASS | PASS | FAIL `"안,녕"` | FAIL `"안,녕"` |
| shift_symbol_question | `"한?"` | `"한?"` | PASS | PASS | PASS | PASS |
| shift_symbol_exclam | `"한!"` | `"한!"` | PASS | PASS | PASS | PASS |
| shift_symbol_at | `"한@"` | `"한@"` | PASS | PASS | PASS | PASS |
| shift_symbol_numbersign | `"한#"` | `"한#"` | PASS | PASS | PASS | PASS |
| shift_symbol_dollar | `"한$"` | `"한$"` | PASS | PASS | PASS | PASS |
| shift_symbol_percent | `"한%"` | `"한%"` | PASS | PASS | PASS | PASS |
| shift_symbol_asciicircum | `"한^"` | `"한^"` | PASS | PASS | PASS | PASS |
| shift_symbol_ampersand | `"한&"` | `"한&"` | PASS | PASS | PASS | PASS |
| shift_symbol_asterisk | `"한*"` | `"한*"` | PASS | PASS | PASS | PASS |
| shift_symbol_parenleft | `"한("` | `"한("` | PASS | PASS | PASS | PASS |
| shift_symbol_parenright | `"한)"` | `"한)"` | PASS | PASS | PASS | PASS |
| shift_symbol_underscore | `"한_"` | `"한_"` | PASS | PASS | PASS | PASS |
| shift_symbol_plus | `"한+"` | `"한+"` | PASS | PASS | PASS | PASS |
| shift_symbol_colon | `"한:"` | `"한:"` | PASS | PASS | PASS | PASS |
| shift_symbol_quotedbl | `"한\""` | `"한\""` | PASS | PASS | PASS | PASS |
| shift_symbol_less | `"한<"` | `"한<"` | PASS | PASS | PASS | PASS |
| shift_symbol_greater | `"한>"` | `"한>"` | PASS | PASS | PASS | PASS |
| shift_symbol_braceleft | `"한{"` | `"한{"` | PASS | PASS | PASS | PASS |
| shift_symbol_braceright | `"한}"` | `"한}"` | PASS | PASS | PASS | PASS |
| shift_symbol_bar | `"한\|"` | `"한\|"` | PASS | PASS | PASS | PASS |
| shift_symbol_asciitilde | `"한~"` | `"한~"` | PASS | PASS | PASS | PASS |
| double_consonants | `"까따빠싸짜\r"` | `"까따빠싸짜\r"` | PASS | PASS | PASS | PASS |
| compound_vowels | `"과되워의\r"` | `"과되워의\r"` | PASS | PASS | PASS | PASS |
| final_cluster | `"값\r"` | `"값\r"` | PASS | PASS | PASS | PASS |
| isolated_jamo | `"ㄱ\r"` | `"ㄱ\r"` | PASS | PASS | PASS | PASS |
| keypad_enter | `"가\r"` | `"가\r"` | PASS | PASS | PASS | PASS |
| hangul_english_toggle | `"한abc글\r"` | `"한abc글\r"` | PASS | PASS | PASS | PASS |
| composing_Tab | `"가\t"` | `"가\t"` | PASS | FAIL `"가"` | PASS | FAIL `"\t가"` |
| composing_Left | `"가\u001b[D"` | `"가\u001b[D"` | PASS | FAIL `"가"` | PASS | FAIL `"\u001b[D가"` |
| composing_Right | `"가\u001b[C"` | `"가\u001b[C"` | PASS | FAIL `"가"` | PASS | FAIL `"\u001b[C가"` |
| composing_Home | `"가\u001b[H"` | `"가\u001b[H"` | PASS | FAIL `"가"` | PASS | FAIL `"\u001b[H가"` |
| composing_End | `"가\u001b[F"` | `"가\u001b[F"` | PASS | FAIL `"가"` | PASS | FAIL `"\u001b[F가"` |
| composing_Delete | `"가\u001b[3~"` | `"가\u001b[3~"` | PASS | FAIL `"가"` | PASS | FAIL `"\u001b[3~가"` |
| shift_left_order | `"가\u001b[D\r"` | `"가\u001b[D\r"` | FAIL `"\u001b[D가\r"` | FAIL `"\u001b[D가\r"` | FAIL `"\u001b[D가\r"` | FAIL `"\u001b[D가\r"` |
| shift_right_order | `"가\u001b[C\r"` | `"가\u001b[C\r"` | FAIL `"\u001b[C가\r"` | FAIL `"\u001b[C가\r"` | FAIL `"\u001b[C가\r"` | FAIL `"\u001b[C가\r"` |
| escape_then_enter | `"가\u001b\r"` | `"가\u001b\r"` | PASS | FAIL `"가\r"` | PASS | FAIL `"\u001b가\r"` |

동기/on 반복 실행은 43 PASS / 5 FAIL로 동일한 불일치를 재현했다. 반복 원본은 JSON의 `repeat-sync1-navon`에 있다.
