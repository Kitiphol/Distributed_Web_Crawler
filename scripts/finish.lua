-- finish.lua: report one URL, all or nothing, at most once.
-- KEYS: 1 proc list, 2 seen, 3 queue, 4 pending, 5 stats, 6 ext, 7 finished, 8 meta, 9 jobs:active
-- ARGV: 1 job id, 2 url, 3 outcome ("file" | "broken" | "redirect"), 4 extension,
--       5 word count, 6 now (unix seconds), 7.. child urls (normalized, under base, deduplicated)
-- Returns pending after this report (0 = job finished),
--   -2 if this loop no longer holds the URL (a reaper gave it to someone else), or
--   -1 if the URL was already reported.

-- 1. Ownership: only the loop that currently holds the URL may report it. If a reaper
--    moved it back to the queue (this node looked dead), the report is rejected, so a
--    late result from a frozen "zombie" node can never win against the real one.
if redis.call('LREM', KEYS[1], 0, ARGV[1] .. ' ' .. ARGV[2]) == 0 then
  return -2
end

-- 2. Count each URL once (a second line of defense).
if redis.call('SADD', KEYS[7], ARGV[2]) == 0 then
  return -1
end

-- 3. Claim children; queue and count the ones we won.
local won = {}
for i = 7, #ARGV do
  if redis.call('SADD', KEYS[2], ARGV[i]) == 1 then
    won[#won + 1] = ARGV[i]
  end
end
if #won > 0 then
  redis.call('INCRBY', KEYS[4], #won)
  for s = 1, #won, 1000 do                       -- push in chunks (Lua unpack limit)
    redis.call('RPUSH', KEYS[3], unpack(won, s, math.min(s + 999, #won)))
  end
end

-- 4. Stats for this URL.
redis.call('HINCRBY', KEYS[5], 'crawled', 1)
if ARGV[3] == 'file' then
  redis.call('HINCRBY', KEYS[5], 'num_files', 1)
  redis.call('HINCRBY', KEYS[6], ARGV[4], 1)
  redis.call('HINCRBY', KEYS[5], 'total_word_count', tonumber(ARGV[5]))
elseif ARGV[3] == 'broken' then
  redis.call('HINCRBY', KEYS[5], 'broken', 1)
elseif ARGV[3] == 'redirect' then
  redis.call('HINCRBY', KEYS[5], 'redirects', 1)
end

-- 5. This URL is finished. Children were added first, so 0 really means "nothing left".
local left = redis.call('DECR', KEYS[4])
if left == 0 then
  redis.call('HSET', KEYS[8], 'status', 'done', 'finished_at', ARGV[6])
  redis.call('SREM', KEYS[9], ARGV[1])
end
return left
