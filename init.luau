--!nocheck
--!nolint UnknownGlobal

local threads: { [thread]: number } = {
	[coroutine.running()] = 1,
}

local threadIdReserved = 1

print("time", "", "thread", "", "info")

local function printDebug(...: string)
	local thread = coroutine.running()

	if threads[thread] then
		return print(string.format("%.3f", os.clock()), `thread {threads[thread]}`, ...)
	else
		threadIdReserved += 1
		threads[thread] = threadIdReserved
		return printDebug(...)
	end
end

local thread1 = task.spawn(function()
	printDebug("Waiting for 1 second...")
	task.wait(1)
	printDebug("Finished waiting")
end)

local thread2 = task.spawn(function()
	printDebug("Waiting for 1 second...")
	task.wait(1)
	printDebug("Finished waiting")
end)

coroutine.close(thread2)

printDebug(`Canceled thread {threads[thread2]}`)
