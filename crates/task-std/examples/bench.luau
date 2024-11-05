local started = os.clock()
for i = 1, 1000000 do
	task.spawn(function() end)
end
local took = os.clock() - started
print(`Spawned 1000000 sleeping threads in {took}s`)
