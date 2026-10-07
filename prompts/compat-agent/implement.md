Implement only the confirmed findings, using the smallest maintainable patch.
Read existing modules and tests before editing. Add a regression test for changed
behavior. Use run_check to demonstrate red before the production fix and green
after it where applicable. Documentation-only changes do not require artificial
tests. Do not remove existing tests or weaken assertions to obtain green checks.
Use only controller tools. Do not modify automation, policy, release versions,
dependencies, schema migrations, installation or deployment. If the fix requires
such changes, return needs_human. Never run live tests or deploy software.
Finish with ready and an English PR title/body explaining the problem, pinned
upstream evidence, solution, actual validation, risks and unverified limitations.
