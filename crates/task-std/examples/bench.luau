local started = os.clock()
local amount = 1000000
for i = 1, amount do
	task.spawn(function()
		task.wait(5)
	end)
end
local took = os.clock() - started
print(`Spawned {amount} sleeping threads in {took}s`)
