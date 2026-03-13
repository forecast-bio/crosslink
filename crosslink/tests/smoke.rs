mod smoke {
    pub mod harness;

    // CLI tests
    mod cli_data; // import/export, archive, knowledge
    mod cli_infra; // config, sync, migrate, integrity, compact, prune
    mod cli_tooling; // cpitd, workflow, context, style, design_doc, mc

    // Server tests
    mod server_api; // REST endpoints + WebSocket

    // Coordination tests
    mod coordination; // events, compaction, locks, push retry, v1->v2

    // Adversarial tests
    mod adversarial; // boundary, corruption, injection, concurrency

    // TUI + proptest
    mod tui_proptest; // TUI renders, proptest extensions
}
