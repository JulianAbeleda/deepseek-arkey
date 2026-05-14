# Coding Principles

These rules are for writing and shaping project code.

They are not optional style preferences. They are build rules.

## Core Rule

Project code should favor:

- centralization of authority
- modularization of execution
- abstraction for simplicity
- orthogonality for independence

These four principles are meant to coexist.

Centralize what defines the system.

Modularize what carries the system out.

Abstract what should stay simple at the interface.

Keep independent concerns orthogonal so change stays local.

## Centralize

Keep one clear authority point for:

- environment and config access
- schemas and durable data shape
- integration boundaries to external services
- routing rules and system policy
- state definitions that other modules depend on

Do not duplicate load-bearing logic across scripts, modules, or interfaces.

If multiple parts of the system need the same rule, move that rule to one explicit source of truth.

## Human-Facing And Machine-Enforced

When a rule is useful to a human and mechanically enforceable, prefer both.

The human-facing layer should explain the rule in plain language.

The machine-facing layer should check the rule automatically when the enforcement cost is reasonable.

Examples:

- a task can be readable in a task-state file and still carry `owner`, `next-review`, and `criterion`
- a module contract can document a test and the test can be run by the project's test runner
- a commit message rule can live in the coding standard and also be checked by a project-specific commit checker

Do not leave load-bearing rules as decorative policy when a small script can enforce them.

Do not over-enforce rules whose judgment cost is higher than the drift they prevent.

## Modularize

Break execution into bounded parts for:

- modules
- adapters
- API handlers
- UI components
- maintenance routines

Modules should be easy to replace, audit, and test without rewriting the whole system.

Modularity does not mean scattering authority. It means keeping execution surfaces narrow and composable.

## Abstract

Hide implementation complexity behind stable, legible interfaces.

The visible surface of the system should be simpler than the machinery behind it.

Use abstraction to reduce cognitive load, not to create mystery.

Good abstraction means:

- a small number of clear commands or entry points
- shared utilities instead of repeated low-level logic
- stable interfaces around backends, paths, runtime state, and control-plane actions
- implementation detail staying inside the owning module

Bad abstraction means:

- vague wrappers that hide where authority actually lives
- helper layers that duplicate underlying rules
- naming that sounds generic but does not reduce real complexity

## Orthogonalize

Keep distinct concerns independent so one change does not force unrelated changes elsewhere.

Orthogonality protects the system from entanglement.

Good orthogonality means:

- runtime use and meta-development stay separate
- policy and execution do not collapse into one file
- feature activation, backend dispatch, path resolution, and audit logic have distinct boundaries
- changing one backend or one feature does not require rewriting unrelated system pieces

Bad orthogonality means:

- one module carrying multiple unrelated responsibilities
- path, policy, state, and execution logic fused together
- side effects that leak across the system without a clear boundary
- a refactor in one subsystem forcing incidental edits in many others

## Implementation Principles

These principles are extracted from mature systems codebases and from the provider CLI shape used by this repo.

### Encode Invariants

Make invalid states hard to represent.

Use types, schemas, constructors, state machines, and validation boundaries to express system invariants directly.

Do not rely on comments, naming, or caller discipline for rules the code can encode.

Prefer:

- typed states over stringly-typed status values
- constructors that validate durable objects
- narrow public APIs around sensitive state transitions
- explicit capability or permission values instead of ambient access
- machine-readable contracts when humans and tools both depend on the rule

### Keep Public Surfaces Boring

Hide internal complexity behind stable, ordinary interfaces.

The more complex the implementation, the simpler the caller-facing surface should be.

Prefer:

- small entry points
- obvious names
- predictable return values
- feature flags or configuration that expose real choices, not internal machinery
- documentation that explains tradeoffs without requiring the reader to learn the whole subsystem

### Separate Ergonomics From Semantics

Convenience should not blur what the system means.

Ergonomic helpers are good when they preserve the same failure modes, authority boundaries, and data model as the lower-level API.

Avoid helpers that:

- skip validation
- hide ownership of state
- swallow meaningful errors
- make a policy decision look like a formatting or transport detail
- create a second unofficial way to perform the same operation

### Treat Errors As System Information

Errors should preserve both machine-actionable structure and human-useful context.

Application code may use broad error types when the caller only needs context and exit behavior.

Library, protocol, storage, and integration boundaries should expose errors precise enough for callers to make decisions.

Every error path should answer:

1. What failed?
2. Where did it fail?
3. Is the failure recoverable?
4. Does the caller need a typed distinction?
5. What context would the operator need during debugging?

### Contain Dangerous Power

Unsafe operations, direct system calls, global state, unchecked casts, raw concurrency primitives, shell execution, file edits, and destructive side effects must be isolated behind small reviewed boundaries.

The boundary should document:

- what invariant makes the operation valid
- who is allowed to call it
- what state it may mutate
- what tests, assertions, or runtime checks defend it

Do not spread privileged operations through convenience helpers.

### Design For Replacement

External services, storage backends, model providers, runtimes, and platform-specific integrations should sit behind replaceable adapters.

Replacement does not mean pretending every backend is identical.

It means:

- shared policy lives above the adapter
- backend-specific behavior stays inside the adapter
- capability differences are explicit
- tests can exercise the contract without depending on every real backend

### Test Behavior At The Boundary

Unit tests are not enough for code whose risk lives at integration boundaries.

Use the cheapest test that can catch the real failure mode.

Prefer:

- small unit tests for pure logic
- regression tests for fixed bugs
- integration tests for command, API, storage, and adapter behavior
- smoke tests for terminal, session, routing, and approval flows
- live provider smoke tests only when credentials and network access are intentionally available

### Explain Tradeoffs Close To The Code

When code chooses performance, compatibility, portability, simplicity, provider behavior, or strict correctness over another value, explain that choice near the implementation.

Good comments explain why the shape exists.

Bad comments restate what the code already says.

## Provider CLI Principles

Provider CLIs have additional load-bearing rules.

Secrets stay outside the repo and outside persisted session state.

Provider-specific facts belong in the provider boundary:

- API base URL
- endpoint shape
- model IDs
- request and response quirks
- thinking-block or stream normalization
- cache-token and usage reporting behavior

Shared CLI behavior belongs above the provider boundary:

- command parsing
- stdout and stderr contract
- session lifecycle
- workspace root selection
- tool routing policy
- approval gates
- transcript shape

Session state should be explicit, inspectable, and disposable.

Workspace-aware behavior must be root-scoped. Prompts or tool calls that reach outside the selected root need clarification or approval instead of silent execution.

Tool execution must preserve a clear escalation ladder:

1. read-only inspection
2. model-authored plan or proposed edit
3. explicit user approval
4. bounded command or exact edit
5. transcript evidence

One-off mode, docked chat, and agent mode should share core behavior where possible but keep their user contracts distinct.

## Anti-Patterns

Avoid:

- duplicated business logic in multiple files
- hidden config access spread across the repo
- one-off scripts that silently redefine canonical rules
- feature work that bypasses shared system boundaries
- modules that own both policy and every downstream implementation detail
- abstractions that make the surface more confusing instead of simpler
- subsystem coupling that makes independent changes impossible
- convenience APIs that bypass canonical validation
- broad error handling at boundaries where callers need typed decisions
- privileged operations scattered through ordinary business logic
- adapters that quietly redefine shared policy
- tests that only confirm implementation details while missing boundary behavior
- provider-specific quirks leaking into command parsing or workspace policy
- persisted secrets, transcripts, or session files that contain credentials
- tool execution without root scope, approval state, and transcript evidence

## Commit Discipline

Every commit should be a small, self-contained change with a project-appropriate subsystem prefix.

The allowed prefixes are project-specific. Keep them in a local override file when this scaffold is reused across projects.

Use the subsystem that owns the behavior being changed. Documentation-only changes use `[docs]` unless the documentation is part of a subsystem contract, in which case use that subsystem.

Non-functional changes must be marked as NFC:

```text
[runtime] NFC — extract preflight helper
```

Do not mix NFC refactors with behavior changes. If a cleanup enables a functional fix, split it into separate commits.

Examples:

```text
[runtime] fix audit evidence labels
[state] ignore generated runtime manifests
[docs] document commit discipline
```

If the project has a commit-message checker, use that checker as the machine-enforced version of this rule.

Malformed commits:

- missing subsystem prefix
- using NFC for a behavior change
- mixing unrelated subsystems in one commit
- bundling generated runtime files with source changes

## Practical Test

Before merging new code, ask:

1. Where is the single source of truth?
2. What module boundary owns execution?
3. Did this abstraction make the interface simpler?
4. Did this change preserve orthogonality between concerns?
5. Did this change create a duplicate rule?
6. If this integration changes, is there one place to update it?
7. Is the commit message prefixed with the owning subsystem?
8. If this is NFC, is it free of behavior changes?
9. Did the code encode the invariant instead of relying on caller discipline?
10. Are dangerous operations contained behind a small boundary?
11. Does the error shape match what the caller needs to know?
12. Is the behavior tested at the boundary where it can actually fail?
13. Did provider-specific logic stay inside the provider boundary?
14. Are secrets excluded from repo files, transcripts, and session state?
15. Does tool-capable behavior preserve root scope, approval, and evidence?

If those answers are unclear, the code is not shaped correctly yet.
