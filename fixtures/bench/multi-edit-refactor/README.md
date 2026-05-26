# multi-edit-refactor (scaffold)

**Status:** scaffold-only. Implementation lands in a follow-up to issue #43.

**Intent:** locate every callsite of a symbol, then drive a sequence of Edit
calls that change the signature and update each callsite. Exercises:

- Search → list of callsites.
- Read on each file in `signatures` mode to confirm context.
- Sequence of Edit calls (relaywash__Edit once the tool ships).

Reuses `fixtures/corpus/multi-edit-refactor/` (whose manifest already lists
`computeTotal` as the refactor target).
