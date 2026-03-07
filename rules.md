TITAN PROTOCOL v2.0: SYSTEM ARCHITECT MANDATE
==============================================

1. ABSOLUTE IMPLEMENTATION (ANTI-STUB)
- ZERO PLACEHOLDERS: Creating stubs, TODOs, or 'logic to be implemented' is a hard failure.
- RECURSIVE DEVELOPMENT: If a feature requires a sub-system, develop that sub-system immediately. Do not stop until the entire execution chain is functional.
- NO MOCKS: Never simulate or fake API responses or hardware behavior. Build the real bridge.

2. HERMETIC PORTABILITY (THE "ZERO-ENV" RULE)
- CODEBASE IS THE WORLD: The codebase must contain everything needed to run. No "manual" outside steps.
- CONTAINERIZED INFRASTRUCTURE: Docker is used only for test databases (docker-compose.test.yml - PostgreSQL, MySQL, MongoDB, Redis). The Rust ingestion engine runs natively on Windows using Native Shared Memory. DuckDB and SpacetimeDB were NOT IMPLEMENTED and removed from architecture.
- TEST INFRASTRUCTURE: External services (PostgreSQL, MySQL, MongoDB, Redis) required for testing must be orchestrated via Docker Compose to ensure reproducible validation environments.
- AUTOMATED SETUP: Provide a 'bootstrap' script that handles dependency downloads, DLL registration, and environment variables.

3. SOURCE-OF-TRUTH SOVEREIGNTY
- CODE OVER DOCS: Always read the source code files before acting. Ignore outdated READMEs or comments.
- AUDIT BEFORE ACTION: Search for existing logic before creating new modules. Update and refactor; do not duplicate.

4. HFT PERFORMANCE & DATA INTEGRITY
- ZERO-COPY HOTPATH: Use Windows Native Shared Memory and Protobuf binary end-to-end. No string conversion or kernel overhead in the tick-stream. The zero-copy hotpath uses Windows Native Shared Memory (via src/titan_shm.cpp and src/shared_memory.rs) - a lock-free Ring Buffer in native Windows Shared Memory that bypasses the networking stack entirely.
- DETERMINISTIC LATENCY: No blocking I/O or unpredictable GC triggers in the ingestion thread.
- DYNAMIC DISCOVERY: No hardcoding symbols. Use metadata-driven reactivity for all symbols.

5. FRONTEND-FIRST VISIBILITY (ADAPTER STANDARDS)
- THE OBSERVER PRINCIPLE: Assume the user is an observer. Every backend state and metric must be visible via the standardized SDK libraries (@titan/core, @titan/react, @titan/vue, @titan/angular, @titan/svelte).
- DEEP OBSERVABILITY: Structured JSON logging and real-time performance metrics (ticks/sec) are mandatory. All backend capabilities must be monitorable through the framework-agnostic core client.

6. COMPLETION INTEGRITY
- TASK DEFINITION: A task is DONE only when it is implemented, portable, tested, and documented.
- TEST-DRIVEN REALISM: Never assume code works. Write integration tests under /tests that run against real binary data and supported database backends.

---

7. Implementation Standards
Always fully develop and integrate all stubs, placeholders, and TODOs — never delete them to silence warnings.
Replace incomplete sections with real, production-grade implementations.
Never simulate, mock, fake, or partially implement production functionality.
Temporary scaffolding must be converted into complete logic before task completion.

8. No Hardcoding & Centralized Configuration
Absolutely no hardcoded business rules, thresholds, or API URLs.
All dynamic values must come from:
- Central config file (config/system.json)
- Environment variables (DATABASE_TYPE, BACKEND_TYPE, etc.)
- Database-managed configuration
Configuration must be centralized, strongly typed, and documented.
Use a single configuration source of truth: config/system.json. Runtime configuration is controlled via environment variables (DATABASE_TYPE, BACKEND_TYPE, etc.).

9. DRY (Zero Duplicate Logic Policy)
No business logic may exist in more than one location.
Shared functionality must live in `/src/traits`, `/src/core`, or decentralized utils.
Frontend and backend must share the same data contracts (Protobuf).

10. Logging & Debug Visibility (Deep Observability Standard)
Every critical function must contain structured JSON logs including:
- File name, Function name, trace_id, and performance timing where relevant.
Silent failures are forbidden.

11. No Fallbacks or Simulations
Systems must fail visibly and transparently. If a dependency fails, surface an explicit ErrorCode.

12. Architecture (Adapter Pattern)
The system MUST remain:
- Database-agnostic: Support multiple backends via `DatabaseAdapter`.
- Backend-agnostic: Support multiple HTTP frameworks via `BackendServer`.
- Frontend-agnostic: Exposure via standardized SDKs.

13. Parallel Development Discipline
Documentation must evolve with code. Every new system requires:
- Architecture explanation (Wiki)
- Configuration reference
- Integration tests

14. Testing Structure
All tests live under `/tests`.
- `/tests/unit`
- `/tests/integration` (against Docker-orchestrated DBs)

15. Completion Integrity (Strict Definition of Done)
- No TODOs or placeholders.
- No hardcoded values.
- Logging fully implemented.
- Tests passing.
- Wiki updated.
- Trace ID propagation verified.
