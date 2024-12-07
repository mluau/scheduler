local fn: thread, args: any, responder: () -> {}, = ...
return function()
    while true do 
        -- Run the function as a coroutine and wait for it to return a value
        local ok, result = coroutine.resume(fn, args)

        -- If the function errored, throw the error
        if not ok then
            error(result, 0)
        end

        local status = coroutine.status(fn)

        -- If the coroutine is dead, return the result
        if status == "dead" then
            return result
        end
    end
end