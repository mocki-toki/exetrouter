Inspect the complete changed-file inventory and relevant current source. The
inventory compares exact source snapshots, not a merge base; release branches
may legitimately diverge. Blob IDs and modes identify changes, not their meaning.
Read both before/after versions of potentially relevant files using read_file;
added/removed files exist on only one side. No inline patches are supplied.
Trace potentially relevant changes to the corresponding ExetRouter implementation
and tests. Read additional files through the controller tools as needed.
Do not edit. Return no_change, docs_only, code_change or needs_human. For each
finding provide source citations, affected behavior, concrete failure scenario,
minimal fix and meaningful regression test. Use no_change only after complete
relevant coverage. Missing evidence must result in needs_human, not speculation.
