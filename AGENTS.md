# Working instructions

## Reuse procedures and retain operational learning

- For matrix runs and their client/preflight checks, start with
  `eval/MATRIX-RUNBOOK.md` and the referenced harness. It is the definitive guide;
  historical campaign scripts are evidence, not alternative launch procedures.
- Reuse a verified procedure when its relevant assumptions still hold. Do not
  rediscover unchanged setup. Consult official documentation for an actual gap,
  changed version/behavior, explicit verification request, or required lookup.
- Whenever an OpenAI documentation lookup is needed to do something, update the
  relevant existing playbook before finishing: record the gap, verified recipe,
  gotchas, source, tested version and evidence. If the lookup only confirms the
  existing procedure, record that briefly rather than adding another guide.
  For other workflows, create a focused playbook only when none already exists.
- Keep run-specific paths and logs in the run record. Do not change global
  configuration or launch extra paid attempts just to validate documentation.

## Git history safety

Use new commits and normal pushes. Never amend a pushed commit, force-push, or
rebase a published branch without the user's explicit request for that rewrite.
