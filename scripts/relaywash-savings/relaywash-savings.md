---
description: Show estimated relaywash savings for the current session
allowed-tools:
  - Bash
---

Run the savings reporter for this session:

```
!`node ${CLAUDE_PLUGIN_ROOT}/bin/wash.mjs savings --session ${CLAUDE_SESSION_ID:-default}`
```
