# BUG-99 (authoritative)

The emit prefix must be exactly **`OUT:`** (uppercase OUT, colon, no space).

Do **not** use `out>` — that was a mistaken hotfix in an old runbook and will fail CI.

Also write `REPORT.md` at the project root with:
- first line: `BUG-99`
- second line: path of the file you changed (relative)
- third line: `verified`

Do not invent other prefixes.
