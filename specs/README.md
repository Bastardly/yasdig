# Specs

What a feature is *for* and what "good" looks like from the person using it. Written
from experience, read by both of us.

The three other kinds of writing in this repo each have a different job:

| where | says | for |
|-------|------|-----|
| `docs/` | what the app does and how to use it | users |
| `.claude/rules/` | where the code bites | Claude, while coding |
| `plan.md` | the current batch of work, thrown away after | both, briefly |
| **`specs/`** | **what should happen and why, with examples** | **both, for good** |

Rules of thumb:

- **Intent, not mechanism.** No function names and no file paths. If a sentence would
  go stale after a refactor, it belongs in `.claude/rules/`.
- **Examples carry the weight.** "I said X, and expected Y" is worth more than a
  paragraph of principle.
- **Only the fuzzy parts.** A spec is for behaviour that can't be pinned by a test
  assertion: mostly the Assistant, where "right" is a judgement call.
- **The person owns the content.** Claude drafts, and marks what it inferred:
  - ✅ said or confirmed by the person
  - ❓ Claude's reading of the code: true today, but nobody has said it's *wanted*
- **When the code and a spec disagree, Claude says so** and fixes neither silently.
