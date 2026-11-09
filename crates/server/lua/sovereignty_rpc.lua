-- The host captures the authenticated request scope and returns the final wire value.
local function snapshot(ctx, args, kwargs)
    return ctx:content_snapshot('sovereignty')
end

register.rpc('sovMgr', 'GetSystemSovereigntyInfo', snapshot)
register.rpc('sovMgr', 'GetInfrastructureHubInfo', snapshot)
register.rpc('sovMgr', 'GetSovStructuresInfoForSolarSystem', snapshot)
register.rpc('sovMgr', 'GetSovStructuresInfoForLocalSolarSystem', snapshot)
register.rpc('allianceRegistry', 'GetAllianceSovereigntyStructuresInfo', snapshot)
register.rpc('allianceRegistry', 'GetCapitalSystemInfo', snapshot)
