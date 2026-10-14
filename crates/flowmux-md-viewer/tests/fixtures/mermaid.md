<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Mermaid 미리보기

```mermaid
%%{init: {"securityLevel": "loose"}}%%
flowchart LR
    A[시작] --> B[완료]
    style A fill:#12ab34
    click A "javascript:document.documentElement.dataset.executed='diagram'"
```

```mermaid
this is not a diagram
```

```mermaid
sequenceDiagram
    Alice->>Bob: 안녕하세요
    Bob-->>Alice: 반갑습니다
```

```mermaid
stateDiagram-v2
    [*] --> Ready
    Ready --> Done
    Done --> [*]
```

```mermaid
flowchart LR
    A["$$x^2$$"] --> B[Math]
```

```rust
let value = "<tag>";
```

<script>document.documentElement.dataset.executed = 'script'</script>
<img src="missing.png" onerror="document.documentElement.dataset.executed='image'">
<a id="script-link" href="javascript:document.documentElement.dataset.executed='link'">Unsafe link</a>
<button id="event-button" onclick="document.documentElement.dataset.executed='event'">Unsafe event</button>
