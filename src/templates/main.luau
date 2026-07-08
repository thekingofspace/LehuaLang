--#include[fs]

-- `require`, `parallel`, `__dirname`, `__filename` are provided per-module.
print("Lehua is running from " .. __dirname)

-- Roots are packages that live under roots/ — require them by name.
local hello = require("hello")
print(hello.greet("Lehua"))

-- Aliases from .luaurc work too: `@Game` points at the project root.
local hello2 = require("@Game/roots/hello")
assert(hello2 == hello, "same module, same instance")

-- Filesystem access relative to THIS script's folder:
fs.write("./hello.txt", "hello from lehua\n")
print("wrote " .. fs.resolve("./hello.txt"))

-- Native multithreading: spawn a worker and talk to it over a Port.
local worker = parallel("./worker.luau")
for i = 1, 5 do
    worker:Push(i)
    local doubled = worker:Pop()
    print(`{i} doubled is {doubled}`)
end
worker:Close()
print("done")
