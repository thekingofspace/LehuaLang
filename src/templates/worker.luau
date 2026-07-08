-- Runs on its own thread. `channel` is this worker's end of the Port.
while true do
    local n = channel:Pop()
    if n == nil then
        break -- channel closed by the parent
    end
    channel:Push(n * 2)
end
