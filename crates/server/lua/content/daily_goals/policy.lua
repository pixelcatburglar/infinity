-- Closed daily policy: host input exposes immutable bounded facts and definitions.
local function matches(input, candidate, entry)
    for i = 1, input:condition_count(candidate, entry) do
        local kind, expected, actual = input:condition(candidate, entry, i)
        if kind ~= "any" and expected ~= actual then return false end
    end
    return true
end
return {
    Assign = function(input)
        local out = {}
        for slot = 1, input:slot_count() do
            local eligible = {}
            for index = 1, input:pool_count(slot) do
                local key = input:pool_key(slot, index)
                local used = false
                for i = 1, #out do if out[i] == key then used = true end end
                if not used then eligible[#eligible + 1] = key end
            end
            if #eligible == 0 then error("no available daily template") end
            out[slot] = eligible[input:entropy(slot, #eligible) + 1]
        end
        return { keys = out }
    end,
    Match = function(input)
        local out = {}
        for candidate = 1, input:candidate_count() do
            if input:method(candidate) == input:fact_method() then
                for entry = 1, input:entry_count(candidate) do
                    if matches(input, candidate, entry) then
                        out[#out + 1] = input:candidate_key(candidate)
                        break
                    end
                end
            end
        end
        return { keys = out }
    end,
    Cascade = function(input)
        local ordinary, dates = {}, {}
        for i = 1, input:ordinary_count() do ordinary[input:ordinary_id(i)] = true end
        for i = 1, input:bonus_date_count() do dates[input:bonus_date(i)] = true end
        local count, days = 0, 0
        for _ in pairs(ordinary) do count = count + 1 end
        for _ in pairs(dates) do days = days + 1 end
        if count > input:bonus_target() then count = input:bonus_target() end
        if days > input:month_maximum() then days = input:month_maximum() end
        return { bonus = count, month = days }
    end,
    Project = function(input)
        local out = {}
        for i = 1, input:projection_count() do
            if input:projection_selected(i) then out[#out + 1] = input:projection_key(i) end
        end
        return { keys = out }
    end,
}
