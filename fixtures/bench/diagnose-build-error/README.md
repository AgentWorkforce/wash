# diagnose-build-error (scaffold)

**Status:** scaffold-only. Implementation lands in a follow-up to issue #43.

**Intent:** point the harness at a captured failing build log (TypeScript or
Rust) and verify that:

- `relaywash__Build` (when implemented) returns the offending file/line.
- A follow-up `relaywash__Read` lands on the right line range.
- The total output bytes stay well under the raw log size (collapsed view).

To implement: drop a `build.log` fixture into this directory, add a `corpus`
pointing at the source it references, and encode the expected steps.
