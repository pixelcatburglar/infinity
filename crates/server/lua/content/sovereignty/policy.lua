-- Server-authored sovereignty policy; timestamps use FILETIME ticks.
return {
    InitializeCapital = function(input, policy)
        if input.current_system ~= nil or input.first_appointment ~= nil
            or input.last_loss ~= nil or input.owned_count ~= 1
            or (input.not_before ~= nil and input.not_before > input.now) then
            return { kind = "unchanged" }
        end
        return { kind = "capital", current_system = input.system,
            not_before = input.now + policy.capital_cooldown_ticks }
    end,
    CapitalLost = function(input, policy)
        if input.current_system ~= input.system then return { kind = "unchanged" } end
        local deadline = input.now + policy.capital_cooldown_ticks
        if input.not_before ~= nil and input.not_before > deadline then deadline = input.not_before end
        return { kind = "capital_lost", not_before = deadline }
    end,
    SelfDestruct = function(input, policy)
        return { kind = "self_destruct", due = input.now + policy.self_destruct_ticks }
    end,
}
