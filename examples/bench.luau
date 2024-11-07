-- for testing
local task = task or require("@lune/task")

local started = os.clock()

local amount = 1000000
local batches = 5
local per_batch = amount / batches

for current = 1, batches do
	local thread = coroutine.running()

	print(`Batch {current} / {batches}`)

	for i = 1, per_batch do
		task.spawn(function()
			task.wait(0.1)

			if i == per_batch then
				assert(coroutine.status(thread) == "suspended")
				task.spawn(thread)
			end
		end)
	end

	coroutine.yield()
end
local took = os.clock() - started
print(`Spawned {amount} sleeping threads in {took}s`)
