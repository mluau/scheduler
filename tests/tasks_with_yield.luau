--!nocheck
-- Create a coroutine
local co = coroutine.create(function()
    print("LUA: Doing _TEST_ASYNC_WORK(0)")
    _TEST_ASYNC_WORK(5)
    print("LUA: Done with _TEST_ASYNC_WORK(0)")
end)

coroutine.resume(co)

print("LUA: Wait 1 second")
task.wait(1)
_TEST_ASYNC_WORK(2)