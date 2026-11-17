-- Development-index policy is an authored package, not an RPC or event handler.
-- It registers only when the deployment bundle publishes the indices dataset.
if content.data('sovereigntyindices') == nil then return end

register.content {
    key = 'sovereignty_indices',
    schema_version = 1,
    content_version = '1',
    datasets = { 'sovereigntyindices' },
    state_schema = 'sovereignty_indices_v1',
    operations = { 'Compile' },
    progress_rules = {},
}
