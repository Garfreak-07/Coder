# Non-Blocking Enhancements

These items are intentionally outside the current release gate.

| Item | Status | Owner area | Why non-blocking | Minimal future acceptance criteria |
| --- | --- | --- | --- | --- |
| Production embedding provider integration | Tracked | Memory/RAG | CI-safe lexical, dense_mock, and hybrid retrieval already cover normal validation without live credentials. | Configured provider, redaction policy, offline tests, and opt-in live smoke. |
| True streaming SSE/WebSocket planner deltas | Tracked | Planner API / React | Current DTOs expose streaming-ready event shapes with non-streaming responses. | `planner.message.started`, `planner.message.delta`, and `planner.message.completed` delivered incrementally with reconnect behavior. |
| Richer OpenHands server compatibility matrix | Tracked | OpenHands adapter | Current adapter supports configured Agent Server paths and existing tests cover common shapes. | Matrix doc and contract tests for supported OpenHands versions/path strategies. |
| Published npm package | Tracked | Packaging | Local npm wrapper dry-run validates packaging shape; publishing credentials are not required for release hardening. | Versioned package published from CI with provenance and install smoke. |
| Published Homebrew tap | Tracked | Packaging | Formula exists, but tap write access is not needed for the current local release gate. | Formula update automation with checksum verification in tap CI. |
| Signed release artifacts | Tracked | Release | Unsigned local artifacts are enough for current install/smoke validation. | CI signing step, public verification instructions, and failure gate. |
| Checksum verification | Tracked | Release | Dry-run packaging validates paths without requiring final release archives. | SHA256 generated and verified for every release artifact before publish. |
| IDE integration | Tracked | Product integrations | Current React/Rust workbench covers the requested Planner-first loop. | IDE extension can start Planner Chat, show approvals, and open evidence refs. |
| Voice/ASR/TTS inspired by AIRI | Tracked | Optional UX | Conversation feel is text-first; voice is not core to Coder's software workbench boundary. | Optional voice mode with explicit privacy controls and no effect on core CI. |
| Avatar/Live2D/VRM | Tracked as non-core | Optional UX | The directive explicitly excludes avatar systems from Coder core. | Separate optional plugin that does not affect Planner, memory, workflow, or release gates. |
