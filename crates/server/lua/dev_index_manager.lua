-- The host captures the authenticated request scope and returns the final wire value.
local function snapshot(ctx, args, kwargs)
    return ctx:content_snapshot('sovereignty')
end

register.rpc('devIndexManager', 'GetDevelopmentIndicesForSystem', snapshot)
register.rpc('devIndexManager', 'GetAllDevelopmentIndices', snapshot)
