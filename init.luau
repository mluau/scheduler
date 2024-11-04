--!nocheck
--!nolint UnknownGlobal

local thread1 = spawn(function()
	print("1", "Waiting for 1 second...")
	wait(1)
	print("1", "Finished waiting")
end)

local thread2 = spawn(function()
	print("2", "Waiting for 1 second...")
	wait(1)
	print("2", "Finished waiting")
end)

print("Separate thread!")

cancel(thread2)
