---
version: "1"
objective: "<State the end goal in one sentence. The planner will decompose this into executable tasks.>"
agents:
  - id: primary
    model: gpt-5
    sandbox: workspace-write
    ask_for_approval: on-request
---

Title: <Human‑readable title>

Summary
- Describe the problem and desired outcome in 3–6 bullets.
- Call out users impacted and the expected UX/devex changes.

Explicit Scope
- List what is in scope for this effort (features, views, modules, services).
- Keep scope intentionally small and shippable.

Out of Scope
- List what will not be addressed in this iteration.

Where To Implement
- Frontend: <files/dirs or “N/A”>
- Backend: <files/dirs or “N/A”>
- Infra/Docs/CI: <files/dirs or “N/A”>

Permissions and Visibility
- Who can use this? What roles/flags gate access? Where should UI elements appear?

Data Mapping / Business Rules
- Define any field‑level mappings, validations, or calculations the feature requires.

API or Service Changes (if applicable)
- Endpoints to add/change, inputs/outputs, error codes.

Configuration
- Keys or settings to add; where they live; defaults.

Validation and Errors
- List expected client/server validation and the user‑facing error messages.

Testing Checklist
- Unit, integration, and any snapshot/UI tests the change needs.

Acceptance Criteria
- Bullet list of objective, testable outcomes that prove the work is complete.

Exact Files To Edit (expand as needed)
- <path/to/key/file1>
- <path/to/key/file2>

Coding Notes
- Keep changes minimal and consistent with existing patterns.
- Return clear errors; log with existing mechanisms.
- Avoid scope creep; prefer follow‑ups for extras.

Instructions to the agent generating task.md
- Read this file and produce a new `task.md` that the Codex planner can execute.
- Use exactly one agent in the front‑matter (`primary`) to trigger planning:
  - version: "1"
  - objective: restated concisely
  - agents: a single `primary` agent with the same properties as above
- Then, create a DAG of concrete tasks under a `tasks:` key where each task has:
  - id, title, agent (always `primary`), instructions, cwd, depends_on
  - clear deliverables and success checks; include tests/docs where appropriate
- Order tasks so they can run concurrently when possible; use `depends_on` to capture sequencing.
- Output only the final `task.md` content. Do not include commentary or this template.
