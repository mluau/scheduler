# mlua_scheduler

A Roblox-like scheduler for mlua

## Features

- Correctness and performance are king. mlua_scheduler is orders of magnitude faster than Lune 0.8's scheduler as of time of writing.
- Simple with simple primitives for getting results out.
- Properly working ``coroutine.yield`` and ``coroutine.resume`` functions that produce equivalent (mostly) results in Roblox's own Luau + Task Scheduling code
- Both mlua non-send and send features supported (thanks to rust async wizardry).
- Custom async function handling that works with Lua's coroutine design paradigm without breaking on edge-cases.