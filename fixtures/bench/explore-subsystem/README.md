# explore-subsystem (scaffold)

**Status:** scaffold-only. Implementation lands in a follow-up to issue #43.

**Intent:** drive a sequence of Search + Read calls that walks through a small
subsystem (3–5 related files) the way an agent would when asked "explain how X
works in this codebase". Should exercise:

- Search by symbol, then by callsite regex.
- Read in `signatures` mode on each hit file.
- Optional follow-up `range` reads into the most relevant body.

Pick a slice of `fixtures/corpus/large-codebase/` (the `BillingEngine` +
`Logger` + `tax` triangle is a good candidate) and encode the steps in
`expectations.json`. See `find-and-read/expectations.json` for the file shape.
