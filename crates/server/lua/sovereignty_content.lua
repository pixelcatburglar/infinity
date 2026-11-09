-- Sovereignty policy is an authored package, not an RPC or event handler.
register.content {
    key = "sovereignty",
    schema_version = 1,
    content_version = "1",
    datasets = {},
    state_schema = "sovereignty_policy_v1",
    operations = { "InitializeCapital", "CapitalLost", "SelfDestruct" },
    progress_rules = {},
}
