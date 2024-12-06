function myCoroutine()
    print("Hello, Codecademy!")
    coroutine.yield()
    print("How are you?")
  end
  
  co = coroutine.create(myCoroutine)
  task.wait(1)
  coroutine.resume(co)
  task.wait(1)
  coroutine.resume(co)
  
  