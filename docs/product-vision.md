# Coder Product Vision

## Product goal

Coder is a local-first Agent workflow app for users who know some code and want
to build reliable project workflows without writing workflow code by hand.

The product is not a single coding agent. The product is a workflow builder and
runner:

```text
choose a project
  -> choose a workflow template
  -> configure agents, tools, model keys, and knowledge sources
  -> manually adjust the visual workflow if needed
  -> run with visible context, approvals, events, and rollback
```

The default product experience should stay template-first. Advanced users can
open the canvas and manually connect agents, tools, gates, and conditions.

## Target user

The primary user knows some code, can understand files, commands, and API keys,
but should not need to understand workflow-engine internals.

This user wants to:

- use their own OpenAI or DeepSeek API key;
- keep project files local by default;
- create repeatable workflows around a project;
- inspect and approve risky operations;
- understand why an agent received specific context;
- avoid wasting tokens by sending full transcripts or full repositories.

## Product identity

Coder should be closer to a controlled local workflow app than to a chat app.

Important product constraints:

- Workflows are manually editable and saved as JSON.
- The default workflow library starts with one coding workflow.
- The UI presents Chinese labels for users, but internal schemas, APIs, and
  workflow JSON keep stable English field names.
- Agents do not pass whole conversations by default. They pass compact,
  structured artifacts.
- RAG and memory are inputs to the context manager, not hidden behavior inside
  every agent.
- Tools, MCP servers, skills, and external agent packs are untrusted
  capabilities until explicitly installed, scoped, and approved.

## Why this direction is feasible

Current agent frameworks and research support the direction, but they point to
one important architecture choice: Coder should own the workflow runtime.

- LangGraph validates the graph/state/checkpoint model for long-running,
  controllable agents. Its official docs emphasize persistence,
  human-in-the-loop, streaming, and memory as core needs for agent workflows:
  <https://docs.langchain.com/oss/python/langgraph/overview>
- RAG is a proven pattern for adding external knowledge to generation without
  fine-tuning the model, from the original Retrieval-Augmented Generation paper:
  <https://arxiv.org/abs/2005.11401>
- Long-context research shows that simply passing more text is unreliable;
  models can miss information in the middle of long contexts. This supports
  Coder's decision to select and compress context instead of forwarding full
  transcripts: <https://arxiv.org/abs/2307.03172>
- MemGPT frames memory as virtual context management, which supports the idea
  that a product should manage what enters the model context instead of relying
  on the model alone: <https://arxiv.org/abs/2310.08560>
- ReAct and Toolformer support tool-using agents, but they do not replace a
  deterministic workflow engine with approvals and scoped permissions:
  <https://arxiv.org/abs/2210.03629> and <https://arxiv.org/abs/2302.04761>
- OpenAI Agents SDK is a strong executor adapter because it provides agents,
  tools, handoffs, guardrails, and tracing, but it should sit below Coder's
  workflow contract: <https://openai.github.io/openai-agents-python/>
- DeepSeek exposes an OpenAI-compatible API surface, so it fits the same
  provider-adapter direction for user-provided keys:
  <https://api-docs.deepseek.com/>
- MCP standardizes tool/resource/prompt exposure across servers, but this is an
  integration layer, not a trust boundary:
  <https://modelcontextprotocol.io/specification/>

## Long-term product pillars

### 1. Manual workflow builder

Users can build and adjust workflows visually. Templates provide the safe
starting point; manual canvas editing is the power-user path.

### 2. Agent framework

An agent is not just a prompt. It has:

- role and goal;
- model/provider configuration;
- input contract;
- output artifact schema;
- context policy;
- memory policy;
- allowed tools;
- permission policy;
- token budget;
- approval requirements.

### 3. Context manager

The context manager is the technical core. It builds a minimal context packet
for each agent by selecting:

- user goal;
- upstream artifacts;
- project summary;
- file snippets;
- retrieved knowledge chunks;
- constraints;
- required output schema.

### 4. Memory and RAG

Memory has separate scopes:

- run memory: current workflow state;
- project memory: local project summaries, conventions, and history;
- knowledge memory: user documents and retrieved chunks.

### 5. Capability system

Tools, MCP servers, skills, and external agent packs should be normalized as
capabilities with metadata, schemas, permissions, and installation source.

### 6. Safety and auditability

Agents request actions. Runtime enforces actions.

File mutation, shell commands, network access, GitHub access, and external
capabilities require explicit policy and approval when risky.

## Non-goals for the next version

The next version should not attempt to build a public marketplace, full desktop
packaging, general automation across all websites, or arbitrary free-form agent
teams. Those can be built after the workflow, context, and memory foundations
are stable.
