-- Server-authored fields-and-deployables policy.
-- Compile maps the authored payload routing onto the typed definitions the
-- server executes mechanically; every dogma value is read from the SDE at
-- capture, never copied into content.
return {
    Compile = function(input)
        local authored = input.authored
        local payloads = {}
        for group, family in pairs(authored.payloads) do
            payloads[group + 0] = family
        end
        return {
            payloads = payloads,
            stasis_collection_id = authored.stasis_collection_id,
        }
    end,
}
