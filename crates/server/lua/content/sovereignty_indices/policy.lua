-- Server-authored sovereignty development-index policy.
-- Compile maps the authored point scalars and the extracted indices dataset onto
-- the typed parameters the server executes mechanically; the interval is
-- converted to FILETIME ticks (100 ns units, client blue time).
return {
    Compile = function(input)
        local authored = input.authored
        local dataset = input.dataset
        local occupancy = dataset.occupancy
        local function band(source)
            return {
                level = source.level,
                min_points = source.min_points,
                max_points = source.max_points,
                modifier = source.modifier,
            }
        end
        local function bands(source)
            local result = {}
            for index, item in pairs(source) do result[index] = band(item) end
            return result
        end
        local function bonuses(source)
            local result = {}
            for index, item in pairs(source) do
                result[index] = { level = item.level, bonus = item.bonus }
            end
            return result
        end
        local function thresholds(source)
            local result = {}
            for index, seconds in pairs(source) do result[index] = seconds end
            return result
        end
        return {
            attributes = {
                military = dataset.attribute_ids.military,
                industrial = dataset.attribute_ids.industrial,
                strategic = dataset.attribute_ids.strategic,
            },
            military = {
                points_per_death = authored.military_points_per_death,
                bands = bands(dataset.military_levels),
            },
            industrial = {
                points_per_m3 = authored.industrial_points_per_m3,
                bands = bands(dataset.industrial_levels),
            },
            strategic_seconds = thresholds(dataset.strategic_seconds),
            decay = {
                points = authored.decay_points,
                interval_ticks = authored.decay_interval_seconds * input.ticks_per_second,
            },
            occupancy = {
                base = occupancy.base,
                min = occupancy.min,
                max = occupancy.max,
                capital = occupancy.capital,
                military = bonuses(occupancy.military),
                industrial = bonuses(occupancy.industrial),
                strategic = bonuses(occupancy.strategic),
            },
        }
    end,
}
