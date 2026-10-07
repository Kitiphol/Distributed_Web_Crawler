-- take.lua: atomically move one URL from the first non-empty job queue into this
-- worker loop's processing list. Non-blocking.
-- KEYS[1]    = proc:{node}:{loop}  (this loop's processing list, 0 or 1 items)
-- KEYS[2..n] = job:{id}:queue keys, already shuffled by the caller
-- ARGV[i]    = the job id belonging to KEYS[i + 1]
-- Returns {job_id, url}, or nil if every queue is empty.
local function split(item)
  local sp = string.find(item, ' ', 1, true)
  return {string.sub(item, 1, sp - 1), string.sub(item, sp + 1)}
end
-- If this loop still holds an item (its previous finish never ran), resume it.
local held = redis.call('LINDEX', KEYS[1], 0)
if held then
  return split(held)
end
for i = 2, #KEYS do
  local url = redis.call('LPOP', KEYS[i])
  if url then
    local id = ARGV[i - 1]
    redis.call('RPUSH', KEYS[1], id .. ' ' .. url)
    return {id, url}
  end
end
return false
