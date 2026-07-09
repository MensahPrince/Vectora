# cutlass-ai

`cutlass-ai` is the prompt-to-edit layer for Cutlass. It turns model responses into validated editor commands that callers can apply through the normal engine path.

The crate does not mutate projects directly. It describes project state for the model, accepts tool calls or command-shaped responses, validates them against a project snapshot, and returns `cutlass-commands` values for the caller to apply.

## Responsibilities

- Define the LLM-facing wire command vocabulary.
- Generate tool schemas for supported edit operations.
- Summarize the current project and editor context for prompts.
- Validate model-requested edits before they reach the engine.
- Return model-readable rejections for invalid requests.
- Provide a provider abstraction for chat and tool-calling backends.
- Support OpenAI-compatible HTTP endpoints and deterministic test providers.
- Run the prompt loop and report agent events to the caller.

## Main APIs

- `run_prompt`: execute one assistant prompt flow.
- `AgentConfig`: prompt-loop configuration.
- `AgentEvent`: streamed events emitted while a prompt runs.
- `PromptOutcome` and `PromptStatus`: final prompt result.
- `EngineBridge`: trait used by the agent loop to inspect and apply edits through a host.
- `summarize`: compact project summary for model context.
- `validate`: lower wire commands into engine commands or return rejections.
- `tool_specs` and `TOOL_SCHEMA_VERSION`: tool schema surface exposed to providers.
- `WireCommand`: model-facing edit request type.
- `AgentExtensions`, `load_agent_dir`, `bundled_skills`, `merge_skills`: prompt extensibility (rules, skills, slash commands).

## Rules, skills, and slash commands

Users can extend the assistant through `~/.cutlass/agent/` (reloaded before every prompt, no restart needed):

- `rules/*.md`: always-on rules injected into the system prompt, capped at `MAX_RULES_BYTES` (over-budget rules truncate with a visible warning in the agent panel).
- `skills/<id>/SKILL.md`: on-demand procedural workflows with YAML frontmatter (`name`, `description`). Only the name/description index enters the system prompt; the model fetches a body through the read-only `read_skill` tool.
- `commands/*.md`: slash-command prompt templates. Typing `/name args` in the chat panel expands the template client-side (`$ARGUMENTS` substitution).

Per-project rules live in `ProjectMetadata.agent_rules`, edited from the agent panel, and travel with the project file. The panel shows them (never silently applies them) when a project arrives from elsewhere. Three first-party skills ship embedded from `assets/skills/`; user skills with the same id win.

All of this is prompt-level only: the closed command vocabulary, validation, dry-run, and one-undo-group invariants are untouched, so a bad rule or skill can at worst propose bad edits.

## Configuration

The desktop app reads AI settings from `~/.cutlass/config.toml`:

```toml
[ai]
base_url = "http://localhost:11434/v1"
model = "qwen3:14b"
# api_key_env = "OPENAI_API_KEY"
```

Secrets should stay in user config or environment variables. They should not be stored in project files or committed to the repository.

## Adding agent capabilities

When the assistant should learn a new edit operation, update the wire type, validation, tool schema snapshot, action description, and end-to-end agent tests together. The agent surface is deliberately closed so model-visible behavior changes are reviewed as code.

## Testing

Run tests with:

```bash
cargo test -p cutlass-ai
```

If the tool schema intentionally changes, re-bless the snapshot with:

```bash
BLESS_TOOL_SCHEMA=1 cargo test -p cutlass-ai
```
