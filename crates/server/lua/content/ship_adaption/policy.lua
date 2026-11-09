-- Pure selection against immutable host-provided catalogue values.
return {
    InjectChips = function(input)
        if not input.injectable then return false end
        return { type_id = input.credit_type, quantity = input.credit_quantity }
    end,
    ChipExchange = function(input)
        if not input.issuer_allowed then return false end
        local result = {}
        local positive = false
        for index = 1, input.count do
            local row = input:entry(index)
            if row == nil or row.quote == nil then return false end
            result[index] = { type_id = row.type_id, quantity = row.quantity, quote = row.quote }
            if row.quote > 0 then positive = true end
        end
        if not positive then return false end
        return result
    end,
}
