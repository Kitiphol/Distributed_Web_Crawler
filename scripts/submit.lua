-- submit.lua: create a crawl job atomically, or return the existing one.
-- KEYS[1] = jobs:by_url        KEYS[2] = jobs:active
-- ARGV[1] = normalized url     ARGV[2] = candidate job id     ARGV[3] = now (unix seconds)
-- Returns {"new", id} or {"existing", id}.
local existing = redis.call('HGET', KEYS[1], ARGV[1])
if existing then
  return {'existing', existing}
end
local id = ARGV[2]
local p = 'job:' .. id .. ':'
redis.call('HSET', KEYS[1], ARGV[1], id)
redis.call('HSET', p .. 'meta', 'base', ARGV[1], 'status', 'running', 'submitted_at', ARGV[3])
redis.call('SADD', p .. 'seen', ARGV[1])
redis.call('SET', p .. 'pending', 1)
redis.call('RPUSH', p .. 'queue', ARGV[1])
redis.call('SADD', KEYS[2], id)
return {'new', id}
