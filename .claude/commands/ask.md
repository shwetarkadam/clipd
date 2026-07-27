---
description: Ask a grounded question about your clipboard history
argument-hint: [question]
allowed-tools: Bash(clipd ask:*)
---

Answer the user's question about their clipboard history using clipd's RAG engine.

Run:

```bash
clipd ask --json "$ARGUMENTS"
```

Then report the result to the user:

- Lead with the `answer` field, in your own formatting.
- List the `sources` — each has a `clip_id`, `preview`, `source_app`, and
  `matched_by` (which retrievers found it). Present these as the evidence.
- State the `confidence` value plainly. `high` means the cited clips were found
  independently by more than one retriever; `low` means the answer cited
  nothing checkable; `none` means clipd found nothing relevant.
- If `withheld_count` is above zero, tell the user that many clips were held
  back because they contain detected secrets and never reached the model.
- If `invalid_citations` is non-empty, say so — the model tried to cite clip
  ids that were not in its context, and clipd stripped them.

If `retrieval_only` is `true`, there is no API key configured. Do not present a
synthesized answer: show the ranked clips from `retrieved` and tell the user to
set `api_key` in `transform.json` for written answers — on macOS that is
`~/Library/Application Support/clipd/transform.json`, on Linux
`~/.local/share/clipd/transform.json`.

Do not add information from your own knowledge. Everything you report must come
from the command's output — clipd's whole guarantee here is that answers are
grounded in clips the user actually copied.
