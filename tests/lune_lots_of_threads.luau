--!nolint UnknownGlobal
local task = task or require("@lune/task")

local NUM_BATCHES = 10
local NUM_THREADS = 100_000

print(`Spawning {NUM_BATCHES * NUM_THREADS} threads split into {NUM_BATCHES} batches\n`)

local before = os.clock()
for i = 1, NUM_BATCHES do
	print(`Batch {i} of {NUM_BATCHES}`)
	local thread = coroutine.running()

	local counter = 0
	for j = 1, NUM_THREADS do
		task.spawn(function()
			task.wait(0.1)
			counter += 1
			if counter == NUM_THREADS then
				task.spawn(thread)
			end
		end)
	end

	coroutine.yield()
end
local after = os.clock()

print(`\nSpawned {NUM_BATCHES * NUM_THREADS} sleeping threads in {after - before}s`)