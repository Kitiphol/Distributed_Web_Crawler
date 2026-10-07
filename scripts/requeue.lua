-- requeue.lua: move one item from a dead worker loop's processing list back to its job queue.
-- KEYS[1] = proc:{dead node}:{loop}
-- Returns the moved item "<job_id> <url>", or nil when the list is empty.
-- `pending` is untouched: the URL was never finished, so it is still counted.
local item = redis.call('LPOP', KEYS[1])
if not item then
  return false
end
local sp = string.find(item, ' ', 1, true)
local id = string.sub(item, 1, sp - 1)
local url = string.sub(item, sp + 1)
if redis.call('SISMEMBER', 'job:' .. id .. ':finished', url) == 0 then
  redis.call('RPUSH', 'job:' .. id .. ':queue', url)
end
return item
