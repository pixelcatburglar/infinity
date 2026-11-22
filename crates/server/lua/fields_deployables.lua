-- Fields-and-deployables policy is an authored package, not an RPC or event
-- handler. It routes published probe charge groups onto the payload families
-- the server executes and names the dbuff collection a stasis pulse lands on.
register.content {
    key = 'fields_deployables',
    schema_version = 1,
    content_version = '1',
    datasets = {},
    state_schema = 'fields_deployables_v1',
    operations = { 'Compile' },
    progress_rules = {},
}
