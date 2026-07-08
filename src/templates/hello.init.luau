-- An example Root package. `@self` refers to this folder.
local hello = {}

function hello.greet(who: string): string
    return `hello, {who}!`
end

return hello
