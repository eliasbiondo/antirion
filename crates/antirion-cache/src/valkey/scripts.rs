//! The entry index's server-side scripts. Every index key starts with the same base (`KEYS[1]`),
//! which holds one hash tag, so on a Redis Cluster the whole index lives in one slot and each
//! script runs atomically there; entries themselves spread over the slots and are deleted by the
//! caller from the `digest:key` pairs a script returns.
//!
//! Index keys under the base: `m` (key → meta JSON), `x` (key → `digest|dims|caps`, the hashes a
//! removal needs), `z0`…`z4` (the sort orders: last use, created, expires, hits, size), `k` (keys
//! in lexical order), `d:<hash>` (keys carrying a dimension value), `c:<hash>` (a cap's keys by
//! last use) and `agg` (count, bytes, hits).

/// Shared by every script: removes one key from the index; its digest, or nil.
const RM: &str = r"
local b = KEYS[1]
local function rm(k)
  local x = redis.call('HGET', b .. 'x', k)
  if not x then return nil end
  local digest, dims, caps = string.match(x, '^([^|]*)|([^|]*)|(.*)$')
  for d in string.gmatch(dims or '', '%S+') do redis.call('SREM', b .. 'd:' .. d, k) end
  for c in string.gmatch(caps or '', '%S+') do redis.call('ZREM', b .. 'c:' .. c, k) end
  local size = tonumber(redis.call('ZSCORE', b .. 'z4', k) or '0')
  local hits = tonumber(redis.call('ZSCORE', b .. 'z3', k) or '0')
  for i = 0, 4 do redis.call('ZREM', b .. 'z' .. i, k) end
  redis.call('ZREM', b .. 'k', k)
  redis.call('HDEL', b .. 'm', k)
  redis.call('HDEL', b .. 'x', k)
  redis.call('HINCRBY', b .. 'agg', 'n', -1)
  redis.call('HINCRBY', b .. 'agg', 'bytes', -size)
  redis.call('HINCRBY', b .. 'agg', 'hits', -hits)
  return digest
end
local function prune(now, limit)
  local dead = redis.call('ZRANGE', b .. 'z2', '-inf', now, 'BYSCORE', 'LIMIT', 0, limit)
  for _, k in ipairs(dead) do rm(k) end
  return #dead
end
";

/// ARGV: key, meta, `digest|dims|caps`, created, expires, size, last use, now, then (cap hash,
/// max) pairs. Returns the `digest:key` of the entries the caps evicted.
const PUT: &str = r"
local key = ARGV[1]
local now = ARGV[8]
rm(key)
prune(now, 16)
local x = ARGV[3]
redis.call('HSET', b .. 'm', key, ARGV[2])
redis.call('HSET', b .. 'x', key, x)
redis.call('ZADD', b .. 'z0', ARGV[7], key)
redis.call('ZADD', b .. 'z1', ARGV[4], key)
redis.call('ZADD', b .. 'z2', ARGV[5], key)
redis.call('ZADD', b .. 'z3', 0, key)
redis.call('ZADD', b .. 'z4', ARGV[6], key)
redis.call('ZADD', b .. 'k', 0, key)
local _, dims = string.match(x, '^([^|]*)|([^|]*)|')
for d in string.gmatch(dims or '', '%S+') do redis.call('SADD', b .. 'd:' .. d, key) end
redis.call('HINCRBY', b .. 'agg', 'n', 1)
redis.call('HINCRBY', b .. 'agg', 'bytes', ARGV[6])
local evicted = {}
for i = 9, #ARGV, 2 do
  local c = b .. 'c:' .. ARGV[i]
  local max = tonumber(ARGV[i + 1])
  redis.call('ZADD', c, now, key)
  while redis.call('ZCARD', c) > max do
    local oldest = redis.call('ZRANGE', c, 0, 0)[1]
    if (not oldest) or oldest == key then break end
    local d = rm(oldest)
    if d then table.insert(evicted, d .. ':' .. oldest) else redis.call('ZREM', c, oldest) end
  end
end
return evicted
";

/// ARGV: key, now. Counts a hit; 1 when the key is indexed.
const TOUCH: &str = r"
local key = ARGV[1]
local x = redis.call('HGET', b .. 'x', key)
if not x then return 0 end
redis.call('ZINCRBY', b .. 'z3', 1, key)
redis.call('ZADD', b .. 'z0', ARGV[2], key)
redis.call('HINCRBY', b .. 'agg', 'hits', 1)
local caps = string.match(x, '^[^|]*|[^|]*|(.*)$')
for c in string.gmatch(caps or '', '%S+') do redis.call('ZADD', b .. 'c:' .. c, ARGV[2], key) end
return 1
";

/// ARGV: key. Removes it; its digest, or nil.
const REMOVE: &str = r"
return rm(ARGV[1])
";

/// ARGV: dimension hash, digest ('' for any), written at or before. Returns `digest:key` pairs.
const DROP_DIM: &str = r"
local out = {}
local le = tonumber(ARGV[3])
for _, k in ipairs(redis.call('SMEMBERS', b .. 'd:' .. ARGV[1])) do
  local created = tonumber(redis.call('ZSCORE', b .. 'z1', k) or '0')
  local x = redis.call('HGET', b .. 'x', k) or ''
  local digest = string.match(x, '^([^|]*)|') or ''
  if created <= le and (ARGV[2] == '' or ARGV[2] == digest) then
    rm(k)
    table.insert(out, digest .. ':' .. k)
  end
end
return out
";

/// ARGV: written at or before. Returns `digest:key` pairs.
const DROP_OLDER: &str = r"
local out = {}
for _, k in ipairs(redis.call('ZRANGE', b .. 'z1', '-inf', ARGV[1], 'BYSCORE')) do
  local d = rm(k)
  if d then table.insert(out, d .. ':' .. k) end
end
return out
";

/// ARGV: now. Removes expired keys; then [count, bytes, hits].
const SIZE: &str = r"
while prune(ARGV[1], 1000) == 1000 do end
local a = redis.call('HMGET', b .. 'agg', 'n', 'bytes', 'hits')
return {tonumber(a[1] or '0'), tonumber(a[2] or '0'), tonumber(a[3] or '0')}
";

/// ARGV: now, dimension hash. Removes expired keys; then the dimension's count.
const COUNT: &str = r"
while prune(ARGV[1], 1000) == 1000 do end
return redis.call('SCARD', b .. 'd:' .. ARGV[2])
";

/// One row of a listing: key, meta, last use, created, expires, hits, size.
const ROW: &str = r"
local function row(out, k)
  table.insert(out, k)
  table.insert(out, redis.call('HGET', b .. 'm', k) or '')
  for i = 0, 4 do table.insert(out, redis.call('ZSCORE', b .. 'z' .. i, k) or '0') end
end
local function after(s, k, has, av, ak, desc)
  if not has then return true end
  if s ~= av then if desc then return s < av else return s > av end end
  if desc then return k < ak else return k > ak end
end
";

/// ARGV: now, sort index, desc (0|1), has position (0|1), position value, position key, limit,
/// prefix ('' for none), totals (0|1), n, n dimension hashes all must carry, m, m hashes one must
/// carry (m = -1 when every entry is visible). Returns [more, count, bytes, hits, rows…].
const LIST: &str = r"
local now = ARGV[1]
while prune(now, 1000) == 1000 do end
local z = b .. 'z' .. ARGV[2]
local desc = ARGV[3] == '1'
local has = ARGV[4] == '1'
local av = tonumber(ARGV[5])
local ak = ARGV[6]
local want = tonumber(ARGV[7])
local prefix = ARGV[8]
local totals = ARGV[9] == '1'
local n = tonumber(ARGV[10])
local all = {}
for i = 1, n do table.insert(all, b .. 'd:' .. ARGV[10 + i]) end
local m = tonumber(ARGV[11 + n])
local any = nil
if m >= 0 then
  any = {}
  for i = 1, m do table.insert(any, b .. 'd:' .. ARGV[11 + n + i]) end
end
local out = {}
local count, bytes, hits = 0, 0, 0
if n == 0 and any == nil and prefix == '' then
  if totals then
    local a = redis.call('HMGET', b .. 'agg', 'n', 'bytes', 'hits')
    count, bytes, hits = tonumber(a[1] or '0'), tonumber(a[2] or '0'), tonumber(a[3] or '0')
  end
  local found = 0
  local offset = 0
  local bound = '+inf'
  if not desc then bound = '-inf' end
  if has then bound = ARGV[5] end
  while found <= want do
    local batch
    if desc then
      batch = redis.call('ZRANGE', z, bound, '-inf', 'BYSCORE', 'REV', 'LIMIT', offset, 256, 'WITHSCORES')
    else
      batch = redis.call('ZRANGE', z, bound, '+inf', 'BYSCORE', 'LIMIT', offset, 256, 'WITHSCORES')
    end
    if #batch == 0 then break end
    for i = 1, #batch, 2 do
      if found > want then break end
      if after(tonumber(batch[i + 1]), batch[i], has, av, ak, desc) then
        row(out, batch[i])
        found = found + 1
      end
    end
    offset = offset + 256
  end
  local more = 0
  if found > want then more = 1 end
  local rows = {more, count, bytes, hits}
  for i = 1, math.min(#out, want * 7) do table.insert(rows, out[i]) end
  return rows
end
local cands
if prefix ~= '' then
  cands = redis.call('ZRANGE', b .. 'k', '[' .. prefix, '[' .. prefix .. '\255', 'BYLEX')
elseif n > 0 then
  cands = redis.call('SINTER', unpack(all))
else
  cands = redis.call('SUNION', unpack(any))
end
local picked = {}
for _, k in ipairs(cands) do
  local ok = true
  for _, d in ipairs(all) do
    if ok and redis.call('SISMEMBER', d, k) == 0 then ok = false end
  end
  if ok and any then
    ok = false
    for _, d in ipairs(any) do
      if (not ok) and redis.call('SISMEMBER', d, k) == 1 then ok = true end
    end
  end
  if ok and prefix ~= '' and string.sub(k, 1, #prefix) ~= prefix then ok = false end
  if ok and redis.call('HEXISTS', b .. 'x', k) == 1 then
    if totals then
      count = count + 1
      bytes = bytes + tonumber(redis.call('ZSCORE', b .. 'z4', k) or '0')
      hits = hits + tonumber(redis.call('ZSCORE', b .. 'z3', k) or '0')
    end
    local s = tonumber(redis.call('ZSCORE', z, k) or '0')
    if after(s, k, has, av, ak, desc) then table.insert(picked, {s, k}) end
  end
end
table.sort(picked, function(p, q)
  if p[1] ~= q[1] then if desc then return p[1] > q[1] else return p[1] < q[1] end end
  if desc then return p[2] > q[2] else return p[2] < q[2] end
end)
local more = 0
if #picked > want then more = 1 end
local rows = {more, count, bytes, hits}
for i = 1, math.min(#picked, want) do row(rows, picked[i][2]) end
return rows
";

/// ARGV: now, key. The key's row, or an empty array.
const READ: &str = r"
if redis.call('HEXISTS', b .. 'x', ARGV[2]) == 0 then return {} end
local e = tonumber(redis.call('ZSCORE', b .. 'z2', ARGV[2]) or '0')
if e <= tonumber(ARGV[1]) then return {} end
local out = {}
row(out, ARGV[2])
return out
";

/// Which shared helpers a script needs.
#[derive(Copy, Clone)]
enum Helpers {
    None,
    Remove,
    Rows,
}

fn script(body: &str, helpers: Helpers) -> String {
    let mut s = String::new();
    match helpers {
        Helpers::None => s.push_str("local b = KEYS[1]\n"),
        Helpers::Remove => s.push_str(RM),
        Helpers::Rows => {
            s.push_str(RM);
            s.push_str(ROW);
        }
    }
    s.push_str(body);
    s
}

/// One script, sent with `EVAL` and the index base as its only key.
pub(super) struct Lua(String);

impl Lua {
    /// The call, to which the caller adds the script's arguments.
    pub(super) fn call(&self, base: &str) -> redis::Cmd {
        let mut cmd = redis::cmd("EVAL");
        cmd.arg(&self.0).arg(1).arg(base);
        cmd
    }
}

/// The scripts, each with the helpers it uses.
pub(super) struct Scripts {
    pub(super) put: Lua,
    pub(super) touch: Lua,
    pub(super) remove: Lua,
    pub(super) drop_dim: Lua,
    pub(super) drop_older: Lua,
    pub(super) size: Lua,
    pub(super) count: Lua,
    pub(super) list: Lua,
    pub(super) read: Lua,
}

impl Scripts {
    pub(super) fn new() -> Scripts {
        let s = |body: &str, helpers: Helpers| Lua(script(body, helpers));
        Scripts {
            put: s(PUT, Helpers::Remove),
            touch: s(TOUCH, Helpers::None),
            remove: s(REMOVE, Helpers::Remove),
            drop_dim: s(DROP_DIM, Helpers::Remove),
            drop_older: s(DROP_OLDER, Helpers::Remove),
            size: s(SIZE, Helpers::Remove),
            count: s(COUNT, Helpers::Remove),
            list: s(LIST, Helpers::Rows),
            read: s(READ, Helpers::Rows),
        }
    }
}
