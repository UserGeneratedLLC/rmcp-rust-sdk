# RMCP Roadmap

This roadmap tracks the path to SEP-1730 Tier 1 for the Rust MCP SDK.

Spec 2025-11-25 (suite 0.1.16): Server 100% (30/30) · Client 100% (18/18)
Spec 2026-07-28 (suite 0.2.0-alpha.9): Server 92.5% (37/40) · Client 75.0% (24/32)

---

## Target spec: 2026-07-28 (release 2026-07-28)

All 2026-07-28 work carries the `2026-07-28` label and the
[`2026-07-28 spec` milestone](https://github.com/modelcontextprotocol/rust-sdk/milestone/3).
Per-scenario conformance status is tracked in the epic issue:
[#977 — Tracking: 2026-07-28 spec conformance](https://github.com/modelcontextprotocol/rust-sdk/issues/977).

### Conformance (baseline 2026-07-13, suite `0.2.0-alpha.9`)

- Server: 3 scenarios (`tools-call-with-progress` stateless behavior, SEP-2243 server-side custom headers, and `server-stateless` — the SEP-2575 discovery/negotiation suite at 2/28 checks)
- Client: 8 scenarios (SEP-2243 headers ×3, `request-metadata`, and 4 single-check auth failures: SEP-2350 step-up, pre-registration, SEP-2352 AS migration, SEP-2468 issuer validation); fixes for SEP-2350 (#888) and SEP-2352 (#965) are already in review
- CI: run the full `--spec-version 2026-07-28` suites (stateless server) instead of hand-picked scenario lists; re-baseline on each draft-suite bump

### Spec features without conformance scenarios

Conformance alone does not cover the full spec surface. Feature work tracked via the milestone:

- SEP-2567 sessionless MCP via explicit state handles (#870)
- SEP-2260 server requests must associate with a client request (#873)
- SEP-2549 follow-up: client-side TTL-honoring cache (#974)

(SEP-2575 discovery & negotiation is covered by the `server-stateless` conformance scenario;
implementation is in review — #869, PRs #973, #943.)

### Release

The 2026-07-28 implementation ships as **v3.0.0** (release PR #964): MRTR, SEP-2549 cache hints,
SEP-2243 standard headers, and the SEP-2106 relaxations are merged but unreleased — tiering and
relegation are evaluated against the latest stable release, so cutting v3.0.0 with the remaining
conformance fixes is on the critical path. Migration guide (draft, kept current until release):
[discussion #969](https://github.com/modelcontextprotocol/rust-sdk/discussions/969).

---

## Tier 1 (non-conformance requirements)

### Governance & Policy

- [ ] Create `VERSIONING.md` — document semver scheme, what constitutes a breaking change, and how breaking changes are communicated
- [ ] Publish a dependency update policy (Tier 1 requires a published policy)
- [ ] Cut v3.0.0 (#964) including all conformance fixes (tier relegation is evaluated against the latest stable release)

### Documentation (26/48 → 48/48 features with prose + examples)

#### Undocumented features (14)

- [ ] Tools — image results
- [ ] Tools — audio results
- [ ] Tools — embedded resources
- [ ] Prompts — embedded resources
- [ ] Prompts — image content
- [ ] Elicitation — URL mode
- [ ] Elicitation — default values
- [ ] Elicitation — complete notification
- [ ] Ping
- [ ] SSE transport — legacy (client)
- [ ] SSE transport — legacy (server)
- [ ] Pagination
- [ ] Protocol version negotiation
- [ ] JSON Schema 2020-12 support *(upgrade from partial)*

#### Partially documented features (7)

- [ ] Tools — error handling *(add dedicated prose + example)*
- [ ] Resources — reading binary *(add dedicated example)*
- [ ] Elicitation — form mode *(add prose docs, not just example README)*
- [ ] Elicitation — schema validation *(add prose docs)*
- [ ] Elicitation — enum values *(add prose docs)*
- [ ] Capability negotiation *(add dedicated prose explaining the builder API)*
- [ ] Protocol version negotiation *(document version negotiation behavior)*

---

## Completed

- [x] 2025-11-25 server conformance 100% (30 scenarios + pending `json-schema-2020-12`, `server-sse-polling`)
- [x] 2025-11-25 client conformance 100% (18 scenarios + legacy `auth/2025-03-26-*`)
- [x] SEP-2322 MRTR (14 server scenarios + `sep-2322-client-request-state`)
- [x] SEP-2164 resource not found
- [x] Cache hints (`caching`)
- [x] `http-header-validation`
- [x] Issue triage labels (bug, enhancement, needs confirmation, needs repro, ready for work, P0–P3)

---

## Informational (not scored for tiering)

These extension scenarios are tracked but do not count toward tier advancement:

| Scenario | Tag | Status |
|---|---|---|
| `auth/client-credentials-jwt` | extension | ❌ Failed — JWT `aud` claim verification error |
| `auth/client-credentials-basic` | extension | ✅ Passed |
| `auth/cross-app-access-complete-flow` | extension | ❌ Failed — sends `authorization_code` grant instead of `jwt-bearer` |
| `tasks-*` | extension | Not yet attempted |

---

## Fork-specific changes (UserGeneratedLLC fork)

The fork diverges from upstream `modelcontextprotocol/rust-sdk` on Claude Code
CLI support. Baseline posture: **skills and code carry only what Claude Code
CLI actually honours**. See the downstream skill
[`.cursor/skills/create-mcp-primitive/audit-checklist.md`](https://github.com/UserGeneratedLLC/iris/blob/main/.cursor/skills/create-mcp-primitive/audit-checklist.md)
for the authoritative Claude-CLI-reality anti-pattern list.

### Divergences from upstream

- **Added** — `anthropic-ext` feature: `anthropic/maxResultSizeChars`,
  `claude/channel` + `claude/channel/permission` capabilities,
  `notify_claude_channel` + `notify_claude_channel_permission` Peer helpers,
  `structured_with_text_fallback` helper (Claude Code #41361 workaround),
  `lint::warn_if_over_2kb` (Claude Code #43474 2 KB truncation cliff).

### Deleted (no Claude-CLI consumer)

- **`sep-2243-draft`** — draft SEP, never implemented in the fork beyond a
  scaffold; Claude CLI doesn't emit or honour the `Mcp-Method` / `Mcp-Name`
  headers or the `x-mcp-header` tool-param promotion. Removed 2026-04-23.

### Proposed but rejected

- **`Peer::notify_tasks_status` emitter** — the workspace previously
  substituted via `notify_logging_message`, but per the Claude-CLI-reality
  filter both the MCP `tasks/*` primitive and `notifications/message` are
  unrendered / not implemented client-side (anthropics/claude-code#18617,
  #3174). No consumer; the workspace deleted both paths instead of adding
  the emitter.
- **`search_tools` meta-tool helper** — Claude CLI already defers MCP tools
  via its built-in client-side Tool Search. Server-side `search_tools` would
  be redundant for the sole consumer.
- **Runtime toolset gating à la `github-mcp-server --toolsets`** —
  `notifications/tools/list_changed` is dropped mid-turn on stdio by Claude
  CLI (anthropics/claude-code#50515). The existing category-level
  `LOAD_INITIALLY = false` + `load_tool_group` pattern is sufficient for
  HTTP consumers; runtime toolset gating on stdio is a no-op.

### Under consideration

- **`#[resource]` + `#[resource_router]` + `#[resource_handler]` macros** —
  would delete ~300 LOC of hand-rolled `ResourceRouter<S>` boilerplate
  across the 4 downstream workspace MCPs. Real consumer. Tracked for a
  future fork PR.
- **`Origin` header validation** — currently listed above under Tier 1
  server conformance. `allowed_hosts` covers `Host`; `Origin` needs a
  first-class `allowed_origins` field on `StreamableHttpServerConfig` to
  match the 2025-11-25 spec requirement.
