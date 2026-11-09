-- Gateway orchestration delegates each operation to the scoped native kernel.
register.content {
    key = "daily_goals",
    schema_version = 1,
    content_version = "1",
    datasets = {},
    state_schema = "daily_goals_v1",
    operations = { "Assign", "Match", "Cascade", "Project" },
    progress_rules = {},
}

local function read(context, _request)
    return context:content_read("daily_goals")
end

local function commit(context, _request)
    return context:content_commit("daily_goals")
end

register.gateway("eve_public.dailygoal.api.GetAllCurrentRequest", read)
register.gateway("eve_public.dailygoal.api.GetRequest", read)
register.gateway("eve_public.dailygoal.api.GetAllWithRewardsRequest", read)
register.gateway("eve_public.dailygoal.api.RedeemRequest", commit)
register.gateway("eve_public.dailygoal.api.PayForCompletionRequest", commit)
register.gateway("eve_public.dailygoal.api.admin.ProgressRequest", commit)
