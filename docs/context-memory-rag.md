# Context, Memory, and RAG Design

## Core principle

Context management is the product's hardest technical problem.

Coder should not rely on longer context windows as the main solution. Research
on long-context language models shows that relevant information can be missed
when it is buried in long inputs. Coder should therefore select, compress, and
prove the provenance of context before calling an agent.

Reference: Lost in the Middle, <https://arxiv.org/abs/2307.03172>

## Agent handoff model

Agents should not hand off full transcripts.

They should hand off validated artifacts:

```text
Planner -> Executor:
  plan_artifact

Executor -> Tester:
  patch_artifact
  changed_files
  check_command

Tester -> Planner or User:
  review_artifact
  failing_evidence
  next_recommendation
```

The previous agent can recommend what the next agent needs, but the context
manager decides what actually enters the next context packet.

## ContextPacket

Every agent invocation should receive a `ContextPacket`:

```json
{
  "task": {
    "user_goal": "",
    "workflow_goal": "",
    "current_node": "",
    "current_agent_role": ""
  },
  "upstream_artifacts": [],
  "project_context": {
    "project_summary": "",
    "selected_files": [],
    "selected_snippets": []
  },
  "knowledge_context": {
    "knowledge_sources": [],
    "retrieved_chunks": []
  },
  "memory_context": {
    "run_memory": [],
    "project_memory": [],
    "user_preferences": []
  },
  "constraints": {
    "allowed_tools": [],
    "write_scopes": [],
    "token_budget": 0,
    "approval_required": true
  },
  "required_output": {
    "artifact_type": "",
    "schema": {}
  },
  "provenance": [],
  "token_estimate": {
    "input_tokens": 0,
    "budget_remaining": 0
  }
}
```

The UI should expose this packet in readable Chinese:

- this agent received these files;
- this agent received these snippets;
- this agent received these document chunks;
- these upstream decisions were included;
- these items were omitted to save tokens.

## Memory scopes

### Run memory

Temporary memory for one workflow run:

- current user goal;
- node results;
- artifacts;
- approvals;
- patch state;
- errors;
- token usage.

This already mostly matches the current runtime state.

### Project memory

Persistent memory for one local project:

- project file tree summary;
- architecture summary;
- package manager and test commands;
- coding conventions;
- known risky files;
- previous run summaries;
- user-approved preferences.

Project memory should be explicit and inspectable. It should not silently learn
from every interaction without user visibility.

### Knowledge memory

User-provided documents and external references:

- markdown notes;
- txt files;
- project docs;
- API docs;
- copied requirements;
- future PDF and Word imports.

Knowledge memory is retrieved by the RAG layer and passed into context packets.

## RAG MVP

The first RAG version should be deliberately small:

- support `.md` and `.txt`;
- store documents locally;
- chunk documents with stable chunk IDs;
- create embeddings through the configured provider or a local embedding model;
- retrieve top-k chunks per agent call;
- include source path and chunk ID in context packet provenance.

Avoid a complex agentic RAG system at first. The first goal is reliable,
inspectable retrieval, not maximum autonomy.

## Project summary MVP

For code projects, build a local project index:

- file tree;
- ignored directories;
- language/framework hints;
- important config files;
- test/build commands when detectable;
- module summaries;
- selected snippets for the current request.

This index should feed the context manager. It should not force every agent to
scan the repository from scratch.

## Token-saving strategy

Default rules:

1. Pass stable instructions separately from dynamic context.
2. Pass compact artifacts instead of full chat transcripts.
3. Pass selected snippets instead of full files.
4. Pass retrieved knowledge chunks with source IDs.
5. Limit each context section independently.
6. Track estimated tokens before and after each agent call.
7. Make full event history and full outputs opt-in.
8. Summarize older artifacts before they are reused.

## Default coding workflow context contracts

### Planner

Receives:

- user goal;
- project summary;
- relevant file list;
- retrieved requirement/document chunks;
- allowed tools and scopes.

Produces:

- `plan_artifact`;
- target files;
- required snippets;
- risks;
- executor instructions;
- suggested check command.

### Executor

Receives:

- `plan_artifact`;
- selected target file snippets;
- relevant constraints;
- patch output schema.

Produces:

- `patch_artifact`;
- changed files;
- implementation summary;
- unresolved questions;
- suggested checks.

### Tester / Reviewer

Receives:

- `patch_artifact`;
- changed file summary;
- check output;
- relevant snippets only when needed.

Produces:

- `review_artifact`;
- pass/fail decision;
- failure evidence;
- next action.

## Hard problems to solve later

- automatic snippet selection for large repositories;
- detecting stale project memory;
- summarizing code without losing important edge cases;
- cross-agent conflict resolution;
- multi-run learning without privacy surprises;
- malicious or low-quality knowledge documents;
- prompt injection in retrieved content and MCP/tool metadata.

## Security notes

RAG and tool metadata are not trusted. Retrieved content can contain malicious
instructions. The runtime should mark retrieved content as data, not developer
instructions, and keep capability permission checks outside the model.

MCP servers and GitHub-hosted agent packs should be treated like third-party
code. They need explicit installation, permissions, provenance, and revocation.
