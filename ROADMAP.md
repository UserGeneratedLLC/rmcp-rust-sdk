# RMCP Roadmap

This roadmap tracks the path to SEP-1730 Tier 1 for the Rust MCP SDK.

Server conformance: 87.5% (28/32) · Client conformance: 80.0% (16/20)

---

## Tier 2 → Tier 1

### Conformance

#### Server (87.5% → 100%)

- [ ] Fix `prompts-get-with-args` — prompt argument handling returns incorrect result (arg1/arg2 not substituted)
- [ ] Fix `prompts-get-embedded-resource` — embedded resource content in prompt responses (invalid content union)
- [ ] Fix `elicitation-sep1330-enums` — enum inference handling per SEP-1330 (missing enumNames for legacy titled enum)
- [ ] Fix `dns-rebinding-protection` — validate `Host` / `Origin` headers on Streamable HTTP transport (accepts invalid headers with 200)

#### Client (80.0% → 100%)

- [ ] Fix `auth/metadata-var3` — AS metadata discovery variant 3 (no authorization support detected)
- [ ] Fix `auth/scope-from-www-authenticate` — use scope parameter from WWW-Authenticate header on 403 insufficient_scope
- [ ] Fix `auth/scope-step-up` — handle 403 `insufficient_scope` and re-authorize with upgraded scopes
- [ ] Fix `auth/2025-03-26-oauth-endpoint-fallback` — legacy OAuth endpoint fallback for pre-2025-06-18 servers (no authorization support detected)

### Governance & Policy

- [ ] Create `VERSIONING.md` — document semver scheme, what constitutes a breaking change, and how breaking changes are communicated

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

## Informational (not scored for tiering)

These draft/extension scenarios are tracked but do not count toward tier advancement:

| Scenario | Tag | Status |
|---|---|---|
| `auth/resource-mismatch` | draft | ❌ Failed |
| `auth/client-credentials-jwt` | extension | ❌ Failed — JWT `aud` claim verification error |
| `auth/client-credentials-basic` | extension | ✅ Passed |
| `auth/cross-app-access-complete-flow` | extension | ❌ Failed — sends `authorization_code` grant instead of `jwt-bearer` |

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
