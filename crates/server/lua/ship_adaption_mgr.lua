-- The package is optional without a configured deployment bundle.
if content.data('shipadaptionnodes') == nil then return end

register.content {
    key = 'ship_adaption',
    schema_version = 1,
    content_version = '2',
    datasets = {
        'shipadaptionnodes', 'shipadaptiontrees', 'shipadaptionnodetypes',
        'shipadaptionnodeunlocktypes', 'shipadaptionconfig', 'shipadaptionchips',
        'shipadaptionchipexchangeconfig',
    },
    state_schema = 'ship_adaption_chip_v1',
    operations = { 'InjectChips', 'ChipExchange' },
    progress_rules = {},
}

local function invoke(ctx, args, kwargs)
    local revision = ctx:content_snapshot('ship_adaption')
    return ctx:content_commit('ship_adaption', revision)
end

register.rpc('shipAdaptionMgr', 'fetch_tree_data', invoke)
register.rpc('shipAdaptionMgr', 'fetch_missions', invoke)
register.rpc('shipAdaptionMgr', 'fetch_user_data', invoke)
register.rpc('shipAdaptionMgr', 'InjectChips', invoke)
register.rpc('shipAdaptionMgr', 'call_chip_exchange', invoke)
