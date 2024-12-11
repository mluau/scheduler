local yielded = false
local thread = task.spawn(function()
    while true do
        local f = coroutine.yield()
        yielded = true
        f()
        yielded = false
    end
end)

task.spawn(thread, function() end)
print(yielded)
task.spawn(thread, task.wait, 1)
print(yielded)
task.wait(1)
print(yielded)
