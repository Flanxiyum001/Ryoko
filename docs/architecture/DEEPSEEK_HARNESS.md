# DeepSeek-Inspired Agent Harness (`ryoko-harness`)

Ryoko incorporates the core architecture, composability principles, and operational
patterns from [DeepSeek Harness (`deepseek-ai/deepseek-harness`)](https://github.com/deepseek-ai/deepseek-harness),
implemented natively in Rust for cross-platform performance without external Node/Python runtime
dependencies.

## Key Design Principles

1. **"Agent = Model + Harness"**:
   The language model handles reasoning and generation; the harness handles
   the runtime execution environment: filesystem access, sandboxed validation,
   staged edits, tool registries, durable tracing, loop prevention, and turn control.

2. **"Everything is a Plugin" (Spatiotemporal Composability)**:
   All auxiliary behavior (loop protection, session journaling, diff validation,
   telemetry) is implemented as composable plugins adhering to the [`HarnessPlugin`] contract.
   Plugins hook into the execution lifecycle:
   - `on_session_start` / `on_session_end`
   - `on_turn_start` / `on_turn_end`
   - `pre_model_request` / `on_model_response`
   - `pre_tool_call` / `post_tool_call`

3. **Operating Profiles**:
   Harness behavior is organized into named profiles that bundle specific toolsets,
   turn/character limits, and guard policies:
   - `code` (default): Specialized for software engineering. Staged diffs, Fast Context,
     review findings, plan tracking, loop guard.
   - `standard`: Full interactive assistant with on-demand skills, project memory,
     and optional command execution under explicit human confirmation.
   - `headless`: Non-interactive automation / CI runner with strict turn caps,
     fast-fail on repeated errors, and JSON trace export.
   - `minimal`: Lightweight runner with restricted toolsets and reduced token footprints (60k char budget).

4. **Session Journaling & Execution Tracing**:
   Every turn, prompt dimension, model response latency, tool invocation, compaction event,
   and loop intervention is recorded in memory within [`SessionJournal`].
   The CLI exports a journal only when the user supplies `--journal`, after the run returns;
   events recorded in memory are not permanently captured, and interrupted runs do not reach export.
   Tool entries include status, output length, and duration, but not full tool output text,
   so the journal cannot provide interruption or output recovery. Exported sessions can be
   inspected with `ryoko harness trace <journal.json>` to review the timeline of model decisions and tool executions.

5. **Loop and Thrashing Prevention**:
   The [`LoopGuard`] tracks consecutive identical tool calls or cyclic thrashing.
   When repeated calls cross the safety threshold, the guard intervenes by returning
   an instructional warning directly to the model, guiding it to re-evaluate its approach
   before exhausting budget or turns.

6. **DeepSeek Model Provider Integration**:
   - Built-in provider preset for DeepSeek (`https://api.deepseek.com`, key env `DEEPSEEK_API_KEY`).
   - Native support for DeepSeek Reasoner (`deepseek-reasoner` / R1 / V4) SSE streams,
     preserving and streaming `reasoning_content` deltas alongside answer text.

## CLI Usage

### Run with a profile
```sh
# Run code engineering agent (default profile)
ryoko harness run --config configs/providers.json --model deepseek/deepseek-chat --goal "Implement feature X"

# Run non-interactive headless automation and save execution trace
ryoko harness run --config configs/providers.json --model deepseek/deepseek-chat --goal "Run audits" --profile headless --journal trace.json

# Shorthand alias
ryoko dsh run --config configs/providers.json --model deepseek/deepseek-chat --goal "Fix bug"
```

### View available profiles
```sh
ryoko harness profiles
```

### Inspect an execution trace
```sh
ryoko harness trace trace.json
```

## Security and Safety

- Edits are **staged for human review**, never written directly to the user's checkout.
- Commands require explicit human approval per call through [`ApprovalGate`].
- Prompt sections treat file contents, tool outputs, and memory as untrusted data.
