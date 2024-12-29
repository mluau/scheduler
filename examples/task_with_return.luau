local flag = 0
task.delay(10, function() 
    print("After 10")
    flag = 1
    return 50
end)

while flag == 0 do
    task.wait(0.1)
end

return 100