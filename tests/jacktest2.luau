local task = task or require("@lune/task")
local thread = task.delay(1, function(foo)
    print(foo)
    print(coroutine.yield())
    print("c")
    print(coroutine.yield())
    print("e")
end, "b")

task.spawn(thread, "a")

task.delay(2, function()
    coroutine.resume(thread, "d")
end)
