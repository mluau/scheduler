# mlua_scheduler

A Roblox-like scheduler for mlua

## Features

- Tokio support + minimal CPU usage
- Correctness and performance are king
- Simple with simple primitives for getting results out.
- Properly working ``coroutine.yield`` and ``coroutine.resume`` functions that produce equivalent (mostly) results in Roblox's own Luau + Task Scheduling code
- Both mlua non-send and send features supported (thanks to rust async wizardry).
- Custom async function handling that works with Lua's coroutine design paradigm without breaking on edge-cases.
- Simple callback API for controlling the scheduler (`SchedulerFeedback` trait)

## Deviations from Roblox's scheduler

- Thread arguments are not preloaded to the stack due to mlua limitations. This is normally not a problem but may cause side-effects. For example, see the below.

```lua
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
```

## V2 scheduler

mlua_scheduler has two scheduler versions. v1 (the original scheduler which used busy poll loop/spinloop that ran every tick to run through tasks) and v2 which is a new scheduler design based on tokio select and delayqueue/tokio channels without any busy polling. Here are the differences:

- v1 scheduler tends to be a bit faster and/or slower than v2 under benchmarks (bench.luau runs a bit faster with v1 than v2 but tests/lune_lots_of_threads.luau runs a bit slower on v1 than v2)
- v1 scheduler eats a lot more CPU than v2 even when there is no work to be done (in production, we found v1 to use 30-100% CPU when idle while v2 only uses 1-2% CPU)
- v2 scheduler is async-aware and polls rust async futures + resumes in scheduler itself. This means that async has similar performance to scheduler native methods in v2 (v1 has a much higher penalty for async calls) and also does not deadlock in async either (see next issue for that)
- v1 scheduler is known to deadlock under large amounts of (potentially recursive or otherwise) rust async calls (especially in ``send`` mode if a LocalRuntime/LocalSet is not used). v2 scheduler is rust async aware and does not have the deadlock issue in rust-async
- v1 scheduler's ``wait_till_done`` may be inaccurate and return early as there is a fundemental race condition between its internal poll loop and lua's thread resume mechanism. v2 scheduler does not have this fundemental race condition and hence has a more accurate ``wait_till_done``