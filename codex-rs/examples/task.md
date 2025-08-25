---
version: "1"
objective: "End-to-end skeleton demo for codex-task"
agents:
  - id: primary
    model: gpt-5
    sandbox: workspace-write
    ask_for_approval: never
  - id: backend
    model: o4-mini
    sandbox: workspace-write
tasks:
  - id: setup
    title: "Initialize project scaffolding"
    agent: primary
    instructions: |
      Create a minimal hello-world program in this directory and print "hello".
    depends_on: []
    retries: 0

  - id: tests
    title: "Add a smoke test"
    agent: backend
    instructions: |
      Add one simple automated test that validates the hello program prints "hello".
    depends_on: ["setup"]
    retries: 0
    timeout_secs: 120
---
