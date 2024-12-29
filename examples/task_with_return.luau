local flag = 0
task.delay(2, function() 
    print("After 2")
    flag = 1
    return 50
end)

while flag == 0 do
    task.wait(0.1)
end

print("After 2 done")

_TEST_ASYNC_WORK(0)

return 100